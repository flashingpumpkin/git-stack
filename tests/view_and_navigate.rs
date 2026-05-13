//! Acceptance tests for step 2: read-only ops.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .args(args)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "git {:?} failed in {}",
        args,
        repo.display()
    );
}

fn git_out(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {:?} failed", args);
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn commit(repo: &Path, file: &str, msg: &str) {
    std::fs::write(repo.join(file), msg).unwrap();
    git(repo, &["add", file]);
    git(repo, &["commit", "-q", "-m", msg]);
}

/// Build a repo with main → b1 → b2 → b3 and a `stacks.json` placed in
/// the central store at the location our identity resolution will pick.
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
    let trunk_head = git_out(&repo, &["rev-parse", "HEAD"]);

    for n in ["b1", "b2", "b3"] {
        git(&repo, &["checkout", "-q", "-b", n]);
        commit(&repo, &format!("{n}.txt"), n);
    }

    let stacks_dir = store.join("repos").join("github.com_flashingpumpkin_demo");
    std::fs::create_dir_all(&stacks_dir).unwrap();
    let b1_head = git_out(&repo, &["rev-parse", "b1"]);
    let b2_head = git_out(&repo, &["rev-parse", "b2"]);
    let stacks_json = format!(
        r#"{{
  "schemaVersion": 1,
  "repository": "github.com:flashingpumpkin/demo",
  "stacks": [{{
    "trunk": {{ "branch": "main", "head": "{trunk_head}" }},
    "lastRefreshedAt": "2026-05-12T11:55:00Z",
    "branches": [
      {{ "branch": "b1", "base": "{trunk_head}" }},
      {{ "branch": "b2", "base": "{b1_head}", "pullRequest": {{
          "number": 42, "id": "PR_x", "url": "https://github.com/flashingpumpkin/demo/pull/42"
      }} }},
      {{ "branch": "b3", "base": "{b2_head}" }}
    ]
  }}]
}}"#
    );
    std::fs::write(stacks_dir.join("stacks.json"), stacks_json).unwrap();

    (tmp, repo, store)
}

fn stack_cmd(repo: &Path, store: &Path) -> Command {
    let mut c = Command::cargo_bin("stack").unwrap();
    c.current_dir(repo)
        .env("GIT_STACK_HOME", store)
        // Disable colour in tests so plain-text assertions are stable
        // regardless of whether the harness happens to look like a TTY.
        .env("NO_COLOR", "1");
    c
}

#[test]
fn view_json_emits_full_stack() {
    let (_tmp, repo, store) = setup();
    git(&repo, &["checkout", "-q", "b2"]);

    let out = stack_cmd(&repo, &store)
        .args(["view", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    assert_eq!(v["trunk"], "main");
    assert_eq!(v["currentBranch"], "b2");
    assert_eq!(v["branches"].as_array().unwrap().len(), 3);
    assert_eq!(v["branches"][1]["name"], "b2");
    assert_eq!(v["branches"][1]["isCurrent"], true);
    assert_eq!(v["branches"][0]["isCurrent"], false);
    // None need rebase yet.
    for b in v["branches"].as_array().unwrap() {
        assert_eq!(
            b["needsRebase"], false,
            "{} should not need rebase",
            b["name"]
        );
    }
}

#[test]
fn view_text_includes_pr_and_refresh_footer() {
    let (_tmp, repo, store) = setup();
    git(&repo, &["checkout", "-q", "b2"]);

    let out = stack_cmd(&repo, &store)
        .env("GIT_STACK_NOW", "2026-05-12T12:00:00Z")
        .args(["view"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();

    // PR number + state appear next to b2.
    assert!(
        stdout.contains("#42") && stdout.contains("open"),
        "expected PR marker, got:\n{stdout}"
    );
    // PR URL on the indented chain row beneath the branch.
    assert!(
        stdout.contains("https://github.com/flashingpumpkin/demo/pull/42"),
        "expected PR URL, got:\n{stdout}"
    );
    // Current branch is marked with the filled circle.
    assert!(
        stdout.contains("●  b2"),
        "expected ●  b2 marker, got:\n{stdout}"
    );
    // Header shows freshness relative to the pinned `now`.
    assert!(
        stdout.contains("refreshed 5m ago"),
        "expected 'refreshed 5m ago' in header, got:\n{stdout}"
    );
    // Trunk anchor at the bottom of the chain.
    assert!(
        stdout.contains("└─ main"),
        "expected trunk anchor, got:\n{stdout}"
    );
}

#[test]
fn view_json_includes_last_refreshed_at() {
    let (_tmp, repo, store) = setup();
    git(&repo, &["checkout", "-q", "b1"]);
    let out = stack_cmd(&repo, &store)
        .args(["view", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["lastRefreshedAt"], "2026-05-12T11:55:00Z");
}

#[test]
fn up_down_top_bottom_navigate() {
    let (_tmp, repo, store) = setup();
    git(&repo, &["checkout", "-q", "b2"]);

    stack_cmd(&repo, &store).args(["up"]).assert().success();
    assert_eq!(git_out(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]), "b3");

    stack_cmd(&repo, &store)
        .args(["down", "2"])
        .assert()
        .success();
    assert_eq!(git_out(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]), "b1");

    stack_cmd(&repo, &store).args(["top"]).assert().success();
    assert_eq!(git_out(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]), "b3");

    stack_cmd(&repo, &store).args(["bottom"]).assert().success();
    assert_eq!(git_out(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]), "b1");
}

#[test]
fn checkout_by_branch_name_works() {
    let (_tmp, repo, store) = setup();
    git(&repo, &["checkout", "-q", "main"]);

    stack_cmd(&repo, &store)
        .args(["checkout", "b2"])
        .assert()
        .success();
    assert_eq!(git_out(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]), "b2");
}

#[test]
fn view_detects_branch_needing_rebase() {
    let (_tmp, repo, store) = setup();
    // Add a new commit on main, then rewrite stacks.json so b1's base is the
    // *old* trunk head — b1 will look out-of-date.
    git(&repo, &["checkout", "-q", "main"]);
    commit(&repo, "extra.txt", "extra");
    let new_trunk = git_out(&repo, &["rev-parse", "HEAD"]);
    let stacks_path = store.join("repos/github.com_flashingpumpkin_demo/stacks.json");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&stacks_path).unwrap()).unwrap();
    json["stacks"][0]["branches"][0]["base"] = serde_json::Value::String(new_trunk);
    std::fs::write(&stacks_path, serde_json::to_string_pretty(&json).unwrap()).unwrap();

    git(&repo, &["checkout", "-q", "b1"]);
    let out = stack_cmd(&repo, &store)
        .args(["view", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["branches"][0]["needsRebase"], true);
}
