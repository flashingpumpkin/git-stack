use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackFile {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub repository: String,
    pub stacks: Vec<Stack>,
}

impl StackFile {
    pub fn empty(repository: impl Into<String>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            repository: repository.into(),
            stacks: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stack {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub prefix: Option<String>,
    pub trunk: Trunk,
    pub branches: Vec<Branch>,
    /// RFC 3339 timestamp of the last time PR data on this stack was
    /// fetched from GitHub (via `submit`, `sync`, or `view --refresh`).
    /// `None` means PR data has never been refreshed since branches were created.
    #[serde(
        rename = "lastRefreshedAt",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub last_refreshed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trunk {
    pub branch: String,
    pub head: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branch {
    pub branch: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub head: Option<String>,
    pub base: String,
    #[serde(
        rename = "pullRequest",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub pull_request: Option<PullRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub merged: bool,
}

impl StackFile {
    /// Find every stack containing the given branch.
    pub fn stacks_containing<'a>(&'a self, branch: &str) -> Vec<&'a Stack> {
        self.stacks.iter().filter(|s| s.contains(branch)).collect()
    }

    /// Resolve "the stack the user is currently on" given the current branch.
    /// Returns `Err(NotInStack)` if no stack matches, `Err(Disambiguation)` if
    /// more than one matches.
    pub fn current_stack<'a>(&'a self, branch: &str) -> Result<&'a Stack, super::StackError> {
        let matches = self.stacks_containing(branch);
        match matches.len() {
            0 => Err(super::StackError::NotInStack),
            1 => Ok(matches[0]),
            _ => Err(super::StackError::Disambiguation(branch.to_string())),
        }
    }
}

impl Stack {
    /// Apply the stack prefix to a user-supplied suffix.
    /// With prefix `feat`, `auth` → `feat/auth`. Without prefix, name is returned unchanged.
    pub fn apply_prefix(&self, name: &str) -> String {
        match &self.prefix {
            Some(p) => format!("{p}/{name}"),
            None => name.to_string(),
        }
    }

    pub fn contains(&self, branch: &str) -> bool {
        self.branches.iter().any(|b| b.branch == branch)
    }

    pub fn position(&self, branch: &str) -> Option<usize> {
        self.branches.iter().position(|b| b.branch == branch)
    }

    /// Active branches = not merged. Used for navigation and push/submit.
    pub fn active_branches(&self) -> Vec<&Branch> {
        self.branches.iter().filter(|b| !b.is_merged()).collect()
    }

    /// Step `n` positions through active branches starting at `from_branch`.
    /// Positive `n` = away from trunk (up). Negative = toward trunk (down).
    /// Clamps to stack bounds.
    pub fn step_active(&self, from_branch: &str, n: isize) -> Option<&Branch> {
        let active = self.active_branches();
        let idx = active.iter().position(|b| b.branch == from_branch)?;
        let target = (idx as isize + n).clamp(0, active.len() as isize - 1) as usize;
        active.get(target).copied()
    }

    pub fn bottom_active(&self) -> Option<&Branch> {
        self.active_branches().into_iter().next()
    }

    pub fn top_active(&self) -> Option<&Branch> {
        self.active_branches().into_iter().last()
    }
}

impl Branch {
    pub fn is_merged(&self) -> bool {
        self.pull_request.as_ref().is_some_and(|p| p.merged)
    }
}

/// What `submit` should do with a branch, in stack order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitAction {
    SkipMerged,
    /// Branch has no commits over its effective base — don't push, don't PR.
    SkipEmpty,
    /// Branch already has a PR; just refresh its stored fields.
    RefreshExisting {
        number: u64,
    },
    /// Branch needs a brand-new PR opened against `base`.
    Create {
        base: String,
    },
}

#[derive(Debug, Clone)]
pub struct SubmitPlanItem {
    pub branch_index: usize,
    pub branch: String,
    pub action: SubmitAction,
}

