//! The failure taxonomy a consumer is expected to match on.
//!
//! Every variant keeps the provider's own words and names the fix in its `help`, so a caller can
//! tell "the machine is offline" from "the key was revoked" from "the pinned model was retired"
//! without reading a message string.
use eyre::Report;

pub type Result<T> = std::result::Result<T, Error>;

#[non_exhaustive]
#[derive(Debug, miette::Diagnostic, thiserror::Error)]
pub enum Error {
	#[error(transparent)]
	#[diagnostic(transparent)]
	MissingToken(#[from] MissingToken),
	#[error(transparent)]
	#[diagnostic(transparent)]
	Transport(#[from] Transport),
	#[error(transparent)]
	#[diagnostic(transparent)]
	Api(#[from] Api),
	#[error(transparent)]
	#[diagnostic(transparent)]
	Cli(#[from] Cli),
	/// The request asks for something the selected backend has no way to express.
	#[error("{backend} cannot serve this request: {what}")]
	#[diagnostic(code(ask_llm::unsupported))]
	Unsupported {
		backend: &'static str,
		what: &'static str,
		#[help]
		help: String,
	},
	/// The provider answered with a shape its own docs do not describe.
	#[error("failed to parse the {provider} response: {source}\n{body}")]
	#[diagnostic(code(ask_llm::schema), help("the provider changed its response shape; bump ask_llm"))]
	Schema {
		provider: &'static str,
		#[source]
		source: serde_json::Error,
		body: String,
	},
	/// The provider generated nothing and said why.
	#[error("{provider} refused to answer: {reason}")]
	#[diagnostic(code(ask_llm::refused), help("the request tripped the provider's content policy; rephrase it or pick another `Model`"))]
	Refused { provider: &'static str, reason: String },
	#[error(transparent)]
	Other(#[from] Report),
}

/// A model was selected whose provider has no key in the config, the builder, or the environment.
#[derive(Debug, miette::Diagnostic, thiserror::Error)]
#[error("`{model}` is served by {provider}, which has no api key configured")]
#[diagnostic(code(ask_llm::missing_token))]
pub struct MissingToken {
	pub provider: &'static str,
	pub model: &'static str,
	#[help]
	pub help: String,
}

impl MissingToken {
	pub(crate) fn new(provider: &'static str, model: &'static str, config_key: &'static str, env_var: &'static str, builder: &'static str) -> Self {
		Self {
			provider,
			model,
			help: format!("hand it over with `Client::{builder}(…)`, put `{config_key}` in ~/.config/ask_llm.nix, or export {env_var}"),
		}
	}
}

/// No HTTP response arrived, so the provider never got a chance to have an opinion.
#[non_exhaustive]
#[derive(Debug, miette::Diagnostic, thiserror::Error)]
pub enum Transport {
	#[error("could not reach {provider}: {source}")]
	#[diagnostic(code(ask_llm::transport::unreachable))]
	Unreachable {
		provider: &'static str,
		#[source]
		source: reqwest::Error,
		#[help]
		help: String,
	},
	#[error("{provider} did not answer in time: {source}")]
	#[diagnostic(code(ask_llm::transport::timeout), help("the connection was accepted but nothing came back; the provider may be saturated"))]
	Timeout {
		provider: &'static str,
		#[source]
		source: reqwest::Error,
	},
	#[error("the {provider} request failed in transit: {source}")]
	#[diagnostic(code(ask_llm::transport::send), help("the connection dropped mid-request; check the network and any proxy in front of it"))]
	Send {
		provider: &'static str,
		#[source]
		source: reqwest::Error,
	},
}

impl Transport {
	pub(crate) fn classify(provider: &'static str, source: reqwest::Error) -> Self {
		match () {
			// Ollama is the only provider here that runs on the same machine, so its "offline" is a different fix.
			_ if source.is_connect() => Self::Unreachable {
				provider,
				help: match provider {
					"Ollama" => "nothing is listening on the ollama port; start it with `ollama serve`".to_string(),
					_ => "the machine may be offline; the request never left".to_string(),
				},
				source,
			},
			_ if source.is_timeout() => Self::Timeout { provider, source },
			_ => Self::Send { provider, source },
		}
	}
}

/// The provider answered, and refused.
#[non_exhaustive]
#[derive(Debug, miette::Diagnostic, thiserror::Error)]
pub enum Api {
	#[error("{provider} rejected the credentials: {message}")]
	#[diagnostic(code(ask_llm::api::auth), help("rotate the key, or check `OPENAI_API_KEY` / `openai_token` in ~/.config/ask_llm.nix"))]
	Auth { provider: &'static str, message: String },
	#[error("the {provider} account is out of credit: {message}")]
	#[diagnostic(code(ask_llm::api::quota), help("the key is valid but the account is out of credit"))]
	Quota { provider: &'static str, message: String },
	#[error("{provider} does not serve the requested model: {message}")]
	#[diagnostic(code(ask_llm::api::model_unavailable))]
	ModelUnavailable {
		provider: &'static str,
		message: String,
		#[help]
		help: String,
	},
	#[error("{provider} is rate-limiting this key: {message}")]
	#[diagnostic(code(ask_llm::api::rate_limited), help("back off and retry; `retry_after` carries the provider's own wait when it sent one"))]
	RateLimited {
		provider: &'static str,
		message: String,
		retry_after: Option<std::time::Duration>,
	},
	#[error("the conversation is longer than {provider} accepts: {message}")]
	#[diagnostic(code(ask_llm::api::context_length), help("drop turns or attachments, or pick a `Model` with a larger window"))]
	ContextLength { provider: &'static str, message: String },
	#[error("{provider} does not serve this region: {message}")]
	#[diagnostic(code(ask_llm::api::geo_blocked), help("the request's exit IP is in a region the provider refuses"))]
	GeoBlocked { provider: &'static str, message: String },
	#[error("{provider} is overloaded: {message}")]
	#[diagnostic(code(ask_llm::api::overloaded), help("the provider is failing on its own side; retry on a backoff"))]
	Overloaded { provider: &'static str, message: String },
	#[error("{provider} rejected the request ({status}): {body}")]
	#[diagnostic(code(ask_llm::api::other))]
	Other { provider: &'static str, status: u16, body: String },
}

impl Api {
	/// `retry_after` is the header verbatim, in seconds; providers also send an HTTP-date form, which is dropped.
	///
	/// Exercised from `examples/upstream_errors.rs` — the mapping is a table against live providers, and a
	/// fixture body is the only thing that catches it rotting.
	pub fn classify(provider: &'static str, status: u16, retry_after: Option<&str>, body: &str) -> Self {
		let envelope = Envelope::parse(body);
		let message = envelope.message.unwrap_or_else(|| truncate(body));
		let code = envelope.code.unwrap_or_default();
		// a locally served model is missing from disk, not retired upstream
		let model_help = match provider {
			"Ollama" => "the model is not on this machine; `ollama pull` it".to_string(),
			_ => "the tier's pinned model was retired; bump ask_llm or pick another `Model`".to_string(),
		};
		match status {
			401 | 403 => Self::Auth { provider, message },
			402 => Self::Quota { provider, message },
			404 => Self::ModelUnavailable {
				provider,
				message,
				help: model_help,
			},
			413 => Self::ContextLength { provider, message },
			429 if code.contains("quota") || code.contains("billing") => Self::Quota { provider, message },
			429 => Self::RateLimited {
				provider,
				message,
				retry_after: retry_after.and_then(|s| s.trim().parse().ok()).map(std::time::Duration::from_secs),
			},
			451 => Self::GeoBlocked { provider, message },
			500 | 502 | 503 | 529 => Self::Overloaded { provider, message },
			_ if code.contains("context_length") || code.contains("too_large") => Self::ContextLength { provider, message },
			_ if code.contains("model_not_found") || code.contains("model_not_available") => Self::ModelUnavailable {
				provider,
				message,
				help: model_help,
			},
			status => Self::Other {
				provider,
				status,
				body: truncate(body),
			},
		}
	}
}

/// Claude is reached by shelling out, so its failures are a process's, not a socket's.
#[non_exhaustive]
#[derive(Debug, miette::Diagnostic, thiserror::Error)]
pub enum Cli {
	#[error("the `claude` binary could not be run: {source}")]
	#[diagnostic(code(ask_llm::cli::not_installed), help("`claude` is not on PATH; every Claude tier routes through it"))]
	NotInstalled {
		#[source]
		source: std::io::Error,
	},
	#[error("`claude` exited with {status}: {stderr}")]
	#[diagnostic(code(ask_llm::cli::exit), help("run the same prompt through `claude -p` by hand to see the CLI's own diagnosis"))]
	Exit { status: String, stderr: String },
	#[error("`claude` reported a failure ({subtype}): {message}")]
	#[diagnostic(code(ask_llm::cli::failed), help("the CLI reached Anthropic and was refused; check `claude /status` for the subscription"))]
	Failed { subtype: String, message: String },
	/// Thinking is billed against the output budget and can consume all of it, which would otherwise read
	/// as "the model had nothing to say" (see CHANGELOG v3.0.1).
	#[error("`claude` returned no text (stop_reason: {stop_reason})")]
	#[diagnostic(code(ask_llm::cli::empty), help("thinking ate the whole output budget; lower `ThinkingLevel` or raise `max_tokens`"))]
	Empty { stop_reason: String },
}

/// OpenAI sends `{"error":{"message","type","code"}}`; Ollama sends `{"error":"<string>"}`. Accepting only the
/// first would land every local failure in [`Api::Other`].
#[derive(Default, serde::Deserialize)]
#[serde(from = "RawEnvelope")]
struct Envelope {
	message: Option<String>,
	code: Option<String>,
}
impl Envelope {
	fn parse(body: &str) -> Self {
		serde_json::from_str(body).unwrap_or_default() // a provider may answer with html from a CDN, which is what `Api::Other` keeps the raw body for
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
