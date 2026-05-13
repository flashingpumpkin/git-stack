use serde::Serialize;

use crate::clock::humanise_age;
use crate::domain::{Branch, PullRequest, Stack, StackError};
use crate::git::Git;

use super::Context;

#[derive(Serialize)]
struct ViewOutput<'a> {
    trunk: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    prefix: Option<&'a str>,
    #[serde(rename = "currentBranch")]
    current_branch: Option<String>,
    #[serde(rename = "lastRefreshedAt", skip_serializing_if = "Option::is_none")]
    last_refreshed_at: Option<&'a str>,
    branches: Vec<BranchOut<'a>>,
}

#[derive(Serialize)]
struct BranchOut<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    head: Option<String>,
    base: &'a str,
    #[serde(rename = "isCurrent")]
    is_current: bool,
    #[serde(rename = "isMerged")]
    is_merged: bool,
    #[serde(rename = "needsRebase")]
    needs_rebase: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr: Option<PrOut<'a>>,
}

#[derive(Serialize)]
struct PrOut<'a> {
    number: u64,
    url: &'a str,
    state: &'static str,
}

pub struct ViewArgs {
    pub json: bool,
    pub refresh: bool,
}

pub fn run(args: ViewArgs) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let ctx = Context::open(&cwd)?;
    let mut file = ctx.store.load(&ctx.identity)?;
    let current = ctx.git.current_branch()?;
    let current_ref = current.as_deref().unwrap_or("");
    let stack_index = file
        .stacks
        .iter()
        .position(|s| s.contains(current_ref))
        .ok_or(StackError::NotInStack)?;

    crate::ops::pr_refresh::refresh_unless(!args.refresh, &cwd, &ctx, &mut file, stack_index)?;

    let stack = &file.stacks[stack_index];
    if args.json {
        emit_json(&ctx.git, stack, current.as_deref())?;
    } else {
        emit_text(&ctx.git, stack, current.as_deref())?;
    }
    Ok(())
}

fn pr_out(pr: &PullRequest) -> PrOut<'_> {
    PrOut {
        number: pr.number,
        url: &pr.url,
        state: if pr.merged { "MERGED" } else { "OPEN" },
    }
}

fn branch_out<'a>(
    b: &'a Branch,
    status: &super::walk::BranchStatus,
    current: Option<&str>,
) -> BranchOut<'a> {
    BranchOut {
        name: &b.branch,
        head: status.live_head.clone(),
        base: &b.base,
        is_current: current.map(|c| c == b.branch).unwrap_or(false),
        is_merged: b.is_merged(),
        needs_rebase: status.needs_rebase,
        pr: b.pull_request.as_ref().map(pr_out),
    }
}

fn emit_json(git: &Git, stack: &Stack, current: Option<&str>) -> Result<(), StackError> {
    let statuses = super::walk::walk(git, stack)?;
    let out = ViewOutput {
        trunk: &stack.trunk.branch,
        prefix: stack.prefix.as_deref(),
        current_branch: current.map(|s| s.to_string()),
        last_refreshed_at: stack.last_refreshed_at.as_deref(),
        branches: stack
            .branches
            .iter()
            .zip(statuses.iter())
            .map(|(b, s)| branch_out(b, s, current))
            .collect(),
    };
    let s = serde_json::to_string_pretty(&out)?;
    println!("{s}");
    Ok(())
}

