# `stack` — a Rust reimplementation of `gh-stack` with worktree-safe state

**Date:** 2026-05-12
**Status:** approved, in implementation
**Author:** alen + Claude (brainstorming session)

## Problem

`gh-stack` stores stack metadata in `.git/gh-stack` inside each working tree. Because `.git/` is not shared between worktrees, switching worktrees or creating a new worktree of the same clone loses stack state. The user has hit this repeatedly.

## Goal

Reimplement `gh-stack` in Rust, persisting stack state in a central, machine-global location keyed by repo identity, so the same stack is visible from any worktree or clone of the same GitHub repository.

## Non-goals (v1)

- The GitHub "linked stacked PRs" GraphQL feature (proprietary, undocumented). PRs are still chained via their `base` ref, which is what reviewers actually need.
- Direct HTTP to the GitHub API. We shell out to `gh` for auth and requests.
- Interactive TUI. `stack view` prints text or JSON; never a TUI.

## Identity

```
1. `git remote get-url origin` → canonicalise to `github.com:owner/repo`
2. Fallback: sha256 of `git rev-parse --git-common-dir`
```

Both worktrees of one clone and separate clones of the same GitHub repo resolve to the same identity (case 1). Local-only repos still get isolated state (case 2).

## Storage

```
$XDG_DATA_HOME/git-stack/repos/<identity>/stacks.json
                                          /stacks.json.lock
                                          /stacks.json.rebase   (mid-rebase only)
```

Override with `GIT_STACK_HOME`. Schema is byte-identical to `gh-stack` v1 so `stack migrate` is a file copy.

Concurrency: exclusive `fd-lock` on `.lock` file, 5 s timeout → exit 8. Writes are tmp-file + fsync + rename.

## Binaries

Two binaries from one crate; both delegate to `git_stack::cli::run()`:

- `stack` — primary CLI (`stack init …`).
- `git-stack` — shim so `git stack …` works (git auto-discovers `git-foo` on PATH).

## Toolchain

`mise.toml` pins Rust and exposes `build`, `test`, `install`, `fmt`, `lint` tasks.

## Command surface

Full parity with `gh stack`: `init`, `add`, `push`, `submit`, `sync`, `rebase`, `view`, `up`/`down`/`top`/`bottom`, `checkout`, `unstack`, plus our own `migrate`. Exit codes mirror gh-stack (0/1/2/3/4/5/6/7/8).

All defaults non-interactive. `view` defaults to text; `view --json` is the machine-readable form. `submit` requires `--auto` (we don't prompt for titles).

## Architecture

Hexagonal:

- `domain/` — pure types (`Stack`, `Branch`, `Trunk`, `PullRequest`, `StackId`) and rules (prefix application, navigation, current-stack lookup, disambiguation).
- `store/` — JSON persistence, locking, atomic writes, identity resolution.
- `git/` — `Git` port + adapter that shells out to the `git` binary.
- `github/` — `GitHub` port + adapter that shells out to `gh`.
- `ops/` — use cases: one module per command, taking the ports as dependencies.
- `cli/` — clap definitions; thin layer over `ops/`.

## Testing

- Unit tests on domain rules.
- Acceptance tests in `tests/acceptance/` drive the CLI against a temp git repo with `GIT_STACK_HOME` and a fake `gh` injected via `PATH`. These are the primary safety net.
- Contract tests run the same suite against the real `git` adapter and the in-memory fake.
- A dedicated worktree regression test: init a stack in one worktree, view it from another, assert it's visible. This is the bug we're fixing.

## Delivery plan

1. Skeleton + storage + `migrate`.
2. Read-only ops: `view`, navigation, `checkout <branch>`.
3. Local mgmt: `init`, `add`, `unstack`.
4. `push`, `rebase` (with `--continue`/`--abort`), `sync` (no PR state).
5. GitHub integration: `submit`, PR state refresh, squash-merge detection, `checkout <pr-number>`.

Each step ships independently and leaves the tool more capable than the previous.
