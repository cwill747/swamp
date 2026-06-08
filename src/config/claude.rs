use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::PathBuf;

use super::paths::is_read_only;

/// The Claude Code events swamp wires up, the status each reports, and the
/// matcher (if any) for the hook group.
const HOOK_EVENTS: &[(&str, &str, Option<&str>)] = &[
    ("UserPromptSubmit", "working", None),
    ("PreToolUse", "working", Some("")),
    ("PostToolUse", "working", None),
    (
        "Notification",
        "waiting",
        Some("permission_prompt|elicitation_dialog"),
    ),
    ("Stop", "idle", None),
];

/// Path to Claude Code's user `settings.json` (honors `CLAUDE_CONFIG_DIR`).
fn claude_settings_path() -> PathBuf {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude")))
        .unwrap_or_else(|| PathBuf::from(".claude"));
    base.join("settings.json")
}

/// The recommended swamp hook command: parses Claude's JSON stdin for the
/// session name/id and forwards them to `swamp hook <status>`.
fn swamp_hook_command(status: &str) -> String {
    format!(
        "input=$(cat); swamp hook {status} \
         --session-name \"$(echo \"$input\" | jq -r '.session_name // empty')\" \
         --session-id \"$(echo \"$input\" | jq -r '.session_id // empty')\" \
         >/dev/null 2>&1 || true"
    )
}

/// A command string belongs to swamp if it invokes `swamp hook`.
fn is_swamp_command(cmd: &str) -> bool {
    cmd.contains("swamp hook")
}

/// Merge swamp's hooks into `settings`, preserving any unrelated hooks the user
/// has configured. Existing swamp hooks are updated in place; missing ones are
/// appended. Returns `true` if anything changed.
fn apply_swamp_hooks(settings: &mut Value) -> bool {
    if !settings.is_object() {
        *settings = json!({});
    }
    let obj = settings.as_object_mut().unwrap();

    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let hooks = hooks.as_object_mut().unwrap();

    let mut changed = false;
    for (event, status, matcher) in HOOK_EVENTS {
        let desired = swamp_hook_command(status);

        let arr = match hooks.get_mut(*event) {
            Some(v) if v.is_array() => v.as_array_mut().unwrap(),
            _ => {
                hooks.insert(event.to_string(), json!([]));
                changed = true;
                hooks.get_mut(*event).unwrap().as_array_mut().unwrap()
            }
        };

        // Update any existing swamp hook command in this event, and refresh the
        // enclosing group's matcher so a stale/missing matcher still fires for
        // every intended case.
        let mut found = false;
        for group in arr.iter_mut() {
            let has_swamp = group
                .get("hooks")
                .and_then(|h| h.as_array())
                .is_some_and(|inner| {
                    inner.iter().any(|hook| {
                        hook.get("command")
                            .and_then(|c| c.as_str())
                            .is_some_and(is_swamp_command)
                    })
                });
            if !has_swamp {
                continue;
            }
            found = true;

            // Bring the group's matcher in line with the desired one.
            let group_obj = group.as_object_mut().unwrap();
            match matcher {
                Some(m) => {
                    if group_obj.get("matcher").and_then(|v| v.as_str()) != Some(*m) {
                        group_obj.insert("matcher".into(), json!(m));
                        changed = true;
                    }
                }
                None => {
                    if group_obj.remove("matcher").is_some() {
                        changed = true;
                    }
                }
            }

            let inner = group_obj
                .get_mut("hooks")
                .and_then(|h| h.as_array_mut())
                .unwrap();
            for hook in inner.iter_mut() {
                let is_swamp = hook
                    .get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(is_swamp_command);
                if is_swamp
                    && hook.get("command").and_then(|c| c.as_str()) != Some(desired.as_str())
                {
                    hook["command"] = json!(desired);
                    changed = true;
                }
            }
        }

        if !found {
            let mut group = serde_json::Map::new();
            if let Some(m) = matcher {
                group.insert("matcher".into(), json!(m));
            }
            group.insert(
                "hooks".into(),
                json!([{ "type": "command", "command": desired }]),
            );
            arr.push(Value::Object(group));
            changed = true;
        }
    }
    changed
}

