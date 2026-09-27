//! The failure taxonomy a consumer is expected to match on.
//!
//! Grouped by what a caller can do about it, not by where it came from: [`Error::Recoverable`] means a
//! later call can answer, [`Error::Unrecoverable`] means none will until something outside changes.
//! Every variant keeps the provider's own words and names the fix in its `help`.
use eyre::Report;
use v_utils::macros::wrap_err;

use crate::Provider;

pub type Result<T> = std::result::Result<T, Error>;

#[non_exhaustive]
#[derive(Debug, miette::Diagnostic, thiserror::Error)]
pub enum Error {
	/// The path ran out, and at least one node on it failed on something that clears on its own.
	#[error(transparent)]
	#[diagnostic(transparent)]
	Recoverable(Exhausted),
	/// Every node on the path refused for good.
	#[error(transparent)]
	#[diagnostic(transparent)]
	Unrecoverable(Exhausted),
	/// Local tooling (ffmpeg, whisper, the filesystem); never reached a model.
	#[error(transparent)]
	Other(#[from] Report),
}

#[derive(Debug, miette::Diagnostic, thiserror::Error)]
#[error("no model on the path answered ({} tried)", attempts.len())]
pub struct Exhausted {
	#[related]
	pub attempts: Vec<Attempt>,
}

#[derive(Debug, thiserror::Error)]
#[error("{provider} `{model}`")]
pub struct Attempt {
	pub provider: Provider,
	pub model: &'static str,
	#[source]
	pub failure: Failure,
}
impl miette::Diagnostic for Attempt {
	fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
		self.failure.code()
	}

	fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
		self.failure.help()
	}
}

/// What one node of the graph returns.
#[wrap_err]
#[derive(Debug, miette::Diagnostic, thiserror::Error)]
pub enum Failure {
	#[own]
	#[diagnostic(transparent)]
	Recoverable(Recoverable),
	#[own]
	#[diagnostic(transparent)]
	Unrecoverable(Unrecoverable),
}

impl Failure {
	/// `retry_after` is the header verbatim, in seconds; providers also send an HTTP-date form, which is dropped.
	///
	/// Exercised from `examples/upstream_errors.rs` — the mapping is a table against live providers, and a
	/// fixture body is the only thing that catches it rotting.
	pub fn classify(provider: Provider, status: u16, retry_after: Option<&str>, body: &str) -> Self {
		let envelope = Envelope::parse(body);
		let message = envelope.message.unwrap_or_else(|| truncate(body));
		let code = envelope.code.unwrap_or_default();
		// a locally served model is missing from disk, not retired upstream
		let model_help = match provider {
			Provider::Ollama => "the model is not on this machine; `ollama pull` it".to_string(),
			_ => "the node's pinned model was retired; bump ask_llm".to_string(),
		};
		match status {
			401 | 403 => Unrecoverable::new_auth(message).into(),
			402 => Unrecoverable::new_quota(message).into(),
			404 => Unrecoverable::new_model_unavailable(message, model_help).into(),
			413 => Unrecoverable::new_context_length(message).into(),
			429 if code.contains("quota") || code.contains("billing") => Unrecoverable::new_quota(message).into(),
			429 => Recoverable::new_rate_limited(message, retry_after.and_then(|s| s.trim().parse().ok()).map(std::time::Duration::from_secs)).into(),
			451 => Unrecoverable::new_geo_blocked(message).into(),
			500 | 502 | 503 | 529 => Recoverable::new_overloaded(message).into(),
			_ if code.contains("context_length") || code.contains("too_large") => Unrecoverable::new_context_length(message).into(),
			_ if code.contains("model_not_found") || code.contains("model_not_available") => Unrecoverable::new_model_unavailable(message, model_help).into(),
			status => Unrecoverable::new_other(status, truncate(body)).into(),
		}
	}
}
impl From<Transport> for Failure {
	fn from(e: Transport) -> Self {
		Recoverable::from(e).into()
	}
}
impl From<Cli> for Failure {
	fn from(e: Cli) -> Self {
		Unrecoverable::from(e).into()
	}
}

