use std::str::FromStr as _;

use eyre::{Result, bail};
use futures::stream::StreamExt;
use reqwest::{
	Client,
	header::{CONTENT_TYPE, HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Conversation, Model, Response, Role};

#[allow(dead_code)]
#[derive(Debug, Eq, PartialEq)]
/// ref: https://docs.claude.com/en/docs/about-claude/models/all-models
enum ClaudeModel {
	Haiku45,
	Sonnet45,
	Opus41,
}
impl ClaudeModel {
	fn to_str(&self) -> &str {
		match self {
			ClaudeModel::Haiku45 => "claude-haiku-4-5",
			ClaudeModel::Sonnet45 => "claude-sonnet-4-5",
			ClaudeModel::Opus41 => "claude-opus-4-1",
		}
	}

	///NB: could end up being outdated, as I freely use "-latest" marker in model defs
	pub fn cost(&self) -> Cost {
		match self {
			Self::Haiku45 => Cost {
				million_input_tokens: 1.0,
				million_output_tokens: 5.0,
			},
			Self::Sonnet45 => Cost {
				million_input_tokens: 3.0,
				million_output_tokens: 15.0,
			},
			Self::Opus41 => Cost {
				million_input_tokens: 15.0,
				million_output_tokens: 75.0,
			},
		}
	}

	pub fn max_tokens(&self) -> usize {
		match self {
			Self::Haiku45 => 64_000,
			Self::Sonnet45 => 64_000,
			Self::Opus41 => 32_000,
		}
	}
}
impl std::str::FromStr for ClaudeModel {
	type Err = eyre::Report;

	fn from_str(s: &str) -> Result<Self> {
		Ok(match s {
			_ if s.to_lowercase().contains("haiku") => Self::Haiku45,
			_ if s.to_lowercase().contains("sonnet") => Self::Sonnet45,
			_ if s.to_lowercase().contains("opus") => Self::Opus41,
			_ => bail!("Unknown model: {s}"),
		})
	}
}

impl From<Model> for ClaudeModel {
	fn from(model: Model) -> Self {
		match model {
			Model::Fast => Self::Haiku45,
			Model::Medium => Self::Sonnet45,
			Model::Slow => Self::Opus41,
		}
	}
}
pub struct Cost {
	pub million_input_tokens: f32,
	pub million_output_tokens: f32,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ClaudeMessageContent {
	Text(String),
	ContentBlocks(Vec<ClaudeContentBlock>),
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum ClaudeContentBlock {
	#[serde(rename = "text")]
	Text { text: String },
	#[serde(rename = "image")]
	Image { source: ImageSource },
}

#[derive(Debug, Serialize)]
struct ImageSource {
	#[serde(rename = "type")]
	source_type: String,
	media_type: String,
	data: String,
}

#[derive(Debug, Serialize)]
struct ClaudeMessage {
	role: &'static str,
	content: ClaudeMessageContent,
}
#[derive(Debug, Serialize)]
struct ClaudeConversation {
	messages: Vec<ClaudeMessage>,
}
impl From<&Conversation> for ClaudeConversation {
	fn from(conversation: &Conversation) -> Self {
		use crate::MessageContent;
		let mut messages = Vec::new();
		for message in &conversation.0 {
			let role = match message.role {
				Role::System => "system",
				Role::User => "user",
				Role::Assistant => "assistant",
			};

			let content = match &message.content {
				MessageContent::Text(text) => ClaudeMessageContent::Text(text.clone()),
				MessageContent::Image { base64_data, media_type } => ClaudeMessageContent::ContentBlocks(vec![ClaudeContentBlock::Image {
					source: ImageSource {
						source_type: "base64".to_string(),
						media_type: media_type.clone(),
						data: base64_data.clone(),
					},
				}]),
				MessageContent::TextAndImages { text, images } => {
					let mut blocks = vec![ClaudeContentBlock::Text { text: text.clone() }];
					for img in images {
						blocks.push(ClaudeContentBlock::Image {
							source: ImageSource {
								source_type: "base64".to_string(),
								media_type: img.media_type.clone(),
								data: img.base64_data.clone(),
							},
						});
					}
					ClaudeMessageContent::ContentBlocks(blocks)
				}
			};

			messages.push(ClaudeMessage { role, content });
		}
		Self { messages }
	}
}

///docs: https://docs.claude.com/claude/reference/messages_post
pub async fn ask_claude<T: AsRef<str>>(conversation: &Conversation, model: Model, requested_max_tokens: Option<usize>, stop_sequences: Option<Vec<T>>) -> Result<Response> {
	let mut conversation = ClaudeConversation::from(conversation);

	let api_key = std::env::var("CLAUDE_TOKEN").expect("CLAUDE_TOKEN environment variable not set");
	let url = "https://api.anthropic.com/v1/messages";

	// Header {{{
	let mut headers = HeaderMap::new();
	headers.insert("x-api-key", HeaderValue::from_str(&api_key).unwrap());
	headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01")); // API standard edition, does not influence model versions
	headers.insert("anthropic-beta", HeaderValue::from_static("output-128k-2025-02-19")); // allows for 128k tokens output on newer models
	headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
	//,}}}

	let request_builder = Client::new().post(url).headers(headers);

	let system_message = match conversation.messages[0].role == "system" {
		true => {
			let system_message = conversation.messages.remove(0);
			Some(system_message.content)
		}
		false => None,
	};

	let claude_model = ClaudeModel::from(model);
	let max_tokens = match requested_max_tokens {
		Some(max_tokens) => max_tokens.min(claude_model.max_tokens()),
		_ => claude_model.max_tokens(),
	};

	// Payload {{{
	let mut payload = json!({
		"model": claude_model.to_str(),
		"temperature": 0.0,
		"max_tokens": max_tokens,
		"messages": conversation.messages
	});
	if let Some(stop_seqs) = stop_sequences {
		let stop_seqs_str: Vec<String> = stop_seqs.into_iter().map(|s| s.as_ref().to_string()).collect();
		payload.as_object_mut().unwrap().insert("stop_sequences".to_string(), serde_json::json!(stop_seqs_str));
	}
	if let Some(system_message) = system_message {
		payload.as_object_mut().unwrap().insert("system".to_string(), serde_json::json!(system_message));
	}
	//,}}}

	Ok(match requested_max_tokens {
		Some(max_tokens) if max_tokens <= 4096 => {
			payload.as_object_mut().unwrap().insert("stream".to_owned(), serde_json::json!(false));
			tracing::info!("getting through a rest get");
			tracing::debug!(?payload);
			rest_g(request_builder.json(&payload)).await?
		}
		_ => {
			payload.as_object_mut().unwrap().insert("stream".to_owned(), serde_json::json!(true));
			tracing::info!("getting through a stream");
			tracing::debug!(?payload);
			stream(request_builder.json(&payload), claude_model).await?
		}
	})
}
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ClaudeContent {
	#[serde(rename = "type")]
	content_type: String,
	text: String,
}
#[derive(Debug, Deserialize)]
struct ClaudeUsage {
	input_tokens: u32,
	output_tokens: u32,
}

// stream {{{
async fn stream(request_builder: reqwest::RequestBuilder, model: ClaudeModel) -> Result<Response> {
	#[derive(Debug, Deserialize, Serialize)]
	struct Delta {
		text: String,
		#[serde(rename = "type")]
		delta_type: String,
	}
	#[derive(Debug, Deserialize, Serialize)]
	struct DeltaContentBlock {
		delta: Delta,
		index: u32,
		#[serde(rename = "type")]
		response_type: String,
	}

	let mut response_stream = request_builder.send().await?.bytes_stream();

	let mut accumulated_message = String::new();

	fn parse_sse(bytes: bytes::Bytes) -> String {
		let s = String::from_utf8(bytes.to_vec()).expect("Found invalid UTF-8");
		let mut parsed_string = String::new();

		let split = s
			.split("event: content_block_delta\ndata: ")
			.map(|s| s.split("\n\nevent: ").collect::<Vec<&str>>().get(0).unwrap().to_string())
			.collect::<Vec<String>>();

		for s in split {
			if let Ok(v) = serde_json::from_str::<DeltaContentBlock>(&s) {
				if v.response_type == "content_block_delta" || v.delta.delta_type == "text_delta" {
					parsed_string.push_str(&v.delta.text);
				}
			}
		}
		parsed_string
	}

	while let Some(events_batch) = response_stream.next().await {
		let events_batch = events_batch?;

		let parsed = parse_sse(events_batch);
		tracing::debug!(parsed);
		accumulated_message.push_str(&parsed);
	}

	let estimated_tokens = accumulated_message.split_whitespace().count() as f32 * 0.7;
	let cost = (model.cost().million_output_tokens * estimated_tokens) / 1_000_000.0;
	Ok(Response::new(accumulated_message, cost))
}
//,}}}

// rest_g {{{
async fn rest_g(request_builder: reqwest::RequestBuilder) -> Result<Response> {
	let value = request_builder.send().await?.json::<Value>().await?;
	tracing::debug!(?value);
	let response = serde_json::from_value::<ClaudeResponse>(value.clone()).map_err(|e| {
		eprintln!(
			"Failed to parse Claude response. Response JSON: {}",
			serde_json::to_string_pretty(&value).unwrap_or_else(|_| format!("{:?}", value))
		);
		e
	})?;

	// Check for refusal
	if response.stop_reason == "refusal" {
		bail!("Claude refused to process the request. This may be due to content policy restrictions.");
	}

	//let response = request_builder.send().await?.json::<ClaudeResponse>().await?;
	return Ok(response.into());

	#[allow(dead_code)]
	#[derive(Debug, Deserialize)]
	pub struct ClaudeResponse {
		id: String,
		#[serde(rename = "type")]
		response_type: String,
		role: String,
		content: Vec<ClaudeContent>,
		model: String,
		stop_reason: String,
		stop_sequence: Option<String>,
		usage: ClaudeUsage,
	}
	impl ClaudeResponse {
		pub fn text(&self) -> String {
			let contents = self.content.iter().filter(|c| c.content_type == "text").map(|c| c.text.to_owned()).collect::<Vec<String>>();
			contents.join("\n")
		}

		pub fn cost_cents(&self) -> f32 {
			let model = ClaudeModel::from_str(&self.model).unwrap();
			let cost = model.cost();
			(self.usage.input_tokens as f32 * cost.million_input_tokens + self.usage.output_tokens as f32 * cost.million_output_tokens) / 10_000.0
		}
	}
	impl From<ClaudeResponse> for Response {
		fn from(response: ClaudeResponse) -> Self {
			Self {
				text: response.text(),
				cost_cents: response.cost_cents(),
			}
		}
	}
}
//,}}}

#[cfg(test)]
mod tests {
	#[test]
	fn deser_model() {
		let model = "claude-haiku-4-5-20251001".parse::<super::ClaudeModel>().unwrap();
		assert_eq!(model, super::ClaudeModel::Haiku45);
	}
}
