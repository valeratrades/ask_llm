//! Whom a call asks, and what it tells of them, against servers that answer as the providers do.
use std::{
	io::{Read as _, Write as _},
	net::TcpListener,
	sync::{Arc, LazyLock, Mutex},
};

use ask_llm::{
	Client, Conversation, Error, Failure, Model, Provider, Recoverable, Role, Source, Stop, Tool, Unrecoverable,
	providers::{claude::ClaudeModel, openai::OpenAiModel},
};
use serde_json::{Value, json};
use tracing_subscriber::layer::SubscriberExt as _;

/// The usage log goes nowhere of the developer's, and no key of theirs is asked on.
static ISOLATED: LazyLock<tempfile::TempDir> = LazyLock::new(|| {
	let home = tempfile::tempdir().unwrap();
	// SAFETY: forced by every test before it asks anything, so nothing reads the environment yet
	unsafe {
		std::env::set_var("XDG_DATA_HOME", home.path());
		std::env::remove_var("OPENAI_API_KEY");
	}
	home
});

struct Asked {
	path: String,
	headers: Vec<(String, String)>,
	body: Value,
}
impl Asked {
	fn header(&self, name: &str) -> Option<&str> {
		self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
	}
}

struct Server {
	origin: String,
	asked: Arc<Mutex<Vec<Asked>>>,
}
impl Server {
	/// Answers each request with the next of `replies`: a status, headers, a body.
	fn start(replies: Vec<(u16, &'static str, Value)>) -> Self {
		LazyLock::force(&ISOLATED);
		let listener = TcpListener::bind("127.0.0.1:0").unwrap();
		let origin = format!("http://{}", listener.local_addr().unwrap());
		let asked: Arc<Mutex<Vec<Asked>>> = Arc::default();
		let kept = asked.clone();
		std::thread::spawn(move || {
			for (status, headers, body) in replies {
				let (mut stream, _) = listener.accept().unwrap();
				let mut read = Vec::new();
				let (head, wanted) = loop {
					let mut chunk = [0; 4096];
					let n = stream.read(&mut chunk).unwrap();
					read.extend_from_slice(&chunk[..n]);
					if let Some(end) = read.windows(4).position(|w| w == b"\r\n\r\n") {
						let head = String::from_utf8(read[..end].to_vec()).unwrap();
						let length = head
							.lines()
							.find_map(|l| l.to_lowercase().strip_prefix("content-length: ").map(|n| n.parse::<usize>().unwrap()))
							.unwrap();
						break (head, end + 4 + length);
					}
				};
				while read.len() < wanted {
					let mut chunk = [0; 4096];
					let n = stream.read(&mut chunk).unwrap();
					read.extend_from_slice(&chunk[..n]);
				}
				let body_at = wanted - (wanted - head.len() - 4);
				kept.lock().unwrap().push(Asked {
					path: head.split_whitespace().nth(1).unwrap().to_string(),
					headers: head
						.lines()
						.skip(1)
						.map(|l| l.split_once(": ").unwrap())
						.map(|(n, v)| (n.to_lowercase(), v.to_string()))
						.collect(),
					body: serde_json::from_slice(&read[body_at..]).unwrap(),
				});
				let body = body.to_string();
				write!(
					stream,
					"HTTP/1.1 {status} X\r\ncontent-type: application/json\r\nconnection: close\r\n{headers}content-length: {}\r\n\r\n{body}",
					body.len()
				)
				.unwrap();
			}
		});
		Self { origin, asked }
	}

	fn claude(&self, token: &str) -> Source {
		Source::from(ClaudeModel::Opus5_5).token(token).at(&self.origin)
	}

	fn openai(&self) -> Source {
		Source::from(OpenAiModel::Luna).token("sk-test").at(&self.origin)
	}

	fn asked(&self) -> std::sync::MutexGuard<'_, Vec<Asked>> {
		self.asked.lock().unwrap()
	}
}