/// Clears on its own: asking again later can answer.
#[non_exhaustive]
#[wrap_err]
#[derive(Debug, miette::Diagnostic, thiserror::Error)]
pub enum Recoverable {
	#[own]
	#[diagnostic(transparent)]
	Transport(Transport),
	#[leaf]
	#[error("rate-limiting this key: {message}")]
	#[diagnostic(code(ask_llm::api::rate_limited), help("back off and retry; `retry_after` carries the provider's own wait when it sent one"))]
	RateLimited { message: String, retry_after: Option<std::time::Duration> },
	#[leaf]
	#[error("overloaded: {message}")]
	#[diagnostic(code(ask_llm::api::overloaded), help("the provider is failing on its own side; retry on a backoff"))]
	Overloaded { message: String },
}

/// Stays failed until something outside the call changes: a key, a balance, the request itself.
#[non_exhaustive]
#[wrap_err]
#[derive(Debug, miette::Diagnostic, thiserror::Error)]
pub enum Unrecoverable {
	#[leaf]
	#[error("no api key configured")]
	#[diagnostic(code(ask_llm::missing_token))]
	MissingToken {
		#[help]
		help: String,
	},
	#[leaf]
	#[error("rejected the credentials: {message}")]
	#[diagnostic(code(ask_llm::api::auth), help("rotate the key, or check `OPENAI_API_KEY` / `openai_token` in ~/.config/ask_llm.nix"))]
	Auth { message: String },
	#[leaf]
	#[error("the account is out of credit: {message}")]
	#[diagnostic(code(ask_llm::api::quota), help("the key is valid but the account is out of credit"))]
	Quota { message: String },
	#[leaf]
	#[error("does not serve this region: {message}")]
	#[diagnostic(code(ask_llm::api::geo_blocked), help("the request's exit IP is in a region the provider refuses"))]
	GeoBlocked { message: String },
	#[leaf]
	#[error("does not serve the requested model: {message}")]
	#[diagnostic(code(ask_llm::api::model_unavailable))]
	ModelUnavailable {
		message: String,
		#[help]
		help: String,
	},
	#[leaf]
	#[error("the conversation is longer than accepted: {message}")]
	#[diagnostic(code(ask_llm::api::context_length), help("drop turns or attachments, or pick a `Model` with a larger window"))]
	ContextLength { message: String },
	/// The request asks for something this node has no way to express.
	#[leaf]
	#[error("cannot serve this request: {what}")]
	#[diagnostic(code(ask_llm::unsupported))]
	Unsupported {
		what: &'static str,
		#[help]
		help: String,
	},
	/// The provider generated nothing and said why.
	#[leaf]
	#[error("refused to answer: {reason}")]
	#[diagnostic(code(ask_llm::refused), help("the request tripped the provider's content policy; rephrase it"))]
	Refused { reason: String },
	/// The provider answered with a shape its own docs do not describe.
	#[leaf]
	#[error("failed to parse the response: {source}\n{body}")]
	#[diagnostic(code(ask_llm::schema), help("the provider changed its response shape; bump ask_llm"))]
	Schema { source: serde_json::Error, body: String },
	#[own]
	#[diagnostic(transparent)]
	Cli(Cli),
	#[leaf]
	#[error("rejected the request ({status}): {body}")]
	#[diagnostic(code(ask_llm::api::other))]
	Other { status: u16, body: String },
}

/// No HTTP response arrived, so the provider never got a chance to have an opinion.
#[non_exhaustive]
#[wrap_err]
#[derive(Debug, miette::Diagnostic, thiserror::Error)]
pub enum Transport {
	#[leaf]
	#[error("could not reach it: {source}")]
	#[diagnostic(code(ask_llm::transport::unreachable))]
	Unreachable {
		source: reqwest::Error,
		#[help]
		help: String,
	},
	#[leaf]
	#[error("did not answer in time: {source}")]
	#[diagnostic(code(ask_llm::transport::timeout), help("the connection was accepted but nothing came back; the provider may be saturated"))]
	Timeout { source: reqwest::Error },
	#[leaf]
	#[error("the request failed in transit: {source}")]
	#[diagnostic(code(ask_llm::transport::send), help("the connection dropped mid-request; check the network and any proxy in front of it"))]
	Send { source: reqwest::Error },
}

