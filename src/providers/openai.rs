use std::time::Duration;

use genai::{
	adapter::AdapterKind,
	chat::{CacheControl, ChatMessage, MessageContent, ReasoningEffort, ToolResponse},
	resolver::AuthData,
};
use serde_json::json;

use crate::{
	Failure, Request, Source, ThinkingLevel, Unrecoverable,
	config::AppConfig,
	usage::Card,
	wire::{self, Answer},
};

#[derive(Debug, Eq, PartialEq)]
/// ref: https://platform.openai.com/docs/models
pub enum OpenAiModel {
	Luna,
}
impl OpenAiModel {
	pub const fn to_str(&self) -> &'static str {
		match self {
			Self::Luna => "gpt-5.6-luna",
		}
	}
}

/// ref: https://developers.openai.com/api/docs/pricing
pub(crate) fn prices(model: &str) -> Option<&'static [Card]> {
	const SHORT: u32 = 272_000;
	#[rustfmt::skip]
	let cards: &'static [Card] = match model {
		"gpt-5.6-luna" => &[
			Card { up_to: SHORT, input: 0.2, cached_input: 0.02, cache_write: 0.25, output: 1.2 },
			Card { up_to: u32::MAX, input: 0.4, cached_input: 0.04, cache_write: 0.5, output: 1.8 },
		],
		"gpt-5.6-terra" => &[
			Card { up_to: SHORT, input: 2., cached_input: 0.2, cache_write: 2.5, output: 12. },
			Card { up_to: u32::MAX, input: 4., cached_input: 0.4, cache_write: 5., output: 18. },
		],
		"gpt-5.6-sol" => &[
			Card { up_to: SHORT, input: 4., cached_input: 0.4, cache_write: 5., output: 20. },
			Card { up_to: u32::MAX, input: 8., cached_input: 0.8, cache_write: 10., output: 30. },
		],
		_ => return None,
	};
	Some(cards)
}

/// docs: https://platform.openai.com/docs/api-reference/responses/create
pub(crate) async fn call(source: &Source, config: &AppConfig, request: &Request<'_>) -> Result<Answer, Failure> {
	let key = source
		.token
		.clone()
		.or_else(|| config.openai_token.clone())
		.or_else(|| std::env::var("OPENAI_API_KEY").ok())
		.ok_or_else(|| Unrecoverable::new_missing_token("hand it over with `Client::openai_token(…)`, put `openai_token` in ~/.config/ask_llm.nix, or export OPENAI_API_KEY".to_string()))?;

	let mut chat = wire::tools(request);
	chat.messages = wire::messages(request)?;
	for round in request.rounds {
		chat.messages.push(ChatMessage::assistant(round.turn.said.clone()));
		let results: Vec<ToolResponse> = round.calls.iter().zip(&round.results).map(|(call, result)| ToolResponse::new(&call.id, result)).collect();
		chat.messages.push(ChatMessage::tool(MessageContent::from_tool_responses(results)));
	}

	let mut options = wire::options(request).with_reasoning_effort(match request.thinking {
		ThinkingLevel::None => ReasoningEffort::Zero,
		ThinkingLevel::Low => ReasoningEffort::Low,
		ThinkingLevel::Medium => ReasoningEffort::Medium,
		ThinkingLevel::High => ReasoningEffort::High,
	});
	options.max_tokens = options.max_tokens.map(|n| n.max(16)); // the least it takes
	if !request.tools.is_empty() {
		options = options
			.with_cache_control(CacheControl::Ephemeral) // genai turns caching off where nothing asks for it
			.with_extra_body(json!({ "include": ["reasoning.encrypted_content"] })); // `capture_reasoning_content` would ask for summaries too
	}

	let origin = source.origin.as_deref().unwrap_or("https://api.openai.com");
	let response = wire::exec(source, AdapterKind::OpenAIResp, format!("{origin}/v1/"), AuthData::from_single(key), chat, options).await?;
	let usage = wire::usage(&response)?;
	let mut answer = wire::answer(source, request, response, usage, Duration::ZERO)?;

	// `stop` is rejected outright, so the sequences are cut out of the returned text instead. Same output, but the tokens past the cut are still billed.
	if let Some(sequences) = &request.stop_sequences
		&& let Some(cut) = sequences.iter().filter_map(|s| answer.response.text.find(s)).min()
	{
		answer.response.text.truncate(cut);
	}
	Ok(answer)
}