impl Stack {
    /// Plan one action per branch for `stack submit`, in stack order.
    ///
    /// `branch_is_empty(base, branch)` is supplied by the caller because
    /// answering it requires git (the domain stays adapter-free). The
    /// callback is only invoked when the branch is a Create candidate.
    ///
    /// Base-chaining rule: for a branch that needs a fresh PR, the base is
    /// the highest previously-planned Create/RefreshExisting branch below
    /// it — i.e. merged and empty branches below are skipped. Falls back
    /// to the trunk.
    pub fn submit_plan<F>(
        &self,
        mut branch_is_empty: F,
    ) -> Result<Vec<SubmitPlanItem>, super::StackError>
    where
        F: FnMut(&str, &str) -> Result<bool, super::StackError>,
    {
        let trunk = self.trunk.branch.clone();
        let mut plan: Vec<SubmitPlanItem> = Vec::with_capacity(self.branches.len());
        for (i, b) in self.branches.iter().enumerate() {
            if b.is_merged() {
                plan.push(SubmitPlanItem {
                    branch_index: i,
                    branch: b.branch.clone(),
                    action: SubmitAction::SkipMerged,
                });
                continue;
            }
            if let Some(pr) = &b.pull_request {
                plan.push(SubmitPlanItem {
                    branch_index: i,
                    branch: b.branch.clone(),
                    action: SubmitAction::RefreshExisting { number: pr.number },
                });
                continue;
            }
            // Base = previous Create/RefreshExisting, else trunk.
            let base = plan
                .iter()
                .rev()
                .find_map(|p| match &p.action {
                    SubmitAction::RefreshExisting { .. } | SubmitAction::Create { .. } => {
                        Some(p.branch.clone())
                    }
                    _ => None,
                })
                .unwrap_or_else(|| trunk.clone());

            let action = if branch_is_empty(&base, &b.branch)? {
                SubmitAction::SkipEmpty
            } else {
                SubmitAction::Create { base }
            };
            plan.push(SubmitPlanItem {
                branch_index: i,
                branch: b.branch.clone(),
                action,
            });
        }
        Ok(plan)
    }
}

/// What we know about a stack's worktree after consulting git and the
/// filesystem. `Missing` means git still tracks the path but the directory
/// is gone (`rm -rf`'d); the user can run `stack prune` to clean it up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeStatus {
    Live(std::path::PathBuf),
    Missing(std::path::PathBuf),
    None,
}

impl Stack {
    /// Resolve a worktree for this stack from the list git reports.
    ///
    /// Preference order:
    ///   1. A live worktree whose path the `cwd` lives under (cwd-ownership).
    ///   2. Any live worktree of any branch in the stack.
    ///   3. Any known-but-missing worktree (so the user sees the stale path).
    ///   4. `None`.
    pub fn resolve_worktree(
        &self,
        worktrees: &[(std::path::PathBuf, String)],
        cwd: &std::path::Path,
    ) -> WorktreeStatus {
        // (1) cwd ownership, but only if it's live.
        if let Some((p, b)) = worktrees.iter().find(|(p, _)| cwd.starts_with(p)) {
            if self.contains(b) && p.exists() {
                return WorktreeStatus::Live(p.clone());
            }
        }
        // (2) and (3) walked together.
        let mut missing_fallback: Option<std::path::PathBuf> = None;
        for b in &self.branches {
            if let Some((p, _)) = worktrees.iter().find(|(_, wb)| wb == &b.branch) {
                if p.exists() {
                    return WorktreeStatus::Live(p.clone());
                } else if missing_fallback.is_none() {
                    missing_fallback = Some(p.clone());
                }
            }
        }
        match missing_fallback {
            Some(p) => WorktreeStatus::Missing(p),
            None => WorktreeStatus::None,
        }
    }
}

