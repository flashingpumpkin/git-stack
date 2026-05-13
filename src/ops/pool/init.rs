//! `pool init [--pool NAME] [--base BRANCH] [-w PATH] [--no-worktree]
//! [-a] BRANCH` — create (or adopt) a single branch off a base, set up
//! its worktree, and add it to a pool.
//!
//! Mirrors `stack init`'s contract: by default also creates a worktree
//! at `~/Worktrees/<repo>/<branch>` and prints its path on stdout, so
//! callers can `cd "$(pool init …)"`. Pool members are independent of
//! each other, so we take one branch at a time — for multiple, run
//! `pool init` repeatedly or use `pool adopt`.

use std::path::PathBuf;

use crate::domain::{Pool, PoolBranch, StackError, Trunk};

use crate::ops::{default_worktree_path, Context};

pub struct InitArgs {
    pub pool: Option<String>,
    /// Base branch the new branch is rooted on. Defaults to the pool's
    /// trunk (or the repo's default branch when creating the pool).
    pub base: Option<String>,
    /// Adopt an existing branch instead of creating a new one.
    pub adopt: bool,
    /// Override the worktree path. When `None`, defaults to
    /// `~/Worktrees/<repo-folder-name>/<branch>`.
    pub worktree: Option<PathBuf>,
    /// Skip creating a worktree; just create the branch in place.
    pub no_worktree: bool,
    /// The branch to create / adopt.
    pub branch: String,
}

pub fn run(args: InitArgs) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let ctx = Context::open(&cwd)?;
    let mut pool_file = ctx.store.load_pools(&ctx.identity)?;
    // The "already in a stack" guardrail needs the stack file.
    let stack_file = ctx.store.load(&ctx.identity)?;

    let branch = args.branch.clone();
    if !stack_file.stacks_containing(&branch).is_empty() {
        return Err(StackError::InvalidArgs(format!(
            "branch `{branch}` is already in a stack; remove it with `stack drop` first"
        )));
    }
    if pool_file.pool_position_containing(&branch).is_some() {
        return Err(StackError::InvalidArgs(format!(
            "branch `{branch}` is already in a pool"
        )));
    }

    // Resolve the base ref. Order: --base; pool's trunk if the pool
    // already exists; the repo's default branch (used both as the new
    // pool's trunk and as this branch's base).
    let base_branch = match &args.base {
        Some(b) => b.clone(),
        None => match pool_file.pool_position(args.pool.as_deref()) {
            Some(i) => pool_file.pools[i].trunk.branch.clone(),
            None => ctx.git.trunk_branch()?,
        },
    };
    let base_head = ctx.git.rev_parse(&base_branch)?;

    if args.adopt {
        if !ctx.git.branch_exists(&branch)? {
            return Err(StackError::InvalidArgs(format!(
                "branch `{branch}` does not exist (adopt requires an existing branch)"
            )));
        }
    } else if ctx.git.branch_exists(&branch)? {
        return Err(StackError::InvalidArgs(format!(
            "branch `{branch}` already exists; use --adopt to take it over"
        )));
    }

    // Decide on a worktree path before we touch anything else, so we
    // can fail fast on collisions.
    let worktree_path: Option<PathBuf> = if args.no_worktree {
        None
    } else {
        Some(match &args.worktree {
            Some(p) => p.clone(),
            None => default_worktree_path(&ctx, &branch)?,
        })
    };
    if let Some(p) = &worktree_path {
        if p.exists() {
            return Err(StackError::InvalidArgs(format!(
                "worktree path {} already exists; pass --worktree to choose another or --no-worktree to skip",
                p.display()
            )));
        }
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }

    // Create the branch without checking it out (the worktree, if any,
    // will pick it up). If --adopt, the branch already exists.
    if !args.adopt {
        ctx.git.create_branch_no_checkout(&branch, &base_head)?;
    }
    let head = ctx.git.rev_parse(&branch)?;

    // Auto-create the target pool on first use.
    let pool_idx = match pool_file.pool_position(args.pool.as_deref()) {
        Some(i) => i,
        None => {
            pool_file.pools.push(Pool {
                name: args.pool.clone(),
                trunk: Trunk {
                    branch: base_branch.clone(),
                    head: base_head.clone(),
                },
                branches: Vec::new(),
                last_refreshed_at: None,
            });
            pool_file.pools.len() - 1
        }
    };

    pool_file.pools[pool_idx].branches.push(PoolBranch {
        branch: branch.clone(),
        head: Some(head),
        base_branch: base_branch.clone(),
        base: base_head.clone(),
        pull_request: None,
    });

    if let Some(p) = &worktree_path {
        ctx.git.worktree_add(p, &branch)?;
    } else {
        // No worktree: switch the current working tree to the new branch.
        ctx.git.checkout(&branch)?;
    }

    ctx.git.enable_rerere().ok();
    ctx.store.save_pools(&pool_file)?;

    let label = pool_file.pools[pool_idx].display_label().to_string();
    eprintln!("✓ added {branch} to pool {label} (base: {base_branch})");
    if let Some(p) = &worktree_path {
        eprintln!("✓ worktree created at {}", p.display());
        eprintln!();
        // Final line on stdout so callers can `cd "$(pool init … | tail -1)"`.
        println!("{}", p.display());
    }
    Ok(())
}
