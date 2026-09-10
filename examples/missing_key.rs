//! What a request reports when the selected model's provider was never given a key.
use ask_llm::{Client, Error, Model, config::AppConfig};

#[tokio::main]
async fn main() {
	let err = Client::new(AppConfig::default()) // an empty config, and no builder token
		.model(Model::Fast)
		.ask("anything")
		.await
		.expect_err("Model::Fast is served by OpenAI, whose key is absent here");
	assert!(matches!(err, Error::MissingToken(_)), "a keyless request fails on the key, got {err:?}");
	println!("{:?}", miette::Report::new(err));

	// Claude never reports MissingToken: the `claude` CLI owns credential resolution, so a bogus token
	// either loses to the CLI's own login or is refused by Anthropic. Either way nothing here goes
	// looking for a key, so neither outcome can be MissingToken.
	match Client::default()
		.claude_token("sk-ant-oat01-not-a-real-token")
		.model(Model::Slow)
		.ask("Reply with the single word: ok")
		.await
	{
		Ok(response) => println!("the CLI resolved its own credentials and answered {:?} {response}", response.text),
		Err(refused) => {
			assert!(!matches!(refused, Error::MissingToken(_)), "past key resolution, this is the provider talking");
			println!("{:?}", miette::Report::new(refused));
		}
	}
}
