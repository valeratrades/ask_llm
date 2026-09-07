//! What a request reports when the selected model's provider was never given a key.
use ask_llm::{Client, MissingToken, Model, config::AppConfig};

#[tokio::main]
async fn main() {
	let err = Client::new(AppConfig::default()) // an empty config, and no builder token
		.model(Model::Fast)
		.ask("anything")
		.await
		.expect_err("Model::Fast is served by OpenAI, whose key is absent here");
	let missing: MissingToken = err.downcast().expect("a keyless request fails on the key");
	println!("{:?}", miette::Report::new(missing));

	// Claude never reports MissingToken: the `claude` CLI owns credential resolution. A token that is
	// present but refused is the provider talking, and must not read as an empty answer.
	let refused = Client::default()
		.claude_token("sk-ant-oat01-not-a-real-token")
		.model(Model::Slow)
		.ask("anything")
		.await
		.expect_err("Anthropic rejects a bogus token");
	assert!(refused.downcast_ref::<MissingToken>().is_none(), "past key resolution, this is the provider talking");
	println!("{refused:#}");
}
