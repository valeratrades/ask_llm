use std::str::FromStr as _;

use eyre::{Result, bail};
use futures::stream::StreamExt;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{Backend, Conversation, Cost, FORCE_JSON_SUFFIX, FileAttachment, MAX_TOKENS, Request, Response, ThinkingLevel};

/// Thinking is billed against `max_tokens` and can consume all of it, leaving zero text behind.
/// Silently handing back an empty string reads as "the model had nothing to say", so error instead.
fn starved(stop_reason: Option<&str>, text: &str) -> Result<()> {
	if stop_reason == Some("max_tokens") && text.trim().is_empty() {
		bail!("thinking consumed the entire max_tokens budget, leaving no answer; raise max_tokens");
	}
	Ok(())
}
pub(crate) struct Claude {
	pub api_key: String,
	pub model: ClaudeModel,
}
impl Claude {
	///docs: https://docs.claude.com/claude/reference/messages_post
	async fn do_conversation(&self, request: &Request<'_>) -> Result<Response> {
		let mut conversation = ClaudeConversation::from(request.conversation);

		// Prepend files to the first user message
		if !request.files.is_empty() {
			if let Some(first_user_msg) = conversation.messages.iter_mut().find(|m| m.role == "user") {
				let mut file_blocks: Vec<ClaudeContentBlock> = request.files.iter().map(|f| file_to_content_block(f)).collect();

				// Convert existing content to blocks and prepend file blocks
				match &first_user_msg.content {
					ClaudeMessageContent::Text(text) => {
						file_blocks.push(ClaudeContentBlock::Text { text: text.clone() });
						first_user_msg.content = ClaudeMessageContent::ContentBlocks(file_blocks);
					}
					ClaudeMessageContent::ContentBlocks(existing_blocks) => {
						file_blocks.extend(existing_blocks.clone());
						first_user_msg.content = ClaudeMessageContent::ContentBlocks(file_blocks);
					}
				}
			}
		}

		let url = "https://api.anthropic.com/v1/messages";

		// Header {{{
		let mut headers = HeaderMap::new();
		headers.insert("x-api-key", HeaderValue::from_str(&self.api_key).unwrap());
		headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01")); // API standard edition, does not influence model versions
		headers.insert("anthropic-beta", HeaderValue::from_static("structured-outputs-2025-11-13"));
		headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
		//,}}}

		let request_builder = reqwest::Client::new().post(url).headers(headers);

		let system_message = match conversation.messages[0].role == "system" {
			true => {
				let system_message = conversation.messages.remove(0);
				Some(system_message.content)
			}
			false => None,
		};

		let max_tokens = request.max_tokens.unwrap_or(MAX_TOKENS).min(MAX_TOKENS);

		// Payload {{{
		if request.force_json {
			let last = conversation.messages.last_mut().expect("conversation is never empty");
			match &mut last.content {
				ClaudeMessageContent::Text(text) => text.push_str(FORCE_JSON_SUFFIX),
				ClaudeMessageContent::ContentBlocks(blocks) => blocks.push(ClaudeContentBlock::Text {
					text: FORCE_JSON_SUFFIX.to_string(),
				}),
			}
		}

		// `disabled` is rejected outright by fable, and above `high` effort by opus, so `None` omits the key instead.
		let effort = match request.thinking {
			ThinkingLevel::None | ThinkingLevel::Low => "low",
			ThinkingLevel::Medium => "medium",
			ThinkingLevel::High => "high",
		};
		let mut payload = json!({
			"model": self.model.to_str(),
			"max_tokens": max_tokens,
			"output_config": {"effort": effort},
			"messages": conversation.messages
		});
		if !matches!(request.thinking, ThinkingLevel::None) {
			payload.as_object_mut().unwrap().insert("thinking".to_string(), json!({"type": "adaptive"}));
		}
		if let Some(ref stop_seqs) = request.stop_sequences {
			payload.as_object_mut().unwrap().insert("stop_sequences".to_string(), serde_json::json!(stop_seqs));
		}
		if let Some(system_message) = system_message {
			payload.as_object_mut().unwrap().insert("system".to_string(), serde_json::json!(system_message));
		}
		//,}}}

		let mut response = match request.max_tokens {
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
				stream(request_builder.json(&payload), &self.model).await?
			}
		};

		response.model = self.model.to_str().to_string();
		response.thinking = request.thinking;
		Ok(response)
	}
}

impl Backend for Claude {
	fn conversation<'a>(&'a self, request: &'a Request<'a>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response>> + Send + 'a>> {
		Box::pin(self.do_conversation(request))
	}
}

#[derive(Debug, Eq, PartialEq)]
/// ref: https://docs.claude.com/en/docs/about-claude/models/all-models
pub(crate) enum ClaudeModel {
	Sonnet5,
	Opus5,
	Fable5,
}
impl ClaudeModel {
	fn to_str(&self) -> &str {
		match self {
			ClaudeModel::Sonnet5 => "claude-sonnet-5",
			ClaudeModel::Opus5 => "claude-opus-5",
			ClaudeModel::Fable5 => "claude-fable-5",
		}
	}

