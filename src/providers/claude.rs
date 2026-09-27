use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;

use crate::{Backend, Cli, Conversation, FORCE_JSON_SUFFIX, Failure, FileAttachment, Request, Response, Role, ThinkingLevel, Unrecoverable};

pub(crate) struct Claude {
	/// `None` leaves credential resolution to the CLI, which reads its own keychain entry.
	pub oauth_token: Option<String>,
	pub model: ClaudeModel,
}
impl Claude {
	/// Shells out to the `claude` CLI instead of `POST /v1/messages`: the CLI bills the Max subscription,
	/// while `x-api-key` bills pay-as-you-go credits.
	/// docs: https://docs.claude.com/en/docs/claude-code/headless
	async fn do_conversation(&self, request: &Request<'_>) -> Result<Response, Failure> {
		if request.stop_sequences.is_some() {
			return Err(Unrecoverable::new_unsupported("`stop_sequences`", "the CLI has no equivalent flag; drop it, or pick a non-Claude `Model`".to_string()).into());
		}
		// `max_tokens` is dropped rather than refused: it caps an answer instead of changing which answer is asked for.

		let (system, mut prompt) = flatten(request.conversation)?;
		if request.force_json {
			prompt.push_str(FORCE_JSON_SUFFIX);
		}

		let effort = match request.thinking {
			ThinkingLevel::None | ThinkingLevel::Low => "low",
			ThinkingLevel::Medium => "medium",
			ThinkingLevel::High => "high",
		};

		// stream-json is the only input the CLI takes content blocks in, so attachments ride along with the prompt
		let mut content = request.files.iter().map(file_to_content_block).collect::<Result<Vec<_>, _>>()?;
		content.push(json!({ "type": "text", "text": prompt }));
		let message = format!("{}\n", json!({ "type": "user", "message": { "role": "user", "content": content } }));

		let mut cmd = tokio::process::Command::new("claude");
		cmd.arg("-p")
			.args(["--input-format", "stream-json"])
			.args(["--output-format", "stream-json"])
			.arg("--verbose") // the CLI refuses stream-json output without it
			.args(["--model", self.model.to_str()])
			.args(["--effort", effort])
			.arg("--safe-mode") // the caller's CLAUDE.md, hooks, plugins and MCP servers are not part of the question being asked
			.args(["--tools", ""]) // an answer, not an agent
			.arg("--no-session-persistence")
			// both are exported globally; an inherited one bills credits, which is what this backend exists to stop
			.env_remove("ANTHROPIC_API_KEY")
			.env_remove("CLAUDE_TOKEN");
		if let Some(token) = &self.oauth_token {
			cmd.env("CLAUDE_CODE_OAUTH_TOKEN", token);
		}
		if let Some(system) = system {
			cmd.arg("--system-prompt").arg(system);
		}
		tracing::debug!(
			model = self.model.to_str(),
			effort,
			prompt_len = prompt.len(),
			files = request.files.len(),
			"invoking the claude cli"
		);
		let mut child = cmd
			.stdin(std::process::Stdio::piped())
			.stdout(std::process::Stdio::piped())
			.stderr(std::process::Stdio::piped())
			.spawn()
			.map_err(|source| match source.kind() {
				std::io::ErrorKind::NotFound => Cli::new_not_installed(source),
				_ => Cli::new_exit("not started".to_string(), source.to_string()),
			})?;
		let mut stdin = child.stdin.take().expect("piped above");
		// written alongside the wait, so a child filling its stdout pipe cannot stall the write
		let (written, output) = tokio::join!(
			async {
				stdin.write_all(message.as_bytes()).await?;
				drop(stdin);
				Ok::<_, std::io::Error>(())
			},
			child.wait_with_output()
		);
		let output = output.map_err(|source| Cli::new_exit("not awaited".to_string(), source.to_string()))?;
		if !output.status.success() {
			return Err(Cli::new_exit(output.status.to_string(), String::from_utf8_lossy(&output.stderr).into_owned()).into());
		}

		written.map_err(|source| Cli::new_exit(output.status.to_string(), format!("writing the message to its stdin: {source}")))?;

		let stdout = String::from_utf8_lossy(&output.stdout);
		let Some(last) = stdout.lines().last() else {
			return Err(Cli::new_exit(output.status.to_string(), "exited cleanly having printed nothing".to_string()).into());
		};
		let envelope: CliResult = serde_json::from_str(last).map_err(|source| Unrecoverable::new_schema(source, stdout.clone().into_owned()))?;
		if envelope.is_error {
			return Err(Cli::new_failed(envelope.subtype, envelope.result).into());
		}
		if envelope.result.trim().is_empty() {
			return Err(Cli::new_empty(envelope.stop_reason.unwrap_or_else(|| "none".to_string())).into());
		}

		Ok(Response {
			text: envelope.result,
			cost_cents: (envelope.total_cost_usd * 100.0) as f32,
			duration: std::time::Duration::ZERO,
			overhead: std::time::Duration::from_millis(envelope.ttft_ms),
			model: self.model.to_str().to_string(),
			thinking: request.thinking,
		})
	}
}

