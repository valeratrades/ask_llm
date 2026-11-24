//! Live: `cargo r --example step -- <claude|openai>`. A two-round tool loop, then an answer held to a schema. Claude goes over
//! http on the device login's access token, while that lives.
use ask_llm::{
	Client, Conversation, Role, Source, Stop, ThinkingLevel, Tool,
	providers::{claude::ClaudeModel, openai::OpenAiModel},
};
use serde_json::json;

#[tokio::main]
async fn main() {
	v_utils::clientside!();
	let source = match std::env::args().nth(1).as_deref() {
		Some("claude") => Source::from(ClaudeModel::Sonnet5_5),
		Some("openai") => Source::from(OpenAiModel::Luna),
		other => panic!("claude or openai, not {other:?}"),
	};
	let client = Client::default().fallback_chain([source]).thinking(ThinkingLevel::High).max_tokens(4000);
	let tools = [
		tool(["drawer_of", "Which drawer a thing is in.", "thing"]),
		tool(["open", "What is written inside a drawer.", "drawer"]),
	];
	// past what the models cache at the least
	let house: String = (0..400).map(|n| format!("Drawer {n} is closed, and is not to be guessed at.\n")).collect();
	let mut conv = Conversation::new_with_system(house);
	conv.add(Role::User, "The brass key weighs 17 times 23 grams, the iron one 19 times 21, and the right one is the prime-factored 13 times 30 plus 1. Work out which is right, find its drawer, then open it, and answer with the word written there alone.");

	let mut step = client.step(&conv, &tools).await.unwrap_or_else(|e| panic!("{:?}", miette::Report::new(e)));
	let mut rounds = 0;
	while let Stop::Tools(calls) = &step.stop {
		let results: Vec<&str> = calls
			.iter()
			.map(|c| match c.name.as_str() {
				"drawer_of" => "drawer 17",
				"open" => "the word is: heliotrope",
				other => panic!("no tool {other}"),
			})
			.collect();
		rounds += 1;
		println!("round {rounds}: {calls:?}\n{:?} {:?}¢", step.usage, step.cents);
		step = step.resume(results).await.unwrap_or_else(|e| panic!("{:?}", miette::Report::new(e)));
	}
	println!("{:?} {:?} {:?}\n{:?} {:?}¢", step.model, step.stop, step.text, step.usage, step.cents);
	assert!(step.text.contains("heliotrope"));
	assert!(rounds >= 2);

	let schema = json!({ "type": "object", "properties": { "capital": { "type": "string" } }, "required": ["capital"] });
	let mut conv = Conversation::new();
	conv.add(Role::User, "The capital of France?");
	let step = client.json_schema(schema).step(&conv, &[]).await.unwrap_or_else(|e| panic!("{:?}", miette::Report::new(e)));
	assert_eq!(step.json::<serde_json::Value>().unwrap(), json!({ "capital": "Paris" }));
	println!("{} {:?} {:?}¢", step.text, step.usage, step.cents);
}

fn tool([name, description, argument]: [&str; 3]) -> Tool {
	Tool {
		name: name.into(),
		description: description.into(),
		input_schema: json!({ "type": "object", "properties": { argument: { "type": "string" } }, "required": [argument] }),
	}
}
