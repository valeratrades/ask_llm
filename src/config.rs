use v_utils::macros::MyConfigPrimitives;
#[cfg(feature = "cli")]
use v_utils::macros::Settings;

#[derive(Clone, Debug, Default, MyConfigPrimitives)]
#[cfg_attr(feature = "cli", derive(Settings))]
pub struct AppConfig {
	pub claude_token: Option<String>,
	pub deepseek_token: Option<String>,
	pub openai_token: Option<String>,
}
