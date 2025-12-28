use v_utils::macros::{MyConfigPrimitives, Settings};

#[derive(Clone, Debug, Default, MyConfigPrimitives, Settings)]
pub struct AppConfig {
	pub claude_token: Option<String>,
}
