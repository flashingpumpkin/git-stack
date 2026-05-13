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
    /// Scope of the cascade that was in progress; re-applied on `--continue`
    /// after the conflicting branch finalises.
    scope: Scope,
    /// Branch the user was on when they invoked `stack rebase`. Re-planning
    /// on continue needs this to compute upstack/downstack slices.
    current_branch: String,
    /// The branch currently mid-rebase. Its base/head are written to the
    /// store once `git rebase --continue` succeeds.
    in_flight: PersistedItem,
}

#[derive(Serialize, Deserialize, Clone)]
struct PersistedItem {
    branch_index: usize,
    branch: String,
    new_base: String,
}

impl From<&RebaseItem> for PersistedItem {
    fn from(p: &RebaseItem) -> Self {
        Self {
            branch_index: p.branch_index,
            branch: p.branch.clone(),
            new_base: p.new_base.clone(),
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

    let did_any = execute(
        &cs.ctx,
        &mut cs.file,
        cs.stack_index,
        args.scope,
        &cs.current_branch,
    )?;
    if !did_any {
        eprintln!("✓ stack already up to date");
    }
    Ok(())
}

/// Drive the cascade by re-planning after each successful rebase. The plan
/// can grow stale after a single step — once branch N is rebased, branch
/// N+1's stored base no longer matches its live parent, but `rebase_plan`
/// hadn't seen that mismatch at the start. Re-planning is the simplest fix:
/// each iteration sees the post-rebase state.
///
/// Returns `true` if at least one branch was rebased.
fn execute(
    ctx: &Context,
    file: &mut StackFile,
    stack_index: usize,
    scope: Scope,
    current: &str,
) -> Result<bool, StackError> {
    let mut did_any = false;
    loop {
        let plan = rebase_plan(&ctx.git, &file.stacks[stack_index], scope, current)?;
        let Some(item) = plan.first().cloned() else {
            break;
        };

        match ctx
            .git
            .rebase_onto(&item.new_base, &item.old_base, &item.branch)
        {
            Ok(()) => {
                let new_head = ctx.git.rev_parse(&item.branch)?;
                let b = &mut file.stacks[stack_index].branches[item.branch_index];
                b.base = item.new_base.clone();
                b.head = Some(new_head);
                ctx.store.save(file)?;
                eprintln!(
                    "✓ rebased {} onto {}",
                    item.branch,
                    &item.new_base[..7.min(item.new_base.len())]
                );
                did_any = true;
            }
            Err(StackError::RebaseConflict) => {
                // Persist the in-flight branch so `--continue` can finalise
                // its store update. The remaining work is re-planned from
                // scratch after that, so we don't bother recording it here.
                let state = RebaseState {
                    stack_index,
                    scope,
                    current_branch: current.to_string(),
                    in_flight: (&item).into(),
                };
                fs::write(state_path(ctx), serde_json::to_vec_pretty(&state)?)?;
                eprintln!(
                    "✗ conflict while rebasing {}; resolve and run `stack rebase --continue`",
                    item.branch
                );
                return Err(StackError::RebaseConflict);
            }
            Err(e) => return Err(e),
        }
    }
    let _ = fs::remove_file(state_path(ctx));
    Ok(did_any)
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

    // Finalise the in-flight rebase: tell git to continue, then write the
    // new base/head into the store.
    ctx.git.rebase_continue()?;
    let new_head = ctx.git.rev_parse(&state.in_flight.branch)?;
    let b = &mut file.stacks[state.stack_index].branches[state.in_flight.branch_index];
    b.base = state.in_flight.new_base.clone();
    b.head = Some(new_head);
    ctx.store.save(&file)?;
    eprintln!("✓ resumed rebase of {}", state.in_flight.branch);

    fs::remove_file(&path)?;

    // Re-plan from the post-resolution state and drive the cascade to
    // completion. A branch the user manually fixed up so that it no longer
    // needs rebasing will simply not appear in the new plan.
    execute(
        ctx,
        &mut file,
        state.stack_index,
        state.scope,
        &state.current_branch,
    )?;
    Ok(())
}
