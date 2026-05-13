//! Acceptance tests for step 3: init, add, remove.

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
    assert!(o.status.success());
    String::from_utf8(o.stdout).unwrap().trim().to_string()
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

fn stack_cmd(repo: &Path, store: &Path) -> Command {
    let mut c = Command::cargo_bin("stack").unwrap();
    c.current_dir(repo).env("GIT_STACK_HOME", store);
    c
}

#[test]
fn init_creates_branches_and_checks_out_bottom() {
    let (_t, repo, store) = fresh_repo();
    stack_cmd(&repo, &store)
        .args(["init", "--no-worktree", "-p", "feat", "auth", "api", "ui"])
        .assert()
        .success();

    for b in ["feat/auth", "feat/api", "feat/ui"] {
        let out = Command::new("git")
            .current_dir(&repo)
            .args(["show-ref", "--verify", &format!("refs/heads/{b}")])
            .status()
            .unwrap();
        assert!(out.success(), "{b} should exist");
    }
    assert_eq!(
        git_out(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feat/auth"
    );

    let json: serde_json::Value = serde_json::from_slice(
        &std::fs::read(store.join("repos/github.com_flashingpumpkin_demo/stacks.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(json["stacks"][0]["prefix"], "feat");
    assert_eq!(json["stacks"][0]["branches"].as_array().unwrap().len(), 3);
}

#[test]
fn add_appends_branch_using_prefix() {
    let (_t, repo, store) = fresh_repo();
    stack_cmd(&repo, &store)
        .args(["init", "--no-worktree", "-p", "feat", "auth"])
        .assert()
        .success();

    stack_cmd(&repo, &store)
        .args(["add", "api"])
        .assert()
        .success();
    assert_eq!(
        git_out(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feat/api"
    );

    // Add with -Am from a dirty tree.
    std::fs::write(repo.join("api.rs"), "fn api(){}").unwrap();
    stack_cmd(&repo, &store)
        .args(["add", "-A", "-m", "ui scaffold", "ui"])
        .assert()
        .success();
    assert_eq!(
        git_out(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feat/ui"
    );
    // The commit landed on feat/api (the prior top), not on feat/ui.
    let log = git_out(&repo, &["log", "feat/api", "--oneline", "-1"]);
    assert!(
        log.contains("ui scaffold"),
        "expected commit on feat/api, got: {log}"
    );
}

#[test]
fn add_rejected_when_not_on_top() {
    let (_t, repo, store) = fresh_repo();
    stack_cmd(&repo, &store)
        .args(["init", "--no-worktree", "-p", "feat", "a", "b", "c"])
        .assert()
        .success();
    // Move to middle of stack.
    git(&repo, &["checkout", "-q", "feat/b"]);
    stack_cmd(&repo, &store)
        .args(["add", "d"])
        .assert()
        .failure();
}

#[test]
fn remove_removes_only_the_matching_stack() {
    let (_t, repo, store) = fresh_repo();
    stack_cmd(&repo, &store)
        .args(["init", "--no-worktree", "-p", "s1", "a"])
        .assert()
        .success();
    git(&repo, &["checkout", "-q", "main"]);
    stack_cmd(&repo, &store)
        .args(["init", "--no-worktree", "-p", "s2", "a"])
        .assert()
        .success();

    // Now we have two stacks. `remove s2/a` should drop only s2.
    git(&repo, &["checkout", "-q", "main"]);
    stack_cmd(&repo, &store)
        .args(["remove", "s2/a"])
        .assert()
        .success();

    let json: serde_json::Value = serde_json::from_slice(
        &std::fs::read(store.join("repos/github.com_flashingpumpkin_demo/stacks.json")).unwrap(),
    )
    .unwrap();
    let stacks = json["stacks"].as_array().unwrap();
    assert_eq!(stacks.len(), 1);
    assert_eq!(stacks[0]["prefix"], "s1");
}
