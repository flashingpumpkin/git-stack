use std::path::{Path, PathBuf};
use std::process::Command;

use crate::domain::StackError;

pub mod fake;

/// Everything `ops` needs from git. Extracted as a trait so tests can inject
/// an in-memory fake. The production adapter is the `Git` struct in this
/// module.
pub trait GitOps {
    fn current_branch(&self) -> Result<Option<String>, StackError>;
    fn rev_parse(&self, rev: &str) -> Result<String, StackError>;
    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool, StackError>;
    fn push_atomic(&self, remote: &str, branches: &[&str]) -> Result<(), StackError>;
    /// Return one entry per commit on `branch` not in `base`, each as
    /// `(subject, body)`. Implementations should preserve commit order
    /// (newest first), but `submit` only cares about counts and subjects.
    fn commits_between(&self, base: &str, branch: &str) -> Result<Vec<CommitSummary>, StackError>;
}

#[derive(Debug, Clone)]
pub struct CommitSummary {
    pub subject: String,
    pub body: String,
}

/// Shell out to `git` in a working directory. We deliberately avoid `git2`
/// to (a) keep the dep tree small and (b) get behaviour identical to the
/// user's actual `git` binary (their config, hooks, credentials).
pub struct Git {
    cwd: PathBuf,
}

impl GitOps for Git {
    fn current_branch(&self) -> Result<Option<String>, StackError> {
        Git::current_branch(self)
    }
    fn rev_parse(&self, rev: &str) -> Result<String, StackError> {
        Git::rev_parse(self, rev)
    }
    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool, StackError> {
        Git::is_ancestor(self, ancestor, descendant)
    }
    fn push_atomic(&self, remote: &str, branches: &[&str]) -> Result<(), StackError> {
        Git::push_atomic(self, remote, branches)
    }
    fn commits_between(&self, base: &str, branch: &str) -> Result<Vec<CommitSummary>, StackError> {
        let raw = self.run(&["log", "--format=%H%x00%B%x1e", &format!("{base}..{branch}")])?;
        let mut out = Vec::new();
        for entry in raw.split('\x1e').filter(|s| !s.trim().is_empty()) {
            let body = entry
                .split_once('\x00')
                .map(|x| x.1)
                .unwrap_or("")
                .trim()
                .to_string();
            let (subject, rest) = body.split_once('\n').unwrap_or((body.as_str(), ""));
            out.push(CommitSummary {
                subject: subject.trim().to_string(),
                body: rest.trim().to_string(),
            });
        }
        Ok(out)
    }
}

impl Git {
    pub fn discover(start: &Path) -> Result<Self, StackError> {
        let out = Command::new("git")
            .arg("-C")
            .arg(start)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .map_err(|e| StackError::Other(format!("failed to invoke git: {e}")))?;
        if !out.status.success() {
            return Err(StackError::Other(format!(
                "not a git repository (or any of the parent directories): {}",
                start.display()
            )));
        }
        let top = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(Self {
            cwd: PathBuf::from(top),
        })
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Absolute path of the common git directory (shared by all worktrees of one clone).
    pub fn common_dir(&self) -> Result<PathBuf, StackError> {
        let out = self.run(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        Ok(PathBuf::from(out.trim()))
    }

    /// URL of the `origin` remote, or `None` if it isn't configured.
    pub fn origin_url(&self) -> Result<Option<String>, StackError> {
        let result = Command::new("git")
            .arg("-C")
            .arg(&self.cwd)
            .args(["remote", "get-url", "origin"])
            .output()
            .map_err(|e| StackError::Other(format!("failed to invoke git: {e}")))?;
        if !result.status.success() {
            return Ok(None);
        }
        let url = String::from_utf8_lossy(&result.stdout).trim().to_string();
        if url.is_empty() {
            Ok(None)
        } else {
            Ok(Some(url))
        }
    }

    /// Name of the currently checked-out branch, or `None` for detached HEAD.
    pub fn current_branch(&self) -> Result<Option<String>, StackError> {
        let out = self.run(&["symbolic-ref", "--quiet", "--short", "HEAD"]);
        match out {
            Ok(s) => {
                let s = s.trim();
                if s.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(s.to_string()))
                }
            }
            // symbolic-ref exits non-zero on detached HEAD; treat as None.
            Err(_) => Ok(None),
        }
    }

    /// Resolve a rev to its SHA.
    pub fn rev_parse(&self, rev: &str) -> Result<String, StackError> {
        Ok(self.run(&["rev-parse", rev])?.trim().to_string())
    }

    /// True if `ancestor` is an ancestor of `descendant`.
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool, StackError> {
        let status = Command::new("git")
            .arg("-C")
            .arg(&self.cwd)
            .args(["merge-base", "--is-ancestor", ancestor, descendant])
            .status()
            .map_err(|e| StackError::Other(format!("failed to invoke git: {e}")))?;
        Ok(status.success())
    }

