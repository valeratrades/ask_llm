//! Each [`Model`] owns one deployment, and [`Model::next`] chains them from cheap to capable; a call climbs
//! the chain until one answers. See `docs/ARCHITECTURE.md` for the graph and its invariants.
use std::future::Future;

use crate::{
	Backend, Response,
	config::AppConfig,
	error::{Attempt, Error, Exhausted, Failure, Unrecoverable},
	providers::{
		claude::{self, ClaudeModel},
		ollama,
		openai::{self, OpenAiModel},
	},
	segment,
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
	/// The fallback edge. Never points to a weaker model.
	const fn next(self) -> Option<Model> {
		match self {
			Self::Translate => Some(Self::Cheap),
			Self::Cheap => Some(Self::Fast),
			Self::Fast | Self::Video => Some(Self::Medium),
			Self::Medium => Some(Self::Slow),
			Self::Slow => Some(Self::PriceInsensitive),
			Self::PriceInsensitive | Self::Segment | Self::SegmentLocal => None,
		}
	}

	const fn deployment(self) -> Deployment {
		match self {
			Self::Translate => Deployment::Ollama("translategemma:4b"),
			Self::Cheap => Deployment::Ollama("qwen3.5:4b"),
			Self::Fast | Self::Video => Deployment::OpenAi(OpenAiModel::Luna),
			Self::Medium | Self::Slow => Deployment::Claude(ClaudeModel::Opus5_5),
			Self::PriceInsensitive => Deployment::Claude(ClaudeModel::Fable5_1),
			Self::Segment => Deployment::Segment(segment::Model::Sam3_1),
			Self::SegmentLocal => Deployment::Segment(segment::Model::Sam2_1),
		}
	}

	/// Resolved per request, so a key missing for *this* model fails only this model.
	pub(crate) fn backend(self, config: &AppConfig) -> Result<Box<dyn Backend>, Failure> {
		Ok(match self.deployment() {
			Deployment::Ollama(model) => Box::new(ollama::Ollama {
				model: model.to_string(),
				url: "http://localhost:11434/api/chat".to_string(),
			}),
			Deployment::OpenAi(model) => Box::new(openai::OpenAi {
				api_key: config.openai_token.clone().or_else(|| std::env::var("OPENAI_API_KEY").ok()).ok_or_else(|| {
					Unrecoverable::new_missing_token("hand it over with `Client::openai_token(…)`, put `openai_token` in ~/.config/ask_llm.nix, or export OPENAI_API_KEY".to_string())
				})?,
				model,
			}),
			Deployment::Claude(model) => Box::new(claude::Claude {
				// not an api key, and optional: the `claude` CLI resolves its own subscription credentials when nothing here overrides them
				oauth_token: config.claude_token.clone().or_else(|| std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok()),
				model,
			}),
			Deployment::Segment(model) => {
				return Err(Unrecoverable::new_unsupported("a conversation", format!("{} makes masks, through `segment::segment`", model.id())).into());
			}
		})
	}
}

impl TryFrom<Model> for segment::Model {
	type Error = eyre::Report;

	fn try_from(model: Model) -> eyre::Result<Self> {
		match model.deployment() {
			Deployment::Segment(m) => Ok(m),
			_ => eyre::bail!("{model:?} makes no masks; `Segment` and `SegmentLocal` (`segment`, `segment-local`) do"),
		}
	}
}

/// What a [`Model`] runs on; the provider and the backend both follow from it.
enum Deployment {
	Ollama(&'static str),
	OpenAi(OpenAiModel),
	Claude(ClaudeModel),
	Segment(segment::Model),
}
impl Deployment {
	const fn provider(&self) -> Provider {
		match self {
			Self::Ollama(_) => Provider::Ollama,
			Self::OpenAi(_) => Provider::OpenAi,
			Self::Claude(_) => Provider::Claude,
			Self::Segment(model) => model.provider(),
		}
	}

	const fn name(&self) -> &'static str {
		match self {
			Self::Ollama(name) => name,
			Self::OpenAi(model) => model.to_str(),
			Self::Claude(model) => model.to_str(),
			Self::Segment(model) => model.id(),
		}
	}
}

