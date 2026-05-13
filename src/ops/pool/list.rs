//! `pool list` — show every pool with each branch's PR state, rebase
//! status, and unread-comment count.
//!
//! `-r/--refresh` first hits GitHub via the same path as `pool refresh`,
//! then renders. After rendering, the seen-comment baseline is advanced
//! so the next list won't repeat the same indicators.

use serde::Serialize;

use crate::clock::humanise_age;
use crate::domain::{Pool, StackError};
use crate::github::GhCli;

use crate::ops::Context;

pub struct ListArgs {
    pub json: bool,
    pub refresh: bool,
}

#[derive(Serialize)]
struct ListOutput<'a> {
    repository: &'a str,
    pools: Vec<PoolOut<'a>>,
}

#[derive(Serialize)]
struct PoolOut<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    trunk: &'a str,
    #[serde(rename = "lastRefreshedAt", skip_serializing_if = "Option::is_none")]
    last_refreshed_at: Option<&'a str>,
    branches: Vec<BranchOut<'a>>,
}

#[derive(Serialize)]
struct BranchOut<'a> {
    name: &'a str,
    /// Ref name of the branch this one rebases onto.
    base: &'a str,
    #[serde(rename = "needsRebase")]
    needs_rebase: bool,
    #[serde(rename = "isMerged")]
    is_merged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr: Option<PrOut<'a>>,
}

#[derive(Serialize)]
struct PrOut<'a> {
    number: u64,
    url: &'a str,
    state: &'static str,
    #[serde(rename = "commentCount")]
    comment_count: u64,
    #[serde(rename = "unreadComments")]
    unread_comments: u64,
    /// Mergeable tri-state: `null` (unknown), `true`, or `false`. Serialised
    /// explicitly (no skip_if) so consumers can distinguish "unknown"
    /// from "field absent".
    mergeable: Option<bool>,
}

pub fn run(args: ListArgs) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let ctx = Context::open(&cwd)?;
    let mut file = ctx.store.load_pools(&ctx.identity)?;

    // Optional refresh pass. Per spec, list -r should mirror stack view -r:
    // pull live PR state, then display.
    if args.refresh {
        let gh = GhCli::new(&cwd);
        for idx in 0..file.pools.len() {
            super::refresh::refresh_pool(&gh, &mut file, idx)?;
            let _dropped = super::refresh::drop_merged(&mut file, idx);
        }
        ctx.store.save_pools(&file)?;
    }

    if args.json {
        emit_json(&ctx, &file)?;
    } else {
        emit_text(&ctx, &file)?;
    }

    // After presenting unread indicators, advance the seen baseline so the
    // next invocation doesn't repeat them. We do this regardless of
    // --refresh — even a purely local list represents "the user looked".
    for idx in 0..file.pools.len() {
        super::refresh::mark_seen(&mut file, idx);
    }
    ctx.store.save_pools(&file)?;
    Ok(())
}

