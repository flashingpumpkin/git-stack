use std::path::Path;

use crate::domain::{PoolFile, Stack, StackError, StackFile};
use crate::git::Git;
use crate::store::{open, RepoIdentity, StoreGuard};

pub mod add;
pub mod checkout;
pub mod drop;
pub mod init;
pub mod list;
pub mod pool;
pub mod pr_refresh;
pub mod prune;
pub mod rebase;
pub mod remove;
pub mod submit;
pub mod switch;
pub mod sync;
pub mod view;
pub mod walk;

/// Drop a stack from `file` and remove the worktrees git knows about for
/// its branches. Shared by `stack remove` and `stack prune` (when a stack
/// is fully merged).
///
/// Returns the absolute paths of worktrees that were actually removed,
/// so callers can report them. The caller is responsible for persisting
/// the modified `file` and for emitting the top-level "dropped stack X"
/// message — this helper only handles the on-disk side-effects.
///
/// Refuses on dirty worktrees unless `force` is true. When `force` is
/// true the dirty check is skipped and `git worktree remove --force` is
/// used. Returns `Err(InvalidArgs)` when `cwd` lives inside one of the
/// worktrees that would be removed.
pub fn drop_stack_with_worktrees(
    ctx: &Context,
    file: &mut StackFile,
    stack_index: usize,
    cwd: &Path,
    force: bool,
    dry_run: bool,
) -> Result<Vec<std::path::PathBuf>, StackError> {
    use std::path::PathBuf;

    let stack_branches: Vec<String> = file.stacks[stack_index]
        .branches
        .iter()
        .map(|b| b.branch.clone())
        .collect();
    let all_worktrees = ctx.git.worktrees().unwrap_or_default();
    let our_worktrees: Vec<PathBuf> = all_worktrees
        .iter()
        .filter(|(_, b)| stack_branches.iter().any(|sb| sb == b))
        .map(|(p, _)| p.clone())
        .collect();

    // Refuse to nuke uncommitted work unless --force.
    if !force {
        let mut dirty: Vec<PathBuf> = Vec::new();
        for p in &our_worktrees {
            if !p.exists() {
                continue;
            }
            if !ctx.git.is_worktree_clean(p)? {
                dirty.push(p.clone());
            }
        }
        if !dirty.is_empty() {
            eprintln!("✗ refusing to drop stack: worktree has uncommitted changes");
            for p in &dirty {
                eprintln!("    {}", p.display());
            }
            eprintln!(
                "  commit, stash, or discard the changes — or re-run with --force to override."
            );
            return Err(StackError::InvalidArgs(
                "worktree has uncommitted changes".into(),
            ));
        }
    }

    // Skip any worktree we're currently sitting inside. `git worktree
    // remove` would refuse anyway, and skipping it here keeps the overall
    // operation atomic: the stack metadata still gets dropped, and the
    // user can clean up the surviving directory by hand. (Most often this
    // is the main working copy, when the stack was created with
    // `--no-worktree`.)
    let our_worktrees: Vec<PathBuf> = our_worktrees
        .into_iter()
        .filter(|p| !cwd.starts_with(p))
        .collect();

    let mut removed: Vec<PathBuf> = Vec::new();
    if !dry_run {
        for p in &our_worktrees {
            if !p.exists() {
                continue;
            }
            ctx.git.worktree_remove(p, force)?;
            removed.push(p.clone());
        }
        file.stacks.remove(stack_index);
    } else {
        // Dry-run: surface what *would* be removed, in the order we'd do it,
        // but don't touch anything.
        removed = our_worktrees.into_iter().filter(|p| p.exists()).collect();
    }
    Ok(removed)
}

/// Push every active branch in the current stack with `--force-with-lease --atomic`.
/// Shared by the `push` CLI command and `sync`.
pub fn push_active(remote: &str) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let cs = Context::current_stack(&cwd)?;
    let names: Vec<&str> = cs
        .stack()
        .active_branches()
        .iter()
        .map(|b| b.branch.as_str())
        .collect();
    if names.is_empty() {
        eprintln!("ℹ nothing to push");
        return Ok(());
    }
    cs.ctx.git.push_atomic(remote, &names)?;
    eprintln!("✓ pushed {} branch(es) to {remote}", names.len());
    Ok(())
}

/// Common context every op needs: a git adapter, the repo identity, and an
/// open (locked) store guard.
///
/// Most ops want one of the higher-level entry points:
///
/// - [`Context::current_stack`] for "do something to the stack I'm on".
/// - [`Context::with_file`] for "look at every stack in the repo".
///
/// `Context::open` is the raw constructor; reach for it when neither
/// pattern fits (e.g. `init`, which is creating the first stack).
pub struct Context {
    pub git: Git,
    pub identity: RepoIdentity,
    pub store: StoreGuard,
}

impl Context {
    pub fn open(start: &Path) -> Result<Self, StackError> {
        let git = Git::discover(start)?;
        let identity = RepoIdentity::from_git(&git)?;
        let store = open(&identity)?;
        Ok(Self {
            git,
            identity,
            store,
        })
    }

    /// Open the store and locate the stack the current branch lives in.
    /// Returns `NotInStack` if no stack matches, `Disambiguation` if the
    /// current branch belongs to more than one.
    pub fn current_stack(start: &Path) -> Result<CurrentStack, StackError> {
        let ctx = Self::open(start)?;
        let file = ctx.store.load(&ctx.identity)?;
        let current_branch = ctx.git.current_branch()?.ok_or_else(|| {
            StackError::Other("detached HEAD; check out a branch in the stack first".into())
        })?;
        let matches: Vec<usize> = file
            .stacks
            .iter()
            .enumerate()
            .filter(|(_, s)| s.contains(&current_branch))
            .map(|(i, _)| i)
            .collect();
        let stack_index = match matches.len() {
            0 => return Err(StackError::NotInStack),
            1 => matches[0],
            _ => return Err(StackError::Disambiguation(current_branch)),
        };
        Ok(CurrentStack {
            ctx,
            file,
            stack_index,
            current_branch,
        })
    }

    /// Open the store and load the full file without resolving any
    /// particular stack. Used by `list`, `prune`, and anything else that
    /// touches every stack.
    pub fn with_file(start: &Path) -> Result<(Self, StackFile), StackError> {
        let ctx = Self::open(start)?;
        let file = ctx.store.load(&ctx.identity)?;
        Ok((ctx, file))
    }

    /// Open the store and load `pools.json`. Pool data lives in a
    /// separate file from stacks; ops that touch pools call this
    /// instead of `with_file`.
    pub fn with_pool_file(start: &Path) -> Result<(Self, PoolFile), StackError> {
        let ctx = Self::open(start)?;
        let file = ctx.store.load_pools(&ctx.identity)?;
        Ok((ctx, file))
    }
}

/// A locked, loaded view of the user's current stack.
///
/// The handle owns the `Context` (and therefore the store lock) and a
/// mutable `StackFile`. Read the stack via [`stack()`](Self::stack), mutate
/// via [`stack_mut()`](Self::stack_mut), persist via
/// [`save()`](Self::save). The lock is released when the handle is dropped.
pub struct CurrentStack {
    pub ctx: Context,
    pub file: StackFile,
    pub stack_index: usize,
    pub current_branch: String,
}

impl CurrentStack {
    pub fn stack(&self) -> &Stack {
        &self.file.stacks[self.stack_index]
    }

    pub fn stack_mut(&mut self) -> &mut Stack {
        &mut self.file.stacks[self.stack_index]
    }

    pub fn save(&self) -> Result<(), StackError> {
        self.ctx.store.save(&self.file)
    }
}
