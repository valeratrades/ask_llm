//! Every http call: a [`Request`] through genai, its answer and its failures in our terms.
use std::time::Duration;

use genai::{
	ModelIden, ServiceTarget,
	adapter::AdapterKind,
	chat::{ChatMessage, ChatOptions, ChatRequest, ChatResponse, ChatResponseFormat, JsonSpec, MessageContent as Said, StopReason},
	resolver::{AuthData, Endpoint},
	webc,
};
use serde_json::Value;

use crate::{ContentPart, Json, MAX_TOKENS, Provider, Request, Response, Role, Source, Stop, ToolCall, Transport, Unrecoverable, Usage, error::Failure};

pub(crate) struct Answer {
	pub response: Response,
	/// Why it stopped, in the provider's word.
	pub reason: String,
	/// `None` off the wire: the CLI's answer is never replayed.
	pub turn: Option<Turn>,
}

/// What a model said, as it is sent back to it.
#[derive(Clone, Debug)]
pub(crate) struct Turn {
	pub said: Said,
	/// The whole response body.
	pub raw: Value,
}

/// A turn that called tools, and what they answered, in the calls' order.
#[derive(Debug)]
pub(crate) struct Round {
	pub turn: Turn,
	pub calls: Vec<ToolCall>,
	pub results: Vec<String>,
}

/// The options every provider reads alike.
pub(crate) fn options(request: &Request<'_>) -> ChatOptions {
	let max_tokens = request.max_tokens.map_or(MAX_TOKENS, |n| n.min(MAX_TOKENS));
	let options = ChatOptions::default()
		.with_capture_raw_body(true)
		.with_max_tokens(u32::try_from(max_tokens).expect("capped above"));
	match request.json {
		Json::Free => options,
		Json::Object => options.with_response_format(ChatResponseFormat::JsonMode),
		Json::Schema(schema) => options.with_response_format(JsonSpec::new("answer", schema.clone())),
	}
}

/// The tools alone; the messages are each provider's.
pub(crate) fn tools(request: &Request<'_>) -> ChatRequest {
	let tools: Vec<genai::chat::Tool> = request
		.tools
		.iter()
		.map(|t| genai::chat::Tool::new(t.name.clone()).with_description(t.description.clone()).with_schema(t.input_schema.clone()))
		.collect();
	match tools.is_empty() {
		true => ChatRequest::default(),
		false => ChatRequest::default().with_tools(tools),
	}
}

/// The conversation as genai's messages, the rounds not among them.
pub(crate) fn messages(request: &Request<'_>) -> Result<Vec<ChatMessage>, Unrecoverable> {
	Ok(request
		.turns()?
		.into_iter()
		.map(|(role, parts)| {
			let said = Said::from_parts(parts.into_iter().map(part).collect::<Vec<_>>());
			match role {
				Role::System => ChatMessage::system(said),
				Role::User => ChatMessage::user(said),
				Role::Assistant => ChatMessage::assistant(said),
			}
		})
		.collect())
}

fn part(part: ContentPart) -> genai::chat::ContentPart {
	match part {
		ContentPart::Text(text) => genai::chat::ContentPart::from_text(text),
		ContentPart::Image { base64_data, media_type } => genai::chat::ContentPart::from_binary_base64(media_type, base64_data, None),
		ContentPart::Document { base64_data, media_type } => {
			let name = format!("attachment.{}", media_type.rsplit('/').next().expect("rsplit always yields one element"));
			genai::chat::ContentPart::from_binary_base64(media_type, base64_data, Some(name))
		}
	}
}

pub(crate) async fn exec(source: &Source, kind: AdapterKind, endpoint: String, auth: AuthData, request: ChatRequest, options: ChatOptions) -> Result<ChatResponse, Failure> {
	let _ = rustls::crypto::ring::default_provider().install_default(); // Err: a provider is already installed
	let mut http = reqwest::Client::builder();
	if source.provider != Provider::Ollama {
		http = http.timeout(Duration::from_mins(10)); // what both vendors' own SDKs give a call
	}
	let http = http.build().expect("the TLS backend initialises");
	let target = ServiceTarget {
		endpoint: Endpoint::from_owned(endpoint),
		auth,
		model: ModelIden::from_static(kind, source.model),
	};
	tracing::debug!(provider = %source.provider, model = source.model, messages = request.messages.len(), "calling over http");
	let response = genai::Client::builder()
		.with_reqwest(http)
		.build()
		.expect("nothing is left to build: the http client is given")
		.exec_chat(target, request, Some(&options))
		.await
		.map_err(|e| failure(source.provider, e))?;
	tracing::debug!(provider = %source.provider, raw = ?response.captured_raw_body);
	Ok(response)
}

