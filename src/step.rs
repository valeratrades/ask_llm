//! One step of an agent loop; the loop is the caller's.
use std::sync::Arc;

use crate::{
	Attempt, Client, Conversation, Error, Interrupted, Request, Response, Result, Source, model,
	wire::{Answer, Round, Turn},
};

#[derive(Clone, Debug)]
pub struct Tool {
	pub name: String,
	pub description: String,
	/// A JSON schema, of an object.
	pub input_schema: serde_json::Value,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ToolCall {
	pub id: String,
	pub name: String,
	pub input: serde_json::Value,
}

/// Why the model stopped.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Stop {
	End,
	/// Cut at the token cap; a call it was making is not among the answer's.
	MaxTokens,
	/// The provider's policy, or the model by it. Only a [`Step`] is ever it: elsewhere it is [`Unrecoverable::Refused`](crate::Unrecoverable::Refused).
	Refused,
	/// To have these run: [`Step::resume`].
	Tools(Vec<ToolCall>),
}

/// A model's answer to a conversation with tools, held to the source that gave it: what follows on its calls is asked of
/// that source alone.
///
/// ```no_run
/// # async fn f(conv: &ask_llm::Conversation, tools: &[ask_llm::Tool], run: impl AsyncFn(&ask_llm::ToolCall) -> String) -> ask_llm::Result<String> {
/// use ask_llm::{Client, Stop};
/// let mut step = Client::default().step(conv, tools).await?;
/// while let Stop::Tools(calls) = &step.stop {
/// 	let mut results = Vec::new();
/// 	for call in calls {
/// 		results.push(run(call).await);
/// 	}
/// 	step = step.resume(results).await?;
/// }
/// # Ok(step.response.text) }
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub struct Step {
	pub response: Response,
	/// What failed before it answered, so in a [`Client::fallback_chain`] its length is the place of the source that did.
	pub failed: Vec<Attempt>,
	turn: Option<Turn>,
	rounds: Vec<Arc<Round>>,
	held: Arc<Held>,
}
impl Step {
	/// The next step, `results` being what the calls answered, in their order. On a recoverable [`Error::Interrupted`] it can be asked again.
	pub async fn resume(&self, results: impl IntoIterator<Item = impl Into<String>>) -> Result<Step> {
		let Stop::Tools(calls) = &self.response.stop else {
			panic!("only a step that stopped for tools is resumed, and this one stopped for {:?}", self.response.stop);
		};
		let results: Vec<String> = results.into_iter().map(Into::into).collect();
		assert_eq!(results.len(), calls.len(), "each call has its result");
		let mut rounds = self.rounds.clone();
		rounds.push(Arc::new(Round {
			turn: self.turn.clone().expect("calls come over the wire"),
			calls: calls.clone(),
			results,
		}));
		let Held { client, source, .. } = &*self.held;
		match source.call(&client.config, &self.held.request(&rounds)).await {
			Ok(answer) => Ok(Self::new(answer, Vec::new(), rounds, self.held.clone())),
			Err(failure) => {
				source.tell(&failure, false);
				Err(Error::Interrupted(Interrupted {
					round: rounds.len(),
					attempt: Attempt {
						provider: source.provider,
						model: source.model,
						failure,
					},
				}))
			}
		}
	}

	fn new(Answer { response, turn, .. }: Answer, failed: Vec<Attempt>, rounds: Vec<Arc<Round>>, held: Arc<Held>) -> Self {
		held.client.log(&response);
		Self {
			response,
			failed,
			turn,
			rounds,
			held,
		}
	}
}
impl std::ops::Deref for Step {
	type Target = Response;

	fn deref(&self) -> &Response {
		&self.response
	}
}

/// What every step of a conversation asks with.
#[derive(Debug)]
struct Held {
	client: Client,
	conversation: Conversation,
	tools: Vec<Tool>,
	source: Source,
}
impl Held {
	fn request<'a>(&'a self, rounds: &'a [Arc<Round>]) -> Request<'a> {
		Request {
			rounds,
			tools: &self.tools,
			step: true,
			..self.client.request(&self.conversation)
		}
	}
}

impl Client {
	/// The first step of an agent loop, and the only one another source may stand in on: see [`Step`]. Without tools, the one
	/// answer, told with what failed before it and, a refusal, as one.
	pub async fn step(&self, conv: &Conversation, tools: &[Tool]) -> Result<Step> {
		let request = Request {
			tools,
			step: true,
			..self.request(conv)
		};
		let model::Walked { answer, source, failed } = model::walk(&self.path, async |source: Source| source.call(&self.config, &request).await).await?;
		let held = Arc::new(Held {
			client: self.clone(),
			conversation: conv.clone(),
			tools: tools.to_vec(),
			source,
		});
		Ok(Step::new(answer, failed, Vec::new(), held))
	}
}
