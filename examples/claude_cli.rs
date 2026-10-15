//! Two live round trips through the `claude` CLI, billed to the subscription. The backend shells out so that it bills
//! the Max subscription; a child that inherited `ANTHROPIC_API_KEY` would silently bill credits instead, so an answer
//! must come back with a poisoned key in the environment.
use base64::Engine as _;

fn main() {
	// SAFETY: set before the runtime exists, so nothing reads the environment concurrently
	unsafe { std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-api03-not-a-real-key") };
	tokio::runtime::Runtime::new().unwrap().block_on(async {
		let response = ask_llm::Client::default()
			.model(ask_llm::Model::Slow)
			.ask("Reply with the single word: pong")
			.await
			.expect("the cli path answers, and answers without the poisoned key");
		assert!(response.text.to_lowercase().contains("pong"), "expected `pong`, got: {}", response.text);

		let red = base64::engine::general_purpose::STANDARD.encode(include_bytes!("red.png"));
		let response = ask_llm::Client::default()
			.model(ask_llm::Model::Medium)
			.append_file(red, "image/png".into())
			.ask("What single colour fills the attached image? One word.")
			.await
			.expect("the cli path takes the image as a content block");
		assert!(response.text.to_lowercase().contains("red"), "expected `red`, got: {}", response.text);
	});
	println!("ok");
}
