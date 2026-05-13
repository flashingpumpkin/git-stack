//! `pool submit [--pool NAME] [--draft] [--remote REMOTE] [--no-refresh]`
//! — push every active pool branch and open a PR for any that doesn't
//! have one yet.
//!
//! Unlike `stack submit`, branches in a pool are independent: each PR
//! targets that branch's own `base_branch`, not the previous branch.
//! Branches that already have a PR get their stored data refreshed in
//! place (no new PR created). Branches with no commits over their base
//! are skipped — GitHub would reject them anyway.

use crate::clock::now_rfc3339;
use crate::domain::{PoolFile, PoolPullRequest, StackError};
use crate::git::GitOps;
use crate::github::{GhCli, GitHub, PrState};
use crate::style::url_err as url_style;

use crate::ops::Context;

pub struct SubmitArgs {
    pub pool: Option<String>,
    pub draft: bool,
    pub remote: String,
    pub no_refresh: bool,
}

/// CLI entrypoint. Wires the production git + gh adapters and persists results.
pub fn run(args: SubmitArgs) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let ctx = Context::open(&cwd)?;
    let gh = GhCli::new(&cwd);
    let mut file = ctx.store.load_pools(&ctx.identity)?;
    submit(&ctx.git, &gh, &mut file, &args)?;
    ctx.store.save_pools(&file)?;
    Ok(())
}

