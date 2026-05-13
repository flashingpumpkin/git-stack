use crate::domain::StackError;

use super::Context;

pub fn run(remote: &str) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let ctx = Context::open(&cwd)?;
    let file = ctx.store.load(&ctx.identity)?;
    let current = ctx
        .git
        .current_branch()?
        .ok_or_else(|| StackError::Other("detached HEAD".into()))?;
    let stack = file.current_stack(&current)?;

    let names: Vec<&str> = stack
        .active_branches()
        .iter()
        .map(|b| b.branch.as_str())
        .collect();
    if names.is_empty() {
        eprintln!("ℹ nothing to push");
        return Ok(());
    }
    ctx.git.push_atomic(remote, &names)?;
    eprintln!("✓ pushed {} branch(es) to {remote}", names.len());
    Ok(())
}