	///NB: could end up being outdated, as I freely use "-latest" marker in model defs
	/// Cache reads are 0.1x base input and 5-minute writes 1.25x, uniformly across the lineup.
	/// The 1-hour TTL bills writes at 2x instead, and is unreachable while no request sets `cache_control`.
	pub fn cost(&self) -> Cost {
		match self {
			Self::Sonnet5 => Cost {
				million_input_tokens: 3.0,
				million_cached_input_tokens: 0.3,
				million_cache_write_tokens: 3.75,
				million_output_tokens: 15.0,
			},
			Self::Opus5 => Cost {
				million_input_tokens: 5.0,
				million_cached_input_tokens: 0.5,
				million_cache_write_tokens: 6.25,
				million_output_tokens: 25.0,
			},
			Self::Fable5 => Cost {
				million_input_tokens: 10.0,
				million_cached_input_tokens: 1.0,
				million_cache_write_tokens: 12.5,
				million_output_tokens: 50.0,
			},
		}
	}
}
impl std::str::FromStr for ClaudeModel {
	type Err = eyre::Report;

	fn from_str(s: &str) -> Result<Self> {
		Ok(match s {
			_ if s.to_lowercase().contains("sonnet") => Self::Sonnet5,
			_ if s.to_lowercase().contains("opus") => Self::Opus5,
			_ if s.to_lowercase().contains("fable") => Self::Fable5,
			_ => bail!("Unknown model: {s}"),
		})
	}
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ClaudeMessageContent {
	Text(String),
	ContentBlocks(Vec<ClaudeContentBlock>),
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type")]
enum ClaudeContentBlock {
	#[serde(rename = "text")]
	Text { text: String },
	#[serde(rename = "image")]
	Image { source: ImageSource },
	#[serde(rename = "document")]
	Document { source: DocumentSource },
}

#[derive(Clone, Debug, Serialize)]
struct ImageSource {
	#[serde(rename = "type")]
	source_type: String,
	media_type: String,
	data: String,
}

#[derive(Clone, Debug, Serialize)]
struct DocumentSource {
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
		use crate::{ContentPart, MessageContent};
		let mut messages = Vec::new();
		for message in &conversation.0 {
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
				MessageContent::Document { base64_data, media_type } => ClaudeMessageContent::ContentBlocks(vec![ClaudeContentBlock::Document {
					source: DocumentSource {
						source_type: "base64".to_string(),
						media_type: media_type.clone(),
						data: base64_data.clone(),
					},
				}]),
				MessageContent::Mixed { parts } => {
					let blocks = parts
						.iter()
						.map(|part| match part {
							ContentPart::Text(text) => ClaudeContentBlock::Text { text: text.clone() },
							ContentPart::Image { base64_data, media_type } => ClaudeContentBlock::Image {
								source: ImageSource {
									source_type: "base64".to_string(),
									media_type: media_type.clone(),
									data: base64_data.clone(),
								},
							},
							ContentPart::Document { base64_data, media_type } => ClaudeContentBlock::Document {
								source: DocumentSource {
									source_type: "base64".to_string(),
									media_type: media_type.clone(),
									data: base64_data.clone(),
								},
							},
						})
						.collect();
					ClaudeMessageContent::ContentBlocks(blocks)
				}
			};

			messages.push(ClaudeMessage { role: message.role.into(), content });
		}
		Self { messages }
	}
}

