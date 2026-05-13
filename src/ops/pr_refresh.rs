//! Refresh stored PR state for one stack against GitHub.
//!
//! Pure over the `GitHub` port — does no I/O beyond what the port exposes,
//! does not persist. Callers are responsible for saving the `StackFile` and
//! for emitting any user-facing "N PR(s) merged" message (context varies:
//! `submit` says "since last refresh", `prune` aggregates across stacks).

use crate::clock::now_rfc3339;
use crate::domain::{StackError, StackFile};
use crate::github::{GitHub, PrState};

/// Refresh PR state for `stack_index` via `gh`. Returns the number of PRs
/// whose `merged` flag flipped from false to true.
pub fn refresh_stack(
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

/// Refresh every stack in the file. Returns total newly-merged count.
pub fn refresh_all(gh: &dyn GitHub, file: &mut StackFile) -> Result<usize, StackError> {
    let mut total = 0usize;
    for idx in 0..file.stacks.len() {
        total += refresh_stack(gh, file, idx)?;
    }
    Ok(total)
}
