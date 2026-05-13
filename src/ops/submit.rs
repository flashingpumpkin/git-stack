use crate::domain::{PullRequest, StackError, StackFile, SubmitAction};
use crate::git::GitOps;
use crate::github::{GhCli, GitHub, PrState};
use crate::style::url_err as url_style;

use super::Context;

pub struct SubmitArgs {
    pub auto: bool,
    pub draft: bool,
    pub remote: String,
    pub no_refresh: bool,
}

/// CLI entrypoint: wires the production git + gh adapters and persists results.
pub fn run(args: SubmitArgs) -> Result<(), StackError> {
    // `--auto` used to be required (gh-stack's history of per-PR title
    // prompts), but this CLI never prompts in the first place, so the flag
    // is now accepted but optional. Keeping it parseable preserves any
    // existing shell aliases.
    let _ = args.auto;
    let cwd = std::env::current_dir()?;
    let ctx = Context::open(&cwd)?;
    let gh = GhCli::new(&cwd);
    let mut file = ctx.store.load(&ctx.identity)?;
    submit(&ctx.git, &gh, &mut file, &args)?;
    ctx.store.save(&file)?;
    Ok(())
}

/// Pure orchestration over the ports. No I/O beyond what the ports expose,
/// so tests can drive it with in-memory fakes.
pub fn submit(
    git: &dyn GitOps,
    gh: &dyn GitHub,
    file: &mut StackFile,
    args: &SubmitArgs,
) -> Result<(), StackError> {
    let current = git
        .current_branch()?
        .ok_or_else(|| StackError::Other("detached HEAD".into()))?;
    let stack_index = file
        .stacks
        .iter()
        .position(|s| s.contains(&current))
        .ok_or(StackError::NotInStack)?;

    // Refresh PR state first: a branch that's already merged shouldn't be
    // pushed again, and refresh updates the merged flag so active_branches()
    // filters it out below.
    if !args.no_refresh {
        let newly_merged = super::pr_refresh::refresh_stack(gh, file, stack_index)?;
        if newly_merged > 0 {
            eprintln!("ℹ {newly_merged} PR(s) merged since last refresh");
        }
    }

    // Build the plan from domain rules. The callback wraps `git.commits_between`
    // since "is this branch empty over its base?" is the only question the
    // planner can't answer purely from the stack file.
    let plan = file.stacks[stack_index]
        .submit_plan(|base, branch| Ok(git.commits_between(base, branch)?.is_empty()))?;

    // Push only the branches we plan to act on. Empty + merged are excluded.
    let names: Vec<&str> = plan
        .iter()
        .filter_map(|p| match &p.action {
            SubmitAction::Create { .. } | SubmitAction::RefreshExisting { .. } => {
                Some(p.branch.as_str())
            }
            _ => None,
        })
        .collect();
    if names.is_empty() {
        eprintln!("ℹ nothing to push");
    } else {
        git.push_atomic(&args.remote, &names)?;
        eprintln!("✓ pushed {} branch(es)", names.len());
    }

    // Execute the plan.
    for item in plan {
        match item.action {
            SubmitAction::SkipMerged => continue,
            SubmitAction::SkipEmpty => {
                eprintln!("ℹ skipping {} (no commits)", item.branch);
            }
            SubmitAction::RefreshExisting { number } => {
                let info = gh.view_pr(number)?;
                let url_for_log = info.url.clone();
                let pr = file.stacks[stack_index].branches[item.branch_index]
                    .pull_request
                    .as_mut()
                    .unwrap();
                pr.merged = info.state == PrState::Merged;
                pr.url = info.url;
                pr.id = info.id;
                eprintln!("ℹ PR #{number} for {} refreshed", item.branch);
                eprintln!("    {}", url_style(&url_for_log));
            }
            SubmitAction::Create { base } => {
                let commits = git.commits_between(&base, &item.branch)?;
                let (title, body) = auto_title_body_from_commits(&item.branch, &commits);
                let info = gh.create_pr(&item.branch, &base, &title, &body, args.draft)?;
                let number = info.number;
                let url_for_log = info.url.clone();
                file.stacks[stack_index].branches[item.branch_index].pull_request =
                    Some(PullRequest {
                        number,
                        id: info.id,
                        url: info.url,
                        merged: info.state == PrState::Merged,
                    });
                eprintln!("✓ created PR #{number} for {}", item.branch);
                eprintln!("    {}", url_style(&url_for_log));
            }
        }
    }

    file.stacks[stack_index].last_refreshed_at = Some(crate::clock::now_rfc3339());
    Ok(())
}