/// A flat collection of independent branches sharing a default trunk.
/// Unlike a `Stack`, pool members don't depend on each other — each
/// branch carries its own `base_branch` and rebases directly onto that.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Pool {
    /// `None` is the default pool; `Some(name)` is a named pool. A repo
    /// can have at most one default pool plus any number of named ones.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,
    /// Default base for branches added without `--base`. Acts as the
    /// pool's nominal trunk; individual branches can target other refs.
    pub trunk: Trunk,
    pub branches: Vec<PoolBranch>,
    /// RFC 3339 of the last `pool refresh` (or `pool list -r`) that
    /// talked to GitHub.
    #[serde(
        rename = "lastRefreshedAt",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub last_refreshed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PoolBranch {
    pub branch: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub head: Option<String>,
    /// Ref name of the branch this one is based on (e.g. `main`,
    /// `release/2026.Q2`, `feat/parent`). Pool members are independent
    /// of each other, but each can sit on a different upstream — `pool
    /// rebase` rebases each branch onto the live head of *its* base.
    /// Serde-default for back-compat; an empty value falls back to the
    /// pool's `trunk`.
    #[serde(rename = "baseBranch", default)]
    pub base_branch: String,
    /// SHA of `base_branch` at the time of the last rebase (or initial
    /// adoption). Used as the `OLD` argument to
    /// `git rebase --onto NEW OLD <branch>`.
    pub base: String,
    #[serde(
        rename = "pullRequest",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub pull_request: Option<PoolPullRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PoolPullRequest {
    pub number: u64,
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub merged: bool,
    /// Current total comment count observed on the PR (issue + review
    /// comments combined). The next refresh compares against this to
    /// detect "new comments since last seen".
    #[serde(rename = "commentCount", default)]
    pub comment_count: u64,
    /// Comment count at the moment the user last looked at this PR
    /// (via `pool list` or `pool refresh`). `comment_count >
    /// seen_comment_count` → has unread activity.
    #[serde(rename = "seenCommentCount", default)]
    pub seen_comment_count: u64,
    /// Mergeable status as reported by GitHub: `None` = unknown,
    /// `Some(true)` = cleanly mergeable, `Some(false)` = conflicts.
    /// Surfaced as `⚠ conflicts` in `pool list`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mergeable: Option<bool>,
}

impl PoolBranch {
    /// Resolve the effective base branch name. Falls back to the pool's
    /// trunk for legacy entries that predate the per-branch base field.
    pub fn effective_base_branch<'a>(&'a self, pool: &'a Pool) -> &'a str {
        if self.base_branch.is_empty() {
            &pool.trunk.branch
        } else {
            &self.base_branch
        }
    }

    pub fn is_merged(&self) -> bool {
        self.pull_request.as_ref().is_some_and(|p| p.merged)
    }

    pub fn unread_comments(&self) -> u64 {
        self.pull_request
            .as_ref()
            .map(|p| p.comment_count.saturating_sub(p.seen_comment_count))
            .unwrap_or(0)
    }
}

impl Pool {
    pub fn contains(&self, branch: &str) -> bool {
        self.branches.iter().any(|b| b.branch == branch)
    }

    pub fn position(&self, branch: &str) -> Option<usize> {
        self.branches.iter().position(|b| b.branch == branch)
    }

    pub fn display_label(&self) -> &str {
        self.name.as_deref().unwrap_or("(default)")
    }
}

/// Persisted form of every pool tracked for a repository. Lives in its
/// own file on disk (`pools.json`) so stack and pool state can evolve
/// independently and be locked separately if we ever want to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PoolFile {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub repository: String,
    pub pools: Vec<Pool>,
}

