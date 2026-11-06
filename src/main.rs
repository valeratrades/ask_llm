use std::path::PathBuf;

use ask_llm::{
	Client, Footage, Model, Pick, Watch,
	config::{AppConfig, SettingsFlags},
	providers::claude::accounts::Accounts,
};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None, subcommand_negates_reqs = true)]
struct Cli {
	#[command(subcommand)]
	dev: Option<Dev>,
	#[clap(required_unless_present_any = ["transcribe", "watch"])]
	question: Option<String>,
	/// Transcribe an audio file locally instead of asking a question
	#[clap(short, long, value_name = "AUDIO")]
	transcribe: Option<PathBuf>,
	/// Read a recording into timed lines: what is said, and what is shown
	#[clap(short, long, value_name = "MEDIA", requires_all = ["footage", "pick", "frames"])]
	watch: Option<PathBuf>,
	/// How the picture of `--watch` moves
	#[clap(long)]
	footage: Option<FootageArg>,
	/// Where `--watch` takes frames from
	#[clap(long)]
	pick: Option<PickArg>,
	/// Least seconds between frames under `--pick likely`
	#[clap(long, default_value_t = 0.5)]
	every: f64,
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

#[derive(Debug, Subcommand)]
enum Dev {
	/// Machine plumbing for other tools on this device
	#[command(hide = true, subcommand)]
	Dev(DevCommand),
}

#[derive(Debug, Subcommand)]
enum DevCommand {
	/// The device's Claude logins, `~/.claude/accounts`
	#[command(subcommand)]
	ClaudeAccount(ClaudeAccount),
}
#[derive(Debug, Subcommand)]
enum ClaudeAccount {
	/// Resolved dir of the account a new claude starts on
	Current,
	/// Resolved dir of the account every running claude follows
	Running,
	/// Point `current` and `running` at another account: new claudes start on it, running ones move onto it
	Rotate {
		name: String,
		/// Point only `current`: running claudes stay where they are, keeping their prompt caches
		#[clap(long)]
		new: bool,
	},
	/// `name\tlabel` per account, `*` on the current one; `--json`: one object per line
	List {
		#[clap(long)]
		json: bool,
	},
}
/// [miette::Report] rather than a `.unwrap()`: a `thiserror` enum unwraps into its `Debug`, which drops the
/// `help` naming the fix.
#[tokio::main]
async fn main() -> miette::Result<()> {
	let cli = Cli::parse();
	// before `clientside!`, whose logs go to stdout, which these are read from
	if let Some(Dev::Dev(DevCommand::ClaudeAccount(command))) = cli.dev {
		return claude_account(command).await.map_err(|e| miette::miette!("{e:?}"));
	}
	v_utils::clientside!();

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
			pick: match cli.pick.expect("clap requires it with --watch") {
				PickArg::Changes => Pick::Changes,
				PickArg::Likely => Pick::Likely { every: cli.every },
			},
			about: None,
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
			"{} frames read by {}, picked by {}, {:.4}¢",
			watched.frames_read,
			watched.model.as_deref().unwrap_or("nothing"),
			watched.picked_by.as_deref().unwrap_or("change alone"),
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
async fn claude_account(command: ClaudeAccount) -> eyre::Result<()> {
	let accounts = Accounts::home();
	match command {
		ClaudeAccount::Current => println!("{}", accounts.current()?.display()),
		ClaudeAccount::Running => println!("{}", accounts.running()?.display()),
		ClaudeAccount::Rotate { name, new } => {
			let rotated = if new { accounts.rotate_new(&name)? } else { accounts.rotate(&name)? };
			if !rotated.logged_in {
				eprintln!("account {name} has no login yet: running claudes stay where they are; /login in a new one adds it");
			}
		}
		ClaudeAccount::List { json } =>
			for account in accounts.list().await? {
				if json {
					println!(
						"{}",
						serde_json::json!({ "name": account.name, "email": account.email(), "dir": account.dir, "current": account.current, "label": account.to_string(), "disabled": account.disabled() })
					);
				} else {
					println!("{}\t{}{account}", account.name, if account.current { "* " } else { "" });
				}
			},
	}
	Ok(())
}
#[derive(Clone, Copy, Debug, ValueEnum)]
enum FootageArg {
	Screen,
	Filmed,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
enum PickArg {
	Changes,
	Likely,
}
