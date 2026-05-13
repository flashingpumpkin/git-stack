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
    /// Rebase every active branch in the pool onto the live trunk head.
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
