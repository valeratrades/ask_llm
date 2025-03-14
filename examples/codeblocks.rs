use ask_llm::Model;

#[tokio::main]
async fn main() {
	v_utils::clientside!();

	let extensionless_codeblock_resp = ask_llm::oneshot("Translate ```How do you do``` to German. Return translation inside a codeblock.", Model::Fast)
		.await
		.unwrap();
	println!("{:#?}", extensionless_codeblock_resp.extract_codeblock(None));

	let py_codeblock_resp = ask_llm::oneshot("How to print hello world in python", Model::Fast).await.unwrap();
	println!("{:#?}", py_codeblock_resp.extract_codeblocks(Some(vec!["python", "py"])));
}

#[cfg(test)]
#[test]
fn test_main() {
	main();
}
