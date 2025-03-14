use eyre::{Result, bail};
use serde::Serialize;

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

#[derive(Clone, Debug, Copy)]
pub enum Model {
	Fast,
	Medium,
	Slow,
}
#[derive(Clone, Debug, Copy)]
pub enum Role {
	System,
	User,
	Assistant,
}
#[derive(Debug, Clone)]
pub struct Message {
	role: Role,
	content: String,
}
impl Message {
	fn new<T: AsRef<str>>(role: Role, content: T) -> Self {
		Self {
			role,
			content: content.as_ref().to_string(),
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
	pub fn extract_codeblocks(&self, extension: &str) -> Result<Vec<String>> {
		let extracted: Vec<String> = self
			.text
			.split("```")
			.enumerate()
			.filter_map(|(i, s)| {
				if i % 2 == 1 /*When we don't have an extension to match on, this is the only way to get separate text inside and outside codeblock delimiters*/ && s.starts_with(extension) {
					Some(s.strip_prefix(extension).unwrap().trim().to_string())
				} else {
					None
				}
			})
			.collect();
		match extracted.is_empty() {
			true => bail!("Failed to find any {extension} codeblocks in the response:\nResponse: {}", self.text),
			false => Ok(extracted),
		}
	}

	pub fn extract_codeblock(&self, extension: &str) -> Result<String> {
		let extracted = self.extract_codeblocks(extension)?; // because performance does not matter. Could use `find` here over `filter` there, but ehh
		Ok(extracted[0].clone())
	}

	pub fn extract_html_tag(&self, tag_name: &str) -> Result<String> {
		let opening_tag = format!("<{}>", tag_name);
		let closing_tag = format!("</{}>", tag_name);
		let from_start = self.text.split_once(&opening_tag).unwrap().1; //TODO: handle error
		let extracted = from_start.split_once(&closing_tag).unwrap().0; //TODO: handle error

		Ok(extracted.to_string())
	}
}

trait LlmConversation: Serialize {
	fn new(conversation: &Conversation) -> Self;
}
