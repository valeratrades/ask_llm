//! `watch` is frames chosen by ffmpeg, read by a remote model, and handed back at the second they were
//! read off. Only a live round trip covers the three together.

use std::{path::Path, process::Command};

use ask_llm::{Client, Footage, Model, Pick, Said, Watch};

#[tokio::test]
#[ignore = "spends a live OpenAI round trip"]
async fn reads_what_the_picture_says_at_its_second() {
	let dir = tempfile::tempdir().unwrap();
	let clip = dir.path().join("clip.mp4");
	invoice_then_refund(&clip, 6);
	check(&clip, &dir.path().join("frames"), None, Pick::Changes, &[(0, "4417"), (6, "9203")], &[]).await;
}

#[tokio::test]
#[ignore = "spends a live OpenAI round trip"]
async fn reads_only_where_the_speech_says_something_is_shown() {
	let dir = tempfile::tempdir().unwrap();
	let clip = dir.path().join("clip.mp4");
	invoice_then_refund(&clip, 40);
	let said = [
		(0., "Hi everyone, thanks for joining. Let me tell you a bit about how we started."),
		(12., "We were three friends back then, working out of a garage, no clients at all."),
		(25., "Anyway, that is enough about us and the old days."),
		(40., "Now let me pull up on my screen what we sent the client last week."),
		(60., "So that is what went out, and that is all for today."),
	];
	let speech = said.map(|(secs, text)| Said { secs, text: text.into() }).to_vec();
	check(&clip, &dir.path().join("frames"), Some(speech), Pick::Likely { every: 0.5 }, &[(40, "9203")], &["4417"]).await;
}

#[tokio::test]
#[ignore = "spends a live OpenAI round trip"]
async fn reads_nothing_where_the_speech_shows_nothing() {
	let dir = tempfile::tempdir().unwrap();
	let clip = dir.path().join("clip.mp4");
	invoice_then_refund(&clip, 20);
	let said = [
		(0., "Hi everyone, thanks for joining. Let me tell you a bit about how we started."),
		(15., "We were three friends back then, working out of a garage, no clients at all."),
		(30., "That is all from me today, thanks for listening, see you next week."),
	];
	let watched = Client::default()
		.model(Model::Video)
		.watch(
			&clip,
			Watch {
				title: "how we started".into(),
				speech: Some(said.map(|(secs, text)| Said { secs, text: text.into() }).to_vec()),
				footage: Footage::Screen,
				pick: Pick::Likely { every: 0.5 },
				about: None,
				frames: dir.path().join("frames"),
			},
		)
		.await
		.unwrap();
	assert_eq!(
		(watched.frames_read, watched.model.as_deref()),
		(0, None),
		"nothing was said to be shown, and frames were read: {:#?}",
		watched.shown
	);
	assert!(watched.picked_by.is_some() && watched.cost_cents > 0., "the pick itself is a request");
}

/// `secs` of `INVOICE 4417`, then `secs` of `REFUND 9203`, with no audio.
fn invoice_then_refund(clip: &Path, secs: u64) {
	let status = Command::new("ffmpeg")
		.args([
			"-v",
			"error",
			"-y",
			"-f",
			"lavfi",
			"-i",
			&format!("color=c=navy:s=640x360:d={secs}:r=10"),
			"-f",
			"lavfi",
			"-i",
			&format!("color=c=darkred:s=640x360:d={secs}:r=10"),
		])
		.args([
			"-filter_complex",
			"[0]drawtext=text='INVOICE 4417':fontsize=60:fontcolor=white:x=(w-tw)/2:y=(h-th)/2[a];\
			 [1]drawtext=text='REFUND 9203':fontsize=60:fontcolor=white:x=(w-tw)/2:y=(h-th)/2[b];[a][b]concat=n=2:v=1[v]",
			"-map",
			"[v]",
		])
		.arg(clip)
		.status()
		.unwrap();
	assert!(status.success());
}

/// Every `expected` text read within a second of where it appears; no `absent` text read at all.
async fn check(clip: &Path, frames: &Path, speech: Option<Vec<Said>>, pick: Pick, expected: &[(u64, &str)], absent: &[&str]) {
	let given = speech.as_ref().map_or(0, Vec::len);
	let watched = Client::default()
		.model(Model::Video)
		.watch(
			clip,
			Watch {
				title: "test clip".into(),
				speech,
				footage: Footage::Screen,
				pick,
				about: None,
				frames: frames.into(),
			},
		)
		.await
		.unwrap();
	assert_eq!(watched.speech.len(), given, "the clip has no audio, so the speech is only what was given: {:?}", watched.speech);
	for (secs, text) in expected {
		let hit = watched.shown.iter().find(|s| format!("{} {:?}", s.shown, s.on_screen_text).contains(text));
		let hit = hit.unwrap_or_else(|| panic!("`{text}` is on screen from {secs}s, and nothing read it: {:#?}", watched.shown));
		assert!(hit.secs.abs_diff(*secs) <= 1, "`{text}` is on screen from {secs}s, and was read at {}s", hit.secs);
		assert!(hit.frame.exists(), "the frame {} is cited and not kept", hit.frame.display());
	}
	for text in absent {
		let hit = watched.shown.iter().find(|s| format!("{} {:?}", s.shown, s.on_screen_text).contains(text));
		assert!(hit.is_none(), "`{text}` is only on screen where nothing is said to be shown, and was read: {hit:#?}");
	}
	assert!(watched.cost_cents > 0.);
}
