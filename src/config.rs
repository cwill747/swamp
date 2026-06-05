use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const LAZYGIT_CONFIG: &str = include_str!("config/lazygit.yml");
const DEFAULT_CONFIG_TOML: &str = include_str!("config/config.toml");

/// User-tunable swamp settings, loaded from `$XDG_CONFIG_HOME/swamp/config.toml`.
/// Every field has a default so a missing or partial file still yields a usable
/// config (`#[serde(default)]` fills the gaps).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct SwampConfig {
    pub dashboard: DashboardConfig,
    pub harness: HarnessConfig,
}

/// The AI coding agent launched in a worktree's agent pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    Claude,
    Codex,
}

impl Harness {
    /// The binary name resolved on `$PATH` to launch this harness.
    pub fn bin(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
        }
    }

    /// Short human label for the UI.
    pub fn label(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
        }
    }
}

/// The tri-state harness preference: pin every pane to one agent, or `Choose`
/// to honor each worktree's own override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HarnessSetting {
    #[default]
    Claude,
    Codex,
    Choose,
}

/// Harness selection knobs. `default` is the repo-wide preference; in `choose`
/// mode each worktree's persisted override (see `.swamp-status.json`) wins.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct HarnessConfig {
    pub default: HarnessSetting,
}

/// Resolve the effective harness for one worktree: a pinned setting forces its
/// agent; `choose` falls back to the worktree's override, defaulting to Claude.
pub fn resolve_harness(setting: HarnessSetting, override_: Option<Harness>) -> Harness {
    match setting {
        HarnessSetting::Claude => Harness::Claude,
        HarnessSetting::Codex => Harness::Codex,
        HarnessSetting::Choose => override_.unwrap_or(Harness::Claude),
    }
}

/// Dashboard layout knobs. The dashboard is three side-by-side columns; these
/// percentages set each column's width and should sum to ~100.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct DashboardConfig {
    /// Width (%) of the left column (worktrees + resources panes).
    pub worktrees_column: u16,
    /// Width (%) of the middle column (ai-status + pr-status panes).
    pub ai_column: u16,
    /// Width (%) of the right column (interactive shell).
    pub shell_column: u16,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            worktrees_column: 33,
            ai_column: 34,
            shell_column: 33,
        }
    }
}

/// Resolved paths to the swamp-managed config files, plus the loaded settings.
pub struct ConfigPaths {
    pub lazygit: PathBuf,
    pub dashboard: DashboardConfig,
    /// Repo-wide harness preference (per-worktree overrides apply in `choose`).
    pub harness: HarnessSetting,
}

/// Returns the `$XDG_CONFIG_HOME/swamp` directory (falls back to `~/.config/swamp`).
fn swamp_config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("swamp")
}

/// Path to the user's TOML config.
fn config_toml_path() -> PathBuf {
    swamp_config_dir().join("config.toml")
}

/// Load the user's `config.toml`. A missing file yields defaults; a malformed
/// one yields defaults plus a warning, so a typo never blocks a launch.
pub fn load_config() -> SwampConfig {
    let path = config_toml_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
            eprintln!(
                "swamp: warning: failed to parse {}: {e}; using defaults",
                path.display()
            );
            SwampConfig::default()
        }),
        Err(_) => SwampConfig::default(),
    }
}

