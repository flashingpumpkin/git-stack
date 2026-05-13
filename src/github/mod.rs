//! GitHub adapter — shells out to `gh` for all operations so the user's
//! existing auth (keychain, env vars, `gh auth login`) just works.

use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use crate::domain::StackError;

pub mod fake;

/// Subset of PR fields we need across the codebase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestInfo {
    pub number: u64,
    pub id: String,
    pub url: String,
    pub state: PrState,
    pub head_ref: String,
    pub base_ref: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

/// The port `ops` consume. The real adapter is `GhCli`; tests use `fake::FakeGitHub`.
pub trait GitHub {
    fn create_pr(
        &self,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
        draft: bool,
    ) -> Result<PullRequestInfo, StackError>;

    fn view_pr(&self, number: u64) -> Result<PullRequestInfo, StackError>;

    /// Find the most relevant PR whose head ref is `branch`, if any.
    /// Used by refresh to discover PRs we haven't recorded yet — e.g.
    /// ones opened via the GitHub UI or `gh pr create` directly.
    ///
    /// When several PRs match, prefer open > merged > closed, then the
    /// highest PR number. Returns `Ok(None)` when none match — not an
    /// error.
    fn find_pr_by_head(&self, branch: &str) -> Result<Option<PullRequestInfo>, StackError>;
}

pub struct GhCli {
    cwd: std::path::PathBuf,
}

impl GhCli {
    pub fn new(cwd: &Path) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
        }
    }

    fn run_json<T: for<'de> Deserialize<'de>>(&self, args: &[&str]) -> Result<T, StackError> {
        let out = Command::new("gh")
            .current_dir(&self.cwd)
            .args(args)
            .output()
            .map_err(|e| StackError::GitHubApi(format!("failed to invoke gh: {e}")))?;
        if !out.status.success() {
            return Err(StackError::GitHubApi(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        serde_json::from_slice(&out.stdout).map_err(|e| {
            StackError::GitHubApi(format!(
                "failed to parse gh output: {e}\nstdout: {}",
                String::from_utf8_lossy(&out.stdout)
            ))
        })
    }
}

#[derive(Deserialize)]
struct GhPrView {
    number: u64,
    url: String,
    id: String,
    state: String,
    #[serde(rename = "headRefName")]
    head_ref: String,
    #[serde(rename = "baseRefName")]
    base_ref: String,
}

impl GitHub for GhCli {
    fn create_pr(
        &self,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
        draft: bool,
    ) -> Result<PullRequestInfo, StackError> {
        let mut args: Vec<&str> = vec![
            "pr", "create", "--head", head, "--base", base, "--title", title, "--body", body,
        ];
        if draft {
            args.push("--draft");
        }
        // Create silently; then re-query to get the full data we need.
        let out = Command::new("gh")
            .current_dir(&self.cwd)
            .args(&args)
            .output()
            .map_err(|e| StackError::GitHubApi(format!("failed to invoke gh: {e}")))?;
        if !out.status.success() {
            return Err(StackError::GitHubApi(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        // `gh pr create` prints the PR URL; parse the trailing path segment.
        let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let number: u64 = url
            .rsplit('/')
            .next()
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| {
                StackError::GitHubApi(format!("could not parse PR number from: {url}"))
            })?;
        self.view_pr(number)
    }

    fn view_pr(&self, number: u64) -> Result<PullRequestInfo, StackError> {
        let view: GhPrView = self.run_json(&[
            "pr",
            "view",
            &number.to_string(),
            "--json",
            "number,url,id,state,headRefName,baseRefName",
        ])?;
        gh_view_to_info(view)
    }

    fn find_pr_by_head(&self, branch: &str) -> Result<Option<PullRequestInfo>, StackError> {
        let views: Vec<GhPrView> = self.run_json(&[
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "all",
            "--limit",
            "20",
            "--json",
            "number,url,id,state,headRefName,baseRefName",
        ])?;
        // Sort by (state priority, number desc): open > merged > closed,
        // then prefer the highest-numbered PR within a state bucket.
        let mut views = views;
        let rank = |s: &str| -> u8 {
            match s {
                "OPEN" => 0,
                "MERGED" => 1,
                "CLOSED" => 2,
                _ => 3,
            }
        };
        views.sort_by(|a, b| {
            rank(&a.state)
                .cmp(&rank(&b.state))
                .then(b.number.cmp(&a.number))
        });
        views.into_iter().next().map(gh_view_to_info).transpose()
    }
}

fn gh_view_to_info(view: GhPrView) -> Result<PullRequestInfo, StackError> {
    let state = match view.state.as_str() {
        "OPEN" => PrState::Open,
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        other => return Err(StackError::GitHubApi(format!("unknown PR state: {other}"))),
    };
    Ok(PullRequestInfo {
        number: view.number,
        id: view.id,
        url: view.url,
        state,
        head_ref: view.head_ref,
        base_ref: view.base_ref,
    })
}
