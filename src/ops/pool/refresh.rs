//! Refresh PR state for a pool: discover PRs by head ref, update merged
//! flags, update comment counts, and report branches whose comment count
//! moved.
//!
//! The pure entry point (`refresh_pool`) is adapter-agnostic so tests can
//! drive it with a `FakeGitHub`. The CLI wrapper at the bottom resolves
//! the pool, calls in, and saves.

use crate::clock::now_rfc3339;
use crate::domain::{PoolFile, PoolPullRequest, StackError};
use crate::github::{GitHub, PrState};

/// One per-branch report line from a refresh.
#[derive(Debug, Clone)]
pub struct PoolRefreshChange {
    pub branch: String,
    pub kind: PoolRefreshKind,
}

#[derive(Debug, Clone)]
pub enum PoolRefreshKind {
    /// A PR was discovered for a branch that previously had none.
    PrDiscovered { number: u64 },
    /// The PR transitioned from open → merged since the last refresh.
    Merged { number: u64 },
    /// New comments arrived since the last `mark_seen`.
    NewComments { number: u64, new: u64, total: u64 },
    /// The PR's mergeable status changed (e.g. became CONFLICTING after a
    /// trunk push). Surfaced so users can plan an early rebase.
    MergeableChanged {
        number: u64,
        mergeable: Option<bool>,
    },
    /// The branch's base was merged on GitHub; we walked up the chain
    /// and reparented onto a non-merged ancestor. `pool rebase` will
    /// rebase onto this new base.
    Reparented { from: String, to: String },
}

#[derive(Debug, Default, Clone)]
pub struct PoolRefreshReport {
    pub changes: Vec<PoolRefreshChange>,
    /// Branches whose PRs are now merged. Caller decides whether to drop
    /// them from tracking — `pool list` keeps them in the "merged" section
    /// while `pool refresh` (the dedicated command) prunes them.
    pub newly_merged_branches: Vec<String>,
}

/// Walk up the base-ref chain on GitHub, skipping branches whose PRs are
/// merged. Returns the first ancestor that is *not* merged (or the
/// original ref when nothing along the chain is merged).
///
/// Cap recursion at `max_depth` so a misconfigured PR graph can't trap us
/// in an infinite walk. Five is plenty for any sane workflow — even
/// deeply-stacked review chains usually bottom out within two hops.
fn resolve_effective_base(
    gh: &dyn GitHub,
    base_ref: &str,
    max_depth: usize,
) -> Result<String, StackError> {
    let mut current = base_ref.to_string();
    for _ in 0..max_depth {
        let pr = gh.find_pr_by_head(&current)?;
        match pr {
            Some(p) if p.state == PrState::Merged => {
                // The base was merged into `p.base_ref`. Walk one more
                // step; the new candidate may itself be merged.
                if p.base_ref == current {
                    // Self-loop guard; bail.
                    break;
                }
                current = p.base_ref;
            }
            _ => return Ok(current),
        }
    }
    Ok(current)
}