/// The one place a genai error becomes a [`Failure`].
fn failure(provider: Provider, error: genai::Error) -> Failure {
	let schema = |cause: String, body: String| Unrecoverable::new_schema(<serde_json::Error as serde::de::Error>::custom(cause), body).into();
	match error {
		genai::Error::WebModelCall { webc_error, .. } => match webc_error {
			webc::Error::ResponseFailedStatus { status, body, headers } => Failure::of_response(provider, status.as_u16(), &headers, &body),
			webc::Error::Reqwest(e) => Transport::classify(provider, e).into(),
			webc::Error::ResponseFailedInvalidJson { body, cause } => schema(cause, body),
			webc::Error::ResponseFailedNotJson { content_type, body } => schema(format!("`{content_type}` where json was asked for"), body),
			e @ webc::Error::JsonValueExt(_) => schema(e.to_string(), String::new()),
		},
		genai::Error::ChatResponseGeneration { response_body, cause, .. } => schema("genai could not read it".to_string(), format!("{cause}\n{response_body}")), // the cause may quote it
		e => Unrecoverable::new_unsupported("this request", e.to_string()).into(),
	}
}

/// What genai read, in our terms. `usage` is each provider's, read off the same body.
pub(crate) fn answer(source: &Source, request: &Request<'_>, chat: ChatResponse, usage: Usage, overhead: Duration) -> Result<Answer, Failure> {
	let raw = chat.captured_raw_body.expect("asked for in `options`");
	let text: String = chat.content.texts().concat();
	let calls: Vec<ToolCall> = chat
		.content
		.tool_calls()
		.into_iter()
		.map(|c| ToolCall {
			id: c.call_id.clone(),
			name: c.fn_name.clone(),
			input: c.fn_arguments.clone(),
		})
		.collect();
	let reason = chat.stop_reason.as_ref().map_or_else(|| "none".to_string(), |r| r.raw().to_string());
	let stop = match chat.stop_reason {
		Some(StopReason::ContentFilter(_)) => Stop::Refused,
		Some(StopReason::MaxTokens(_)) => Stop::MaxTokens, // calls it cut short are not whole
		Some(StopReason::Completed(_) | StopReason::StopSequence(_) | StopReason::ToolCall(_)) if !calls.is_empty() => Stop::Tools(calls),
		Some(StopReason::Completed(_)) if refuses(&raw) => Stop::Refused,
		Some(StopReason::Completed(_) | StopReason::StopSequence(_)) => Stop::End,
		Some(StopReason::ToolCall(_) | StopReason::Other(_)) | None => return Err(Unrecoverable::new_unusable(format!("stopped for `{reason}`"), raw.to_string(), usage).into()),
	};
	if text.trim().is_empty() && request.tools.is_empty() && !matches!(stop, Stop::Refused) {
		return Err(Unrecoverable::new_empty(reason, usage).into());
	}
	Ok(Answer {
		response: Response::new(
			text,
			usage.cents(source.provider, source.model),
			overhead,
			&chat.provider_model_iden.model_name,
			request.thinking,
			usage,
			stop,
		),
		reason,
		turn: Some(Turn { said: chat.content, raw }),
	})
}

/// OpenAI's refusal is a part of a message that completed, which genai drops.
fn refuses(raw: &Value) -> bool {
	let parts = raw["output"].as_array().into_iter().flatten().flat_map(|item| item["content"].as_array().into_iter().flatten());
	parts.into_iter().any(|part| part["type"] == "refusal")
}

/// Counts that include the cache's tiers, resolved into ones that do not.
pub(crate) fn usage(chat: &ChatResponse) -> Result<Usage, Unrecoverable> {
	let count = |n: i32| u32::try_from(n).expect("tokens are counted up from zero");
	let (Some(prompt), Some(output)) = (chat.usage.prompt_tokens, chat.usage.completion_tokens) else {
		let body = chat.captured_raw_body.as_ref().expect("asked for in `options`").to_string();
		return Err(Unrecoverable::new_schema(serde::de::Error::missing_field("usage"), body));
	};
	let details = chat.usage.prompt_tokens_details.as_ref();
	let cached_input = details.and_then(|d| d.cached_tokens).map_or(0, count); // genai reads a zero as none
	let cache_write = details.and_then(|d| d.cache_creation_tokens).map_or(0, count);
	Ok(Usage {
		input: count(prompt)
			.checked_sub(cached_input + cache_write)
			.expect("cached and written tokens are a breakdown of the prompt's, not an addition to them"),
		cached_input,
		cache_write,
		output: count(output),
	})
}