fn claude(content: Value, stop_reason: &str) -> (u16, &'static str, Value) {
	let usage = json!({ "input_tokens": 10, "cache_creation_input_tokens": 20, "cache_read_input_tokens": 30, "output_tokens": 40 });
	(
		200,
		"",
		json!({ "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-opus-5-5", "content": content, "stop_reason": stop_reason, "usage": usage }),
	)
}

fn claude_error(status: u16, r#type: &str) -> (u16, &'static str, Value) {
	(status, "", json!({ "type": "error", "error": { "type": r#type, "message": "the provider's words" } }))
}

fn openai(output: Value) -> (u16, &'static str, Value) {
	let usage = json!({ "input_tokens": 100, "input_tokens_details": { "cached_tokens": 60, "cache_write_tokens": 10 }, "output_tokens": 7, "total_tokens": 107 });
	(
		200,
		"",
		json!({ "id": "resp_1", "status": "completed", "model": "gpt-5.6-luna", "output": output, "usage": usage }),
	)
}

fn openai_text(text: &str) -> (u16, &'static str, Value) {
	openai(json!([{ "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": text }] }]))
}

fn question() -> Conversation {
	let mut conv = Conversation::new_with_system("be brief");
	conv.add(Role::User, "how warm is it?");
	conv
}

fn thermometer() -> [Tool; 1] {
	[Tool {
		name: "temperature".into(),
		description: "the temperature outside".into(),
		input_schema: json!({ "type": "object", "properties": { "unit": { "type": "string" } }, "required": ["unit"] }),
	}]
}

/// Levels of the events told while it lives, on this thread.
fn events() -> (Arc<Mutex<Vec<tracing::Level>>>, tracing::subscriber::DefaultGuard) {
	struct Levels(Arc<Mutex<Vec<tracing::Level>>>);
	impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Levels {
		fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
			if event.metadata().target().starts_with("ask_llm") {
				self.0.lock().unwrap().push(*event.metadata().level());
			}
		}
	}
	let levels = Arc::default();
	let guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(Levels(Arc::clone(&levels))));
	(levels, guard)
}

fn count(levels: &Mutex<Vec<tracing::Level>>, level: tracing::Level) -> usize {
	levels.lock().unwrap().iter().filter(|l| **l == level).count()
}

#[tokio::test]
async fn a_chain_is_used_exactly_and_a_dead_source_told_once() {
	let dead = Server::start(vec![claude_error(401, "authentication_error"), claude_error(401, "authentication_error")]);
	let alive = Server::start(vec![openai_text("warm")]);
	let (levels, _guard) = events();
	let step = Client::default()
		.fallback_chain([dead.claude("sk-ant-oat01-a"), dead.claude("sk-ant-oat01-a"), alive.openai()])
		.model(Model::Slow)
		.step(&question(), &[])
		.await
		.unwrap();
	assert_eq!(step.text, "warm");
	assert_eq!(step.failed.len(), 2, "the place of the source that answered");
	assert!(
		step.failed
			.iter()
			.all(|a| a.provider == Provider::Claude && matches!(a.failure, Failure::Unrecoverable(Unrecoverable::Auth { .. })))
	);
	assert_eq!((dead.asked().len(), alive.asked().len()), (2, 1));
	assert_eq!(count(&levels, tracing::Level::ERROR), 1);
}

#[tokio::test]
async fn a_dead_source_is_told_after_it_failed_otherwise() {
	let server = Server::start(vec![claude_error(400, "invalid_request_error"), claude_error(401, "authentication_error")]);
	let (levels, _guard) = events();
	let failed = Client::default()
		.fallback_chain([server.claude("sk-ant-oat01-a"), server.claude("sk-ant-oat01-a")])
		.conversation(&question())
		.await;
	assert!(matches!(failed, Err(Error::Unrecoverable(_))), "{failed:?}");
	assert_eq!(count(&levels, tracing::Level::ERROR), 1);
}

