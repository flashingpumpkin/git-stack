use crate::domain::StackError;

use super::Context;

pub fn run(branch: Option<String>) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let ctx = Context::open(&cwd)?;
    let mut file = ctx.store.load(&ctx.identity)?;

    let key = match branch {
        Some(b) => b,
        None => ctx
            .git
            .current_branch()?
            .ok_or_else(|| StackError::Other("detached HEAD; pass a branch name".into()))?,
    };

    let before = file.stacks.len();
    file.stacks.retain(|s| !s.contains(&key));
    let removed = before - file.stacks.len();
    if removed == 0 {
        return Err(StackError::NotInStack);
    }
    ctx.store.save(&file)?;
    eprintln!("✓ removed {removed} stack(s) containing `{key}`");
    Ok(())
}
