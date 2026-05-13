//! Acceptance tests for step 1: storage + migrate.
//!
//! The worktree test is the regression guard for the bug that motivated this
//! whole project — gh-stack's per-worktree `.git/gh-stack` file gets lost
//! when you switch worktrees. Our central store must survive that.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use tempfile::TempDir;

const SAMPLE_GH_STACK: &str = r#"{
  "schemaVersion": 1,
  "repository": "github.com:acme/platform-api",
  "stacks": [
    {
      "prefix": "feat/auth",
      "trunk": { "branch": "main", "head": "90f5035636185a1ba82e738737f6c09f8f0835b6" },
      "branches": [
        {
          "branch": "feat/auth/01-init",
          "head": "e20ef72102ee72b36838b645a0f65f1a20b89a57",
          "base": "cbe1036ae4c94e1decccd1b718fd2552235c5a35",
          "pullRequest": {
            "number": 42,
            "id": "PR_kwexample002",
            "url": "https://github.com/acme/platform-api/pull/42"
          }
        }
      ]
    }
  ]
}"#;

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .args(args)
        .status()
        .expect("run git");
    assert!(
        status.success(),
        "git {:?} failed in {}",
        args,
        repo.display()
    );
}

fn init_repo(repo: &Path, origin: &str) {
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "Test"]);
    git(repo, &["remote", "add", "origin", origin]);
    std::fs::write(repo.join("README.md"), b"hi").unwrap();
    git(repo, &["add", "README.md"]);
    git(repo, &["commit", "-q", "-m", "init"]);
}

#[test]
fn migrate_imports_legacy_gh_stack_file() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let store = tmp.path().join("store");
    std::fs::create_dir(&repo).unwrap();

    init_repo(&repo, "git@github.com:acme/platform-api.git");
    std::fs::write(repo.join(".git/gh-stack"), SAMPLE_GH_STACK).unwrap();

    let mut cmd = Command::cargo_bin("stack").unwrap();
    cmd.current_dir(&repo)
        .env("GIT_STACK_HOME", &store)
        .args(["migrate"]);
    cmd.assert().success();

    let target = store
        .join("repos")
        .join("github.com_acme_platform-api")
        .join("stacks.json");
    assert!(
        target.exists(),
        "stacks.json should exist at {}",
        target.display()
    );

    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
    assert_eq!(written["schemaVersion"], 1);
    assert_eq!(written["repository"], "github.com:acme/platform-api");
    assert_eq!(
        written["stacks"][0]["branches"][0]["pullRequest"]["number"],
        42
    );

    // Legacy file was renamed.
    assert!(!repo.join(".git/gh-stack").exists());
    assert!(repo.join(".git/gh-stack.migrated").exists());
}

#[test]
fn migrate_refuses_to_overwrite_without_force() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let store = tmp.path().join("store");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo, "git@github.com:foo/bar.git");

    // First migration succeeds.
    std::fs::write(repo.join(".git/gh-stack"), SAMPLE_GH_STACK).unwrap();
    Command::cargo_bin("stack")
        .unwrap()
        .current_dir(&repo)
        .env("GIT_STACK_HOME", &store)
        .args(["migrate"])
        .assert()
        .success();

    // Put the legacy file back and try again — should fail without --force.
    std::fs::write(repo.join(".git/gh-stack"), SAMPLE_GH_STACK).unwrap();
    Command::cargo_bin("stack")
        .unwrap()
        .current_dir(&repo)
        .env("GIT_STACK_HOME", &store)
        .args(["migrate"])
        .assert()
        .failure();

    // With --force it succeeds.
    Command::cargo_bin("stack")
        .unwrap()
        .current_dir(&repo)
        .env("GIT_STACK_HOME", &store)
        .args(["migrate", "--force"])
        .assert()
        .success();
}

#[test]
fn worktrees_share_the_same_store_entry() {
    // The regression test. Create a main checkout + a linked worktree.
    // `stack where` from either must resolve to the same store path.
    let tmp = TempDir::new().unwrap();
    let main = tmp.path().join("main");
    let wt = tmp.path().join("feature-wt");
    let store = tmp.path().join("store");
    std::fs::create_dir(&main).unwrap();
    init_repo(&main, "git@github.com:flashingpumpkin/git-stack.git");

    // Create a linked worktree.
    git(
        &main,
        &["worktree", "add", wt.to_str().unwrap(), "-b", "feature"],
    );

    let output_main = Command::cargo_bin("stack")
        .unwrap()
        .current_dir(&main)
        .env("GIT_STACK_HOME", &store)
        .args(["where"])
        .output()
        .unwrap();
    let output_wt = Command::cargo_bin("stack")
        .unwrap()
        .current_dir(&wt)
        .env("GIT_STACK_HOME", &store)
        .args(["where"])
        .output()
        .unwrap();

    assert!(output_main.status.success());
    assert!(output_wt.status.success());

    let main_out = String::from_utf8_lossy(&output_main.stdout).to_string();
    let wt_out = String::from_utf8_lossy(&output_wt.stdout).to_string();

    // Both must resolve to the same store directory — this is the bug fix.
    let main_store_line = main_out
        .lines()
        .find(|l| l.starts_with("store"))
        .expect("store line in main output");
    let wt_store_line = wt_out
        .lines()
        .find(|l| l.starts_with("store"))
        .expect("store line in worktree output");
    assert_eq!(
        main_store_line, wt_store_line,
        "worktree should share the same store dir as the main checkout\n\nmain:\n{}\nworktree:\n{}",
        main_out, wt_out
    );

    // And the slug must be the GitHub identity, not a local hash.
    assert!(
        main_store_line.contains("github.com_flashingpumpkin_git-stack"),
        "expected GitHub identity in store path, got: {}",
        main_store_line
    );
}