impl Transport {
	pub(crate) fn classify(provider: Provider, source: reqwest::Error) -> Self {
		match () {
			// Ollama is the only provider here that runs on the same machine, so its "offline" is a different fix.
			_ if source.is_connect() => Self::new_unreachable(
				source,
				match provider {
					Provider::Ollama => "nothing is listening on the ollama port; start it with `ollama serve`".to_string(),
					_ => "the machine may be offline; the request never left".to_string(),
				},
			),
			_ if source.is_timeout() => Self::new_timeout(source),
			_ => Self::new_send(source),
		}
	}
}

/// Claude is reached by shelling out, so its failures are a process's, not a socket's.
#[non_exhaustive]
#[wrap_err]
#[derive(Debug, miette::Diagnostic, thiserror::Error)]
pub enum Cli {
	#[leaf]
	#[error("the `claude` binary could not be run: {source}")]
	#[diagnostic(code(ask_llm::cli::not_installed), help("`claude` is not on PATH; every Claude node routes through it"))]
	NotInstalled { source: std::io::Error },
	#[leaf]
	#[error("`claude` exited with {status}: {stderr}")]
	#[diagnostic(code(ask_llm::cli::exit), help("run the same prompt through `claude -p` by hand to see the CLI's own diagnosis"))]
	Exit { status: String, stderr: String },
	#[leaf]
	#[error("`claude` reported a failure ({subtype}): {message}")]
	#[diagnostic(code(ask_llm::cli::failed), help("the CLI reached Anthropic and was refused; check `claude /status` for the subscription"))]
	Failed { subtype: String, message: String },
	/// Thinking is billed against the output budget and can consume all of it, which would otherwise read
	/// as "the model had nothing to say" (see CHANGELOG v3.0.1).
	#[leaf]
	#[error("`claude` returned no text (stop_reason: {stop_reason})")]
	#[diagnostic(code(ask_llm::cli::empty), help("thinking ate the whole output budget; lower `ThinkingLevel` or raise `max_tokens`"))]
	Empty { stop_reason: String },
}

/// OpenAI sends `{"error":{"message","type","code"}}`; Ollama sends `{"error":"<string>"}`. Accepting only the
/// first would land every local failure in [`Unrecoverable::Other`].
#[derive(Default, serde::Deserialize)]
#[serde(from = "RawEnvelope")]
struct Envelope {
	message: Option<String>,
	code: Option<String>,
}
impl Envelope {
	fn parse(body: &str) -> Self {
		serde_json::from_str(body).unwrap_or_default() // a provider may answer with html from a CDN, which is what `Unrecoverable::Other` keeps the raw body for
	}
}

#[derive(serde::Deserialize)]
struct RawEnvelope {
	error: Option<RawError>,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum RawError {
	Plain(String),
	Structured {
		#[serde(default)]
		message: Option<String>,
		#[serde(default)]
		r#type: Option<String>,
		#[serde(default)]
		code: Option<String>,
	},
}

impl From<RawEnvelope> for Envelope {
	fn from(raw: RawEnvelope) -> Self {
		match raw.error {
			Some(RawError::Plain(message)) => Self { message: Some(message), code: None },
			// either one can be the only one naming the failure: a spent balance is `type: insufficient_quota`, `code: credit_balance_exhausted`
			Some(RawError::Structured { message, r#type, code }) => Self {
				message,
				code: [code, r#type].into_iter().flatten().reduce(|a, b| format!("{a} {b}")),
			},
			None => Self::default(),
		}
	}
}

/// A rejection body is occasionally a whole html page.
fn truncate(body: &str) -> String {
	const LIMIT: usize = 2048;
	match body.char_indices().nth(LIMIT) {
		Some((cut, _)) => format!("{}… ({} bytes truncated)", &body[..cut], body.len() - cut),
		None => body.to_string(),
	}
}
