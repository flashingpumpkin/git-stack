use crate::domain::StackError;

use super::Context;

pub fn run(remote: &str) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let cs = Context::current_stack(&cwd)?;

    let names: Vec<&str> = cs
        .stack()
        .active_branches()
        .iter()
        .map(|b| b.branch.as_str())
        .collect();
    if names.is_empty() {
        eprintln!("ℹ nothing to push");
        return Ok(());
    }
    cs.ctx.git.push_atomic(remote, &names)?;
    eprintln!("✓ pushed {} branch(es) to {remote}", names.len());
    Ok(())
}
