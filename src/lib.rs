use eyre::{Result, bail};

mod claude;

/// Client for interacting with LLMs.
///
/// Default settings produce a simple oneshot call with Model::Medium.
#[derive(Clone, Debug)]
pub struct Client {
	pub config: config::AppConfig,
	pub model: Model,
	pub temperature: Option<f32>,
	pub max_tokens: Option<usize>,
	pub stop_sequences: Option<Vec<String>>,
	pub force_json: bool,
	pub files: Vec<FileAttachment>,
}
impl Client {
	/// Create a new client using default config (reads from environment).
	pub fn new() -> Self {
		Self::with_config(config::AppConfig::default())
	}

	/// Create a new client with explicit config.
	pub fn with_config(config: config::AppConfig) -> Self {
		Self {
			config,
			model: Model::default(),
			temperature: None,
			max_tokens: None,
			stop_sequences: None,
			force_json: false,
			files: Vec::new(),
		}
	}

	pub fn model(mut self, model: Model) -> Self {
		self.model = model;
		self
	}

	pub fn temperature(mut self, temperature: f32) -> Self {
		self.temperature = Some(temperature);
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

	/// Append a file to be included with the request.
	/// Supported media types: application/pdf, text/plain, text/markdown, text/csv,
	/// application/vnd.openxmlformats-officedocument.wordprocessingml.document (docx),
	/// application/vnd.openxmlformats-officedocument.spreadsheetml.sheet (xlsx)
	pub fn append_file(mut self, base64_data: String, media_type: String) -> Self {
		self.files.push(FileAttachment { base64_data, media_type });
		self
	}

	/// Append a file from a filesystem path.
	pub fn append_file_from_path(self, path: impl AsRef<std::path::Path>) -> Result<Self> {
		let path = path.as_ref();
		let data = std::fs::read(path)?;
		let base64_data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);
		let media_type = mime_type_from_extension(path.extension().and_then(|s| s.to_str()).unwrap_or(""));
		Ok(self.append_file(base64_data, media_type.to_string()))
	}

	pub async fn ask(&self, message: impl Into<String>) -> Result<Response> {
		let mut conv = Conversation::new();
		conv.add(Role::User, message.into());
		let stop_seqs: Option<Vec<&str>> = self.stop_sequences.as_ref().map(|v| v.iter().map(|s| s.as_str()).collect());
		claude::ask_claude(&self.config, &conv, self.model, self.temperature, self.max_tokens, stop_seqs, self.force_json, &self.files).await
	}

	pub async fn conversation(&self, conv: &Conversation) -> Result<Response> {
		let stop_seqs: Option<Vec<&str>> = self.stop_sequences.as_ref().map(|v| v.iter().map(|s| s.as_str()).collect());
		claude::ask_claude(&self.config, conv, self.model, self.temperature, self.max_tokens, stop_seqs, self.force_json, &self.files).await
	}
}

#[derive(Clone, Debug)]
pub struct FileAttachment {
	pub base64_data: String,
	pub media_type: String,
}
#[derive(Clone, Copy, Debug, Default, derive_more::FromStr)]
pub enum Model {
	Fast,
	#[default]
	Medium,
	Slow,
}
#[derive(Clone, Copy, Debug)]
pub enum Role {
	System,
	User,
	Assistant,
}
#[derive(Clone, Debug)]
pub enum MessageContent {
	Text(String),
	Image { base64_data: String, media_type: String },
	TextAndImages { text: String, images: Vec<ImageContent> },
	Document { base64_data: String, media_type: String },
	Mixed { parts: Vec<ContentPart> },
}
#[derive(Clone, Debug)]
pub enum ContentPart {
	Text(String),
	Image { base64_data: String, media_type: String },
	Document { base64_data: String, media_type: String },
}
#[derive(Clone, Debug)]
pub struct ImageContent {
	pub base64_data: String,
	pub media_type: String,
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

#[derive(Debug, derive_new::new)]
pub struct Response {
	pub text: String,
	pub cost_cents: f32,
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
pub mod config;
mod shortcuts;
pub use shortcuts::*;

impl Default for Client {
	fn default() -> Self {
		Self::new()
	}
}

impl std::fmt::Display for Response {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "Response: {}\nCost (cents): {}", self.text, self.cost_cents)
	}
}