#[tokio::test]
async fn another_token_stands_in_for_a_person_s_dead_one_unpaged() {
	let server = Server::start(vec![claude_error(403, "permission_error"), claude(json!([{ "type": "text", "text": "warm" }]), "end_turn")]);
	let (levels, _guard) = events();
	let answer = Client::default()
		.fallback_chain([server.claude("sk-ant-oat01-theirs").theirs(), server.claude("sk-ant-oat01-ours")])
		.conversation(&question())
		.await
		.unwrap();
	assert_eq!(answer.text, "warm");
	assert_eq!(server.asked()[1].header("authorization"), Some("Bearer sk-ant-oat01-ours"));
	assert_eq!(count(&levels, tracing::Level::ERROR), 0);
	assert_eq!(count(&levels, tracing::Level::WARN), 1);
}

#[tokio::test]
async fn a_chain_that_ends_lines_up_with_its_attempts() {
	let server = Server::start(vec![claude_error(401, "authentication_error"), claude_error(529, "overloaded_error")]);
	let failed = Client::default()
		.fallback_chain([server.claude("sk-ant-oat01-a"), server.claude("sk-ant-oat01-b")])
		.conversation(&question())
		.await;
	let Err(Error::Recoverable(exhausted)) = failed else {
		panic!("an overload clears on its own, got {failed:?}");
	};
	assert!(matches!(exhausted.attempts[0].failure, Failure::Unrecoverable(Unrecoverable::Auth { .. })));
	assert!(matches!(exhausted.attempts[1].failure, Failure::Recoverable(Recoverable::Overloaded { .. })));
}

#[tokio::test]
async fn what_is_unset_or_passing_is_no_error() {
	let busy = Server::start(vec![(
		429,
		"retry-after: 20\r\n",
		json!({ "error": { "message": "slow down", "type": "requests", "code": "rate_limit_exceeded" } }),
	)]);
	let (levels, _guard) = events();
	let failed = Client::default().fallback_chain([Source::from(OpenAiModel::Luna), busy.openai()]).conversation(&question()).await;
	let Err(Error::Recoverable(exhausted)) = failed else {
		panic!("a rate limit clears on its own, got {failed:?}");
	};
	assert!(matches!(exhausted.attempts[0].failure, Failure::Unrecoverable(Unrecoverable::MissingToken { .. })));
	assert!(matches!(exhausted.attempts[1].failure, Failure::Recoverable(Recoverable::RateLimited { retry_after: Some(d), .. }) if d.as_secs() == 20));
	assert_eq!(count(&levels, tracing::Level::ERROR), 0);
	assert_eq!(count(&levels, tracing::Level::WARN), 1);
}

#[tokio::test]
async fn a_spent_subscription_says_when_it_resets() {
	let headers = "anthropic-ratelimit-unified-status: rejected\r\nanthropic-ratelimit-unified-reset: 1793606681\r\n";
	let server = Server::start(vec![(429, headers, json!({ "type": "error", "error": { "type": "rate_limit_error", "message": "limit" } }))]);
	let failed = Client::default().fallback_chain([server.claude("sk-ant-oat01-a")]).conversation(&question()).await;
	let Err(Error::Recoverable(exhausted)) = failed else {
		panic!("a window resets on its own, got {failed:?}");
	};
	let Failure::Recoverable(Recoverable::LimitReached { resets: Some(resets), .. }) = exhausted.attempts[0].failure else {
		panic!("{exhausted:?}");
	};
	assert_eq!(resets, std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_793_606_681));
}