    pub fn branch_exists(&self, name: &str) -> Result<bool, StackError> {
        let status = Command::new("git")
            .arg("-C")
            .arg(&self.cwd)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{name}"),
            ])
            .status()
            .map_err(|e| StackError::Other(format!("failed to invoke git: {e}")))?;
        Ok(status.success())
    }

    pub fn checkout(&self, branch: &str) -> Result<(), StackError> {
        self.run(&["checkout", branch])?;
        Ok(())
    }

    /// Create `branch` pointing at `start_point` and switch to it.
    pub fn create_branch(&self, branch: &str, start_point: &str) -> Result<(), StackError> {
        self.run(&["checkout", "-b", branch, start_point])?;
        Ok(())
    }

    /// Create `branch` pointing at `start_point` without switching to it.
    pub fn create_branch_no_checkout(
        &self,
        branch: &str,
        start_point: &str,
    ) -> Result<(), StackError> {
        self.run(&["branch", branch, start_point])?;
        Ok(())
    }

    /// Run `git checkout <branch>` inside an existing worktree at `at`.
    pub fn checkout_at(&self, at: &Path, branch: &str) -> Result<(), StackError> {
        let out = Command::new("git")
            .arg("-C")
            .arg(at)
            .args(["checkout", branch])
            .output()
            .map_err(|e| StackError::Other(format!("failed to invoke git: {e}")))?;
        if !out.status.success() {
            return Err(StackError::Other(format!(
                "git checkout {branch} (in worktree {}) failed: {}",
                at.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }

    /// List every worktree of this clone with its checked-out branch.
    /// Parses `git worktree list --porcelain`. Worktrees in detached HEAD
    /// are omitted because their `branch` field is absent.
    pub fn worktrees(&self) -> Result<Vec<(PathBuf, String)>, StackError> {
        let raw = self.run(&["worktree", "list", "--porcelain"])?;
        let mut out = Vec::new();
        let mut current_path: Option<PathBuf> = None;
        let mut current_branch: Option<String> = None;
        for line in raw.lines() {
            if let Some(rest) = line.strip_prefix("worktree ") {
                if let (Some(p), Some(b)) = (current_path.take(), current_branch.take()) {
                    out.push((p, b));
                }
                current_path = Some(PathBuf::from(rest));
                current_branch = None;
            } else if let Some(rest) = line.strip_prefix("branch refs/heads/") {
                current_branch = Some(rest.to_string());
            } else if line.is_empty() {
                if let (Some(p), Some(b)) = (current_path.take(), current_branch.take()) {
                    out.push((p, b));
                }
            }
        }
        if let (Some(p), Some(b)) = (current_path, current_branch) {
            out.push((p, b));
        }
        Ok(out)
    }

    /// Run `git worktree prune -v` and return its verbose output (one line
    /// per pruned entry). Empty string when nothing was prunable.
    pub fn worktree_prune(&self) -> Result<String, StackError> {
        Ok(self.run(&["worktree", "prune", "-v"])?.trim().to_string())
    }

    /// Dry-run variant: report what `worktree prune` would remove without
    /// actually removing.
    pub fn worktree_prune_dry_run(&self) -> Result<String, StackError> {
        Ok(self
            .run(&["worktree", "prune", "-v", "--dry-run"])?
            .trim()
            .to_string())
    }

    /// Create a new worktree at `path` for `branch` (which must already exist).
    pub fn worktree_add(&self, path: &Path, branch: &str) -> Result<(), StackError> {
        self.run(&["worktree", "add", &path.to_string_lossy(), branch])?;
        Ok(())
    }

    pub fn enable_rerere(&self) -> Result<(), StackError> {
        self.run(&["config", "rerere.enabled", "true"])?;
        Ok(())
    }

    pub fn add_all(&self) -> Result<(), StackError> {
        self.run(&["add", "-A"])?;
        Ok(())
    }

    pub fn add_tracked(&self) -> Result<(), StackError> {
        self.run(&["add", "-u"])?;
        Ok(())
    }

    pub fn commit(&self, message: &str) -> Result<(), StackError> {
        self.run(&["commit", "-q", "-m", message])?;
        Ok(())
    }

    /// True if there are no commits on `branch` beyond `start_point` and the
    /// working tree is clean.
    pub fn has_commits(&self, branch: &str) -> Result<bool, StackError> {
        let out = self
            .run(&["rev-list", "--count", branch])?
            .trim()
            .parse::<u64>()
            .unwrap_or(0);
        Ok(out > 0)
    }

    pub fn trunk_branch(&self) -> Result<String, StackError> {
        // Try the symbolic ref of origin/HEAD first.
        if let Ok(s) = self.run(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]) {
            let s = s.trim();
            if let Some(branch) = s.strip_prefix("origin/") {
                return Ok(branch.to_string());
            }
        }
        // Common defaults.
        for candidate in ["main", "master"] {
            if self.branch_exists(candidate)? {
                return Ok(candidate.to_string());
            }
        }
        Err(StackError::Other(
            "could not determine default trunk branch; pass --base".into(),
        ))
    }

    pub fn fetch(&self, remote: &str) -> Result<(), StackError> {
        self.run(&["fetch", "--prune", remote])?;
        Ok(())
    }

    /// Fast-forward `branch` to match `remote/branch`. Returns Ok(false) if
    /// they already match.
    pub fn fast_forward(&self, branch: &str, remote: &str) -> Result<bool, StackError> {
        let local = self.rev_parse(branch)?;
        let remote_ref = format!("refs/remotes/{remote}/{branch}");
        let remote_sha = match self.rev_parse(&remote_ref) {
            Ok(s) => s,
            Err(_) => return Ok(false),
        };
        if local == remote_sha {
            return Ok(false);
        }
        // Only allow fast-forward.
        if !self.is_ancestor(&local, &remote_sha)? {
            return Err(StackError::Other(format!(
                "{branch} has diverged from {remote}/{branch}; manual intervention required"
            )));
        }
        // Update the ref without checkout, to keep current HEAD stable.
        self.run(&["update-ref", &format!("refs/heads/{branch}"), &remote_sha])?;
        Ok(true)
    }

    /// `git rebase --onto <new_base> <old_base> <branch>`. Returns Ok(()) on
    /// success, Err(StackError::RebaseConflict) on conflict (the working tree
    /// is left in the rebasing state for the user/agent to fix up).
    pub fn rebase_onto(
        &self,
        new_base: &str,
        old_base: &str,
        branch: &str,
    ) -> Result<(), StackError> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.cwd)
            .args(["rebase", "--onto", new_base, old_base, branch])
            .output()
            .map_err(|e| StackError::Other(format!("failed to invoke git: {e}")))?;
        if out.status.success() {
            return Ok(());
        }
        // Detect conflict vs other failures.
        if self.rebase_in_progress() {
            return Err(StackError::RebaseConflict);
        }
        Err(StackError::Other(format!(
            "rebase failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }

    pub fn rebase_continue(&self) -> Result<(), StackError> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.cwd)
            .env("GIT_EDITOR", "true")
            .args(["rebase", "--continue"])
            .output()
            .map_err(|e| StackError::Other(format!("failed to invoke git: {e}")))?;
        if out.status.success() {
            return Ok(());
        }
        if self.rebase_in_progress() {
            return Err(StackError::RebaseConflict);
        }
        Err(StackError::Other(format!(
            "rebase --continue failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }

    pub fn rebase_abort(&self) -> Result<(), StackError> {
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.cwd)
            .args(["rebase", "--abort"])
            .status();
        Ok(())
    }

    pub fn rebase_in_progress(&self) -> bool {
        let git_dir = match self.run(&["rev-parse", "--git-dir"]) {
            Ok(s) => PathBuf::from(s.trim()),
            Err(_) => return false,
        };
        let abs = if git_dir.is_absolute() {
            git_dir
        } else {
            self.cwd.join(git_dir)
        };
        abs.join("rebase-merge").exists() || abs.join("rebase-apply").exists()
    }

    pub fn push_atomic(&self, remote: &str, branches: &[&str]) -> Result<(), StackError> {
        if branches.is_empty() {
            return Ok(());
        }
        let mut args = vec!["push", "--force-with-lease", "--atomic", remote];
        args.extend(branches.iter().copied());
        self.run(&args)?;
        Ok(())
    }

    fn run(&self, args: &[&str]) -> Result<String, StackError> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.cwd)
            .args(args)
            .output()
            .map_err(|e| StackError::Other(format!("failed to invoke git: {e}")))?;
        if !out.status.success() {
            return Err(StackError::Other(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }
}

/// Convert any remote URL to the canonical `github.com:owner/repo` form.
/// Returns `None` for non-GitHub URLs — caller will fall back to local identity.
pub fn canonicalise_github_url(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches(".git");

    // git@github.com:owner/repo
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        return Some(format!("github.com:{}", rest));
    }
    // ssh://git@github.com/owner/repo
    if let Some(rest) = url.strip_prefix("ssh://git@github.com/") {
        return Some(format!("github.com:{}", rest));
    }
    // https://github.com/owner/repo  or  http://...
    for scheme in ["https://github.com/", "http://github.com/"] {
        if let Some(rest) = url.strip_prefix(scheme) {
            return Some(format!("github.com:{}", rest));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalises_ssh() {
        assert_eq!(
            canonicalise_github_url("git@github.com:acme/platform-api.git"),
            Some("github.com:acme/platform-api".into())
        );
    }

    #[test]
    fn canonicalises_https() {
        assert_eq!(
            canonicalise_github_url("https://github.com/flashingpumpkin/git-stack"),
            Some("github.com:flashingpumpkin/git-stack".into())
        );
    }

    #[test]
    fn canonicalises_https_dot_git() {
        assert_eq!(
            canonicalise_github_url("https://github.com/flashingpumpkin/git-stack.git"),
            Some("github.com:flashingpumpkin/git-stack".into())
        );
    }

    #[test]
    fn returns_none_for_non_github() {
        assert_eq!(canonicalise_github_url("git@gitlab.com:foo/bar.git"), None);
    }
}
