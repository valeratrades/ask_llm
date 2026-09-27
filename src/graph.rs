//! Every [`Model`] tier enters a static DAG of concrete deployments, climbing from cheap to capable
//! until one answers. See `docs/ARCHITECTURE.md` for the graph and its invariants.
use std::future::Future;

use crate::{
	Backend, Model, Response,
	config::AppConfig,
	error::{Attempt, Error, Exhausted, Failure, Unrecoverable},
	providers::{
		claude::{self, ClaudeModel},
		ollama,
		openai::{self, OpenAiModel},
	},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, derive_more::Display)]
pub enum Provider {
	Ollama,
	#[display("OpenAI")]
	OpenAi,
	Claude,
}

/// Declared in capability order: an edge only ever points further down this list.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Node {
	TranslateGemma,
	Qwen,
	Luna,
	Sonnet5,
	Opus5_5,
	Fable5_1,
}
impl Node {
	const fn provider(self) -> Provider {
		match self {
			Self::TranslateGemma | Self::Qwen => Provider::Ollama,
			Self::Luna => Provider::OpenAi,
			Self::Sonnet5 | Self::Opus5_5 | Self::Fable5_1 => Provider::Claude,
		}
	}

	/// Ordered by preference, cheapest first.
	const fn next(self) -> &'static [Node] {
		match self {
			Self::TranslateGemma => &[Self::Qwen],
			Self::Qwen => &[Self::Luna],
			Self::Luna => &[Self::Sonnet5],
			Self::Sonnet5 => &[Self::Opus5_5],
			Self::Opus5_5 => &[Self::Fable5_1],
			Self::Fable5_1 => &[],
		}
	}

	const fn model(self) -> &'static str {
		match self {
			Self::TranslateGemma => "translategemma:4b",
			Self::Qwen => "qwen3.5:4b",
			Self::Luna => OpenAiModel::Luna.to_str(),
			Self::Sonnet5 => ClaudeModel::Sonnet5.to_str(),
			Self::Opus5_5 => ClaudeModel::Opus5_5.to_str(),
			Self::Fable5_1 => ClaudeModel::Fable5_1.to_str(),
		}
	}

	/// Resolved per request, so a key missing for *this* node fails only this node.
	pub(crate) fn backend(self, config: &AppConfig) -> Result<Box<dyn Backend>, Failure> {
		Ok(match self {
			Self::TranslateGemma | Self::Qwen => Box::new(ollama::Ollama {
				model: self.model().to_string(),
				url: "http://localhost:11434/api/chat".to_string(),
			}),
			Self::Luna => Box::new(openai::OpenAi {
				api_key: config.openai_token.clone().or_else(|| std::env::var("OPENAI_API_KEY").ok()).ok_or_else(|| {
					Unrecoverable::new_missing_token("hand it over with `Client::openai_token(…)`, put `openai_token` in ~/.config/ask_llm.nix, or export OPENAI_API_KEY".to_string())
				})?,
				model: OpenAiModel::Luna,
			}),
			Self::Sonnet5 | Self::Opus5_5 | Self::Fable5_1 => Box::new(claude::Claude {
				// not an api key, and optional: the `claude` CLI resolves its own subscription credentials when nothing here overrides them
				oauth_token: config.claude_token.clone().or_else(|| std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok()),
				model: match self {
					Self::Sonnet5 => ClaudeModel::Sonnet5,
					Self::Opus5_5 => ClaudeModel::Opus5_5,
					Self::Fable5_1 => ClaudeModel::Fable5_1,
					_ => unreachable!("matched on the outer arm"),
				},
			}),
		})
	}
}

impl Model {
	pub(crate) const fn entry(self) -> Node {
		match self {
			Model::Translate => Node::TranslateGemma,
			Model::Cheap => Node::Qwen,
			Model::Fast | Model::Video => Node::Luna,
			Model::Medium | Model::Slow => Node::Opus5_5,
			Model::PriceInsensitive => Node::Fable5_1,
		}
	}
}

