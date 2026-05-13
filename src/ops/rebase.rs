//! Cascade rebase across a stack.
//!
//! When a parent branch's head moves (because trunk advanced, or because a
//! lower branch was amended), every child needs its base rewritten. We rebase
//! bottom-up: for each branch B with new_base != old_base, run
//! `git rebase --onto <new_base> <old_base> <B>`. After success we update
//! B.base and B.head in the store.
//!
//! Conflicts: when rebase exits non-zero with rebase state on disk, we
//! persist a `stacks.json.rebase` file describing where we were so a later
//! `--continue` can resume the loop.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::domain::{StackError, StackFile};

use super::walk::{rebase_plan, RebaseItem};
use super::Context;

pub use super::walk::Scope;

pub struct RebaseArgs {
    pub scope: Scope,
    pub cont: bool,
    pub abort: bool,
    pub remote: String,
    pub no_refresh: bool,
}

#[derive(Serialize, Deserialize)]
struct RebaseState {
    stack_index: usize,
    /// Indices into `stack.branches` still to process, in order.
    remaining: Vec<usize>,
    /// The branch currently mid-rebase (its base/head will be updated on continue).
    current: PersistedItem,
    /// Branches already completed in this rebase (so we can record their new bases on continue).
    completed: Vec<PersistedItem>,
}

#[derive(Serialize, Deserialize, Clone)]
struct PersistedItem {
    branch_index: usize,
    branch: String,
    new_base: String,
    old_base: String,
}

impl From<&RebaseItem> for PersistedItem {
    fn from(p: &RebaseItem) -> Self {
        Self {
            branch_index: p.branch_index,
            branch: p.branch.clone(),
            new_base: p.new_base.clone(),
            old_base: p.old_base.clone(),
        }
    }
}

impl From<PersistedItem> for RebaseItem {
    fn from(p: PersistedItem) -> Self {
        Self {
            branch_index: p.branch_index,
            branch: p.branch,
            new_base: p.new_base,
            old_base: p.old_base,
        }
    }
}

fn state_path(ctx: &Context) -> PathBuf {
    ctx.store.paths().dir.join("stacks.json.rebase")
}

pub fn run(args: RebaseArgs) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let ctx = Context::open(&cwd)?;

    if args.abort {
        ctx.git.rebase_abort()?;
        let _ = fs::remove_file(state_path(&ctx));
        eprintln!("✓ rebase aborted");
        return Ok(());
    }

    if args.cont {
        return continue_rebase(&ctx);
    }

    if ctx.git.rebase_in_progress() {
        return Err(StackError::RebaseInProgress);
    }

    // We already opened the Context to handle --abort / --continue above;
    // re-enter via current_stack to pick up the resolved-stack handle.
    drop(ctx);
    let mut cs = Context::current_stack(&cwd)?;

    // Refresh PR state first so merged branches are skipped during planning.
    // Without this, cascade rebase tries to replay squash-merged commits and
    // conflicts against their already-in-trunk equivalents.
    if !args.no_refresh {
        let gh = crate::github::GhCli::new(&cwd);
        let n = super::pr_refresh::refresh_stack(&gh, &mut cs.file, cs.stack_index)?;
        cs.save()?;
        if n > 0 {
            eprintln!("ℹ {n} PR(s) merged on GitHub since last refresh");
        }
    }

    let plan = rebase_plan(
        &cs.ctx.git,
        &cs.file.stacks[cs.stack_index],
        args.scope,
        &cs.current_branch,
    )?;
    if plan.is_empty() {
        eprintln!("✓ stack already up to date");
        return Ok(());
    }

    execute(&cs.ctx, &mut cs.file, cs.stack_index, plan)?;
    Ok(())
}

fn execute(
    ctx: &Context,
    file: &mut StackFile,
    stack_index: usize,
    plan: Vec<RebaseItem>,
) -> Result<(), StackError> {
    let mut completed: Vec<PersistedItem> = Vec::new();
    let mut remaining: Vec<RebaseItem> = plan;

    while let Some(item) = remaining.first().cloned() {
        // Re-derive new_base from the parent's current head (it may have moved
        // because we just rebased the parent).
        let new_base = if item.branch_index == 0 {
            ctx.git.rev_parse(&file.stacks[stack_index].trunk.branch)?
        } else {
            ctx.git
                .rev_parse(&file.stacks[stack_index].branches[item.branch_index - 1].branch)?
        };
        let live = RebaseItem {
            new_base,
            ..item.clone()
        };

        match ctx
            .git
            .rebase_onto(&live.new_base, &live.old_base, &live.branch)
        {
            Ok(()) => {
                let new_head = ctx.git.rev_parse(&live.branch)?;
                let b = &mut file.stacks[stack_index].branches[live.branch_index];
                b.base = live.new_base.clone();
                b.head = Some(new_head);
                ctx.store.save(file)?;
                eprintln!(
                    "✓ rebased {} onto {}",
                    live.branch,
                    &live.new_base[..7.min(live.new_base.len())]
                );
                completed.push((&live).into());
                remaining.remove(0);
            }
            Err(StackError::RebaseConflict) => {
                let state = RebaseState {
                    stack_index,
                    remaining: remaining[1..].iter().map(|p| p.branch_index).collect(),
                    current: (&live).into(),
                    completed,
                };
                fs::write(state_path(ctx), serde_json::to_vec_pretty(&state)?)?;
                eprintln!(
                    "✗ conflict while rebasing {}; resolve and run `stack rebase --continue`",
                    state.current.branch
                );
                return Err(StackError::RebaseConflict);
            }
            Err(e) => return Err(e),
        }
    }

    let _ = fs::remove_file(state_path(ctx));
    Ok(())
}

fn continue_rebase(ctx: &Context) -> Result<(), StackError> {
    let path = state_path(ctx);
    if !path.exists() {
        // No persisted state; just defer to git.
        ctx.git.rebase_continue()?;
        return Ok(());
    }
    let bytes = fs::read(&path)?;
    let state: RebaseState = serde_json::from_slice(&bytes)?;
    let mut file = ctx.store.load(&ctx.identity)?;

    // Finish the in-flight rebase.
    ctx.git.rebase_continue()?;
    let new_head = ctx.git.rev_parse(&state.current.branch)?;
    let b = &mut file.stacks[state.stack_index].branches[state.current.branch_index];
    b.base = state.current.new_base.clone();
    b.head = Some(new_head);
    ctx.store.save(&file)?;
    eprintln!("✓ resumed rebase of {}", state.current.branch);

    // Reconstruct remaining plan from indices.
    let remaining_plan: Vec<RebaseItem> = state
        .remaining
        .iter()
        .map(|&i| {
            let b = &file.stacks[state.stack_index].branches[i];
            RebaseItem {
                branch_index: i,
                branch: b.branch.clone(),
                new_base: String::new(),
                old_base: b.base.clone(),
            }
        })
        .collect();

    fs::remove_file(&path)?;
    execute(ctx, &mut file, state.stack_index, remaining_plan)?;
    Ok(())
}
