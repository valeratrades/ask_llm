//! `watch` is frames chosen by ffmpeg, read by a remote model, and handed back at the second they were
//! read off. Only a live round trip covers the three together.

use std::{path::Path, process::Command};

use ask_llm::{Client, Footage, Model, Watch};

#[tokio::test]
#[ignore = "spends a live OpenAI round trip"]
async fn reads_what_the_picture_says_at_its_second() {
	let dir = tempfile::tempdir().unwrap();
	let clip = dir.path().join("clip.mp4");
	let status = Command::new("ffmpeg")
		.args([
			"-v",
			"error",
			"-y",
			"-f",
			"lavfi",
			"-i",
			"color=c=navy:s=640x360:d=6:r=10",
			"-f",
			"lavfi",
			"-i",
			"color=c=darkred:s=640x360:d=6:r=10",
		])
		.args([
			"-filter_complex",
			"[0]drawtext=text='INVOICE 4417':fontsize=60:fontcolor=white:x=(w-tw)/2:y=(h-th)/2[a];\
			 [1]drawtext=text='REFUND 9203':fontsize=60:fontcolor=white:x=(w-tw)/2:y=(h-th)/2[b];[a][b]concat=n=2:v=1[v]",
			"-map",
			"[v]",
		])
		.arg(&clip)
		.status()
		.unwrap();
	assert!(status.success());

	check(&clip, &dir.path().join("frames"), &[(0, "4417"), (6, "9203")]).await;
}

async fn check(clip: &Path, frames: &Path, expected: &[(u64, &str)]) {
	let watched = Client::default()
		.model(Model::Video)
		.watch(
			clip,
			Watch {
				title: "test clip".into(),
				speech: None,
				footage: Footage::Screen,
				frames: frames.into(),
			},
		)
		.await
		.unwrap();
	assert!(watched.speech.is_empty(), "the clip has no audio: {:?}", watched.speech);
	for (secs, text) in expected {
		let hit = watched.shown.iter().find(|s| format!("{} {:?}", s.shown, s.on_screen_text).contains(text));
		let hit = hit.unwrap_or_else(|| panic!("`{text}` is on screen from {secs}s, and nothing read it: {:#?}", watched.shown));
		assert!(hit.secs.abs_diff(*secs) <= 1, "`{text}` is on screen from {secs}s, and was read at {}s", hit.secs);
		assert!(hit.frame.exists(), "the frame {} is cited and not kept", hit.frame.display());
	}
	assert!(watched.cost_cents > 0.);
}
