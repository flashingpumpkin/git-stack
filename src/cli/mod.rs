use std::path::PathBuf;
use std::process::ExitCode as StdExitCode;

use clap::{Parser, Subcommand};

use crate::domain::StackError;
use crate::ops::Context;

pub mod migrate;

#[derive(Debug, Clone, Copy)]
enum Direction {
    Up,
    Down,
    Top,
    Bottom,
}

fn navigate(direction: Direction, count: usize) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let cs = Context::current_stack(&cwd)?;

    let target = match direction {
        Direction::Up => cs.stack().step_active(&cs.current_branch, count as isize),
        Direction::Down => cs
            .stack()
            .step_active(&cs.current_branch, -(count as isize)),
        Direction::Top => cs.stack().top_active(),
        Direction::Bottom => cs.stack().bottom_active(),
    }
    .ok_or_else(|| StackError::Other("no active branches in stack".into()))?;

    if target.branch == cs.current_branch {
        eprintln!("ℹ already at {}", cs.current_branch);
        return Ok(());
    }

    cs.ctx.git.checkout(&target.branch)?;
    eprintln!("✓ checked out {}", target.branch);
    Ok(())
}

#[derive(Parser, Debug)]
#[command(
    name = "stack",
    about = "Stacked-branch / stacked-PR workflow with worktree-safe state",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Import a `.git/gh-stack` file into the central store.
    Migrate {
        /// Path to a git repository (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Overwrite an existing entry in the central store.
        #[arg(long)]
        force: bool,
    },
    /// Print the resolved repo identity and the path of its store directory.
    Where {
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// List every stack tracked for this repository.
    List {
        /// Machine-readable JSON output.
        #[arg(long)]
        json: bool,
        /// Refresh PR state from GitHub before printing. Without this, PR
        /// info comes from the local store and may be stale.
        #[arg(short = 'r', long)]
        refresh: bool,
    },
    /// Show the current stack.
    View {
        /// Emit machine-readable JSON instead of formatted text.
        #[arg(long)]
        json: bool,
        /// Refresh PR state from GitHub before printing. Without this, PR
        /// info comes from the local store and may be stale.
        #[arg(short = 'r', long)]
        refresh: bool,
    },
    /// Move toward the top of the stack (away from trunk).
    Up {
        #[arg(default_value_t = 1)]
        count: usize,
    },
    /// Move toward the bottom of the stack (closer to trunk).
    Down {
        #[arg(default_value_t = 1)]
        count: usize,
    },
    /// Jump to the top of the stack.
    Top,
    /// Jump to the bottom of the stack.
    Bottom,
    /// Check out a branch belonging to a locally-tracked stack.
    Checkout { target: String },
    /// Interactive branch picker for the current stack.
    Switch,
    /// Initialise a new stack. By default also creates a worktree at
    /// `~/Worktrees/<repo>/<bottom-branch>` and prints its path on stdout.
    Init {
        /// Trunk branch (defaults to the repo's default branch).
        #[arg(short = 'b', long)]
        base: Option<String>,
        /// Branch-name prefix (e.g. `feat`). Subsequent `add` calls only need the suffix.
        #[arg(short = 'p', long)]
        prefix: Option<String>,
        /// Adopt existing branches instead of creating new ones.
        #[arg(short = 'a', long)]
        adopt: bool,
        /// Override the worktree path.
        #[arg(short = 'w', long, conflicts_with = "no_worktree")]
        worktree: Option<std::path::PathBuf>,
        /// Don't create a worktree; init on the current working tree instead.
        #[arg(long = "no-worktree")]
        no_worktree: bool,
        /// Branch names (suffix only when prefix is set).
        #[arg(required = true)]
        branches: Vec<String>,
    },
    /// Add a new branch on top of the current stack.
    Add {
        /// Stage all changes (including untracked) before committing.
        #[arg(short = 'A', long, conflicts_with = "update")]
        all: bool,
        /// Stage tracked changes before committing.
        #[arg(short = 'u', long)]
        update: bool,
        /// Commit message (required with -A/-u).
        #[arg(short = 'm', long)]
        message: Option<String>,
        /// Branch name (suffix only when stack has a prefix).
        branch: String,
    },
    /// Stop tracking the stack containing `branch` and remove its worktree(s).
    /// Leaves the underlying git branches alone.
    Remove {
        /// A branch belonging to the stack to remove.
        branch: String,
        /// Remove worktrees even if they have uncommitted changes.
        #[arg(long)]
        force: bool,
    },
    /// Remove a single branch from its stack, re-parenting children onto its parent.
    Drop {
        /// Branch to drop (defaults to the current branch).
        branch: Option<String>,
    },
    /// Housekeeping: drop fully-merged stacks (including their worktrees)
    /// and run `git worktree prune`.
    Prune {
        /// Report what would be removed without writing.
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Skip refreshing PR state from GitHub before deciding what's merged.
        #[arg(long = "no-refresh")]
        no_refresh: bool,
        /// Remove worktrees even if they have uncommitted changes.
        #[arg(long)]
        force: bool,
    },
    /// Push all active branches in the current stack.
    Push {
        #[arg(long, default_value = "origin")]
        remote: String,
    },
    /// Cascade rebase the current stack onto trunk.
    Rebase {
        #[arg(long, conflicts_with_all = ["downstack", "cont", "abort"])]
        upstack: bool,
        #[arg(long, conflicts_with_all = ["upstack", "cont", "abort"])]
        downstack: bool,
        #[arg(long = "continue", conflicts_with = "abort")]
        cont: bool,
        #[arg(long)]
        abort: bool,
        #[arg(long, default_value = "origin")]
        remote: String,
        /// Skip refreshing PR state from GitHub before planning. Use only
        /// when you know none of your PRs were squash-merged since the last
        /// refresh — otherwise rebase will try to replay merged commits.
        #[arg(long = "no-refresh")]
        no_refresh: bool,
    },
    /// Fetch, fast-forward trunk, cascade rebase, push.
    Sync {
        #[arg(long, default_value = "origin")]
        remote: String,
        /// Skip refreshing PR state from GitHub.
        #[arg(long = "no-refresh")]
        no_refresh: bool,
    },
    /// Push branches and create/refresh GitHub PRs.
    Submit {
        /// Accepted for back-compat with `gh stack submit`. The CLI never
        /// prompts; titles are always auto-generated.
        #[arg(long)]
        auto: bool,
        /// Create PRs as drafts.
        #[arg(long)]
        draft: bool,
        #[arg(long, default_value = "origin")]
        remote: String,
        /// Skip refreshing PR state from GitHub before pushing.
        #[arg(long = "no-refresh")]
        no_refresh: bool,
    },
}

