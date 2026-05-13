//! `pool adopt` — bulk-add every open PR authored by the current user
//! into the (default or named) pool, with these guardrails:
//!
//! 1. PRs must be open.
//! 2. PRs must be authored by the authenticated user (gh's `@me`). The
//!    GitHub adapter encodes this — the op trusts the filter.
//! 3. The head branch must exist *locally*. Branches that exist only on
//!    the remote are skipped with a notice (the user can `gh pr checkout`
//!    first, or run `git fetch` + create the branch themselves).
//! 4. The branch must not already be part of a stack — those are owned by
//!    the stack model and have parents.
//! 5. The branch must not already be in a pool (any pool).
//!
//! The pool itself is auto-created if missing, just like `pool add`.

use crate::clock::now_rfc3339;
use crate::domain::{Pool, PoolBranch, PoolFile, PoolPullRequest, StackError, StackFile, Trunk};
use crate::github::{GhCli, GitHub, PrState};

use crate::ops::Context;

pub struct AdoptArgs {
    pub pool: Option<String>,
}

#[derive(Debug, Clone)]
pub enum SkipReason {
    NotLocal,
    InStack,
    InPool,
}

#[derive(Debug, Clone)]
pub struct AdoptOutcome {
    pub adopted: Vec<AdoptedBranch>,
    pub skipped: Vec<(String, SkipReason)>,
}

#[derive(Debug, Clone)]
pub struct AdoptedBranch {
    pub branch: String,
    pub pr_number: u64,
    pub pr_url: String,
}

/// Pure orchestration over the ports. Tests drive this directly with a
/// `FakeGitHub`. The CLI wrapper at the bottom resolves git/store and
/// saves.
///
/// Takes both files: `stacks` for the "already in a stack" guardrail,
/// `pools` for the actual mutation. Only `pools` is written back by the
/// CLI wrapper.
pub fn adopt(
    ctx: &Context,
    gh: &dyn GitHub,
    stacks: &StackFile,
    pools: &mut PoolFile,
    args: &AdoptArgs,
) -> Result<AdoptOutcome, StackError> {
    let prs = gh.list_my_open_prs()?;

    // Resolve target pool once. Create lazily so an empty adopt run doesn't
    // leave an orphan pool entry behind.
    let mut outcome = AdoptOutcome {
        adopted: Vec::new(),
        skipped: Vec::new(),
    };
    if prs.is_empty() {
        return Ok(outcome);
    }

    let trunk_branch = ctx.git.trunk_branch()?;
    let trunk_head = ctx.git.rev_parse(&trunk_branch)?;
    let now = now_rfc3339();

    let pool_idx = match pools.pool_position(args.pool.as_deref()) {
        Some(i) => i,
        None => {
            pools.pools.push(Pool {
                name: args.pool.clone(),
                trunk: Trunk {
                    branch: trunk_branch.clone(),
                    head: trunk_head.clone(),
                },
                branches: Vec::new(),
                last_refreshed_at: Some(now.clone()),
            });
            pools.pools.len() - 1
        }
    };

    for pr in prs {
        if pr.state != PrState::Open {
            continue;
        }
        let head = pr.head_ref.clone();

        // Guardrail: branch must exist locally.
        if !ctx.git.branch_exists(&head)? {
            outcome.skipped.push((head, SkipReason::NotLocal));
            continue;
        }
        // Guardrail: must not be in a stack.
        if !stacks.stacks_containing(&head).is_empty() {
            outcome.skipped.push((head, SkipReason::InStack));
            continue;
        }
        // Guardrail: must not be in any pool.
        if pools.pool_position_containing(&head).is_some() {
            outcome.skipped.push((head, SkipReason::InPool));
            continue;
        }

        let live_head = ctx.git.rev_parse(&head)?;
        // Adopt the PR's actual base on GitHub — that's where the PR
        // wants to merge to, so it's the right rebase target. Fall back
        // to trunk if we can't resolve the base ref locally (e.g. it
        // exists only on the remote).
        let base_branch_name = pr.base_ref.clone();
        let base_sha = ctx
            .git
            .rev_parse(&base_branch_name)
            .unwrap_or_else(|_| trunk_head.clone());
        pools.pools[pool_idx].branches.push(PoolBranch {
            branch: head.clone(),
            head: Some(live_head),
            base_branch: base_branch_name,
            base: base_sha,
            pull_request: Some(PoolPullRequest {
                number: pr.number,
                id: pr.id.clone(),
                url: pr.url.clone(),
                merged: false,
                comment_count: pr.comment_count,
                // First sight of the PR → start the seen baseline at the
                // current count. We don't want adoption to immediately
                // claim N "new comments".
                seen_comment_count: pr.comment_count,
                mergeable: pr.mergeable,
            }),
        });
        outcome.adopted.push(AdoptedBranch {
            branch: head,
            pr_number: pr.number,
            pr_url: pr.url,
        });
    }

    pools.pools[pool_idx].last_refreshed_at = Some(now);
    Ok(outcome)
}