/// Pure orchestration over the ports. No I/O beyond what the ports
/// expose, so tests can drive it with in-memory fakes.
pub fn submit(
    git: &dyn GitOps,
    gh: &dyn GitHub,
    file: &mut PoolFile,
    args: &SubmitArgs,
) -> Result<(), StackError> {
    let idx = super::resolve_pool_index(file, args.pool.as_deref())?;
    let label = file.pools[idx].display_label().to_string();

    // Refresh first so squash-merged PRs are flagged and new ones (e.g.
    // created via gh pr create since we last looked) get picked up.
    if !args.no_refresh {
        let _ = super::refresh::refresh_pool(gh, file, idx)?;
    }

    // Classify every active branch. We snapshot the names so we can
    // mutate `file` inside the loop without re-borrowing.
    #[derive(Clone)]
    enum Plan {
        SkipMerged,
        SkipEmpty,
        RefreshExisting { number: u64 },
        Create { base: String },
    }
    let mut plan: Vec<(usize, String, Plan)> = Vec::new();
    for (i, b) in file.pools[idx].branches.iter().enumerate() {
        let action = if b.is_merged() {
            Plan::SkipMerged
        } else if let Some(pr) = b.pull_request.as_ref() {
            Plan::RefreshExisting { number: pr.number }
        } else {
            let base = b.effective_base_branch(&file.pools[idx]).to_string();
            // Empty branches (no commits over their base) get skipped —
            // GitHub rejects "no commits" PRs anyway.
            if git.commits_between(&base, &b.branch)?.is_empty() {
                Plan::SkipEmpty
            } else {
                Plan::Create { base }
            }
        };
        plan.push((i, b.branch.clone(), action));
    }

    // Push every branch we're about to act on, atomically.
    let to_push: Vec<&str> = plan
        .iter()
        .filter_map(|(_, name, p)| match p {
            Plan::Create { .. } | Plan::RefreshExisting { .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    if to_push.is_empty() {
        eprintln!("ℹ pool {label}: nothing to push");
    } else {
        git.push_atomic(&args.remote, &to_push)?;
        eprintln!("✓ pool {label}: pushed {} branch(es)", to_push.len());
    }

    for (branch_idx, branch_name, action) in plan {
        match action {
            Plan::SkipMerged => continue,
            Plan::SkipEmpty => {
                eprintln!("ℹ skipping {branch_name} (no commits over its base)");
            }
            Plan::RefreshExisting { number } => {
                let info = gh.view_pr(number)?;
                let url_for_log = info.url.clone();
                // Replace the stored PR data wholesale so comment counts
                // and mergeable state stay current; preserve the seen
                // baseline so unread indicators behave correctly.
                let prev_seen = file.pools[idx].branches[branch_idx]
                    .pull_request
                    .as_ref()
                    .map(|p| p.seen_comment_count)
                    .unwrap_or(0);
                file.pools[idx].branches[branch_idx].pull_request = Some(PoolPullRequest {
                    number: info.number,
                    id: info.id,
                    url: info.url,
                    merged: info.state == PrState::Merged,
                    comment_count: info.comment_count,
                    seen_comment_count: prev_seen,
                    mergeable: info.mergeable,
                });
                eprintln!("ℹ PR #{number} for {branch_name} refreshed");
                eprintln!("    {}", url_style(&url_for_log));
            }
            Plan::Create { base } => {
                let commits = git.commits_between(&base, &branch_name)?;
                let (title, body) =
                    crate::ops::auto_title_body_from_commits(&branch_name, &commits);
                let info = gh.create_pr(&branch_name, &base, &title, &body, args.draft)?;
                let number = info.number;
                let url_for_log = info.url.clone();
                file.pools[idx].branches[branch_idx].pull_request = Some(PoolPullRequest {
                    number,
                    id: info.id,
                    url: info.url,
                    merged: info.state == PrState::Merged,
                    comment_count: info.comment_count,
                    // First sight of the PR — start the seen baseline at
                    // whatever GitHub reports so we don't immediately
                    // claim N "new comments".
                    seen_comment_count: info.comment_count,
                    mergeable: info.mergeable,
                });
                eprintln!("✓ created PR #{number} for {branch_name}");
                eprintln!("    {}", url_style(&url_for_log));
            }
        }
    }

    file.pools[idx].last_refreshed_at = Some(now_rfc3339());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Pool, PoolBranch, PoolFile, Trunk, SCHEMA_VERSION};
    use crate::git::{fake::FakeGit, CommitSummary};
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

    fn pb(name: &str, base_branch: &str) -> PoolBranch {
        PoolBranch {
            branch: name.into(),
            head: Some("h".into()),
            base_branch: base_branch.into(),
            base: "trunk_sha".into(),
            pull_request: None,
        }
    }

    fn args() -> SubmitArgs {
        SubmitArgs {
            pool: None,
            draft: false,
            remote: "origin".into(),
            no_refresh: true,
        }
    }

    #[test]
    fn creates_a_pr_for_every_branch_without_one_using_its_own_base() {
        // Two branches, different bases, neither has a PR yet.
        let mut file = file_with(vec![pb("fix/typo", "main"), pb("hotfix", "release/Q2")]);
        let git = FakeGit::new("fix/typo");
        git.seed_commits(
            "main",
            "fix/typo",
            vec![CommitSummary {
                subject: "Fix typo".into(),
                body: String::new(),
            }],
        );
        git.seed_commits(
            "release/Q2",
            "hotfix",
            vec![CommitSummary {
                subject: "Hotfix".into(),
                body: "details".into(),
            }],
        );
        let gh = FakeGitHub::new();

        submit(&git, &gh, &mut file, &args()).unwrap();

        let calls = gh.create_calls();
        assert_eq!(calls.len(), 2);
        // Pool members are independent — each PR targets its own base ref,
        // not a chain.
        assert!(calls
            .iter()
            .any(|c| c.head == "fix/typo" && c.base == "main"));
        assert!(calls
            .iter()
            .any(|c| c.head == "hotfix" && c.base == "release/Q2"));
        // Both branches now have stored PR data.
        for b in &file.pools[0].branches {
            assert!(b.pull_request.is_some(), "{} still missing PR", b.branch);
        }
    }

    #[test]
    fn skips_branches_that_already_have_a_pr_and_only_refreshes_their_state() {
        let mut br = pb("feat/a", "main");
        br.pull_request = Some(PoolPullRequest {
            number: 7,
            id: "old".into(),
            url: "old-url".into(),
            merged: false,
            comment_count: 0,
            seen_comment_count: 0,
            mergeable: None,
        });
        let mut file = file_with(vec![br]);
        let git = FakeGit::new("feat/a");
        let gh = FakeGitHub::new();
        gh.seed_pr(PullRequestInfo {
            number: 7,
            id: "PR_new".into(),
            url: "https://github.com/x/y/pull/7".into(),
            state: PrState::Open,
            head_ref: "feat/a".into(),
            base_ref: "main".into(),
            comment_count: 3,
            mergeable: Some(true),
        });

        submit(&git, &gh, &mut file, &args()).unwrap();

        assert_eq!(gh.create_calls().len(), 0, "must not create a duplicate PR");
        let stored = file.pools[0].branches[0].pull_request.as_ref().unwrap();
        assert_eq!(stored.id, "PR_new");
        assert_eq!(stored.comment_count, 3);
        assert_eq!(stored.mergeable, Some(true));
    }

    #[test]
    fn skips_empty_branches_without_pushing_or_pring_them() {
        // fix/empty has no commits over main; hotfix has one. Pool
        // submit should push only hotfix, only PR hotfix.
        let mut file = file_with(vec![pb("fix/empty", "main"), pb("hotfix", "main")]);
        let git = FakeGit::new("hotfix");
        git.seed_commits(
            "main",
            "hotfix",
            vec![CommitSummary {
                subject: "Fix".into(),
                body: String::new(),
            }],
        );
        let gh = FakeGitHub::new();

        submit(&git, &gh, &mut file, &args()).unwrap();

        let pushes = git.pushes();
        assert_eq!(pushes.len(), 1);
        assert_eq!(pushes[0].branches, vec!["hotfix"]);
        let calls = gh.create_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].head, "hotfix");
    }
}
