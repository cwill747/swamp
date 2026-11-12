//! `swamp slot ...`: the CLI front end for the slot pool in
//! [`crate::worktree::slots`].
//!
//! Stdout carries only machine-readable output (a path, or JSON with
//! `--json`). Messages and warnings go to stderr.

use crate::cli::{SlotArgs, SlotCmd};
use crate::config;
use crate::worktree::git_common_dir;
use crate::worktree::slots::{
    self, AllocOptions, PoolEntry, PrFacts, Release, ReleaseOptions, ReleaseOutcome,
};
use crate::worktree::{SlotState, WorktreeKind};
use anyhow::{Context, Result};
use std::path::Path;

pub fn run(args: SlotArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let start = args.dir.clone().unwrap_or_else(|| cwd.clone());
    let common = git_common_dir(&start).context("not inside a git repo")?;

    match args.command {
        SlotCmd::Alloc { branch, offline } => {
            let pool_size = config::slots_config(&common)?.pool_size;
            let a = slots::alloc(
                &common,
                &branch,
                AllocOptions {
                    pool_size,
                    fetch: !offline,
                },
            )?;
            for w in &a.warnings {
                eprintln!("swamp: {w}");
            }
            if args.json {
                println!("{}", serde_json::to_string(&a)?);
            } else {
                let msg = match (a.existing, a.kind) {
                    (true, _) => format!("{} already has {}", a.name, a.branch),
                    (false, WorktreeKind::Overflow) => format!(
                        "pool is full; created overflow worktree {} for {}",
                        a.name, a.branch
                    ),
                    _ => format!("allocated {} to {}", a.name, a.branch),
                };
                eprintln!("swamp: {msg}");
                println!("{}", a.path.display());
            }
            Ok(())
        }
        SlotCmd::Release {
            target,
            force,
            offline,
        } => {
            let pr = if offline {
                None
            } else {
                pr_facts(&common, target.as_deref(), &start)
            };
            let r = slots::release(
                &common,
                target.as_deref(),
                &start,
                &ReleaseOptions {
                    force,
                    fetch: !offline,
                    pr,
                },
            )?;
            if args.json {
                println!("{}", serde_json::to_string(&r)?);
            } else {
                eprintln!("swamp: {}", describe_release(&r));
            }
            Ok(())
        }
        SlotCmd::Pin { slot, reason } => {
            slots::pin(&common, &slot, &reason.join(" "))?;
            eprintln!("swamp: pinned {slot}");
            Ok(())
        }
        SlotCmd::Unpin { slot } => {
            if slots::unpin(&common, &slot)? {
                eprintln!("swamp: unpinned {slot}");
            } else {
                eprintln!("swamp: {slot} had no pin");
            }
            Ok(())
        }
        SlotCmd::List => {
            let pool_size = config::slots_config(&common)?.pool_size;
            let entries = slots::list_pool(&common, pool_size)?;
            if args.json {
                println!("{}", serde_json::to_string(&entries)?);
            } else {
                print!("{}", format_list(&entries));
            }
            Ok(())
        }
    }
}

/// Look up the PR for the branch that `target` resolves to, before release
/// takes the slot lock: the lookup is a network call. Any failure (no `gh`, no
/// auth, offline) yields `None`, and release then relies on git state only.
fn pr_facts(common: &Path, target: Option<&str>, cwd: &Path) -> Option<PrFacts> {
    let wt = slots::resolve_target(common, target, cwd).ok()?;
    let branch = slots::checked_out_branch(&wt.path)?;
    let mut prs = crate::github::list_prs_for_branches(&wt.path, std::slice::from_ref(&branch))
        .map_err(|e| tracing::debug!("PR lookup for {branch} failed: {e:#}"))
        .ok()?;
    let pr = prs.remove(&branch)?;
    Some(PrFacts {
        branch,
        state: pr.state,
        head_oid: pr.head_oid.and_then(|o| o.parse().ok()),
    })
}

fn describe_release(r: &Release) -> String {
    let branch = r.branch.as_deref().unwrap_or("?");
    match r.outcome {
        ReleaseOutcome::Detached {
            branch_deleted: true,
        } => format!("released {}; deleted branch {branch} (PR finished)", r.name),
        ReleaseOutcome::Detached {
            branch_deleted: false,
        } => format!("released {}; kept branch {branch}", r.name),
        ReleaseOutcome::AlreadyFree => format!("{} was already free", r.name),
        ReleaseOutcome::KeptOverflow => format!(
            "{} is an overflow worktree; it stays until you remove it",
            r.name
        ),
    }
}

fn format_list(entries: &[PoolEntry]) -> String {
    let width = entries.iter().map(|e| e.name.len()).max().unwrap_or(4);
    let mut out = String::new();
    for e in entries {
        let state = match (e.kind, e.state) {
            (WorktreeKind::Default, _) => "default",
            (WorktreeKind::Overflow, _) => "overflow",
            (WorktreeKind::Slot, Some(SlotState::Free)) => "free",
            (WorktreeKind::Slot, Some(SlotState::Pinned)) => "pinned",
            (WorktreeKind::Slot, _) => "busy",
        };
        let mut detail = e.branch.clone().unwrap_or_default();
        if let Some(pin) = &e.pin {
            detail = format!("{detail} (pin: {pin})").trim().to_string();
        }
        if !e.in_pool {
            detail = format!("{detail} [outside pool]").trim().to_string();
        }
        out.push_str(&format!("{:<width$}  {state:<8}  {detail}\n", e.name));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(name: &str, kind: WorktreeKind, state: Option<SlotState>) -> PoolEntry {
        PoolEntry {
            name: name.into(),
            path: PathBuf::from("/r").join(name),
            kind,
            state,
            branch: None,
            pin: None,
            in_pool: true,
        }
    }

    #[test]
    fn list_shows_state_branch_pin_and_pool() {
        let mut busy = entry("work1", WorktreeKind::Slot, Some(SlotState::Busy));
        busy.branch = Some("fix-ci".into());
        let mut pinned = entry("work2", WorktreeKind::Slot, Some(SlotState::Pinned));
        pinned.pin = Some("review #172".into());
        let mut outside = entry("work6", WorktreeKind::Slot, Some(SlotState::Free));
        outside.in_pool = false;
        let mut spike = entry("spike-x", WorktreeKind::Overflow, None);
        spike.branch = Some("spike-x".into());

        let text = format_list(&[busy, pinned, outside, spike]);

        assert_eq!(
            text,
            "work1    busy      fix-ci\n\
             work2    pinned    (pin: review #172)\n\
             work6    free      [outside pool]\n\
             spike-x  overflow  spike-x\n"
        );
    }
}
