#![feature(default_field_values)]
#![feature(error_generic_member_access)]
use std::path::Path;

mod error;
mod model;
pub mod providers;
mod step;
mod usage;
mod wire;
pub use error::{Attempt, Cli, Error, Exhausted, Failure, Interrupted, Recoverable, Result, Transport, Unrecoverable};
pub use model::{Model, Provider, Source};
pub use step::{Step, Stop, Tool, ToolCall};
pub use usage::Usage;

impl Client {
	/// Keys absent from `config` are looked up when the request is made: in the environment, except Claude's (see [`config::AppConfig::claude_token`]).
	pub fn new(config: config::AppConfig) -> Self {
		Self {
			config,
			path: model::Path::Tiers(Model::default()),
			max_tokens: None,
			stop_sequences: None,
			force_json: false,
			schema: None,
			files: Vec::new(),
			thinking: ThinkingLevel::default(),
		}
	}

	/// The tier asked for; the ones below it stand in, in order, when it does not answer. A [`Self::fallback_chain`] outranks it.
	pub fn model(mut self, model: Model) -> Self {
		if let model::Path::Tiers(entry) = &mut self.path {
			*entry = model;
		}
		self
	}

	/// Exactly these, in order, and nothing else. Every one is tried until one answers, so the place of an attempt is that
	/// of its source:
	/// ```no_run
	/// use ask_llm::{Client, Model, Provider, Source, providers::claude::ClaudeModel};
	/// # fn f(theirs: &str, ours: &str) -> Client {
	/// Client::default().fallback_chain([
	/// 	Source::from(ClaudeModel::Opus5_5).token(theirs).theirs(),
	/// 	Source::from(ClaudeModel::Opus5_5).token(ours),
	/// 	Source::new(Provider::OpenAi, "gpt-5.6-terra"),
	/// 	Model::Fast.into(),
	/// ])
	/// # }
	/// ```
	pub fn fallback_chain<S: Into<Source>>(mut self, sources: impl IntoIterator<Item = S>) -> Self {
		let sources: Vec<Source> = sources.into_iter().map(Into::into).collect();
		assert!(!sources.is_empty(), "a chain names who answers");
		self.path = model::Path::Chain(sources);
		self
	}

	/// Overrides the device's current Claude account.
	pub fn claude_token(mut self, token: impl Into<String>) -> Self {
		self.config.claude_token = Some(token.into());
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

	/// What the answer must match: held to it by the provider, where it can be. Read it with [`Response::json`].
	pub fn json_schema(mut self, schema: serde_json::Value) -> Self {
		self.schema = Some(schema);
		self
	}

	pub fn thinking(mut self, level: ThinkingLevel) -> Self {
		self.thinking = level;
		self
	}

	/// Append a file to be included with the request.
	/// Supported media types: application/pdf, images, and whatever is utf-8 text.
	pub fn append_file(mut self, base64_data: String, media_type: String) -> Self {
		self.files.push(FileAttachment { base64_data, media_type });
		self
	}

	/// Append a file from a filesystem path.
	pub fn append_file_from_path(self, path: impl AsRef<Path>) -> eyre::Result<Self> {
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
		self.send(conv, &self.files, self.json()).await
	}

	async fn send(&self, conv: &Conversation, files: &[FileAttachment], json: Json<'_>) -> Result<Response> {
		let request = Request { files, json, ..self.request(conv) };
		let walked = model::walk(&self.path, async |source: Source| {
			let answer = source.call(&self.config, &request).await?;
			match answer.response.stop {
				Stop::Refused => Err(Unrecoverable::new_refused(answer.reason).into()),
				_ => Ok(answer.response),
			}
		})
		.await?;
		usage::log(&self.path.requested(), &walked.answer);
		Ok(walked.answer)
	}

	fn request<'a>(&'a self, conversation: &'a Conversation) -> Request<'a> {
		Request {
			conversation,
			rounds: &[],
			tools: &[],
			agentic: false,
			max_tokens: self.max_tokens,
			stop_sequences: self.stop_sequences.as_ref().map(|v| v.iter().map(|s| s.as_str()).collect()),
			json: self.json(),
			files: &self.files,
			thinking: self.thinking,
		}
	}

	fn json(&self) -> Json<'_> {
		match &self.schema {
			Some(schema) => Json::Schema(schema),
			None if self.force_json => Json::Object,
			None => Json::Free,
		}
	}
}

