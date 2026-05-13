//! Shared helper: refresh stored PR state for one stack against GitHub.
//!
//! Two entry points:
//!
//! - `refresh_unless` is the production wrapper used by sync/rebase/view/
//!   prune. It constructs `GhCli`, calls the inner helper, persists, and
//!   emits the canonical "N PR(s) merged on GitHub since last refresh"
//!   message.
//! - `refresh_stack_with` is the port-friendly version used by `submit`'s
//!   testable orchestration. Tests pass in a `FakeGitHub`.
//!
//! Both ultimately call the same inner walk so the per-PR behaviour, the
//! `last_refreshed_at` stamp, and the "newly merged" count stay consistent.

use std::path::Path;

use crate::clock::now_rfc3339;
use crate::domain::{StackError, StackFile};
use crate::github::{GhCli, GitHub, PrState};

use super::Context;

/// Production wrapper: if `no_refresh` is false, fetch PR state for the
/// given stack via the `gh` CLI, persist, and report the newly-merged
/// count. Returns the number of PRs whose `merged` flag flipped, so
/// callers can short-circuit further work (rebase plans, etc.) when
/// nothing's changed and they want to.
pub fn refresh_unless(
    no_refresh: bool,
    cwd: &Path,
    ctx: &Context,
    file: &mut StackFile,
    stack_index: usize,
) -> Result<usize, StackError> {
    if no_refresh {
        return Ok(0);
    }
    let gh = GhCli::new(cwd);
    let newly_merged = refresh_stack_with(&gh, file, stack_index)?;
    ctx.store.save(file)?;
    if newly_merged > 0 {
        eprintln!("ℹ {newly_merged} PR(s) merged on GitHub since last refresh");
    }
    Ok(newly_merged)
}

/// Like `refresh_unless`, but walks every tracked stack in the file.
/// Used by `prune` which makes decisions across all stacks. Skips the
/// save when `also_skip_save` is true so the caller can chain the
/// refresh into a larger transaction.
pub fn refresh_all_unless(
    no_refresh: bool,
    cwd: &Path,
    ctx: &Context,
    file: &mut StackFile,
    also_skip_save: bool,
) -> Result<usize, StackError> {
    if no_refresh {
        return Ok(0);
    }
    let gh = GhCli::new(cwd);
    let mut total = 0usize;
    for idx in 0..file.stacks.len() {
        total += refresh_stack_with(&gh, file, idx)?;
    }
    if !also_skip_save {
        ctx.store.save(file)?;
    }
    if total > 0 {
        eprintln!("ℹ {total} PR(s) merged on GitHub since last refresh");
    }
    Ok(total)
}

/// Port-friendly version: caller supplies the `GitHub` adapter, the save
/// happens elsewhere. Used by `submit::submit` which threads a fake
/// `GitHub` through for unit tests.
pub fn refresh_stack_with(
    gh: &dyn GitHub,
    file: &mut StackFile,
    stack_index: usize,
) -> Result<usize, StackError> {
    let numbers: Vec<u64> = file.stacks[stack_index]
        .branches
        .iter()
        .filter(|b| !b.is_merged())
        .filter_map(|b| b.pull_request.as_ref().map(|p| p.number))
        .collect();
    if numbers.is_empty() {
        file.stacks[stack_index].last_refreshed_at = Some(now_rfc3339());
        return Ok(0);
    }

    let mut newly_merged = 0usize;
    for n in &numbers {
        let info = gh.view_pr(*n)?;
        if let Some(b) = file.stacks[stack_index]
            .branches
            .iter_mut()
            .find(|b| b.pull_request.as_ref().is_some_and(|p| p.number == *n))
        {
            if let Some(pr) = b.pull_request.as_mut() {
                let was_merged = pr.merged;
                pr.merged = info.state == PrState::Merged;
                pr.url = info.url;
                pr.id = info.id;
                if !was_merged && pr.merged {
                    newly_merged += 1;
                }
            }
        }
    }
    file.stacks[stack_index].last_refreshed_at = Some(now_rfc3339());
    Ok(newly_merged)
}
