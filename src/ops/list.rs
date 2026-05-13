use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::domain::{Stack, StackError};

use super::Context;

#[derive(Serialize)]
struct ListOutput<'a> {
    repository: &'a str,
    stacks: Vec<StackSummary<'a>>,
}

#[derive(Serialize)]
struct StackSummary<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    prefix: Option<&'a str>,
    trunk: &'a str,
    #[serde(rename = "branchCount")]
    branch_count: usize,
    #[serde(rename = "activeCount")]
    active_count: usize,
    branches: Vec<&'a str>,
    #[serde(rename = "isCurrent")]
    is_current: bool,
    /// Absolute path of a worktree associated with this stack, if any.
    /// `None` when no branch in the stack has any worktree at all.
    /// Path may refer to a directory that has been deleted on disk; see
    /// `worktreeMissing` to disambiguate.
    #[serde(skip_serializing_if = "Option::is_none")]
    worktree: Option<String>,
    /// `true` when the worktree path is what git still tracks but the
    /// directory has been removed. Run `stack prune` to clean up.
    #[serde(rename = "worktreeMissing", skip_serializing_if = "is_false")]
    worktree_missing: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

pub fn run(json: bool) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let (ctx, file) = Context::with_file(&cwd)?;
    let current = ctx.git.current_branch()?;
    let current_ref = current.as_deref();

    // Map branch name -> absolute worktree path, from `git worktree list`.
    // git reports paths it knows about, but the directory may have been
    // deleted out from under it (`rm -rf`). We disambiguate live vs missing
    // at the call site below.
    let worktrees: HashMap<String, PathBuf> = ctx
        .git
        .worktrees()
        .unwrap_or_default()
        .into_iter()
        .map(|(p, b)| (b, p))
        .collect();

    /// What we know about a stack's worktree, after checking the filesystem.
    #[derive(Debug, Clone)]
    enum WorktreeStatus {
        Live(PathBuf),
        Missing(PathBuf),
        None,
    }

    let resolve = |path: &PathBuf| -> WorktreeStatus {
        if path.exists() {
            WorktreeStatus::Live(path.clone())
        } else {
            WorktreeStatus::Missing(path.clone())
        }
    };

    // Resolve a worktree for a stack:
    //   1. A LIVE worktree whose path the cwd lives under.
    //   2. Any LIVE worktree of any branch in the stack.
    //   3. Any KNOWN-BUT-MISSING worktree (so the user sees the stale path
    //      and can `git worktree prune`).
    //   4. None.
    let pick_worktree = |s: &Stack| -> WorktreeStatus {
        // (1) cwd ownership, but only if it's live.
        if let Some((b, p)) = worktrees.iter().find(|(_, p)| cwd.starts_with(p)) {
            if s.contains(b) && p.exists() {
                return WorktreeStatus::Live(p.clone());
            }
        }
        // (2) and (3) walked together.
        let mut missing_fallback: Option<PathBuf> = None;
        for b in &s.branches {
            if let Some(p) = worktrees.get(&b.branch) {
                match resolve(p) {
                    WorktreeStatus::Live(p) => return WorktreeStatus::Live(p),
                    WorktreeStatus::Missing(p) if missing_fallback.is_none() => {
                        missing_fallback = Some(p);
                    }
                    _ => {}
                }
            }
        }
        match missing_fallback {
            Some(p) => WorktreeStatus::Missing(p),
            None => WorktreeStatus::None,
        }
    };

    if json {
        let out = ListOutput {
            repository: &file.repository,
            stacks: file
                .stacks
                .iter()
                .map(|s| {
                    let (worktree, worktree_missing) = match pick_worktree(s) {
                        WorktreeStatus::Live(p) => (Some(p.to_string_lossy().into_owned()), false),
                        WorktreeStatus::Missing(p) => {
                            (Some(p.to_string_lossy().into_owned()), true)
                        }
                        WorktreeStatus::None => (None, false),
                    };
                    StackSummary {
                        prefix: s.prefix.as_deref(),
                        trunk: &s.trunk.branch,
                        branch_count: s.branches.len(),
                        active_count: s.active_branches().len(),
                        branches: s.branches.iter().map(|b| b.branch.as_str()).collect(),
                        is_current: current_ref.is_some_and(|c| s.contains(c)),
                        worktree,
                        worktree_missing,
                    }
                })
                .collect(),
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    use crate::style::{accent, bold, dim, glyph, secondary, warn};

    if file.stacks.is_empty() {
        println!(
            "  {}",
            dim(&format!("no stacks tracked for {}", file.repository))
        );
        return Ok(());
    }

    let count = file.stacks.len();
    let sep = dim("·");
    println!(
        "  {}  {sep}  {}",
        bold(&file.repository),
        dim(&format!(
            "{count} stack{}",
            if count == 1 { "" } else { "s" }
        )),
    );
    println!();

    // Pre-compute worktree status per stack so we can width-align and
    // colour-flag the path column.
    let statuses: Vec<WorktreeStatus> = file.stacks.iter().map(&pick_worktree).collect();
    // Raw text (for width measurement) and pre-styled (for printing) in
    // parallel. The styled string includes ANSI escapes so we measure
    // against the raw form to keep alignment correct.
    let path_cells: Vec<(String, String)> = statuses
        .iter()
        .map(|st| match st {
            WorktreeStatus::Live(p) => {
                let s = shorten_home(p);
                (s.clone(), secondary(&s))
            }
            WorktreeStatus::Missing(p) => {
                let s = format!("{} (missing)", shorten_home(p));
                let styled = format!("{} {}", secondary(&shorten_home(p)), warn("(missing)"));
                (s, styled)
            }
            WorktreeStatus::None => {
                let s = "(no worktree)".to_string();
                (s.clone(), dim(&s))
            }
        })
        .collect();

    let widest_label = file
        .stacks
        .iter()
        .map(|s| s.prefix.as_deref().unwrap_or("(no prefix)").len())
        .max()
        .unwrap_or(0);
    let widest_trunk = file
        .stacks
        .iter()
        .map(|s| s.trunk.branch.len())
        .max()
        .unwrap_or(0);
    let widest_path = path_cells
        .iter()
        .map(|(raw, _)| raw.len())
        .max()
        .unwrap_or(0);

    for (s, (raw_path, styled_path)) in file.stacks.iter().zip(path_cells.iter()) {
        let is_current = current_ref.is_some_and(|c| s.contains(c));
        let marker = if is_current {
            accent(glyph::CURRENT)
        } else {
            dim(glyph::ACTIVE)
        };
        let label_raw = s.prefix.as_deref().unwrap_or("(no prefix)");
        let label = if is_current {
            accent(label_raw)
        } else {
            label_raw.to_string()
        };
        let label_pad = " ".repeat(widest_label.saturating_sub(label_raw.len()));
        // Two tiers: `dim` for the word "trunk" (chrome), `secondary` for
        // the actual branch name (data the user might act on).
        let trunk = format!("{} {}", dim("trunk"), secondary(&s.trunk.branch));
        let trunk_pad = " ".repeat(widest_trunk.saturating_sub(s.trunk.branch.len()));

        let path_pad = " ".repeat(widest_path.saturating_sub(raw_path.len()));

        let active = s.active_branches().len();
        let total = s.branches.len();
        let merged = total - active;
        let summary = if merged > 0 {
            format!(
                "{} {} ({} {}, {} {})",
                secondary(&total.to_string()),
                dim(if total == 1 { "branch" } else { "branches" }),
                secondary(&active.to_string()),
                dim("active"),
                secondary(&merged.to_string()),
                dim("merged"),
            )
        } else {
            format!(
                "{} {}",
                secondary(&total.to_string()),
                dim(if total == 1 { "branch" } else { "branches" }),
            )
        };
        println!(
            "  {marker}  {label}{label_pad}    {trunk}{trunk_pad}    {styled_path}{path_pad}    {summary}"
        );
    }
    Ok(())
}

/// Replace a leading `$HOME` with `~`. Falls back to the absolute path
/// when `$HOME` isn't available or the path doesn't live under it.
fn shorten_home(p: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = p.strip_prefix(&home) {
            let mut s = String::from("~");
            if !rest.as_os_str().is_empty() {
                s.push('/');
                s.push_str(&rest.to_string_lossy());
            }
            return s;
        }
    }
    p.to_string_lossy().into_owned()
}
