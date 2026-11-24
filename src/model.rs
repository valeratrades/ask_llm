//! Each tier ([`Model`]) stands for one [`Source`], and [`Model::next`] chains them downwards; a call walks the chain,
//! or the one its caller gave, until a source answers. See `docs/ARCHITECTURE.md` for the graph and its invariants.
use std::future::Future;

use tracing::Level;

use crate::{
	Cli, Request, Transport,
	config::AppConfig,
	error::{Attempt, Error, Exhausted, Failure, Recoverable, Unrecoverable},
	providers::{
		claude::{self, ClaudeModel},
		ollama,
		openai::{self, OpenAiModel},
	},
	segment,
	wire::Answer,
};

#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, derive_more::Display)]
pub enum Provider {
	Ollama,
	#[display("OpenAI")]
	OpenAi,
	Claude,
	#[display("fal")]
	Fal,
	/// this machine's GPU
	#[display("local")]
	Local,
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, derive_more::FromStr)]
pub enum Model {
	Fast,
	#[default]
	Medium,
	Slow,
	PriceInsensitive,
	Cheap,
	Translate,
	/// Reads frames, for [`Client::watch`](crate::Client::watch).
	Video,
	/// The best mask a provider makes, for [`segment`](crate::segment::segment) only: `segment::Model::try_from`.
	Segment,
	/// The best mask made on this machine, as [`Model::Segment`].
	#[from_str(rename_all = "kebab-case")]
	SegmentLocal,
}
impl Model {
	/// The fallback edge. Only ever down.
	const fn next(self) -> Option<Model> {
		match self {
			Self::PriceInsensitive => Some(Self::Slow),
			Self::Slow => Some(Self::Medium),
			Self::Medium => Some(Self::Fast),
			Self::Fast | Self::Translate => Some(Self::Cheap),
			Self::Cheap | Self::Video | Self::Segment | Self::SegmentLocal => None,
		}
	}
}

impl TryFrom<Model> for segment::Model {
	type Error = eyre::Report;

	fn try_from(model: Model) -> eyre::Result<Self> {
		match model {
			Model::Segment => Ok(segment::Model::Sam3_1),
			Model::SegmentLocal => Ok(segment::Model::Sam2_1),
			_ => eyre::bail!("{model:?} makes no masks; `Segment` and `SegmentLocal` (`segment`, `segment-local`) do"),
		}
	}
}

/// Who answers: a model of a provider's, on whose credential, and where.
#[derive(Clone)]
pub struct Source {
	pub(crate) provider: Provider,
	pub(crate) model: &'static str,
	pub(crate) token: Option<String>,
	pub(crate) origin: Option<String>,
	theirs: bool,
}
impl Source {
	/// `model` as the provider names it.
	pub const fn new(provider: Provider, model: &'static str) -> Self {
		Self {
			provider,
			model,
			token: None,
			origin: None,
			theirs: false,
		}
	}

	/// Claude: `sk-ant-oat01-…`, as [`AppConfig::claude_token`]. OpenAI: an api key. Without one the client's config decides.
	pub fn token(mut self, token: impl Into<String>) -> Self {
		self.token = Some(token.into());
		self
	}

	/// What comes before the provider's own path, eg `https://api.anthropic.com`.
	pub fn at(mut self, origin: impl Into<String>) -> Self {
		self.origin = Some(origin.into());
		self
	}

	/// The credential is somebody else's, theirs to fix: an account refusing it logs at warn, where ours logs at error.
	pub fn theirs(mut self) -> Self {
		self.theirs = true;
		self
	}

	pub(crate) async fn call(&self, config: &AppConfig, request: &Request<'_>) -> Result<Answer, Failure> {
		let start = std::time::Instant::now();
		let mut answer = match self.provider {
			Provider::Claude => claude::call(self, config, request).await,
			Provider::OpenAi => openai::call(self, config, request).await,
			Provider::Ollama => ollama::call(self, request).await,
			Provider::Fal | Provider::Local => Err(Unrecoverable::new_unsupported("a conversation", format!("{} makes masks, through `segment::segment`", self.model)).into()),
		}?;
		answer.response.duration = start.elapsed();
		Ok(answer)
	}

