use std::str::FromStr as _;

use eyre::{Result, bail};
use futures::stream::StreamExt;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Backend, Conversation, FileAttachment, Request, Response, Role, ThinkingLevel};

pub struct Cost {
	pub million_input_tokens: f32,
	pub million_output_tokens: f32,
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
		headers.insert("anthropic-beta", HeaderValue::from_static("output-128k-2025-02-19,structured-outputs-2025-11-13"));
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

		let max_tokens = match request.max_tokens {
			Some(max_tokens) => max_tokens.min(self.model.max_tokens()),
			_ => self.model.max_tokens(),
		};

		// Payload {{{
		let thinking_budget = match request.thinking {
			ThinkingLevel::None => None,
			ThinkingLevel::Low => Some(2_048),
			ThinkingLevel::Medium => Some(8_192),
			ThinkingLevel::High => Some(32_000),
		};
		// Claude requires temperature=1 when extended thinking is enabled
		let temperature = match thinking_budget {
			Some(_) => 1.0,
			None => request.temperature.unwrap_or(0.0),
		};
		let mut payload = json!({
			"model": self.model.to_str(),
			"temperature": temperature,
			"max_tokens": max_tokens,
			"messages": conversation.messages
		});
		if let Some(budget) = thinking_budget {
			payload
				.as_object_mut()
				.unwrap()
				.insert("thinking".to_string(), json!({"type": "enabled", "budget_tokens": budget}));
		}
		if let Some(ref stop_seqs) = request.stop_sequences {
			payload.as_object_mut().unwrap().insert("stop_sequences".to_string(), serde_json::json!(stop_seqs));
		}
		if let Some(system_message) = system_message {
			payload.as_object_mut().unwrap().insert("system".to_string(), serde_json::json!(system_message));
		}
		if request.force_json {
			// Use prefill approach - Claude's structured outputs require strict schemas with
			// additionalProperties: false, which doesn't work for generic JSON responses.
			// Prefill works universally across all models.
			conversation.messages.push(ClaudeMessage {
				role: "assistant",
				content: ClaudeMessageContent::Text("{".to_string()),
			});
			payload.as_object_mut().unwrap().insert("messages".to_string(), serde_json::json!(conversation.messages));
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

		// Prepend the "{" we used for prefilling when force_json was enabled
		if request.force_json {
			response.text = format!("{{{}", response.text);
		}

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

			messages.push(ClaudeMessage { role, content });
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
	text: String,
}
#[derive(Debug, Deserialize)]
struct ClaudeUsage {
	input_tokens: u32,
	output_tokens: u32,
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
	let mut response_stream = request_builder.send().await?.bytes_stream();
	let ttfb = ttfb_start.elapsed();

	let mut accumulated_message = String::new();

	fn parse_sse(bytes: bytes::Bytes) -> String {
		let s = String::from_utf8(bytes.to_vec()).expect("Found invalid UTF-8");
		let mut parsed_string = String::new();

		let split = s
			.split("event: content_block_delta\ndata: ")
			.map(|s| s.split("\n\nevent: ").collect::<Vec<&str>>().get(0).unwrap().to_string())
			.collect::<Vec<String>>();

		for s in split {
			if let Ok(v) = serde_json::from_str::<DeltaContentBlock>(&s)
				&& (v.response_type == "content_block_delta" || v.delta.delta_type == "text_delta")
			{
				parsed_string.push_str(&v.delta.text);
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
	Ok(Response {
		text: accumulated_message,
		cost_cents: cost,
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
	let value = request_builder.send().await?.json::<Value>().await?;
	let ttfb = ttfb_start.elapsed();
	tracing::debug!(?value);
	let response = serde_json::from_value::<ClaudeResponse>(value.clone()).inspect_err(|e| {
		eprintln!(
			"Failed to parse Claude response. Response JSON: {}\n{e:?}",
			serde_json::to_string_pretty(&value).unwrap_or_else(|_| format!("{:?}", value))
		);
	})?;

	// Check for refusal
	if response.stop_reason == "refusal" {
		bail!("Claude refused to process the request. This may be due to content policy restrictions.");
	}

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
		let model = "claude-haiku-4-5-20251001".parse::<super::ClaudeModel>().unwrap();
		assert_eq!(model, super::ClaudeModel::Haiku45);
	}
}
