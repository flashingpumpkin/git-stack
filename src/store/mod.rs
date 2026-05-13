use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fd_lock::RwLock;
use sha2::{Digest, Sha256};

use crate::domain::{PoolFile, StackError, StackFile};
use crate::git::{canonicalise_github_url, Git};

/// Root directory for all stack state on this machine.
/// Honours `GIT_STACK_HOME`, otherwise `$XDG_DATA_HOME/git-stack`, otherwise
/// `~/.local/share/git-stack`.
pub fn home() -> PathBuf {
    if let Ok(p) = std::env::var("GIT_STACK_HOME") {
        return PathBuf::from(p);
    }
    if let Some(d) = dirs::data_dir() {
        return d.join("git-stack");
    }
    PathBuf::from(".git-stack")
}

/// Identity of a git repository, used to key the central state file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoIdentity {
    /// The canonical name we store in the `repository` field of `stacks.json`.
    pub repository: String,
    /// Directory-safe slug used as the path under `home()/repos/`.
    pub slug: String,
}

impl RepoIdentity {
    /// Resolve identity from a git worktree.
    /// 1. If `origin` is a GitHub URL → `github.com:owner/repo`.
    /// 2. Otherwise → `local:<sha256(common_dir)>`.
    pub fn from_git(git: &Git) -> Result<Self, StackError> {
        if let Some(url) = git.origin_url()? {
            if let Some(canon) = canonicalise_github_url(&url) {
                let slug = canon.replace(['/', ':'], "_");
                return Ok(Self {
                    repository: canon,
                    slug,
                });
            }
        }
        let common = git.common_dir()?;
        let mut h = Sha256::new();
        h.update(common.to_string_lossy().as_bytes());
        let digest = hex::encode(h.finalize());
        let short = &digest[..16];
        Ok(Self {
            repository: format!("local:{short}"),
            slug: format!("local_{short}"),
        })
    }
}

/// Filesystem layout for one repository's state.
pub struct StorePaths {
    pub dir: PathBuf,
}

impl StorePaths {
    pub fn for_identity(identity: &RepoIdentity) -> Self {
        Self {
            dir: home().join("repos").join(&identity.slug),
        }
    }

    pub fn stacks_json(&self) -> PathBuf {
        self.dir.join("stacks.json")
    }

    pub fn pools_json(&self) -> PathBuf {
        self.dir.join("pools.json")
    }

    pub fn lock_file(&self) -> PathBuf {
        self.dir.join("stacks.json.lock")
    }

    pub fn ensure_dir(&self) -> Result<(), StackError> {
        fs::create_dir_all(&self.dir)?;
        Ok(())
    }
}

/// Holds an exclusive file lock for the lifetime of the value.
pub struct StoreGuard {
    _lock_handle: File,
    paths: StorePaths,
}

/// Open the store for a given identity, acquiring an exclusive advisory lock.
/// Times out after 5 seconds and returns `StackError::Locked`.
pub fn open(identity: &RepoIdentity) -> Result<StoreGuard, StackError> {
    let paths = StorePaths::for_identity(identity);
    paths.ensure_dir()?;

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(paths.lock_file())?;

    // Try to acquire with a deadline.
    let mut rwlock = RwLock::new(file);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match rwlock.try_write() {
            Ok(guard) => {
                // We have the lock; forget the guard but keep the underlying File.
                // fd-lock releases on drop of the guard, so we leak it intentionally
                // by forgetting and re-opening the file handle for storage.
                std::mem::forget(guard);
                let handle = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(paths.lock_file())?;
                return Ok(StoreGuard {
                    _lock_handle: handle,
                    paths,
                });
            }
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(_) => return Err(StackError::Locked),
        }
    }
}

impl StoreGuard {
    pub fn paths(&self) -> &StorePaths {
        &self.paths
    }

    /// Read the current `stacks.json`. Returns an empty file with the given
    /// repository identity if it doesn't exist yet.
    pub fn load(&self, identity: &RepoIdentity) -> Result<StackFile, StackError> {
        let path = self.paths.stacks_json();
        match read_json::<StackFile>(&path)? {
            Some(file) => Ok(file),
            None => Ok(StackFile::empty(&identity.repository)),
        }
    }

    /// Atomically replace `stacks.json` with `file`. Write to a tmp sibling,
    /// fsync, then rename.
    pub fn save(&self, file: &StackFile) -> Result<(), StackError> {
        write_json_atomic(&self.paths.stacks_json(), file)
    }

    /// Read the current `pools.json`. Returns an empty file with the
    /// given repository identity if it doesn't exist yet. Pools live in
    /// their own file so stacks and pools can evolve independently.
    pub fn load_pools(&self, identity: &RepoIdentity) -> Result<PoolFile, StackError> {
        let path = self.paths.pools_json();
        match read_json::<PoolFile>(&path)? {
            Some(file) => Ok(file),
            None => Ok(PoolFile::empty(&identity.repository)),
        }
    }

    /// Atomically replace `pools.json` with `file`.
    pub fn save_pools(&self, file: &PoolFile) -> Result<(), StackError> {
        write_json_atomic(&self.paths.pools_json(), file)
    }
}

fn read_json<T: for<'de> serde::Deserialize<'de>>(path: &Path) -> Result<Option<T>, StackError> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let parsed: T = serde_json::from_slice(&bytes)?;
    Ok(Some(parsed))
}

fn write_json_atomic<T: serde::Serialize>(target: &Path, value: &T) -> Result<(), StackError> {
    let tmp = target.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(value)?;
    {
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)?;
        f.write_all(&bytes)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    fs::rename(&tmp, target)?;
    Ok(())
}

/// Locate `.git/gh-stack` for a given repo, if it exists.
pub fn legacy_gh_stack_path(git: &Git) -> Result<Option<PathBuf>, StackError> {
    let common = git.common_dir()?;
    let candidate = common.join("gh-stack");
    if candidate.exists() {
        Ok(Some(candidate))
    } else {
        Ok(None)
    }
}

/// Mark a legacy file as imported by renaming it to `gh-stack.migrated`.
pub fn mark_legacy_migrated(legacy_path: &Path) -> Result<(), StackError> {
    let target = legacy_path.with_extension("migrated");
    fs::rename(legacy_path, target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_honours_env_override() {
        std::env::set_var("GIT_STACK_HOME", "/tmp/git-stack-test-home");
        assert_eq!(home(), PathBuf::from("/tmp/git-stack-test-home"));
        std::env::remove_var("GIT_STACK_HOME");
    }
}