/// Depth-first from `entry`, successors in listed order. A provider that failed for good is skipped for the
/// rest of this call; nothing about it outlives the call.
pub(crate) async fn walk<F, Fut>(entry: Node, mut attempt: F) -> crate::Result<Response>
where
	F: FnMut(Node) -> Fut,
	Fut: Future<Output = Result<Response, Failure>>, {
	let mut dead: Vec<Provider> = Vec::new();
	let mut visited: Vec<Node> = Vec::new();
	let mut attempts: Vec<Attempt> = Vec::new();
	let mut frontier = vec![entry];
	while let Some(node) = frontier.pop() {
		if visited.contains(&node) {
			continue;
		}
		visited.push(node);
		frontier.extend(node.next().iter().rev());
		if dead.contains(&node.provider()) {
			continue;
		}
		if let Some(failed) = attempts.last() {
			tracing::info!(provider = %node.provider(), model = node.model(), replacing = failed.model, "falling back");
		}
		let failure = match attempt(node).await {
			Ok(response) => return Ok(response),
			Err(failure) => failure,
		};
		tracing::warn!(provider = %node.provider(), model = node.model(), %failure, "climbing past a failed node");
		if let Failure::Unrecoverable(u) = &failure
			&& !node_scoped(u)
		{
			dead.push(node.provider());
		}
		attempts.push(Attempt {
			provider: node.provider(),
			model: node.model(),
			failure,
		});
	}
	assert!(!attempts.is_empty(), "the entry node is always attempted");
	let recoverable = attempts.iter().any(|a| matches!(a.failure, Failure::Recoverable(_)));
	let exhausted = Exhausted { attempts };
	Err(match recoverable {
		true => Error::Recoverable(exhausted),
		false => Error::Unrecoverable(exhausted),
	})
}

/// About the node, not the account behind it: `translategemma` not being pulled says nothing about `qwen`.
fn node_scoped(u: &Unrecoverable) -> bool {
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

	const ALL: [Node; 6] = [Node::TranslateGemma, Node::Qwen, Node::Luna, Node::Sonnet5, Node::Opus5_5, Node::Fable5_1];

	fn answered(node: Node) -> Response {
		Response {
			text: "pong".to_string(),
			cost_cents: 0.,
			duration: std::time::Duration::ZERO,
			overhead: std::time::Duration::ZERO,
			model: node.model().to_string(),
			thinking: ThinkingLevel::None,
		}
	}

	async fn transport_failure() -> Failure {
		let source = reqwest::Client::new().get("http://127.0.0.1:1").send().await.expect_err("nothing listens on port 1");
		Transport::classify(Provider::Ollama, source).into()
	}

	/// Runs the walk with `fail` deciding each node's outcome, and returns what it did with the nodes it tried.
	async fn run(entry: Node, fail: impl AsyncFn(Node) -> Option<Failure>) -> (crate::Result<Response>, Vec<Node>) {
		let tried = RefCell::new(Vec::new());
		let result = walk(entry, |node| {
			tried.borrow_mut().push(node);
			let outcome = fail(node);
			async move { outcome.await.map_or_else(|| Ok(answered(node)), Err) }
		})
		.await;
		(result, tried.into_inner())
	}

	#[test]
	fn edges_only_go_up() {
		for node in ALL {
			for next in node.next() {
				assert!(*next > node, "{node:?} -> {next:?} points down");
			}
		}
	}

	#[tokio::test]
	async fn spent_openai_account_hands_over_to_claude() {
		let (result, tried) = run(Model::Fast.entry(), async |node| {
			(node == Node::Luna).then(|| Unrecoverable::new_quota("no credits".into()).into())
		})
		.await;
		assert_eq!(result.unwrap().model, Node::Sonnet5.model());
		assert_eq!(tried, [Node::Luna, Node::Sonnet5]);
	}

	#[tokio::test]
	async fn rejected_claude_credentials_skip_every_claude_node() {
		let (result, tried) = run(Node::Sonnet5, async |_| Some(Unrecoverable::new_auth("revoked".into()).into())).await;
		assert_eq!(tried, [Node::Sonnet5]);
		let Err(Error::Unrecoverable(exhausted)) = result else {
			panic!("an account refusing for good is unrecoverable, got {result:?}");
		};
		assert_eq!(exhausted.attempts.len(), 1);
		assert_eq!(exhausted.attempts[0].provider, Provider::Claude);
	}

	#[tokio::test]
	async fn missing_local_model_does_not_take_its_provider_down() {
		let (result, tried) = run(Model::Translate.entry(), async |node| {
			(node == Node::TranslateGemma).then(|| Unrecoverable::new_model_unavailable("not pulled".into(), String::new()).into())
		})
		.await;
		assert_eq!(result.unwrap().model, Node::Qwen.model());
		assert_eq!(tried, [Node::TranslateGemma, Node::Qwen]);
	}

	#[tokio::test]
	async fn transport_everywhere_is_recoverable() {
		let (result, tried) = run(Model::Translate.entry(), async |_| Some(transport_failure().await)).await;
		assert_eq!(tried, ALL);
		assert!(matches!(result, Err(Error::Recoverable(_))), "{result:?}");
	}
}
