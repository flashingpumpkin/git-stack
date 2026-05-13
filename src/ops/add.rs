use crate::domain::{Branch, StackError};

use super::Context;

pub struct AddArgs {
    pub branch: String,
    pub all: bool,
    pub update: bool,
    pub message: Option<String>,
}

pub fn run(args: AddArgs) -> Result<(), StackError> {
    if args.all && args.update {
        return Err(StackError::InvalidArgs(
            "-A and -u are mutually exclusive".into(),
        ));
    }
    if (args.all || args.update) && args.message.is_none() {
        return Err(StackError::InvalidArgs("-A/-u require -m <message>".into()));
    }

    let cwd = std::env::current_dir()?;
    let ctx = Context::open(&cwd)?;
    let mut file = ctx.store.load(&ctx.identity)?;
    let current = ctx
        .git
        .current_branch()?
        .ok_or_else(|| StackError::Other("detached HEAD".into()))?;

    let stack_idx = file
        .stacks
        .iter()
        .position(|s| s.contains(&current))
        .ok_or(StackError::NotInStack)?;

    // Must be on the topmost active branch.
    let top = file.stacks[stack_idx]
        .top_active()
        .map(|b| b.branch.clone())
        .unwrap_or_else(|| file.stacks[stack_idx].trunk.branch.clone());
    if top != current {
        return Err(StackError::InvalidArgs(format!(
            "can only add branches on top of the stack (current top: {top})"
        )));
    }

    let full = file.stacks[stack_idx].apply_prefix(&args.branch);

    if !file.stacks_containing(&full).is_empty() {
        return Err(StackError::InvalidArgs(format!(
            "branch `{full}` is already in a stack"
        )));
    }
    if ctx.git.branch_exists(&full)? {
        return Err(StackError::InvalidArgs(format!(
            "branch `{full}` already exists"
        )));
    }

    // Stage and commit on the *current* branch before creating the new one, if requested.
    if let Some(msg) = &args.message {
        if args.all {
            ctx.git.add_all()?;
        } else if args.update {
            ctx.git.add_tracked()?;
        }
        ctx.git.commit(msg)?;
    }

    let parent_sha = ctx.git.rev_parse(&current)?;
    ctx.git.create_branch(&full, &parent_sha)?;
    let new_head = ctx.git.rev_parse(&full)?;

    file.stacks[stack_idx].branches.push(Branch {
        branch: full.clone(),
        head: Some(new_head),
        base: parent_sha,
        pull_request: None,
    });
    ctx.store.save(&file)?;

    eprintln!("✓ added {full}");
    Ok(())
}
