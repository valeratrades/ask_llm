//! ≈$0.30 a run: 3s of footage re-made by Gemini Omni Flash at 720p. `cargo r --example video_edit -- <in.mp4> "<instruction>"`
use ask_llm::{
	config::AppConfig,
	video::{self, Resolution},
};

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
	color_eyre::install()?;
	let [_, input, instruction] = &std::env::args().collect::<Vec<_>>()[..] else {
		color_eyre::eyre::bail!("video_edit <in.mp4> <instruction>");
	};
	let mp4 = video::edit(&std::fs::read(input)?, instruction, Resolution::P720, &AppConfig::default()).await?;
	let out = std::env::temp_dir().join("ask_llm_video_edit.mp4");
	std::fs::write(&out, mp4)?;
	println!("{}", out.display());
	Ok(())
}
