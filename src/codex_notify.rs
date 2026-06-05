//! Bridge Codex's `notify` hook into swamp's agent-status pipeline.
//!
//! Codex is configured (see `config::ensure_codex_notify`) to run
//! `swamp codex-notify` whenever it emits an event, appending a single JSON
//! payload argument. Codex's only event is `agent-turn-complete` (it has no
//! "turn started" signal), so a Codex pane reports **idle** when a turn finishes
//! and never a live "working" state.
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
    let session_id = json
        .get("thread-id")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // Best-effort: never let a notify failure surface to Codex.
    let _ = crate::hook::run("idle".to_string(), dir, None, session_id).await;
    Ok(())
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
        assert_eq!(json.get("thread-id").and_then(|v| v.as_str()), Some("t-1"));
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
