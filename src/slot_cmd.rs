//! `swamp slot ...`: the CLI front end for the slot pool in
//! [`crate::worktree::slots`].
//!
//! Stdout carries only machine-readable output (a path, or JSON with
//! `--json`). Messages and warnings go to stderr.

use crate::cli::{SlotArgs, SlotCmd};
use crate::config;
use crate::daemon::socket::{ClientMsg, ServerMsg, read_server_msg, write_client_msg};
use crate::slot_service;
use crate::worktree::git_common_dir;
use crate::worktree::slots::{
    self, AllocOptions, Allocation, PoolEntry, PrFacts, Release, ReleaseOptions, ReleaseOutcome,
};
use crate::worktree::{SlotState, WorktreeKind};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::UnixStream;

pub async fn run(args: SlotArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let start = args.dir.clone().unwrap_or_else(|| cwd.clone());
    let common = git_common_dir(&start).context("not inside a git repo")?;

    match args.command {
        SlotCmd::Alloc { branch, offline } => {
            let a = alloc(&common, branch, offline).await?;
            for w in &a.warnings {
                eprintln!("swamp: {w}");
            }
            if args.json {
                println!("{}", serde_json::to_string(&a)?);
            } else {
                eprintln!("swamp: {}", describe_alloc(&a));
                println!("{}", a.path.display());
            }
            Ok(())
        }
        SlotCmd::Release {
            target,
            force,
            offline,
        } => {
            let r = release(&common, target, start, force, offline).await?;
            if args.json {
                println!("{}", serde_json::to_string(&r)?);
            } else {
                eprintln!("swamp: {}", describe_release(&r));
            }
            Ok(())
        }
        SlotCmd::Pin { slot, reason } => {
            slots::pin(&common, &slot, &reason.join(" "))?;
            refresh_daemon(&common).await;
            eprintln!("swamp: pinned {slot}");
            Ok(())
        }
        SlotCmd::Unpin { slot } => {
            let had_pin = slots::unpin(&common, &slot)?;
            refresh_daemon(&common).await;
            if had_pin {
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

/// Allocate through the daemon when one answers, so it updates its agent
/// records and broadcasts the change; otherwise allocate in-process and edit
/// the status file.
pub async fn alloc(common: &Path, branch: String, offline: bool) -> Result<Allocation> {
    if let Some(mut stream) = connect_daemon(common).await {
        let msg = ClientMsg::SlotAlloc {
            branch,
            base: None,
            offline,
        };
        return match request(&mut stream, &msg).await? {
            ServerMsg::SlotAllocated { allocation } => Ok(allocation),
            other => Err(reply_error(other)),
        };
    }
    let common = common.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let opts = AllocOptions {
            fetch: !offline,
            ..AllocOptions::pool(config::slots_config(&common)?.pool_size)
        };
        let (a, changes) = slot_service::alloc(&common, &branch, &opts)?;
        slot_service::apply_to_status_file(&common, &changes)?;
        Ok(a)
    })
    .await?
}

/// Release through the daemon when one answers (it supplies PR facts from its
/// own PR map); otherwise look the PR up here and release in-process.
pub async fn release(
    common: &Path,
    target: Option<String>,
    cwd: PathBuf,
    force: bool,
    offline: bool,
) -> Result<Release> {
    if let Some(mut stream) = connect_daemon(common).await {
        let msg = ClientMsg::SlotRelease {
            target,
            cwd,
            force,
            offline,
            pr: None,
        };
        return match request(&mut stream, &msg).await? {
            ServerMsg::SlotReleased { release } => Ok(release),
            other => Err(reply_error(other)),
        };
    }
    let common = common.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let pr = if offline {
            None
        } else {
            pr_facts(&common, target.as_deref(), &cwd)
        };
        let opts = ReleaseOptions {
            force,
            fetch: !offline,
            pr,
        };
        let agents = slot_service::load_agents(&common);
        let (r, changes) = slot_service::release(&common, target.as_deref(), &cwd, &opts, &agents)?;
        slot_service::apply_to_status_file(&common, &changes)?;
        Ok(r)
    })
    .await?
}

/// Connect to the repo daemon. `None` when no daemon listens, which selects the
/// in-process path. Errors after a connection succeeds are real errors: the
/// daemon may already have acted on the request.
async fn connect_daemon(common: &Path) -> Option<UnixStream> {
    let sock = crate::daemon::socket_path(common).ok()?;
    if !sock.exists() {
        return None;
    }
    tokio::time::timeout(Duration::from_millis(200), UnixStream::connect(&sock))
        .await
        .ok()?
        .ok()
}

/// Send one request and wait for its reply. Slot operations fetch and check
/// out, so the wait is long.
async fn request(stream: &mut UnixStream, msg: &ClientMsg) -> Result<ServerMsg> {
    write_client_msg(stream, msg)
        .await
        .context("send request to daemon")?;
    tokio::time::timeout(Duration::from_secs(300), read_server_msg(stream))
        .await
        .context("daemon reply timed out")??
        .context("daemon closed the connection before it replied")
}

fn reply_error(reply: ServerMsg) -> anyhow::Error {
    match reply {
        ServerMsg::Err { message } => anyhow::anyhow!(message),
        other => anyhow::anyhow!("unexpected daemon reply: {other:?}"),
    }
}

/// Ask a running daemon to rescan so its snapshot shows a pin change. Best
/// effort: without a daemon there is nothing to refresh.
async fn refresh_daemon(common: &Path) {
    if let Some(mut stream) = connect_daemon(common).await
        && let Err(e) = request(&mut stream, &ClientMsg::Refresh).await
    {
        tracing::debug!("daemon refresh after pin change failed: {e:#}");
    }
}

fn describe_alloc(a: &Allocation) -> String {
    match (a.existing, a.kind) {
        (true, _) => format!("{} already has {}", a.name, a.branch),
        (false, WorktreeKind::Overflow) => format!(
            "pool is full; created overflow worktree {} for {}",
            a.name, a.branch
        ),
        _ => format!("allocated {} to {}", a.name, a.branch),
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
