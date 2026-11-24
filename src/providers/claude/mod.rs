pub mod accounts;
mod cli;
mod http;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{ContentPart, Failure, Request, Source, Unrecoverable, Usage, config::AppConfig, usage::Card, wire::Answer};

#[derive(Debug, Eq, PartialEq)]
/// ref: https://docs.claude.com/en/docs/about-claude/models/all-models
pub enum ClaudeModel {
	Haiku5_5,
	Sonnet5_5,
	Opus5_5,
	Fable5_1,
}
impl ClaudeModel {
	pub const fn to_str(&self) -> &'static str {
		match self {
			ClaudeModel::Haiku5_5 => "claude-haiku-5-5",
			ClaudeModel::Sonnet5_5 => "claude-sonnet-5-5",
			ClaudeModel::Opus5_5 => "claude-opus-5-5",
			ClaudeModel::Fable5_1 => "claude-fable-5-1",
		}
	}
}

/// 5-minute cache writes, the only ones asked for. ref: https://platform.claude.com/docs/en/about-claude/pricing
pub(crate) fn prices(model: &str) -> Option<&'static [Card]> {
	#[rustfmt::skip]
	let cards: &'static [Card] = match model {
		"claude-haiku-5-5" => &[
			Card { up_to: 100_000, input: 0.1, cached_input: 0.01, cache_write: 0.125, output: 0.5 },
			Card { up_to: u32::MAX, input: 0.5, cached_input: 0.05, cache_write: 0.625, output: 2.5 },
		],
		"claude-sonnet-5-5" => &[Card { up_to: u32::MAX, input: 2., cached_input: 0.1, cache_write: 2.5, output: 10. }],
		"claude-opus-5-5" => &[Card { up_to: u32::MAX, input: 4., cached_input: 0.2, cache_write: 5., output: 20. }],
		"claude-fable-5-1" => &[Card { up_to: u32::MAX, input: 10., cached_input: 0.25, cache_write: 12.5, output: 50. }],
		_ => return None,
	};
	Some(cards)
}

pub(crate) async fn call(source: &Source, config: &AppConfig, request: &Request<'_>) -> Result<Answer, Failure> {
	if let Some(token) = source.token.as_ref().or(config.claude_token.as_ref()) {
		return http::call(token, source, request).await;
	}
	let missing = |e: eyre::Report| {
		Unrecoverable::new_missing_token(format!(
			"hand a token over with `Client::claude_token(…)` or `claude_token` in ~/.config/ask_llm.nix, or log in an account under ~/.claude/accounts ({e:#})"
		))
	};
	let account = accounts::Accounts::home().current().map_err(missing)?;
	if source.origin.is_none() && cli::says(request) {
		return cli::call(&account, source, request).await;
	}
	match accounts::access_token(&account).map_err(missing)? {
		Some(token) => http::call(&token, source, request).await,
		None => Err(Unrecoverable::new_unsupported("more than plain text on an expired device login", "run `claude` once: it alone may refresh the login".to_string()).into()),
	}
}

/// A Messages API content block.
fn block(part: ContentPart) -> Value {
	let source = |media_type: String, data: String| json!({ "type": "base64", "media_type": media_type, "data": data });
	match part {
		ContentPart::Text(text) => json!({ "type": "text", "text": text }),
		ContentPart::Image { base64_data, media_type } => json!({ "type": "image", "source": source(media_type, base64_data) }),
		ContentPart::Document { base64_data, media_type } => json!({ "type": "document", "source": source(media_type, base64_data) }),
	}
}

/// The cache tiers are additional to `input_tokens`, not a breakdown of it.
#[derive(Debug, Deserialize)]
struct Tokens {
	input_tokens: u32,
	cache_read_input_tokens: u32,
	cache_creation_input_tokens: u32,
	output_tokens: u32,
}
impl From<Tokens> for Usage {
	fn from(tokens: Tokens) -> Self {
		Self {
			input: tokens.input_tokens,
			cached_input: tokens.cache_read_input_tokens,
			cache_write: tokens.cache_creation_input_tokens,
			output: tokens.output_tokens,
		}
	}
}
