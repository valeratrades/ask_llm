/// A model was selected whose provider has no key in the config, the builder, or the environment.
#[derive(Debug, miette::Diagnostic, thiserror::Error)]
#[error("`{model}` is served by {provider}, which has no api key configured")]
#[diagnostic(code(ask_llm::missing_token))]
pub struct MissingToken {
	pub provider: &'static str,
	pub model: &'static str,
	#[help]
	pub help: String,
}

impl MissingToken {
	pub(crate) fn new(provider: &'static str, model: &'static str, config_key: &'static str, env_var: &'static str, builder: &'static str) -> Self {
		Self {
			provider,
			model,
			help: format!("hand it over with `Client::{builder}(…)`, put `{config_key}` in ~/.config/ask_llm.nix, or export {env_var}"),
		}
	}
}
