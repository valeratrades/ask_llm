use eyre::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{Backend, Request, Response, Role, ThinkingLevel};

pub(crate) struct Ollama {
	pub model: String,
	pub url: String,
}
impl Ollama {
	async fn do_conversation(&self, request: &Request<'_>) -> Result<Response> {
		if !request.files.is_empty() {
			bail!("Ollama backend does not support file attachments");
		}

		let mut messages: Vec<OllamaMessage> = Vec::new();

		for message in &request.conversation.0 {
			let role = match message.role {
				Role::System => "system",
				Role::User => "user",
				Role::Assistant => "assistant",
			};
			let text = match &message.content {
				crate::MessageContent::Text(t) => t.clone(),
				_ => bail!("Ollama backend only supports text messages"),
			};
			messages.push(OllamaMessage {
				role: role.to_string(),
				content: text,
			});
		}

		if request.force_json {
			if let Some(last) = messages.last_mut() {
				if last.role == "user" {
					last.content.push_str("\n\nRespond with valid JSON only, no other text.");
				}
			}
		}

		let think = !matches!(request.thinking, ThinkingLevel::None);

		let mut ollama_request = OllamaRequest {
			model: self.model.clone(),
			messages,
			stream: false,
			think,
			options: OllamaOptions {
				temperature: request.temperature.unwrap_or(0.0),
				num_predict: request.max_tokens,
				stop: None,
			},
		};

		if let Some(ref seqs) = request.stop_sequences {
			ollama_request.options.stop = Some(seqs.iter().map(|s| s.to_string()).collect());
		}

		let response = reqwest::Client::new().post(&self.url).json(&ollama_request).send().await?;

		let status = response.status();
		if !status.is_success() {
			let body = response.text().await.unwrap_or_default();
			bail!("Ollama request failed ({status}): {body}");
		}

		let value: serde_json::Value = response.json().await?;
		tracing::debug!(?value);

		let parsed: OllamaResponse = serde_json::from_value(value.clone()).inspect_err(|e| {
			eprintln!(
				"Failed to parse Ollama response: {}\n{e:?}",
				serde_json::to_string_pretty(&value).unwrap_or_else(|_| format!("{:?}", value))
			);
		})?;

		Ok(Response {
			text: parsed.message.content,
			cost_cents: 0.0,
			duration: std::time::Duration::ZERO,
			model: self.model.clone(),
			thinking: request.thinking,
		})
	}
}

impl Backend for Ollama {
	fn conversation<'a>(&'a self, request: &'a Request<'a>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response>> + Send + 'a>> {
		Box::pin(self.do_conversation(request))
	}
}

#[derive(Debug, Serialize)]
struct OllamaRequest {
	model: String,
	messages: Vec<OllamaMessage>,
	stream: bool,
	think: bool,
	options: OllamaOptions,
}

#[derive(Debug, Serialize)]
struct OllamaOptions {
	temperature: f32,
	#[serde(skip_serializing_if = "Option::is_none")]
	num_predict: Option<usize>,
	#[serde(skip_serializing_if = "Option::is_none")]
	stop: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct OllamaMessage {
	role: String,
	content: String,
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
	message: OllamaMessage,
}
