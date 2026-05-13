//! `stack sync` — refresh PR state, fetch trunk, cascade rebase, push.
//!
//! The PR-state refresh comes first, deliberately. Without it the cascade
//! rebase plans against stale `base` SHAs and tries to replay commits that
//! have already been squash-merged into trunk under different SHAs — see
//! https://github.com/flashingpumpkin/git-stack for the bug that motivated
//! this ordering.

use crate::domain::StackError;

use super::{pr_refresh, rebase, Context};

pub struct SyncArgs {
    pub remote: String,
    pub no_refresh: bool,
}

pub fn run(args: SyncArgs) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let mut cs = Context::current_stack(&cwd)?;

    pr_refresh::refresh_unless(args.no_refresh, &cwd, &cs.ctx, &mut cs.file, cs.stack_index)?;

    cs.ctx.git.fetch(&args.remote)?;
    eprintln!("✓ fetched from {}", args.remote);

    let trunk = cs.stack().trunk.branch.clone();
    let ff = cs.ctx.git.fast_forward(&trunk, &args.remote)?;
    if ff {
        let new_head = cs.ctx.git.rev_parse(&trunk)?;
        cs.stack_mut().trunk.head = new_head;
        cs.save()?;
        eprintln!("✓ trunk {trunk} fast-forwarded");
    } else {
        eprintln!("✓ trunk {trunk} already up to date");
    }

    // Drop the lock-holding store guard before re-entering ops::rebase (which
    // re-opens the store with its own lock). We've already persisted everything.
    drop(cs);

    // We already refreshed; tell rebase to skip its own refresh.
    rebase::run(rebase::RebaseArgs {
        scope: rebase::Scope::Full,
        cont: false,
        abort: false,
        remote: args.remote.clone(),
        no_refresh: true,
    })?;

    super::push::run(&args.remote)?;
    eprintln!("✓ stack synced");
    Ok(())
}
