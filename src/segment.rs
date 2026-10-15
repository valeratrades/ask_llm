//! Video segmentation on fal SAM 3. Outside the [`Model`](crate::Model) fallback graph, as [`video`](crate::video) is:
//! the caller names the model and gets exactly its mask or its error.
use std::{path::Path, time::Duration};

use eyre::{Result, bail, eyre};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::config::{AppConfig, missing};

const POLL: Duration = Duration::from_secs(5);
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum Model {
	#[serde(rename = "sam-3")]
	Sam3,
	#[serde(rename = "sam-3-1")]
	Sam3_1,
}
impl Model {
	pub const fn id(self) -> &'static str {
		match self {
			Self::Sam3 => "fal-ai/sam-3/video",
			Self::Sam3_1 => "fal-ai/sam-3-1/video",
		}
	}
}

#[derive(Debug)]
pub struct Request {
	pub model: Model,
	/// mp4
	pub video: Vec<u8>,
	/// what to segment, eg "the hand"; every object it names goes into the one mask
	pub prompt: String,
	/// clicks on frame 0, for when the text alone picks the wrong thing
	pub points: Vec<Point>,
}

/// Pixels of frame 0.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Point {
	pub x: u32,
	pub y: u32,
	pub inside: bool,
}

/// One mp4: grey 0..255, how much of each pixel the prompt's objects cover, at the input's size and frame count.
/// Billed per frame; nothing is retried, and fal's own error comes back verbatim.
pub async fn segment(req: &Request, config: &AppConfig) -> Result<Vec<u8>> {
	let key = config.fal_key.clone().or_else(|| std::env::var("FAL_KEY").ok()).ok_or_else(|| missing("fal_key", "FAL_KEY"))?;
	crate::transcribe::preflight("ffmpeg").await?;
	crate::transcribe::preflight("ffprobe").await?;
	let dir = tempfile::tempdir()?;
	let input = dir.path().join("input.mp4");
	std::fs::write(&input, &req.video)?;
	let shape = probe(&input).await?;

	let http = reqwest::Client::new();
	let auth = format!("Key {key}");
	let call = async |request: reqwest::RequestBuilder| -> Result<Value> {
		let response = request.header("Authorization", &auth).send().await?;
		let status = response.status();
		let text = response.text().await?;
		match status.is_success() {
			true => Ok(serde_json::from_str(&text)?),
			false => bail!("fal {} {status}: {text}", req.model.id()),
		}
	};

	let upload = call(
		http.post("https://rest.fal.ai/storage/upload/initiate?storage_type=fal-cdn-v3")
			.json(&json!({ "file_name": "input.mp4", "content_type": "video/mp4" })),
	)
	.await?;
	let (Some(put), Some(video_url)) = (upload["upload_url"].as_str(), upload["file_url"].as_str()) else {
		bail!("fal storage gave no upload_url/file_url: {upload}");
	};
	http.put(put).header("Content-Type", "video/mp4").body(req.video.clone()).send().await?.error_for_status()?;

	tracing::info!(model = req.model.id(), "sam submit");
	let points: Vec<Value> = req.points.iter().map(|p| json!({ "x": p.x, "y": p.y, "label": p.inside as u8 })).collect();
	let queued = call(http.post(format!("https://queue.fal.run/{}", req.model.id())).json(&json!({
		"video_url": video_url,
		"prompt": req.prompt,
		"point_prompts": points,
		"apply_mask": false,
	})))
	.await?;
	let (Some(status_url), Some(response_url)) = (queued["status_url"].as_str(), queued["response_url"].as_str()) else {
		bail!("fal {} queued without status_url/response_url: {queued}", req.model.id());
	};
	//LOOP: fal's queue owns the timeout, and reports it as a status that is not COMPLETED
	loop {
		tokio::time::sleep(POLL).await;
		let status = call(http.get(status_url)).await?;
		match status["status"].as_str() {
			Some("COMPLETED") => break,
			Some("IN_QUEUE" | "IN_PROGRESS") => {}
			_ => bail!("fal {}: {status}", req.model.id()),
		}
	}
	let done = call(http.get(response_url)).await?;
	let mask_url = done["video"]["url"].as_str().ok_or_else(|| eyre!("fal {} finished without a video: {done}", req.model.id()))?;
	let raw = dir.path().join("mask.mp4");
	std::fs::write(&raw, http.get(mask_url).send().await?.error_for_status()?.bytes().await?)?;

	let got = probe(&raw).await?;
	if got != shape {
		bail!(
			"fal {} returned {}x{} × {} frames for an input of {}x{} × {}",
			req.model.id(),
			got.0,
			got.1,
			got.2,
			shape.0,
			shape.1,
			shape.2
		);
	}
	let grey = dir.path().join("grey.mp4");
	let out = Command::new("ffmpeg")
		.args(["-y", "-loglevel", "error", "-i"])
		.arg(&raw)
		.args(["-fps_mode", "passthrough", "-vf", "format=gray", "-c:v", "libx264", "-crf", "0"])
		.arg(&grey)
		.output()
		.await?;
	if !out.status.success() {
		bail!("ffmpeg failed to grey the mask: {}", String::from_utf8_lossy(&out.stderr).trim());
	}
	Ok(std::fs::read(grey)?)
}

/// width, height, frames
async fn probe(media: &Path) -> Result<(u32, u32, u32)> {
	let out = Command::new("ffprobe")
		.args([
			"-v",
			"error",
			"-select_streams",
			"v:0",
			"-count_frames",
			"-show_entries",
			"stream=width,height,nb_read_frames",
			"-of",
			"csv=p=0",
		])
		.arg(media)
		.output()
		.await?;
	if !out.status.success() {
		bail!("ffprobe could not read {}: {}", media.display(), String::from_utf8_lossy(&out.stderr).trim());
	}
	let text = String::from_utf8(out.stdout)?;
	let fields: Vec<u32> = text
		.trim()
		.split(',')
		.map(str::parse)
		.collect::<Result<_, _>>()
		.map_err(|e| eyre!("ffprobe read {} as `{}`: {e}", media.display(), text.trim()))?;
	let [w, h, n] = fields[..] else {
		bail!("ffprobe read {} as `{}`, not width,height,frames", media.display(), text.trim());
	};
	Ok((w, h, n))
}
