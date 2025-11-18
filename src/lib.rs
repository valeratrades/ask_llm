use eyre::{Result, bail};

//TODO: add reading conversation from json file or directory of json files

mod blocking;
mod claude;

pub async fn oneshot<T: AsRef<str>>(message: T, model: Model) -> Result<Response> {
	let mut conv = Conversation::new();
	conv.add(Role::User, message);
	conversation(&conv, model, None, None).await
}

//TODO!: determine whether streaming is in order based on the length of the input. Or just always streaem.
pub async fn conversation(conv: &Conversation, model: Model, max_tokens: Option<usize>, stop_sequences: Option<Vec<&str>>) -> Result<Response> {
	claude::ask_claude(conv, model, max_tokens, stop_sequences).await
}

#[derive(Clone, Copy, Debug, derive_more::FromStr)]
pub enum Model {
	Fast,
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
	role: Role,
	content: MessageContent,
}
impl Message {
	fn new<T: AsRef<str>>(role: Role, content: T) -> Self {
		Self {
			role,
			content: MessageContent::Text(content.as_ref().to_string()),
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

	pub fn new_with_system<T: AsRef<str>>(system_message: T) -> Self {
		Self(vec![Message::new(Role::System, system_message)])
	}

	pub fn add<T: AsRef<str>>(&mut self, role: Role, content: T) {
		self.0.push(Message::new(role, content));
	}

	pub fn add_exchange<T: AsRef<str>>(&mut self, user_message: T, assistant_message: T) {
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
			exts.sort_by_key(|b| std::cmp::Reverse(b.len())); // sort, with longer first (so "pytho" goes before "py")
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
							None // No matching extension found
						}
						_ => {
							// No extensions specified or empty vec, strip all language identifiers
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
		let from_start = self.text.split_once(&opening_tag).unwrap().1; //TODO: handle error
		let extracted = from_start.split_once(&closing_tag).unwrap().0; //TODO: handle error

		Ok(extracted.to_string())
	}
}
