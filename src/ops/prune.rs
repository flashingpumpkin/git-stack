//! `stack prune` — housekeeping for stale stack metadata + git's worktree
//! bookkeeping.
//!
//! Two operations in one pass:
//!
//! 1. Refresh PR state (default; opt out with `--no-refresh`), then drop
//!    any stack whose every branch is now marked merged. The metadata for
//!    a fully-merged stack is dead weight: its commits live in trunk, the
//!    PRs are closed, and `stack view` on those branches just shows a
//!    stack of ✓ rows.
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
}

pub fn run(args: PruneArgs) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let (ctx, mut file) = Context::with_file(&cwd)?;

    // Refresh PR state across every stack so the "fully merged" check is
    // honest. The shared helper saves for us unless we ask it not to —
    // skip the save on dry-runs so we don't persist refresh side-effects
    // when the user said "report only".
    pr_refresh::refresh_all_unless(args.no_refresh, &cwd, &ctx, &mut file, args.dry_run)?;

    // 2. Collect indices of fully-merged stacks (every branch's PR is
    //    merged AND the stack actually has branches).
    let to_drop: Vec<usize> = file
        .stacks
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.branches.is_empty() && s.branches.iter().all(|b| b.is_merged()))
        .map(|(i, _)| i)
        .collect();

    if to_drop.is_empty() {
        eprintln!("✓ no fully-merged stacks to prune");
    } else {
        for &i in &to_drop {
            let label = file.stacks[i]
                .prefix
                .as_deref()
                .map(String::from)
                .unwrap_or_else(|| {
                    format!("(no prefix, {} branches)", file.stacks[i].branches.len())
                });
            let verb = if args.dry_run {
                "would drop"
            } else {
                "✓ dropped"
            };
            eprintln!("{verb} stack {label}");
        }
        if !args.dry_run {
            // Walk indices in reverse so removals don't shift later indices.
            for &i in to_drop.iter().rev() {
                file.stacks.remove(i);
            }
            ctx.store.save(&file)?;
        }
    }

    // 3. git worktree prune. Surface the verbose output so the user sees
    //    which paths were affected.
    let prune_out = if args.dry_run {
        ctx.git.worktree_prune_dry_run()?
    } else {
        ctx.git.worktree_prune()?
    };
    if prune_out.is_empty() {
        eprintln!("✓ no worktrees to prune");
    } else {
        let verb = if args.dry_run {
            "would prune worktrees:"
        } else {
            "✓ pruned worktrees:"
        };
        eprintln!("{verb}");
        for line in prune_out.lines() {
            eprintln!("    {line}");
        }
    }

    Ok(())
}
