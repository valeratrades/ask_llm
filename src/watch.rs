use std::{
	path::{Path, PathBuf},
	time::Duration,
};

use eyre::{bail, eyre};
use tokio::process::Command;

use crate::{Api, Client, Conversation, Error, FileAttachment, Result, Role};

/// The scene score past which ffmpeg counts a frame as the picture changing.
const SCENE: f64 = 0.06;
/// Frames looked at in one request.
const BATCH: usize = 24;
const ATTEMPTS: u64 = 8;

/// What [`Client::watch`] is told about the recording besides the media itself.
#[derive(Clone, Debug)]
pub struct Watch {
	/// Context for the model, never parsed.
	pub title: String,
	/// What is said, where the caller already has it timed. `None` has local whisper transcribe the audio.
	pub speech: Option<Vec<Said>>,
	pub footage: Footage,
	/// Where each frame an entry cites is kept, as `<secs>.jpg`.
	pub frames: PathBuf,
}

/// How the picture moves, which decides the frames looked at.
#[derive(Clone, Copy, Debug)]
pub enum Footage {
	/// A shared screen, slides, edited video: changes in steps, and a board scrolled slowly never differs
	/// much from one frame to the next, so one is taken every 30s besides.
	Screen,
	/// A phone filming a place pans past a sign in a second or two, so every second of it is looked at.
	Filmed,
}
impl Footage {
	/// `(gap, floor)`: frames at least `gap` apart, and one `floor` after the last even if nothing changed.
	fn pace(self) -> (f64, u64) {
		match self {
			Self::Screen => (4., 30),
			Self::Filmed => (1., 1),
		}
	}
}

#[derive(Clone, Debug)]
pub struct Said {
	pub secs: f64,
	pub text: String,
}

#[derive(Clone, Debug)]
pub struct Shown {
	/// The second of the frame this was read off.
	pub secs: u64,
	pub shown: String,
	/// Legible text, copied as written.
	pub on_screen_text: Option<String>,
	pub frame: PathBuf,
}

#[derive(Debug)]
pub struct Watched {
	pub speech: Vec<Said>,
	pub shown: Vec<Shown>,
	pub frames_read: usize,
	pub cost_cents: f32,
	/// `None` where there was no picture, so nothing was asked.
	pub model: Option<String>,
}

