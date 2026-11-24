use std::path::Path;

use crate::{
	Provider, Response,
	providers::{claude, openai},
};

/// Token counts lifted out of a provider's usage block. Providers disagree on whether the cache tiers are a
/// breakdown of the input count or additional to it, so each wire resolves its own before filling this in.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct Usage {
	/// Billed at full rate: neither served from cache nor written to it.
	pub input: u32,
	pub cached_input: u32,
	pub cache_write: u32,
	pub output: u32,
}
impl Usage {
	/// What these tokens list for. `None`: we hold no price of the model's.
	pub(crate) fn cents(self, provider: Provider, model: &str) -> Option<f32> {
		let cards = match provider {
			Provider::Claude => claude::prices(model)?,
			Provider::OpenAi => openai::prices(model)?,
			Provider::Ollama | Provider::Local => return Some(0.),
			Provider::Fal => return None,
		};
		let prompt = self.input + self.cached_input + self.cache_write;
		let card = cards.iter().find(|c| prompt <= c.up_to).expect("the last card of a model has no bound");
		Some((self.input as f32 * card.input + self.cached_input as f32 * card.cached_input + self.cache_write as f32 * card.cache_write + self.output as f32 * card.output) / 10_000.0)
	}
}

/// USD per 1M tokens, as every provider quotes them, for a prompt of at most `up_to` tokens.
pub(crate) struct Card {
	pub up_to: u32,
	pub input: f32,
	pub cached_input: f32,
	pub cache_write: f32,
	pub output: f32,
}

/// One line per answered call, under the app's `argv[0]` stem; see docs/ARCHITECTURE.md#usage-log.
pub(crate) fn log(requested: &str, response: &Response) {
	use std::io::Write as _;

	let argv0 = std::env::args_os().next().expect("every process is started with argv[0]");
	let app = Path::new(&argv0).file_stem().expect("argv[0] names a file").to_string_lossy().into_owned();
	let now = jiff::Timestamp::now();
	let dir = Path::new(&v_utils::io::xdg::xdg_data_fallback()).join("ask_llm/usage").join(app);
	let usage = response.usage;
	let mut line = serde_json::json!({
		"ts": now.strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
		"requested": requested,
		"model": response.model,
		"thinking": response.thinking.to_string(),
		"cost_cents": response.cents,
		"input": usage.input,
		"cached_input": usage.cached_input,
		"cache_write": usage.cache_write,
		"output": usage.output,
		"duration_ms": response.duration.as_millis(),
		"overhead_ms": response.overhead.as_millis(),
	})
	.to_string();
	line.push('\n');
	let written = std::fs::create_dir_all(&dir)
		.and_then(|()| std::fs::OpenOptions::new().create(true).append(true).open(dir.join(now.strftime("%Y-%m.jsonl").to_string())))
		.and_then(|mut f| f.write_all(line.as_bytes())); // one write on an O_APPEND fd, so concurrent callers don't interleave
	if let Err(error) = written {
		// the answer is already paid for, and a server's may have nowhere to write
		tracing::error!(dir = %dir.display(), %error, "the usage log is not written");
	}
}
