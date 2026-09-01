use eyre::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{Backend, Cost, FORCE_JSON_SUFFIX, Request, Response, ThinkingLevel};

/// `deepseek-chat`/`deepseek-reasoner` were discontinued 2026-07-24, thinking is now a request param instead of a model
const MODEL: &str = "deepseek-v4-flash";
/// cache-miss rates, ref: https://api-docs.deepseek.com/quick_start/pricing
const COST: Cost = Cost {
	million_input_tokens: 0.14,
	million_output_tokens: 0.28,
};

pub(crate) struct DeepSeek {
	pub api_key: String,
}
impl DeepSeek {
	///docs: https://api-docs.deepseek.com/api/create-chat-completion
	async fn do_conversation(&self, request: &Request<'_>) -> Result<Response> {
		if !request.files.is_empty() {
			bail!("DeepSeek backend does not support file attachments");
		}

		let mut messages: Vec<DeepSeekMessage> = Vec::new();
		for message in &request.conversation.0 {
			let text = match &message.content {
				crate::MessageContent::Text(t) => t.clone(),
				_ => bail!("DeepSeek backend only supports text messages"),
			};
			messages.push(DeepSeekMessage {
				role: <&str>::from(message.role).to_string(),
				content: text,
			});
		}

		if request.force_json {
			// json_object mode returns empty content unless the prompt itself asks for json
			let last = messages.last_mut().expect("conversation is never empty");
			last.content.push_str(FORCE_JSON_SUFFIX);
		}

		// thinking is on by default server-side, so `None` has to disable it explicitly
		let thinking = match request.thinking {
			ThinkingLevel::None => json!({"type": "disabled"}),
			ThinkingLevel::Low => json!({"type": "enabled", "reasoning_effort": "low"}),
			ThinkingLevel::Medium => json!({"type": "enabled", "reasoning_effort": "high"}),
			ThinkingLevel::High => json!({"type": "enabled", "reasoning_effort": "max"}),
		};

		let mut payload = json!({
			"model": MODEL,
			"messages": messages,
			"thinking": thinking,
			"stream": false,
		});
		let payload_map = payload.as_object_mut().unwrap();
		// thinking mode silently discards sampling params
		if matches!(request.thinking, ThinkingLevel::None) {
			payload_map.insert("temperature".to_string(), json!(0.0));
		}
		if let Some(max_tokens) = request.max_tokens {
			payload_map.insert("max_tokens".to_string(), json!(max_tokens));
		}
		if let Some(ref stop_seqs) = request.stop_sequences {
			payload_map.insert("stop".to_string(), json!(stop_seqs));
		}
		if request.force_json {
			payload_map.insert("response_format".to_string(), json!({"type": "json_object"}));
		}
		tracing::debug!(?payload);

		let ttfb_start = std::time::Instant::now();
		let http_response = reqwest::Client::new()
			.post("https://api.deepseek.com/chat/completions")
			.bearer_auth(&self.api_key)
			.json(&payload)
			.send()
			.await?;
		let ttfb = ttfb_start.elapsed();
		let parsed: DeepSeekResponse = crate::json_response(http_response, "DeepSeek").await?;

		let choice = match parsed.choices.into_iter().next() {
			Some(choice) => choice,
			None => bail!("DeepSeek returned no choices"),
		};
		if choice.finish_reason == "content_filter" {
			bail!("DeepSeek refused to process the request. This may be due to content policy restrictions.");
		}

		Ok(Response {
			text: choice.message.content,
			cost_cents: COST.cents(parsed.usage.prompt_tokens, parsed.usage.completion_tokens),
			duration: std::time::Duration::ZERO,
			overhead: ttfb,
			model: MODEL.to_string(),
			thinking: request.thinking,
		})
	}
}

impl Backend for DeepSeek {
	fn conversation<'a>(&'a self, request: &'a Request<'a>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response>> + Send + 'a>> {
		Box::pin(self.do_conversation(request))
	}
}

#[derive(Debug, Deserialize, Serialize)]
struct DeepSeekMessage {
	role: String,
	content: String,
}

#[derive(Debug, Deserialize)]
struct DeepSeekChoice {
	message: DeepSeekMessage,
	finish_reason: String,
}

#[derive(Debug, Deserialize)]
struct DeepSeekUsage {
	prompt_tokens: u32,
	completion_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct DeepSeekResponse {
	choices: Vec<DeepSeekChoice>,
	usage: DeepSeekUsage,
}