impl PoolFile {
    pub fn empty(repository: impl Into<String>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            repository: repository.into(),
            pools: Vec::new(),
        }
    }

    /// Locate a pool by its name (`None` = default pool).
    pub fn pool_position(&self, name: Option<&str>) -> Option<usize> {
        self.pools.iter().position(|p| p.name.as_deref() == name)
    }

    /// Locate any pool containing `branch`.
    pub fn pool_position_containing(&self, branch: &str) -> Option<usize> {
        self.pools.iter().position(|p| p.contains(branch))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_prefix_with_prefix() {
        let s = Stack {
            prefix: Some("feat".into()),
            trunk: Trunk {
                branch: "main".into(),
                head: "abc".into(),
            },
            branches: vec![],
            last_refreshed_at: None,
        };
        assert_eq!(s.apply_prefix("auth"), "feat/auth");
    }

    #[test]
    fn apply_prefix_without_prefix_is_identity() {
        let s = Stack {
            prefix: None,
            trunk: Trunk {
                branch: "main".into(),
                head: "abc".into(),
            },
            branches: vec![],
            last_refreshed_at: None,
        };
        assert_eq!(s.apply_prefix("auth"), "auth");
    }

    fn b(name: &str, merged: bool) -> Branch {
        Branch {
            branch: name.into(),
            head: None,
            base: "x".into(),
            pull_request: if merged {
                Some(PullRequest {
                    number: 1,
                    id: "id".into(),
                    url: "u".into(),
                    merged: true,
                })
            } else {
                None
            },
        }
    }

    fn stack_with(names: &[(&str, bool)]) -> Stack {
        Stack {
            prefix: None,
            trunk: Trunk {
                branch: "main".into(),
                head: "t".into(),
            },
            branches: names.iter().map(|(n, m)| b(n, *m)).collect(),
            last_refreshed_at: None,
        }
    }

    #[test]
    fn step_active_skips_merged_branches() {
        // a(merged) → b → c(merged) → d → e
        let s = stack_with(&[
            ("a", true),
            ("b", false),
            ("c", true),
            ("d", false),
            ("e", false),
        ]);
        // Active sequence is b → d → e.
        assert_eq!(s.step_active("b", 1).unwrap().branch, "d");
        assert_eq!(s.step_active("b", 2).unwrap().branch, "e");
        assert_eq!(s.step_active("d", -1).unwrap().branch, "b");
        // Clamps.
        assert_eq!(s.step_active("e", 10).unwrap().branch, "e");
        assert_eq!(s.step_active("b", -10).unwrap().branch, "b");
    }

    #[test]
    fn top_and_bottom_skip_merged() {
        let s = stack_with(&[("a", true), ("b", false), ("c", false), ("d", true)]);
        assert_eq!(s.bottom_active().unwrap().branch, "b");
        assert_eq!(s.top_active().unwrap().branch, "c");
    }

    #[test]
    fn current_stack_disambiguation() {
        let s1 = stack_with(&[("shared", false), ("only-s1", false)]);
        let s2 = stack_with(&[("shared", false), ("only-s2", false)]);
        let file = StackFile {
            schema_version: SCHEMA_VERSION,
            repository: "r".into(),
            stacks: vec![s1, s2],
        };
        assert!(matches!(
            file.current_stack("shared"),
            Err(super::super::StackError::Disambiguation(_))
        ));
        assert_eq!(
            file.current_stack("only-s1").unwrap().branches[1].branch,
            "only-s1"
        );
        assert!(matches!(
            file.current_stack("not-in-any-stack"),
            Err(super::super::StackError::NotInStack)
        ));
    }

    #[test]
    fn roundtrips_gh_stack_schema() {
        // Sample lifted from the user's actual .git/gh-stack file.
        let input = r#"{
  "schemaVersion": 1,
  "repository": "github.com:acme/platform-api",
  "stacks": [
    {
      "prefix": "feat/auth",
      "trunk": { "branch": "main", "head": "90f5035636185a1ba82e738737f6c09f8f0835b6" },
      "branches": [
        {
          "branch": "feat/auth/01-init",
          "head": "e20ef72102ee72b36838b645a0f65f1a20b89a57",
          "base": "cbe1036ae4c94e1decccd1b718fd2552235c5a35",
          "pullRequest": {
            "number": 42,
            "id": "PR_kwexample002",
            "url": "https://github.com/acme/platform-api/pull/42"
          }
        }
      ]
    }
  ]
}"#;
        let parsed: StackFile = serde_json::from_str(input).expect("parse");
        assert_eq!(parsed.schema_version, 1);
        assert_eq!(parsed.stacks.len(), 1);
        assert_eq!(
            parsed.stacks[0].branches[0]
                .pull_request
                .as_ref()
                .unwrap()
                .number,
            42
        );
        assert!(
            !parsed.stacks[0].branches[0]
                .pull_request
                .as_ref()
                .unwrap()
                .merged
        );

        // Round-trip is loss-free.
        let reserialised = serde_json::to_string(&parsed).expect("serialise");
        let reparsed: StackFile = serde_json::from_str(&reserialised).expect("reparse");
        assert_eq!(parsed, reparsed);
    }
}