fn emit_text(git: &Git, stack: &Stack, current: Option<&str>) -> Result<(), StackError> {
    use crate::style::{accent, bold, dim, glyph, merged, ok, secondary, url, warn};
    let statuses = super::walk::walk(git, stack)?;
    let needs_for = |name: &str| -> bool {
        statuses
            .iter()
            .find(|s| s.branch == name)
            .map(|s| s.needs_rebase)
            .unwrap_or(false)
    };

    // Header: prefix · trunk · freshness. Separators and labels in chrome
    // grey; the actual trunk branch name in the brighter secondary tier
    // since it's data (you might branch off it, sync to it, etc.).
    let label = stack.prefix.as_deref().unwrap_or("(no prefix)");
    let sep = dim("·");
    let freshness = match stack.last_refreshed_at.as_deref() {
        Some(ts) => dim(&format!("refreshed {}", humanise_age(ts))),
        None => dim("PR state never refreshed (`stack view -r` to fetch)"),
    };
    println!(
        "  {}  {sep}  {} {}  {sep}  {}",
        bold(label),
        dim("trunk"),
        secondary(&stack.trunk.branch),
        freshness,
    );
    println!();

    // Partition: active (rendered top-to-bottom as the chain) and merged
    // (footer section). `stack.branches[0]` is the bottom; reverse to put
    // the top at the top of the screen.
    let mut actives: Vec<&Branch> = Vec::new();
    let mut mergeds: Vec<&Branch> = Vec::new();
    for b in stack.branches.iter().rev() {
        if b.is_merged() {
            mergeds.push(b);
        } else {
            actives.push(b);
        }
    }

    // Display name: strip the stack prefix so the eye scans suffixes only.
    let display_name = |b: &Branch| -> String {
        match &stack.prefix {
            Some(p) => b
                .branch
                .strip_prefix(p)
                .and_then(|s| s.strip_prefix('/'))
                .unwrap_or(&b.branch)
                .to_string(),
            None => b.branch.clone(),
        }
    };
    let widest_active = actives
        .iter()
        .map(|b| display_name(b).len())
        .max()
        .unwrap_or(0);

    // Active section: the chain. Each branch row, then an indented PR URL
    // beneath it on the same chain pipe. After the last active branch a
    // single `│` connects down to `└─ main`.
    for b in &actives {
        let is_current = current == Some(&b.branch);
        let marker = if is_current {
            accent(glyph::CURRENT)
        } else {
            dim(glyph::ACTIVE)
        };
        let name = display_name(b);
        let name_styled = if is_current {
            accent(&name)
        } else {
            name.clone()
        };

        let pr_part = match &b.pull_request {
            Some(p) => {
                let state_word = if p.merged {
                    merged("merged")
                } else {
                    ok("open")
                };
                // PR number is data (clickable in some terminals, copyable);
                // give it the brighter secondary grey rather than chrome dim.
                format!("{}  {}", secondary(&format!("#{}", p.number)), state_word)
            }
            None => dim("(no PR)").to_string(),
        };

        // The rebase warning is the only attention-grabbing element on the
        // active line. Reserve amber for it exclusively.
        let rebase_tag = if needs_for(&b.branch) {
            format!("  {} {}", warn(glyph::WARN), warn("needs rebase"))
        } else {
            String::new()
        };

        // Pad on raw name width (ANSI escapes don't count for layout).
        let pad = " ".repeat(widest_active.saturating_sub(name.len()));
        println!("  {marker}  {name_styled}{pad}    {pr_part}{rebase_tag}");
        if let Some(pr) = &b.pull_request {
            println!("  {}     {}", dim(glyph::CHAIN), url(&pr.url));
        }
    }

    // Anchor to trunk. The connector is chrome, the trunk branch name is
    // data (the same colour as the trunk shown in the header).
    if !actives.is_empty() {
        println!("  {}", dim(glyph::CHAIN));
        println!("  {} {}", dim(glyph::ELBOW), secondary(&stack.trunk.branch));
    } else {
        println!("  {} {}", dim(glyph::ELBOW), secondary(&stack.trunk.branch));
        println!("  {}", dim("(no active branches)"));
    }

    // Merged section.
    if !mergeds.is_empty() {
        println!();
        let title = format!("Merged ({})", mergeds.len());
        let rule = "─".repeat(60usize.saturating_sub(title.len() + 3));
        println!("  {} {}", dim(&title), dim(&rule));

        let widest_merged = mergeds
            .iter()
            .map(|b| display_name(b).len())
            .max()
            .unwrap_or(0);
        for b in &mergeds {
            let name = display_name(b);
            let pad = " ".repeat(widest_merged.saturating_sub(name.len()));
            let pr_n = b
                .pull_request
                .as_ref()
                .map(|p| format!("#{}", p.number))
                .unwrap_or_default();
            println!(
                "  {}  {}{pad}    {}",
                merged(glyph::MERGED),
                merged(&name),
                merged(&pr_n)
            );
        }
    }

    Ok(())
}
