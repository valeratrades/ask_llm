//! ≈$0.04 a run: 4s at 30fps through SAM 3, a green square crossing noise.
use std::process::Command;

use ask_llm::{
	config::AppConfig,
	segment::{self, Model, Request},
};

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
	color_eyre::install()?;
	let dir = tempfile::tempdir()?;
	let clip = dir.path().join("clip.mp4");
	let made = Command::new("ffmpeg")
		.args(["-v", "error", "-y", "-f", "lavfi", "-i", "color=c=gray:s=1280x720:d=4:r=30,noise=alls=40:allf=t"])
		.args(["-f", "lavfi", "-i", "color=c=0x00ff00:s=120x120:d=4:r=30"])
		.args(["-filter_complex", "[0][1]overlay=x='100+n*200/30':y=300", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
		.arg(&clip)
		.status()?;
	assert!(made.success());
	let req = Request {
		model: Model::Sam3,
		video: std::fs::read(&clip)?,
		prompt: "green square".into(),
		points: vec![],
	};
	let mp4 = segment::segment(&req, &AppConfig::default()).await?;
	let out = std::env::temp_dir().join("ask_llm_segment.mp4");
	std::fs::write(&out, mp4)?;
	println!("{}", out.display());
	Ok(())
}
