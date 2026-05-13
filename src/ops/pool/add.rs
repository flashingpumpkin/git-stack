//! `pool add [--pool NAME] [--base BRANCH] BRANCH...` — add existing local
//! branches to a pool. Auto-creates the pool if it doesn't exist.
//!
//! Base resolution order, per branch:
//!   1. `--base` if the caller specified one
//!   2. The PR's `baseRefName` if a PR exists for the branch on GitHub
//!      (looked up via the `gh` adapter; skipped silently on lookup failure)
//!   3. The pool's trunk

use crate::domain::{Pool, PoolBranch, StackError, Trunk};
use crate::github::{GhCli, GitHub};

use crate::ops::Context;

pub struct AddArgs {
    pub pool: Option<String>,
    /// Explicit base branch override. When `None`, base is resolved from
    /// the PR (if any) or falls back to the pool's trunk.
    pub base: Option<String>,
    pub branches: Vec<String>,
}

pub fn run(args: AddArgs) -> Result<(), StackError> {
    if args.branches.is_empty() {
        return Err(StackError::InvalidArgs(
            "supply at least one branch name".into(),
        ));
    }

    let cwd = std::env::current_dir()?;
    let ctx = Context::open(&cwd)?;
    let mut pool_file = ctx.store.load_pools(&ctx.identity)?;
    // We also need the stack file to check the "already in a stack" guardrail.
    let stack_file = ctx.store.load(&ctx.identity)?;

    // Validate that every branch exists locally and isn't already in a
    // stack or another pool.
    for b in &args.branches {
        if !ctx.git.branch_exists(b)? {
            return Err(StackError::InvalidArgs(format!(
                "branch `{b}` does not exist locally"
            )));
        }
        if !stack_file.stacks_containing(b).is_empty() {
            return Err(StackError::InvalidArgs(format!(
                "branch `{b}` is already in a stack; remove it with `stack drop` first"
            )));
        }
        if pool_file.pool_position_containing(b).is_some() {
            return Err(StackError::InvalidArgs(format!(
                "branch `{b}` is already in a pool"
            )));
        }
    }

    // Pool selection: existing-by-name, existing-default, or auto-create.
    let pool_idx = match pool_file.pool_position(args.pool.as_deref()) {
        Some(i) => i,
        None => {
            let trunk_branch = ctx.git.trunk_branch()?;
            let trunk_head = ctx.git.rev_parse(&trunk_branch)?;
            pool_file.pools.push(Pool {
                name: args.pool.clone(),
                trunk: Trunk {
                    branch: trunk_branch,
                    head: trunk_head,
                },
                branches: Vec::new(),
                last_refreshed_at: None,
            });
            pool_file.pools.len() - 1
        }
    };

    let pool_trunk_branch = pool_file.pools[pool_idx].trunk.branch.clone();
    let gh = GhCli::new(&cwd);
    for b in &args.branches {
        let head = ctx.git.rev_parse(b)?;
        // Resolve base: --base wins; else PR's base_ref; else pool trunk.
        let base_branch_name = match &args.base {
            Some(name) => name.clone(),
            None => gh
                .find_pr_by_head(b)
                .ok()
                .flatten()
                .map(|pr| pr.base_ref)
                .unwrap_or_else(|| pool_trunk_branch.clone()),
        };
        // Snapshot the live head of the chosen base ref. If git can't
        // resolve it (e.g. a stale base branch name), fall back to the
        // pool trunk's stored head so we still record a usable old_base.
        let base_sha = ctx
            .git
            .rev_parse(&base_branch_name)
            .unwrap_or_else(|_| pool_file.pools[pool_idx].trunk.head.clone());
        pool_file.pools[pool_idx].branches.push(PoolBranch {
            branch: b.clone(),
            head: Some(head),
            base_branch: base_branch_name,
            base: base_sha,
            pull_request: None,
        });
    }
    ctx.store.save_pools(&pool_file)?;

    let label = pool_file.pools[pool_idx].display_label().to_string();
    eprintln!("✓ added {} branch(es) to pool {label}", args.branches.len());
    Ok(())
}
