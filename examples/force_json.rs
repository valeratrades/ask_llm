use ask_llm::{Client, Model};

#[tokio::main]
async fn main() {
	v_utils::clientside!();

	let response = Client::new()
		.model(Model::Fast)
		.max_tokens(100)
		.force_json()
		.ask("What are the first 3 prime numbers? Return as JSON with a 'primes' array.")
		.await
		.unwrap();

	println!("Response: {}", response.text);

	// Verify it's valid JSON
	let parsed: serde_json::Value = serde_json::from_str(&response.text).expect("Response should be valid JSON");
	println!("Parsed: {:#?}", parsed);
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn test_force_json() {
		let response = Client::new()
			.model(Model::Fast)
			.max_tokens(100)
			.force_json()
			.ask("Return a JSON object with 'name' set to 'test' and 'value' set to 42.")
			.await
			.unwrap();

		let parsed: serde_json::Value = serde_json::from_str(&response.text).expect("Response should be valid JSON");
		assert!(parsed.is_object(), "Response should be a JSON object");
	}
}
