use std::path::PathBuf;

use crate::domain::{Branch, Stack, StackError, Trunk};

use super::{default_worktree_path, Context};

pub struct InitArgs {
    pub base: Option<String>,
    pub prefix: Option<String>,
    pub adopt: bool,
    pub branches: Vec<String>,
    /// Override the worktree path. When `None`, defaults to
    /// `~/Worktrees/<repo-folder-name>/<first-branch>`.
    pub worktree: Option<PathBuf>,
    /// Skip creating a worktree; init in the current working tree (old behaviour).
    pub no_worktree: bool,
}

pub fn run(args: InitArgs) -> Result<(), StackError> {
    if args.branches.is_empty() {
        return Err(StackError::InvalidArgs(
            "supply at least one branch name (this CLI never prompts)".into(),
        ));
    }

    let cwd = std::env::current_dir()?;
    let ctx = Context::open(&cwd)?;
    let mut file = ctx.store.load(&ctx.identity)?;

    let trunk_branch = match &args.base {
        Some(b) => b.clone(),
        None => ctx.git.trunk_branch()?,
    };
    let trunk_head = ctx.git.rev_parse(&trunk_branch)?;

    let full_names: Vec<String> = args
        .branches
        .iter()
        .map(|n| match &args.prefix {
            Some(p) => format!("{p}/{n}"),
            None => n.clone(),
        })
        .collect();

    for n in &full_names {
        if !file.stacks_containing(n).is_empty() {
            return Err(StackError::InvalidArgs(format!(
                "branch `{n}` is already in a stack; use `stack remove` first"
            )));
        }
    }

    if args.adopt {
        for n in &full_names {
            if !ctx.git.branch_exists(n)? {
                return Err(StackError::InvalidArgs(format!(
                    "branch `{n}` does not exist (adopt requires existing branches)"
                )));
            }
        }
    } else {
        for n in &full_names {
            if ctx.git.branch_exists(n)? {
                return Err(StackError::InvalidArgs(format!(
                    "branch `{n}` already exists; use --adopt to take it over"
                )));
            }
        }
    }

    // Decide whether to make a worktree, and where.
    let worktree_path: Option<PathBuf> = if args.no_worktree {
        None
    } else {
        Some(match &args.worktree {
            Some(p) => p.clone(),
            None => default_worktree_path(&ctx, &full_names[0])?,
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

    // Create branches without touching the current working tree.
    let mut branches: Vec<Branch> = Vec::with_capacity(full_names.len());
    let mut parent_sha = trunk_head.clone();
    for n in &full_names {
        if !args.adopt {
            ctx.git.create_branch_no_checkout(n, &parent_sha)?;
        }
        let head = ctx.git.rev_parse(n)?;
        branches.push(Branch {
            branch: n.clone(),
            head: Some(head.clone()),
            base: parent_sha.clone(),
            pull_request: None,
        });
        parent_sha = head;
    }

    let bottom = full_names.first().expect("validated non-empty");

    if let Some(p) = &worktree_path {
        // Worktree starts on the bottom branch — that's where new work is built up from.
        ctx.git.worktree_add(p, bottom)?;
    } else {
        // No worktree: switch the current working tree to the bottom branch.
        ctx.git.checkout(bottom)?;
    }

    ctx.git.enable_rerere().ok();

    file.stacks.push(Stack {
        prefix: args.prefix,
        trunk: Trunk {
            branch: trunk_branch,
            head: trunk_head,
        },
        branches,
        last_refreshed_at: None,
    });
    ctx.store.save(&file)?;

    eprintln!("✓ initialised stack with {} branch(es)", full_names.len());
    if let Some(p) = &worktree_path {
        eprintln!("✓ worktree created at {}", p.display());
        eprintln!();
        // Final line on stdout so callers can `cd "$(stack init ... | tail -1)"`.
        println!("{}", p.display());
    }
    Ok(())
}
