//! The Claude backend shells out to the `claude` CLI so that it bills the Max subscription. A child that
//! inherits `ANTHROPIC_API_KEY` would silently bill pay-as-you-go credits instead, so the scrub and the
//! round trip are one invariant: an answer comes back, and it comes back with a poisoned key in the environment.

#[tokio::test]
async fn answers_without_an_api_key() {
	// SAFETY: set before the runtime spawns anything that reads the environment
	unsafe { std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-api03-not-a-real-key") };

	let response = ask_llm::Client::default()
		.model(ask_llm::Model::Slow)
		.ask("Reply with the single word: pong")
		.await
		.expect("the cli path answers, and answers without the poisoned key");

	assert!(response.text.to_lowercase().contains("pong"), "expected `pong`, got: {}", response.text);
}