#[tokio::test]
async fn a_tool_round_stays_on_its_source_and_goes_back_as_it_came() {
	let content = json!([
		{ "type": "thinking", "thinking": "", "signature": "s1" },
		{ "type": "text", "text": "looking" },
		{ "type": "tool_use", "id": "t1", "name": "temperature", "input": { "unit": "C" } },
		{ "type": "thinking", "thinking": "", "signature": "s2" },
		{ "type": "tool_use", "id": "t2", "name": "temperature", "input": { "unit": "F" } },
	]);
	let server = Server::start(vec![
		claude(content.clone(), "tool_use"),
		claude_error(529, "overloaded_error"),
		claude(json!([{ "type": "text", "text": "20 C" }]), "end_turn"),
	]);
	let other = Server::start(vec![openai_text("never asked")]);
	let step = Client::default()
		.fallback_chain([server.claude("sk-ant-oat01-a"), other.openai()])
		.step(&question(), &thermometer())
		.await
		.unwrap();
	let Stop::Tools(calls) = &step.stop else {
		panic!("{:?}", step.stop);
	};
	assert_eq!(
		calls.iter().map(|c| (c.id.as_str(), c.name.as_str(), &c.input["unit"])).collect::<Vec<_>>(),
		[("t1", "temperature", &json!("C")), ("t2", "temperature", &json!("F"))]
	);

	let Err(Error::Interrupted(interrupted)) = step.resume(["20", "68"]).await else {
		panic!("its source is overloaded");
	};
	assert_eq!((interrupted.round, interrupted.attempt.model), (1, "claude-opus-5-5"));
	assert!(matches!(interrupted.attempt.failure, Failure::Recoverable(Recoverable::Overloaded { .. })));

	let done = step.resume(["20", "68"]).await.unwrap();
	assert_eq!((done.text.as_str(), matches!(done.stop, Stop::End)), ("20 C", true));
	assert!(other.asked().is_empty());

	let asked = server.asked();
	for a in asked.iter() {
		assert_eq!(a.path, "/v1/messages");
		assert_eq!(a.header("authorization"), Some("Bearer sk-ant-oat01-a"));
		assert_eq!(a.header("anthropic-beta"), Some("oauth-2025-04-20"));
		assert_eq!(a.header("x-api-key"), None);
		assert_eq!(a.body["system"][0]["text"], "You are Claude Code, Anthropic's official CLI for Claude.");
		assert_eq!(a.body["system"][1]["text"], "be brief");
		assert_eq!(a.body["cache_control"], json!({ "type": "ephemeral" }));
		assert_eq!(a.body["tools"][0]["input_schema"], thermometer()[0].input_schema);
	}
	assert_eq!(asked[2].body["messages"][1], json!({ "role": "assistant", "content": content }));
	assert_eq!(
		asked[2].body["messages"][2]["content"],
		json!([
			{ "type": "tool_result", "tool_use_id": "t1", "content": "20" },
			{ "type": "tool_result", "tool_use_id": "t2", "content": "68" },
		])
	);
}

