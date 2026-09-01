//! What a request reports when the selected model's provider was never given a key.
use ask_llm::{Client, MissingToken, Model, config::AppConfig};

#[tokio::main]
async fn main() {
	let err = Client::new(AppConfig::default()) // an empty config, and no builder token
		.model(Model::Slow)
		.ask("anything")
		.await
		.expect_err("Model::Slow is served by Anthropic, whose key is absent here");
	let missing: MissingToken = err.downcast().expect("a keyless request fails on the key");
	println!("{:?}", miette::Report::new(missing));

	// a key that exists but is refused is a different failure, and must not read as an empty answer
	let refused = Client::default()
		.claude_token("sk-not-a-real-key")
		.model(Model::Slow)
		.ask("anything")
		.await
		.expect_err("Anthropic rejects a bogus key");
	assert!(refused.downcast_ref::<MissingToken>().is_none(), "past key resolution, this is the provider talking");
	println!("{refused:#}");
}
