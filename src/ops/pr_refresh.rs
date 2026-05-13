//! Refresh stored PR state for one stack against GitHub.
//!
//! Pure over the `GitHub` port — does no I/O beyond what the port exposes,
//! does not persist. Callers are responsible for saving the `StackFile` and
//! for emitting any user-facing "N PR(s) merged" message (context varies:
//! `submit` says "since last refresh", `prune` aggregates across stacks).
//!
//! Two passes per refresh:
//!
//!   1. **Discovery.** For each branch with no `pullRequest` in the store,
//!      ask `gh` whether a PR with that branch as its head ref exists. If
//!      so, record it. Catches PRs opened via the GitHub UI or
//!      `gh pr create` directly — without this step, `prune` would never
//!      drop a stack whose PRs we hadn't authored ourselves.
//!   2. **Refresh.** For each known PR, ask `gh` for the current state and
//!      update the stored `merged` flag.

use crate::clock::now_rfc3339;
use crate::domain::{PullRequest, StackError, StackFile};
use crate::github::{GitHub, PrState};

/// Refresh PR state for `stack_index` via `gh`. Returns the number of PRs
/// whose `merged` flag flipped from false to true *during this call* —
/// counting both newly-discovered already-merged PRs and existing PRs
/// that merged on GitHub since the last refresh.
pub fn refresh_stack(
    gh: &dyn GitHub,
    file: &mut StackFile,
    stack_index: usize,
) -> Result<usize, StackError> {
    let mut newly_merged = 0usize;

    // Pass 1: discovery. Branches without a recorded PR get one if `gh`
    // finds a matching head ref. Best-effort: a failure here (no network,
    // no `gh` auth, repo isn't a real GitHub repo) silently skips
    // discovery — refresh still proceeds for any PRs we already know about.
    let unknown: Vec<(usize, String)> = file.stacks[stack_index]
        .branches
        .iter()
        .enumerate()
        .filter(|(_, b)| b.pull_request.is_none())
        .map(|(i, b)| (i, b.branch.clone()))
        .collect();
    for (i, branch) in unknown {
        let Ok(maybe) = gh.find_pr_by_head(&branch) else {
            // gh returned an error — likely no auth or repo not on GitHub.
            // Skip the rest of discovery; whatever made the first call
            // fail will fail the rest too.
            break;
        };
        if let Some(info) = maybe {
            let merged = info.state == PrState::Merged;
            file.stacks[stack_index].branches[i].pull_request = Some(PullRequest {
                number: info.number,
                id: info.id,
                url: info.url,
                merged,
            });
            if merged {
                newly_merged += 1;
            }
        }
    }

    // Pass 2: refresh state for every known PR (including the ones we just
    // discovered, which is harmless since `find_pr_by_head` already gave
    // us their current state — but it keeps the code path uniform).
    let numbers: Vec<u64> = file.stacks[stack_index]
        .branches
        .iter()
        .filter_map(|b| b.pull_request.as_ref().map(|p| p.number))
        .collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Branch, Stack, Trunk, SCHEMA_VERSION};
    use crate::github::fake::FakeGitHub;
    use crate::github::PullRequestInfo;

    fn b(name: &str) -> Branch {
        Branch {
            branch: name.into(),
            head: Some("h".into()),
            base: "x".into(),
            pull_request: None,
        }
    }

    fn file_with(branches: Vec<Branch>) -> StackFile {
        StackFile {
            schema_version: SCHEMA_VERSION,
            repository: "test".into(),
            stacks: vec![Stack {
                prefix: None,
                trunk: Trunk {
                    branch: "main".into(),
                    head: "t".into(),
                },
                branches,
                last_refreshed_at: None,
            }],
        }
    }

    #[test]
    fn discovers_pr_for_branch_without_one_and_records_merged_state() {
        // The bug from the field: a branch with no `pullRequest` in the
        // store even though gh knows there's a merged PR for its head ref.
        // After refresh, the branch should be marked merged so prune can
        // see the stack as fully-merged.
        let mut file = file_with(vec![b("feat/a")]);
        let gh = FakeGitHub::new();
        gh.seed_pr(PullRequestInfo {
            number: 7,
            id: "PR_7".into(),
            url: "https://github.com/x/y/pull/7".into(),
            state: PrState::Merged,
            head_ref: "feat/a".into(),
            base_ref: "main".into(),
            comment_count: 0,
            mergeable: None,
        });

        let n = refresh_stack(&gh, &mut file, 0).unwrap();

        let pr = file.stacks[0].branches[0].pull_request.as_ref().unwrap();
        assert_eq!(pr.number, 7);
        assert!(pr.merged);
        assert_eq!(n, 1, "newly-merged count should include the discovered PR");
    }

    #[test]
    fn discovery_prefers_open_then_highest_number() {
        let mut file = file_with(vec![b("feat/a")]);
        let gh = FakeGitHub::new();
        // Three PRs for the same head ref: a closed one, a merged older
        // one, and an open newer one. The open one wins.
        gh.seed_pr(PullRequestInfo {
            number: 10,
            id: "PR_10".into(),
            url: "u10".into(),
            state: PrState::Closed,
            head_ref: "feat/a".into(),
            base_ref: "main".into(),
            comment_count: 0,
            mergeable: None,
        });
        gh.seed_pr(PullRequestInfo {
            number: 11,
            id: "PR_11".into(),
            url: "u11".into(),
            state: PrState::Merged,
            head_ref: "feat/a".into(),
            base_ref: "main".into(),
            comment_count: 0,
            mergeable: None,
        });
        gh.seed_pr(PullRequestInfo {
            number: 12,
            id: "PR_12".into(),
            url: "u12".into(),
            state: PrState::Open,
            head_ref: "feat/a".into(),
            base_ref: "main".into(),
            comment_count: 0,
            mergeable: None,
        });

        refresh_stack(&gh, &mut file, 0).unwrap();
        let pr = file.stacks[0].branches[0].pull_request.as_ref().unwrap();
        assert_eq!(pr.number, 12);
        assert!(!pr.merged);
    }

    #[test]
    fn discovery_is_noop_when_no_matching_pr_exists() {
        let mut file = file_with(vec![b("feat/a")]);
        let gh = FakeGitHub::new();
        refresh_stack(&gh, &mut file, 0).unwrap();
        assert!(file.stacks[0].branches[0].pull_request.is_none());
    }
}