fn emit_json(ctx: &Context, file: &crate::domain::PoolFile) -> Result<(), StackError> {
    let mut pools_out = Vec::with_capacity(file.pools.len());
    for pool in &file.pools {
        let mut branches_out = Vec::with_capacity(pool.branches.len());
        for b in &pool.branches {
            let base_branch_name = b.effective_base_branch(pool);
            // Compare against the live head of each branch's own base —
            // pool members are independent and can sit on different
            // upstreams.
            let needs_rebase = match ctx.git.rev_parse(base_branch_name) {
                Ok(live) => !b.is_merged() && live != b.base,
                Err(_) => false,
            };
            branches_out.push(BranchOut {
                name: &b.branch,
                base: base_branch_name,
                needs_rebase,
                is_merged: b.is_merged(),
                pr: b.pull_request.as_ref().map(|p| PrOut {
                    number: p.number,
                    url: &p.url,
                    state: if p.merged { "MERGED" } else { "OPEN" },
                    comment_count: p.comment_count,
                    unread_comments: p.comment_count.saturating_sub(p.seen_comment_count),
                    mergeable: p.mergeable,
                }),
            });
        }
        pools_out.push(PoolOut {
            name: pool.name.as_deref(),
            trunk: &pool.trunk.branch,
            last_refreshed_at: pool.last_refreshed_at.as_deref(),
            branches: branches_out,
        });
    }
    let out = ListOutput {
        repository: &file.repository,
        pools: pools_out,
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn emit_text(ctx: &Context, file: &crate::domain::PoolFile) -> Result<(), StackError> {
    use crate::style::{
        accent, bold, dim, glyph, merged as merged_style, ok, secondary, url, warn,
    };

    if file.pools.is_empty() {
        println!(
            "  {}",
            dim(&format!("no pools tracked for {}", file.repository))
        );
        return Ok(());
    }

    let sep = dim("·");
    println!(
        "  {}  {sep}  {}",
        bold(&file.repository),
        dim(&format!(
            "{} pool{}",
            file.pools.len(),
            if file.pools.len() == 1 { "" } else { "s" }
        )),
    );

    for pool in &file.pools {
        println!();
        let freshness = match pool.last_refreshed_at.as_deref() {
            Some(ts) => dim(&format!("refreshed {}", humanise_age(ts))),
            None => dim("never refreshed (`pool refresh`)"),
        };
        println!(
            "  {}  {sep}  {} {}  {sep}  {}",
            bold(pool_label(pool)),
            dim("default base"),
            secondary(&pool.trunk.branch),
            freshness,
        );

        if pool.branches.is_empty() {
            println!("  {}", dim("(empty pool)"));
            continue;
        }

        let widest = pool
            .branches
            .iter()
            .map(|b| b.branch.len())
            .max()
            .unwrap_or(0);

        for b in &pool.branches {
            let base_branch_name = b.effective_base_branch(pool);
            // Resolve the live head of each branch's own base. Pool
            // members are independent, so we can't share a single trunk
            // SHA across them.
            let needs_rebase = match ctx.git.rev_parse(base_branch_name) {
                Ok(live) => !b.is_merged() && live != b.base,
                Err(_) => false,
            };
            let marker = if b.is_merged() {
                merged_style(glyph::MERGED)
            } else {
                dim(glyph::ACTIVE)
            };
            let name_styled = if b.is_merged() {
                merged_style(&b.branch)
            } else {
                b.branch.clone()
            };
            let pad = " ".repeat(widest.saturating_sub(b.branch.len()));

            let base_part = format!("{} {}", dim("→"), secondary(base_branch_name),);

            let pr_part = match &b.pull_request {
                Some(p) => {
                    let state_word = if p.merged {
                        merged_style("merged")
                    } else {
                        ok("open")
                    };
                    format!("{}  {}", secondary(&format!("#{}", p.number)), state_word)
                }
                None => dim("(no PR)").to_string(),
            };

            let rebase_tag = if needs_rebase {
                format!("  {} {}", warn(glyph::WARN), warn("needs rebase"))
            } else {
                String::new()
            };

            // Surface PR merge-conflict state. Rebase and sync will both
            // stall on these, so we want them flagged before the user
            // kicks off either command.
            let conflict_tag = match b.pull_request.as_ref().and_then(|p| p.mergeable) {
                Some(false) => format!("  {} {}", warn(glyph::WARN), warn("conflicts")),
                _ => String::new(),
            };

            let unread = b.unread_comments();
            let unread_tag = if unread > 0 {
                format!(
                    "  {} {}",
                    accent("✉"),
                    accent(&format!(
                        "{unread} new comment{}",
                        if unread == 1 { "" } else { "s" }
                    )),
                )
            } else {
                String::new()
            };

            println!(
                "  {marker}  {name_styled}{pad}    {base_part}    {pr_part}{rebase_tag}{conflict_tag}{unread_tag}"
            );
            if let Some(pr) = &b.pull_request {
                println!("  {}     {}", dim(glyph::CHAIN), url(&pr.url));
            }
        }
    }
    Ok(())
}

fn pool_label(p: &Pool) -> &str {
    p.display_label()
}
