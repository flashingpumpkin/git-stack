use std::process::ExitCode as StdExitCode;
use thiserror::Error;

/// Exit codes mirror `gh stack`'s conventions so scripts written against
/// `gh stack` continue to work against `stack`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    Ok = 0,
    Generic = 1,
    NotInStack = 2,
    RebaseConflict = 3,
    GitHubApi = 4,
    InvalidArgs = 5,
    Disambiguation = 6,
    RebaseInProgress = 7,
    Locked = 8,
}

impl From<ExitCode> for StdExitCode {
    fn from(c: ExitCode) -> Self {
        StdExitCode::from(c as u8)
    }
}

#[derive(Debug, Error)]
pub enum StackError {
    #[error("not in a stack")]
    NotInStack,

    #[error("branch `{0}` belongs to multiple stacks; check out a non-shared branch first")]
    Disambiguation(String),

    #[error("invalid argument: {0}")]
    InvalidArgs(String),

    #[error("store is locked by another `stack` process; try again")]
    Locked,

    #[error("rebase in progress; run `stack rebase --continue` or `stack rebase --abort`")]
    RebaseInProgress,

    #[error("rebase conflict; resolve and run `stack rebase --continue`")]
    RebaseConflict,

    #[error("github api error: {0}")]
    GitHubApi(String),

    #[error("{0}")]
    Other(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl StackError {
    pub fn exit_code(&self) -> ExitCode {
        match self {
            StackError::NotInStack => ExitCode::NotInStack,
            StackError::Disambiguation(_) => ExitCode::Disambiguation,
            StackError::InvalidArgs(_) => ExitCode::InvalidArgs,
            StackError::Locked => ExitCode::Locked,
            StackError::RebaseInProgress => ExitCode::RebaseInProgress,
            StackError::RebaseConflict => ExitCode::RebaseConflict,
            StackError::GitHubApi(_) => ExitCode::GitHubApi,
            _ => ExitCode::Generic,
        }
    }
}
