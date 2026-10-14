#![feature(default_field_values)]
//! ≈$0.20 a run: 4s of Veo 3.1 Lite at 720p, no audio.
use ask_llm::{
	config::AppConfig,
	video::{self, Aspect, Input, Model, Request, Resolution},
};

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
	color_eyre::install()?;
	let req = Request {
		model: Model::Veo3_1Lite,
		prompt: "a red kettle on a gas stove starts to steam, static shot, morning light".into(),
		input: Input::Text,
		aspect: Aspect::Landscape,
		resolution: Resolution::P720,
		secs: Some(4.),
		fps: None,
		seed: None,
		negative: None,
		..
	};
	let mp4 = video::generate(&req, &AppConfig::default()).await?;
	let out = std::env::temp_dir().join("ask_llm_video.mp4");
	std::fs::write(&out, mp4)?;
	println!("{}", out.display());
	Ok(())
}
