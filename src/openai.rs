use std::str::FromStr as _;

use eyre::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Backend, ContentPart, FileAttachment, MessageContent, Request, Response, Role, ThinkingLevel, claude::Cost};

const FORCE_JSON_SUFFIX: &str = "\n\nRespond with valid JSON only, no other text.";

pub(crate) struct OpenAi {
	pub api_key: String,
	pub model: OpenAiModel,
}
impl OpenAi {
	///docs: https://platform.openai.com/docs/api-reference/chat/create
	async fn do_conversation(&self, request: &Request<'_>) -> Result<Response> {
		let mut messages: Vec<OpenAiMessage> = request
			.conversation
			.0
			.iter()
			.map(|message| OpenAiMessage {
				role: match message.role {
					Role::System => "system",
					Role::User => "user",
					Role::Assistant => "assistant",
				},
				content: (&message.content).into(),
			})
			.collect();

		if !request.files.is_empty()
			&& let Some(first_user_msg) = messages.iter_mut().find(|m| m.role == "user")
		{
			let mut parts: Vec<OpenAiPart> = request.files.iter().map(file_to_part).collect();
			parts.extend(first_user_msg.content.take_parts());
			first_user_msg.content = OpenAiContent::Parts(parts);
		}

		if request.force_json {
			let last = messages.last_mut().expect("conversation is never empty");
			match &mut last.content {
				OpenAiContent::Text(text) => text.push_str(FORCE_JSON_SUFFIX),
				OpenAiContent::Parts(parts) => parts.push(OpenAiPart::Text {
					text: FORCE_JSON_SUFFIX.to_string(),
				}),
			}
		}

		let effort = match request.thinking {
			ThinkingLevel::None => "none",
			ThinkingLevel::Low => "low",
			ThinkingLevel::Medium => "medium",
			ThinkingLevel::High => "high",
		};
		let max_tokens = match request.max_tokens {
			Some(max_tokens) => max_tokens.min(self.model.max_tokens()),
			_ => self.model.max_tokens(),
		};

		// `max_tokens` and `temperature` are both rejected by the reasoning models
		let mut payload = json!({
			"model": self.model.to_str(),
			"messages": messages,
			"reasoning_effort": effort,
			"max_completion_tokens": max_tokens,
			"stream": false,
		});
		if request.force_json {
			payload.as_object_mut().unwrap().insert("response_format".to_string(), json!({"type": "json_object"}));
		}
		tracing::debug!(?payload);

		let ttfb_start = std::time::Instant::now();
		let http_response = reqwest::Client::new()
			.post("https://api.openai.com/v1/chat/completions")
			.bearer_auth(&self.api_key)
			.json(&payload)
			.send()
			.await?;
		let ttfb = ttfb_start.elapsed();

		let status = http_response.status();
		if !status.is_success() {
			let body = http_response.text().await.unwrap_or_default();
			bail!("OpenAI request failed ({status}): {body}");
		}

		let value: Value = http_response.json().await?;
		tracing::debug!(?value);
		let parsed: OpenAiResponse = serde_json::from_value(value.clone()).inspect_err(|e| {
			eprintln!(
				"Failed to parse OpenAI response: {}\n{e:?}",
				serde_json::to_string_pretty(&value).unwrap_or_else(|_| format!("{value:?}"))
			);
		})?;

		let choice = match parsed.choices.into_iter().next() {
			Some(choice) => choice,
			None => bail!("OpenAI returned no choices"),
		};
		if choice.finish_reason == "content_filter" {
			bail!("OpenAI refused to process the request. This may be due to content policy restrictions.");
		}

		// `stop` is rejected outright by the reasoning models, so the sequences are cut out of the returned text instead. Same output, but the tokens past the cut are still billed.
		let mut text = choice.message.content;
		if let Some(ref stop_seqs) = request.stop_sequences
			&& let Some(cut) = stop_seqs.iter().filter_map(|s| text.find(s)).min()
		{
			text.truncate(cut);
		}

		let cost = OpenAiModel::from_str(&parsed.model)?.cost();
		let cost_cents = (parsed.usage.prompt_tokens as f32 * cost.million_input_tokens + parsed.usage.completion_tokens as f32 * cost.million_output_tokens) / 10_000.0;

		Ok(Response {
			text,
			cost_cents,
			duration: std::time::Duration::ZERO,
			overhead: ttfb,
			model: parsed.model,
			thinking: request.thinking,
		})
	}
}

impl Backend for OpenAi {
	fn conversation<'a>(&'a self, request: &'a Request<'a>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response>> + Send + 'a>> {
		Box::pin(self.do_conversation(request))
	}
}

#[derive(Debug, Eq, PartialEq)]
/// ref: https://platform.openai.com/docs/models
pub(crate) enum OpenAiModel {
	Sol,
	Terra,
	Luna,
}
impl OpenAiModel {
	fn to_str(&self) -> &str {
		match self {
			Self::Sol => "gpt-5.6-sol",
			Self::Terra => "gpt-5.6-terra",
			Self::Luna => "gpt-5.6-luna",
		}
	}