	/// The failure as our logs tell it. `told`: this credential's refusal already was, in this call.
	pub(crate) fn tell(&self, failure: &Failure, told: bool) {
		let level = match failure {
			Failure::Recoverable(Recoverable::Transport(Transport::Unreachable { .. })) if self.provider == Provider::Ollama => Level::INFO,
			Failure::Recoverable(_) => Level::WARN,
			Failure::Unrecoverable(u) => match u {
				Unrecoverable::MissingToken { .. } | Unrecoverable::Cli(Cli::NotInstalled { .. }) => Level::INFO,
				Unrecoverable::ModelUnavailable { .. } if self.provider == Provider::Ollama => Level::INFO,
				Unrecoverable::ModelUnavailable { .. } => Level::ERROR,
				Unrecoverable::Auth { .. } | Unrecoverable::Quota { .. } | Unrecoverable::GeoBlocked { .. } => match self.theirs || told {
					true => Level::WARN,
					false => Level::ERROR,
				},
				Unrecoverable::Cli(Cli::Exit { stderr: said, .. } | Cli::Failed { message: said, .. }) => match told || said.contains("limit") {
					true => Level::WARN, // a spent window: the CLI has no other word for it
					false => Level::ERROR,
				},
				Unrecoverable::Cli(Cli::Empty { .. })
				| Unrecoverable::ContextLength { .. }
				| Unrecoverable::Unsupported { .. }
				| Unrecoverable::Refused { .. }
				| Unrecoverable::Schema { .. }
				| Unrecoverable::Empty { .. }
				| Unrecoverable::Other { .. } => Level::WARN,
			},
		};
		let (provider, model) = (self.provider, self.model);
		match level {
			Level::ERROR => tracing::error!(%provider, model, reason = %failure, "a dead source"),
			Level::WARN => tracing::warn!(%provider, model, reason = %failure, "a source failed"),
			_ => tracing::info!(%provider, model, reason = %failure, "a source not set up here"),
		}
	}
}
impl std::fmt::Debug for Source {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Source")
			.field("provider", &self.provider)
			.field("model", &self.model)
			.field("origin", &self.origin)
			.field("theirs", &self.theirs)
			.finish_non_exhaustive()
	}
}
impl From<Model> for Source {
	fn from(model: Model) -> Self {
		match model {
			Model::Translate => Self::new(Provider::Ollama, "translategemma:4b"),
			Model::Cheap => Self::new(Provider::Ollama, "qwen3.5:4b"),
			Model::Fast | Model::Video => OpenAiModel::Luna.into(),
			Model::Medium => ClaudeModel::Sonnet5_5.into(),
			Model::Slow => ClaudeModel::Opus5_5.into(),
			Model::PriceInsensitive => ClaudeModel::Fable5_1.into(),
			Model::Segment => Self::new(Provider::Fal, segment::Model::Sam3_1.id()),
			Model::SegmentLocal => Self::new(Provider::Local, segment::Model::Sam2_1.id()),
		}
	}
}
impl From<ClaudeModel> for Source {
	fn from(model: ClaudeModel) -> Self {
		Self::new(Provider::Claude, model.to_str())
	}
}
impl From<OpenAiModel> for Source {
	fn from(model: OpenAiModel) -> Self {
		Self::new(Provider::OpenAi, model.to_str())
	}
}

/// What a call walks.
#[derive(Clone)]
pub(crate) enum Path {
	/// The tiers from this one down.
	Tiers(Model),
	/// Exactly these, none skipped, so an attempt's place is its source's.
	Chain(Vec<Source>),
}
impl Path {
	/// What was asked for, as the logs name it.
	pub fn requested(&self) -> String {
		match self {
			Self::Tiers(entry) => format!("{entry:?}"),
			Self::Chain(sources) => sources[0].model.to_string(),
		}
	}
}