pub fn run() -> StdExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Cmd::Migrate { repo, force } => migrate::run(repo, force),
        Cmd::Where { repo } => where_cmd(repo),
        Cmd::List { json, refresh } => {
            crate::ops::list::run(crate::ops::list::ListArgs { json, refresh })
        }
        Cmd::View { json, refresh } => {
            crate::ops::view::run(crate::ops::view::ViewArgs { json, refresh })
        }
        Cmd::Up { count } => navigate(Direction::Up, count),
        Cmd::Down { count } => navigate(Direction::Down, count),
        Cmd::Top => navigate(Direction::Top, 0),
        Cmd::Bottom => navigate(Direction::Bottom, 0),
        Cmd::Checkout { target } => crate::ops::checkout::run(&target),
        Cmd::Switch => crate::ops::switch::run(),
        Cmd::Init {
            base,
            prefix,
            adopt,
            worktree,
            no_worktree,
            branches,
        } => crate::ops::init::run(crate::ops::init::InitArgs {
            base,
            prefix,
            adopt,
            branches,
            worktree,
            no_worktree,
        }),
        Cmd::Add {
            all,
            update,
            message,
            branch,
        } => crate::ops::add::run(crate::ops::add::AddArgs {
            branch,
            all,
            update,
            message,
        }),
        Cmd::Remove { branch, force } => {
            crate::ops::remove::run(crate::ops::remove::RemoveArgs { branch, force })
        }
        Cmd::Drop { branch } => crate::ops::drop::run(branch),
        Cmd::Prune {
            dry_run,
            no_refresh,
            force,
        } => crate::ops::prune::run(crate::ops::prune::PruneArgs {
            dry_run,
            no_refresh,
            force,
        }),
        Cmd::Push { remote } => crate::ops::push_active(&remote),
        Cmd::Rebase {
            upstack,
            downstack,
            cont,
            abort,
            remote,
            no_refresh,
        } => {
            let scope = if upstack {
                crate::ops::rebase::Scope::Upstack
            } else if downstack {
                crate::ops::rebase::Scope::Downstack
            } else {
                crate::ops::rebase::Scope::Full
            };
            crate::ops::rebase::run(crate::ops::rebase::RebaseArgs {
                scope,
                cont,
                abort,
                remote,
                no_refresh,
            })
        }
        Cmd::Sync { remote, no_refresh } => {
            crate::ops::sync::run(crate::ops::sync::SyncArgs { remote, no_refresh })
        }
        Cmd::Submit {
            auto,
            draft,
            remote,
            no_refresh,
        } => crate::ops::submit::run(crate::ops::submit::SubmitArgs {
            auto,
            draft,
            remote,
            no_refresh,
        }),
    };

    match result {
        Ok(()) => StdExitCode::SUCCESS,
        Err(e) => {
            eprintln!("✗ {e}");
            e.exit_code().into()
        }
    }
}

fn where_cmd(repo: Option<PathBuf>) -> Result<(), StackError> {
    use crate::git::Git;
    use crate::store::{RepoIdentity, StorePaths};

    let start = repo.unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let git = Git::discover(&start)?;
    let id = RepoIdentity::from_git(&git)?;
    let paths = StorePaths::for_identity(&id);
    println!("repository = {}", id.repository);
    println!("slug       = {}", id.slug);
    println!("store      = {}", paths.dir.display());
    println!("stacks.json= {}", paths.stacks_json().display());
    Ok(())
}