/// Follows [`Model::next`] from `entry`. A provider that failed for good is skipped for the rest of this call;
/// nothing about it outlives the call.
pub(crate) async fn walk<F, Fut>(entry: Model, mut attempt: F) -> crate::Result<Response>
where
	F: FnMut(Model) -> Fut,
	Fut: Future<Output = Result<Response, Failure>>, {
	let mut dead: Vec<Provider> = Vec::new();
	let mut attempts: Vec<Attempt> = Vec::new();
	let mut next = Some(entry);
	while let Some(model) = next {
		next = model.next();
		let deployment = model.deployment();
		if dead.contains(&deployment.provider()) {
			continue;
		}
		if let Some(failed) = attempts.last() {
			tracing::info!(provider = %deployment.provider(), model = deployment.name(), replacing = failed.model, "falling back");
		}
		let failure = match attempt(model).await {
			Ok(response) => return Ok(response),
			Err(failure) => failure,
		};
		tracing::warn!(provider = %deployment.provider(), model = deployment.name(), %failure, "climbing past a failed model");
		if let Failure::Unrecoverable(u) = &failure
			&& !model_scoped(u)
		{
			dead.push(deployment.provider());
		}
		attempts.push(Attempt {
			provider: deployment.provider(),
			model: deployment.name(),
			failure,
		});
	}
	assert!(!attempts.is_empty(), "the entry model is always attempted");
	let recoverable = attempts.iter().any(|a| matches!(a.failure, Failure::Recoverable(_)));
	let exhausted = Exhausted { attempts };
	Err(match recoverable {
		true => Error::Recoverable(exhausted),
		false => Error::Unrecoverable(exhausted),
	})
}

/// About the model, not the account behind it: `translategemma` not being pulled says nothing about `qwen`.
fn model_scoped(u: &Unrecoverable) -> bool {
	match u {
		Unrecoverable::ModelUnavailable { .. } | Unrecoverable::ContextLength { .. } | Unrecoverable::Unsupported { .. } | Unrecoverable::Cli(crate::Cli::Empty { .. }) => true,
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
	use crate::{ThinkingLevel, Transport};

	fn answered(model: Model) -> Response {
		Response {
			text: "pong".to_string(),
			cost_cents: 0.,
			duration: std::time::Duration::ZERO,
			overhead: std::time::Duration::ZERO,
			model: model.deployment().name().to_string(),
			thinking: ThinkingLevel::None,
			usage: crate::Usage::default(),
		}
	}

	async fn transport_failure() -> Failure {
		let source = reqwest::Client::new().get("http://127.0.0.1:1").send().await.expect_err("nothing listens on port 1");
		Transport::classify(Provider::Ollama, source).into()
	}

	/// Runs the walk with `fail` deciding each model's outcome, and returns what it did with the models it tried.
	async fn run(entry: Model, fail: impl AsyncFn(Model) -> Option<Failure>) -> (crate::Result<Response>, Vec<Model>) {
		let tried = RefCell::new(Vec::new());
		let result = walk(entry, |model| {
			tried.borrow_mut().push(model);
			let outcome = fail(model);
			async move { outcome.await.map_or_else(|| Ok(answered(model)), Err) }
		})
		.await;
		(result, tried.into_inner())
	}

	#[tokio::test]
	async fn spent_openai_account_hands_over_to_claude() {
		let (result, tried) = run(Model::Fast, async |model| (model == Model::Fast).then(|| Unrecoverable::new_quota("no credits".into()).into())).await;
		assert_eq!(result.unwrap().model, Model::Medium.deployment().name());
		assert_eq!(tried, [Model::Fast, Model::Medium]);
	}

	#[tokio::test]
	async fn rejected_claude_credentials_skip_every_claude_model() {
		let (result, tried) = run(Model::Medium, async |_| Some(Unrecoverable::new_auth("revoked".into()).into())).await;
		assert_eq!(tried, [Model::Medium]);
		let Err(Error::Unrecoverable(exhausted)) = result else {
			panic!("an account refusing for good is unrecoverable, got {result:?}");
		};
		assert_eq!(exhausted.attempts.len(), 1);
		assert_eq!(exhausted.attempts[0].provider, Provider::Claude);
	}

	#[tokio::test]
	async fn missing_local_model_does_not_take_its_provider_down() {
		let (result, tried) = run(Model::Translate, async |model| {
			(model == Model::Translate).then(|| Unrecoverable::new_model_unavailable("not pulled".into(), String::new()).into())
		})
		.await;
		assert_eq!(result.unwrap().model, Model::Cheap.deployment().name());
		assert_eq!(tried, [Model::Translate, Model::Cheap]);
	}

	#[tokio::test]
	async fn transport_everywhere_is_recoverable() {
		let (result, tried) = run(Model::Translate, async |_| Some(transport_failure().await)).await;
		assert_eq!(tried, [Model::Translate, Model::Cheap, Model::Fast, Model::Medium, Model::Slow, Model::PriceInsensitive]);
		assert!(matches!(result, Err(Error::Recoverable(_))), "{result:?}");
	}
}
