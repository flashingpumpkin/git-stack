//! Acceptance tests for `stack drop`.

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

/// Build a repo with a stack of three branches `feat/a → feat/b → feat/c`,
/// each carrying one real commit, and bring stored bases in sync.
fn setup() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
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
    commit(&repo, "README.md", "init");

    stack_cmd(&repo, &store)
        .args(["init", "--no-worktree", "-p", "feat", "a", "b", "c"])
        .assert()
        .success();

    for n in ["feat/a", "feat/b", "feat/c"] {
        git(&repo, &["checkout", "-q", n]);
        commit(&repo, &format!("{}.txt", n.replace('/', "_")), n);
    }
    git(&repo, &["checkout", "-q", "feat/c"]);
    stack_cmd(&repo, &store).args(["rebase"]).assert().success();

    (tmp, repo, store)
}

fn stacks_json(store: &Path) -> serde_json::Value {
    let p = store.join("repos/github.com_flashingpumpkin_demo/stacks.json");
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

#[test]
fn drop_top_branch_removes_only_that_branch() {
    let (_t, repo, store) = setup();
    stack_cmd(&repo, &store)
        .args(["drop", "feat/c"])
        .assert()
        .success();

    let json = stacks_json(&store);
    let names: Vec<&str> = json["stacks"][0]["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["branch"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["feat/a", "feat/b"]);

    // The git branch still exists — drop is metadata-only.
    let exists = Command::new("git")
        .current_dir(&repo)
        .args(["show-ref", "--verify", "--quiet", "refs/heads/feat/c"])
        .status()
        .unwrap();
    assert!(exists.success(), "feat/c branch should still exist locally");
}

#[test]
fn drop_middle_branch_reparents_children_onto_grandparent() {
    let (_t, repo, store) = setup();
    let a_head = git_out(&repo, &["rev-parse", "feat/a"]);

    git(&repo, &["checkout", "-q", "feat/c"]);
    stack_cmd(&repo, &store)
        .args(["drop", "feat/b"])
        .assert()
        .success();

    // feat/c should now be rebased onto feat/a.
    let c_parent = git_out(&repo, &["rev-parse", "feat/c^"]);
    assert_eq!(c_parent, a_head, "feat/c should be re-parented onto feat/a");

    let json = stacks_json(&store);
    let branches = json["stacks"][0]["branches"].as_array().unwrap();
    assert_eq!(branches.len(), 2);
    assert_eq!(branches[0]["branch"], "feat/a");
    assert_eq!(branches[1]["branch"], "feat/c");
    assert_eq!(branches[1]["base"], a_head);
}

#[test]
fn drop_bottom_branch_reparents_children_onto_trunk() {
    let (_t, repo, store) = setup();
    let trunk_head = git_out(&repo, &["rev-parse", "main"]);

    stack_cmd(&repo, &store)
        .args(["drop", "feat/a"])
        .assert()
        .success();

    // feat/b should now sit on main.
    let b_parent = git_out(&repo, &["rev-parse", "feat/b^"]);
    assert_eq!(b_parent, trunk_head);

    let json = stacks_json(&store);
    let branches = json["stacks"][0]["branches"].as_array().unwrap();
    assert_eq!(branches.len(), 2);
    assert_eq!(branches[0]["branch"], "feat/b");
    assert_eq!(branches[0]["base"], trunk_head);
}

#[test]
fn drop_last_branch_removes_the_stack_entry() {
    let (_t, repo, store) = setup();
    stack_cmd(&repo, &store)
        .args(["drop", "feat/c"])
        .assert()
        .success();
    stack_cmd(&repo, &store)
        .args(["drop", "feat/b"])
        .assert()
        .success();
    stack_cmd(&repo, &store)
        .args(["drop", "feat/a"])
        .assert()
        .success();

    let json = stacks_json(&store);
    assert_eq!(json["stacks"].as_array().unwrap().len(), 0);
}

#[test]
fn drop_unknown_branch_fails() {
    let (_t, repo, store) = setup();
    stack_cmd(&repo, &store)
        .args(["drop", "not-a-real-branch"])
        .assert()
        .failure();
}
