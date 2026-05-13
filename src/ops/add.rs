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
    let mut cs = Context::current_stack(&cwd)?;

    // Must be on the topmost active branch.
    let top = cs
        .stack()
        .top_active()
        .map(|b| b.branch.clone())
        .unwrap_or_else(|| cs.stack().trunk.branch.clone());
    if top != cs.current_branch {
        return Err(StackError::InvalidArgs(format!(
            "can only add branches on top of the stack (current top: {top})"
        )));
    }

    let full = cs.stack().apply_prefix(&args.branch);

    if !cs.file.stacks_containing(&full).is_empty() {
        return Err(StackError::InvalidArgs(format!(
            "branch `{full}` is already in a stack"
        )));
    }
    if cs.ctx.git.branch_exists(&full)? {
        return Err(StackError::InvalidArgs(format!(
            "branch `{full}` already exists"
        )));
    }

    // Stage and commit on the *current* branch before creating the new one, if requested.
    if let Some(msg) = &args.message {
        if args.all {
            cs.ctx.git.add_all()?;
        } else if args.update {
            cs.ctx.git.add_tracked()?;
        }
        cs.ctx.git.commit(msg)?;
    }

    let parent_sha = cs.ctx.git.rev_parse(&cs.current_branch)?;
    cs.ctx.git.create_branch(&full, &parent_sha)?;
    let new_head = cs.ctx.git.rev_parse(&full)?;

    cs.stack_mut().branches.push(Branch {
        branch: full.clone(),
        head: Some(new_head),
        base: parent_sha,
        pull_request: None,
    });
    cs.save()?;

    eprintln!("✓ added {full}");
    Ok(())
}
