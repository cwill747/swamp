use crate::daemon::socket::{ClientMsg, ServerMsg, read_server_msg, write_client_msg};
use crate::daemon::state::{SubagentEvent, SubagentEventKind};
use crate::daemon::{self};
use crate::util::now_unix;
use crate::worktree::{git_common_dir, list_worktrees};
use anyhow::{Context, Result};
use serde_json::json;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::UnixStream;

pub async fn run(
    status: String,
    dir: Option<PathBuf>,
    session_name: Option<String>,
    session_id: Option<String>,
    payload: HookPayload,
) -> Result<()> {
    if payload.skip {
        tracing::debug!(event = ?payload.hook_event, "hook payload is not for swamp; ignoring");
        return Ok(());
    }
    let start = dir.unwrap_or(std::env::current_dir()?);
    let common = git_common_dir(&start).context("not inside a git repo")?;
    let wt_name = resolve_worktree_name(&common, &start).unwrap_or_else(|| {
        start
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".into())
    });

    let sock = daemon::socket_path(&common)?;
    // Try daemon first.
    if sock.exists() {
        match forward_to_daemon(
            &sock,
            ClientMsg::Hook {
                worktree: wt_name.clone(),
                status: status.clone(),
                session_name: session_name.clone(),
                session_id: session_id.clone(),
                subagent: payload.subagent.clone(),
            },
        )
        .await
        {
            Ok(()) => {
                tracing::debug!(worktree = %wt_name, status = %status, "forwarded hook to daemon");
                return Ok(());
            }
            Err(e) => {
                tracing::debug!(worktree = %wt_name, status = %status, "daemon hook failed; falling back to status file: {e:?}");
            }
        }
    }

    write_status_file(
        &common,
        wt_name,
        &status,
        session_name,
        session_id,
        &payload,
    )
    .await
}

/// Record the hook in `<common>/.swamp-status.json` when no daemon answers.
async fn write_status_file(
    common: &Path,
    wt_name: String,
    status: &str,
    session_name: Option<String>,
    session_id: Option<String>,
    payload: &HookPayload,
) -> Result<()> {
    // Subagents live in daemon memory only, and a subagent event must never
    // change the parent status, so without a daemon there is nothing to record.
    if payload
        .subagent
        .as_ref()
        .is_some_and(|ev| ev.kind != SubagentEventKind::Spawn)
    {
        tracing::debug!(worktree = %wt_name, "daemon down; dropping subagent hook");
        return Ok(());
    }
    tracing::debug!(worktree = %wt_name, status = %status, "daemon down; writing hook to status file");
    // Fallback: mutate .swamp-status.json directly.
    let path = common.join(".swamp-status.json");
    let mut map = read_status_map(&path).await?;
    // Carry forward prior session_name / session_id / harness when this hook
    // omits them, so transient `working`/`idle` pings don't erase data we need
    // later: the session id to resume Claude (#33), and the per-worktree harness
    // override the user picked with `h`. Mirrors DaemonState::apply_hook.
    let prior = map.get(&wt_name).cloned();
    let prior_field = |key: &str| {
        prior
            .as_ref()
            .and_then(|p| p.get(key))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    };
    let session_name = session_name
        .filter(|s| !s.is_empty())
        .or_else(|| prior_field("session_name"));
    let session_id = session_id
        .filter(|s| !s.is_empty())
        .or_else(|| prior_field("session_id"));
    // A hook never carries the harness override; always preserve the saved one,
    // otherwise this fallback silently reverts `choose` repos to Claude.
    let harness = prior_field("harness");

    let mut entry = serde_json::Map::new();
    entry.insert("status".into(), json!(status.to_lowercase()));
    entry.insert("ts".into(), json!(now_unix()));
    if let Some(ref name) = session_name {
        entry.insert("session_name".into(), json!(name));
    }
    if let Some(ref id) = session_id {
        entry.insert("session_id".into(), json!(id));
    }
    if let Some(ref h) = harness {
        entry.insert("harness".into(), json!(h));
    }
    map.insert(wt_name, json!(entry));
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, serde_json::to_vec_pretty(&map)?).await?;
    tokio::fs::rename(&tmp, &path).await?;
    Ok(())
}

/// Largest hook payload `swamp hook` reads from stdin. Tool hooks can carry
/// large tool input or output; a cut payload does not parse, so the cap is
/// generous.
const MAX_PAYLOAD_BYTES: u64 = 8 * 1024 * 1024;
/// How long `swamp hook` waits for stdin to reach EOF.
const STDIN_TIMEOUT: Duration = Duration::from_millis(100);
/// Longest string swamp keeps from a payload field. The daemon rejects
/// fields above its own limit, which would drop the whole status update.
const MAX_PAYLOAD_FIELD: usize = 256;

