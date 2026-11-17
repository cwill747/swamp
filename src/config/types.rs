/// User-tunable swamp settings, loaded from `$XDG_CONFIG_HOME/swamp/config.toml`.
/// Every field has a default so a missing or partial file still yields a usable
/// config (`#[serde(default)]` fills the gaps). Malformed config is rejected by
/// the loader.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct SwampConfig {
    pub dashboard: DashboardConfig,
    pub harness: HarnessConfig,
    pub logging: LoggingConfig,
    pub slots: SlotsConfig,
    pub badges: BadgesConfig,
    pub notifications: NotificationsConfig,
    pub ssh_hosts: std::collections::BTreeMap<String, SshHostConfig>,
}

/// Native desktop alerts. The notifier is an executable, never a shell command.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotificationsConfig {
    pub enabled: bool,
    pub waiting: bool,
    pub finished: bool,
    pub command: Option<String>,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            waiting: true,
            finished: true,
            command: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SshHostConfig {
    pub target: String,
    /// Override the executable when a noninteractive SSH shell cannot find it.
    pub swamp_path: Option<String>,
}

impl SshHostConfig {
    pub fn validate(&self, id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            !id.is_empty()
                && id.len() <= 64
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
            "invalid SSH host name {id:?}; use up to 64 letters, digits, dots, hyphens or underscores"
        );
        anyhow::ensure!(
            !self.target.is_empty()
                && !self.target.starts_with('-')
                && !self
                    .target
                    .chars()
                    .any(|c| c.is_control() || c.is_whitespace()),
            "invalid SSH target for {id}"
        );
        if let Some(path) = &self.swamp_path {
            anyhow::ensure!(
                std::path::Path::new(path).is_absolute() && !path.chars().any(char::is_control),
                "swamp_path for {id} must be an absolute path"
            );
        }
        Ok(())
    }
}

/// Default number of slot worktrees in the pool (`work1`..`work5`).
pub const DEFAULT_POOL_SIZE: u32 = 5;

/// Slot pool settings. `pool_size` is the most slot worktrees swamp creates; 0
/// turns the pool off so every allocation gets its own overflow worktree.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct SlotsConfig {
    pub pool_size: u32,
    pub autoclean: bool,
    pub hooks: SlotHooks,
}

impl Default for SlotsConfig {
    fn default() -> Self {
        Self {
            pool_size: DEFAULT_POOL_SIZE,
            autoclean: true,
            hooks: SlotHooks::default(),
        }
    }
}

/// Commands that swamp runs around slot allocation and release.
///
/// ```toml
/// [[slots.hooks.pre-release]]
/// run = "bazel shutdown"
///
/// [[slots.hooks.post-alloc]]
/// run = "direnv allow"
/// timeout_secs = 60
/// ```
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SlotHooks {
    /// Run in the worktree after an allocation checks a branch out.
    #[serde(rename = "post-alloc")]
    pub post_alloc: Vec<HookCommand>,
    /// Run in the slot before a release detaches it.
    #[serde(rename = "pre-release")]
    pub pre_release: Vec<HookCommand>,
}

/// Default time limit for one hook command.
pub const DEFAULT_HOOK_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookCommand {
    /// Shell command, run with `sh -c`.
    pub run: String,
    /// Time limit in seconds (default 300). Swamp kills the command's process
    /// group when the limit passes.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// The per-repository config at `<git-common-dir>/swamp/config.toml`. Each
/// field that is set overrides the user config for that repository only.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct RepoConfig {
    pub slots: RepoSlotsConfig,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct RepoSlotsConfig {
    pub pool_size: Option<u32>,
    pub autoclean: Option<bool>,
    pub hooks: SlotHooks,
}

impl SlotsConfig {
    /// Apply a per-repository config on top of the user setting. A set
    /// `pool_size` replaces the user value. Hooks add up: user hooks run
    /// first, then the repository's.
    pub fn merged_with(mut self, repo: &RepoSlotsConfig) -> Self {
        if let Some(enabled) = repo.autoclean {
            self.autoclean = enabled;
        }
        if let Some(size) = repo.pool_size {
            self.pool_size = size;
        }
        self.hooks
            .post_alloc
            .extend(repo.hooks.post_alloc.iter().cloned());
        self.hooks
            .pre_release
            .extend(repo.hooks.pre_release.iter().cloned());
        self
    }
}

/// Attention badges in Zellij tab names: `● <worktree>` for a worktree whose
/// agent waits, and `dashboard ●<n>` for waiting worktrees in other repos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub struct BadgesConfig {
    pub enabled: bool,
}