#[tokio::test]
async fn a_tool_round_on_openai_and_on_ollama() {
	let openai = Server::start(vec![
		openai(json!([
			{ "type": "reasoning", "id": "rs_1", "encrypted_content": "sealed", "summary": [] },
			{ "type": "function_call", "call_id": "t1", "name": "temperature", "arguments": "{\"unit\":\"C\"}" },
		])),
		openai_text("20 C"),
	]);
	let ollama = Server::start(vec![
		(
			200,
			"",
			json!({ "model": "qwen", "message": { "role": "assistant", "content": "", "tool_calls": [
			{ "function": { "name": "temperature", "arguments": { "unit": "C" } } }, { "function": { "name": "temperature", "arguments": { "unit": "F" } } },
		] }, "done_reason": "stop", "prompt_eval_count": 5, "eval_count": 6, "load_duration": 2_000_000_000u64 }),
		),
		(
			200,
			"",
			json!({ "model": "qwen", "message": { "role": "assistant", "content": "20 C" }, "done_reason": "stop", "eval_count": 6 }),
		),
	]);

	let step = Client::default().fallback_chain([openai.openai()]).step(&question(), &thermometer()).await.unwrap();
	assert_eq!(step.resume(["20"]).await.unwrap().text, "20 C");
	let asked = openai.asked();
	assert_eq!((asked[0].path.as_str(), asked[0].header("authorization")), ("/v1/responses", Some("Bearer sk-test")));
	assert_eq!(asked[0].body["include"], json!(["reasoning.encrypted_content"]));
	let input = asked[1].body["input"].as_array().unwrap();
	assert_eq!(input[input.len() - 3]["encrypted_content"], "sealed");
	assert_eq!(input[input.len() - 2]["call_id"], "t1");
	assert_eq!(input[input.len() - 1], json!({ "type": "function_call_output", "call_id": "t1", "output": "20" }));

	let step = Client::default()
		.fallback_chain([Source::new(Provider::Ollama, "qwen").at(&ollama.origin)])
		.step(&question(), &thermometer())
		.await
		.unwrap();
	assert_eq!(step.overhead.as_secs(), 2);
	assert_eq!((step.usage.input, step.usage.output, step.cents), (5, 6, Some(0.)));
	assert_eq!(step.resume(["20", "68"]).await.unwrap().text, "20 C");
	let asked = ollama.asked();
	let results: Vec<&Value> = asked[1].body["messages"]
		.as_array()
		.unwrap()
		.iter()
		.filter(|m| m["role"] == "tool")
		.map(|m| &m["content"])
		.collect();
	assert_eq!(results, ["20", "68"]);
}

#[tokio::test]
async fn usage_is_split_and_priced_per_provider() {
	let server = Server::start(vec![claude(json!([{ "type": "text", "text": "warm" }]), "end_turn")]);
	let answer = Client::default().fallback_chain([server.claude("sk-ant-oat01-a")]).conversation(&question()).await.unwrap();
	assert_eq!((answer.usage.input, answer.usage.cache_write, answer.usage.cached_input, answer.usage.output), (10, 20, 30, 40));
	let cents = (10. * 4. + 20. * 5. + 30. * 0.2 + 40. * 20.) / 10_000.;
	assert!((answer.cents.unwrap() - cents).abs() < 1e-6, "{:?}", answer.cents);

	let server = Server::start(vec![openai_text("warm")]);
	let answer = Client::default().fallback_chain([server.openai()]).conversation(&question()).await.unwrap();
	assert_eq!((answer.usage.input, answer.usage.cache_write, answer.usage.cached_input, answer.usage.output), (30, 10, 60, 7));
}

#[tokio::test]
#[allow(deprecated)]
async fn an_unpriced_model_costs_nothing_known() {
	let server = Server::start(vec![openai_text("warm")]);
	let answer = Client::default()
		.fallback_chain([Source::new(Provider::OpenAi, "gpt-unheard-of").token("sk-test").at(&server.origin)])
		.conversation(&question())
		.await
		.unwrap();
	assert_eq!(answer.cents, None);
	assert!(answer.cost_cents.is_nan());
	assert!(answer.to_string().contains("cost: unknown"), "{answer}");
}

#[tokio::test]
async fn a_schema_is_enforced_on_every_wire() {
	let schema = json!({ "type": "object", "properties": { "warm": { "type": "boolean" } }, "required": ["warm"] });
	let claude = Server::start(vec![claude(json!([{ "type": "text", "text": "{\"warm\":true}" }]), "end_turn")]);
	let openai = Server::start(vec![openai_text("{\"warm\":true}"), openai_text("rather")]);
	let ollama = Server::start(vec![(
		200,
		"",
		json!({ "model": "qwen", "message": { "role": "assistant", "content": "{\"warm\":true}" }, "done_reason": "stop" }),
	)]);
	let client = Client::default().json_schema(schema.clone());

	for source in [claude.claude("sk-ant-oat01-a"), openai.openai(), Source::new(Provider::Ollama, "qwen").at(&ollama.origin)] {
		let answer = client.clone().fallback_chain([source]).conversation(&question()).await.unwrap();
		assert_eq!(answer.json::<Value>().unwrap()["warm"], true);
	}
	assert_eq!(claude.asked()[0].body["output_config"]["format"]["type"], "json_schema");
	assert_eq!(openai.asked()[0].body["text"]["format"]["strict"], true);
	assert_eq!(ollama.asked()[0].body["format"], schema);

	let unread = client.fallback_chain([openai.openai()]).conversation(&question()).await.unwrap().json::<Value>();
	assert!(unread.unwrap_err().to_string().contains("gpt-5.6-luna"));
}