/// What `swamp hook` takes from a Claude Code hook's stdin payload.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HookPayload {
    /// `hook_event_name`, for example `UserPromptSubmit`.
    pub hook_event: Option<String>,
    pub subagent: Option<SubagentEvent>,
    /// True when the payload shows this hook must not change any status: a
    /// subagent event swamp cannot attribute, or one from an internal agent.
    pub skip: bool,
}

/// Read and parse the hook payload from stdin. Returns an empty payload when
/// stdin is a terminal, is empty, stays open past a short timeout, or does
/// not hold JSON. A reader thread does the read so a stdin that never closes
/// cannot hold the process open.
pub fn read_stdin_payload() -> HookPayload {
    use std::io::{IsTerminal, Read};
    if std::io::stdin().is_terminal() {
        return HookPayload::default();
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let res = std::io::stdin()
            .lock()
            .take(MAX_PAYLOAD_BYTES)
            .read_to_end(&mut buf);
        let _ = tx.send(res.map(|_| buf));
    });
    match rx.recv_timeout(STDIN_TIMEOUT) {
        Ok(Ok(buf)) if !buf.is_empty() => match serde_json::from_slice(&buf) {
            Ok(value) => parse_payload(&value),
            Err(e) => {
                tracing::debug!("hook stdin is not JSON: {e}");
                HookPayload::default()
            }
        },
        Ok(Ok(_)) => HookPayload::default(),
        Ok(Err(e)) => {
            tracing::debug!("hook stdin read failed: {e}");
            HookPayload::default()
        }
        Err(_) => {
            tracing::debug!("hook stdin read timed out");
            HookPayload::default()
        }
    }
}

/// Map a Claude Code hook payload to the event swamp forwards. See the
/// `show-subagents-in-ai-status` design for the rules.
pub fn parse_payload(v: &serde_json::Value) -> HookPayload {
    let str_field = |v: &serde_json::Value, key: &str| {
        v.get(key)
            .and_then(|x| x.as_str())
            .map(clean_field)
            .filter(|s| !s.is_empty())
    };
    let hook_event = str_field(v, "hook_event_name");
    let agent_id = str_field(v, "agent_id");
    let agent_type = str_field(v, "agent_type").unwrap_or_default();
    let mut out = HookPayload {
        hook_event: hook_event.clone(),
        ..Default::default()
    };
    let event = hook_event.as_deref().unwrap_or("");

    // Lifecycle events are about the subagent alone. When swamp cannot name
    // the subagent, the event must not fall through to the parent status.
    if matches!(event, "SubagentStart" | "SubagentStop") {
        match agent_id {
            Some(id) if !agent_type.is_empty() => {
                out.subagent = Some(SubagentEvent {
                    kind: if event == "SubagentStart" {
                        SubagentEventKind::Start
                    } else {
                        SubagentEventKind::Stop
                    },
                    agent_id: Some(id),
                    agent_type,
                    description: None,
                });
            }
            _ => out.skip = true,
        }
        return out;
    }

    // A hook that fired inside a subagent. `Notification` stays with the
    // parent: a permission prompt needs the parent to show "waiting".
    if let Some(id) = agent_id
        && event != "Notification"
    {
        if agent_type.is_empty() {
            out.skip = true;
        } else {
            out.subagent = Some(SubagentEvent {
                kind: SubagentEventKind::Activity,
                agent_id: Some(id),
                agent_type,
                description: None,
            });
        }
        return out;
    }

    // The parent's own `Agent` tool call carries the task description.
    let tool = v.get("tool_name").and_then(|x| x.as_str()).unwrap_or("");
    if matches!(tool, "Agent" | "Task") && matches!(event, "PreToolUse" | "PostToolUse") {
        let input = v.get("tool_input").unwrap_or(&serde_json::Value::Null);
        let description = str_field(input, "description");
        let response = v.get("tool_response").unwrap_or(&serde_json::Value::Null);
        let agent_id = (event == "PostToolUse")
            .then(|| str_field(response, "agentId"))
            .flatten();
        // The PreToolUse queues the description. A PostToolUse is useful only
        // when it names the agent. Without an id, it would queue the
        // description again for a later subagent of the same type.
        if description.is_some() && (event == "PreToolUse" || agent_id.is_some()) {
            out.subagent = Some(SubagentEvent {
                kind: SubagentEventKind::Spawn,
                agent_id,
                agent_type: str_field(input, "subagent_type")
                    .unwrap_or_else(|| "general-purpose".to_string()),
                description,
            });
        }
    }
    out
}

