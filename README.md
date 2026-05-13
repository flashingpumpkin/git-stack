# git-stack

[![CI](https://github.com/flashingpumpkin/git-stack/actions/workflows/ci.yml/badge.svg)](https://github.com/flashingpumpkin/git-stack/actions/workflows/ci.yml)

A worktree-safe CLI for stacked branches and stacked pull requests, written in Rust. Ships two independent tools that share a per-repo state directory (each with its own JSON file inside):

- **`stack`** — work on a chain of dependent branches (stacked PRs). Reimplementation of [github/gh-stack](https://github.com/github/gh-stack) that fixes the bug that made gh-stack unusable in multi-worktree workflows.
- **`pool`** — track a flat collection of independent branches (a grab-bag of open PRs), with PR state, comment activity, conflict detection, and per-branch rebases.

State lives outside the repository, in `$XDG_DATA_HOME/git-stack/`, keyed by the GitHub `owner/repo` of `origin` (or a hash of the git common-dir for non-GitHub repos). Every worktree and every clone of the same GitHub repository sees the same stacks and pools.

### Stacks vs. pools — which do I want?

| | Stack | Pool |
| --- | --- | --- |
| Relationship between branches | Linear chain; each branch builds on the one below | Flat; branches are independent |
| Default base for each branch | The previous branch in the chain | Trunk, or per-branch (PR-targeted or `--base`) |
| Rebase target | Cascade: each branch onto its parent | Each branch onto the live head of its own base |
| Best for | Breaking one big change into reviewable layers | A grab-bag of unrelated open PRs |
| Bulk adopt from GitHub | `stack checkout <pr-number>` (one chain at a time) | `pool adopt` (every open `@me` PR at once) |

A repo can have any number of stacks **and** any number of pools at the same time; a branch belongs to at most one of either.

## Install

The project pins its Rust toolchain via [mise](https://mise.jdx.dev). With mise installed:

```sh
git clone git@github.com:flashingpumpkin/git-stack.git
cd git-stack
mise trust
mise install
mise run install     # cargo install --path . --force
```

This places four binaries on `PATH`:

- `stack` — the canonical name (`stack init …`).
- `git-stack` — a shim so `git stack …` works (`git` auto-discovers `git-foo` on `PATH`).
- `pool` — the canonical name for the pool command (`pool add …`).
- `git-pool` — a shim so `git pool …` works.

All four binaries delegate to the same library. The GitHub commands (`submit`, `view --refresh`, `checkout <pr-number>`, every `pool` command that talks to GitHub) shell out to the [`gh`](https://cli.github.com) CLI, so you'll want that installed and authenticated as well.

---

## Stacks

A stack is an ordered list of branches where each branch builds on the one below it, rooted on a trunk (usually `main`). Each branch maps to one pull request whose base is the branch below it, so reviewers see only the diff for that layer.

```
main (trunk)
 └── feat/auth-layer     → PR #1 (base: main)
  └── feat/api-endpoints → PR #2 (base: feat/auth-layer)
   └── feat/frontend     → PR #3 (base: feat/api-endpoints)
```

This lets you break a large change into small, reviewable PRs that ship together.

### What `stack view` looks like

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

### Stack quick start

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

### Stack commands

| Command | Purpose |
| --- | --- |
| `stack migrate` | Import an existing `.git/gh-stack` file into the central store. |
| `stack where` | Print resolved repo identity and store path (debugging). |
| `stack init [-p PREFIX] [-b BASE] [-a] [-w PATH] [--no-worktree] BRANCHES...` | Create a stack. Default: also creates a worktree, prints its path on stdout. |
| `stack add [-A\|-u] [-m MSG] BRANCH` | Add a branch on top of the current stack. |
| `stack drop [BRANCH]` | Remove one branch from a stack and re-parent its children. |
| `stack remove BRANCH [--force]` | Stop tracking the stack containing `BRANCH` and remove its worktree(s). Refuses on uncommitted changes unless `--force`. Leaves git branches alone. |
| `stack prune [--dry-run] [--no-refresh]` | Drop fully-merged stacks and run `git worktree prune`. |
| `stack list [--json] [-r/--refresh]` | Per-stack index across the repo: header row (prefix, trunk, worktree, counts) followed by each branch with PR number, state, and URL. `-r` refreshes PR state from GitHub first. |
| `stack view [--json] [-r/--refresh]` | Show the current stack. `-r` hits GitHub to refresh PR state first. |
| `stack up [N]` / `down [N]` / `top` / `bottom` | Navigate within a stack. |
| `stack checkout <branch\|pr-number>` | Switch to a tracked branch or fetch a PR's stack from GitHub. |
| `stack switch` | Interactive branch picker for the current stack (the only TUI). |
| `stack push` | Force-with-lease atomic push of every active branch. |
| `stack rebase [--upstack\|--downstack] [--continue\|--abort] [--no-refresh]` | Cascade rebase. Refreshes PR state first by default. |
| `stack sync [--no-refresh]` | Fetch, fast-forward trunk, cascade-rebase, push. Refreshes PR state first by default. |
| `stack submit [--draft] [--no-refresh]` | Push and create/refresh GitHub PRs. Refreshes PR state first; skips empty branches. |

Run `stack --help` (or `stack <cmd> --help`) for full flag listings.

---

## Pools

A pool is a flat collection of *independent* branches. Unlike a stack, pool members don't depend on each other — each one tracks its own base branch (most often trunk, but a pool can mix branches targeted at trunk with branches targeted at a release line or any other long-lived ref). Pools are for the everyday case of having half a dozen unrelated PRs open at once: you want one place to see their state, get notified about new review comments, spot merge conflicts before you start rebasing, and rebase them all in one shot when their bases move.

```
main (trunk)
 ├── fix/typo-in-readme            → PR #50  (base: main)
 ├── chore/bump-deps               → PR #51  (base: main)
 └── feat/new-metric               → PR #52  (base: release/2026.Q2)
```

`pool` is its own top-level command, installed as both `pool` (standalone) and `git-pool` (so `git pool …` works). Pool state lives in its own file (`pools.json`) in the same per-repo state directory `stack` uses, so the two tools coexist cleanly in one repo without sharing schema.

### What `pool list` looks like

```
$ pool list
  github.com:acme/platform-api  ·  2 pools

  (default)  ·  default base main  ·  refreshed 4m ago

  ○  fix/typo-in-readme     → main                #50  open
  │     https://github.com/acme/platform-api/pull/50
  ○  chore/bump-deps        → main                #51  open  ▲ needs rebase  ✉ 3 new comments
  │     https://github.com/acme/platform-api/pull/51
  ○  feat/new-metric        → release/2026.Q2     #52  open  ⚠ conflicts
  │     https://github.com/acme/platform-api/pull/52

  review-queue  ·  default base main  ·  refreshed 4m ago

  ○  feat/auth-rotation     → main                #48  open  ✉ 1 new comment
  │     https://github.com/acme/platform-api/pull/48
```

Each row shows the branch's **own** base, not the pool's default. `pool rebase` rebases each branch onto the live head of that branch's base, so a pool can mix branches targeted at trunk with branches targeted at a release line.

What `pool` tracks for each branch (refreshed from GitHub via `pool refresh` or `pool list -r`):

- **PR state** — number, URL, open/merged.
- **Rebase health** — `▲ needs rebase` when the branch's stored base SHA doesn't match the live head of its base ref.
- **Merge-conflict state** — `⚠ conflicts` when GitHub reports the PR as `CONFLICTING`. Surfaced *before* you kick off `pool rebase` so you know which branches will halt the cascade.
- **Unread review activity** — `✉ N new comments` counts everything (issue comments, review summaries, inline review-thread comments) added since you last looked. The baseline advances after every `pool list` so the indicator clears on its own.
- **Merged-base reparenting** — if your branch's base is itself merged on GitHub (you were parked on a feature branch that just landed), refresh walks up the PR chain and reparents you onto the first non-merged ancestor. The next `pool rebase` then targets the right base automatically — same mechanism `stack rebase` uses to skip merged parents.

### Pool quick start

```sh
# Start a new branch from scratch: pool init creates the branch off
# trunk (or whatever --base you pick), sets up a worktree at
# ~/Worktrees/<repo>/<branch>, registers it in the pool, and prints
# the worktree path on stdout. cd into the worktree to start working:
cd "$(pool init feat/new-metric | tail -1)"

# Adopt every open PR you've got into the default pool, automatically.
# (Skips branches already in a stack or pool, and PRs whose head branch
# you don't have locally.)
pool adopt

# Or add branches one at a time. The base is inferred from the PR's
# baseRefName when a PR already exists; otherwise it defaults to trunk.
pool add fix/typo chore/bump-deps feat/new-metric

# Override the base explicitly for a branch that lives on a non-trunk
# upstream (release line, parent feature branch, etc).
pool add --base release/2026.Q2 fix/critical-regression

# See where everything stands. -r refreshes PR state from GitHub first
# and resets the new-comment baseline after rendering. Without -r, list
# stays offline and reads from the local cache.
pool list -r

# Pull just the activity report. Reports new PRs discovered, merged
# transitions, new comments, conflict-state flips, and branches that
# got reparented because their base merged.
pool refresh

# Push every branch and open a PR for any that doesn't have one yet.
# Branches with existing PRs get their stored data refreshed in place;
# empty branches (no commits over their base) are skipped.
pool submit --draft

# Rebase every active pool branch onto the live head of its own base.
# Prints a pre-flight list of branches whose PRs GitHub already marks
# as CONFLICTING so you know what to expect. If your branch's base was
# merged since your last refresh, you'll get rebased onto whatever the
# merged base merged into — automatic reparenting.
pool rebase

# Tidy up: drop any branch whose PR has been merged.
pool prune

# Drop a specific branch from the pool without touching the git branch
# itself.
pool remove fix/typo
```

A repo can have one unnamed default pool plus any number of named ones. Pass `--pool <name>` to target a specific one:

```sh
pool add --pool review-queue feat/audit-trail
pool list                              # shows every pool
pool refresh --pool review-queue
pool rebase --pool review-queue
pool prune --pool review-queue
```

`git pool …` works identically to `pool …` because the `git-pool` binary is on `PATH`.

### Pool commands

| Command | Purpose |
| --- | --- |
| `pool init [--pool NAME] [-b BASE] [-a] [-w PATH] [--no-worktree] BRANCH` | Create (or `--adopt`) a single branch off `BASE`, set up its worktree, and add it to a pool. Auto-creates the pool. Prints the worktree path on stdout. |
| `pool add [--pool NAME] [--base BRANCH] BRANCH...` | Add existing local branches to a pool. Base resolves to `--base`, then the PR's GitHub base ref, then the pool's default trunk. |
| `pool adopt [--pool NAME]` | Bulk-adopt every open PR authored by you (`gh @me`) whose head branch is local and not already in a stack or pool. |
| `pool remove [BRANCH]` | Drop a branch from its pool (defaults to current). Leaves the git branch alone. |
| `pool list [-r/--refresh] [--json]` | Show every pool, with PR state, comment activity, rebase health, and `⚠ conflicts` indicators. `-r` hits GitHub first. |
| `pool refresh [--pool NAME]` | Hit GitHub: discover PRs by head ref, update merged flags, comment counts, mergeable status, and reparent branches whose base was merged. Reports what changed since the last refresh. Doesn't drop branches — that's `pool prune`. |
| `pool prune [--pool NAME] [--dry-run] [--no-refresh]` | Drop branches whose PRs are merged. Refreshes first by default. |
| `pool submit [--pool NAME] [--draft] [--remote ORIGIN] [--no-refresh]` | Push every active branch and open a PR for any that doesn't have one yet. PRs that already exist get refreshed in place. Each PR targets its branch's own base ref; empty branches are skipped. |
| `pool rebase [--pool NAME] [--continue] [--abort] [--no-refresh]` | Rebase every active branch onto the live head of *its own* base branch (trunk for most; whatever the PR targets for the rest). Prints a pre-flight list of branches whose PRs are marked conflicting on GitHub. |

Run `pool --help` (or `pool <cmd> --help`) for full flag listings.

---

## PR state and freshness

`stack view`, `stack list`, and `pool list` read PR data from the local store, so they're fast and offline. Commands that *act on* a stack or pool (`stack sync`, `stack rebase`, `stack submit`, `pool rebase`, `pool prune`) refresh PR state from GitHub before planning — without this, a PR that's been squash-merged since the last refresh would still look open, and rebases would try to replay its commits and conflict against the already-in-trunk equivalents.

Use `--no-refresh` to skip the network round-trip if you know nothing's changed on the GitHub side:

```sh
stack sync --no-refresh
stack rebase --no-refresh
stack submit --auto --no-refresh
pool rebase --no-refresh
pool prune --no-refresh
```

`stack view` and `pool list` stay offline by default; their `-r` / `--refresh` flags are the explicit refresh paths. The header tells you how stale the cached state is:

```
  feat/auth   ·  trunk main         ·  refreshed 6m ago
  (default)   ·  default base main  ·  refreshed 4m ago
```

For pools, every PR carries a `seenCommentCount` baseline alongside the live `commentCount`; `pool list` advances the baseline after rendering so `✉ N new comments` indicators clear once you've actually seen them. The schema records freshness per stack and per pool in an optional `lastRefreshedAt` (RFC 3339) field.

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
| 3 | Rebase conflict (resolve, then `stack rebase --continue` / `pool rebase --continue`) |
| 4 | GitHub API failure |
| 5 | Invalid arguments |
| 6 | Disambiguation (current branch is in multiple stacks) |
| 7 | Rebase already in progress |
| 8 | Store locked by another `stack` / `pool` process |

## Storage

```
$XDG_DATA_HOME/git-stack/
└── repos/
    ├── github.com_OWNER_REPO/
    │   ├── stacks.json              (stacks only)
    │   ├── pools.json                (pools only)
    │   ├── stacks.json.lock         (advisory lock, fd-lock; guards both files)
    │   └── stacks.json.rebase       (only present mid-rebase)
    └── local_<sha256-prefix>/
        └── ...
```

- Override the root with `GIT_STACK_HOME=/some/path` (used by the test suite, useful for sandboxing).
- Stacks and pools live in **separate JSON files** so the two tools can evolve independently. Either file is absent on disk until something is written to it.
- `stacks.json` is byte-compatible with gh-stack's `.git/gh-stack` file, so `stack migrate` is a one-shot copy. `pools.json` is a new file with its own schema (same `schemaVersion`/`repository` framing).
- Writes are atomic (tmp-file + `rename`); concurrent `stack` and `pool` processes contend for the same advisory lock (`stacks.json.lock`) with a 5-second timeout, so a `stack rebase` and a `pool refresh` won't interleave writes.

Identity resolution:

1. `git remote get-url origin` → if it's a GitHub URL, canonicalise to `github.com:OWNER/REPO` and slug-ify to `github.com_OWNER_REPO`.
2. Otherwise, sha256 the absolute path of the git common-dir and use `local_<prefix>`.

So worktrees of one clone, and separate clones of the same GitHub repo, all converge on the same entry. That's the bug this project exists to fix.

## Architecture

Hexagonal:

- `domain/` — pure types (`Stack`, `Branch`, `Trunk`, `PullRequest`, `Pool`, `PoolBranch`, `PoolPullRequest`, `PoolFile`) and rules (prefix application, navigation, disambiguation, comment-baseline accounting).
- `store/` — JSON persistence, file locking, atomic tmp-rename writes, identity resolution. Stacks and pools share the same lock but live in separate files (`stacks.json` and `pools.json`).
- `git/` — `GitOps` trait + production adapter that shells out to the `git` binary + `FakeGit` for tests.
- `github/` — `GitHub` trait + `GhCli` adapter that shells out to `gh` + `FakeGitHub` for tests. The port exposes PR view, head-ref lookup (used by both stack refresh and pool refresh to discover PRs opened outside the tool), and `@me` listing (for `pool adopt`).
- `ops/` — one module per command, taking the ports as parameters so orchestration is unit-testable. Pool ops live under `ops/pool/`.
- `style/` — tiny SGR helper with TTY + `NO_COLOR` detection and a narrow four-colour palette.
- `cli/` — `clap` definitions; a thin layer over `ops/`. `cli::run` is the `stack` entry point; `cli::pool::run` is the `pool` entry point.

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

The CLI covers the full gh-stack surface plus `migrate`, `list`, `drop`, `remove`, `switch`, worktree-aware `init`, opt-in `view --refresh`, and the entire pool toolset. Known limitations:

Stack-specific:

1. **Linear stacks only** — no branching stacks (multiple children per parent).
2. **No GitHub "Stack" linking** — PRs are chained via their `base` refs, which is what reviewers actually see. The undocumented `linkPullRequestsAsStack` GraphQL mutation is out of scope.
3. **`stack checkout <pr-number>` walks one PR at a time** — for multi-PR stacks, check out the bottommost PR.
4. **Merging happens in the GitHub UI** — the CLI never merges PRs.
5. **PR titles are auto-generated** — use `gh pr edit` to customise after `submit`.

Pool-specific:

1. **No automatic PR creation** — `pool` tracks existing PRs but doesn't open them. Use `gh pr create` (or `stack submit` for stacked work) first; `pool refresh` then discovers the PR by head ref.
2. **Adoption requires a local branch** — `pool adopt` skips PRs whose head branch doesn't exist locally. Run `gh pr checkout` first if you want one of those tracked.
3. **Merged-base reparenting walks at most five hops** — should be plenty for any realistic chain, but a deeper graph won't be fully resolved in a single refresh.

## License

MIT.
