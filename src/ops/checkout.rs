use crate::clock::now_rfc3339;
use crate::domain::{Branch, PullRequest, Stack, StackError, Trunk};
use crate::github::{GhCli, GitHub, PrState, PullRequestInfo};

use super::Context;

/// `stack checkout <branch>` for locally-tracked stacks, or
/// `stack checkout <pr-number>` to fetch a stack from GitHub.
pub fn run(target: &str) -> Result<(), StackError> {
    let cwd = std::env::current_dir()?;
    let ctx = Context::open(&cwd)?;

    if let Ok(pr_number) = target.parse::<u64>() {
        let gh = GhCli::new(&cwd);
        return checkout_pr(&ctx, &gh, pr_number);
    }

    let file = ctx.store.load(&ctx.identity)?;
    let matches = file.stacks_containing(target);
    match matches.len() {
        0 => Err(StackError::Other(format!(
            "branch `{target}` is not part of any locally-tracked stack"
        ))),
        1 => {
            ctx.git.checkout(target)?;
            eprintln!("✓ checked out {target}");
            Ok(())
        }
        _ => Err(StackError::Disambiguation(target.to_string())),
    }
}

/// Walk a PR's `base` chain on GitHub until we hit a branch whose base is
/// the trunk; that gives us the full stack. Fetch each branch locally,
/// store the stack, and check out the requested PR's branch.
pub fn checkout_pr(ctx: &Context, gh: &dyn GitHub, pr_number: u64) -> Result<(), StackError> {
    let trunk = ctx.git.trunk_branch()?;

    // Walk down from the requested PR.
    let mut chain: Vec<PullRequestInfo> = Vec::new();
    let mut next = Some(pr_number);
    while let Some(n) = next {
        let info = gh.view_pr(n)?;
        let base = info.base_ref.clone();
        chain.push(info);
        if base == trunk {
            break;
        }
        // Find the PR whose head_ref == base of the previous PR. We have no
        // direct lookup, so use `gh pr list` via a single view call by branch.
        // For simplicity we assume a PR exists for each layer; if not, we
        // stop walking — that branch becomes the stack bottom.
        match find_pr_by_head(gh, &base) {
            Ok(Some(parent_pr)) => next = Some(parent_pr.number),
            _ => next = None,
        }
    }
    // Bottom of stack is first in chain after reverse.
    chain.reverse();

    // Fetch each branch locally.
    ctx.git.fetch("origin")?;
    for info in &chain {
        // Create or fast-forward a local branch tracking origin/<head>.
        let remote_ref = format!("refs/remotes/origin/{}", info.head_ref);
        let sha = ctx.git.rev_parse(&remote_ref).map_err(|_| {
            StackError::Other(format!(
                "branch `{}` not found on origin after fetch",
                info.head_ref
            ))
        })?;
        if ctx.git.branch_exists(&info.head_ref)? {
            // Skip — already exists locally.
            let _ = sha;
        } else {
            ctx.git.create_branch(&info.head_ref, &sha)?;
        }
    }

    // Build the stack record.
    let trunk_head = ctx.git.rev_parse(&trunk)?;
    let mut branches: Vec<Branch> = Vec::with_capacity(chain.len());
    let mut parent_sha = trunk_head.clone();
    for info in &chain {
        let head = ctx.git.rev_parse(&info.head_ref)?;
        branches.push(Branch {
            branch: info.head_ref.clone(),
            head: Some(head.clone()),
            base: parent_sha,
            pull_request: Some(PullRequest {
                number: info.number,
                id: info.id.clone(),
                url: info.url.clone(),
                merged: info.state == PrState::Merged,
            }),
        });
        parent_sha = head;
    }

    let mut file = ctx.store.load(&ctx.identity)?;
    // If any of these branches are already in a stack, refuse.
    for b in &branches {
        if !file.stacks_containing(&b.branch).is_empty() {
            return Err(StackError::InvalidArgs(format!(
                "branch `{}` is already in a local stack; run `stack unstack` first",
                b.branch
            )));
        }
    }
    file.stacks.push(Stack {
        prefix: None,
        trunk: Trunk {
            branch: trunk,
            head: trunk_head,
        },
        branches,
        last_refreshed_at: Some(now_rfc3339()),
    });
    ctx.store.save(&file)?;

    // Checkout the originally requested PR's branch (top of what we asked for,
    // which is the *last* element after we reversed; find it explicitly).
    let target_branch = chain
        .iter()
        .find(|p| p.number == pr_number)
        .map(|p| p.head_ref.clone())
        .unwrap();
    ctx.git.checkout(&target_branch)?;
    eprintln!("✓ checked out PR #{pr_number} ({target_branch})");
    Ok(())
}

fn find_pr_by_head(_gh: &dyn GitHub, _head: &str) -> Result<Option<PullRequestInfo>, StackError> {
    // We don't have a `list` op on the port yet. For v1, stop chain-walking
    // here — the user can call `checkout <pr>` on the bottommost PR instead.
    Ok(None)
}
