//! Acceptance tests for the `pool` standalone command and the
//! `git-pool` subcommand binary.

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

fn pool_cmd(repo: &Path, store: &Path) -> Command {
    let mut c = Command::cargo_bin("pool").unwrap();
    c.current_dir(repo).env("GIT_STACK_HOME", store);
    c
}

fn git_pool_cmd(repo: &Path, store: &Path) -> Command {
    let mut c = Command::cargo_bin("git-pool").unwrap();
    c.current_dir(repo).env("GIT_STACK_HOME", store);
    c
}

fn pools_json(store: &Path) -> serde_json::Value {
    let p = store.join("repos/github.com_flashingpumpkin_demo/pools.json");
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

fn pools_json_path(store: &Path) -> std::path::PathBuf {
    store.join("repos/github.com_flashingpumpkin_demo/pools.json")
}

fn stacks_json_path(store: &Path) -> std::path::PathBuf {
    store.join("repos/github.com_flashingpumpkin_demo/stacks.json")
}

#[test]
fn add_auto_creates_default_pool_and_records_branch() {
    let (_t, repo, store) = fresh_repo();
    // Create a real branch sitting on trunk.
    git(&repo, &["checkout", "-q", "-b", "feature-1"]);
    git(&repo, &["checkout", "-q", "main"]);

    pool_cmd(&repo, &store)
        .args(["add", "feature-1"])
        .assert()
        .success();

    let json = pools_json(&store);
    let pools = json["pools"].as_array().unwrap();
    assert_eq!(pools.len(), 1, "exactly one pool, the default one");
    // Default pool: no `name` key written.
    assert!(pools[0].get("name").is_none());
    let names: Vec<&str> = pools[0]["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["branch"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["feature-1"]);
}

#[test]
fn add_to_named_pool_separates_from_default() {
    let (_t, repo, store) = fresh_repo();
    git(&repo, &["checkout", "-q", "-b", "a"]);
    git(&repo, &["checkout", "-q", "-b", "b"]);
    git(&repo, &["checkout", "-q", "main"]);

    pool_cmd(&repo, &store)
        .args(["add", "a"])
        .assert()
        .success();
    pool_cmd(&repo, &store)
        .args(["add", "--pool", "review-queue", "b"])
        .assert()
        .success();

    let json = pools_json(&store);
    let pools = json["pools"].as_array().unwrap();
    assert_eq!(pools.len(), 2);
    let named = pools
        .iter()
        .find(|p| p["name"].as_str() == Some("review-queue"))
        .expect("named pool must exist");
    let default = pools
        .iter()
        .find(|p| p.get("name").is_none())
        .expect("default pool must exist");
    assert_eq!(default["branches"][0]["branch"].as_str().unwrap(), "a");
    assert_eq!(named["branches"][0]["branch"].as_str().unwrap(), "b");
}

#[test]
fn add_refuses_branch_already_in_pool() {
    let (_t, repo, store) = fresh_repo();
    git(&repo, &["checkout", "-q", "-b", "x"]);
    git(&repo, &["checkout", "-q", "main"]);

    pool_cmd(&repo, &store)
        .args(["add", "x"])
        .assert()
        .success();
    pool_cmd(&repo, &store)
        .args(["add", "x"])
        .assert()
        .failure();
}

#[test]
fn remove_drops_branch_from_pool_but_leaves_git_branch_alone() {
    let (_t, repo, store) = fresh_repo();
    git(&repo, &["checkout", "-q", "-b", "y"]);
    git(&repo, &["checkout", "-q", "main"]);

    pool_cmd(&repo, &store)
        .args(["add", "y"])
        .assert()
        .success();
    pool_cmd(&repo, &store)
        .args(["remove", "y"])
        .assert()
        .success();

    let json = pools_json(&store);
    let pools = json["pools"].as_array().unwrap();
    assert!(pools[0]["branches"].as_array().unwrap().is_empty());

    // The git branch is left alone.
    let exists = Command::new("git")
        .current_dir(&repo)
        .args(["show-ref", "--verify", "--quiet", "refs/heads/y"])
        .status()
        .unwrap()
        .success();
    assert!(exists, "git branch should still exist after `pool remove`");
}

#[test]
fn list_json_emits_expected_shape() {
    let (_t, repo, store) = fresh_repo();
    git(&repo, &["checkout", "-q", "-b", "z"]);
    git(&repo, &["checkout", "-q", "main"]);

    pool_cmd(&repo, &store)
        .args(["add", "z"])
        .assert()
        .success();
    let out = pool_cmd(&repo, &store)
        .args(["list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["pools"][0]["branches"][0]["name"], "z");
    assert_eq!(v["pools"][0]["branches"][0]["needsRebase"], false);
    assert_eq!(v["pools"][0]["branches"][0]["isMerged"], false);
}

#[test]
fn git_pool_binary_is_invokable_alongside_pool() {
    // `git pool` resolves to the `git-pool` binary via git's auto-discovery,
    // so we just need to confirm the binary itself runs end-to-end with the
    // same dispatch path.
    let (_t, repo, store) = fresh_repo();
    git(&repo, &["checkout", "-q", "-b", "w"]);
    git(&repo, &["checkout", "-q", "main"]);

    git_pool_cmd(&repo, &store)
        .args(["add", "w"])
        .assert()
        .success();
    git_pool_cmd(&repo, &store)
        .args(["list", "--json"])
        .assert()
        .success();
}

#[test]
fn prune_dry_run_writes_nothing() {
    // Set up a pool with a fake "already merged" branch by manually
    // editing pools.json — the easiest way to exercise prune without
    // network. The branch must still exist locally so add() accepts it.
    let (_t, repo, store) = fresh_repo();
    git(&repo, &["checkout", "-q", "-b", "merged-branch"]);
    git(&repo, &["checkout", "-q", "main"]);

    pool_cmd(&repo, &store)
        .args(["add", "merged-branch"])
        .assert()
        .success();

    // Hand-edit the store to mark the PR as merged.
    let path = pools_json_path(&store);
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    v["pools"][0]["branches"][0]["pullRequest"] = serde_json::json!({
        "number": 1,
        "id": "PR_1",
        "url": "https://example/1",
        "merged": true,
        "commentCount": 0,
        "seenCommentCount": 0,
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();

    pool_cmd(&repo, &store)
        .args(["prune", "--dry-run", "--no-refresh"])
        .assert()
        .success();

    // Branch is still present after dry-run.
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        json["pools"][0]["branches"][0]["branch"].as_str().unwrap(),
        "merged-branch"
    );

    pool_cmd(&repo, &store)
        .args(["prune", "--no-refresh"])
        .assert()
        .success();

    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(json["pools"][0]["branches"].as_array().unwrap().is_empty());
}

#[test]
fn pool_data_lives_in_pools_json_not_stacks_json() {
    // Splitting storage is a deliberate design choice — assert it stays
    // that way. After a `pool add`, pools.json must exist and contain the
    // branch, and stacks.json must NOT mention pools at all (it either
    // doesn't exist yet or contains only an empty `stacks` array).
    let (_t, repo, store) = fresh_repo();
    git(&repo, &["checkout", "-q", "-b", "boundary-check"]);
    git(&repo, &["checkout", "-q", "main"]);

    pool_cmd(&repo, &store)
        .args(["add", "boundary-check"])
        .assert()
        .success();

    let pools_path = pools_json_path(&store);
    let stacks_path = stacks_json_path(&store);

    assert!(pools_path.exists(), "pools.json must exist after pool add");
    let pools: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&pools_path).unwrap()).unwrap();
    assert_eq!(
        pools["pools"][0]["branches"][0]["branch"].as_str().unwrap(),
        "boundary-check"
    );

    if stacks_path.exists() {
        let stacks: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&stacks_path).unwrap()).unwrap();
        assert!(
            stacks.get("pools").is_none(),
            "stacks.json must not contain a `pools` field after the storage split"
        );
    }
}

#[test]
fn init_creates_branch_worktree_and_records_in_pool() {
    let (_t, repo, store) = fresh_repo();

    // --no-worktree path: confirm the branch exists locally and lands in
    // pools.json with the right base. Avoids the worktree-on-tempdir
    // failure mode where the default ~/Worktrees path tries to write
    // outside the sandbox.
    pool_cmd(&repo, &store)
        .args(["init", "--no-worktree", "feature/new"])
        .assert()
        .success();

    // git branch was created.
    let exists = Command::new("git")
        .current_dir(&repo)
        .args(["show-ref", "--verify", "--quiet", "refs/heads/feature/new"])
        .status()
        .unwrap()
        .success();
    assert!(exists, "init must create the git branch");

    // pools.json records the new branch with main as its base.
    let json = pools_json(&store);
    let pools = json["pools"].as_array().unwrap();
    assert_eq!(pools.len(), 1);
    let b = &pools[0]["branches"][0];
    assert_eq!(b["branch"].as_str().unwrap(), "feature/new");
    assert_eq!(b["baseBranch"].as_str().unwrap(), "main");
    assert!(b["pullRequest"].is_null() || b["pullRequest"].as_object().is_none());
}

#[test]
fn init_refuses_when_branch_already_exists_without_adopt() {
    let (_t, repo, store) = fresh_repo();
    Command::new("git")
        .current_dir(&repo)
        .args(["branch", "already-here"])
        .status()
        .unwrap();
    pool_cmd(&repo, &store)
        .args(["init", "--no-worktree", "already-here"])
        .assert()
        .failure();
}

#[test]
fn init_adopt_takes_over_an_existing_branch() {
    let (_t, repo, store) = fresh_repo();
    Command::new("git")
        .current_dir(&repo)
        .args(["branch", "preexisting"])
        .status()
        .unwrap();
    pool_cmd(&repo, &store)
        .args(["init", "--no-worktree", "--adopt", "preexisting"])
        .assert()
        .success();
    let json = pools_json(&store);
    assert_eq!(
        json["pools"][0]["branches"][0]["branch"].as_str().unwrap(),
        "preexisting"
    );
}
