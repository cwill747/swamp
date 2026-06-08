use anyhow::{Context, Result};
use std::path::PathBuf;

use super::paths::is_read_only;

/// Path to Codex's `config.toml` (honors `CODEX_HOME`, default `~/.codex`).
fn codex_config_path() -> PathBuf {
    let base = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"));
    base.join("config.toml")
}

/// The `notify` array swamp wants Codex to call on each `agent-turn-complete`.
/// `swamp codex-notify` parses the JSON payload Codex appends and forwards the
/// status to the daemon.
const CODEX_NOTIFY: &[&str] = &["swamp", "codex-notify"];

/// Set Codex's `notify` to swamp's forwarder in `doc`, preserving every other
/// key/comment. Returns `true` if the document changed.
fn apply_codex_notify(doc: &mut toml_edit::DocumentMut) -> bool {
    let desired = {
        let mut arr = toml_edit::Array::new();
        for s in CODEX_NOTIFY {
            arr.push(*s);
        }
        arr
    };
    // Already pointing at swamp's forwarder? Leave it (and its formatting) alone.
    if let Some(existing) = doc.get("notify").and_then(|v| v.as_array())
        && existing.len() == desired.len()
        && existing
            .iter()
            .zip(CODEX_NOTIFY)
            .all(|(v, want)| v.as_str() == Some(*want))
    {
        return false;
    }
    doc["notify"] = toml_edit::value(desired);
    true
}

/// Install or update swamp's Codex `notify` hook in Codex's `config.toml`, so
/// Codex panes report agent status back to swamp. Mirrors [`ensure_claude_hooks`]:
/// a read-only file (nix/home-manager) is left untouched with a warning.
pub fn ensure_codex_notify() -> Result<()> {
    let path = codex_config_path();
    let original = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc = original
        .parse::<toml_edit::DocumentMut>()
        .unwrap_or_else(|_| toml_edit::DocumentMut::new());

    if !apply_codex_notify(&mut doc) {
        println!(
            "swamp: Codex notify already up to date in {}",
            path.display()
        );
        return Ok(());
    }

    if path.exists() && is_read_only(&path) {
        println!(
            "swamp: {} is read-only (common under nix/home-manager); not modifying it.",
            path.display()
        );
        eprintln!(
            "swamp: warning: Codex notify is missing or out of date. Add \
             `notify = [\"swamp\", \"codex-notify\"]` to your Codex config manually."
        );
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    std::fs::write(&path, doc.to_string()).with_context(|| format!("write {}", path.display()))?;
    println!(
        "swamp: {} Codex notify in {}",
        if original.is_empty() {
            "wrote"
        } else {
            "updated"
        },
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_codex_notify_sets_and_is_idempotent() {
        let mut doc = "model = \"o3\"\n"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert!(apply_codex_notify(&mut doc), "notify must be added");
        // The unrelated key survives.
        assert_eq!(doc.get("model").and_then(|v| v.as_str()), Some("o3"));
        let arr = doc.get("notify").and_then(|v| v.as_array()).unwrap();
        let got: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(got, CODEX_NOTIFY);
        // Second pass is a no-op.
        assert!(!apply_codex_notify(&mut doc));
    }
}
