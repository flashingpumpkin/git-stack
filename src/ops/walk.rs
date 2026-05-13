//! Walk a stack bottom-up, asking git for live state.
//!
//! Both `view`'s "needs rebase" indicator and `rebase`'s cascade planner
//! need the same per-branch facts: what's the live parent ref, what's its
//! head SHA, what's the branch's own head, do they agree. Centralising the
//! walk keeps the two consumers honest about the same semantics — the
//! recent false-positive "needs rebase" fix would have been a one-line
//! change here instead of needing two parallel edits.
//!
//! The walk uses *live* parent refs (not the stored `base` SHA), which is
//! the user-facing definition of "needs rebase". Stored bases are still
//! exposed for callers like the rebase planner that need them as
//! `git rebase --onto OLD <branch>` arguments.

use crate::domain::{Stack, StackError};
use crate::git::Git;

/// What we know about one branch after asking git.
#[derive(Debug, Clone)]
pub struct BranchStatus {
    /// Stack-relative index. Useful for the rebase planner.
    pub index: usize,
    /// Full branch name (with prefix if the stack has one).
    pub branch: String,
    /// True when this branch's tracked PR has merged.
    pub is_merged: bool,
    /// The previous *active* branch in the stack, or trunk for the bottom.
    /// This is what we ask git about when computing `needs_rebase`.
    pub live_parent_ref: String,
    /// Live head of `live_parent_ref`, when resolvable.
    pub live_parent_head: Option<String>,
    /// Live head of `branch`, when resolvable.
    pub live_head: Option<String>,
    /// Base SHA as recorded in `stacks.json`. The rebase planner uses
    /// this as the `OLD` argument to `git rebase --onto NEW OLD <branch>`.
    pub stored_base: String,
    /// True when `live_parent_head` is not an ancestor of `live_head`.
    /// The user-facing meaning: would running `stack rebase` change
    /// anything for this branch.
    pub needs_rebase: bool,
}

/// Walk the stack bottom-up. The result is in stack order (bottom first),
/// matching `stack.branches`.
pub fn walk(git: &Git, stack: &Stack) -> Result<Vec<BranchStatus>, StackError> {
    let mut out = Vec::with_capacity(stack.branches.len());
    let mut parent_ref: String = stack.trunk.branch.clone();
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
        out.push(BranchStatus {
            index: i,
            branch: b.branch.clone(),
            is_merged: b.is_merged(),
            live_parent_ref: parent_ref.clone(),
            live_parent_head,
            live_head,
            stored_base: b.base.clone(),
            needs_rebase,
        });
        // Next branch's parent is this branch — unless this one is merged,
        // in which case it stays at the previous non-merged branch (or
        // trunk). Matches the merge-skip rule that both view and rebase
        // already implement.
        if !b.is_merged() {
            parent_ref = b.branch.clone();
        }
    }
    Ok(out)
}