impl Client {
	/// Read a recording into timed text: what is said, and what is shown that the speech does not say.
	///
	/// Takes anything ffmpeg decodes. Frames are taken where the picture changes and read with the
	/// client's model, which has to take images:
	/// ```no_run
	/// # async fn f() -> ask_llm::Result<()> {
	/// use ask_llm::{Client, Footage, Model, Watch};
	/// let watched = Client::default()
	/// 	.model(Model::Video)
	/// 	.watch("call.mp4", Watch { title: "setup call".into(), speech: None, footage: Footage::Screen, frames: "call/frames".into() })
	/// 	.await?;
	/// # Ok(()) }
	/// ```
	pub async fn watch(&self, media: impl AsRef<Path>, spec: Watch) -> Result<Watched> {
		let media = media.as_ref();
		if !self.files.is_empty() {
			return Err(eyre!("`watch` sends the recording's frames and nothing else; drop the attached files").into());
		}
		crate::transcribe::preflight("ffprobe").await?;
		let speech = match spec.speech {
			Some(speech) => speech,
			None if has_stream(media, "a").await? => crate::transcribe::said(media).await?,
			None => Vec::new(),
		};
		let mut watched = Watched {
			speech,
			shown: Vec::new(),
			frames_read: 0,
			cost_cents: 0.,
			model: None,
		};
		if !has_stream(media, "v").await? {
			return Ok(watched);
		}

		let (gap, floor) = spec.footage.pace();
		let scratch = tempfile::tempdir().map_err(|e| eyre!(e))?;
		let frames = changes(media, scratch.path(), gap, floor).await?;
		watched.frames_read = frames.len();
		std::fs::create_dir_all(&spec.frames).map_err(|e| eyre!("{}: {e}", spec.frames.display()))?;

		for (i, batch) in frames.chunks(BATCH).enumerate() {
			let from = batch[0].0;
			let to = frames.get((i + 1) * BATCH).map_or(u64::MAX, |f| f.0);
			let around: String = watched
				.speech
				.iter()
				.enumerate()
				.filter(|(i, s)| (s.secs as u64) < to && watched.speech.get(i + 1).is_none_or(|next| next.secs as u64 > from))
				.map(|(_, s)| format!("{} {}\n", stamp(s.secs as u64), s.text))
				.collect();
			let times: Vec<String> = batch.iter().map(|(t, _)| format!("{} ({t})", stamp(*t))).collect();
			let prompt = format!(
				"The {} images are frames of the recording \"{}\", in order at these times (seconds in brackets): {}.\n\
				 Each was taken where the picture changed, or {floor}s after the last. Each carries its second in the band under it.\n\
				 List what the frames show that the speech does not already say: screen content, UI states, numbers on screen, physical scene details. \
				 The people talking are never entries: webcams, call tiles, a talking head, their names, how many there are, and their joining, leaving, or turning a camera on or off. \
				 What is shared, shown or filmed is: a shared screen, a slide, a document, a dashboard, a site, a phone screen, a place.\n\
				 Answer {{\"entries\": [{{\"t_secs\": <the seconds printed under the one frame it is read off>, \"shown\": <one sentence>, \"on_screen_text\": <legible text copied as written, where it carries something, else omitted>}}]}}. \
				 A frame that shows nothing new gets no entry.\n\n{}",
				batch.len(),
				spec.title,
				times.join(", "),
				match around.trim().is_empty() {
					true => "Nobody speaks around these frames.".to_string(),
					false => format!("What is said around them, already transcribed:\n\n{around}"),
				}
			);
			let files = batch.iter().map(|(_, jpg)| jpeg(jpg)).collect::<eyre::Result<Vec<_>>>()?;
			let answer = self.patiently(&prompt, &files).await?;
			watched.cost_cents += answer.cost_cents;
			watched.model.get_or_insert(answer.model.clone());

			let listed: Answer = serde_json::from_str(&answer.text).map_err(|e| eyre!("{}: the answer is not the json asked for: {e}\n{}", answer.model, answer.text))?;
			for Entry { t_secs, shown, on_screen_text } in listed.entries {
				let Some((_, jpg)) = batch.iter().find(|(t, _)| *t == t_secs) else {
					return Err(eyre!("{}: an entry at {t_secs}s, which is no frame it was shown — {shown}", answer.model).into());
				};
				let frame = spec.frames.join(format!("{t_secs}.jpg"));
				std::fs::copy(jpg, &frame).map_err(|e| eyre!("{}: {e}", frame.display()))?;
				watched.shown.push(Shown {
					secs: t_secs,
					shown: shown.trim().to_string(),
					on_screen_text: on_screen_text.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
					frame,
				});
			}
		}
		watched.shown.sort_by_key(|s| s.secs);
		Ok(watched)
	}

	/// A read of many requests must not lose what it already spent to one 429, so the busy provider is waited out.
	async fn patiently(&self, prompt: &str, files: &[FileAttachment]) -> Result<crate::Response> {
		let mut conv = Conversation::new();
		conv.add(Role::User, prompt);
		//LOOP: bounded by the attempts
		for attempt in 1..=ATTEMPTS {
			let wait = match self.send(&conv, files, true).await {
				Err(Error::Api(Api::RateLimited { retry_after, .. })) if attempt < ATTEMPTS => retry_after.unwrap_or(Duration::from_secs(30 * attempt)),
				Err(Error::Api(Api::Overloaded { .. })) if attempt < ATTEMPTS => Duration::from_secs(30 * attempt),
				done => return done,
			};
			tracing::warn!(attempt, wait_secs = wait.as_secs(), "provider busy, waiting");
			tokio::time::sleep(wait).await;
		}
		unreachable!("the last attempt returns")
	}
}