impl Default for BadgesConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Minimum severity recorded in the diagnostic log. Mirrors the standard
/// tracing levels; an unknown value fails TOML parsing (caught by `load_config`)
/// rather than silently defaulting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    /// The directive string understood by `tracing_subscriber::EnvFilter`.
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }
}

/// Diagnostic-logging knobs. `level` is the common case (a bare severity);
/// `filter` is the escape hatch accepting full `EnvFilter` directive syntax
/// (e.g. `swamp::zellij=debug,info`) and, when set, wins over `level`. Both are
/// overridden by the `RUST_LOG` environment variable at init time.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    pub level: LogLevel,
    pub filter: Option<String>,
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

/// Dashboard layout and sizing knobs. The percentage settings remain available
/// for the explicit classic layout.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct DashboardConfig {
    /// Generated layout: the Herdr-style workspace or the legacy classic grid.
    pub layout: DashboardLayout,
    /// Width in cells of the workspace sidebar (16..=60).
    #[serde(deserialize_with = "deserialize_sidebar_width")]
    pub sidebar_width: u16,
    /// Width (%) of the left column (worktrees + resources panes).
    pub worktrees_column: u16,
    /// Width (%) of the middle column (ai-status + pr-status panes).
    pub ai_column: u16,
    /// Width (%) of the right column (interactive shell).
    pub shell_column: u16,
}

fn deserialize_sidebar_width<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{Error, Visitor};
    use std::fmt;

    struct WidthVisitor;
    impl Visitor<'_> for WidthVisitor {
        type Value = u16;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an integer from 16 through 60")
        }

        fn visit_i64<E: Error>(self, value: i64) -> Result<Self::Value, E> {
            if (16..=60).contains(&value) {
                Ok(value as u16)
            } else {
                Err(E::custom(
                    "[dashboard] sidebar_width must be between 16 and 60",
                ))
            }
        }

        fn visit_u64<E: Error>(self, value: u64) -> Result<Self::Value, E> {
            if (16..=60).contains(&value) {
                Ok(value as u16)
            } else {
                Err(E::custom(
                    "[dashboard] sidebar_width must be between 16 and 60",
                ))
            }
        }
    }

    deserializer.deserialize_any(WidthVisitor)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DashboardLayout {
    #[default]
    Workspace,
    Classic,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            layout: DashboardLayout::Workspace,
            sidebar_width: 32,
            worktrees_column: 33,
            ai_column: 34,
            shell_column: 33,
        }
    }
}

impl DashboardConfig {
    /// Validate values whose ranges are part of the generated layout contract.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (16..=60).contains(&self.sidebar_width),
            "[dashboard] sidebar_width must be between 16 and 60"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_CONFIG_TOML: &str = include_str!("config.toml");

    #[test]
    fn notification_defaults_and_overrides() {
        for text in ["", "[notifications]", DEFAULT_CONFIG_TOML] {
            let cfg: SwampConfig = toml::from_str(text).unwrap();
            assert!(
                cfg.notifications.enabled
                    && cfg.notifications.waiting
                    && cfg.notifications.finished
            );
            assert!(cfg.notifications.command.is_none());
        }
        let cfg: SwampConfig = toml::from_str("[notifications]\nenabled = false\nwaiting = false\nfinished = false\ncommand = '/custom/notifier'").unwrap();
        assert!(
            !cfg.notifications.enabled && !cfg.notifications.waiting && !cfg.notifications.finished
        );
        assert_eq!(
            cfg.notifications.command.as_deref(),
            Some("/custom/notifier")
        );
        assert!(toml::from_str::<SwampConfig>("[notifications]\nenabeld = false").is_err());
    }

