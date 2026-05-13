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
    let cs = Context::current_stack(&cwd)?;

    let target = match direction {
        Direction::Up => cs.stack().step_active(&cs.current_branch, count as isize),
        Direction::Down => cs
            .stack()
            .step_active(&cs.current_branch, -(count as isize)),
        Direction::Top => cs.stack().top_active(),
        Direction::Bottom => cs.stack().bottom_active(),
    }
    .ok_or_else(|| StackError::Other("no active branches in stack".into()))?;

    if target.branch == cs.current_branch {
        eprintln!("ℹ already at {}", cs.current_branch);
        return Ok(());
    }

    cs.ctx.git.checkout(&target.branch)?;
    eprintln!("✓ checked out {}", target.branch);
    Ok(())
}