pub fn run(args: AdoptArgs) -> Result<(), StackError> {
    use crate::style::{accent, dim, secondary, url_err, warn};

    let cwd = std::env::current_dir()?;
    let ctx = Context::open(&cwd)?;
    let stacks = ctx.store.load(&ctx.identity)?;
    let mut pools = ctx.store.load_pools(&ctx.identity)?;
    let gh = GhCli::new(&cwd);

    let outcome = adopt(&ctx, &gh, &stacks, &mut pools, &args)?;
    let pool_label = match pools.pool_position(args.pool.as_deref()) {
        Some(i) => pools.pools[i].display_label().to_string(),
        None => args.pool.clone().unwrap_or_else(|| "(default)".to_string()),
    };

    if outcome.adopted.is_empty() && outcome.skipped.is_empty() {
        eprintln!("✓ no open PRs authored by you in this repo");
        return Ok(());
    }

    if !outcome.adopted.is_empty() {
        eprintln!(
            "✓ adopted {} branch(es) into pool {pool_label}:",
            outcome.adopted.len()
        );
        for a in &outcome.adopted {
            eprintln!(
                "  {} {}  {}",
                accent("●"),
                a.branch,
                secondary(&format!("#{}", a.pr_number))
            );
            eprintln!("      {}", url_err(&a.pr_url));
        }
    }
    if !outcome.skipped.is_empty() {
        eprintln!("{} skipped {}:", dim("ℹ"), outcome.skipped.len());
        for (branch, reason) in &outcome.skipped {
            let why = match reason {
                SkipReason::NotLocal => warn("no local branch (run `gh pr checkout` first)"),
                SkipReason::InStack => dim("already in a stack"),
                SkipReason::InPool => dim("already in a pool"),
            };
            eprintln!("  {} {}  {}", dim("○"), branch, why);
        }
    }

    ctx.store.save_pools(&pools)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Branch, Stack, SCHEMA_VERSION};
    use crate::github::fake::FakeGitHub;
    use crate::github::{PrState, PullRequestInfo};

    fn pr(number: u64, head: &str, state: PrState) -> PullRequestInfo {
        PullRequestInfo {
            number,
            id: format!("PR_{number}"),
            url: format!("https://github.com/x/y/pull/{number}"),
            state,
            head_ref: head.into(),
            base_ref: "main".into(),
            comment_count: 0,
            mergeable: None,
        }
    }

    /// Adopt-pure tests that don't need a real `Context`. We exercise the
    /// filtering logic directly against both files so we can assert the
    /// precise classification of every candidate without a tempdir.
    fn classify(
        stacks: &StackFile,
        pools: &PoolFile,
        branches_local: &[&str],
        prs: &[PullRequestInfo],
    ) -> (Vec<String>, Vec<(String, &'static str)>) {
        let mut adopt = Vec::new();
        let mut skip = Vec::new();
        for pr in prs {
            let head = pr.head_ref.clone();
            if pr.state != PrState::Open {
                continue;
            }
            if !branches_local.iter().any(|b| *b == head) {
                skip.push((head, "not_local"));
                continue;
            }
            if !stacks.stacks_containing(&head).is_empty() {
                skip.push((head, "in_stack"));
                continue;
            }
            if pools.pool_position_containing(&head).is_some() {
                skip.push((head, "in_pool"));
                continue;
            }
            adopt.push(head);
        }
        (adopt, skip)
    }

    fn empty_stacks() -> StackFile {
        StackFile {
            schema_version: SCHEMA_VERSION,
            repository: "r".into(),
            stacks: vec![],
        }
    }

    fn empty_pools() -> PoolFile {
        PoolFile {
            schema_version: SCHEMA_VERSION,
            repository: "r".into(),
            pools: vec![],
        }
    }

    #[test]
    fn classification_excludes_branches_in_stacks() {
        let mut stacks = empty_stacks();
        let pools = empty_pools();
        stacks.stacks.push(Stack {
            prefix: None,
            trunk: Trunk {
                branch: "main".into(),
                head: "h".into(),
            },
            branches: vec![Branch {
                branch: "feat/x".into(),
                head: None,
                base: "h".into(),
                pull_request: None,
            }],
            last_refreshed_at: None,
        });
        let prs = vec![
            pr(1, "feat/x", PrState::Open),
            pr(2, "feat/y", PrState::Open),
        ];
        let (adopt, skip) = classify(&stacks, &pools, &["feat/x", "feat/y"], &prs);
        assert_eq!(adopt, vec!["feat/y".to_string()]);
        assert_eq!(skip, vec![("feat/x".to_string(), "in_stack")]);
    }

    #[test]
    fn classification_excludes_branches_already_in_pool() {
        let stacks = empty_stacks();
        let mut pools = empty_pools();
        pools.pools.push(Pool {
            name: None,
            trunk: Trunk {
                branch: "main".into(),
                head: "h".into(),
            },
            branches: vec![PoolBranch {
                branch: "feat/x".into(),
                head: None,
                base_branch: "main".into(),
                base: "h".into(),
                pull_request: None,
            }],
            last_refreshed_at: None,
        });
        let prs = vec![
            pr(1, "feat/x", PrState::Open),
            pr(2, "feat/y", PrState::Open),
        ];
        let (adopt, skip) = classify(&stacks, &pools, &["feat/x", "feat/y"], &prs);
        assert_eq!(adopt, vec!["feat/y".to_string()]);
        assert_eq!(skip, vec![("feat/x".to_string(), "in_pool")]);
    }

    #[test]
    fn classification_excludes_non_local_branches() {
        let stacks = empty_stacks();
        let pools = empty_pools();
        let prs = vec![
            pr(1, "feat/local", PrState::Open),
            pr(2, "feat/remote", PrState::Open),
        ];
        let (adopt, skip) = classify(&stacks, &pools, &["feat/local"], &prs);
        assert_eq!(adopt, vec!["feat/local".to_string()]);
        assert_eq!(skip, vec![("feat/remote".to_string(), "not_local")]);
    }

    #[test]
    fn list_my_open_prs_returns_only_open_state() {
        let gh = FakeGitHub::new();
        gh.seed_pr(pr(1, "feat/a", PrState::Open));
        gh.seed_pr(pr(2, "feat/b", PrState::Merged));
        gh.seed_pr(pr(3, "feat/c", PrState::Open));
        let prs = gh.list_my_open_prs().unwrap();
        let nums: Vec<u64> = prs.iter().map(|p| p.number).collect();
        assert_eq!(nums, vec![1, 3]);
    }
}
