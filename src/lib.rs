#![feature(default_field_values)]
use std::{future::Future, path::Path, pin::Pin};

use eyre::{Result, bail};

mod claude;
mod deepseek;
mod error;
mod ollama;
mod openai;
pub use error::MissingToken;

impl Client {
	/// Keys absent from `config` are looked up in the environment when the request is made.
	pub fn new(config: config::AppConfig) -> Self {
		Self {
			config,
			model: Model::default(),
			max_tokens: None,
			stop_sequences: None,
			force_json: false,
			files: Vec::new(),
			thinking: ThinkingLevel::default(),
		}
	}

	pub fn model(mut self, model: Model) -> Self {
		self.model = model;
		self
	}

	pub fn claude_token(mut self, token: impl Into<String>) -> Self {
		self.config.claude_token = Some(token.into());
		self
	}

	pub fn deepseek_token(mut self, token: impl Into<String>) -> Self {
		self.config.deepseek_token = Some(token.into());
		self
	}

	pub fn openai_token(mut self, token: impl Into<String>) -> Self {
		self.config.openai_token = Some(token.into());
		self
	}

	pub fn max_tokens(mut self, max_tokens: usize) -> Self {
		self.max_tokens = Some(max_tokens);
		self
	}

	pub fn stop_sequences<T: Into<String>>(mut self, sequences: Vec<T>) -> Self {
		self.stop_sequences = Some(sequences.into_iter().map(Into::into).collect());
		self
	}

	pub fn force_json(mut self) -> Self {
		self.force_json = true;
		self
	}

	pub fn thinking(mut self, level: ThinkingLevel) -> Self {
		self.thinking = level;
		self
	}

	/// Append a file to be included with the request.
	/// Supported media types: application/pdf, text/plain, text/markdown, text/csv,
	/// application/vnd.openxmlformats-officedocument.wordprocessingml.document (docx),
	/// application/vnd.openxmlformats-officedocument.spreadsheetml.sheet (xlsx)
	pub fn append_file(mut self, base64_data: String, media_type: String) -> Self {
		self.files.push(FileAttachment { base64_data, media_type });
		self
	}

	/// Append a file from a filesystem path.
	pub fn append_file_from_path(self, path: impl AsRef<Path>) -> Result<Self> {
		let path = path.as_ref();
		let data = std::fs::read(path)?;
		let base64_data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);
		let media_type = mime_type_from_extension(path.extension().and_then(|s| s.to_str()).unwrap_or(""));
		Ok(self.append_file(base64_data, media_type.to_string()))
	}

	pub async fn ask(&self, message: impl Into<String>) -> Result<Response> {
		let mut conv = Conversation::new();
		conv.add(Role::User, message.into());
		self.conversation(&conv).await
	}

	pub async fn conversation(&self, conv: &Conversation) -> Result<Response> {
		let stop_seqs: Option<Vec<&str>> = self.stop_sequences.as_ref().map(|v| v.iter().map(|s| s.as_str()).collect());
		let request = Request {
			conversation: conv,
			max_tokens: self.max_tokens,
			stop_sequences: stop_seqs,
			force_json: self.force_json,
			files: &self.files,
			thinking: self.thinking,
		};
		let backend = self.model.into_backend(&self.config)?;
		let start = std::time::Instant::now();
		let mut response = backend.conversation(&request).await?;
		response.duration = start.elapsed();
		Ok(response)
	}
}

