//! Acceptance test for `stack list`.

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

#[test]
fn list_reports_no_stacks_for_empty_store() {
    let (_t, repo, store) = fresh_repo();
    let out = stack_cmd(&repo, &store).args(["list"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no stacks tracked"), "got: {stdout}");
}

#[test]
fn list_reports_worktree_when_one_exists() {
    let (tmp, repo, store) = fresh_repo();
    let wt = tmp.path().join("wt");
    stack_cmd(&repo, &store)
        .args(["init", "-w", wt.to_str().unwrap(), "-p", "feat", "a", "b"])
        .assert()
        .success();

    let canonical_wt = std::fs::canonicalize(&wt).unwrap();
    // Text form: path appears (canonicalised by git's worktree list).
    let out = stack_cmd(&repo, &store).args(["list"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(canonical_wt.to_str().unwrap()),
        "expected worktree path in list output, got:\n{stdout}"
    );

    // JSON form: worktree field present and absolute.
    let out = stack_cmd(&repo, &store)
        .args(["list", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let s = &v["stacks"][0];
    assert_eq!(
        s["worktree"].as_str().unwrap(),
        canonical_wt.to_str().unwrap()
    );
}

#[test]
fn list_flags_worktree_as_missing_when_directory_deleted() {
    let (tmp, repo, store) = fresh_repo();
    let wt = tmp.path().join("wt-doomed");
    stack_cmd(&repo, &store)
        .args(["init", "-w", wt.to_str().unwrap(), "-p", "feat", "a"])
        .assert()
        .success();

    // Simulate the worktree directory being deleted out from under git.
    // git's bookkeeping in .git/worktrees/<name>/ still points at the path.
    std::fs::remove_dir_all(&wt).unwrap();

    // Text form: still shows the path, plus a (missing) flag.
    let out = stack_cmd(&repo, &store).args(["list"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("(missing)"),
        "expected '(missing)' marker, got:\n{stdout}"
    );

    // JSON: worktreeMissing field is true.
    let out = stack_cmd(&repo, &store)
        .args(["list", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["stacks"][0]["worktreeMissing"], true);
}

#[test]
fn list_falls_back_to_main_checkout_when_no_explicit_worktree() {
    let (_t, repo, store) = fresh_repo();
    stack_cmd(&repo, &store)
        .args(["init", "--no-worktree", "-p", "feat", "a"])
        .assert()
        .success();
    // With --no-worktree, the branch was created in the main checkout —
    // which still counts as a worktree from `git worktree list`'s
    // perspective. The right behaviour is to surface that path.
    let out = stack_cmd(&repo, &store).args(["list"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let canonical_repo = std::fs::canonicalize(&repo).unwrap();
    assert!(
        stdout.contains(canonical_repo.to_str().unwrap()),
        "expected main checkout path in list output, got:\n{stdout}"
    );
}

#[test]
fn list_shows_every_stack_and_marks_current() {
    let (_t, repo, store) = fresh_repo();
    stack_cmd(&repo, &store)
        .args(["init", "--no-worktree", "-p", "s1", "a", "b"])
        .assert()
        .success();
    git(&repo, &["checkout", "-q", "main"]);
    stack_cmd(&repo, &store)
        .args(["init", "--no-worktree", "-p", "s2", "x"])
        .assert()
        .success();

    // Currently on s2/x.
    let out = stack_cmd(&repo, &store)
        .args(["list", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    assert_eq!(v["repository"], "github.com:flashingpumpkin/demo");
    let stacks = v["stacks"].as_array().unwrap();
    assert_eq!(stacks.len(), 2);

    // Find each stack by prefix.
    let s1 = stacks.iter().find(|s| s["prefix"] == "s1").unwrap();
    let s2 = stacks.iter().find(|s| s["prefix"] == "s2").unwrap();
    assert_eq!(s1["branchCount"], 2);
    assert_eq!(s1["isCurrent"], false);
    assert_eq!(s2["branchCount"], 1);
    assert_eq!(s2["isCurrent"], true);
    assert_eq!(s2["branches"][0]["name"], "s2/x");
    assert_eq!(s2["branches"][0]["isCurrent"], true);
    assert_eq!(s2["branches"][0]["isMerged"], false);
}
