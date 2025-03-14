use ask_llm::*;
use clap::Parser;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
	question: String,
	#[clap(short, long, default_value = "medium")]
	model: ask_llm::Model,
	/// If true, will avoid streaming (caps response at 4096 tokens)
	#[clap(short, long)]
	fast: bool,
}

#[tokio::main]
async fn main() {
	v_utils::clientside!();
	let cli = Cli::parse();

	let mut conv = Conversation::new();
	conv.add(Role::User, cli.question);
	let max_tokens = if cli.fast { Some(4096) } else { None };
	let answer = conversation(&conv, cli.model, max_tokens, None).await.unwrap().text;

	println!("{answer:#}");
}