impl Model {
	/// Resolved per request rather than at construction, so a key that is missing for *this* model
	/// surfaces as [`MissingToken`] on the call that needs it.
	fn into_backend(self, config: &config::AppConfig) -> Result<Box<dyn Backend>, MissingToken> {
		Ok(match self {
			Model::Cheap => Box::new(ollama::Ollama {
				model: "qwen3.5:4b".to_string(),
				url: "http://localhost:11434/api/chat".to_string(),
			}),
			Model::Translate => Box::new(ollama::Ollama {
				model: "translategemma:4b".to_string(),
				url: "http://localhost:11434/api/chat".to_string(),
			}),
			Model::Fast => Box::new(openai::OpenAi {
				api_key: openai_api_key(config, "gpt-5.6-luna")?,
				model: openai::OpenAiModel::Luna,
			}),
			Model::Medium => Box::new(openai::OpenAi {
				api_key: openai_api_key(config, "gpt-5.6-terra")?,
				model: openai::OpenAiModel::Terra,
			}),
			Model::Slow => Box::new(claude::Claude {
				api_key: claude_api_key(config, "claude-opus-5")?,
				model: claude::ClaudeModel::Opus5,
			}),
			Model::PriceInsensitive => Box::new(claude::Claude {
				api_key: claude_api_key(config, "claude-fable-5-1")?,
				model: claude::ClaudeModel::Fable5,
			}),
		})
	}
}

impl Message {
	fn new(role: Role, content: impl Into<String>) -> Self {
		Self {
			role,
			content: MessageContent::Text(content.into()),
		}
	}

	pub fn new_with_image(role: Role, base64_data: String, media_type: String) -> Self {
		Self {
			role,
			content: MessageContent::Image { base64_data, media_type },
		}
	}

	pub fn new_with_text_and_images(role: Role, text: String, images: Vec<ImageContent>) -> Self {
		Self {
			role,
			content: MessageContent::TextAndImages { text, images },
		}
	}
}

impl Conversation {
	pub fn new() -> Self {
		Self(Vec::new())
	}

	pub fn new_with_system(system_message: impl Into<String>) -> Self {
		Self(vec![Message::new(Role::System, system_message)])
	}

	pub fn add(&mut self, role: Role, content: impl Into<String>) {
		self.0.push(Message::new(role, content));
	}

	pub fn add_exchange(&mut self, user_message: impl Into<String>, assistant_message: impl Into<String>) {
		self.add(Role::User, user_message);
		self.add(Role::Assistant, assistant_message);
	}
}

impl Response {
	/// Extract codeblocks with optional extension filtering.
	/// If extensions is None or empty, all codeblocks are returned.
	/// Extensions are tried in reverse sorted order (longer extensions first).
	/// Returns an empty Vec if no matching codeblocks are found.
	pub fn extract_codeblocks(&self, extensions: Option<Vec<&str>>) -> Vec<String> {
		let sorted_extensions = extensions.map(|mut exts| {
			exts.sort_by_key(|b| std::cmp::Reverse(b.len()));
			exts
		});

		self.text
			.split("```")
			.enumerate()
			.filter_map(|(i, s)| {
				if i % 2 == 1 {
					match &sorted_extensions {
						Some(exts) if !exts.is_empty() => {
							for ext in exts {
								if s.starts_with(ext) {
									return Some(s.strip_prefix(ext).unwrap().trim().to_string());
								}
							}
							None
						}
						_ => {
							let code = match s.split_once('\n') {
								Some((_, rest)) => rest.trim().to_string(),
								_ => s.trim().to_string(),
							};
							Some(code)
						}
					}
				} else {
					None
				}
			})
			.collect()
	}

	/// Convenience wrapper around [extract_codeblocks](#method.extract_codeblocks).
	/// Returns an error unless exactly one codeblock is found.
	pub fn extract_codeblock(&self, extensions: Option<Vec<&str>>) -> Result<String> {
		let blocks = self.extract_codeblocks(extensions);
		if blocks.len() == 1 {
			Ok(blocks.into_iter().next().unwrap())
		} else {
			bail!("No codeblocks found or more than one codeblock found.")
		}
	}

	pub fn extract_html_tag(&self, tag_name: &str) -> Result<String> {
		let opening_tag = format!("<{tag_name}>");
		let closing_tag = format!("</{tag_name}>");
		let from_start = self.text.split_once(&opening_tag).unwrap().1;
		let extracted = from_start.split_once(&closing_tag).unwrap().0;
		Ok(extracted.to_string())
	}
}

pub mod config;
mod shortcuts;
mod transcribe;
pub mod tts;
pub use shortcuts::*;
pub use transcribe::transcribe;