fn auto_title_body_from_commits(
    branch: &str,
    commits: &[crate::git::CommitSummary],
) -> (String, String) {
    match commits.len() {
        0 => (humanise(branch), String::new()),
        1 => (commits[0].subject.clone(), commits[0].body.clone()),
        _ => {
            let body = commits
                .iter()
                .map(|c| format!("- {}", c.subject))
                .collect::<Vec<_>>()
                .join("\n");
            (humanise(branch), body)
        }
    }
}

fn humanise(branch: &str) -> String {
    let leaf = branch.rsplit('/').next().unwrap_or(branch);
    leaf.replace(['-', '_'], " ")
        .split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(first) => first.to_uppercase().chain(c).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Branch, Stack, Trunk, SCHEMA_VERSION};
    use crate::git::{fake::FakeGit, CommitSummary};
    use crate::github::fake::FakeGitHub;
    use crate::github::PullRequestInfo;

    fn file_with(branches: Vec<Branch>) -> StackFile {
        StackFile {
            schema_version: SCHEMA_VERSION,
            repository: "test".into(),
            stacks: vec![Stack {
                prefix: Some("feat".into()),
                trunk: Trunk {
                    branch: "main".into(),
                    head: "trunk_sha".into(),
                },
                branches,
                last_refreshed_at: None,
            }],
        }
    }

    fn b(name: &str) -> Branch {
        Branch {
            branch: name.into(),
            head: Some("h".into()),
            base: "x".into(),
            pull_request: None,
        }
    }

    fn args() -> SubmitArgs {
        SubmitArgs {
            auto: true,
            draft: false,
            remote: "origin".into(),
            // Default for these tests: skip refresh so we exercise the
            // create-PR path directly. The dedicated refresh test below
            // sets no_refresh: false.
            no_refresh: true,
        }
    }

    #[test]
    fn pushes_then_creates_one_pr_per_branch_with_chained_bases() {
        let mut file = file_with(vec![b("feat/a"), b("feat/b"), b("feat/c")]);
        let git = FakeGit::new("feat/c");
        git.seed_commits(
            "main",
            "feat/a",
            vec![CommitSummary {
                subject: "Add A".into(),
                body: "details about A".into(),
            }],
        );
        // feat/b has two commits → humanised branch name as title.
        git.seed_commits(
            "feat/a",
            "feat/b",
            vec![
                CommitSummary {
                    subject: "First b commit".into(),
                    body: String::new(),
                },
                CommitSummary {
                    subject: "Second b commit".into(),
                    body: String::new(),
                },
            ],
        );
        git.seed_commits(
            "feat/b",
            "feat/c",
            vec![CommitSummary {
                subject: "Add C".into(),
                body: String::new(),
            }],
        );
        let gh = FakeGitHub::new();

        submit(&git, &gh, &mut file, &args()).unwrap();

        let pushes = git.pushes();
        assert_eq!(pushes.len(), 1);
        assert_eq!(pushes[0].remote, "origin");
        assert_eq!(pushes[0].branches, vec!["feat/a", "feat/b", "feat/c"]);

        let calls = gh.create_calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].head, "feat/a");
        assert_eq!(calls[0].base, "main");
        assert_eq!(calls[0].title, "Add A");
        assert_eq!(calls[0].body, "details about A");
        assert_eq!(calls[1].head, "feat/b");
        assert_eq!(calls[1].base, "feat/a");
        assert_eq!(calls[1].title, "B");
        assert!(calls[1].body.contains("First b commit"));
        assert!(calls[1].body.contains("Second b commit"));
        assert_eq!(calls[2].head, "feat/c");
        assert_eq!(calls[2].base, "feat/b");

        // Store updated with PR data for every branch.
        for br in &file.stacks[0].branches {
            assert!(br.pull_request.is_some(), "{} missing pr", br.branch);
        }
    }

    #[test]
    fn skips_create_when_pr_already_exists_and_refreshes_state() {
        let mut br = b("feat/a");
        br.pull_request = Some(crate::domain::PullRequest {
            number: 42,
            id: "old".into(),
            url: "old".into(),
            merged: false,
        });
        let mut file = file_with(vec![br]);
        let git = FakeGit::new("feat/a");
        let gh = FakeGitHub::new();
        gh.seed_pr(PullRequestInfo {
            number: 42,
            id: "PR_new".into(),
            url: "https://github.com/x/y/pull/42".into(),
            state: PrState::Merged,
            head_ref: "feat/a".into(),
            base_ref: "main".into(),
        });

        submit(&git, &gh, &mut file, &args()).unwrap();

        assert_eq!(gh.create_calls().len(), 0, "must not create a new PR");
        let pr = file.stacks[0].branches[0].pull_request.as_ref().unwrap();
        assert!(pr.merged);
        assert_eq!(pr.id, "PR_new");
    }

    #[test]
    fn skips_empty_branches_without_attempting_to_create_a_pr() {
        // feat/a has no commits over trunk (e.g. a worktree that was
        // created but never modified). feat/b builds on it and has
        // a real commit. Submit should: skip feat/a entirely, push only
        // feat/b, and chain feat/b's PR base to trunk (not to the empty
        // feat/a) so GitHub doesn't reject "no commits".
        let mut file = file_with(vec![b("feat/a"), b("feat/b")]);
        let git = FakeGit::new("feat/b");
        // No commits seeded for (main, feat/a) → commits_between returns [].
        git.seed_commits(
            "main",
            "feat/b",
            vec![CommitSummary {
                subject: "B work".into(),
                body: String::new(),
            }],
        );
        let gh = FakeGitHub::new();

        submit(&git, &gh, &mut file, &args()).unwrap();

        // Only feat/b was pushed; feat/a's empty branch was skipped.
        let pushes = git.pushes();
        assert_eq!(pushes.len(), 1);
        assert_eq!(pushes[0].branches, vec!["feat/b"]);

        // Only one PR was created (for feat/b) and it bases on trunk.
        let calls = gh.create_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].head, "feat/b");
        assert_eq!(calls[0].base, "main");
    }

    #[test]
    fn auto_refresh_marks_merged_pr_and_excludes_it_from_push() {
        // The scenario from the bug report: feat/a's PR was squash-merged
        // on GitHub since the last refresh. submit (default: refresh on)
        // must see this, mark feat/a as merged, and NOT push feat/a or
        // create a duplicate PR for it. feat/b is still active and chains
        // to trunk (not to merged feat/a).
        let mut a = b("feat/a");
        a.pull_request = Some(crate::domain::PullRequest {
            number: 1,
            id: "old".into(),
            url: "old".into(),
            merged: false, // store still says open
        });
        let mut file = file_with(vec![a, b("feat/b")]);
        let git = FakeGit::new("feat/b");
        git.seed_commits(
            "main",
            "feat/b",
            vec![CommitSummary {
                subject: "B work".into(),
                body: String::new(),
            }],
        );
        let gh = FakeGitHub::new();
        // Source of truth: PR #1 IS merged.
        gh.seed_pr(PullRequestInfo {
            number: 1,
            id: "PR_a".into(),
            url: "https://github.com/x/y/pull/1".into(),
            state: PrState::Merged,
            head_ref: "feat/a".into(),
            base_ref: "main".into(),
        });

        let mut a = args();
        a.no_refresh = false; // explicitly exercise the refresh path
        submit(&git, &gh, &mut file, &a).unwrap();

        // feat/a is now marked merged in the store.
        let pr_a = file.stacks[0].branches[0].pull_request.as_ref().unwrap();
        assert!(pr_a.merged, "feat/a should be marked merged after refresh");

        // Only feat/b was pushed; feat/a was filtered out by active_branches().
        let pushes = git.pushes();
        assert_eq!(pushes.len(), 1);
        assert_eq!(pushes[0].branches, vec!["feat/b"]);

        // Only feat/b got a fresh PR; we didn't create a duplicate for
        // feat/a or chain feat/b's base to it.
        let calls = gh.create_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].head, "feat/b");
        assert_eq!(calls[0].base, "main", "chain should skip merged feat/a");
    }

    #[test]
    fn skips_merged_branches_for_base_chaining() {
        // feat/a is merged → feat/b's base should be trunk, not feat/a.
        let mut a = b("feat/a");
        a.pull_request = Some(crate::domain::PullRequest {
            number: 1,
            id: "x".into(),
            url: "u".into(),
            merged: true,
        });
        let mut file = file_with(vec![a, b("feat/b")]);
        let git = FakeGit::new("feat/b");
        git.seed_commits(
            "main",
            "feat/b",
            vec![CommitSummary {
                subject: "B".into(),
                body: String::new(),
            }],
        );
        let gh = FakeGitHub::new();

        submit(&git, &gh, &mut file, &args()).unwrap();

        let calls = gh.create_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].head, "feat/b");
        assert_eq!(
            calls[0].base, "main",
            "merged a should be skipped, base = trunk"
        );
    }
}
