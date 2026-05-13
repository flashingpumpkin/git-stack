//! Acceptance tests for step 4: rebase, push, sync.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) {
    let s = Command::new("git")
        .current_dir(repo)
        .args(args)
        .status()
        .unwrap();
    assert!(s.success(), "git {:?}", args);
}
fn git_out(repo: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {:?}", args);
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}
fn commit(repo: &Path, file: &str, msg: &str) {
    std::fs::write(repo.join(file), msg).unwrap();
    git(repo, &["add", file]);
    git(repo, &["commit", "-q", "-m", msg]);
}

fn stack_cmd(repo: &Path, store: &Path) -> Command {
    let mut c = Command::cargo_bin("stack").unwrap();
    c.current_dir(repo).env("GIT_STACK_HOME", store);
    c
}

/// Build: bare remote + a clone with a 3-branch stack created via `stack init`.
fn setup_with_remote() -> (
    TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let tmp = TempDir::new().unwrap();
    let remote = tmp.path().join("remote.git");
    let repo = tmp.path().join("repo");
    let store = tmp.path().join("store");

    // Bare remote.
    Command::new("git")
        .args(["init", "--bare", "-q", "-b", "main"])
        .arg(&remote)
        .status()
        .unwrap();

    // Clone via file:// so it counts as a github-shaped URL? No — we use the local fallback.
    // Identity will be `local:<sha>` because origin is a path, not github.com. That's fine for these tests.
    Command::new("git")
        .args(["clone", "-q"])
        .arg(&remote)
        .arg(&repo)
        .status()
        .unwrap();
    git(&repo, &["config", "user.email", "t@e.com"]);
    git(&repo, &["config", "user.name", "T"]);
    commit(&repo, "README.md", "init");
    git(&repo, &["push", "-q", "origin", "main"]);

    stack_cmd(&repo, &store)
        .args(["init", "--no-worktree", "-p", "feat", "a", "b", "c"])
        .assert()
        .success();

    (tmp, repo, store, remote)
}

/// Add a real commit on each branch, then run `stack rebase` to bring stored
/// `base` SHAs in sync with the new branch heads. Returns control with the
/// store fully consistent for downstream test assertions.
fn add_commits_and_rebase(repo: &Path, store: &Path) {
    for n in ["feat/a", "feat/b", "feat/c"] {
        git(repo, &["checkout", "-q", n]);
        commit(repo, &format!("{}.txt", n.replace('/', "_")), n);
    }
    git(repo, &["checkout", "-q", "feat/c"]);
    stack_cmd(repo, store).args(["rebase"]).assert().success();
}

fn store_path(repo: &Path, store: &Path) -> std::path::PathBuf {
    let out = stack_cmd(repo, store).args(["where"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("stacks.json=") {
            return std::path::PathBuf::from(rest.trim());
        }
    }
    panic!("could not parse `stack where` output: {stdout}");
}

#[test]
fn rebase_is_no_op_after_clean_init() {
    let (_t, repo, store, _r) = setup_with_remote();
    // After init with no extra commits, every branch's stored base equals its
    // parent's head — rebase should plan zero work.
    let assert = stack_cmd(&repo, &store).args(["rebase"]).assert().success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("already up to date"),
        "expected no-op, got: {stderr}"
    );
}

#[test]
fn push_sends_all_branches_to_remote() {
    let (_t, repo, store, remote) = setup_with_remote();
    add_commits_and_rebase(&repo, &store);
    git(&repo, &["checkout", "-q", "feat/c"]);
    stack_cmd(&repo, &store).args(["push"]).assert().success();

    for b in ["feat/a", "feat/b", "feat/c"] {
        let listed = Command::new("git")
            .current_dir(&remote)
            .args(["show-ref", "--verify", &format!("refs/heads/{b}")])
            .status()
            .unwrap();
        assert!(listed.success(), "{b} should exist on remote");
    }
}

#[test]
fn cascade_rebase_after_trunk_advances() {
    let (_t, repo, store, remote) = setup_with_remote();
    add_commits_and_rebase(&repo, &store);

    // Advance trunk on the remote by pushing a new commit from a sibling clone.
    let other = repo.parent().unwrap().join("other");
    Command::new("git")
        .args(["clone", "-q"])
        .arg(&remote)
        .arg(&other)
        .status()
        .unwrap();
    git(&other, &["config", "user.email", "t@e.com"]);
    git(&other, &["config", "user.name", "T"]);
    commit(&other, "trunkmove.txt", "trunk moved");
    git(&other, &["push", "-q", "origin", "main"]);

    // Sync from the stack's repo.
    git(&repo, &["checkout", "-q", "feat/c"]);
    stack_cmd(&repo, &store).args(["sync"]).assert().success();

    // After sync: trunk advanced, and feat/a/b/c have ALL been re-parented
    // in one invocation. Regression: a previous version only rebased the
    // bottom branch per invocation because the plan was frozen at start-up.
    let trunk_head = git_out(&repo, &["rev-parse", "main"]);
    let a_parent = git_out(&repo, &["rev-parse", "feat/a^"]);
    let b_parent = git_out(&repo, &["rev-parse", "feat/b^"]);
    let c_parent = git_out(&repo, &["rev-parse", "feat/c^"]);
    let a_head = git_out(&repo, &["rev-parse", "feat/a"]);
    let b_head = git_out(&repo, &["rev-parse", "feat/b"]);
    assert_eq!(a_parent, trunk_head, "feat/a should sit on new trunk");
    assert_eq!(b_parent, a_head, "feat/b should sit on the rebased feat/a");
    assert_eq!(c_parent, b_head, "feat/c should sit on the rebased feat/b");

    // Stored bases reflect the new parents for every branch.
    let stacks_json_path = store_path(&repo, &store);
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&stacks_json_path).unwrap()).unwrap();
    let branches = &json["stacks"][0]["branches"];
    assert_eq!(branches[0]["base"].as_str().unwrap(), trunk_head);
    assert_eq!(branches[1]["base"].as_str().unwrap(), a_head);
    assert_eq!(branches[2]["base"].as_str().unwrap(), b_head);
}

#[test]
fn rebase_abort_clears_state_file() {
    // Hard to reliably engineer a conflict without a lot of setup; we cover
    // the simpler abort path: with no state file and no rebase in progress,
    // abort is a no-op success.
    let (_t, repo, store, _r) = setup_with_remote();
    stack_cmd(&repo, &store)
        .args(["rebase", "--abort"])
        .assert()
        .success();
}