/// Every remote model currently offered by every provider here caps out at the same place.
pub(crate) const MAX_TOKENS: usize = 128_000;
/// Fences are mentioned because the models otherwise wrap the object in them even when told to emit json only.
pub(crate) const FORCE_JSON_SUFFIX: &str = "\n\nRespond with valid JSON only, no other text or markdown fences.";
#[derive(Debug)]
pub struct Response {
	pub text: String,
	pub cost_cents: f32,
	pub duration: std::time::Duration,
	/// Overhead before generation starts (model load for Ollama, network TTFB for Claude).
	pub overhead: std::time::Duration,
	pub model: String,
	pub thinking: ThinkingLevel,
}

#[derive(Clone, Debug, Default)]
pub struct Conversation(pub Vec<Message>);

#[derive(Clone, Debug)]
pub struct Message {
	pub(crate) role: Role,
	pub(crate) content: MessageContent,
}

#[derive(Clone, Debug)]
pub struct ImageContent {
	pub base64_data: String,
	pub media_type: String,
}

#[derive(Clone, Debug)]
pub enum ContentPart {
	Text(String),
	Image { base64_data: String, media_type: String },
	Document { base64_data: String, media_type: String },
}

#[derive(Clone, Debug)]
pub enum MessageContent {
	Text(String),
	Image { base64_data: String, media_type: String },
	TextAndImages { text: String, images: Vec<ImageContent> },
	Document { base64_data: String, media_type: String },
	Mixed { parts: Vec<ContentPart> },
}

#[derive(Clone, Copy, Debug)]
pub enum Role {
	System,
	User,
	Assistant,
}

#[derive(Clone, Copy, Debug, Default)]
pub enum ThinkingLevel {
	#[default]
	None,
	Low,
	Medium,
	High,
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, derive_more::FromStr)]
pub enum Model {
	Fast,
	#[default]
	Medium,
	Slow,
	PriceInsensitive,
	Cheap,
	Translate,
}

#[derive(Clone, Debug)]
pub struct FileAttachment {
	pub base64_data: String,
	pub media_type: String,
}

/// Client for interacting with LLMs.
///
/// Default settings produce a simple oneshot call with Model::Medium.
pub struct Client {
	config: config::AppConfig,
	model: Model,
	max_tokens: Option<usize>,
	stop_sequences: Option<Vec<String>>,
	force_json: bool,
	files: Vec<FileAttachment>,
	thinking: ThinkingLevel,
}
pub(crate) trait Backend: Send + Sync {
	fn conversation<'a>(&'a self, request: &'a Request<'a>) -> Pin<Box<dyn Future<Output = Result<Response>> + Send + 'a>>;
}

/// Per-1M-token rates, as every provider quotes them.
pub(crate) struct Cost {
	pub million_input_tokens: f32,
	/// Input served from a cached prefix, which every provider here discounts to a tenth.
	pub million_cached_input_tokens: f32,
	/// Input written into the cache. Zero where the provider charges nothing for the write.
	pub million_cache_write_tokens: f32,
	pub million_output_tokens: f32,
}
impl Cost {
	pub fn cents(&self, usage: Usage) -> f32 {
		(usage.input as f32 * self.million_input_tokens
			+ usage.cached_input as f32 * self.million_cached_input_tokens
			+ usage.cache_write as f32 * self.million_cache_write_tokens
			+ usage.output as f32 * self.million_output_tokens)
			/ 10_000.0
	}
}

/// Token counts lifted out of a provider's usage block. Providers disagree on whether the cache tiers are a
/// breakdown of the input count or additional to it, so each backend resolves its own before filling this in.
#[derive(Debug, Default)]
pub(crate) struct Usage {
	/// Billed at full rate: neither served from cache nor written to it.
	pub input: u32,
	pub cached_input: u32,
	pub cache_write: u32,
	pub output: u32,
}

impl From<Role> for &'static str {
	fn from(role: Role) -> Self {
		match role {
			Role::System => "system",
			Role::User => "user",
			Role::Assistant => "assistant",
		}
	}
}

