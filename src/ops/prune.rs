//! `stack prune` — housekeeping for stale stack metadata + git's worktree
//! bookkeeping.
//!
//! Two operations in one pass:
//!
//! 1. Refresh PR state (default; opt out with `--no-refresh`), then drop
//!    any stack whose every branch is now marked merged. The metadata for
//!    a fully-merged stack is dead weight: its commits live in trunk, the
//!    PRs are closed, and `stack view` on those branches just shows a
//!    stack of ✓ rows. Worktrees associated with dropped stacks are
//!    removed too — same refusal-on-dirty rule as `stack remove`.
//!
//! 2. Call `git worktree prune` so paths whose directories were `rm -rf`'d
//!    are cleared from git's bookkeeping. This is the operation we hint at
//!    in `stack list`'s "(missing)" marker.
//!
//! `--dry-run` reports what would happen without writing.

use crate::domain::StackError;

use super::{pr_refresh, Context};

pub struct PruneArgs {
    pub dry_run: bool,
    pub no_refresh: bool,
    pub force: bool,
}

pub fn run(args: PruneArgs) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let (ctx, mut file) = Context::with_file(&cwd)?;
    let mut any_change = false;

    // Refresh PR state across every stack so the "fully merged" check is
    // honest. Skip the save on dry-runs so we don't persist refresh
    // side-effects when the user said "report only".
    if !args.no_refresh {
        let gh = crate::github::GhCli::new(&cwd);
        let total = pr_refresh::refresh_all(&gh, &mut file)?;
        if !args.dry_run {
            ctx.store.save(&file)?;
        }
        if total > 0 {
            eprintln!("ℹ {total} PR(s) merged on GitHub since last refresh");
            any_change = true;
        }
    }

    // Drop fully-merged stacks. Walk in reverse so each drop_stack_with_worktrees
    // call sees stable indices for the remaining stacks.
    let to_drop: Vec<usize> = file
        .stacks
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.branches.is_empty() && s.branches.iter().all(|b| b.is_merged()))
        .map(|(i, _)| i)
        .collect();

    for &i in to_drop.iter().rev() {
        let label = file.stacks[i]
            .prefix
            .clone()
            .unwrap_or_else(|| "(no prefix)".into());
        let removed =
            super::drop_stack_with_worktrees(&ctx, &mut file, i, &cwd, args.force, args.dry_run)?;
        let verb = if args.dry_run {
            "would drop"
        } else {
            "✓ dropped"
        };
        eprintln!("{verb} {label}");
        for p in &removed {
            let wt_verb = if args.dry_run {
                "would remove worktree"
            } else {
                "removed worktree"
            };
            eprintln!("  ↳ {wt_verb} {}", p.display());
        }
        any_change = true;
    }
    if !args.dry_run && !to_drop.is_empty() {
        ctx.store.save(&file)?;
    }

    // git worktree prune: clean stale bookkeeping for any worktree dirs
    // that were rm -rf'd outside our knowledge. Only report when there's
    // something to say.
    let prune_out = if args.dry_run {
        ctx.git.worktree_prune_dry_run()?
    } else {
        ctx.git.worktree_prune()?
    };
    if !prune_out.is_empty() {
        let verb = if args.dry_run {
            "would prune stale worktree bookkeeping:"
        } else {
            "✓ pruned stale worktree bookkeeping:"
        };
        eprintln!("{verb}");
        for line in prune_out.lines() {
            eprintln!("    {line}");
        }
        any_change = true;
    }

    if !any_change {
        eprintln!("✓ nothing to prune");
    }

    Ok(())
}
