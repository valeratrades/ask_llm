//! The Messages API on a subscription's OAuth token.
use std::time::Duration;

use genai::{Headers, adapter::AdapterKind, chat::ReasoningEffort, resolver::AuthData};
use serde_json::{Value, json};

use super::{Tokens, block};
use crate::{
	ContentPart, Failure, Json, Request, Role, Source, Stop, ThinkingLevel, Unrecoverable,
	wire::{self, Answer},
};

/// A subscription's token is served only to a system prompt that opens so.
const SUBSCRIPTION: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

pub(super) async fn call(token: &str, source: &Source, request: &Request<'_>) -> Result<Answer, Failure> {
	let (system, messages) = transcript(request)?;
	let mut body = json!({ "system": system, "messages": messages });
	if request.agentic {
		body["cache_control"] = json!({ "type": "ephemeral" }); // the breakpoint follows the rounds
	}
	let mut options = wire::options(request)
		.with_reasoning_effort(match request.thinking {
			ThinkingLevel::None | ThinkingLevel::Low => ReasoningEffort::Low,
			ThinkingLevel::Medium => ReasoningEffort::Medium,
			ThinkingLevel::High => ReasoningEffort::High,
		})
		.with_extra_body(body);
	if let Some(sequences) = &request.stop_sequences {
		options = options.with_stop_sequences(sequences.iter().map(|s| s.to_string()).collect());
	}

	let origin = source.origin.as_deref().unwrap_or("https://api.anthropic.com");
	// genai's own auth is `x-api-key`, which a subscription's token is not
	let auth = AuthData::RequestOverride {
		url: format!("{origin}/v1/messages"),
		headers: Headers::from([
			("authorization", format!("Bearer {token}")),
			("anthropic-version", "2023-06-01".to_string()),
			("anthropic-beta", "oauth-2025-04-20".to_string()),
		]),
	};
	let response = wire::exec(source, AdapterKind::Anthropic, origin.to_string(), auth, wire::tools(request), options).await?;
	let raw = response.captured_raw_body.as_ref().expect("asked for in `options`");
	let usage: Tokens = serde_json::from_value(raw["usage"].clone()).map_err(|e| Unrecoverable::new_schema(e, raw.to_string()))?;
	let answer = wire::answer(source, request, response, usage.into(), Duration::ZERO)?;
	// there is no mode for any json, so nothing held it to the suffix
	if let (Json::Object, Stop::End) = (request.json, &answer.response.stop)
		&& let Err(e) = serde_json::from_str::<Value>(&answer.response.text)
	{
		return Err(Unrecoverable::new_schema(e, answer.response.text).into());
	}
	Ok(answer)
}

/// The `system` and `messages` of the body. A turn that called tools goes back as it came: thinking is valid only in the
/// exact conversation it was made in, and genai moves it to the front of its turn.
fn transcript(request: &Request<'_>) -> Result<(Vec<Value>, Vec<Value>), Unrecoverable> {
	let mut system = vec![block(ContentPart::Text(SUBSCRIPTION.to_string()))];
	let mut messages = Vec::new();
	for (role, parts) in request.turns()? {
		let content: Vec<Value> = parts.into_iter().map(block).collect();
		match role {
			Role::System if messages.is_empty() => system.extend(content),
			Role::System => {
				return Err(Unrecoverable::new_unsupported(
					"a system message past the first turn",
					"only leading ones are the system prompt".to_string(),
				));
			}
			role => messages.push(json!({ "role": <&str>::from(role), "content": content })),
		}
	}
	for round in request.rounds {
		messages.push(json!({ "role": "assistant", "content": round.turn.raw["content"] }));
		let results: Vec<Value> = round
			.calls
			.iter()
			.zip(&round.results)
			.map(|(call, result)| json!({ "type": "tool_result", "tool_use_id": call.id, "content": result }))
			.collect();
		messages.push(json!({ "role": "user", "content": results }));
	}
	Ok((system, messages))
}
