# git-stack

[![CI](https://github.com/flashingpumpkin/git-stack/actions/workflows/ci.yml/badge.svg)](https://github.com/flashingpumpkin/git-stack/actions/workflows/ci.yml)

A worktree-safe CLI for stacked branches and stacked pull requests, written in Rust.

`stack` (also invokable as `git stack`) is a reimplementation of [github/gh-stack](https://github.com/github/gh-stack) that fixes the bug that made gh-stack unusable in multi-worktree workflows: it stored stack metadata inside the working directory's `.git/` folder, so creating a new worktree of the same clone — or switching between them — silently lost every stack you had defined.

`git-stack` keeps its state outside the repository, in `$XDG_DATA_HOME/git-stack/`, keyed by the GitHub `owner/repo` of `origin` (or a hash of the git common-dir for non-GitHub repos). Every worktree and every clone of the same GitHub repository sees the same stacks.

## What is a stack?

A stack is an ordered list of branches where each branch builds on the one below it, rooted on a trunk (usually `main`). Each branch maps to one pull request whose base is the branch below it, so reviewers see only the diff for that layer.

```
main (trunk)
 └── feat/auth-layer     → PR #1 (base: main)
  └── feat/api-endpoints → PR #2 (base: feat/auth-layer)
   └── feat/frontend     → PR #3 (base: feat/api-endpoints)
```

This lets you break a large change into small, reviewable PRs that ship together.

## What it looks like

```
$ stack view
  feat/auth  ·  trunk main  ·  refreshed 6m ago

  ●  05-acceptance-tests       #46  open  ▲ needs rebase
  │     https://github.com/acme/platform-api/pull/46
  ○  04-frontend               #45  open  ▲ needs rebase
  │     https://github.com/acme/platform-api/pull/45
  │
  └─ main

  Merged (3) ───────────────────────────────────────────────
  ✓  03-api-routes             #44
  ✓  02-domain-types           #43
  ✓  01-database-schema        #42
```

In a real terminal the current branch is bright cyan, the warnings amber, and everything else dim grey — so the eye lands on the one thing that needs attention.

## Why use this over gh-stack?

| | gh-stack | git-stack |
| --- | --- | --- |
| State location | `.git/gh-stack` (per worktree) | `~/.local/share/git-stack/` (shared) |
| Survives worktree switches | no | yes |
| Survives a new clone of the same repo | no | yes |
| `init` creates a worktree | no | yes, at `~/Worktrees/<repo>/<bottom-branch>` |
| `view` output | interactive TUI | plain text (or `--json`); never a TUI |
| Interactive prompts | several (must work around) | one (`stack switch`), opt-in |
| PR-state refresh | every `view` hits the API | local cache; `view -r` is the explicit refresh |
| Colour | no | yes; honours `NO_COLOR` |

The on-disk schema is byte-compatible with gh-stack's, so `stack migrate` is a one-shot file copy. (We add one optional field, `lastRefreshedAt`, that older files simply don't have.)

## Install

The project pins its Rust toolchain via [mise](https://mise.jdx.dev). With mise installed:

```sh
git clone git@github.com:flashingpumpkin/git-stack.git
cd git-stack
mise trust
mise install
mise run install     # cargo install --path . --force
```

This places two binaries on `PATH`:

- `stack` — the canonical name (`stack init …`).
- `git-stack` — a shim so `git stack …` works (`git` auto-discovers `git-foo` on `PATH`).

Both binaries delegate to the same code. The GitHub-PR commands (`submit`, `view --refresh`, `checkout <pr-number>`) shell out to the [`gh`](https://cli.github.com) CLI, so you'll want that installed and authenticated as well.

## Quick start

```sh
# Migrate any existing gh-stack state into the central store (one-off).
cd /path/to/your/repo
stack migrate

# Create a stack and `cd` into the worktree it just made for you.
cd "$(stack init -p feat auth api ui | tail -1)"

# You start on the bottom (feat/auth). Work, commit, move up.
$EDITOR auth.rs && git commit -am "Add auth"
stack up
$EDITOR api.rs && git commit -am "Add API"
stack up
$EDITOR ui.rs && git commit -am "Add UI"

# See where you are.
stack view

# Push everything and open draft PRs.
stack submit --draft

# Days later, after PR #1 lands on main:
stack sync            # fetch, fast-forward trunk, cascade-rebase, push.
stack view -r         # refresh PR state from GitHub before printing.
```

## Commands

| Command | Purpose |
| --- | --- |
| `stack migrate` | Import an existing `.git/gh-stack` file into the central store. |
| `stack where` | Print resolved repo identity and store path (debugging). |
| `stack init [-p PREFIX] [-b BASE] [-a] [-w PATH] [--no-worktree] BRANCHES...` | Create a stack. Default: also creates a worktree, prints its path on stdout. |
| `stack add [-A\|-u] [-m MSG] BRANCH` | Add a branch on top of the current stack. |
| `stack drop [BRANCH]` | Remove one branch from a stack and re-parent its children. |
| `stack unstack [BRANCH]` | Remove an entire stack from tracking (no rebases). |
| `stack prune [--dry-run] [--no-refresh]` | Drop fully-merged stacks and run `git worktree prune`. |
| `stack list [--json]` | One-line-per-stack index across the repo, including each stack's worktree path. |
| `stack view [--json] [-r/--refresh]` | Show the current stack. `-r` hits GitHub to refresh PR state first. |
| `stack up [N]` / `down [N]` / `top` / `bottom` | Navigate within a stack. |
| `stack checkout <branch\|pr-number>` | Switch to a tracked branch or fetch a PR's stack from GitHub. |
| `stack switch` | Interactive branch picker for the current stack (the only TUI). |
| `stack push` | Force-with-lease atomic push of every active branch. |
| `stack rebase [--upstack\|--downstack] [--continue\|--abort] [--no-refresh]` | Cascade rebase. Refreshes PR state first by default. |
| `stack sync [--no-refresh]` | Fetch, fast-forward trunk, cascade-rebase, push. Refreshes PR state first by default. |
| `stack submit [--draft] [--no-refresh]` | Push and create/refresh GitHub PRs. Refreshes PR state first; skips empty branches. |

Run `stack --help` (or `stack <cmd> --help`) for full flag listings.

## PR state and freshness

`view` and `list` read PR data from the local store, so they're fast and offline. Commands that *act on* the stack (`sync`, `rebase`, `submit`) refresh PR state from GitHub before planning — without this, a PR that's been squash-merged since the last refresh would still look open, and cascade rebase would try to replay its commits and conflict against the already-in-trunk equivalents.

Use `--no-refresh` to skip the network round-trip if you know nothing's changed on the GitHub side:

```sh
stack sync --no-refresh
stack rebase --no-refresh
stack submit --auto --no-refresh
```

`stack view` is read-only and stays offline by default; `stack view -r` (or `--refresh`) is the explicit refresh path. The header tells you how stale the cached state is:

```
  feat/auth  ·  trunk main  ·  refreshed 6m ago
```

The schema records freshness per stack in an optional `lastRefreshedAt` (RFC 3339) field.

## Output, colour, exit codes

- **Status messages** go to stderr with emoji prefixes: `✓`, `✗`, `ℹ`.
- **Data output** (`--json`, `stack where`, the worktree path printed by `init`) goes to stdout, so it's safe to pipe.
- **Colour** is auto-enabled when stdout is a TTY. Honours `NO_COLOR` and `GIT_STACK_NO_COLOR`.
- **No prompts**, with one deliberate exception: `stack switch` opens a branch picker for humans. Every other command runs non-interactively.

Exit codes mirror gh-stack's convention so scripts written against `gh stack` continue to work:

| Code | Meaning |
| --- | --- |
| 0 | Success |
| 1 | Generic error |
| 2 | Not in a stack |
| 3 | Rebase conflict (resolve, then `stack rebase --continue`) |
| 4 | GitHub API failure |
| 5 | Invalid arguments |
| 6 | Disambiguation (current branch is in multiple stacks) |
| 7 | Rebase already in progress |
| 8 | Store locked by another `stack` process |

## Storage

```
$XDG_DATA_HOME/git-stack/
└── repos/
    ├── github.com_OWNER_REPO/
    │   ├── stacks.json
    │   ├── stacks.json.lock         (advisory lock, fd-lock)
    │   └── stacks.json.rebase       (only present mid-rebase)
    └── local_<sha256-prefix>/
        └── ...
```

- Override the root with `GIT_STACK_HOME=/some/path` (used by the test suite, useful for sandboxing).
- The on-disk schema is byte-compatible with gh-stack's `.git/gh-stack` file; we add one optional field (`lastRefreshedAt`) that older files simply omit.
- Writes are atomic (tmp-file + `rename`); concurrent `stack` processes contend for an advisory lock with a 5-second timeout.

Identity resolution:

1. `git remote get-url origin` → if it's a GitHub URL, canonicalise to `github.com:OWNER/REPO` and slug-ify to `github.com_OWNER_REPO`.
2. Otherwise, sha256 the absolute path of the git common-dir and use `local_<prefix>`.

So worktrees of one clone, and separate clones of the same GitHub repo, all converge on the same entry. That's the bug this project exists to fix.

## Architecture

Hexagonal:

- `domain/` — pure types (`Stack`, `Branch`, `Trunk`, `PullRequest`) and rules (prefix application, navigation, disambiguation).
- `store/` — JSON persistence, file locking, atomic tmp-rename writes, identity resolution.
- `git/` — `GitOps` trait + production adapter that shells out to the `git` binary + `FakeGit` for tests.
- `github/` — `GitHub` trait + `GhCli` adapter that shells out to `gh` + `FakeGitHub` for tests.
- `ops/` — one module per command, taking the ports as parameters so orchestration is unit-testable.
- `style/` — tiny SGR helper with TTY + `NO_COLOR` detection and a narrow four-colour palette.
- `cli/` — `clap` definitions; a thin layer over `ops/`.

See `docs/plans/2026-05-12-stack-cli-design.md` for the original design discussion.

## Development

```sh
mise run build       # cargo build --release
mise run test        # cargo test
mise run fmt         # cargo fmt --all
mise run lint        # cargo clippy --all-targets -- -D warnings
mise run check       # fmt check + lint + test
```

CI (GitHub Actions) runs `rustfmt`, `clippy -D warnings`, and the full test suite on Linux and macOS for every push and PR.

The test strategy: a small set of pure-domain unit tests, in-process acceptance tests for every command using a real `git` binary against tempdir repos, and in-memory fakes (`FakeGit`, `FakeGitHub`) wherever an adapter would otherwise force network or filesystem state. A dedicated worktree regression test creates a `git worktree`, runs `stack` from both checkouts, and asserts they resolve to the same store entry — the canary for the bug this project exists to fix.

## Status and limitations

The CLI covers the full gh-stack surface plus `migrate`, `list`, `drop`, `switch`, worktree-aware `init`, and opt-in `view --refresh`. Known limitations:

1. **Linear stacks only** — no branching stacks (multiple children per parent).
2. **No GitHub "Stack" linking** — PRs are chained via their `base` refs, which is what reviewers actually see. The undocumented `linkPullRequestsAsStack` GraphQL mutation is out of scope.
3. **`checkout <pr-number>` walks one PR at a time** — for multi-PR stacks, check out the bottommost PR.
4. **Merging happens in the GitHub UI** — the CLI never merges PRs.
5. **PR titles are auto-generated** — use `gh pr edit` to customise after `submit`.

## License

MIT.