/// Refresh one pool. Does NOT touch the seen baseline — see `mark_seen`.
pub fn refresh_pool(
    gh: &dyn GitHub,
    file: &mut PoolFile,
    pool_index: usize,
) -> Result<PoolRefreshReport, StackError> {
    let mut report = PoolRefreshReport::default();
    let n_branches = file.pools[pool_index].branches.len();
    for i in 0..n_branches {
        let (branch_name, has_pr, stored_number, was_merged, prev_count, prev_mergeable) = {
            let b = &file.pools[pool_index].branches[i];
            (
                b.branch.clone(),
                b.pull_request.is_some(),
                b.pull_request.as_ref().map(|p| p.number),
                b.pull_request.as_ref().is_some_and(|p| p.merged),
                b.pull_request
                    .as_ref()
                    .map(|p| p.comment_count)
                    .unwrap_or(0),
                b.pull_request.as_ref().and_then(|p| p.mergeable),
            )
        };

        let info_opt = if has_pr {
            // We always have a number when we have a PR.
            Some(gh.view_pr(stored_number.unwrap())?)
        } else {
            gh.find_pr_by_head(&branch_name)?
        };

        let Some(info) = info_opt else {
            // Branch has no PR at all. Even so, its stored base might
            // have been merged since the last refresh — walk up so the
            // next `pool rebase` reparents onto the right ancestor.
            let stored_base = file.pools[pool_index].branches[i].base_branch.clone();
            if !stored_base.is_empty() {
                let resolved = resolve_effective_base(gh, &stored_base, 5)?;
                if resolved != stored_base {
                    file.pools[pool_index].branches[i].base_branch = resolved.clone();
                    report.changes.push(PoolRefreshChange {
                        branch: branch_name.clone(),
                        kind: PoolRefreshKind::Reparented {
                            from: stored_base,
                            to: resolved,
                        },
                    });
                }
            }
            continue;
        };

        let merged_now = info.state == PrState::Merged;
        let new_base_ref = info.base_ref.clone();
        let new_pr = PoolPullRequest {
            number: info.number,
            id: info.id,
            url: info.url,
            merged: merged_now,
            comment_count: info.comment_count,
            // Preserve the existing baseline; only `mark_seen` advances it.
            seen_comment_count: file.pools[pool_index].branches[i]
                .pull_request
                .as_ref()
                .map(|p| p.seen_comment_count)
                .unwrap_or(0),
            mergeable: info.mergeable,
        };

        // Classification before we overwrite.
        if !has_pr {
            report.changes.push(PoolRefreshChange {
                branch: branch_name.clone(),
                kind: PoolRefreshKind::PrDiscovered {
                    number: info.number,
                },
            });
        }
        if !was_merged && merged_now {
            report.changes.push(PoolRefreshChange {
                branch: branch_name.clone(),
                kind: PoolRefreshKind::Merged {
                    number: info.number,
                },
            });
            report.newly_merged_branches.push(branch_name.clone());
        }
        if info.comment_count > prev_count {
            report.changes.push(PoolRefreshChange {
                branch: branch_name.clone(),
                kind: PoolRefreshKind::NewComments {
                    number: info.number,
                    new: info.comment_count - prev_count,
                    total: info.comment_count,
                },
            });
        }
        if info.mergeable != prev_mergeable {
            report.changes.push(PoolRefreshChange {
                branch: branch_name.clone(),
                kind: PoolRefreshKind::MergeableChanged {
                    number: info.number,
                    mergeable: info.mergeable,
                },
            });
        }

        // Mirror the PR's base ref onto the branch's base_branch so a
        // retargeted PR on GitHub gets picked up by the next `pool rebase`.
        // If the chosen base is itself a merged branch, walk up to find
        // the first non-merged ancestor and reparent onto that — mirrors
        // how `stack rebase` skips merged parents in a chain.
        let resolved_base_ref = resolve_effective_base(gh, &new_base_ref, 5)?;
        let prev_base_branch = file.pools[pool_index].branches[i].base_branch.clone();
        let branch_ref = &mut file.pools[pool_index].branches[i];
        if branch_ref.base_branch != resolved_base_ref {
            branch_ref.base_branch = resolved_base_ref.clone();
        }
        branch_ref.pull_request = Some(new_pr);
        if !prev_base_branch.is_empty() && prev_base_branch != resolved_base_ref {
            report.changes.push(PoolRefreshChange {
                branch: branch_name.clone(),
                kind: PoolRefreshKind::Reparented {
                    from: prev_base_branch,
                    to: resolved_base_ref,
                },
            });
        }
    }
    file.pools[pool_index].last_refreshed_at = Some(now_rfc3339());
    Ok(report)
}

/// Advance the "seen comments" baseline for every PR in the pool to its
/// current comment count. Call after presenting unread indicators to the
/// user (`pool list`, `pool refresh`).
pub fn mark_seen(file: &mut PoolFile, pool_index: usize) {
    for b in &mut file.pools[pool_index].branches {
        if let Some(pr) = b.pull_request.as_mut() {
            pr.seen_comment_count = pr.comment_count;
        }
    }
}

/// Drop merged branches from a pool. Returns the names of branches that
/// were removed, in stable order.
pub fn drop_merged(file: &mut PoolFile, pool_index: usize) -> Vec<String> {
    let mut dropped = Vec::new();
    file.pools[pool_index].branches.retain(|b| {
        if b.is_merged() {
            dropped.push(b.branch.clone());
            false
        } else {
            true
        }
    });
    dropped
}

pub struct RefreshArgs {
    pub pool: Option<String>,
}

