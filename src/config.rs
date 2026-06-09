mod claude;
mod codex;
mod paths;
mod types;

pub use claude::ensure_claude_hooks;
pub use codex::ensure_codex_notify;
#[allow(unused_imports)]
pub use paths::{ConfigPaths, ensure_config_toml, ensure_configs, load_config};
#[allow(unused_imports)]
pub use types::{
    DashboardConfig, Harness, HarnessConfig, HarnessSetting, LogLevel, LoggingConfig, SwampConfig,
    resolve_harness,
};

use anyhow::Result;

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
