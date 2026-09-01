use eyre::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{Backend, FORCE_JSON_SUFFIX, Request, Response, ThinkingLevel};

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
			let text = match &message.content {
				crate::MessageContent::Text(t) => t.clone(),
				_ => bail!("Ollama backend only supports text messages"),
			};
			messages.push(OllamaMessage {
				role: <&str>::from(message.role).to_string(),
				content: text,
			});
		}

		if request.force_json
			&& let Some(last) = messages.last_mut()
			&& last.role == "user"
		{
			last.content.push_str(FORCE_JSON_SUFFIX);
		}

		let think = !matches!(request.thinking, ThinkingLevel::None);

		let mut ollama_request = OllamaRequest {
			model: self.model.clone(),
			messages,
			stream: false,
			think,
			options: OllamaOptions {
				temperature: 0.0,
				num_predict: request.max_tokens,
				stop: None,
			},
		};

		if let Some(ref seqs) = request.stop_sequences {
			ollama_request.options.stop = Some(seqs.iter().map(|s| s.to_string()).collect());
		}

		let response = reqwest::Client::new().post(&self.url).json(&ollama_request).send().await?;
		let parsed: OllamaResponse = crate::json_response(response, "Ollama").await?;

		let overhead_nanos = parsed.load_duration + parsed.prompt_eval_duration;
		Ok(Response {
			text: parsed.message.content,
			cost_cents: 0.0,
			duration: std::time::Duration::ZERO,
			overhead: std::time::Duration::from_nanos(overhead_nanos),
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
	#[serde(default)]
	load_duration: u64,
	#[serde(default)]
	prompt_eval_duration: u64,
}
