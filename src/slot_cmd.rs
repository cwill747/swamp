//! `swamp slot ...`: the CLI front end for the slot pool in
//! [`crate::worktree::slots`].
//!
//! Stdout carries only machine-readable output (a path, or JSON with
//! `--json`). Messages and warnings go to stderr.

use crate::cli::{SlotArgs, SlotCmd, SlotTabOp};
use crate::config;
use crate::daemon::socket::{ClientMsg, ServerMsg, read_server_msg, write_client_msg};
use crate::slot_service;
use crate::worktree::git_common_dir;
use crate::worktree::slots::{
    self, AllocOptions, Allocation, PoolEntry, Release, ReleaseOptions, ReleaseOutcome,
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
    // Slot commands often run from hooks with no terminal; record them in the
    // repository diagnostic log (`swamp logs`).
    let log_cfg = config::load_config().map(|c| c.logging).unwrap_or_default();
    crate::logging::init(&common, false, false, &log_cfg);

    match args.command {
        SlotCmd::Alloc {
            branch,
            hook_json,
            offline,
            no_hooks,
            no_tab,
        } => {
            let branch = match branch {
                Some(b) => b,
                None => hook_field(&read_hook_payload()?, "name")?,
            };
            let a = alloc(&common, branch, offline, no_hooks).await?;
            if !no_tab && !hook_json {
                open_tab(&common, &a).await;
            }
            crate::slot_hooks::print_runs("post-alloc", &a.hooks);
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
            hook_json,
            force,
            offline,
            no_hooks,
            no_tab,
        } => {
            let target = match (target, hook_json) {
                (None, true) => Some(hook_field(&read_hook_payload()?, "worktree_path")?),
                (target, _) => target,
            };
            let r = release(&common, target, start, force, offline, no_hooks).await?;
            crate::slot_hooks::print_runs("pre-release", &r.hooks);
            if !no_tab && matches!(r.outcome, ReleaseOutcome::Detached { .. }) {
                close_tab(&common, &r);
            }
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
pub async fn alloc(
    common: &Path,
    branch: String,
    offline: bool,
    no_hooks: bool,
) -> Result<Allocation> {
    if let Some(mut stream) = connect_daemon(common).await {
        let msg = ClientMsg::SlotAlloc {
            branch,
            base: None,
            offline,
            no_hooks,
        };
        let wait = reply_timeout(common, no_hooks, |h| &h.post_alloc);
        return match request(&mut stream, &msg, wait).await? {
            ServerMsg::SlotAllocated { allocation } => Ok(allocation),
            other => Err(reply_error(other)),
        };
    }
    let common = common.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let cfg = config::slots_config(&common)?;
        let opts = AllocOptions {
            fetch: !offline,
            ..AllocOptions::pool(cfg.pool_size)
        };
        let hooks = if no_hooks {
            &[][..]
        } else {
            &cfg.hooks.post_alloc[..]
        };
        slot_service::alloc_with_status_file(&common, &branch, &opts, hooks)
    })
    .await?
}

/// Release through the daemon when one answers, otherwise in-process. Either
/// way the CLI looks the PR up first: the daemon's PR map stays empty while no
/// TUI subscribes, and release must not depend on that.
pub async fn release(
    common: &Path,
    target: Option<String>,
    cwd: PathBuf,
    force: bool,
    offline: bool,
    no_hooks: bool,
) -> Result<Release> {
    let pr = if offline {
        None
    } else {
        let (common, target, cwd) = (common.to_path_buf(), target.clone(), cwd.clone());
        tokio::task::spawn_blocking(move || {
            slot_service::pr_facts(&common, target.as_deref(), &cwd)
        })
        .await?
    };
    if let Some(mut stream) = connect_daemon(common).await {
        let msg = ClientMsg::SlotRelease {
            target,
            cwd,
            force,
            offline,
            no_hooks,
            pr,
        };
        let wait = reply_timeout(common, no_hooks, |h| &h.pre_release);
        return match request(&mut stream, &msg, wait).await? {
            ServerMsg::SlotReleased { release } => Ok(release),
            other => Err(reply_error(other)),
        };
    }
    let common = common.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let opts = ReleaseOptions {
            force,
            fetch: !offline,
            pr,
        };
        let cfg = config::slots_config(&common)?;
        let hooks = if no_hooks {
            &[][..]
        } else {
            &cfg.hooks.pre_release[..]
        };
        slot_service::release_with_status_file(&common, target.as_deref(), &cwd, &opts, hooks)
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

/// How long a slot operation may take without hooks: a fetch and a checkout.
const BASE_REPLY_TIMEOUT: Duration = Duration::from_secs(300);

/// The reply deadline for a daemon slot operation: the base time plus the
/// time limit of each hook the daemon runs for the event. The daemon reads the
/// same config files, so the CLI can add up the same hooks.
fn reply_timeout(
    common: &Path,
    no_hooks: bool,
    event: impl Fn(&config::SlotHooks) -> &Vec<config::HookCommand>,
) -> Duration {
    if no_hooks {
        return BASE_REPLY_TIMEOUT;
    }
    let hooks = config::slots_config(common)
        .map(|c| event(&c.hooks).clone())
        .unwrap_or_default();
    BASE_REPLY_TIMEOUT + hooks_budget(&hooks)
}

fn hooks_budget(hooks: &[config::HookCommand]) -> Duration {
    hooks
        .iter()
        .map(|h| Duration::from_secs(h.timeout_secs.unwrap_or(config::DEFAULT_HOOK_TIMEOUT_SECS)))
        .sum()
}

/// Send one request and wait up to `wait` for its reply.
async fn request(stream: &mut UnixStream, msg: &ClientMsg, wait: Duration) -> Result<ServerMsg> {
    write_client_msg(stream, msg)
        .await
        .context("send request to daemon")?;
    tokio::time::timeout(wait, read_server_msg(stream))
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
        && let Err(e) = request(&mut stream, &ClientMsg::Refresh, BASE_REPLY_TIMEOUT).await
    {
        tracing::debug!("daemon refresh after pin change failed: {e:#}");
    }
}

/// Read a Claude Code hook payload (JSON) from stdin.
///
/// Contract (Claude Code hooks reference, `WorktreeCreate` / `WorktreeRemove`,
/// checked 2026-11):
/// - `WorktreeCreate` stdin carries the common fields (`session_id`,
///   `transcript_path`, `cwd`, `hook_event_name`) plus `name`, a slug such as
///   `bold-oak-a3f2`. The hook prints the worktree path on stdout; a failure
///   or an empty stdout fails the creation.
/// - `WorktreeRemove` stdin carries `worktree_path`. A non-zero exit while the
///   directory still exists fails the removal, which is how a refused release
///   keeps the slot. Claude Code never deletes the branch itself.
/// - Neither event takes a matcher. The default timeout is 600 seconds.
/// - `WorktreeRemove` does not fire when a subagent finishes, so a slot that a
///   subagent allocated stays busy until someone releases it.
fn read_hook_payload() -> Result<serde_json::Value> {
    use std::io::Read;
    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .context("read hook payload from stdin")?;
    tracing::debug!(payload = %raw, "claude worktree hook payload");
    serde_json::from_str(&raw).context("hook payload on stdin is not JSON")
}

/// A non-empty string field of a hook payload.
fn hook_field(payload: &serde_json::Value, field: &str) -> Result<String> {
    payload
        .get(field)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(String::from)
        .with_context(|| format!("hook payload has no `{field}` string field"))
}

/// Open the worktree's tab in the repo session, if that session runs. A tab
/// failure is only a warning: the worktree itself is ready.
async fn open_tab(common: &Path, a: &Allocation) {
    let (common, name, path) = (common.to_path_buf(), a.name.clone(), a.path.clone());
    let res = tokio::task::spawn_blocking(move || {
        crate::launch::spawn_slot_tab(&common, SlotTabOp::Open, &name, &path, true)
    })
    .await;
    if let Ok(Err(e)) | Err(e) = res.map_err(anyhow::Error::from) {
        eprintln!("swamp: could not open a tab for {}: {e:#}", a.name);
    }
}

/// Close the released slot's tab. The close runs detached, because the caller
/// may be a pane inside that tab.
fn close_tab(common: &Path, r: &Release) {
    if let Err(e) = crate::launch::spawn_slot_tab(common, SlotTabOp::Close, &r.name, &r.path, false)
    {
        eprintln!("swamp: could not close the tab for {}: {e:#}", r.name);
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

    #[test]
    fn hooks_budget_adds_each_time_limit() {
        let hooks = [
            config::HookCommand {
                run: "a".into(),
                timeout_secs: Some(600),
            },
            config::HookCommand {
                run: "b".into(),
                timeout_secs: None,
            },
        ];
        assert_eq!(
            hooks_budget(&hooks),
            Duration::from_secs(600 + config::DEFAULT_HOOK_TIMEOUT_SECS)
        );
        assert_eq!(hooks_budget(&[]), Duration::ZERO);
    }

    #[test]
    fn hook_fields_from_claude_payloads() {
        let create: serde_json::Value = serde_json::from_str(
            r#"{"session_id":"s","cwd":"/r","hook_event_name":"WorktreeCreate","name":"bold-oak-a3f2"}"#,
        )
        .unwrap();
        assert_eq!(hook_field(&create, "name").unwrap(), "bold-oak-a3f2");
        let remove: serde_json::Value =
            serde_json::from_str(r#"{"worktree_path":"/r/work2"}"#).unwrap();
        assert_eq!(hook_field(&remove, "worktree_path").unwrap(), "/r/work2");
        assert!(hook_field(&remove, "name").is_err());
        let blank: serde_json::Value = serde_json::from_str(r#"{"name":"  "}"#).unwrap();
        assert!(hook_field(&blank, "name").is_err());
    }

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
