//! `pool` — flat collections of independent branches with PR tracking.
//!
//! A pool is unlike a stack in that its members don't depend on each
//! other: each branch has its own base. We still track the same things
//! — is a rebase needed, is there a PR, what's the PR's state — and
//! additionally track new comments since the last time the user looked
//! plus PR mergeable status.
//!
//! Pool state lives in `pools.json`, separate from `stacks.json`. See
//! `domain::PoolFile`. A repo can have one unnamed default pool plus any
//! number of named pools.

use crate::domain::{Pool, PoolFile, StackError};

pub mod add;
pub mod adopt;
pub mod list;
pub mod prune;
pub mod rebase;
pub mod refresh;
pub mod remove;

/// Resolve which pool the caller means.
///
/// - `Some(name)` → must exist by that name.
/// - `None` → if exactly one pool exists, use it (regardless of name);
///   otherwise require the default pool by absence-of-name.
///
/// This matches the "single pool, no naming required" UX while still
/// supporting multi-pool workflows.
pub fn resolve_pool_index(file: &PoolFile, name: Option<&str>) -> Result<usize, StackError> {
    if let Some(n) = name {
        return file
            .pool_position(Some(n))
            .ok_or_else(|| StackError::InvalidArgs(format!("no pool named `{n}` in this repo")));
    }
    match file.pools.len() {
        0 => Err(StackError::InvalidArgs(
            "no pools tracked for this repo; add a branch with `pool add` first".into(),
        )),
        1 => Ok(0),
        _ => file.pool_position(None).ok_or_else(|| {
            StackError::InvalidArgs("multiple named pools tracked; pass --pool <name>".into())
        }),
    }
}

/// Look up the pool containing `branch`. Errors if multiple pools claim it
/// (shouldn't happen by construction, but defensive).
pub fn resolve_pool_index_for_branch(file: &PoolFile, branch: &str) -> Result<usize, StackError> {
    let hits: Vec<usize> = file
        .pools
        .iter()
        .enumerate()
        .filter(|(_, p)| p.contains(branch))
        .map(|(i, _)| i)
        .collect();
    match hits.len() {
        0 => Err(StackError::Other(format!(
            "branch `{branch}` is not in any pool"
        ))),
        1 => Ok(hits[0]),
        _ => Err(StackError::Disambiguation(branch.to_string())),
    }
}

/// Display label for messaging. `Some(name)` → name; `None` → "(default)".
pub fn label_for(pool: &Pool) -> &str {
    pool.display_label()
}
