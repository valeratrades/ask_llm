//! The device login's plain text goes through the `claude` CLI: it bills the subscription, and alone may refresh the login.
//! docs: https://docs.claude.com/en/docs/claude-code/headless
use std::path::Path;

use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncWriteExt as _;

use super::{Tokens, block};
use crate::{Cli, ContentPart, Conversation, FORCE_JSON_SUFFIX, Failure, Json, Request, Response, Role, Source, Stop, ThinkingLevel, Unrecoverable, wire::Answer};

/// Whether the CLI has a way to ask it.
pub(super) fn says(request: &Request<'_>) -> bool {
	!request.step && request.stop_sequences.is_none() && request.conversation.0.iter().all(|m| m.content.parts().iter().all(|p| matches!(p, ContentPart::Text(_))))
}

/// `account` is an [`Accounts`](super::accounts::Accounts) dir, which the CLI reads its login from.
pub(super) async fn call(account: &Path, source: &Source, request: &Request<'_>) -> Result<Answer, Failure> {
	// `max_tokens` is dropped rather than refused: it caps an answer instead of changing which answer is asked for.

	let (system, mut prompt) = flatten(request.conversation)?;
	let schema = match request.json {
		Json::Free => None,
		Json::Object => Some(json!({ "type": "object" })),
		Json::Schema(schema) => Some(schema.clone()),
	};
	if schema.is_some() {
		prompt.push_str(FORCE_JSON_SUFFIX);
	}

	let effort = match request.thinking {
		ThinkingLevel::None | ThinkingLevel::Low => "low",
		ThinkingLevel::Medium => "medium",
		ThinkingLevel::High => "high",
	};

	// stream-json is the only input the CLI takes content blocks in, so attachments ride along with the prompt
	let mut content = request.files.iter().map(|f| f.part().map(block)).collect::<Result<Vec<_>, _>>()?;
	content.push(json!({ "type": "text", "text": prompt }));
	let message = format!("{}\n", json!({ "type": "user", "message": { "role": "user", "content": content } }));

	let mut cmd = tokio::process::Command::new("claude");
	cmd.arg("-p")
		.args(["--input-format", "stream-json"])
		.args(["--output-format", "stream-json"])
		.arg("--verbose") // the CLI refuses stream-json output without it
		.args(["--model", source.model])
		.args(["--effort", effort])
		.arg("--safe-mode") // the caller's CLAUDE.md, hooks, plugins and MCP servers are not part of the question being asked
		.args(["--tools", ""]) // an answer, not an agent
		.arg("--no-session-persistence")
		// both are exported globally; an inherited one bills credits, which is what this backend exists to stop
		.env_remove("ANTHROPIC_API_KEY")
		.env_remove("CLAUDE_TOKEN");
	if let Some(schema) = schema {
		// the suffix alone lets prose through before the object; an object of untyped fields gets nested lists back json-encoded as strings
		cmd.args(["--json-schema", &schema.to_string()]);
	}
	// an inherited token would outrank the account
	cmd.env_remove("CLAUDE_CODE_OAUTH_TOKEN").env("CLAUDE_SECURESTORAGE_CONFIG_DIR", account);
	if let Some(system) = system {
		cmd.arg("--system-prompt").arg(system);
	}
	tracing::debug!(model = source.model, effort, prompt_len = prompt.len(), files = request.files.len(), "invoking the claude cli");
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

	let stop = match envelope.stop_reason.as_deref() {
		Some("max_tokens") => Stop::MaxTokens,
		_ => Stop::End,
	};
	Ok(Answer {
		response: Response::new(
			envelope.result,
			Some((envelope.total_cost_usd * 100.0) as f32),
			std::time::Duration::from_millis(envelope.ttft_ms),
			source.model,
			request.thinking,
			envelope.usage.into(),
			stop,
		),
		reason: envelope.stop_reason.unwrap_or_else(|| "none".to_string()),
		turn: None,
	})
}

/// The CLI takes one prompt string, so turns past the first are labelled by role and concatenated,
/// and a leading system message is lifted out into `--system-prompt`.
fn flatten(conversation: &Conversation) -> Result<(Option<String>, String), Unrecoverable> {
	let unsupported = |what: &'static str, help: &str| Unrecoverable::new_unsupported(what, help.to_string());

	let mut system = None;
	let mut turns: Vec<(Role, &str)> = Vec::new();
	for message in &conversation.0 {
		let crate::MessageContent::Text(text) = &message.content else {
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

/// the last line of `--output-format stream-json`, the `result` event
#[derive(Debug, Deserialize)]
struct CliResult {
	is_error: bool,
	subtype: String,
	#[serde(default)]
	result: String,
	#[serde(default)]
	stop_reason: Option<String>,
	/// notional: the CLI bills the subscription
	total_cost_usd: f64,
	usage: Tokens,
	/// time to first token as the CLI measures it, so process startup sits outside it
	#[serde(default)]
	ttft_ms: u64,
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
