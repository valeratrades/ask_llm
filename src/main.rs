use ask_llm::{Client, config};
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
	#[command(flatten)]
	settings: config::SettingsFlags,
}

#[tokio::main]
async fn main() {
	v_utils::clientside!();
	let cli = Cli::parse();

	let _ = config::init(cli.settings);

	let mut client = Client::new().model(cli.model);
	if cli.fast {
		client = client.max_tokens(4096);
	}
	let answer = client.ask(cli.question).await.unwrap().text;

	println!("{answer:#}");
}