    #[test]
    fn badges_are_on_unless_the_config_turns_them_off() {
        // Absent section.
        let cfg: SwampConfig = toml::from_str("[dashboard]\nshell_column = 20\n").unwrap();
        assert!(cfg.badges.enabled);
        // Present section, with and without the key.
        let cfg: SwampConfig = toml::from_str("[badges]\nenabled = true\n").unwrap();
        assert!(cfg.badges.enabled);
        let cfg: SwampConfig = toml::from_str("[badges]\n").unwrap();
        assert!(cfg.badges.enabled);
        let cfg: SwampConfig = toml::from_str("[badges]\nenabled = false\n").unwrap();
        assert!(!cfg.badges.enabled);
        // The default config that `swamp init` writes.
        let cfg: SwampConfig = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
        assert!(cfg.badges.enabled);
        assert!(DEFAULT_CONFIG_TOML.contains("[badges]"));
    }

    #[test]
    fn default_config_toml_parses_to_defaults() {
        let cfg: SwampConfig = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
        let def = DashboardConfig::default();
        assert_eq!(cfg.dashboard.layout, DashboardLayout::Workspace);
        assert_eq!(cfg.dashboard.sidebar_width, 32);
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
        assert_eq!(cfg.dashboard.layout, DashboardLayout::Workspace);
        assert_eq!(cfg.dashboard.sidebar_width, 32);
        // An absent [harness] block defaults to Claude.
        assert_eq!(cfg.harness.default, HarnessSetting::Claude);
    }

    #[test]
    fn dashboard_layout_config_keeps_classic_knobs_and_validates_sidebar_width() {
        let cfg: SwampConfig = toml::from_str(
            "[dashboard]\nlayout = 'classic'\nsidebar_width = 60\nworktrees_column = 25\n",
        )
        .unwrap();
        assert_eq!(cfg.dashboard.layout, DashboardLayout::Classic);
        assert_eq!(cfg.dashboard.sidebar_width, 60);
        assert_eq!(cfg.dashboard.worktrees_column, 25);
        assert!(cfg.dashboard.validate().is_ok());
        for width in [-1_i64, 0, 15, 61, 65_536, i64::MAX] {
            let error =
                toml::from_str::<SwampConfig>(&format!("[dashboard]\nsidebar_width = {width}\n"))
                    .unwrap_err()
                    .to_string();
            assert!(error.contains("between 16 and 60"), "{width}: {error}");
        }
        assert!(toml::from_str::<SwampConfig>("[dashboard]\nlayout = 'other'\n").is_err());
    }

    #[test]
    fn logging_defaults_to_info_and_no_filter() {
        let cfg = SwampConfig::default();
        assert_eq!(cfg.logging.level, LogLevel::Info);
        assert!(cfg.logging.filter.is_none());
        // An absent [logging] block also yields the info default.
        let cfg: SwampConfig = toml::from_str("[harness]\ndefault = \"claude\"\n").unwrap();
        assert_eq!(cfg.logging.level, LogLevel::Info);
    }

    #[test]
    fn logging_level_and_filter_parse() {
        let cfg: SwampConfig =
            toml::from_str("[logging]\nlevel = \"debug\"\nfilter = \"swamp::zellij=trace\"\n")
                .unwrap();
        assert_eq!(cfg.logging.level, LogLevel::Debug);
        assert_eq!(cfg.logging.filter.as_deref(), Some("swamp::zellij=trace"));
        assert_eq!(LogLevel::Debug.as_str(), "debug");
    }

    #[test]
    fn logging_rejects_unknown_level() {
        let err = toml::from_str::<SwampConfig>("[logging]\nlevel = \"verbose\"\n").unwrap_err();
        assert!(err.to_string().contains("verbose") || err.to_string().contains("unknown"));
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
}
