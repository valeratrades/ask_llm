use std::path::PathBuf;

use ask_llm::{
	Client, Footage, Model, Watch,
	config::{AppConfig, SettingsFlags},
};
use clap::{Parser, ValueEnum};

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
	#[clap(required_unless_present_any = ["transcribe", "watch"])]
	question: Option<String>,
	/// Transcribe an audio file locally instead of asking a question
	#[clap(short, long, value_name = "AUDIO")]
	transcribe: Option<PathBuf>,
	/// Read a recording into timed lines: what is said, and what is shown
	#[clap(short, long, value_name = "MEDIA", requires_all = ["footage", "frames"])]
	watch: Option<PathBuf>,
	/// How the picture of `--watch` moves
	#[clap(long)]
	footage: Option<FootageArg>,
	/// Where `--watch` keeps the frames it cites
	#[clap(long, value_name = "DIR")]
	frames: Option<PathBuf>,
	/// `medium` for a question, `video` for `--watch`
	#[clap(short, long)]
	model: Option<Model>,
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

	if let Some(media) = cli.watch {
		let spec = Watch {
			title: media.file_stem().expect("a media file has a name").to_string_lossy().into_owned(),
			speech: None,
			footage: match cli.footage.expect("clap requires it with --watch") {
				FootageArg::Screen => Footage::Screen,
				FootageArg::Filmed => Footage::Filmed,
			},
			frames: cli.frames.expect("clap requires it with --watch"),
		};
		let watched = Client::new(config).model(cli.model.unwrap_or(Model::Video)).watch(&media, spec).await?;
		let mut lines: Vec<(u64, String)> = watched.speech.iter().map(|s| (s.secs as u64, s.text.clone())).collect();
		lines.extend(watched.shown.iter().map(|s| {
			let text = s.on_screen_text.as_ref().map(|t| format!("\n\t> {}", t.replace('\n', " / "))).unwrap_or_default();
			(s.secs, format!("[shown] {}{text}\n\t{}", s.shown, s.frame.display()))
		}));
		lines.sort_by_key(|l| l.0);
		for (secs, line) in lines {
			println!("{:02}:{:02}:{:02} {line}", secs / 3600, secs % 3600 / 60, secs % 60);
		}
		eprintln!(
			"{} frames read by {}, {:.4}¢",
			watched.frames_read,
			watched.model.as_deref().unwrap_or("nothing"),
			watched.cost_cents
		);
		return Ok(());
	}

	let mut client = Client::new(config).model(cli.model.unwrap_or(Model::Medium));
	if cli.fast {
		client = client.max_tokens(4096);
	}
	let answer: String = client.ask(cli.question.expect("clap requires it unless --transcribe or --watch")).await?.text;

	println!("{answer:#}");
	Ok(())
}
#[derive(Clone, Copy, Debug, ValueEnum)]
enum FootageArg {
	Screen,
	Filmed,
}
