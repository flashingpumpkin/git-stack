//! In-memory `GitHub` fake for tests. Records calls and lets tests
//! pre-populate PRs returned by `view_pr`.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::domain::StackError;

use super::{GitHub, PrState, PullRequestInfo};

#[derive(Default)]
pub struct FakeGitHub {
    inner: RefCell<Inner>,
}

#[derive(Default)]
struct Inner {
    next_number: u64,
    prs: HashMap<u64, PullRequestInfo>,
    create_calls: Vec<CreateCall>,
}

#[derive(Debug, Clone)]
pub struct CreateCall {
    pub head: String,
    pub base: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

impl FakeGitHub {
    pub fn new() -> Self {
        Self {
            inner: RefCell::new(Inner {
                next_number: 100,
                ..Default::default()
            }),
        }
    }

    pub fn seed_pr(&self, pr: PullRequestInfo) {
        self.inner.borrow_mut().prs.insert(pr.number, pr);
    }

    pub fn create_calls(&self) -> Vec<CreateCall> {
        self.inner.borrow().create_calls.clone()
    }
}

impl GitHub for FakeGitHub {
    fn create_pr(
        &self,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
        draft: bool,
    ) -> Result<PullRequestInfo, StackError> {
        let mut inner = self.inner.borrow_mut();
        inner.create_calls.push(CreateCall {
            head: head.into(),
            base: base.into(),
            title: title.into(),
            body: body.into(),
            draft,
        });
        let n = inner.next_number;
        inner.next_number += 1;
        let pr = PullRequestInfo {
            number: n,
            id: format!("PR_fake_{n}"),
            url: format!("https://github.com/fake/repo/pull/{n}"),
            state: PrState::Open,
            head_ref: head.into(),
            base_ref: base.into(),
        };
        inner.prs.insert(n, pr.clone());
        Ok(pr)
    }

    fn view_pr(&self, number: u64) -> Result<PullRequestInfo, StackError> {
        self.inner
            .borrow()
            .prs
            .get(&number)
            .cloned()
            .ok_or_else(|| StackError::GitHubApi(format!("PR #{number} not found")))
    }

    fn find_pr_by_head(&self, branch: &str) -> Result<Option<PullRequestInfo>, StackError> {
        let rank = |s: PrState| -> u8 {
            match s {
                PrState::Open => 0,
                PrState::Merged => 1,
                PrState::Closed => 2,
            }
        };
        let inner = self.inner.borrow();
        let mut candidates: Vec<&PullRequestInfo> = inner
            .prs
            .values()
            .filter(|p| p.head_ref == branch)
            .collect();
        candidates.sort_by(|a, b| {
            rank(a.state)
                .cmp(&rank(b.state))
                .then(b.number.cmp(&a.number))
        });
        Ok(candidates.first().map(|p| (*p).clone()))
    }
}
