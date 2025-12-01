use ask_llm::{Client, Model};

#[tokio::main]
async fn main() {
	v_utils::clientside!();

	let extensionless_codeblock_resp = Client::new()
		.model(Model::Fast)
		.ask("Translate ```How do you do``` to German. Return translation inside a codeblock.")
		.await
		.unwrap();
	println!("{:#?}", extensionless_codeblock_resp.extract_codeblock(None));

	let py_codeblock_resp = Client::new().model(Model::Fast).ask("How to print hello world in python").await.unwrap();
	println!("{:#?}", py_codeblock_resp.extract_codeblocks(Some(vec!["python", "py"])));
}

#[cfg(test)]
#[test]
fn test_main() {
	main();
}
