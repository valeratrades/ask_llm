use ask_llm::{Client, Model};

const PARAGRAPHS: &str = "\
The rapid advancement of artificial intelligence has transformed numerous industries, from healthcare \
to finance. Machine learning models can now diagnose diseases with remarkable accuracy, predict market \
trends, and even generate creative content that rivals human output. Yet these capabilities come with \
significant ethical considerations that society must address.

Perhaps the most pressing concern is the displacement of human workers. As AI systems become more \
capable, many traditional jobs face automation. However, history suggests that technological revolutions \
ultimately create more opportunities than they destroy, provided that education systems adapt to prepare \
workers for the new landscape of employment.";

#[tokio::main]
async fn main() {
	v_utils::clientside!();

	let response = Client::new()
		.model(Model::Fast)
		.ask(format!(
			"Translate the following English text to German. Output ONLY the translation, nothing else.\n\n{PARAGRAPHS}"
		))
		.await
		.unwrap();

	println!("=== Original ===\n{PARAGRAPHS}\n");
	println!("=== German Translation (Qwen 3.5 9B, local) ===\n{}", response.text);
	println!("\n[cost: {:.4}¢]", response.cost_cents);
}