/// Install or update swamp's Claude Code hooks in the user's `settings.json`.
/// If the file is read-only (common under nix/home-manager), don't attempt a
/// write: log that, and warn if the existing hooks are out of date.
pub fn ensure_claude_hooks() -> Result<()> {
    let path = claude_settings_path();
    let original = std::fs::read_to_string(&path).ok();
    let mut settings: Value = original
        .as_deref()
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or_else(|| json!({}));

    let changed = apply_swamp_hooks(&mut settings);
    if !changed {
        println!(
            "swamp: Claude hooks already up to date in {}",
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
            "swamp: warning: Claude hooks are missing or out of date. Add swamp's hooks \
             manually — see the \"Claude Code hooks\" section of the README."
        );
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    let mut text = serde_json::to_string_pretty(&settings)?;
    text.push('\n');
    std::fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
    println!(
        "swamp: {} Claude hooks in {}",
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

    #[test]
    fn apply_swamp_hooks_adds_all_events() {
        let mut settings = json!({});
        assert!(apply_swamp_hooks(&mut settings));
        let hooks = &settings["hooks"];
        for (event, _, _) in HOOK_EVENTS {
            let arr = hooks[*event].as_array().expect("event present");
            assert!(
                arr.iter().any(|g| g["hooks"][0]["command"]
                    .as_str()
                    .is_some_and(is_swamp_command)),
                "event {event} should carry a swamp hook"
            );
        }
    }

    #[test]
    fn apply_swamp_hooks_idempotent() {
        let mut settings = json!({});
        apply_swamp_hooks(&mut settings);
        // A second pass over already-correct settings is a no-op.
        assert!(!apply_swamp_hooks(&mut settings));
    }

    #[test]
    fn apply_swamp_hooks_updates_stale_command() {
        let mut settings = json!({
            "hooks": {
                "Stop": [
                    { "hooks": [ { "type": "command", "command": "swamp hook idle" } ] }
                ]
            }
        });
        assert!(
            apply_swamp_hooks(&mut settings),
            "stale command must update"
        );
        let cmd = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(cmd, swamp_hook_command("idle"));
        // Only one swamp group — we updated in place, didn't append a duplicate.
        assert_eq!(settings["hooks"]["Stop"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn apply_swamp_hooks_refreshes_stale_matcher() {
        // An existing swamp hook whose group has a too-narrow matcher should get
        // its matcher refreshed, not just its command.
        let mut settings = json!({
            "hooks": {
                "Notification": [
                    {
                        "matcher": "permission_prompt",
                        "hooks": [ { "type": "command", "command": swamp_hook_command("waiting") } ]
                    }
                ]
            }
        });
        assert!(
            apply_swamp_hooks(&mut settings),
            "stale matcher must trigger an update"
        );
        let group = &settings["hooks"]["Notification"][0];
        assert_eq!(
            group["matcher"].as_str().unwrap(),
            "permission_prompt|elicitation_dialog"
        );
        // No duplicate group appended.
        assert_eq!(
            settings["hooks"]["Notification"].as_array().unwrap().len(),
            1
        );
        // Idempotent on a second pass.
        assert!(!apply_swamp_hooks(&mut settings));
    }

    #[test]
    fn apply_swamp_hooks_drops_unwanted_matcher() {
        // A swamp hook on a no-matcher event should have any stray matcher removed.
        let mut settings = json!({
            "hooks": {
                "Stop": [
                    {
                        "matcher": "something",
                        "hooks": [ { "type": "command", "command": swamp_hook_command("idle") } ]
                    }
                ]
            }
        });
        assert!(
            apply_swamp_hooks(&mut settings),
            "stray matcher must be removed"
        );
        assert!(settings["hooks"]["Stop"][0].get("matcher").is_none());
        assert!(!apply_swamp_hooks(&mut settings));
    }

    #[test]
    fn apply_swamp_hooks_preserves_foreign_hooks() {
        let mut settings = json!({
            "hooks": {
                "Stop": [
                    { "hooks": [ { "type": "command", "command": "echo hi" } ] }
                ]
            }
        });
        apply_swamp_hooks(&mut settings);
        let stop = settings["hooks"]["Stop"].as_array().unwrap();
        // The user's hook survives and ours is appended alongside.
        assert!(stop.iter().any(|g| g["hooks"][0]["command"] == "echo hi"));
        assert!(stop.iter().any(|g| {
            g["hooks"][0]["command"]
                .as_str()
                .is_some_and(is_swamp_command)
        }));
    }
}
