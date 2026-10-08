//! `cargo r --example watch -- <media> <screen|filmed> <changes|likely> <frames dir>`
use ask_llm::{Client, Footage, Model, Pick, Watch};

#[tokio::main]
async fn main() {
	v_utils::clientside!();
	let [media, footage, pick, frames] = std::env::args()
		.skip(1)
		.collect::<Vec<_>>()
		.try_into()
		.expect("usage: watch <media> <screen|filmed> <changes|likely> <frames dir>");
	let footage = match footage.as_str() {
		"screen" => Footage::Screen,
		"filmed" => Footage::Filmed,
		other => panic!("footage is `screen` or `filmed`, not `{other}`"),
	};
	let pick = match pick.as_str() {
		"changes" => Pick::Changes,
		"likely" => Pick::Likely { every: 0.5 },
		other => panic!("pick is `changes` or `likely`, not `{other}`"),
	};
	let spec = Watch {
		title: media.clone(),
		speech: None,
		footage,
		pick,
		about: None,
		frames: frames.into(),
	};
	let watched = Client::default().model(Model::Video).watch(&media, spec).await.unwrap();
	for s in &watched.shown {
		println!("{} {}\n  > {:?}", s.secs, s.shown, s.on_screen_text);
	}
	println!(
		"{} said segments, {} frames read, {:.3}¢ by {:?}, picked by {:?}",
		watched.speech.len(),
		watched.frames_read,
		watched.cost_cents,
		watched.model,
		watched.picked_by
	);
}