/// Write the default `config.toml` if it doesn't exist yet. Unlike the embedded
/// configs, this is user-owned: an existing file is never clobbered. Returns the
/// path and whether a new file was written.
pub fn ensure_config_toml() -> Result<(PathBuf, bool)> {
    let path = config_toml_path();
    if path.exists() {
        return Ok((path, false));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    std::fs::write(&path, DEFAULT_CONFIG_TOML)
        .with_context(|| format!("write {}", path.display()))?;
    Ok((path, true))
}

/// Write `content` to `path` only if the file is absent or differs.
/// Returns `true` if the file was (re)written.
fn write_if_changed(path: &PathBuf, content: &str) -> Result<bool> {
    if let Ok(existing) = std::fs::read_to_string(path)
        && existing == content
    {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    std::fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

/// Ensure the embedded swamp configs are present on disk and load user settings.
/// Writes managed files only when absent or differing (idempotent).
pub fn ensure_configs() -> Result<ConfigPaths> {
    let dir = swamp_config_dir();
    let lazygit = dir.join("lazygit.yml");

    write_if_changed(&lazygit, LAZYGIT_CONFIG)?;

    let cfg = load_config();
    Ok(ConfigPaths {
        lazygit,
        dashboard: cfg.dashboard,
        harness: cfg.harness.default,
    })
}

// ---------------------------------------------------------------------------
// Claude Code hook management
// ---------------------------------------------------------------------------

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

/// Whether `path` can't be written: a symlink into the immutable nix store
/// (home-manager) or a file whose permissions are read-only.
fn is_read_only(path: &Path) -> bool {
    if let Ok(target) = std::fs::read_link(path)
        && target.starts_with("/nix/store")
    {
        return true;
    }
    std::fs::metadata(path).is_ok_and(|m| m.permissions().readonly())
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

// ---------------------------------------------------------------------------
// Codex notify management
// ---------------------------------------------------------------------------

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

/// `swamp init`: write the default TOML config, refresh the embedded configs,
/// and install/update Claude Code hooks + Codex notify.
pub fn init() -> Result<()> {
    let (cfg_path, wrote) = ensure_config_toml()?;
    println!(
        "swamp: config {} at {}",
        if wrote { "written" } else { "already present" },
        cfg_path.display()
    );

    let paths = ensure_configs()?;
    println!("swamp: lazygit config at {}", paths.lazygit.display());

    ensure_claude_hooks()?;
    ensure_codex_notify()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_dir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("swamp-test-{}-{}", std::process::id(), line!()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn write_if_changed_creates_file() {
        let dir = tmp_dir();
        let path = dir.join("sub").join("file.toml");
        let wrote = write_if_changed(&path, "hello").unwrap();
        assert!(wrote, "should have written a new file");
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn write_if_changed_idempotent() {
        let dir = tmp_dir();
        let path = dir.join("idempotent.toml");
        write_if_changed(&path, "content").unwrap();
        let wrote = write_if_changed(&path, "content").unwrap();
        assert!(!wrote, "should NOT rewrite when content matches");
    }

    #[test]
    fn write_if_changed_overwrites_on_mismatch() {
        let dir = tmp_dir();
        let path = dir.join("mismatch.toml");
        write_if_changed(&path, "old").unwrap();
        let wrote = write_if_changed(&path, "new").unwrap();
        assert!(wrote, "should rewrite when content differs");
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn ensure_configs_returns_expected_paths() {
        // Point XDG_CONFIG_HOME at a temp dir so we don't pollute the real one.
        let base = tmp_dir();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };
        let paths = ensure_configs().unwrap();
        assert_eq!(paths.lazygit, base.join("swamp").join("lazygit.yml"));
        assert!(paths.lazygit.exists());
    }

    #[test]
    fn ensure_configs_idempotent() {
        let base = tmp_dir();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };
        ensure_configs().unwrap();
        // Second call must not error and paths must still exist.
        let paths = ensure_configs().unwrap();
        assert!(paths.lazygit.exists());
    }

    #[test]
    fn default_config_toml_parses_to_defaults() {
        let cfg: SwampConfig = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
        let def = DashboardConfig::default();
        assert_eq!(cfg.dashboard.worktrees_column, def.worktrees_column);
        assert_eq!(cfg.dashboard.ai_column, def.ai_column);
        assert_eq!(cfg.dashboard.shell_column, def.shell_column);
        assert_eq!(cfg.harness.default, HarnessSetting::Claude);
    }

    #[test]
    fn partial_config_fills_defaults() {
        let cfg: SwampConfig = toml::from_str("[dashboard]\nshell_column = 20\n").unwrap();
        assert_eq!(cfg.dashboard.shell_column, 20);
        // Unset fields keep their defaults.
        assert_eq!(cfg.dashboard.worktrees_column, 33);
        assert_eq!(cfg.dashboard.ai_column, 34);
        // An absent [harness] block defaults to Claude.
        assert_eq!(cfg.harness.default, HarnessSetting::Claude);
    }

    #[test]
    fn harness_setting_parses() {
        let cfg: SwampConfig = toml::from_str("[harness]\ndefault = \"codex\"\n").unwrap();
        assert_eq!(cfg.harness.default, HarnessSetting::Codex);
        let cfg: SwampConfig = toml::from_str("[harness]\ndefault = \"choose\"\n").unwrap();
        assert_eq!(cfg.harness.default, HarnessSetting::Choose);
    }

    #[test]
    fn resolve_harness_pins_and_chooses() {
        // Pinned settings ignore the override.
        assert_eq!(
            resolve_harness(HarnessSetting::Claude, Some(Harness::Codex)),
            Harness::Claude
        );
        assert_eq!(
            resolve_harness(HarnessSetting::Codex, Some(Harness::Claude)),
            Harness::Codex
        );
        // Choose honors the override, defaulting to Claude when absent.
        assert_eq!(
            resolve_harness(HarnessSetting::Choose, Some(Harness::Codex)),
            Harness::Codex
        );
        assert_eq!(
            resolve_harness(HarnessSetting::Choose, None),
            Harness::Claude
        );
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
