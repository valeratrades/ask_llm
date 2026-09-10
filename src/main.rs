use std::path::PathBuf;

use ask_llm::{
	Client, Model,
	config::{AppConfig, SettingsFlags},
};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
	#[clap(required_unless_present = "transcribe")]
	question: Option<String>,
	/// Transcribe an audio file locally instead of asking a question
	#[clap(short, long, value_name = "AUDIO")]
	transcribe: Option<PathBuf>,
	#[clap(short, long, default_value = "medium")]
	model: Model,
	/// If true, will avoid streaming (caps response at 4096 tokens)
	#[clap(short, long)]
	fast: bool,
	#[command(flatten)]
	settings: SettingsFlags,
}
/// [miette::Report] rather than a `.unwrap()`: a `thiserror` enum unwraps into its `Debug`, which drops the
/// `help` naming the fix.
#[tokio::main]
async fn main() -> miette::Result<()> {
	v_utils::clientside!();
	let cli = Cli::parse();

	if let Some(audio) = cli.transcribe {
		println!("{}", ask_llm::transcribe(audio).await.unwrap());
		return Ok(());
	}

	let config = AppConfig::try_build(cli.settings).expect("Failed to build config");

	let mut client = Client::new(config).model(cli.model);
	if cli.fast {
		client = client.max_tokens(4096);
	}
	let answer: String = client.ask(cli.question.expect("clap requires it unless --transcribe")).await?.text;

	println!("{answer:#}");
	Ok(())
}
