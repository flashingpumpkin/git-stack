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

    let label = file.stacks[stack_index]
        .prefix
        .clone()
        .unwrap_or_else(|| "(no prefix)".into());

    let removed =
        super::drop_stack_with_worktrees(&ctx, &mut file, stack_index, &cwd, args.force, false)?;
    ctx.store.save(&file)?;
    let _ = ctx.git.worktree_prune();

    eprintln!("✓ dropped {label}");
    for p in &removed {
        eprintln!("  ↳ removed worktree {}", p.display());
    }
    Ok(())
}
