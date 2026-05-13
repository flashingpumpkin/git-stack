//! Live per-branch state for a stack, answered as two narrow questions
//! rather than a fact-sheet.
//!
//! - `live_status` answers "for each branch, what should the indicator say?"
//!   (used by `view`). Exposes only what presentation needs.
//! - `rebase_plan` answers "which branches need rebasing, with their old/new
//!   bases?" (used by `rebase`). Honours scope (full / upstack / downstack).
//!
//! Both share an internal walk that steps the parent ref forward across the
//! stack, skipping merged branches — the rule that view's "needs rebase"
//! indicator and rebase's planner have to agree on.

use crate::domain::{Stack, StackError};
use crate::git::GitOps;

/// Per-branch presentation status (for `view`).
#[derive(Debug, Clone)]
pub struct BranchHealth {
    pub branch: String,
    pub is_merged: bool,
    pub live_head: Option<String>,
    pub needs_rebase: bool,
}

/// One step in a cascade rebase plan.
#[derive(Debug, Clone)]
pub struct RebaseItem {
    /// Stack-relative index, needed to update `branches[i].base/head` after success.
    pub branch_index: usize,
    pub branch: String,
    /// Live head of the live parent; becomes the new stored base.
    pub new_base: String,
    /// Currently stored base — `git rebase --onto NEW OLD <branch>` argument.
    pub old_base: String,
}

/// Scope of a rebase plan — which slice of the stack to consider.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub enum Scope {
    Full,
    Upstack,
    Downstack,
}

/// One walk step's facts. Private — callers consume `BranchHealth` or
/// `RebaseItem` instead.
struct Step {
    index: usize,
    branch: String,
    is_merged: bool,
    live_parent_head: Option<String>,
    live_head: Option<String>,
    stored_base: String,
    needs_rebase: bool,
}

fn walk(git: &dyn GitOps, stack: &Stack, remote: &str) -> Result<Vec<Step>, StackError> {
    let mut out = Vec::with_capacity(stack.branches.len());
    // Bottom branch's parent is the trunk. Prefer the remote-tracking ref
    // (`<remote>/<trunk>`) so the cascade rebases onto whatever the remote
    // currently has, without us ever writing to the local trunk ref.
    // Falls back to the local trunk for local-only repos where there's no
    // remote-tracking ref.
    let remote_trunk = format!("{remote}/{}", stack.trunk.branch);
    let mut parent_ref: String = if git.rev_parse(&remote_trunk).is_ok() {
        remote_trunk
    } else {
        stack.trunk.branch.clone()
    };
    for (i, b) in stack.branches.iter().enumerate() {
        let live_head = git.rev_parse(&b.branch).ok();
        let live_parent_head = git.rev_parse(&parent_ref).ok();
        let needs_rebase = if b.is_merged() {
            // Merged branches are skipped during rebase; the indicator
            // shouldn't draw attention to them either.
            false
        } else {
            match (live_parent_head.as_deref(), live_head.as_deref()) {
                (Some(parent), Some(head)) => !git.is_ancestor(parent, head).unwrap_or(true),
                _ => false,
            }
        };
        out.push(Step {
            index: i,
            branch: b.branch.clone(),
            is_merged: b.is_merged(),
            live_parent_head,
            live_head,
            stored_base: b.base.clone(),
            needs_rebase,
        });
        // Next branch's parent is this branch — unless this one is merged,
        // in which case it stays at the previous non-merged branch (or trunk).
        if !b.is_merged() {
            parent_ref = b.branch.clone();
        }
    }
    Ok(out)
}

/// What `view` needs: presentation status per branch, in stack order.
/// `remote` is the remote name (e.g. "origin"); the walk prefers
/// `<remote>/<trunk>` over the local trunk ref to avoid being misled by
/// a stale local trunk.
pub fn live_status(
    git: &dyn GitOps,
    stack: &Stack,
    remote: &str,
) -> Result<Vec<BranchHealth>, StackError> {
    Ok(walk(git, stack, remote)?
        .into_iter()
        .map(|s| BranchHealth {
            branch: s.branch,
            is_merged: s.is_merged,
            live_head: s.live_head,
            needs_rebase: s.needs_rebase,
        })
        .collect())
}

/// What `rebase` needs: every active branch in `scope` whose live parent
/// doesn't match its stored base. Same question view's needs-rebase
/// indicator asks, narrowed by scope.
pub fn rebase_plan(
    git: &dyn GitOps,
    stack: &Stack,
    scope: Scope,
    current: &str,
    remote: &str,
) -> Result<Vec<RebaseItem>, StackError> {
    let current_pos = stack
        .position(current)
        .ok_or_else(|| StackError::Other("current branch not found in stack".into()))?;
    let (start, end) = match scope {
        Scope::Full => (0, stack.branches.len()),
        Scope::Downstack => (0, current_pos + 1),
        Scope::Upstack => (current_pos, stack.branches.len()),
    };

    let mut plan = Vec::new();
    for step in walk(git, stack, remote)?
        .into_iter()
        .skip(start)
        .take(end - start)
    {
        if step.is_merged {
            continue;
        }
        let Some(parent_head) = step.live_parent_head else {
            continue;
        };
        if parent_head != step.stored_base {
            plan.push(RebaseItem {
                branch_index: step.index,
                branch: step.branch,
                new_base: parent_head,
                old_base: step.stored_base,
            });
        }
    }
    Ok(plan)
}
