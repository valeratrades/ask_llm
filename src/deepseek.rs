use eyre::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{Backend, Cost, FORCE_JSON_SUFFIX, Request, Response, ThinkingLevel};

/// `deepseek-chat`/`deepseek-reasoner` were discontinued 2026-07-24, thinking is now a request param instead of a model
const MODEL: &str = "deepseek-v4-flash";
/// Off-peak rates, ref: https://api-docs.deepseek.com/quick_start/pricing. Writing to cache is not charged for.
const OFF_PEAK_COST: Cost = Cost {
	million_input_tokens: 0.22,
	million_cached_input_tokens: 0.007,
	million_cache_write_tokens: 0.0,
	million_output_tokens: 0.66,
};

/// Peak is 01:00-04:00 and 06:00-10:00 UTC, Monday through Friday, and bills at double. Which window a request
/// landed in is only knowable from the clock — the response carries no marker for it.
fn cost_now() -> Cost {
	let secs = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.expect("system clock is set later than 1970")
		.as_secs();
	peak_rates(secs)
}

fn peak_rates(unix_secs: u64) -> Cost {
	let weekday = (unix_secs / 86_400 + 4) % 7; // the epoch fell on a Thursday, so 0 is Sunday
	let hour = unix_secs % 86_400 / 3_600;
	match (1..=5).contains(&weekday) && matches!(hour, 1..4 | 6..10) {
		true => Cost {
			million_input_tokens: OFF_PEAK_COST.million_input_tokens * 2.0,
			million_cached_input_tokens: OFF_PEAK_COST.million_cached_input_tokens * 2.0,
			million_cache_write_tokens: OFF_PEAK_COST.million_cache_write_tokens * 2.0,
			million_output_tokens: OFF_PEAK_COST.million_output_tokens * 2.0,
		},
		false => OFF_PEAK_COST,
	}
}

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
			cost_cents: cost_now().cents((&parsed.usage).into()),
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
/// The hit/miss split partitions `prompt_tokens`; a miss is what pays the write, which is not billed separately.
struct DeepSeekUsage {
	completion_tokens: u32,
	prompt_cache_hit_tokens: u32,
	prompt_cache_miss_tokens: u32,
}
impl From<&DeepSeekUsage> for crate::Usage {
	fn from(usage: &DeepSeekUsage) -> Self {
		Self {
			input: usage.prompt_cache_miss_tokens,
			cached_input: usage.prompt_cache_hit_tokens,
			cache_write: 0,
			output: usage.completion_tokens,
		}
	}
}

#[derive(Debug, Deserialize)]
struct DeepSeekResponse {
	choices: Vec<DeepSeekChoice>,
	usage: DeepSeekUsage,
}

#[cfg(test)]
mod tests {
	/// The weekday is derived by hand from the epoch, which is the only part of the peak window that cannot be read off the constants.
	#[test]
	fn peak_window() {
		let peak = |unix_secs| super::peak_rates(unix_secs).million_output_tokens > super::OFF_PEAK_COST.million_output_tokens;
		assert!(peak(1_788_228_000), "tue 02:00 utc");
		assert!(peak(1_788_247_800), "tue 07:30 utc");
		assert!(!peak(1_788_238_800), "tue 05:00 utc, the gap between the two peak blocks");
		assert!(!peak(1_788_256_800), "tue 10:00 utc, one second past the end");
		assert!(!peak(1_788_573_600), "sat 02:00 utc, weekends bill off-peak throughout");
	}
}
