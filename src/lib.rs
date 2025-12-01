use eyre::{Result, bail};

mod claude;
pub mod config;
mod shortcuts;
pub use shortcuts::*;

/// Client for interacting with LLMs.
///
/// Default settings produce a simple oneshot call with Model::Medium.
#[derive(Clone, Debug, Default)]
pub struct Client {
	pub model: Model,
	pub temperature: Option<f32>,
	pub max_tokens: Option<usize>,
	pub stop_sequences: Option<Vec<String>>,
}

impl Client {
	pub fn new() -> Self {
		Self::default()
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

	pub async fn ask(&self, message: impl Into<String>) -> Result<Response> {
		let mut conv = Conversation::new();
		conv.add(Role::User, message.into());
		let stop_seqs: Option<Vec<&str>> = self.stop_sequences.as_ref().map(|v| v.iter().map(|s| s.as_str()).collect());
		claude::ask_claude(&conv, self.model, self.temperature, self.max_tokens, stop_seqs).await
	}
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

impl std::fmt::Display for Response {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "Response: {}\nCost (cents): {}", self.text, self.cost_cents)
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
		let opening_tag = format!("<{}>", tag_name);
		let closing_tag = format!("</{}>", tag_name);
		let from_start = self.text.split_once(&opening_tag).unwrap().1;
		let extracted = from_start.split_once(&closing_tag).unwrap().0;
		Ok(extracted.to_string())
	}
}
