//! A tier whose entry provider was never given a key climbs to the next node instead of failing the call.
use ask_llm::{Client, Model, config::AppConfig};

#[tokio::main]
async fn main() {
	// SAFETY: set before the runtime spawns anything that reads the environment
	unsafe { std::env::remove_var("OPENAI_API_KEY") };
	let response = Client::new(AppConfig::default()) // an empty config, and no builder token
		.model(Model::Fast)
		.ask("Reply with the single word: pong")
		.await
		.unwrap_or_else(|e| panic!("{:?}", miette::Report::new(e)));
	assert_eq!(response.model, "claude-sonnet-5", "Luna has no key, so Sonnet 5 is the next node up");
	println!("{:?} {response}", response.text);
}
