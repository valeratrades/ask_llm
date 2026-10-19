//! `cargo r --example segment -- <sam-3|sam-3-1|sam-2-1>`: ≈$0.04 a run on fal SAM 3, under a cent of Gemini seeding on
//! local SAM 2 (`nix shell .#sam2`). 4s at 30fps: a green square crosses noise; its mask must sit where it is, frame for
//! frame. Prints the mask's path and what it cost.
use std::{path::Path, process::Command};

use ask_llm::{
	config::AppConfig,
	segment::{self, Model, Request},
};

const W: u32 = 1280;
const H: u32 = 720;
const FPS: u32 = 30;
const FRAMES: u32 = 4 * FPS;
const SIDE: u32 = 120;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
	color_eyre::install()?;
	let dir = tempfile::tempdir()?;
	let clip = dir.path().join("clip.mp4");
	square_over_noise(&clip);
	let model = std::env::args().nth(1).expect("usage: segment <model>, eg sam-2-1");
	let req = Request {
		model: serde_json::from_value::<Model>(serde_json::Value::String(model))?,
		video: std::fs::read(&clip)?,
		prompt: "green square".into(),
		points: vec![],
	};
	let mask = std::env::temp_dir().join("ask_llm_segment.mp4");
	let made = segment::segment(&req, &AppConfig::default()).await?;
	std::fs::write(&mask, made.video)?;
	println!("{} {:?}", mask.display(), made.bill);

	let raw = Command::new("ffmpeg")
		.args(["-v", "error", "-i"])
		.arg(&mask)
		.args(["-f", "rawvideo", "-pix_fmt", "gray", "-"])
		.output()?;
	assert!(raw.status.success(), "{}", String::from_utf8_lossy(&raw.stderr));
	let frame = (W * H) as usize;
	assert_eq!(raw.stdout.len(), frame * FRAMES as usize, "mask is not {FRAMES} frames of {W}x{H}");

	for n in [0, FRAMES / 2, FRAMES - 1] {
		let pixels = &raw.stdout[n as usize * frame..][..frame];
		let (x0, y0) = square(n);
		let inside = |x: u32, y: u32| (x0..x0 + SIDE).contains(&x) && (y0..y0 + SIDE).contains(&y);
		let (mut sx, mut sy, mut white, mut both) = (0u64, 0u64, 0u64, 0u64);
		for (i, &p) in pixels.iter().enumerate() {
			let (x, y) = (i as u32 % W, i as u32 / W);
			if p > 127 {
				sx += x as u64;
				sy += y as u64;
				white += 1;
				both += inside(x, y) as u64;
			}
		}
		assert!(white > 0, "frame {n}: mask is empty");
		let (cx, cy) = (sx as f64 / white as f64, sy as f64 / white as f64);
		let (ex, ey) = (x0 as f64 + SIDE as f64 / 2., y0 as f64 + SIDE as f64 / 2.);
		assert!((cx - ex).hypot(cy - ey) < 5., "frame {n}: centroid ({cx:.1}, {cy:.1}), square's ({ex}, {ey})");
		let iou = both as f64 / (white + (SIDE * SIDE) as u64 - both) as f64;
		assert!(iou > 0.8, "frame {n}: IoU {iou:.2}");
	}
	Ok(())
}

/// top-left of the square on frame `n`
fn square(n: u32) -> (u32, u32) {
	(100 + n * 200 / FPS, 300)
}

fn square_over_noise(out: &Path) {
	let status = Command::new("ffmpeg")
		.args(["-v", "error", "-y", "-f", "lavfi", "-i"])
		.arg(format!("color=c=gray:s={W}x{H}:d=4:r={FPS},noise=alls=40:allf=t"))
		.args(["-f", "lavfi", "-i"])
		.arg(format!("color=c=0x00ff00:s={SIDE}x{SIDE}:d=4:r={FPS}"))
		.arg("-filter_complex")
		.arg(format!("[0][1]overlay=x='100+n*200/{FPS}':y=300"))
		.args(["-frames:v", &FRAMES.to_string(), "-c:v", "libx264", "-pix_fmt", "yuv420p"])
		.arg(out)
		.status()
		.unwrap();
	assert!(status.success());
}
