use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

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

/// Write `content` to `path` atomically (via a temp file in the same directory)
/// and, if `backup_path` is Some, copy the original to that path first.
fn atomic_write(path: &Path, content: &str, backup_path: Option<&Path>) -> Result<()> {
    if let Some(bak) = backup_path {
        std::fs::copy(path, bak)
            .with_context(|| format!("backup {} -> {}", path.display(), bak.display()))?;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = parent.join(format!(".swamp-tmp-{}", std::process::id()));
    std::fs::write(&tmp, content).with_context(|| format!("write tmp {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename {} -> {}", tmp.display(), path.display())
    })?;
    Ok(())
}

/// Install or update swamp's Codex `notify` hook in Codex's `config.toml`, so
/// Codex panes report agent status back to swamp. Mirrors [`ensure_claude_hooks`]:
/// a read-only file (nix/home-manager) is left untouched with a warning.
pub fn ensure_codex_notify() -> Result<()> {
    let path = codex_config_path();
    let original = std::fs::read_to_string(&path).ok();

    let mut doc = match &original {
        None => toml_edit::DocumentMut::new(),
        Some(text) => text.parse::<toml_edit::DocumentMut>().with_context(|| {
            format!(
                "{} is malformed TOML; fix or remove it before running swamp",
                path.display()
            )
        })?,
    };

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

    // Back up the original when it exists and content is actually changing.
    let backup = original.as_ref().map(|_| {
        let mut bak = path.clone().into_os_string();
        bak.push(".bak");
        PathBuf::from(bak)
    });

    atomic_write(&path, &doc.to_string(), backup.as_deref())?;

    println!(
        "swamp: {} Codex notify in {}",
        if original.is_some() {
            "updated"
        } else {
            "wrote"
        },
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp_dir() -> PathBuf {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("swamp-codex-test-{}-{id}", std::process::id()));
        fs::create_dir_all(&p).unwrap();
        p
    }

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

    // ── ensure_codex_notify integration tests ────────────────────────────────

    // Serializes tests that mutate the process-global CODEX_HOME.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_codex_dir<F: FnOnce(PathBuf)>(f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tmp_dir();
        unsafe { std::env::set_var("CODEX_HOME", &dir) };
        f(dir);
        unsafe { std::env::remove_var("CODEX_HOME") };
    }

    #[test]
    fn malformed_config_returns_error_and_file_untouched() {
        with_codex_dir(|dir| {
            let path = dir.join("config.toml");
            let bad = "notify = [\n"; // unclosed array — invalid TOML
            fs::write(&path, bad).unwrap();

            let err = ensure_codex_notify().unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("malformed") || msg.contains("config.toml"),
                "error should mention malformed/path, got: {msg}"
            );
            // File must be left completely untouched.
            assert_eq!(fs::read_to_string(&path).unwrap(), bad);
        });
    }

    #[test]
    fn fresh_file_is_created_without_backup() {
        with_codex_dir(|dir| {
            let path = dir.join("config.toml");
            let bak = dir.join("config.toml.bak");
            assert!(!path.exists());
            ensure_codex_notify().unwrap();
            assert!(path.exists(), "config.toml should be created");
            assert!(!bak.exists(), "no backup when creating fresh file");
        });
    }

    #[test]
    fn existing_file_gets_backup_on_modification() {
        with_codex_dir(|dir| {
            let path = dir.join("config.toml");
            let bak = dir.join("config.toml.bak");
            // Write a valid but notify-less config file.
            fs::write(&path, "model = \"o3\"\n").unwrap();
            ensure_codex_notify().unwrap();
            assert!(path.exists());
            assert!(
                bak.exists(),
                "backup should exist after modifying existing file"
            );
            // Backup should contain the original content.
            let bak_content = fs::read_to_string(&bak).unwrap();
            assert!(bak_content.contains("model"));
        });
    }

    #[test]
    fn no_backup_when_already_correct() {
        with_codex_dir(|dir| {
            let bak = dir.join("config.toml.bak");
            // Run once to populate.
            ensure_codex_notify().unwrap();
            // Remove any backup (there was none; fresh file).
            let _ = fs::remove_file(&bak);
            // Run again — should be idempotent: no write, no backup.
            ensure_codex_notify().unwrap();
            assert!(!bak.exists(), "no backup on a no-op second run");
        });
    }
}
