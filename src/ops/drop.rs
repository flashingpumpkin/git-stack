//! `stack drop <branch>` — remove a single branch from a stack and rebase
//! its children onto the dropped branch's parent.
//!
//! - Removes only the metadata; the git branch itself is left alone (you can
//!   still `git checkout` it or re-adopt it later).
//! - Children's stored `base` is updated to point at the dropped branch's
//!   parent (the previous branch in the stack, or trunk if dropping the
//!   bottom). A cascade rebase then brings their git history into line.
//! - If the dropped branch is currently checked out, we switch to its
//!   parent first so git doesn't refuse later operations on the branch.

use crate::domain::StackError;

use super::{rebase, Context};

pub fn run(branch: Option<String>) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let (ctx, mut file) = Context::with_file(&cwd)?;

    let target = match branch {
        Some(b) => b,
        None => ctx
            .git
            .current_branch()?
            .ok_or_else(|| StackError::Other("detached HEAD; pass a branch name".into()))?,
    };

    // Resolve which stack contains it.
    let matches = file.stacks_containing(&target);
    match matches.len() {
        0 => return Err(StackError::NotInStack),
        1 => {}
        _ => return Err(StackError::Disambiguation(target.clone())),
    }
    let stack_index = file
        .stacks
        .iter()
        .position(|s| s.contains(&target))
        .expect("validated above");
    let pos = file.stacks[stack_index]
        .position(&target)
        .expect("validated above");

    // If we're currently on the branch being dropped, move off it first.
    // Prefer moving to a surviving stack branch so subsequent ops still find
    // "the current stack". Order of preference: next child, previous branch,
    // trunk.
    let current = ctx.git.current_branch()?;
    if current.as_deref() == Some(target.as_str()) {
        let next_child = file.stacks[stack_index]
            .branches
            .get(pos + 1)
            .map(|b| b.branch.clone());
        let prev = if pos > 0 {
            Some(file.stacks[stack_index].branches[pos - 1].branch.clone())
        } else {
            None
        };
        let move_to = next_child
            .or(prev)
            .unwrap_or_else(|| file.stacks[stack_index].trunk.branch.clone());
        ctx.git.checkout(&move_to)?;
        eprintln!("ℹ moved off {target} to {move_to}");
    }

    // Remove the branch entry. Leave the next child's stored `base` pointing
    // at the *dropped* branch's old head — that becomes the rebase's
    // `old_base`, while the live parent's head is the `new_base`. The cascade
    // rebase op handles the rewrite.
    file.stacks[stack_index].branches.remove(pos);
    ctx.store.save(&file)?;

    eprintln!("✓ dropped {target} from stack");

    // If the stack is now empty, drop the stack entry entirely.
    if file.stacks[stack_index].branches.is_empty() {
        file.stacks.remove(stack_index);
        ctx.store.save(&file)?;
        eprintln!("ℹ stack is now empty; removed from tracking");
        return Ok(());
    }

    // Release the lock before re-entering rebase (which acquires its own).
    drop(ctx);

    // Cascade rebase to bring children's git history in line with the new base.
    rebase::run(rebase::RebaseArgs {
        scope: rebase::Scope::Full,
        cont: false,
        abort: false,
        remote: "origin".to_string(),
        // Already on a fresh local op; no need to re-hit GitHub.
        no_refresh: true,
    })?;
    Ok(())
}
