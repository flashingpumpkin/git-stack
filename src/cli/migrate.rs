use std::path::PathBuf;

use crate::domain::{StackError, StackFile};
use crate::git::Git;
use crate::store::{legacy_gh_stack_path, mark_legacy_migrated, open, RepoIdentity};

pub fn run(repo: Option<PathBuf>, force: bool) -> Result<(), StackError> {
    let start = repo.unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let git = Git::discover(&start)?;
    let identity = RepoIdentity::from_git(&git)?;

    let legacy = legacy_gh_stack_path(&git)?.ok_or_else(|| {
        StackError::Other(format!(
            "no .git/gh-stack file found in {}",
            git.cwd().display()
        ))
    })?;

    let bytes = std::fs::read(&legacy)?;
    let mut parsed: StackFile = serde_json::from_slice(&bytes)
        .map_err(|e| StackError::Other(format!("failed to parse {}: {e}", legacy.display())))?;

    // Rewrite the repository field to our canonical identity so future
    // reads from other worktrees match.
    parsed.repository = identity.repository.clone();

    let guard = open(&identity)?;
    let existing = guard.load(&identity)?;
    if !existing.stacks.is_empty() && !force {
        return Err(StackError::Other(format!(
            "central store already has {} stack(s) for `{}`; pass --force to overwrite",
            existing.stacks.len(),
            identity.repository
        )));
    }

    guard.save(&parsed)?;
    mark_legacy_migrated(&legacy)?;

    eprintln!(
        "✓ migrated {} stack(s) for {} → {}",
        parsed.stacks.len(),
        identity.repository,
        guard.paths().stacks_json().display()
    );
    eprintln!(
        "ℹ legacy file renamed to {}",
        legacy.with_extension("migrated").display()
    );
    Ok(())
}
