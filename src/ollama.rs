use eyre::{Result, bail};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::{Conversation, Response, Role};

const OLLAMA_MODEL: &str = "qwen3.5:9b";
const OLLAMA_URL: &str = "http://localhost:11434/v1/chat/completions";

pub async fn ask_ollama(
	conversation: &Conversation,
	temperature: Option<f32>,
	requested_max_tokens: Option<usize>,
	stop_sequences: Option<Vec<impl AsRef<str>>>,
	force_json: bool,
) -> Result<Response> {
	let mut messages: Vec<OllamaMessage> = Vec::new();

	for message in &conversation.0 {
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

	if force_json {
		// Append instruction to return JSON
		if let Some(last) = messages.last_mut() {
			if last.role == "user" {
				last.content.push_str("\n\nRespond with valid JSON only, no other text.");
			}
		}
	}

	let mut request = OllamaRequest {
		model: OLLAMA_MODEL.to_string(),
		messages,
		temperature: temperature.unwrap_or(0.0),
		max_tokens: requested_max_tokens,
		stop: None,
		stream: false,
	};

	if let Some(seqs) = stop_sequences {
		request.stop = Some(seqs.iter().map(|s| s.as_ref().to_string()).collect());
	}

	let response = Client::new().post(OLLAMA_URL).json(&request).send().await?;

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

	let text = parsed.choices.into_iter().next().map(|c| c.message.content).unwrap_or_default();

	// Ollama/local models have zero API cost
	Ok(Response::new(text, 0.0))
}

#[derive(Debug, Serialize)]
struct OllamaRequest {
	model: String,
	messages: Vec<OllamaMessage>,
	temperature: f32,
	#[serde(skip_serializing_if = "Option::is_none")]
	max_tokens: Option<usize>,
	#[serde(skip_serializing_if = "Option::is_none")]
	stop: Option<Vec<String>>,
	stream: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct OllamaMessage {
	role: String,
	content: String,
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
	choices: Vec<OllamaChoice>,
}

#[derive(Debug, Deserialize)]
struct OllamaChoice {
	message: OllamaMessage,
}
