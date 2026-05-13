//! In-memory `GitOps` fake for tests. Records pushes and lets tests
//! pre-populate the result of `commits_between`.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::domain::StackError;

use super::{CommitSummary, GitOps};

#[derive(Default)]
pub struct FakeGit {
    inner: RefCell<Inner>,
}

#[derive(Default)]
struct Inner {
    current_branch: Option<String>,
    pushes: Vec<PushCall>,
    commits: HashMap<(String, String), Vec<CommitSummary>>,
    revs: HashMap<String, String>,
    ancestors: HashMap<(String, String), bool>,
}

#[derive(Debug, Clone)]
pub struct PushCall {
    pub remote: String,
    pub branches: Vec<String>,
}

impl FakeGit {
    pub fn new(current_branch: &str) -> Self {
        Self {
            inner: RefCell::new(Inner {
                current_branch: Some(current_branch.into()),
                ..Default::default()
            }),
        }
    }

    pub fn seed_commits(&self, base: &str, branch: &str, commits: Vec<CommitSummary>) {
        self.inner
            .borrow_mut()
            .commits
            .insert((base.into(), branch.into()), commits);
    }

    pub fn pushes(&self) -> Vec<PushCall> {
        self.inner.borrow().pushes.clone()
    }

    pub fn seed_rev(&self, rev: &str, sha: &str) {
        self.inner.borrow_mut().revs.insert(rev.into(), sha.into());
    }

    pub fn seed_ancestor(&self, ancestor: &str, descendant: &str, is_ancestor: bool) {
        self.inner
            .borrow_mut()
            .ancestors
            .insert((ancestor.into(), descendant.into()), is_ancestor);
    }
}

impl GitOps for FakeGit {
    fn current_branch(&self) -> Result<Option<String>, StackError> {
        Ok(self.inner.borrow().current_branch.clone())
    }

    fn rev_parse(&self, rev: &str) -> Result<String, StackError> {
        self.inner
            .borrow()
            .revs
            .get(rev)
            .cloned()
            .ok_or_else(|| StackError::Other(format!("FakeGit: no seeded rev for {rev}")))
    }

    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool, StackError> {
        Ok(self
            .inner
            .borrow()
            .ancestors
            .get(&(ancestor.into(), descendant.into()))
            .copied()
            .unwrap_or(true))
    }

    fn push_atomic(&self, remote: &str, branches: &[&str]) -> Result<(), StackError> {
        self.inner.borrow_mut().pushes.push(PushCall {
            remote: remote.into(),
            branches: branches.iter().map(|s| s.to_string()).collect(),
        });
        Ok(())
    }

    fn commits_between(&self, base: &str, branch: &str) -> Result<Vec<CommitSummary>, StackError> {
        Ok(self
            .inner
            .borrow()
            .commits
            .get(&(base.into(), branch.into()))
            .cloned()
            .unwrap_or_default())
    }
}
