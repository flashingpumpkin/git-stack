//! GitHub adapter — shells out to `gh` for all operations so the user's
//! existing auth (keychain, env vars, `gh auth login`) just works.
//!
//! Read paths (`view_pr`, `find_pr_by_head`, `list_my_open_prs`) go through
//! `gh api graphql` so we can query fields the REST-shaped `gh pr view`
//! command doesn't expose (notably `reviewThreads`, which is needed for
//! an accurate inline-comment count). Mutating paths (`create_pr`) keep
//! using `gh pr create` because the CLI handles base/head/draft/body
//! ergonomics that we'd otherwise have to reproduce by hand.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

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
    /// Total observed comment count: top-level PR comments + per-review
    /// summaries + inline review-thread comments. Used by pool tracking
    /// to detect "new comments since last seen". Zero when the adapter
    /// doesn't populate it.
    pub comment_count: u64,
    /// Mergeable status from GitHub. `None` when GitHub hasn't computed
    /// it (e.g. for closed PRs, or while it's still being calculated).
    pub mergeable: Option<bool>,
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

    /// List every open pull request authored by the currently
    /// authenticated user (`gh @me`). Used by `pool adopt` to
    /// bulk-discover candidate branches.
    fn list_my_open_prs(&self) -> Result<Vec<PullRequestInfo>, StackError>;
}

pub struct GhCli {
    cwd: std::path::PathBuf,
    /// Owner/name of the GitHub repo as `gh` sees it (resolved from the
    /// repo's `origin` remote on first GraphQL call, then cached).
    repo: OnceLock<(String, String)>,
    /// Login of the authenticated `gh` user, resolved on first call.
    /// Cached because every `list_my_open_prs` would otherwise hit the
    /// API for the same value.
    viewer: OnceLock<String>,
}

