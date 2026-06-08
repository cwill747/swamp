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

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_CONFIG_TOML: &str = include_str!("config.toml");

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
}
