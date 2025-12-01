use std::sync::OnceLock;

use v_utils::macros::{MyConfigPrimitives, Settings};

#[derive(Clone, Debug, Default, MyConfigPrimitives, Settings)]
pub struct AppConfig {
	pub claude_token: Option<String>,
}

static CONFIG: OnceLock<AppConfig> = OnceLock::new();

/// Initialize config with CLI flags. Call this once at startup.
/// If not called, `get_config` will load with default flags.
pub fn init(flags: SettingsFlags) -> eyre::Result<()> {
	let config = AppConfig::try_build(flags)?;
	CONFIG.set(config).map_err(|_| eyre::eyre!("Config already initialized"))?;
	Ok(())
}

/// Get the initialized config, or load with default flags if not initialized.
pub fn get() -> AppConfig {
	CONFIG.get_or_init(|| AppConfig::try_build(SettingsFlags::default()).unwrap_or_default()).clone()
}