/// CLI entrypoint: refresh the named (or default) pool, drop merged
/// branches, print a per-branch change log, advance the seen baseline.
pub fn run(args: RefreshArgs) -> Result<(), StackError> {
    use crate::style::{accent, dim, merged as merged_style, ok, secondary, url_err, warn};

    let cwd = std::env::current_dir()?;
    let ctx = crate::ops::Context::open(&cwd)?;
    let mut file = ctx.store.load_pools(&ctx.identity)?;
    let idx = super::resolve_pool_index(&file, args.pool.as_deref())?;
    let label = file.pools[idx].display_label().to_string();

    let gh = crate::github::GhCli::new(&cwd);
    let report = refresh_pool(&gh, &mut file, idx)?;

    if report.changes.is_empty() {
        eprintln!("✓ pool {label} up to date — no changes since last refresh");
    } else {
        eprintln!("Pool {} — changes:", accent(&label));
        for c in &report.changes {
            match &c.kind {
                PoolRefreshKind::PrDiscovered { number } => {
                    eprintln!(
                        "  {} {}  {}  {}",
                        ok("●"),
                        c.branch,
                        secondary(&format!("#{number}")),
                        dim("(PR discovered)"),
                    );
                    if let Some(pr) = file.pools[idx]
                        .branches
                        .iter()
                        .find(|b| b.branch == c.branch)
                        .and_then(|b| b.pull_request.as_ref())
                    {
                        eprintln!("      {}", url_err(&pr.url));
                    }
                }
                PoolRefreshKind::Merged { number } => {
                    eprintln!(
                        "  {} {}  {}  {}",
                        merged_style("✓"),
                        c.branch,
                        secondary(&format!("#{number}")),
                        merged_style("merged"),
                    );
                }
                PoolRefreshKind::NewComments { number, new, total } => {
                    eprintln!(
                        "  {} {}  {}  {}",
                        accent("✉"),
                        c.branch,
                        secondary(&format!("#{number}")),
                        accent(&format!(
                            "{new} new comment{} ({total} total)",
                            if *new == 1 { "" } else { "s" }
                        )),
                    );
                }
                PoolRefreshKind::MergeableChanged { number, mergeable } => {
                    let (glyph_str, msg) = match mergeable {
                        Some(false) => (warn("⚠"), warn("now conflicts with trunk")),
                        Some(true) => (ok("●"), ok("cleanly mergeable")),
                        None => (dim("○"), dim("mergeable state unknown")),
                    };
                    eprintln!(
                        "  {glyph_str} {}  {}  {msg}",
                        c.branch,
                        secondary(&format!("#{number}")),
                    );
                }
                PoolRefreshKind::Reparented { from, to } => {
                    eprintln!(
                        "  {} {}  {} → {} {}",
                        accent("⇡"),
                        c.branch,
                        secondary(from),
                        secondary(to),
                        dim("(base merged; reparented)"),
                    );
                }
            }
        }
    }

    // Don't auto-drop merged branches here — `pool prune` owns that
    // lifecycle step. Refresh is for "what's new"; prune is for "tidy up".
    let _ = label;

    mark_seen(&mut file, idx);
    ctx.store.save_pools(&file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Pool, PoolBranch, PoolPullRequest, Trunk, SCHEMA_VERSION};
    use crate::github::fake::FakeGitHub;
    use crate::github::{PrState, PullRequestInfo};

    fn file_with(branches: Vec<PoolBranch>) -> PoolFile {
        PoolFile {
            schema_version: SCHEMA_VERSION,
            repository: "test".into(),
            pools: vec![Pool {
                name: None,
                trunk: Trunk {
                    branch: "main".into(),
                    head: "trunk_sha".into(),
                },
                branches,
                last_refreshed_at: None,
            }],
        }
    }

    fn pb(name: &str) -> PoolBranch {
        PoolBranch {
            branch: name.into(),
            head: Some("h".into()),
            base_branch: "main".into(),
            base: "trunk_sha".into(),
            pull_request: None,
        }
    }

    fn pr(number: u64, head: &str, state: PrState, comments: u64) -> PullRequestInfo {
        PullRequestInfo {
            number,
            id: format!("PR_{number}"),
            url: format!("https://github.com/x/y/pull/{number}"),
            state,
            head_ref: head.into(),
            base_ref: "main".into(),
            comment_count: comments,
            mergeable: None,
        }
    }

    #[test]
    fn discovers_pr_by_head_when_branch_has_no_stored_pr() {
        let mut file = file_with(vec![pb("feat/a")]);
        let gh = FakeGitHub::new();
        gh.seed_pr(pr(42, "feat/a", PrState::Open, 0));

        let report = refresh_pool(&gh, &mut file, 0).unwrap();

        // The change log mentions discovery.
        let kinds: Vec<_> = report.changes.iter().map(|c| &c.kind).collect();
        assert!(matches!(
            kinds[0],
            PoolRefreshKind::PrDiscovered { number: 42 }
        ));
        // The PR is now stored on the branch.
        let stored = file.pools[0].branches[0].pull_request.as_ref().unwrap();
        assert_eq!(stored.number, 42);
        assert!(!stored.merged);
    }

    #[test]
    fn reports_new_comments_since_last_seen() {
        let mut file = file_with(vec![PoolBranch {
            branch: "feat/a".into(),
            head: Some("h".into()),
            base_branch: "main".into(),
            base: "trunk_sha".into(),
            pull_request: Some(PoolPullRequest {
                number: 1,
                id: "PR_1".into(),
                url: "u".into(),
                merged: false,
                comment_count: 2,
                seen_comment_count: 2,
                mergeable: None,
            }),
        }]);
        let gh = FakeGitHub::new();
        gh.seed_pr(pr(1, "feat/a", PrState::Open, 5));

        let report = refresh_pool(&gh, &mut file, 0).unwrap();
        assert!(report.changes.iter().any(|c| matches!(
            &c.kind,
            PoolRefreshKind::NewComments {
                new: 3,
                total: 5,
                ..
            }
        )));

        // Seen baseline NOT bumped automatically.
        let stored = file.pools[0].branches[0].pull_request.as_ref().unwrap();
        assert_eq!(stored.comment_count, 5);
        assert_eq!(stored.seen_comment_count, 2);

        mark_seen(&mut file, 0);
        let stored = file.pools[0].branches[0].pull_request.as_ref().unwrap();
        assert_eq!(stored.seen_comment_count, 5);
    }

    #[test]
    fn drops_merged_branches() {
        let mut file = file_with(vec![pb("feat/a"), pb("feat/b")]);
        let gh = FakeGitHub::new();
        gh.seed_pr(pr(1, "feat/a", PrState::Merged, 0));
        gh.seed_pr(pr(2, "feat/b", PrState::Open, 0));

        refresh_pool(&gh, &mut file, 0).unwrap();
        let dropped = drop_merged(&mut file, 0);
        assert_eq!(dropped, vec!["feat/a"]);
        let remaining: Vec<_> = file.pools[0]
            .branches
            .iter()
            .map(|b| b.branch.as_str())
            .collect();
        assert_eq!(remaining, vec!["feat/b"]);
    }

    #[test]
    fn no_change_when_nothing_moved() {
        let mut file = file_with(vec![PoolBranch {
            branch: "feat/a".into(),
            head: Some("h".into()),
            base_branch: "main".into(),
            base: "trunk_sha".into(),
            pull_request: Some(PoolPullRequest {
                number: 1,
                id: "PR_1".into(),
                url: "u".into(),
                merged: false,
                comment_count: 3,
                seen_comment_count: 3,
                mergeable: None,
            }),
        }]);
        let gh = FakeGitHub::new();
        gh.seed_pr(pr(1, "feat/a", PrState::Open, 3));

        let report = refresh_pool(&gh, &mut file, 0).unwrap();
        assert!(
            report.changes.is_empty(),
            "no changes expected, got {:?}",
            report.changes
        );
    }

    #[test]
    fn reparents_when_base_branch_was_merged() {
        // Pool branch `feat/child` is based on `feat/parent`. `feat/parent`'s
        // PR has just been merged into `main`. Refresh must rewrite
        // `feat/child.base_branch` to `main` (and surface a Reparented
        // change). This is the mechanism that lets `pool rebase` do the
        // right thing automatically.
        let mut file = file_with(vec![PoolBranch {
            branch: "feat/child".into(),
            head: Some("h".into()),
            base_branch: "feat/parent".into(),
            base: "parent_sha".into(),
            pull_request: Some(PoolPullRequest {
                number: 10,
                id: "PR_10".into(),
                url: "u".into(),
                merged: false,
                comment_count: 0,
                seen_comment_count: 0,
                mergeable: None,
            }),
        }]);
        let gh = FakeGitHub::new();
        // child's PR targets feat/parent.
        let mut child = pr(10, "feat/child", PrState::Open, 0);
        child.base_ref = "feat/parent".into();
        gh.seed_pr(child);
        // parent's PR is merged into main.
        let mut parent = pr(20, "feat/parent", PrState::Merged, 0);
        parent.base_ref = "main".into();
        gh.seed_pr(parent);

        let report = refresh_pool(&gh, &mut file, 0).unwrap();
        let reparent = report
            .changes
            .iter()
            .find_map(|c| match &c.kind {
                PoolRefreshKind::Reparented { from, to } => Some((from.clone(), to.clone())),
                _ => None,
            })
            .expect("Reparented change expected");
        assert_eq!(reparent, ("feat/parent".into(), "main".into()));
        assert_eq!(file.pools[0].branches[0].base_branch, "main");
    }

    #[test]
    fn reparents_branch_without_pr_when_stored_base_is_merged() {
        // No PR on the pool branch itself, but its stored base_branch
        // has been merged. Refresh should still walk up.
        let mut file = file_with(vec![PoolBranch {
            branch: "feat/no-pr".into(),
            head: Some("h".into()),
            base_branch: "feat/parent".into(),
            base: "parent_sha".into(),
            pull_request: None,
        }]);
        let gh = FakeGitHub::new();
        // No PR exists for feat/no-pr.
        let mut parent = pr(20, "feat/parent", PrState::Merged, 0);
        parent.base_ref = "main".into();
        gh.seed_pr(parent);

        let report = refresh_pool(&gh, &mut file, 0).unwrap();
        assert!(report
            .changes
            .iter()
            .any(|c| matches!(&c.kind, PoolRefreshKind::Reparented { to, .. } if to == "main")));
        assert_eq!(file.pools[0].branches[0].base_branch, "main");
    }
}
