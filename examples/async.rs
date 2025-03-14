use ask_llm::{Conversation, Model, Role};

#[tokio::main]
async fn main() {
	v_utils::clientside!();

	let oneshot_response = ask_llm::oneshot("Speak now", Model::Fast).await.unwrap();
	println!("{:#?}", oneshot_response);

	let mut conv = Conversation::new_with_system("Today is January 1, 1950");
	conv.add(Role::User, "What day is today?");
	let conv_response = ask_llm::conversation(&conv, Model::Fast, Some(10), Some(vec![";"])).await.unwrap();
	println!("{:#?}", conv_response);
}

#[cfg(test)]
#[test]
fn test_main() {
	main();
}