pub mod config;
pub mod segment;
mod shortcuts;
mod transcribe;
pub mod tts;
pub mod video;
mod watch;
pub use shortcuts::*;
pub use transcribe::transcribe;
pub use watch::{Footage, Pick, Said, Shown, Watch, Watched};

/// Every remote model currently offered by every provider here caps out at the same place.
pub(crate) const MAX_TOKENS: usize = 128_000;
/// Fences are mentioned because the models otherwise wrap the object in them even when told to emit json only.
pub(crate) const FORCE_JSON_SUFFIX: &str = "\n\nRespond with valid JSON only, no other text or markdown fences.";
#[derive(Debug)]
#[non_exhaustive]
pub struct Response {
	pub text: String,
	/// What the tokens list for, in US cents; under a subscription nobody is billed it. `None`: we hold no price of the model's.
	pub cents: Option<f32>,
	#[deprecated(since = "3.13.0", note = "`cents`, which says when the price is unknown; this is NaN then")]
	pub cost_cents: f32,
	pub duration: std::time::Duration,
	/// Overhead before generation starts, where the wire tells it: model load for Ollama, time to first token for the Claude CLI.
	pub overhead: std::time::Duration,
	/// The deployment that answered.
	pub model: String,
	pub thinking: ThinkingLevel,
	pub usage: Usage,
	pub stop: Stop,
}
impl Response {
	#[allow(deprecated)] // the one place that fills `cost_cents`
	pub(crate) fn new(text: String, cents: Option<f32>, overhead: std::time::Duration, model: &str, thinking: ThinkingLevel, usage: Usage, stop: Stop) -> Self {
		Self {
			text,
			cents,
			cost_cents: cents.unwrap_or(f32::NAN), // summed or printed, it stays visibly unknown
			duration: std::time::Duration::ZERO,
			overhead,
			model: model.to_string(),
			thinking,
			usage,
			stop,
		}
	}

	/// The answer to a [`Client::json_schema`] or a [`Client::force_json`].
	pub fn json<T: serde::de::DeserializeOwned>(&self) -> eyre::Result<T> {
		serde_json::from_str(&self.text).map_err(|e| eyre::eyre!("{}, stopped for {:?}: the answer is not the json asked for: {e}\n{}", self.model, self.stop, self.text))
	}

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
	pub fn extract_codeblock(&self, extensions: Option<Vec<&str>>) -> eyre::Result<String> {
		let blocks = self.extract_codeblocks(extensions);
		if blocks.len() == 1 {
			Ok(blocks.into_iter().next().unwrap())
		} else {
			eyre::bail!("No codeblocks found or more than one codeblock found.");
		}
	}

	pub fn extract_html_tag(&self, tag_name: &str) -> eyre::Result<String> {
		let opening_tag = format!("<{tag_name}>");
		let closing_tag = format!("</{tag_name}>");
		let from_start = self.text.split_once(&opening_tag).unwrap().1;
		let extracted = from_start.split_once(&closing_tag).unwrap().0;
		Ok(extracted.to_string())
	}
}

#[derive(Clone, Debug, Default)]
pub struct Conversation(pub Vec<Message>);
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

