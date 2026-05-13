use std::path::Path;

use crate::domain::{Stack, StackError, StackFile};
use crate::git::Git;
use crate::store::{open, RepoIdentity, StoreGuard};

pub mod add;
pub mod checkout;
pub mod drop;
pub mod init;
pub mod list;
pub mod navigate;
pub mod pr_refresh;
pub mod prune;
pub mod push;
pub mod rebase;
pub mod submit;
pub mod switch;
pub mod sync;
pub mod unstack;
pub mod view;
pub mod walk;

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
