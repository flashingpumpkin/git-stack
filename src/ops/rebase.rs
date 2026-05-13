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

use super::Context;

#[derive(Debug, Clone, Copy)]
pub enum Scope {
    Full,
    Upstack,
    Downstack,
}

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
    current: PlanItem,
    /// Branches already completed in this rebase (so we can record their new bases on continue).
    completed: Vec<PlanItem>,
}

#[derive(Serialize, Deserialize, Clone)]
struct PlanItem {
    branch_index: usize,
    branch: String,
    new_base: String,
    old_base: String,
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
    super::pr_refresh::refresh_unless(
        args.no_refresh,
        &cwd,
        &cs.ctx,
        &mut cs.file,
        cs.stack_index,
    )?;

    let plan = build_plan(
        &cs.ctx,
        &cs.file,
        cs.stack_index,
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

fn build_plan(
    ctx: &Context,
    file: &StackFile,
    stack_index: usize,
    scope: Scope,
    current: &str,
) -> Result<Vec<PlanItem>, StackError> {
    let stack = &file.stacks[stack_index];
    let current_pos = stack
        .position(current)
        .ok_or_else(|| StackError::Other("current branch not found in stack".into()))?;

    let (start, end) = match scope {
        Scope::Full => (0, stack.branches.len()),
        Scope::Downstack => (0, current_pos + 1),
        Scope::Upstack => (current_pos, stack.branches.len()),
    };

    // Shared walk produces per-branch live state. The rebase plan is "every
    // active branch in scope whose live parent doesn't match the stored
    // base" — same question view's needs-rebase indicator asks.
    let statuses = super::walk::walk(&ctx.git, stack)?;
    let mut plan = Vec::new();
    for status in statuses.iter().skip(start).take(end - start) {
        if status.is_merged {
            continue;
        }
        let Some(parent_head) = status.live_parent_head.as_ref() else {
            continue;
        };
        if parent_head != &status.stored_base {
            plan.push(PlanItem {
                branch_index: status.index,
                branch: status.branch.clone(),
                new_base: parent_head.clone(),
                old_base: status.stored_base.clone(),
            });
        }
    }
    Ok(plan)
}

fn execute(
    ctx: &Context,
    file: &mut StackFile,
    stack_index: usize,
    plan: Vec<PlanItem>,
) -> Result<(), StackError> {
    let mut completed: Vec<PlanItem> = Vec::new();
    let mut remaining: Vec<PlanItem> = plan;

    while let Some(item) = remaining.first().cloned() {
        // Re-derive new_base from the parent's current head (it may have moved
        // because we just rebased the parent).
        let new_base = if item.branch_index == 0 {
            ctx.git.rev_parse(&file.stacks[stack_index].trunk.branch)?
        } else {
            ctx.git
                .rev_parse(&file.stacks[stack_index].branches[item.branch_index - 1].branch)?
        };
        let live = PlanItem {
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
                completed.push(live);
                remaining.remove(0);
            }
            Err(StackError::RebaseConflict) => {
                let state = RebaseState {
                    stack_index,
                    remaining: remaining[1..].iter().map(|p| p.branch_index).collect(),
                    current: live,
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
    let remaining_plan: Vec<PlanItem> = state
        .remaining
        .iter()
        .map(|&i| {
            let b = &file.stacks[state.stack_index].branches[i];
            PlanItem {
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