#[derive(Clone, Debug)]
pub struct Message {
	pub(crate) role: Role,
	pub(crate) content: MessageContent,
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
impl MessageContent {
	pub(crate) fn parts(&self) -> Vec<ContentPart> {
		let image = |base64_data: &String, media_type: &String| ContentPart::Image {
			base64_data: base64_data.clone(),
			media_type: media_type.clone(),
		};
		match self {
			Self::Text(text) => vec![ContentPart::Text(text.clone())],
			Self::Image { base64_data, media_type } => vec![image(base64_data, media_type)],
			Self::TextAndImages { text, images } => std::iter::once(ContentPart::Text(text.clone()))
				.chain(images.iter().map(|i| image(&i.base64_data, &i.media_type)))
				.collect(),
			Self::Document { base64_data, media_type } => vec![ContentPart::Document {
				base64_data: base64_data.clone(),
				media_type: media_type.clone(),
			}],
			Self::Mixed { parts } => parts.clone(),
		}
	}
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
#[derive(Clone, Debug)]
pub struct FileAttachment {
	pub base64_data: String,
	pub media_type: String,
}
impl FileAttachment {
	/// PDFs as documents, images as images, anything else as its text.
	pub(crate) fn part(&self) -> std::result::Result<ContentPart, Unrecoverable> {
		use base64::Engine;

		let Self { base64_data, media_type } = self.clone();
		Ok(match media_type.as_str() {
			"application/pdf" => ContentPart::Document { base64_data, media_type },
			mt if mt.starts_with("image/") => ContentPart::Image { base64_data, media_type },
			mt => ContentPart::Text(
				base64::engine::general_purpose::STANDARD
					.decode(&base64_data)
					.ok()
					.and_then(|bytes| String::from_utf8(bytes).ok())
					.ok_or_else(|| Unrecoverable::new_unsupported("a binary attachment that is neither an image nor a PDF", format!("`{mt}` has no content block; convert it first")))?,
			),
		})
	}
}
/// Client for interacting with LLMs.
///
/// Default settings produce a simple oneshot call with Model::Medium.
#[derive(Clone)]
pub struct Client {
	config: config::AppConfig,
	path: model::Path,
	max_tokens: Option<usize>,
	stop_sequences: Option<Vec<String>>,
	force_json: bool,
	schema: Option<serde_json::Value>,
	files: Vec<FileAttachment>,
	thinking: ThinkingLevel,
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

/// What the answer is held to.
#[derive(Clone, Copy)]
pub(crate) enum Json<'a> {
	Free,
	Object,
	Schema(&'a serde_json::Value),
}

pub(crate) struct Request<'a> {
	pub conversation: &'a Conversation,
	/// Answered tool calls, after the conversation.
	pub rounds: &'a [std::sync::Arc<wire::Round>],
	pub tools: &'a [Tool],
	/// More rounds may follow on the same prefix.
	pub agentic: bool,
	pub max_tokens: Option<usize>,
	pub stop_sequences: Option<Vec<&'a str>>,
	pub json: Json<'a>,
	pub files: &'a [FileAttachment],
	pub thinking: ThinkingLevel,
}
impl Request<'_> {
	/// The conversation, with the files ahead of the first user turn and, where no schema is enforced on it, the wish for json at its end.
	pub fn turns(&self) -> std::result::Result<Vec<(Role, Vec<ContentPart>)>, Unrecoverable> {
		let mut turns: Vec<(Role, Vec<ContentPart>)> = self.conversation.0.iter().map(|m| (m.role, m.content.parts())).collect();
		let (Some(first), Some(_)) = (turns.iter().position(|(role, _)| matches!(role, Role::User)), turns.last()) else {
			return Err(Unrecoverable::new_unsupported("an empty conversation", "add at least one user turn".to_string()));
		};
		let files = self.files.iter().map(FileAttachment::part).collect::<std::result::Result<Vec<_>, _>>()?;
		turns[first].1.splice(..0, files);
		if let Json::Object = self.json {
			let last = &mut turns.last_mut().expect("the first user turn is among them").1;
			match last.last_mut() {
				Some(ContentPart::Text(text)) => text.push_str(FORCE_JSON_SUFFIX),
				_ => last.push(ContentPart::Text(FORCE_JSON_SUFFIX.to_string())),
			}
		}
		Ok(turns)
	}
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
			"[model: {} | thinking: {} | cost: {} | overhead: {overhead:.1}s | gen: {gen_secs:.1}s | {ms_per_char:.1}ms/char]",
			self.model,
			self.thinking,
			self.cents.map_or_else(|| "unknown".to_string(), |c| format!("{c:.4}¢"))
		)
	}
}