#[derive(serde::Deserialize)]
struct Answer {
	entries: Vec<Entry>,
}
#[derive(serde::Deserialize)]
struct Entry {
	t_secs: u64,
	shown: String,
	on_screen_text: Option<String>,
}

fn jpeg(path: &Path) -> eyre::Result<FileAttachment> {
	let data = std::fs::read(path)?;
	Ok(FileAttachment {
		base64_data: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data),
		media_type: "image/jpeg".to_string(),
	})
}

/// `v` or `a`. Cover art rides in a video stream, and is no picture.
async fn has_stream(media: &Path, kind: &str) -> eyre::Result<bool> {
	let out = Command::new("ffprobe")
		.args([
			"-v",
			"error",
			"-select_streams",
			kind,
			"-show_entries",
			"stream=index:stream_disposition=attached_pic",
			"-of",
			"csv=p=0",
		])
		.arg(media)
		.output()
		.await?;
	if !out.status.success() {
		bail!("ffprobe could not read {}: {}", media.display(), String::from_utf8_lossy(&out.stderr).trim());
	}
	Ok(String::from_utf8(out.stdout)?.lines().any(|l| !l.trim().is_empty() && !l.ends_with(",1")))
}

/// `(secs, jpg)` for every frame where the picture changes or `floor` seconds have passed, at least
/// `gap` seconds apart, the first frame always among them.
async fn changes(media: &Path, scratch: &Path, gap: f64, floor: u64) -> eyre::Result<Vec<(u64, PathBuf)>> {
	let out = Command::new("ffmpeg")
		.args(["-hide_banner", "-nostats", "-nostdin", "-i"])
		.arg(media)
		.args([
			"-an",
			"-vf",
			&format!("select='eq(n\\,0)+gt(scene\\,{SCENE})+gte(t-prev_selected_t\\,{floor})',showinfo,scale=-2:'min(720\\,ih)',pad=iw:ih+40:0:0:black,drawtext=text='%{{eif\\:t\\:d}}s':x=10:y=h-32:fontsize=26:fontcolor=white"),
			"-fps_mode",
			"passthrough", // `vfr` drops a frame that shares its stamp with the one before, and showinfo still names it
			"-q:v",
			"4",
		])
		.arg(scratch.join("%06d.jpg"))
		.output()
		.await?;
	let log = String::from_utf8_lossy(&out.stderr);
	if !out.status.success() {
		bail!("ffmpeg could not read the frames of {}: {log}", media.display());
	}
	let mut kept: Vec<(u64, PathBuf)> = Vec::new();
	let mut last = f64::NEG_INFINITY;
	for (i, rest) in log
		.lines()
		.filter(|l| l.contains("Parsed_showinfo"))
		.filter_map(|l| l.split_once(" pts_time:").map(|(_, r)| r))
		.enumerate()
	{
		let t: f64 = rest
			.split_whitespace()
			.next()
			.expect("split yields at least once")
			.parse()
			.map_err(|e| eyre!("showinfo wrote `{rest}`: {e}"))?;
		let jpg = scratch.join(format!("{:06}.jpg", i + 1));
		if !jpg.exists() {
			bail!("showinfo named a frame ffmpeg did not write: {}", jpg.display());
		}
		if t - last >= gap {
			kept.push((t as u64, jpg));
			last = t;
		}
	}
	if kept.is_empty() {
		bail!("{}: a picture with no first frame", media.display());
	}
	Ok(kept)
}

/// `MM:SS` under an hour, `H:MM:SS` past it.
fn stamp(secs: u64) -> String {
	match secs / 3600 {
		0 => format!("{:02}:{:02}", secs / 60, secs % 60),
		h => format!("{h}:{:02}:{:02}", (secs % 3600) / 60, secs % 60),
	}
}
