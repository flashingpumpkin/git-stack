//! Acceptance tests for `stack prune`.

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
    c.current_dir(repo)
        .env("GIT_STACK_HOME", store)
        .env("NO_COLOR", "1");
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

/// Pre-mark every branch in the named stack as merged in the stored
/// stacks.json so `prune` can decide without a real GitHub.
fn mark_stack_merged(store: &Path, prefix: &str) {
    let path = store.join("repos/github.com_flashingpumpkin_demo/stacks.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for stack in v["stacks"].as_array_mut().unwrap() {
        if stack["prefix"] == prefix {
            for b in stack["branches"].as_array_mut().unwrap() {
                b["pullRequest"] = serde_json::json!({
                    "number": 1, "id": "x", "url": "u", "merged": true
                });
            }
        }
    }
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
}

#[test]
fn prune_drops_fully_merged_stack_and_keeps_active_one() {
    let (_t, repo, store) = fresh_repo();
    stack_cmd(&repo, &store)
        .args(["init", "--no-worktree", "-p", "done", "a", "b"])
        .assert()
        .success();
    git(&repo, &["checkout", "-q", "main"]);
    stack_cmd(&repo, &store)
        .args(["init", "--no-worktree", "-p", "live", "x"])
        .assert()
        .success();
    mark_stack_merged(&store, "done");

    let out = stack_cmd(&repo, &store)
        .args(["prune", "--no-refresh"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("dropped stack done"),
        "expected 'dropped stack done', got:\n{stderr}"
    );

    // Only the active stack remains.
    let stacks_path = store.join("repos/github.com_flashingpumpkin_demo/stacks.json");
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&stacks_path).unwrap()).unwrap();
    let stacks = v["stacks"].as_array().unwrap();
    assert_eq!(stacks.len(), 1);
    assert_eq!(stacks[0]["prefix"], "live");
}

#[test]
fn prune_dry_run_does_not_modify_store() {
    let (_t, repo, store) = fresh_repo();
    stack_cmd(&repo, &store)
        .args(["init", "--no-worktree", "-p", "done", "a"])
        .assert()
        .success();
    mark_stack_merged(&store, "done");

    let stacks_path = store.join("repos/github.com_flashingpumpkin_demo/stacks.json");
    let before = std::fs::read_to_string(&stacks_path).unwrap();

    let out = stack_cmd(&repo, &store)
        .args(["prune", "--dry-run", "--no-refresh"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("would drop stack done"),
        "expected 'would drop stack done', got:\n{stderr}"
    );

    let after = std::fs::read_to_string(&stacks_path).unwrap();
    assert_eq!(before, after, "dry-run should not modify stacks.json");
}

#[test]
fn prune_reports_nothing_when_all_clean() {
    let (_t, repo, store) = fresh_repo();
    stack_cmd(&repo, &store)
        .args(["init", "--no-worktree", "-p", "live", "x"])
        .assert()
        .success();

    let out = stack_cmd(&repo, &store)
        .args(["prune", "--no-refresh"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no fully-merged stacks to prune"));
}
