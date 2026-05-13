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

    use crate::style::{accent, bold, dim, glyph, merged, ok, secondary};

    if file.stacks.is_empty() {
        println!(
            "  {}",
            dim(&format!("no stacks tracked for {}", file.repository))
        );
        return Ok(());
    }

    // Most repos have one canonical trunk and every stack tracks it. Show
    // it once on the repo line and suppress it from headers when it matches
    // — saves a redundant column on every stack row. If trunks diverge,
    // the repo line drops the trunk fragment and each header carries it.
    let shared_trunk = {
        let first = file.stacks[0].trunk.branch.as_str();
        file.stacks
            .iter()
            .all(|s| s.trunk.branch == first)
            .then(|| first.to_string())
    };
    let count = file.stacks.len();
    let sep = dim("·");
    let header_tail = match &shared_trunk {
        Some(t) => format!("  {sep}  {} {}", dim("trunk"), secondary(t)),
        None => String::new(),
    };
    println!(
        "  {}  {sep}  {}{}",
        bold(&file.repository),
        dim(&format!(
            "{count} stack{}",
            if count == 1 { "" } else { "s" }
        )),
        header_tail,
    );
    println!();

    for s in &file.stacks {
        // Header label: prefix verbatim, or "(no prefix)" placeholder.
        let label_raw = s.prefix.as_deref().unwrap_or("(no prefix)");
        let label = dim(label_raw);

        // Right-hand slot precedence:
        //   1. live worktree path
        //   2. missing worktree path (warn-marked)
        //   3. branch-count fragment if something interesting (merged tail
        //      or >1 branch) to flag
        //   4. fall through to "(no worktree)"
        let wt = pick_worktree(s);
        let active = s.active_branches().len();
        let total = s.branches.len();
        let merged_count = total - active;
        let right_slot = match wt {
            WorktreeStatus::Live(ref p) => secondary(&shorten_home(p)),
            WorktreeStatus::Missing(ref p) => {
                format!("{} {}", secondary(&shorten_home(p)), dim("(missing)"))
            }
            WorktreeStatus::None => {
                if merged_count > 0 {
                    if total == 1 {
                        format!(
                            "{} {}  {} {}",
                            secondary(&total.to_string()),
                            dim("branch"),
                            secondary(&merged_count.to_string()),
                            dim("merged"),
                        )
                    } else {
                        format!(
                            "{} {}  {} {}",
                            secondary(&total.to_string()),
                            dim("branches"),
                            secondary(&merged_count.to_string()),
                            dim("merged"),
                        )
                    }
                } else if total > 1 {
                    format!("{} {}", secondary(&total.to_string()), dim("branches"),)
                } else {
                    dim("(no worktree)")
                }
            }
        };

        // Per-stack header trunk fragment: only when this stack's trunk
        // differs from the repo-wide shared trunk (or there is no shared one).
        let trunk_fragment = match &shared_trunk {
            Some(t) if t == &s.trunk.branch => String::new(),
            _ => format!("  {sep}  {} {}", dim("trunk"), secondary(&s.trunk.branch)),
        };

        // Header line. Two tabs-worth of gap between left (label) and right
        // (worktree / counts). No global column alignment — each stack is
        // its own block.
        println!("{label}{trunk_fragment}        {right_slot}");
        println!();

        // Per-stack branch block: top-to-bottom (newest layer first).
        // The chain (`│`) connects branch dots, and runs through the URL
        // row at column 3 with the URL itself sitting much further right
        // so it never breaks the vertical line.
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
            // Branch name pops: bold on current, default-bright otherwise,
            // soft-grey only when merged.
            let name_styled = if is_current_branch {
                accent(&name_raw)
            } else if b.is_merged() {
                merged(&name_raw)
            } else {
                bold(&name_raw)
            };
            let name_pad = " ".repeat(widest_name.saturating_sub(name_raw.len()));
            let pr_part = match &b.pull_request {
                Some(p) => {
                    let state_word = if p.merged {
                        merged("merged")
                    } else {
                        ok("open")
                    };
                    format!("{}  {}", dim(&format!("#{}", p.number)), state_word)
                }
                None => dim("(no PR)").to_string(),
            };
            println!("  {glyph_str}  {name_styled}{name_pad}    {pr_part}");
            if let Some(p) = &b.pull_request {
                // The chain glyph sits at the same column as the branch
                // dot; the URL sits far right (under the PR slot) so the
                // vertical line stays unbroken. URL is the deepest dim tier.
                let url_indent = 4 + widest_name + 4;
                println!(
                    "  {}{}{}",
                    dim(glyph::CHAIN),
                    " ".repeat(url_indent.saturating_sub(1)),
                    dim(&p.url),
                );
            }
        }
        // Anchor each stack to its trunk explicitly. Consistent with
        // `stack view`'s `└─ main` footer.
        println!("  {} {}", dim(glyph::ELBOW), dim(&s.trunk.branch));
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