	pub fn cost(&self) -> Cost {
		match self {
			Self::Sol => Cost {
				million_input_tokens: 4.0, // promotional through 2026-11-21, after which it's 5.0/30.0
				million_output_tokens: 20.0,
			},
			Self::Terra => Cost {
				million_input_tokens: 2.0,
				million_output_tokens: 12.0,
			},
			Self::Luna => Cost {
				million_input_tokens: 0.2,
				million_output_tokens: 1.2,
			},
		}
	}

	pub fn max_tokens(&self) -> usize {
		128_000
	}
}
impl std::str::FromStr for OpenAiModel {
	type Err = eyre::Report;

	fn from_str(s: &str) -> Result<Self> {
		Ok(match s {
			_ if s.to_lowercase().contains("sol") => Self::Sol,
			_ if s.to_lowercase().contains("terra") => Self::Terra,
			_ if s.to_lowercase().contains("luna") => Self::Luna,
			_ => bail!("Unknown model: {s}"),
		})
	}
}

#[derive(Debug, Serialize)]
struct OpenAiMessage {
	role: &'static str,
	content: OpenAiContent,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum OpenAiContent {
	Text(String),
	Parts(Vec<OpenAiPart>),
}
impl OpenAiContent {
	fn take_parts(&mut self) -> Vec<OpenAiPart> {
		match std::mem::replace(self, Self::Parts(Vec::new())) {
			Self::Text(text) => vec![OpenAiPart::Text { text }],
			Self::Parts(parts) => parts,
		}
	}
}
impl From<&MessageContent> for OpenAiContent {
	fn from(content: &MessageContent) -> Self {
		match content {
			MessageContent::Text(text) => Self::Text(text.clone()),
			MessageContent::Image { base64_data, media_type } => Self::Parts(vec![image_part(base64_data, media_type)]),
			MessageContent::TextAndImages { text, images } => {
				let mut parts = vec![OpenAiPart::Text { text: text.clone() }];
				parts.extend(images.iter().map(|img| image_part(&img.base64_data, &img.media_type)));
				Self::Parts(parts)
			}
			MessageContent::Document { base64_data, media_type } => Self::Parts(vec![document_part(base64_data, media_type)]),
			MessageContent::Mixed { parts } => Self::Parts(
				parts
					.iter()
					.map(|part| match part {
						ContentPart::Text(text) => OpenAiPart::Text { text: text.clone() },
						ContentPart::Image { base64_data, media_type } => image_part(base64_data, media_type),
						ContentPart::Document { base64_data, media_type } => document_part(base64_data, media_type),
					})
					.collect(),
			),
		}
	}
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type")]
enum OpenAiPart {
	#[serde(rename = "text")]
	Text { text: String },
	#[serde(rename = "image_url")]
	ImageUrl { image_url: ImageUrl },
	#[serde(rename = "file")]
	File { file: FileData },
}

#[derive(Clone, Debug, Serialize)]
struct ImageUrl {
	url: String,
}

#[derive(Clone, Debug, Serialize)]
struct FileData {
	filename: String,
	file_data: String,
}

fn image_part(base64_data: &str, media_type: &str) -> OpenAiPart {
	OpenAiPart::ImageUrl {
		image_url: ImageUrl {
			url: format!("data:{media_type};base64,{base64_data}"),
		},
	}
}

fn document_part(base64_data: &str, media_type: &str) -> OpenAiPart {
	OpenAiPart::File {
		file: FileData {
			filename: format!("attachment.{}", media_type.rsplit('/').next().expect("rsplit always yields one element")),
			file_data: format!("data:{media_type};base64,{base64_data}"),
		},
	}
}

/// Mirrors `claude::file_to_content_block`: PDFs go up as files, images as data URIs, everything else is decoded into text.
fn file_to_part(file: &FileAttachment) -> OpenAiPart {
	use base64::Engine;
	match file.media_type.as_str() {
		"application/pdf" => document_part(&file.base64_data, &file.media_type),
		mt if mt.starts_with("image/") => image_part(&file.base64_data, &file.media_type),
		_ => {
			let decoded = base64::engine::general_purpose::STANDARD
				.decode(&file.base64_data)
				.ok()
				.and_then(|bytes| String::from_utf8(bytes).ok())
				.unwrap_or_else(|| format!("[Binary file: {}]", file.media_type));
			OpenAiPart::Text { text: decoded }
		}
	}
}

#[derive(Debug, Deserialize)]
struct OpenAiResponseMessage {
	content: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
	message: OpenAiResponseMessage,
	finish_reason: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
	prompt_tokens: u32,
	completion_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
	choices: Vec<OpenAiChoice>,
	usage: OpenAiUsage,
	model: String,
}

#[cfg(test)]
mod tests {
	#[test]
	fn deser_model() {
		let model = "gpt-5.6-terra".parse::<super::OpenAiModel>().unwrap();
		assert_eq!(model, super::OpenAiModel::Terra);
	}
}