/// Replace control characters with spaces, trim, and cap the length on a
/// character boundary, so a payload field always passes daemon validation.
fn clean_field(s: &str) -> String {
    let mut out: String = s
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if out.len() > MAX_PAYLOAD_FIELD {
        let mut end = MAX_PAYLOAD_FIELD;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    out
}

fn resolve_worktree_name(common: &Path, start: &Path) -> Option<String> {
    let target = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    list_worktrees(common)
        .ok()?
        .into_iter()
        .filter_map(|wt| {
            let path = std::fs::canonicalize(&wt.path).unwrap_or_else(|_| wt.path.clone());
            target.starts_with(&path).then(|| (path, wt.name()))
        })
        .max_by_key(|(path, _)| path.components().count())
        .map(|(_, name)| name)
}

async fn read_status_map(path: &Path) -> Result<serde_json::Map<String, serde_json::Value>> {
    let Ok(bytes) = tokio::fs::read(path).await else {
        return Ok(Default::default());
    };
    match serde_json::from_slice(&bytes) {
        Ok(map) => Ok(map),
        Err(e) => {
            let corrupt = corrupt_path(path);
            match tokio::fs::rename(path, &corrupt).await {
                Ok(()) => tracing::warn!(
                    path = %path.display(),
                    corrupt = %corrupt.display(),
                    "renamed corrupt swamp status file: {e}"
                ),
                Err(rename_err) => tracing::warn!(
                    path = %path.display(),
                    "could not rename corrupt swamp status file ({e}): {rename_err}"
                ),
            }
            Ok(Default::default())
        }
    }
}

fn corrupt_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{}.corrupt", now_unix()));
    PathBuf::from(name)
}

