use ask_llm::{Client, Model};
use base64::Engine;

#[tokio::main]
async fn main() {
	v_utils::clientside!();

	// Example 1: Attach inline base64 content
	let test_content = "Hello, this is a test document.\nIt has multiple lines.\nLine 3 here.";
	let base64_data = base64::engine::general_purpose::STANDARD.encode(test_content.as_bytes());

	let response = Client::new()
		.model(Model::Fast)
		.max_tokens(100)
		.append_file(base64_data, "text/plain".to_string())
		.ask("How many lines are in the attached document?")
		.await
		.unwrap();
	println!("Inline attachment response:\n{:#?}\n", response);

	// Example 2: Attach file from path
	// Uncomment and modify path to test with a local file:
	// let response = Client::new()
	//     .model(Model::Fast)
	//     .max_tokens(100)
	//     .append_file_from_path("/path/to/file.txt")
	//     .unwrap()
	//     .ask("Summarize the document")
	//     .await
	//     .unwrap();
	// println!("File path attachment response:\n{:#?}", response);
}
