//! `pool remove BRANCH` — drop a branch from its pool. The git branch
//! itself is left untouched; we only forget about it. If the containing
//! pool ends up empty, we keep it around so the user can re-add — pools
//! are cheap.

use crate::domain::StackError;

use crate::ops::Context;

pub fn run(branch: Option<String>) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let ctx = Context::open(&cwd)?;
    let mut file = ctx.store.load_pools(&ctx.identity)?;

    let target = match branch {
        Some(b) => b,
        None => ctx
            .git
            .current_branch()?
            .ok_or_else(|| StackError::Other("detached HEAD; pass a branch name".into()))?,
    };

    let idx = super::resolve_pool_index_for_branch(&file, &target)?;
    let pos = file.pools[idx]
        .position(&target)
        .expect("resolve_pool_index_for_branch validated containment");
    file.pools[idx].branches.remove(pos);
    let label = file.pools[idx].display_label().to_string();
    ctx.store.save_pools(&file)?;

    eprintln!("✓ removed {target} from pool {label}");
    Ok(())
}
