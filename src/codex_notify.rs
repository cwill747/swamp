//! Bridge Codex's `notify` hook into swamp's agent-status pipeline.
//!
//! Codex is configured (see `config::ensure_codex_notify`) to run
//! `swamp codex-notify` whenever it emits an event, appending a single JSON
//! payload argument. Codex's only event is `agent-turn-complete` (it has no
//! "turn started" signal), so a Codex pane reports **idle** when a turn finishes
//! and never a live "working" state. The explicit turn identity marks that idle
//! state as a completion and deduplicates desktop alerts.
//!
//! We parse the payload, resolve the worktree from its `cwd`, and forward the
//! status through the same path Claude hooks use ([`crate::hook::run`]). This is
//! best-effort: a malformed payload or an unhandled event type is a silent
//! no-op so Codex never sees a failing notify program.

use anyhow::Result;
use std::path::PathBuf;

/// Parse the JSON payload Codex appends and forward `agent-turn-complete` to the
/// daemon as an idle status for the originating worktree.
pub async fn run(payload: Vec<String>) -> Result<()> {
    // Codex passes the payload as a single JSON argument; rejoin defensively in
    // case the shell split it.
    let raw = payload.join(" ");
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Ok(());
    };

    // The only event Codex emits is `agent-turn-complete` → the turn finished,
    // so the agent is idle/awaiting input. Anything else is ignored.
    if json.get("type").and_then(|v| v.as_str()) != Some("agent-turn-complete") {
        return Ok(());
    }

    let dir = json.get("cwd").and_then(|v| v.as_str()).map(PathBuf::from);
    let completed_turn = completion_identity(&json);

    // Deliberately do *not* forward Codex's `thread-id`. The generic `session_id`
    // field is a Claude resume token: the daemon persists it and `launch.rs`
    // emits `claude --resume <id>` for any stored id when the effective harness
    // is Claude. Codex never resumes from it (it has no `--resume` path), so
    // passing a Codex thread id here would overwrite a worktree's saved Claude
    // UUID and break resuming that conversation after switching back to Claude.
    //
    // Best-effort: never let a notify failure surface to Codex.
    let _ = crate::hook::run(
        "idle".to_string(),
        dir,
        None,
        None,
        crate::hook::HookPayload {
            completed_turn: Some(completed_turn),
            ..Default::default()
        },
    )
    .await;
    Ok(())
}

fn completion_identity(json: &serde_json::Value) -> String {
    if let (Some(thread), Some(turn)) = (
        json.get("thread-id").and_then(|v| v.as_str()),
        json.get("turn-id").and_then(|v| v.as_str()),
    ) && !thread.is_empty()
        && !turn.is_empty()
        && thread.len() <= 256
        && turn.len() <= 256
    {
        // JSON encodes the pair unambiguously, even when IDs contain delimiters.
        return serde_json::to_string(&(thread, turn)).unwrap();
    }
    // Older payloads have no turn ID; each invocation represents a completion.
    format!(
        "legacy-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_turn_complete_fields() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"type":"agent-turn-complete","cwd":"/repo/feat","thread-id":"t-1"}"#,
        )
        .unwrap();
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("agent-turn-complete")
        );
        assert_eq!(json.get("cwd").and_then(|v| v.as_str()), Some("/repo/feat"));
    }

    #[test]
    fn completion_identity_is_stable_and_scoped_to_thread_and_turn() {
        let payload = serde_json::json!({"thread-id": "thread-a", "turn-id": "turn-1"});
        assert_eq!(completion_identity(&payload), completion_identity(&payload));
        for other in [
            serde_json::json!({"thread-id": "thread-b", "turn-id": "turn-1"}),
            serde_json::json!({"thread-id": "thread-a", "turn-id": "turn-2"}),
        ] {
            assert_ne!(completion_identity(&payload), completion_identity(&other));
        }
    }

    /// A malformed payload and an unhandled event type are both silent no-ops.
    #[tokio::test]
    async fn ignores_garbage_and_other_events() {
        run(vec!["not json".into()]).await.unwrap();
        run(vec![r#"{"type":"session-start"}"#.into()])
            .await
            .unwrap();
    }
}