pub(crate) struct Walked<T> {
	pub answer: T,
	pub source: Source,
	pub failed: Vec<Attempt>,
}

/// A provider whose account failed for good is skipped for the rest of a walk down the tiers; nothing about it outlives
/// the call.
pub(crate) async fn walk<T, F, Fut>(path: &Path, mut attempt: F) -> crate::Result<Walked<T>>
where
	F: FnMut(Source) -> Fut,
	Fut: Future<Output = Result<T, Failure>>, {
	let sources: Vec<Source> = match path {
		Path::Tiers(entry) => std::iter::successors(Some(*entry), |m| m.next()).map(Source::from).collect(),
		Path::Chain(sources) => sources.clone(),
	};
	let mut dead: Vec<(Provider, Option<String>)> = Vec::new();
	let mut told: Vec<(Provider, Option<String>)> = Vec::new();
	let mut failed: Vec<Attempt> = Vec::new();
	for source in sources {
		let account = (source.provider, source.token.clone());
		if dead.contains(&account) && matches!(path, Path::Tiers(_)) {
			continue;
		}
		let failure = match attempt(source.clone()).await {
			Ok(answer) => {
				if let Some(last) = failed.last() {
					let (requested, replacing) = (path.requested(), last.model);
					match path {
						Path::Tiers(_) => tracing::warn!(requested, provider = %source.provider, answering = source.model, replacing, "falling back: a tier below the one asked for answers"),
						Path::Chain(_) => tracing::info!(requested, provider = %source.provider, answering = source.model, replacing, "falling back"),
					}
				}
				return Ok(Walked { answer, source, failed });
			}
			Err(failure) => failure,
		};
		source.tell(&failure, told.contains(&account));
		if matches!(
			failure,
			Failure::Unrecoverable(Unrecoverable::Auth { .. } | Unrecoverable::Quota { .. } | Unrecoverable::GeoBlocked { .. } | Unrecoverable::Cli(_))
		) {
			told.push(account.clone());
		}
		if let Failure::Unrecoverable(u) = &failure
			&& !node_scoped(u)
		{
			dead.push(account);
		}
		failed.push(Attempt {
			provider: source.provider,
			model: source.model,
			failure,
		});
	}
	assert!(!failed.is_empty(), "the first source is always attempted");
	// nobody serves ollama on most machines, and waiting does not change that
	let unserved = |a: &Attempt| a.provider == Provider::Ollama && matches!(a.failure, Failure::Recoverable(Recoverable::Transport(Transport::Unreachable { .. })));
	let recoverable = failed.iter().enumerate().any(|(i, a)| matches!(a.failure, Failure::Recoverable(_)) && (i == 0 || !unserved(a)));
	let exhausted = Exhausted { attempts: failed };
	Err(match recoverable {
		true => Error::Recoverable(exhausted),
		false => Error::Unrecoverable(exhausted),
	})
}

/// About the model, not the account behind it: `translategemma` not being pulled says nothing about `qwen`.
fn node_scoped(u: &Unrecoverable) -> bool {
	match u {
		Unrecoverable::ModelUnavailable { .. }
		| Unrecoverable::ContextLength { .. }
		| Unrecoverable::Unsupported { .. }
		| Unrecoverable::Empty { .. }
		| Unrecoverable::Cli(Cli::Empty { .. }) => true,
		Unrecoverable::MissingToken { .. }
		| Unrecoverable::Auth { .. }
		| Unrecoverable::Quota { .. }
		| Unrecoverable::GeoBlocked { .. }
		| Unrecoverable::Refused { .. }
		| Unrecoverable::Schema { .. }
		| Unrecoverable::Cli(_)
		| Unrecoverable::Other { .. } => false,
	}
}

#[cfg(test)]
mod tests {
	use std::cell::RefCell;