#[tokio::test]
async fn a_refusal_is_a_step_s_answer_and_an_ask_s_error() {
	let refusal = || claude(json!([]), "refusal");
	let server = Server::start(vec![refusal(), refusal()]);
	let client = Client::default().fallback_chain([server.claude("sk-ant-oat01-a")]);

	let step = client.step(&question(), &[]).await.unwrap();
	assert!(matches!(step.stop, Stop::Refused));
	assert_eq!(step.usage.output, 40);

	let Err(Error::Unrecoverable(exhausted)) = client.conversation(&question()).await else {
		panic!("a refusal answers no ask");
	};
	assert!(matches!(exhausted.attempts[0].failure, Failure::Unrecoverable(Unrecoverable::Refused { .. })));

	let server = Server::start(vec![openai(
		json!([{ "type": "message", "role": "assistant", "content": [{ "type": "refusal", "refusal": "no" }] }]),
	)]);
	let step = Client::default().fallback_chain([server.openai()]).step(&question(), &thermometer()).await.unwrap();
	assert!(matches!(step.stop, Stop::Refused));
}

#[tokio::test]
async fn a_step_without_tools_pays_for_no_cache() {
	let claude_ = Server::start(vec![claude(json!([{ "type": "text", "text": "warm" }]), "end_turn")]);
	Client::default().fallback_chain([claude_.claude("sk-ant-oat01-a")]).step(&question(), &[]).await.unwrap();
	assert_eq!(claude_.asked()[0].body.get("cache_control"), None);

	let openai = Server::start(vec![openai_text("warm")]);
	Client::default().fallback_chain([openai.openai()]).step(&question(), &[]).await.unwrap();
	assert_eq!(openai.asked()[0].body.get("include"), None);
}

#[tokio::test]
async fn a_model_not_served_to_a_person_is_not_paged() {
	let server = Server::start(vec![claude_error(404, "not_found_error")]);
	let (levels, _guard) = events();
	let failed = Client::default().fallback_chain([server.claude("sk-ant-oat01-theirs").theirs()]).conversation(&question()).await;
	assert!(matches!(failed, Err(Error::Unrecoverable(_))), "{failed:?}");
	assert_eq!((count(&levels, tracing::Level::ERROR), count(&levels, tracing::Level::WARN)), (0, 1));
}

#[tokio::test]
async fn an_answer_is_never_in_what_its_failure_displays() {
	const MAIL: &str = "dear Ada";
	let shown = async |reply: (u16, &'static str, Value)| {
		let server = Server::start(vec![reply]);
		let failed = Client::default().force_json().fallback_chain([server.claude("sk-ant-oat01-a")]).step(&question(), &[]).await;
		let Err(Error::Unrecoverable(exhausted)) = failed else {
			panic!("{failed:?}");
		};
		let failure = &exhausted.attempts[0].failure;
		assert!(format!("{failure:?}").contains(MAIL), "it stays in the field: {failure:?}");
		failure.to_string()
	};
	let text = || json!([{ "type": "text", "text": MAIL }]);
	let mut unread = claude(text(), "end_turn");
	unread.2["usage"] = Value::Null;
	for reply in [unread, claude(text(), "end_turn"), claude(text(), "pause_turn"), (200, "", json!({ "content": MAIL }))] {
		let shown = shown(reply).await;
		assert!(!shown.contains(MAIL), "{shown}");
	}
	let refused = shown((400, "", json!({ "type": "error", "error": { "type": "invalid_request_error", "message": MAIL } }))).await;
	assert!(refused.contains(MAIL), "what a provider says of a refused request is told: {refused}");

	let server = Server::start(vec![openai_text("{\"warm\": \"dear Ada\"}")]);
	let answer = Client::default().fallback_chain([server.openai()]).conversation(&question()).await.unwrap();
	let unread = format!("{:?}", answer.json::<std::collections::HashMap<String, bool>>().unwrap_err());
	assert!(!unread.contains(MAIL), "{unread}");
}

