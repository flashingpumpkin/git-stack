//! `pool prune` — drop branches whose PRs have been merged.
//!
//! Mirrors `stack prune`: refresh PR state first (default; opt out with
//! `--no-refresh`), then drop every branch in every pool whose PR is now
//! `merged`. `--dry-run` reports what would be removed without writing.
//!
//! `pool refresh` is intentionally non-destructive — it surfaces what
//! changed and leaves merged branches visible. Prune is the cleanup step.

use crate::domain::StackError;

use crate::ops::Context;

pub struct PruneArgs {
    pub pool: Option<String>,
    pub dry_run: bool,
    pub no_refresh: bool,
}

pub fn run(args: PruneArgs) -> Result<(), StackError> {
    use crate::style::dim;

    let cwd = std::env::current_dir()?;
    let ctx = Context::open(&cwd)?;
    let mut file = ctx.store.load_pools(&ctx.identity)?;

    // Determine target pools: one if --pool given, else every pool.
    let pool_indices: Vec<usize> = match &args.pool {
        Some(_) => vec![super::resolve_pool_index(&file, args.pool.as_deref())?],
        None => (0..file.pools.len()).collect(),
    };

    if !args.no_refresh {
        let gh = crate::github::GhCli::new(&cwd);
        for &idx in &pool_indices {
            let _ = super::refresh::refresh_pool(&gh, &mut file, idx)?;
        }
        // Persist refreshed state even when we're about to drop nothing
        // (so the user's next dry-run reflects the latest data) — unless
        // this is a dry-run itself, in which case we don't write at all.
        if !args.dry_run {
            ctx.store.save_pools(&file)?;
        }
    }

    let mut total_dropped: usize = 0;
    for &idx in &pool_indices {
        let label = file.pools[idx].display_label().to_string();
        // Collect names first so we can report them whether or not we save.
        let to_drop: Vec<String> = file.pools[idx]
            .branches
            .iter()
            .filter(|b| b.is_merged())
            .map(|b| b.branch.clone())
            .collect();
        if to_drop.is_empty() {
            continue;
        }
        let verb = if args.dry_run {
            "would drop"
        } else {
            "✓ dropped"
        };
        eprintln!("{verb} {} branch(es) from pool {label}:", to_drop.len());
        for name in &to_drop {
            eprintln!("  {}  {name}", dim("○"));
        }
        if !args.dry_run {
            super::refresh::drop_merged(&mut file, idx);
        }
        total_dropped += to_drop.len();
    }

    if total_dropped == 0 {
        eprintln!("✓ no merged branches to prune");
    }

    if !args.dry_run {
        ctx.store.save_pools(&file)?;
    }
    Ok(())
}