/// The body carries the provider's explanation of a rejection, which `error_for_status` throws away, and the raw
/// json is logged before deserializing so that a schema drift is legible rather than a bare serde path.
pub(crate) async fn json_response<T: serde::de::DeserializeOwned>(response: reqwest::Response, provider: &str) -> Result<T> {
	let status = response.status();
	if !status.is_success() {
		bail!("{provider} request failed ({status}): {}", response.text().await?);
	}
	let value: serde_json::Value = response.json().await?;
	tracing::debug!(provider, ?value);
	serde_json::from_value(value.clone()).map_err(|e| {
		eyre::eyre!(
			"failed to parse {provider} response: {e}\n{}",
			serde_json::to_string_pretty(&value).unwrap_or_else(|_| format!("{value:?}"))
		)
	})
}
fn claude_api_key(config: &config::AppConfig, model: &'static str) -> Result<String, MissingToken> {
	config
		.claude_token
		.clone()
		.or_else(|| std::env::var("CLAUDE_TOKEN").ok())
		.ok_or_else(|| MissingToken::new("Anthropic", model, "claude_token", "CLAUDE_TOKEN", "claude_token"))
}
fn deepseek_api_key(config: &config::AppConfig, model: &'static str) -> Result<String, MissingToken> {
	config
		.deepseek_token
		.clone()
		.or_else(|| std::env::var("DEEPSEEK_KEY").ok())
		.ok_or_else(|| MissingToken::new("DeepSeek", model, "deepseek_token", "DEEPSEEK_KEY", "deepseek_token"))
}
fn openai_api_key(config: &config::AppConfig, model: &'static str) -> Result<String, MissingToken> {
	config
		.openai_token
		.clone()
		.or_else(|| std::env::var("OPENAI_API_KEY").ok())
		.ok_or_else(|| MissingToken::new("OpenAI", model, "openai_token", "OPENAI_API_KEY", "openai_token"))
}

pub(crate) struct Request<'a> {
	pub conversation: &'a Conversation,
	pub max_tokens: Option<usize>,
	pub stop_sequences: Option<Vec<&'a str>>,
	pub force_json: bool,
	pub files: &'a [FileAttachment],
	pub thinking: ThinkingLevel,
}

impl std::fmt::Debug for Client {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Client")
			.field("max_tokens", &self.max_tokens)
			.field("stop_sequences", &self.stop_sequences)
			.field("force_json", &self.force_json)
			.field("thinking", &self.thinking)
			.field("files", &self.files)
			.finish_non_exhaustive()
	}
}

fn mime_type_from_extension(ext: &str) -> &'static str {
	match ext.to_lowercase().as_str() {
		"pdf" => "application/pdf",
		"txt" => "text/plain",
		"md" => "text/markdown",
		"csv" => "text/csv",
		"docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
		"xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
		"png" => "image/png",
		"jpg" | "jpeg" => "image/jpeg",
		"gif" => "image/gif",
		"webp" => "image/webp",
		_ => "application/octet-stream",
	}
}

impl Default for Client {
	fn default() -> Self {
		Self::new(config::AppConfig::default())
	}
}

impl std::fmt::Display for ThinkingLevel {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::None => write!(f, "none"),
			Self::Low => write!(f, "low"),
			Self::Medium => write!(f, "medium"),
			Self::High => write!(f, "high"),
		}
	}
}

impl std::fmt::Display for Response {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let secs = self.duration.as_secs_f32();
		let overhead = self.overhead.as_secs_f32();
		let gen_secs = secs - overhead;
		let chars = self.text.len();
		let ms_per_char = if chars > 0 { gen_secs * 1000.0 / chars as f32 } else { 0.0 };
		write!(
			f,
			"[model: {} | thinking: {} | cost: {:.4}¢ | overhead: {overhead:.1}s | gen: {gen_secs:.1}s | {ms_per_char:.1}ms/char]",
			self.model, self.thinking, self.cost_cents
		)
	}
}
