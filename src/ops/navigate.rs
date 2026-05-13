use crate::domain::StackError;

use super::Context;

#[derive(Debug, Clone, Copy)]
pub enum Direction {
    Up,
    Down,
    Top,
    Bottom,
}

pub fn run(direction: Direction, count: usize) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let ctx = Context::open(&cwd)?;
    let file = ctx.store.load(&ctx.identity)?;
    let current = ctx
        .git
        .current_branch()?
        .ok_or_else(|| StackError::Other("detached HEAD; check out a branch first".into()))?;
    let stack = file.current_stack(&current)?;

    let target = match direction {
        Direction::Up => stack.step_active(&current, count as isize),
        Direction::Down => stack.step_active(&current, -(count as isize)),
        Direction::Top => stack.top_active(),
        Direction::Bottom => stack.bottom_active(),
    }
    .ok_or_else(|| StackError::Other("no active branches in stack".into()))?;

    if target.branch == current {
        eprintln!("ℹ already at {}", current);
        return Ok(());
    }

    ctx.git.checkout(&target.branch)?;
    eprintln!("✓ checked out {}", target.branch);
    Ok(())
}
