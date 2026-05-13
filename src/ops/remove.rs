//! `stack remove <branch>` — stop tracking the stack containing `<branch>`
//! and remove its worktree(s).
//!
//! - Stack metadata is dropped from the central store.
//! - Any worktrees git knows about for branches in the stack are removed
//!   from disk via `git worktree remove`.
//! - Refuses if a worktree has uncommitted changes; pass `--force` to
//!   override (propagates to `git worktree remove --force`).
//! - Leaves the underlying git branches alone — re-adopt later with
//!   `stack init --adopt` or delete with `git branch -D`.

use std::path::PathBuf;

use crate::domain::StackError;

use super::Context;

pub struct RemoveArgs {
    pub branch: String,
    pub force: bool,
}

pub fn run(args: RemoveArgs) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let (ctx, mut file) = Context::with_file(&cwd)?;

    let matches = file.stacks_containing(&args.branch);
    match matches.len() {
        0 => return Err(StackError::NotInStack),
        1 => {}
        _ => return Err(StackError::Disambiguation(args.branch.clone())),
    }
    let stack_index = file
        .stacks
        .iter()
        .position(|s| s.contains(&args.branch))
        .expect("validated above");

    // Find worktrees that belong to branches in this stack. git knows about
    // every worktree of the clone; we filter to ones tracking our branches.
    let all_worktrees = ctx.git.worktrees().unwrap_or_default();
    let stack_branches: Vec<&str> = file.stacks[stack_index]
        .branches
        .iter()
        .map(|b| b.branch.as_str())
        .collect();
    let our_worktrees: Vec<PathBuf> = all_worktrees
        .iter()
        .filter(|(_, b)| stack_branches.contains(&b.as_str()))
        .map(|(p, _)| p.clone())
        .collect();

    // Refuse to nuke uncommitted work unless --force.
    if !args.force {
        let mut dirty: Vec<PathBuf> = Vec::new();
        for p in &our_worktrees {
            if !p.exists() {
                continue;
            }
            if !ctx.git.is_worktree_clean(p)? {
                dirty.push(p.clone());
            }
        }
        if !dirty.is_empty() {
            eprintln!("✗ refusing to remove stack: worktree has uncommitted changes");
            for p in &dirty {
                eprintln!("    {}", p.display());
            }
            eprintln!(
                "  commit, stash, or discard the changes — or re-run with --force to override."
            );
            return Err(StackError::InvalidArgs(
                "worktree has uncommitted changes".into(),
            ));
        }
    }

    // Cowardly: don't `worktree remove` from inside the worktree we're
    // removing. git would error anyway, but the message is friendlier here.
    if let Some(p) = our_worktrees.iter().find(|p| cwd.starts_with(p)) {
        return Err(StackError::InvalidArgs(format!(
            "cannot remove a stack from inside its own worktree ({}); cd elsewhere first",
            p.display()
        )));
    }

    for p in &our_worktrees {
        if !p.exists() {
            // git still tracks it but the directory's gone — `worktree prune`
            // territory, but cleaning it here keeps the operation atomic.
            continue;
        }
        ctx.git.worktree_remove(p, args.force)?;
        eprintln!("✓ removed worktree {}", p.display());
    }

    // Drop the stack from metadata and run a worktree prune to clean up
    // any stale bookkeeping entries we just freed.
    file.stacks.remove(stack_index);
    ctx.store.save(&file)?;
    let _ = ctx.git.worktree_prune();

    eprintln!("✓ removed stack containing `{}`", args.branch);
    Ok(())
}