impl Backend for Claude {
	fn conversation<'a>(&'a self, request: &'a Request<'a>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, Failure>> + Send + 'a>> {
		Box::pin(self.do_conversation(request))
	}
}

/// The CLI takes one prompt string, so turns past the first are labelled by role and concatenated,
/// and a leading system message is lifted out into `--system-prompt`.
fn flatten(conversation: &Conversation) -> Result<(Option<String>, String), Unrecoverable> {
	use crate::MessageContent;

	let unsupported = |what: &'static str, help: &str| Unrecoverable::new_unsupported(what, help.to_string());

	let mut system = None;
	let mut turns: Vec<(Role, &str)> = Vec::new();
	for message in &conversation.0 {
		let MessageContent::Text(text) = &message.content else {
			return Err(unsupported("images and documents", "pick a non-Claude `Model`"));
		};
		match message.role {
			Role::System if system.is_none() && turns.is_empty() => system = Some(text.clone()),
			Role::System => return Err(unsupported("a system message past the first turn", "only a leading one maps onto `--system-prompt`")),
			role => turns.push((role, text)),
		}
	}

	let prompt = match turns.as_slice() {
		[] => return Err(unsupported("an empty conversation", "add at least one user turn")),
		[(Role::User, only)] => (*only).to_string(),
		many => many.iter().map(|(role, text)| format!("{}: {text}", <&str>::from(*role))).collect::<Vec<_>>().join("\n\n"),
	};
	Ok((system, prompt))
}

/// A request's file as a Messages API content block: PDFs as documents, images as images, anything else as its text.
fn file_to_content_block(file: &FileAttachment) -> Result<Value, Unrecoverable> {
	use base64::Engine;
	let source = json!({ "type": "base64", "media_type": file.media_type, "data": file.base64_data });
	Ok(match file.media_type.as_str() {
		"application/pdf" => json!({ "type": "document", "source": source }),
		mt if mt.starts_with("image/") => json!({ "type": "image", "source": source }),
		mt => {
			let text = base64::engine::general_purpose::STANDARD
				.decode(&file.base64_data)
				.ok()
				.and_then(|bytes| String::from_utf8(bytes).ok())
				.ok_or_else(|| Unrecoverable::new_unsupported("a binary attachment that is neither an image nor a PDF", format!("`{mt}` has no content block; convert it first")))?;
			json!({ "type": "text", "text": text })
		}
	})
}

/// the last line of `--output-format stream-json`, the `result` event
#[derive(Debug, Deserialize)]
struct CliResult {
	is_error: bool,
	subtype: String,
	#[serde(default)]
	result: String,
	#[serde(default)]
	stop_reason: Option<String>,
	total_cost_usd: f64,
	/// time to first token as the CLI measures it, so process startup sits outside it
	#[serde(default)]
	ttft_ms: u64,
}

#[derive(Debug, Eq, PartialEq)]
/// ref: https://docs.claude.com/en/docs/about-claude/models/all-models
pub(crate) enum ClaudeModel {
	Opus5_5,
	Fable5_1,
}
impl ClaudeModel {
	pub const fn to_str(&self) -> &'static str {
		match self {
			ClaudeModel::Opus5_5 => "claude-opus-5-5",
			ClaudeModel::Fable5_1 => "claude-fable-5-1",
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Message;

	#[test]
	fn flatten_conversation() {
		let mut conv = Conversation::new_with_system("be terse");
		conv.add(Role::User, "hi");
		assert_eq!(flatten(&conv).unwrap(), (Some("be terse".to_string()), "hi".to_string()));

		conv.add(Role::Assistant, "hello");
		conv.add(Role::User, "bye");
		assert_eq!(flatten(&conv).unwrap().1, "user: hi\n\nassistant: hello\n\nuser: bye");

		let mut trailing_system = Conversation::new();
		trailing_system.0.push(Message::new(Role::User, "hi"));
		trailing_system.0.push(Message::new(Role::System, "too late"));
		assert!(flatten(&trailing_system).is_err());
	}
}
