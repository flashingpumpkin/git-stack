//! Acceptance tests for `stack remove`.

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

fn stack_cmd(repo: &Path, store: &Path) -> Command {
    let mut c = Command::cargo_bin("stack").unwrap();
    c.current_dir(repo).env("GIT_STACK_HOME", store);
    c
}

fn fresh_repo() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let store = tmp.path().join("store");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.com"]);
    git(&repo, &["config", "user.name", "T"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:flashingpumpkin/demo.git",
        ],
    );
    std::fs::write(repo.join("README.md"), "x").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    (tmp, repo, store)
}

#[test]
fn remove_drops_stack_and_clears_worktree_when_clean() {
    let (tmp, repo, store) = fresh_repo();
    let wt = tmp.path().join("wt");

    stack_cmd(&repo, &store)
        .args(["init", "-w", wt.to_str().unwrap(), "-p", "feat", "a"])
        .assert()
        .success();
    assert!(wt.exists());

    stack_cmd(&repo, &store)
        .args(["remove", "feat/a"])
        .assert()
        .success();

    assert!(!wt.exists(), "worktree should be gone after remove");

    // Stack file no longer tracks the stack.
    let stacks_json = store.join("repos/github.com_flashingpumpkin_demo/stacks.json");
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&stacks_json).unwrap()).unwrap();
    assert_eq!(json["stacks"].as_array().unwrap().len(), 0);

    // Git branch is still there — remove leaves branches alone.
    let branches = Command::new("git")
        .current_dir(&repo)
        .args(["branch", "--list", "feat/a"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&branches.stdout).contains("feat/a"));
}

#[test]
fn remove_refuses_when_worktree_dirty() {
    let (tmp, repo, store) = fresh_repo();
    let wt = tmp.path().join("wt");

    stack_cmd(&repo, &store)
        .args(["init", "-w", wt.to_str().unwrap(), "-p", "feat", "a"])
        .assert()
        .success();

    // Make the worktree dirty.
    std::fs::write(wt.join("dirty.txt"), "uncommitted").unwrap();

    stack_cmd(&repo, &store)
        .args(["remove", "feat/a"])
        .assert()
        .failure();

    // Worktree and stack still present.
    assert!(wt.exists(), "worktree must survive refusal");
    let stacks_json = store.join("repos/github.com_flashingpumpkin_demo/stacks.json");
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&stacks_json).unwrap()).unwrap();
    assert_eq!(json["stacks"].as_array().unwrap().len(), 1);
}

#[test]
fn remove_force_drops_dirty_worktree() {
    let (tmp, repo, store) = fresh_repo();
    let wt = tmp.path().join("wt");

    stack_cmd(&repo, &store)
        .args(["init", "-w", wt.to_str().unwrap(), "-p", "feat", "a"])
        .assert()
        .success();
    std::fs::write(wt.join("dirty.txt"), "uncommitted").unwrap();

    stack_cmd(&repo, &store)
        .args(["remove", "feat/a", "--force"])
        .assert()
        .success();

    assert!(!wt.exists());
}