	use super::*;

	async fn unreachable() -> Failure {
		let _ = rustls::crypto::ring::default_provider().install_default(); // Err: a provider is already installed
		let source = reqwest::Client::new().get("http://127.0.0.1:1").send().await.expect_err("nothing listens on port 1");
		Transport::classify(Provider::Ollama, source).into()
	}

	/// Runs the walk with `fail` deciding each source's outcome, and returns the model that answered with those tried.
	async fn run(entry: Model, fail: impl AsyncFn(&Source) -> Option<Failure>) -> (crate::Result<&'static str>, Vec<&'static str>) {
		let tried = RefCell::new(Vec::new());
		let result = walk(&Path::Tiers(entry), async |source: Source| {
			tried.borrow_mut().push(source.model);
			fail(&source).await.map_or(Ok(()), Err)
		})
		.await;
		(result.map(|w| w.source.model), tried.into_inner())
	}

	const fn name(model: Model) -> &'static str {
		match model {
			Model::PriceInsensitive => ClaudeModel::Fable5_1.to_str(),
			Model::Slow => ClaudeModel::Opus5_5.to_str(),
			Model::Medium => ClaudeModel::Sonnet5_5.to_str(),
			Model::Fast => OpenAiModel::Luna.to_str(),
			Model::Cheap => "qwen3.5:4b",
			Model::Translate => "translategemma:4b",
			_ => panic!("not on the path"),
		}
	}

	#[tokio::test]
	async fn rejected_claude_credentials_hand_down_past_every_claude_model() {
		let (result, tried) = run(Model::PriceInsensitive, async |s| {
			(s.provider == Provider::Claude).then(|| Unrecoverable::new_auth("revoked".into()).into())
		})
		.await;
		assert_eq!(result.unwrap(), name(Model::Fast));
		assert_eq!(tried, [name(Model::PriceInsensitive), name(Model::Fast)]);
	}

	#[tokio::test]
	async fn overloads_walk_every_tier_down() {
		let (result, tried) = run(Model::PriceInsensitive, async |_| Some(Recoverable::new_overloaded("busy".into()).into())).await;
		assert_eq!(tried, [Model::PriceInsensitive, Model::Slow, Model::Medium, Model::Fast, Model::Cheap].map(name));
		assert!(matches!(result, Err(Error::Recoverable(_))), "{result:?}");
	}

	#[tokio::test]
	async fn an_account_refusing_for_good_is_unrecoverable_where_nobody_serves_ollama() {
		let (result, tried) = run(Model::Fast, async |s| {
			Some(match s.provider {
				Provider::Ollama => unreachable().await,
				_ => Unrecoverable::new_quota("no credits".into()).into(),
			})
		})
		.await;
		assert_eq!(tried, [name(Model::Fast), name(Model::Cheap)]);
		let Err(Error::Unrecoverable(exhausted)) = result else {
			panic!("an account refusing for good is unrecoverable, got {result:?}");
		};
		assert_eq!(exhausted.attempts.iter().map(|a| a.provider).collect::<Vec<_>>(), [Provider::OpenAi, Provider::Ollama]);
	}

	#[tokio::test]
	async fn missing_local_model_does_not_take_its_provider_down() {
		let (result, tried) = run(Model::Translate, async |s| {
			(s.model == name(Model::Translate)).then(|| Unrecoverable::new_model_unavailable("not pulled".into(), String::new()).into())
		})
		.await;
		assert_eq!(result.unwrap(), name(Model::Cheap));
		assert_eq!(tried, [name(Model::Translate), name(Model::Cheap)]);
	}

	#[tokio::test]
	async fn transport_everywhere_is_recoverable() {
		let (result, tried) = run(Model::Translate, async |_| Some(unreachable().await)).await;
		assert_eq!(tried, [name(Model::Translate), name(Model::Cheap)]);
		assert!(matches!(result, Err(Error::Recoverable(_))), "{result:?}");
	}
}
