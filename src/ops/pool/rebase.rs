//! `pool rebase` — rebase every active branch in a pool onto the live
//! head of *its own* base branch.
//!
//! Pool members are independent of each other, but each one can sit on a
//! different upstream (trunk, a release branch, another long-lived
//! branch). The plan walks every branch, resolves its base branch's live
//! head, and runs `git rebase --onto <live_base> <stored_base> <branch>`.
//! After success we update the stored base SHA + head.
//!
//! Like `stack rebase`, we never touch local base refs. We `git fetch`
//! the remote and prefer `<remote>/<base>` over the local ref so a
//! shared `main` checked out in another worktree doesn't move under us.
//! Falls back to the local ref for local-only bases (e.g. a feature
//! branch with no remote tracking).
//!
//! When a branch's base has itself been merged on GitHub (you were
//! parked on a feature branch that just landed), the pre-rebase refresh
//! walks up the base chain and reparents you onto the first non-merged
//! ancestor. Same mechanism `stack rebase` uses to skip merged parents
//! in a chain.
//!
//! On conflict we bail and let the user resolve via standard
//! `git rebase --continue` / `--abort` — work already persisted for
//! earlier branches stays put, so the user can re-run `pool rebase` to
//! resume from where they were.

use crate::domain::StackError;

use crate::ops::Context;

pub struct RebaseArgs {
    pub pool: Option<String>,
    pub remote: String,
    pub no_refresh: bool,
    pub cont: bool,
    pub abort: bool,
}

pub fn run(args: RebaseArgs) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let ctx = Context::open(&cwd)?;

    if args.abort {
        ctx.git.rebase_abort()?;
        eprintln!("✓ rebase aborted");
        return Ok(());
    }
    if args.cont {
        ctx.git.rebase_continue()?;
        eprintln!("✓ rebase continued");
        eprintln!("ℹ rerun `pool rebase` to pick up the next branch in the plan");
        return Ok(());
    }

    if ctx.git.rebase_in_progress() {
        return Err(StackError::RebaseInProgress);
    }

    let mut file = ctx.store.load_pools(&ctx.identity)?;
    let idx = super::resolve_pool_index(&file, args.pool.as_deref())?;
    let label = file.pools[idx].display_label().to_string();

    // Refresh first so squash-merged branches are skipped during planning.
    if !args.no_refresh {
        let gh = crate::github::GhCli::new(&cwd);
        let _ = super::refresh::refresh_pool(&gh, &mut file, idx)?;
        let _ = super::refresh::drop_merged(&mut file, idx);
        ctx.store.save_pools(&file)?;
    }

    // Heads-up: any branch with a known mergeable=false will conflict
    // during this rebase. Print them up front so the user isn't surprised
    // when the cascade halts on the first one.
    let conflicting: Vec<String> = file.pools[idx]
        .branches
        .iter()
        .filter(|b| {
            !b.is_merged() && b.pull_request.as_ref().and_then(|p| p.mergeable) == Some(false)
        })
        .map(|b| b.branch.clone())
        .collect();
    if !conflicting.is_empty() {
        use crate::style::warn;
        eprintln!(
            "{} {} branch(es) marked as conflicting on GitHub — expect rebase conflicts:",
            warn("⚠"),
            conflicting.len()
        );
        for name in &conflicting {
            eprintln!("    {name}");
        }
    }

    // Fetch so every remote-tracking ref we might rebase onto is current.
    // Unlike the old behaviour, we never fast-forward local refs — they
    // may be checked out in another worktree, and moving the local ref
    // under that worktree's feet leaves its working tree out of sync
    // with HEAD. This mirrors the fix `stack sync` shipped on main.
    // Best-effort: a fetch failure (no network, missing remote) prints
    // a warning but continues against the existing remote-tracking ref.
    if let Err(e) = ctx.git.fetch(&args.remote) {
        eprintln!("⚠ fetch {} failed: {e}", args.remote);
        eprintln!("  continuing against existing remote-tracking ref");
    }

    let mut did_any = false;
    let n_branches = file.pools[idx].branches.len();
    for i in 0..n_branches {
        let (branch_name, old_base, base_branch_name, is_merged) = {
            let pool = &file.pools[idx];
            let b = &pool.branches[i];
            (
                b.branch.clone(),
                b.base.clone(),
                b.effective_base_branch(pool).to_string(),
                b.is_merged(),
            )
        };
        if is_merged {
            continue;
        }

        // Prefer `<remote>/<base>` (per-clone, no working tree attached)
        // over the local ref. Fall back to the local ref for bases that
        // have no remote-tracking counterpart (e.g. a purely local
        // feature branch you happen to be parked on).
        let remote_ref = format!("refs/remotes/{}/{}", args.remote, base_branch_name);
        let new_base = match ctx.git.rev_parse(&remote_ref) {
            Ok(s) => s,
            Err(_) => match ctx.git.rev_parse(&base_branch_name) {
                Ok(s) => s,
                Err(_) => {
                    eprintln!(
                        "ℹ skipping {branch_name}: base ref `{base_branch_name}` not found (locally or on {})",
                        args.remote
                    );
                    continue;
                }
            },
        };
        if old_base == new_base {
            continue;
        }

        match ctx.git.rebase_onto(&new_base, &old_base, &branch_name) {
            Ok(outcome) => {
                let new_head = ctx.git.rev_parse(&branch_name)?;
                let b = &mut file.pools[idx].branches[i];
                b.base = new_base.clone();
                b.head = Some(new_head);
                ctx.store.save_pools(&file)?;
                eprintln!(
                    "✓ rebased {branch_name} onto {base_branch_name} ({})",
                    &new_base[..7.min(new_base.len())]
                );
                if outcome.rerere_replays > 0 {
                    eprintln!(
                        "  ↳ rerere replayed {} resolution{}",
                        outcome.rerere_replays,
                        if outcome.rerere_replays == 1 { "" } else { "s" },
                    );
                }
                did_any = true;
            }
            Err(StackError::RebaseConflict) => {
                eprintln!(
                    "✗ conflict while rebasing {branch_name}; resolve and run `pool rebase --continue`, then rerun `pool rebase`"
                );
                return Err(StackError::RebaseConflict);
            }
            Err(e) => return Err(e),
        }
    }

    if did_any {
        eprintln!("✓ pool {label} rebased");
    } else {
        eprintln!("✓ pool {label} already up to date");
    }
    Ok(())
}