impl GhCli {
    pub fn new(cwd: &Path) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            repo: OnceLock::new(),
            viewer: OnceLock::new(),
        }
    }

    /// Resolve the GitHub `owner/name` for the repo at `self.cwd`. Caches
    /// the result so repeated GraphQL calls don't re-query gh.
    fn repo(&self) -> Result<&(String, String), StackError> {
        if let Some(v) = self.repo.get() {
            return Ok(v);
        }
        #[derive(Deserialize)]
        struct R {
            name: String,
            owner: Owner,
        }
        #[derive(Deserialize)]
        struct Owner {
            login: String,
        }
        let r: R = self.run_gh_json(&["repo", "view", "--json", "name,owner"])?;
        let _ = self.repo.set((r.owner.login, r.name));
        Ok(self.repo.get().expect("just set"))
    }

    /// Login of the authenticated user. Used to attribute `list_my_open_prs`
    /// without a separate `--author @me` round trip in the GraphQL search.
    fn viewer_login(&self) -> Result<&str, StackError> {
        if let Some(v) = self.viewer.get() {
            return Ok(v.as_str());
        }
        let resp: GraphQlResponse<ViewerResp> = self.run_graphql(VIEWER_QUERY, &[])?;
        let login = resp.into_data()?.viewer.login;
        let _ = self.viewer.set(login);
        Ok(self.viewer.get().expect("just set").as_str())
    }

    /// Run `gh` and decode its stdout as JSON.
    fn run_gh_json<T: for<'de> Deserialize<'de>>(&self, args: &[&str]) -> Result<T, StackError> {
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

    /// Run a GraphQL query via `gh api graphql`. `vars` is a list of
    /// `(name, value)` pairs serialised as `-F name=value`. `gh api`
    /// auto-typechecks numeric and boolean values when they're passed
    /// with `-F`; string values become string variables.
    fn run_graphql<T: for<'de> Deserialize<'de>>(
        &self,
        query: &str,
        vars: &[(&str, &str)],
    ) -> Result<T, StackError> {
        let mut cmd = Command::new("gh");
        cmd.current_dir(&self.cwd)
            .args(["api", "graphql", "-f", &format!("query={query}")]);
        for (name, value) in vars {
            cmd.args(["-F", &format!("{name}={value}")]);
        }
        let out = cmd
            .output()
            .map_err(|e| StackError::GitHubApi(format!("failed to invoke gh: {e}")))?;
        if !out.status.success() {
            return Err(StackError::GitHubApi(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        serde_json::from_slice(&out.stdout).map_err(|e| {
            StackError::GitHubApi(format!(
                "failed to parse gh graphql output: {e}\nstdout: {}",
                String::from_utf8_lossy(&out.stdout)
            ))
        })
    }
}

// === GraphQL response envelope ===========================================

#[derive(Deserialize)]
struct GraphQlResponse<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Vec<GraphQlError>,
}

#[derive(Deserialize)]
struct GraphQlError {
    message: String,
}

impl<T> GraphQlResponse<T> {
    fn into_data(self) -> Result<T, StackError> {
        if !self.errors.is_empty() {
            let joined = self
                .errors
                .iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(StackError::GitHubApi(format!("graphql: {joined}")));
        }
        self.data
            .ok_or_else(|| StackError::GitHubApi("graphql: empty data".into()))
    }
}

// === GraphQL shapes ======================================================

/// Fields we extract from any PullRequest node. Used by all three read
/// paths so the parsing layer stays uniform.
#[derive(Deserialize)]
struct GqlPullRequest {
    number: u64,
    id: String,
    url: String,
    state: String,
    #[serde(rename = "headRefName")]
    head_ref: String,
    #[serde(rename = "baseRefName")]
    base_ref: String,
    /// GitHub's `MergeableState` enum: MERGEABLE / CONFLICTING / UNKNOWN.
    /// None for PRs in states where mergeability isn't computed.
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(default)]
    comments: Option<TotalCount>,
    #[serde(default)]
    reviews: Option<TotalCount>,
    #[serde(default, rename = "reviewThreads")]
    review_threads: Option<ThreadConnection>,
}

#[derive(Deserialize)]
struct TotalCount {
    #[serde(rename = "totalCount", default)]
    total_count: u64,
}

#[derive(Deserialize)]
struct ThreadConnection {
    #[serde(default)]
    nodes: Vec<ReviewThread>,
}

#[derive(Deserialize)]
struct ReviewThread {
    #[serde(default)]
    comments: Option<TotalCount>,
}

impl GqlPullRequest {
    fn comment_count(&self) -> u64 {
        let top = self.comments.as_ref().map(|t| t.total_count).unwrap_or(0);
        let reviews = self.reviews.as_ref().map(|t| t.total_count).unwrap_or(0);
        let inline: u64 = self
            .review_threads
            .as_ref()
            .map(|tc| {
                tc.nodes
                    .iter()
                    .map(|t| t.comments.as_ref().map(|c| c.total_count).unwrap_or(0))
                    .sum()
            })
            .unwrap_or(0);
        top + reviews + inline
    }

    fn into_info(self) -> Result<PullRequestInfo, StackError> {
        let state = match self.state.as_str() {
            "OPEN" => PrState::Open,
            "MERGED" => PrState::Merged,
            "CLOSED" => PrState::Closed,
            other => return Err(StackError::GitHubApi(format!("unknown PR state: {other}"))),
        };
        let mergeable = self.mergeable.as_deref().and_then(|s| match s {
            "MERGEABLE" => Some(true),
            "CONFLICTING" => Some(false),
            _ => None,
        });
        let comment_count = self.comment_count();
        Ok(PullRequestInfo {
            number: self.number,
            id: self.id,
            url: self.url,
            state,
            head_ref: self.head_ref,
            base_ref: self.base_ref,
            comment_count,
            mergeable,
        })
    }
}

// === Queries =============================================================

/// Reusable fragment for the fields we want off every PullRequest node.
/// The `reviewThreads(first: 50)` cap is generous — PRs rarely have more
/// than a handful of distinct threads, and we only need totals per thread,
/// not the comments themselves.
const PR_FIELDS: &str = r#"
    number
    id
    url
    state
    headRefName
    baseRefName
    mergeable
    comments { totalCount }
    reviews { totalCount }
    reviewThreads(first: 50) {
        nodes {
            comments { totalCount }
        }
    }
"#;

const VIEWER_QUERY: &str = "query { viewer { login } }";

#[derive(Deserialize)]
struct ViewerResp {
    viewer: ViewerNode,
}

#[derive(Deserialize)]
struct ViewerNode {
    login: String,
}

#[derive(Deserialize)]
struct ViewPrResp {
    repository: Option<ViewPrRepo>,
}

#[derive(Deserialize)]
struct ViewPrRepo {
    #[serde(rename = "pullRequest")]
    pull_request: Option<GqlPullRequest>,
}

#[derive(Deserialize)]
struct ListPrsResp {
    repository: Option<ListPrsRepo>,
}

#[derive(Deserialize)]
struct ListPrsRepo {
    #[serde(rename = "pullRequests")]
    pull_requests: PrConnection,
}

#[derive(Deserialize)]
struct PrConnection {
    #[serde(default)]
    nodes: Vec<GqlPullRequest>,
}

#[derive(Deserialize)]
struct SearchResp {
    search: SearchConn,
}

#[derive(Deserialize)]
struct SearchConn {
    #[serde(default)]
    nodes: Vec<GqlPullRequest>,
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
        let (owner, name) = self.repo()?.clone();
        let number_str = number.to_string();
        let query = format!(
            "query($owner: String!, $name: String!, $number: Int!) {{
                repository(owner: $owner, name: $name) {{
                    pullRequest(number: $number) {{ {PR_FIELDS} }}
                }}
            }}"
        );
        let resp: GraphQlResponse<ViewPrResp> = self.run_graphql(
            &query,
            &[
                ("owner", owner.as_str()),
                ("name", name.as_str()),
                ("number", number_str.as_str()),
            ],
        )?;
        let data = resp.into_data()?;
        let pr = data
            .repository
            .and_then(|r| r.pull_request)
            .ok_or_else(|| StackError::GitHubApi(format!("PR #{number} not found")))?;
        pr.into_info()
    }

    fn find_pr_by_head(&self, branch: &str) -> Result<Option<PullRequestInfo>, StackError> {
        let (owner, name) = self.repo()?.clone();
        let query = format!(
            "query($owner: String!, $name: String!, $head: String!) {{
                repository(owner: $owner, name: $name) {{
                    pullRequests(headRefName: $head, first: 20,
                        orderBy: {{ field: CREATED_AT, direction: DESC }}) {{
                        nodes {{ {PR_FIELDS} }}
                    }}
                }}
            }}"
        );
        let resp: GraphQlResponse<ListPrsResp> = self.run_graphql(
            &query,
            &[
                ("owner", owner.as_str()),
                ("name", name.as_str()),
                ("head", branch),
            ],
        )?;
        let data = resp.into_data()?;
        let mut nodes = data
            .repository
            .map(|r| r.pull_requests.nodes)
            .unwrap_or_default();
        let rank = |s: &str| -> u8 {
            match s {
                "OPEN" => 0,
                "MERGED" => 1,
                "CLOSED" => 2,
                _ => 3,
            }
        };
        nodes.sort_by(|a, b| {
            rank(&a.state)
                .cmp(&rank(&b.state))
                .then(b.number.cmp(&a.number))
        });
        nodes.into_iter().next().map(|p| p.into_info()).transpose()
    }

    fn list_my_open_prs(&self) -> Result<Vec<PullRequestInfo>, StackError> {
        let (owner, name) = self.repo()?.clone();
        let viewer = self.viewer_login()?.to_string();
        // Scope to this repo + this author + open. Search's PR query
        // syntax: `is:pr repo:owner/name author:<login> is:open`.
        let q = format!("is:pr repo:{owner}/{name} author:{viewer} is:open");
        let query = format!(
            "query($q: String!) {{
                search(query: $q, type: ISSUE, first: 100) {{
                    nodes {{ ... on PullRequest {{ {PR_FIELDS} }} }}
                }}
            }}"
        );
        let resp: GraphQlResponse<SearchResp> = self.run_graphql(&query, &[("q", q.as_str())])?;
        let data = resp.into_data()?;
        data.search
            .nodes
            .into_iter()
            .map(|p| p.into_info())
            .collect()
    }
}
