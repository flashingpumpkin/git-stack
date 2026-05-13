//! Acceptance test for `stack init` creating a worktree.

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
fn init_creates_worktree_at_explicit_path_and_checks_out_bottom_branch() {
    let (tmp, repo, store) = fresh_repo();
    let wt = tmp.path().join("custom-wt");

    let out = stack_cmd(&repo, &store)
        .args([
            "init",
            "-w",
            wt.to_str().unwrap(),
            "-p",
            "feat",
            "a",
            "b",
            "c",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Worktree exists and is on the bottom branch.
    assert!(
        wt.join(".git").exists(),
        "worktree should exist at {}",
        wt.display()
    );
    assert_eq!(
        git_out(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feat/a"
    );

    // Worktree path printed on stdout (last line).
    let stdout = String::from_utf8_lossy(&out.stdout);
    let last = stdout.trim().lines().last().unwrap();
    assert_eq!(Path::new(last), wt.as_path());

    // The main checkout is still on main — init didn't disturb it.
    assert_eq!(
        git_out(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main"
    );
}

#[test]
fn init_refuses_when_worktree_path_exists() {
    let (tmp, repo, store) = fresh_repo();
    let wt = tmp.path().join("taken");
    std::fs::create_dir_all(&wt).unwrap();

    stack_cmd(&repo, &store)
        .args(["init", "-w", wt.to_str().unwrap(), "-p", "feat", "a"])
        .assert()
        .failure();
}
