use std::path::Path;

use crate::domain::StackError;
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
}