#[tokio::test]
async fn a_billed_answer_that_is_none_tells_what_it_used() {
	let server = Server::start(vec![claude(json!([]), "max_tokens"), claude(json!([{ "type": "text", "text": "rather" }]), "end_turn")]);
	let client = Client::default().force_json().fallback_chain([server.claude("sk-ant-oat01-a")]);
	for _ in 0..2 {
		let Err(Error::Unrecoverable(exhausted)) = client.conversation(&question()).await else {
			panic!("nothing in it to use");
		};
		let (Failure::Unrecoverable(Unrecoverable::Empty { usage, .. }) | Failure::Unrecoverable(Unrecoverable::Unusable { usage, .. })) = &exhausted.attempts[0].failure else {
			panic!("{exhausted:?}");
		};
		assert_eq!(usage.output, 40);
	}
}

#[tokio::test]
async fn an_unlogged_client_writes_no_usage() {
	let logged = |model: &str| {
		let dirs = std::fs::read_dir(ISOLATED.path().join("ask_llm/usage")).unwrap();
		let files = dirs
			.flat_map(|d| std::fs::read_dir(d.unwrap().path()).unwrap())
			.map(|f| std::fs::read_to_string(f.unwrap().path()).unwrap());
		files.map(|f| f.matches(model).count()).sum::<usize>()
	};
	let server = Server::start(vec![openai_text("warm"), openai_text("warm"), openai_text("warm")]);
	let client = |model| Client::default().fallback_chain([Source::new(Provider::OpenAi, model).token("sk-test").at(&server.origin)]);
	client("gpt-counted").conversation(&question()).await.unwrap();
	client("gpt-uncounted").unlogged().conversation(&question()).await.unwrap();
	client("gpt-uncounted").unlogged().step(&question(), &[]).await.unwrap();
	assert!(logged("gpt-counted") > 0);
	assert_eq!(logged("gpt-uncounted"), 0);
}

#[test]
fn no_data_home_is_told_and_not_fatal() {
	let alone = std::process::Command::new(std::env::current_exe().unwrap())
		.args(["--ignored", "--exact", "sources::asked_without_a_data_home"])
		.env_remove("HOME")
		.output()
		.unwrap();
	let told = String::from_utf8_lossy(&alone.stdout);
	assert!(told.contains("1 passed"), "{told}");
}

#[tokio::test]
#[ignore = "`no_data_home_is_told_and_not_fatal` runs it, in a process of its own"]
async fn asked_without_a_data_home() {
	let server = Server::start(vec![openai_text("warm")]);
	// SAFETY: the only test of its process
	unsafe { std::env::remove_var("XDG_DATA_HOME") };
	let (levels, _guard) = events();
	Client::default().fallback_chain([server.openai()]).conversation(&question()).await.unwrap();
	assert_eq!(count(&levels, tracing::Level::ERROR), 1);
}

#[test]
fn a_server_can_spawn_them() {
	fn sendable<T: Send>(_: T) {}
	let client = Client::default();
	let conv = question();
	sendable(client.conversation(&conv));
	sendable(client.step(&conv, &[]));
	sendable(async {
		let step = client.step(&conv, &[]).await?;
		step.resume(["20"]).await
	});
}
