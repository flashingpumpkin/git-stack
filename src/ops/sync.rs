//! `stack sync` — refresh PR state, fetch trunk, cascade rebase, push.
//!
//! The PR-state refresh comes first, deliberately. Without it the cascade
//! rebase plans against stale `base` SHAs and tries to replay commits that
//! have already been squash-merged into trunk under different SHAs.
//!
//! We deliberately do *not* fast-forward the local trunk ref. A `main`
//! checked out in another worktree (very common: the main clone has
//! `main`, the user runs `stack sync` from a feature worktree) would have
//! its tip moved under it, leaving its working tree out of sync with the
//! ref. Instead we just `git fetch` and let the cascade rebase onto the
//! remote-tracking ref (`<remote>/<trunk>`), which is per-clone and has
//! no working tree attached. This matches the manual workflow of
//! `git pull -r origin main` from a feature branch.

use crate::domain::StackError;

use super::{pr_refresh, rebase, Context};

pub struct SyncArgs {
    pub remote: String,
    pub no_refresh: bool,
}

pub fn run(args: SyncArgs) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let mut cs = Context::current_stack(&cwd)?;

    if !args.no_refresh {
        let gh = crate::github::GhCli::new(&cwd);
        let n = pr_refresh::refresh_stack(&gh, &mut cs.file, cs.stack_index)?;
        cs.save()?;
        if n > 0 {
            eprintln!("ℹ {n} PR(s) merged on GitHub since last refresh");
        }
    }

    cs.ctx.git.fetch(&args.remote)?;
    eprintln!("✓ fetched from {}", args.remote);

    // Drop the lock-holding store guard before re-entering ops::rebase
    // (which re-opens the store with its own lock).
    drop(cs);

    // Cascade rebase onto `<remote>/<trunk>`. We already refreshed; tell
    // rebase to skip its own refresh.
    rebase::run(rebase::RebaseArgs {
        scope: rebase::Scope::Full,
        cont: false,
        abort: false,
        remote: args.remote.clone(),
        no_refresh: true,
    })?;

    super::push_active(&args.remote)?;
    eprintln!("✓ stack synced");
    Ok(())
}
