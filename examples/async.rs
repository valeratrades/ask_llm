use ask_llm::{Client, Model};

#[tokio::main]
async fn main() {
	v_utils::clientside!();

	// Simple oneshot
	let oneshot_response = ask_llm::oneshot("Speak now").await.unwrap();
	println!("{oneshot_response:#?}");

	// With options
	let response = Client::new().model(Model::Fast).max_tokens(10).stop_sequences(vec![";"]).ask("What day is today?").await.unwrap();
	println!("{response:#?}");
}

#[cfg(test)]
#[test]
fn test_main() {
	main();
}
