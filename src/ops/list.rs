use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::domain::{StackError, WorktreeStatus};

use super::Context;

pub struct ListArgs {
    pub json: bool,
    pub refresh: bool,
}

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
    branches: Vec<BranchSummary<'a>>,
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

#[derive(Serialize)]
struct BranchSummary<'a> {
    name: &'a str,
    #[serde(rename = "isMerged")]
    is_merged: bool,
    #[serde(rename = "isCurrent")]
    is_current: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr: Option<PrSummary<'a>>,
}

#[derive(Serialize)]
struct PrSummary<'a> {
    number: u64,
    url: &'a str,
    state: &'static str,
}

fn is_false(b: &bool) -> bool {
    !*b
}

pub fn run(args: ListArgs) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let (ctx, mut file) = Context::with_file(&cwd)?;

    if args.refresh {
        let gh = crate::github::GhCli::new(&cwd);
        let total = crate::ops::pr_refresh::refresh_all(&gh, &mut file)?;
        ctx.store.save(&file)?;
        if total > 0 {
            eprintln!("ℹ {total} PR(s) merged on GitHub since last refresh");
        }
    }

    let current = ctx.git.current_branch()?;
    let current_ref = current.as_deref();

    // git reports paths it knows about, but the directory may have been
    // deleted out from under it (`rm -rf`). `Stack::resolve_worktree`
    // disambiguates live vs missing.
    let worktrees: Vec<(PathBuf, String)> = ctx.git.worktrees().unwrap_or_default();
    let pick_worktree =
        |s: &crate::domain::Stack| -> WorktreeStatus { s.resolve_worktree(&worktrees, &cwd) };

    if args.json {
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
                        branches: s
                            .branches
                            .iter()
                            .map(|b| BranchSummary {
                                name: &b.branch,
                                is_merged: b.is_merged(),
                                is_current: current_ref == Some(&b.branch),
                                pr: b.pull_request.as_ref().map(|p| PrSummary {
                                    number: p.number,
                                    url: &p.url,
                                    state: if p.merged { "MERGED" } else { "OPEN" },
                                }),
                            })
                            .collect(),
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

    use crate::style::{accent, bold, dim, glyph, merged, ok, secondary, url as url_style, warn};

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
        let merged_count = total - active;
        let summary = if merged_count > 0 {
            format!(
                "{} {} ({} {}, {} {})",
                secondary(&total.to_string()),
                dim(if total == 1 { "branch" } else { "branches" }),
                secondary(&active.to_string()),
                dim("active"),
                secondary(&merged_count.to_string()),
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

        // Per-stack branch block: top-to-bottom (newest layer first),
        // current branch accented, PR number + state + URL beside each.
        let display_name = |bn: &str| -> String {
            match &s.prefix {
                Some(p) => bn
                    .strip_prefix(p)
                    .and_then(|s| s.strip_prefix('/'))
                    .unwrap_or(bn)
                    .to_string(),
                None => bn.to_string(),
            }
        };
        let widest_name = s
            .branches
            .iter()
            .map(|b| display_name(&b.branch).len())
            .max()
            .unwrap_or(0);
        for b in s.branches.iter().rev() {
            let is_current_branch = current_ref == Some(&b.branch);
            let glyph_str = if is_current_branch {
                accent(glyph::CURRENT)
            } else if b.is_merged() {
                merged(glyph::MERGED)
            } else {
                dim(glyph::ACTIVE)
            };
            let name_raw = display_name(&b.branch);
            let name_styled = if is_current_branch {
                accent(&name_raw)
            } else if b.is_merged() {
                merged(&name_raw)
            } else {
                name_raw.clone()
            };
            let name_pad = " ".repeat(widest_name.saturating_sub(name_raw.len()));
            let pr_part = match &b.pull_request {
                Some(p) => {
                    let state_word = if p.merged {
                        merged("merged")
                    } else {
                        ok("open")
                    };
                    format!("{}  {}", secondary(&format!("#{}", p.number)), state_word)
                }
                None => dim("(no PR)").to_string(),
            };
            println!("       {glyph_str}  {name_styled}{name_pad}    {pr_part}");
            if let Some(p) = &b.pull_request {
                println!("       {}     {}", dim(glyph::CHAIN), url_style(&p.url));
            }
        }
        println!();
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
