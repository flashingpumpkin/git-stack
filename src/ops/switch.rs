//! `stack switch` — interactive branch picker for the current stack.
//!
//! A small `inquire` Select tuned to match the rest of the CLI's visual
//! vocabulary: the cursor is a cyan arrow, options show the same chain
//! markers as `stack view`, and PR state badges use the same colours.

use inquire::ui::{Attributes, Color, RenderConfig, StyleSheet, Styled};
use inquire::Select;

use crate::domain::{Branch, StackError};
use crate::style::{glyph, use_color};

use super::Context;

pub fn run() -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let ctx = Context::open(&cwd)?;
    let file = ctx.store.load(&ctx.identity)?;
    let current = ctx
        .git
        .current_branch()?
        .ok_or_else(|| StackError::Other("detached HEAD; check out a branch first".into()))?;
    let stack = file.current_stack(&current)?;

    // Top-of-stack first so the order matches `stack view`.
    let strip_prefix = stack.prefix.clone();
    let widest = stack
        .branches
        .iter()
        .map(|b| display_name(&strip_prefix, b).len())
        .max()
        .unwrap_or(0);

    let options: Vec<BranchChoice> = stack
        .branches
        .iter()
        .rev()
        .map(|b| BranchChoice::from_branch(b, &current, &strip_prefix, widest))
        .collect();

    if options.is_empty() {
        return Err(StackError::Other("stack has no branches".into()));
    }

    let starting_index = options
        .iter()
        .position(|o| o.branch_name == current)
        .unwrap_or(0);

    let header = match &stack.prefix {
        Some(p) => format!("Switch branch in {p}:"),
        None => "Switch branch:".to_string(),
    };
    let help = format!(
        "{arrow}{arrow} move   {enter} switch   {esc} cancel",
        arrow = "↑↓",
        enter = "↵",
        esc = "esc"
    );

    let mut prompt = Select::new(&header, options)
        .with_starting_cursor(starting_index)
        .with_help_message(&help)
        .with_page_size(12);
    if use_color() {
        prompt = prompt.with_render_config(render_config());
    }

    let choice = match prompt.prompt() {
        Ok(c) => c,
        Err(inquire::InquireError::OperationCanceled)
        | Err(inquire::InquireError::OperationInterrupted) => {
            eprintln!("ℹ cancelled");
            return Ok(());
        }
        Err(e) => return Err(StackError::Other(format!("picker failed: {e}"))),
    };

    if choice.branch_name == current {
        eprintln!("ℹ already on {current}");
        return Ok(());
    }
    ctx.git.checkout(&choice.branch_name)?;
    eprintln!("✓ switched to {}", choice.branch_name);
    Ok(())
}

fn display_name(prefix: &Option<String>, b: &Branch) -> String {
    match prefix {
        Some(p) => b
            .branch
            .strip_prefix(p)
            .and_then(|s| s.strip_prefix('/'))
            .unwrap_or(&b.branch)
            .to_string(),
        None => b.branch.clone(),
    }
}

/// Inquire RenderConfig mirroring our palette: bright cyan accents for the
/// selected row, dim grey for prompt chrome, sage for the prompt prefix.
fn render_config() -> RenderConfig<'static> {
    let dim = StyleSheet::new().with_fg(Color::DarkGrey);
    let accent = StyleSheet::new()
        .with_attr(Attributes::BOLD)
        .with_fg(Color::LightCyan);

    RenderConfig::default()
        .with_prompt_prefix(Styled::new(glyph::ARROW).with_style_sheet(accent))
        .with_answered_prompt_prefix(Styled::new(glyph::CURRENT).with_style_sheet(accent))
        .with_help_message(dim)
        .with_highlighted_option_prefix(Styled::new("›").with_style_sheet(accent))
        .with_selected_option(Some(accent))
        .with_canceled_prompt_indicator(Styled::new("(cancelled)").with_style_sheet(dim))
}

struct BranchChoice {
    branch_name: String,
    label: String,
}

impl BranchChoice {
    fn from_branch(b: &Branch, current: &str, prefix: &Option<String>, widest: usize) -> Self {
        // We can't selectively colour parts of an inquire option label per
        // hover state, so keep labels uncoloured and let inquire's
        // RenderConfig handle the highlight. Use markers + padding to carry
        // hierarchy.
        let marker = if b.branch == current {
            glyph::CURRENT
        } else if b.is_merged() {
            glyph::MERGED
        } else {
            glyph::ACTIVE
        };
        let name = display_name(prefix, b);
        let pad = " ".repeat(widest.saturating_sub(name.len()));
        let state = match (&b.pull_request, b.is_merged()) {
            (Some(p), true) => format!("#{} merged", p.number),
            (Some(p), false) => format!("#{} open", p.number),
            (None, _) => "(no PR)".to_string(),
        };
        Self {
            branch_name: b.branch.clone(),
            label: format!("{marker}  {name}{pad}    {state}"),
        }
    }
}

impl std::fmt::Display for BranchChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}
