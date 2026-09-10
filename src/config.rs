use v_utils::macros::MyConfigPrimitives;
#[cfg(feature = "cli")]
use v_utils::macros::Settings;

#[derive(Clone, Debug, Default, MyConfigPrimitives)]
#[cfg_attr(feature = "cli", derive(Settings))]
pub struct AppConfig {
	/// `sk-ant-oat01-…`, as `CLAUDE_CODE_OAUTH_TOKEN` holds it. An `sk-ant-api03-…` key does not work
	/// here: Claude is reached through the `claude` CLI so that it bills the subscription, not credits.
	pub claude_token: Option<String>,
	pub openai_token: Option<String>,
}
