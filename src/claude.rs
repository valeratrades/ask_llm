use serde::Deserialize;

use crate::{Backend, Cli, Conversation, Error, FORCE_JSON_SUFFIX, Request, Response, Result, Role, ThinkingLevel};

/// Everything under this backend is the `claude` CLI, not `api.anthropic.com`.
const BACKEND: &str = "the `claude` CLI";

pub(crate) struct Claude {
	/// `None` leaves credential resolution to the CLI, which reads its own keychain entry.
	pub oauth_token: Option<String>,
	pub model: ClaudeModel,
}
impl Claude {
	/// Shells out to the `claude` CLI instead of `POST /v1/messages`: the CLI bills the Max subscription,
	/// while `x-api-key` bills pay-as-you-go credits.
	/// docs: https://docs.claude.com/en/docs/claude-code/headless
	async fn do_conversation(&self, request: &Request<'_>) -> Result<Response> {
		if !request.files.is_empty() {
			return Err(Error::Unsupported {
				backend: BACKEND,
				what: "`files`",
				help: "drop the attachment, or pick a non-Claude `Model`".to_string(),
			});
		}
		if request.stop_sequences.is_some() {
			return Err(Error::Unsupported {
				backend: BACKEND,
				what: "`stop_sequences`",
				help: "the CLI has no equivalent flag; drop it, or pick a non-Claude `Model`".to_string(),
			});
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

		let mut cmd = tokio::process::Command::new("claude");
		cmd.arg("-p")
			.arg(&prompt)
			.args(["--model", self.model.to_str()])
			.args(["--output-format", "json"])
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
		tracing::debug!(model = self.model.to_str(), effort, prompt_len = prompt.len(), "invoking the claude cli");
		let output = cmd.output().await.map_err(|source| match source.kind() {
			std::io::ErrorKind::NotFound => Cli::NotInstalled { source },
			_ => Cli::Exit {
				status: "not started".to_string(),
				stderr: source.to_string(),
			},
		})?;
		if !output.status.success() {
			return Err(Cli::Exit {
				status: output.status.to_string(),
				stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
			}
			.into());
		}

		let envelope: CliResult = serde_json::from_slice(&output.stdout).map_err(|source| Error::Schema {
			provider: "claude",
			source,
			body: String::from_utf8_lossy(&output.stdout).into_owned(),
		})?;
		if envelope.is_error {
			return Err(Cli::Failed {
				subtype: envelope.subtype,
				message: envelope.result,
			}
			.into());
		}
		if envelope.result.trim().is_empty() {
			return Err(Cli::Empty {
				stop_reason: envelope.stop_reason.unwrap_or_else(|| "none".to_string()),
			}
			.into());
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
	fn conversation<'a>(&'a self, request: &'a Request<'a>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response>> + Send + 'a>> {
		Box::pin(self.do_conversation(request))
	}
}

/// The CLI takes one prompt string, so turns past the first are labelled by role and concatenated,
/// and a leading system message is lifted out into `--system-prompt`.
fn flatten(conversation: &Conversation) -> Result<(Option<String>, String)> {
	use crate::MessageContent;

	let unsupported = |what: &'static str, help: &str| Error::Unsupported {
		backend: BACKEND,
		what,
		help: help.to_string(),
	};

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

/// the `--output-format json` envelope
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
	Sonnet5,
	Opus5_5,
	Fable5_1,
}
impl ClaudeModel {
	fn to_str(&self) -> &str {
		match self {
			ClaudeModel::Sonnet5 => "claude-sonnet-5",
			ClaudeModel::Opus5_5 => "claude-opus-5-5",
			ClaudeModel::Fable5_1 => "claude-fable-5-1",
		}
	}
}
impl std::str::FromStr for ClaudeModel {
	type Err = eyre::Report;

	fn from_str(s: &str) -> eyre::Result<Self> {
		Ok(match s {
			_ if s.to_lowercase().contains("sonnet") => Self::Sonnet5,
			_ if s.to_lowercase().contains("opus") => Self::Opus5_5,
			_ if s.to_lowercase().contains("fable") => Self::Fable5_1,
			_ => eyre::bail!("Unknown model: {s}"),
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Message;

	#[test]
	fn deser_model() {
		let model = "claude-sonnet-5".parse::<ClaudeModel>().unwrap();
		assert_eq!(model, ClaudeModel::Sonnet5);
	}

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
