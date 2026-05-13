//! Standalone CLI for `pool`. Two binaries dispatch to this module:
//!
//! - `pool ...`     — direct invocation
//! - `git-pool ...` — git's auto-discovered subcommand (`git pool ...`)
//!
//! The parser intentionally lives outside the `stack` CLI so that `pool`
//! is a first-class top-level command, not a subcommand of `stack`.

use std::process::ExitCode as StdExitCode;

use clap::{Parser, Subcommand};

use crate::domain::StackError;

#[derive(Parser, Debug)]
#[command(
    name = "pool",
    about = "Track a flat collection of independent branches with PR state, comment activity, and rebase health",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Create (or adopt) a single branch off a base, set up its
    /// worktree, and add it to a pool. Auto-creates the pool. Prints
    /// the worktree path on stdout so callers can
    /// `cd "$(pool init <branch> | tail -1)"`.
    Init {
        /// Target a named pool. Omit to use the default pool.
        #[arg(long)]
        pool: Option<String>,
        /// Base branch (defaults to the pool's trunk, or the repo's
        /// default branch when creating the pool).
        #[arg(short = 'b', long)]
        base: Option<String>,
        /// Adopt an existing branch instead of creating a new one.
        #[arg(short = 'a', long)]
        adopt: bool,
        /// Override the worktree path. Defaults to
        /// `~/Worktrees/<repo>/<branch>`.
        #[arg(short = 'w', long, conflicts_with = "no_worktree")]
        worktree: Option<std::path::PathBuf>,
        /// Don't create a worktree; switch the current working tree to
        /// the new branch instead.
        #[arg(long = "no-worktree")]
        no_worktree: bool,
        /// Branch to create (or adopt).
        branch: String,
    },
    /// Add one or more existing local branches to a pool. Creates the
    /// pool on first use. Base resolution order: `--base`, then the PR's
    /// GitHub base ref, then the pool's default trunk.
    Add {
        /// Target a named pool. Omit to use the default pool.
        #[arg(long)]
        pool: Option<String>,
        /// Base branch override. Applied to every branch in this
        /// invocation.
        #[arg(long)]
        base: Option<String>,
        /// One or more branches to add (must already exist locally).
        #[arg(required = true)]
        branches: Vec<String>,
    },
    /// Bulk-adopt every open PR authored by you whose head branch exists
    /// locally and isn't already in a stack or pool.
    Adopt {
        /// Target a named pool. Omit to use the default pool.
        #[arg(long)]
        pool: Option<String>,
    },
    /// Remove a branch from its pool. The git branch itself is left alone.
    Remove {
        /// Branch to remove (defaults to the current branch).
        branch: Option<String>,
    },
    /// Drop branches whose PRs have been merged on GitHub.
    Prune {
        /// Target a named pool. Omit to prune every pool.
        #[arg(long)]
        pool: Option<String>,
        /// Report what would be removed without writing.
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Skip refreshing PR state from GitHub before deciding what's merged.
        #[arg(long = "no-refresh")]
        no_refresh: bool,
    },
    /// List all pools and their branches, with PR state, rebase health,
    /// and unread-comment counts.
    List {
        /// Refresh PR state from GitHub before listing. Without this,
        /// information comes from the local store and may be stale.
        #[arg(short = 'r', long)]
        refresh: bool,
        /// Machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Refresh PR state for a pool: discover PRs by head ref, update
    /// merged flags and comment counts, drop merged branches, and print
    /// what changed since the previous refresh.
    Refresh {
        /// Target a named pool. Omit to use the default pool.
        #[arg(long)]
        pool: Option<String>,
    },
    /// Push every active branch and open a PR for any that doesn't have
    /// one yet. PRs already linked have their stored data refreshed in
    /// place. Each PR targets its branch's own base ref.
    Submit {
        /// Target a named pool. Omit to use the default pool.
        #[arg(long)]
        pool: Option<String>,
        /// Create PRs as drafts.
        #[arg(long)]
        draft: bool,
        #[arg(long, default_value = "origin")]
        remote: String,
        /// Skip the pre-submit PR refresh.
        #[arg(long = "no-refresh")]
        no_refresh: bool,
    },
    /// Rebase every active branch in the pool onto the live head of
    /// its own base.
    Rebase {
        /// Target a named pool. Omit to use the default pool.
        #[arg(long)]
        pool: Option<String>,
        #[arg(long, default_value = "origin")]
        remote: String,
        /// Skip the pre-rebase PR refresh.
        #[arg(long = "no-refresh")]
        no_refresh: bool,
        /// Finalise an in-progress rebase after resolving conflicts.
        #[arg(long = "continue", conflicts_with = "abort")]
        cont: bool,
        /// Abort an in-progress rebase.
        #[arg(long)]
        abort: bool,
    },
}

pub fn run() -> StdExitCode {
    let cli = Cli::parse();
    let result = dispatch(cli.command);
    match result {
        Ok(()) => StdExitCode::SUCCESS,
        Err(e) => {
            eprintln!("✗ {e}");
            e.exit_code().into()
        }
    }
}

fn dispatch(cmd: Cmd) -> Result<(), StackError> {
    use crate::ops::pool;
    match cmd {
        Cmd::Init {
            pool: name,
            base,
            adopt,
            worktree,
            no_worktree,
            branch,
        } => pool::init::run(pool::init::InitArgs {
            pool: name,
            base,
            adopt,
            worktree,
            no_worktree,
            branch,
        }),
        Cmd::Add {
            pool: name,
            base,
            branches,
        } => pool::add::run(pool::add::AddArgs {
            pool: name,
            base,
            branches,
        }),
        Cmd::Adopt { pool: name } => pool::adopt::run(pool::adopt::AdoptArgs { pool: name }),
        Cmd::Remove { branch } => pool::remove::run(branch),
        Cmd::Prune {
            pool: name,
            dry_run,
            no_refresh,
        } => pool::prune::run(pool::prune::PruneArgs {
            pool: name,
            dry_run,
            no_refresh,
        }),
        Cmd::List { refresh, json } => pool::list::run(pool::list::ListArgs { refresh, json }),
        Cmd::Refresh { pool: name } => {
            pool::refresh::run(pool::refresh::RefreshArgs { pool: name })
        }
        Cmd::Submit {
            pool: name,
            draft,
            remote,
            no_refresh,
        } => pool::submit::run(pool::submit::SubmitArgs {
            pool: name,
            draft,
            remote,
            no_refresh,
        }),
        Cmd::Rebase {
            pool: name,
            remote,
            no_refresh,
            cont,
            abort,
        } => pool::rebase::run(pool::rebase::RebaseArgs {
            pool: name,
            remote,
            no_refresh,
            cont,
            abort,
        }),
    }
}