async fn forward_to_daemon(sock: &std::path::Path, msg: ClientMsg) -> Result<()> {
    let mut s = tokio::time::timeout(Duration::from_millis(200), UnixStream::connect(sock))
        .await
        .context("connect to daemon timed out")?
        .context("connect to daemon")?;
    tokio::time::timeout(Duration::from_millis(500), write_client_msg(&mut s, &msg))
        .await
        .context("write hook to daemon timed out")?
        .context("write hook to daemon")?;
    match tokio::time::timeout(Duration::from_millis(500), read_server_msg(&mut s))
        .await
        .context("read hook ack timed out")?
        .context("read hook ack")?
    {
        Some(ServerMsg::Ok) => Ok(()),
        Some(ServerMsg::Err { message }) => anyhow::bail!(message),
        Some(other) => anyhow::bail!("unexpected hook reply: {other:?}"),
        None => anyhow::bail!("daemon closed before hook ack"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(v: serde_json::Value) -> HookPayload {
        parse_payload(&v)
    }

    #[test]
    fn subagent_start_and_stop() {
        let p = parse(json!({
            "hook_event_name": "SubagentStart",
            "session_id": "s", "agent_id": "agent-1", "agent_type": "Explore"
        }));
        assert!(!p.skip);
        let ev = p.subagent.unwrap();
        assert_eq!(ev.kind, SubagentEventKind::Start);
        assert_eq!(ev.agent_id.as_deref(), Some("agent-1"));
        assert_eq!(ev.agent_type, "Explore");

        let p = parse(json!({
            "hook_event_name": "SubagentStop",
            "agent_id": "agent-1", "agent_type": "Explore",
            "last_assistant_message": "done"
        }));
        assert_eq!(p.subagent.unwrap().kind, SubagentEventKind::Stop);
    }

    #[test]
    fn internal_agent_or_missing_id_is_skipped() {
        let p =
            parse(json!({"hook_event_name": "SubagentStop", "agent_id": "x", "agent_type": ""}));
        assert!(p.skip);
        assert!(p.subagent.is_none());
        let p = parse(json!({"hook_event_name": "SubagentStart", "agent_type": "Plan"}));
        assert!(p.skip);
        let p =
            parse(json!({"hook_event_name": "PreToolUse", "agent_id": "x", "tool_name": "Bash"}));
        assert!(p.skip);
    }

    #[test]
    fn tool_hook_inside_subagent_is_activity() {
        let p = parse(json!({
            "hook_event_name": "PostToolUse",
            "agent_id": "agent-2", "agent_type": "general-purpose",
            "tool_name": "Read", "tool_response": {"content": "..."}
        }));
        assert!(!p.skip);
        let ev = p.subagent.unwrap();
        assert_eq!(ev.kind, SubagentEventKind::Activity);
        assert_eq!(ev.agent_id.as_deref(), Some("agent-2"));
    }

    #[test]
    fn notification_stays_with_parent() {
        let p = parse(json!({
            "hook_event_name": "Notification",
            "agent_id": "agent-2", "agent_type": "Explore",
            "notification_type": "permission_prompt"
        }));
        assert!(!p.skip);
        assert!(p.subagent.is_none());
        assert_eq!(p.hook_event.as_deref(), Some("Notification"));
    }

    #[test]
    fn parent_agent_tool_call_is_spawn() {
        let p = parse(json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Agent",
            "tool_input": {"description": "Find hook\ncode", "subagent_type": "Explore", "prompt": "..."}
        }));
        let ev = p.subagent.unwrap();
        assert_eq!(ev.kind, SubagentEventKind::Spawn);
        assert_eq!(ev.agent_id, None);
        assert_eq!(ev.agent_type, "Explore");
        assert_eq!(ev.description.as_deref(), Some("Find hook code"));

        // No subagent_type means the default agent.
        let p = parse(json!({
            "hook_event_name": "PreToolUse", "tool_name": "Task",
            "tool_input": {"description": "Do it"}
        }));
        assert_eq!(p.subagent.unwrap().agent_type, "general-purpose");
    }

    #[test]
    fn background_agent_response_carries_id() {
        let p = parse(json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Agent",
            "tool_input": {"description": "Watch CI", "subagent_type": "general-purpose"},
            "tool_response": {"status": "async_launched", "agentId": "agent-9", "description": "Watch CI"}
        }));
        let ev = p.subagent.unwrap();
        assert_eq!(ev.kind, SubagentEventKind::Spawn);
        assert_eq!(ev.agent_id.as_deref(), Some("agent-9"));
    }

    #[test]
    fn foreground_agent_response_without_id_is_not_spawn() {
        let p = parse(json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Agent",
            "tool_input": {"description": "Find code", "subagent_type": "Explore"},
            "tool_response": {"content": [{"type": "text", "text": "done"}]}
        }));
        assert!(!p.skip);
        assert!(p.subagent.is_none());
    }

    #[test]
    fn plain_payloads_carry_only_the_event() {
        let p = parse(json!({"hook_event_name": "UserPromptSubmit", "prompt": "hi"}));
        assert_eq!(
            p,
            HookPayload {
                hook_event: Some("UserPromptSubmit".into()),
                ..Default::default()
            }
        );
        assert_eq!(parse(json!("garbage")), HookPayload::default());
        assert_eq!(parse(json!({"hook_event_name": 5})), HookPayload::default());
    }

    #[test]
    fn long_fields_are_capped_on_char_boundary() {
        let long = "é".repeat(400);
        let p = parse(json!({
            "hook_event_name": "PreToolUse", "tool_name": "Agent",
            "tool_input": {"description": long}
        }));
        let d = p.subagent.unwrap().description.unwrap();
        assert!(d.len() <= MAX_PAYLOAD_FIELD);
        assert!(d.chars().all(|c| c == 'é'));
    }

    /// Without a daemon, a subagent event leaves the status file untouched.
    #[tokio::test]
    async fn fallback_skips_subagent_events() {
        // Call the fallback directly: `run` resolves the daemon socket from
        // `XDG_RUNTIME_DIR`, which other tests change while they run.
        let dir = std::env::temp_dir().join(format!("swamp-hook-fallback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(".swamp-status.json");
        for event in [
            json!({"hook_event_name": "SubagentStart", "agent_id": "a1", "agent_type": "Explore"}),
            json!({"hook_event_name": "SubagentStop", "agent_id": "a1", "agent_type": "Explore"}),
            json!({"hook_event_name": "PreToolUse", "agent_id": "a1", "agent_type": "Explore"}),
        ] {
            write_status_file(&dir, "main".into(), "working", None, None, &parse(event))
                .await
                .unwrap();
            assert!(!file.exists());
        }

        // A plain status, and the parent's own `Agent` call, still write it.
        let spawn = parse(json!({
            "hook_event_name": "PreToolUse", "tool_name": "Agent",
            "tool_input": {"description": "Look"}
        }));
        write_status_file(&dir, "main".into(), "working", None, None, &spawn)
            .await
            .unwrap();
        let map: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(map["main"]["status"], "working");
        assert!(
            !String::from_utf8(std::fs::read(&file).unwrap())
                .unwrap()
                .contains("Look")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