/// Convert a file attachment to the appropriate content block.
/// PDFs use the document block, text-based files are decoded and inserted as text.
fn file_to_content_block(file: &FileAttachment) -> ClaudeContentBlock {
	use base64::Engine;
	match file.media_type.as_str() {
		"application/pdf" => ClaudeContentBlock::Document {
			source: DocumentSource {
				source_type: "base64".to_string(),
				media_type: file.media_type.clone(),
				data: file.base64_data.clone(),
			},
		},
		// Images use image blocks
		mt if mt.starts_with("image/") => ClaudeContentBlock::Image {
			source: ImageSource {
				source_type: "base64".to_string(),
				media_type: file.media_type.clone(),
				data: file.base64_data.clone(),
			},
		},
		// Text-based files are decoded and included as text
		_ => {
			let decoded = base64::engine::general_purpose::STANDARD
				.decode(&file.base64_data)
				.ok()
				.and_then(|bytes| String::from_utf8(bytes).ok())
				.unwrap_or_else(|| format!("[Binary file: {}]", file.media_type));
			ClaudeContentBlock::Text { text: decoded }
		}
	}
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ClaudeContent {
	#[serde(rename = "type")]
	content_type: String,
	/// absent on non-text blocks (`thinking` carries `thinking`+`signature` instead)
	#[serde(default)]
	text: String,
}
#[derive(Debug, Deserialize)]
/// `input_tokens` counts only what follows the last cache breakpoint, so the cache tiers add to it rather than divide it.
struct ClaudeUsage {
	input_tokens: u32,
	output_tokens: u32,
	#[serde(default)]
	cache_read_input_tokens: u32,
	#[serde(default)]
	cache_creation_input_tokens: u32,
	#[serde(default)]
	cache_creation: ClaudeCacheCreation,
}

#[derive(Debug, Default, Deserialize)]
struct ClaudeCacheCreation {
	#[serde(default)]
	ephemeral_1h_input_tokens: u32,
}
impl From<&ClaudeUsage> for crate::Usage {
	fn from(usage: &ClaudeUsage) -> Self {
		// reachable only once something sets `cache_control`, at which point the 2x rate has to be priced rather than folded into the 1.25x one
		assert_eq!(
			usage.cache_creation.ephemeral_1h_input_tokens, 0,
			"a 1-hour cache write bills at 2x base input, which `Cost` cannot express"
		);
		Self {
			input: usage.input_tokens,
			cached_input: usage.cache_read_input_tokens,
			cache_write: usage.cache_creation_input_tokens,
			output: usage.output_tokens,
		}
	}
}

// stream {{{
async fn stream(request_builder: reqwest::RequestBuilder, model: &ClaudeModel) -> Result<Response> {
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

	let ttfb_start = std::time::Instant::now();
	let http_response = request_builder.send().await?;
	let status = http_response.status();
	if !status.is_success() {
		bail!("Claude request failed ({status}): {}", http_response.text().await?);
	}
	let mut response_stream = http_response.bytes_stream();
	let ttfb = ttfb_start.elapsed();

	let mut accumulated_message = String::new();
	let mut refused = false;

	fn parse_sse(s: &str) -> String {
		let mut parsed_string = String::new();

		let split = s
			.split("event: content_block_delta\ndata: ")
			.map(|s| s.split("\n\nevent: ").collect::<Vec<&str>>().get(0).unwrap().to_string())
			.collect::<Vec<String>>();

		for s in split {
			if let Ok(v) = serde_json::from_str::<DeltaContentBlock>(&s)
				&& v.response_type == "content_block_delta"
				&& v.delta.delta_type == "text_delta"
			{
				parsed_string.push_str(&v.delta.text);
			}
		}
		parsed_string
	}

	/// only field we act on; `message_delta` carries the terminal stop_reason
	#[derive(Deserialize)]
	struct MessageDelta {
		stop_reason: Option<String>,
	}

	let mut stop_reason = None;
	while let Some(events_batch) = response_stream.next().await {
		let events_batch = events_batch?;
		let s = String::from_utf8(events_batch.to_vec()).expect("Found invalid UTF-8");

		for chunk in s.split("event: message_delta\ndata: ").skip(1) {
			let data = chunk.split("\n\n").next().expect("split always yields one element");
			if let Ok(d) = serde_json::from_str::<serde_json::Value>(data)
				&& let Ok(delta) = serde_json::from_value::<MessageDelta>(d.get("delta").cloned().unwrap_or_default())
				&& let Some(r) = delta.stop_reason
			{
				refused |= r == "refusal";
				stop_reason = Some(r);
			}
		}

		let parsed = parse_sse(&s);
		tracing::debug!(parsed);
		accumulated_message.push_str(&parsed);
	}

	if refused {
		bail!("Claude refused to process the request. This may be due to content policy restrictions.");
	}
	starved(stop_reason.as_deref(), &accumulated_message)?;

	let estimated_tokens = accumulated_message.split_whitespace().count() as f32 * 0.7;
	Ok(Response {
		cost_cents: model.cost().cents(crate::Usage {
			output: estimated_tokens as u32,
			..Default::default()
		}),
		text: accumulated_message,
		duration: std::time::Duration::ZERO,
		overhead: ttfb,
		model: String::new(),
		thinking: ThinkingLevel::None,
	})
}
//,}}}

// rest_g {{{
async fn rest_g(request_builder: reqwest::RequestBuilder) -> Result<Response> {
	let ttfb_start = std::time::Instant::now();
	let response: ClaudeResponse = crate::json_response(request_builder.send().await?, "Claude").await?;
	let ttfb = ttfb_start.elapsed();

	// Check for refusal
	if response.stop_reason == "refusal" {
		bail!("Claude refused to process the request. This may be due to content policy restrictions.");
	}
	starved(Some(&response.stop_reason), &response.text())?;

	let mut resp: Response = response.into();
	resp.overhead = ttfb;
	return Ok(resp);

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
			ClaudeModel::from_str(&self.model).unwrap().cost().cents((&self.usage).into())
		}
	}
	impl From<ClaudeResponse> for Response {
		fn from(response: ClaudeResponse) -> Self {
			Self {
				text: response.text(),
				cost_cents: response.cost_cents(),
				duration: std::time::Duration::ZERO,
				overhead: std::time::Duration::ZERO,
				model: String::new(),
				thinking: ThinkingLevel::None,
			}
		}
	}
}
//,}}}

#[cfg(test)]
mod tests {
	#[test]
	fn deser_model() {
		let model = "claude-sonnet-5".parse::<super::ClaudeModel>().unwrap();
		assert_eq!(model, super::ClaudeModel::Sonnet5);
	}
}
