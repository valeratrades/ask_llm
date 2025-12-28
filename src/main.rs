use ask_llm::{
	Client, Model,
	config::{AppConfig, SettingsFlags},
};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
	question: String,
	#[clap(short, long, default_value = "medium")]
	model: Model,
	/// If true, will avoid streaming (caps response at 4096 tokens)
	#[clap(short, long)]
	fast: bool,
	#[command(flatten)]
	settings: SettingsFlags,
}

#[tokio::main]
async fn main() {
	v_utils::clientside!();
	let cli = Cli::parse();

	let config = AppConfig::try_build(cli.settings).expect("Failed to build config");

	let mut client = Client::with_config(config).model(cli.model);
	if cli.fast {
		client = client.max_tokens(4096);
	}
	let answer = client.ask(cli.question).await.unwrap().text;

	println!("{answer:#}");
}
