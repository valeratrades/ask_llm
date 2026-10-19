//! Video segmentation, by the model named: fal SAM 3, or SAM 2.1 on the local GPU. Outside the [`Model`](crate::Model)
//! fallback graph, as [`video`](crate::video) is: the caller names the model and gets exactly its mask or its error.
use std::{path::Path, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use eyre::{Result, bail, ensure, eyre};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::{
	Provider,
	config::{AppConfig, missing},
};

/// Boxes each object `prompt` names for [`Model::Sam2_1`], on the frame it shows best.
pub const SEEDER: &str = "gemini-3-flash-preview";
const POLL: Duration = Duration::from_secs(5);
/// Frames of the input [`SEEDER`] looks at, spread over it.
const LOOKS: u32 = 12;
const SAM2: &str = "ask_llm-sam2";
const SAM2_PY: &str = include_str!("segment/sam2.py");
/// Best first.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum Model {
	#[serde(rename = "sam-3-1")]
	Sam3_1,
	#[serde(rename = "sam-3")]
	Sam3,
	/// On the local GPU, through `ask_llm-sam2` (this flake's `packages.sam2`), seeded by [`SEEDER`] boxing `prompt`.
	#[serde(rename = "sam-2-1")]
	Sam2_1,
}
impl Model {
	pub const ALL: [Self; 3] = [Self::Sam3_1, Self::Sam3, Self::Sam2_1];

	pub const fn id(self) -> &'static str {
		match self {
			Self::Sam3_1 => "fal-ai/sam-3-1/video",
			Self::Sam3 => "fal-ai/sam-3/video",
			Self::Sam2_1 => "facebook/sam2.1-hiera-large",
		}
	}

	pub const fn provider(self) -> Provider {
		match self {
			Self::Sam3_1 | Self::Sam3 => Provider::Fal,
			Self::Sam2_1 => Provider::Local,
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
	/// clicks on frame 0, for when the text alone picks the wrong thing; fal's only
	pub points: Vec<Point>,
}

/// Pixels of frame 0.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Point {
	pub x: u32,
	pub y: u32,
	pub inside: bool,
}

#[derive(Debug)]
pub struct Mask {
	/// mp4: grey 0..255, how much of each pixel the prompt's objects cover, at the input's size and frame count
	pub video: Vec<u8>,
	pub bill: Bill,
}

/// What one call cost, and who billed it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bill {
	pub vendor: &'static str,
	pub model: &'static str,
	pub usd: f64,
}

/// Nothing is retried, and the provider's own error comes back verbatim.
pub async fn segment(req: &Request, config: &AppConfig) -> Result<Mask> {
	crate::transcribe::preflight("ffmpeg").await?;
	crate::transcribe::preflight("ffprobe").await?;
	let dir = tempfile::tempdir()?;
	let input = dir.path().join("input.mp4");
	std::fs::write(&input, &req.video)?;
	let shape = probe(&input).await?;
	let raw = dir.path().join("mask.mp4");
	let bill = match req.model.provider() {
		Provider::Fal => fal(req, config, &raw, shape.2).await?,
		Provider::Local => sam2(req, config, dir.path(), &input, shape).await?,
		Provider::Ollama | Provider::OpenAi | Provider::Claude => unreachable!("{} is fal's or local", req.model.id()),
	};

	let got = probe(&raw).await?;
	if got != shape {
		bail!(
			"{} returned {}x{} × {} frames for an input of {}x{} × {}",
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
	Ok(Mask { video: std::fs::read(grey)?, bill })
}

/// Billed per 16 frames.
async fn fal(req: &Request, config: &AppConfig, raw: &Path, frames: u32) -> Result<Bill> {
	let key = config.fal_key.clone().or_else(|| std::env::var("FAL_KEY").ok()).ok_or_else(|| missing("fal_key", "FAL_KEY"))?;
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
	std::fs::write(raw, http.get(mask_url).send().await?.error_for_status()?.bytes().await?)?;
	let per_16_frames = match req.model {
		Model::Sam3 => 0.005,
		Model::Sam3_1 => 0.01,
		Model::Sam2_1 => unreachable!("local"),
	};
	Ok(Bill {
		vendor: "fal",
		model: req.model.id(),
		usd: frames.div_ceil(16) as f64 * per_16_frames,
	})
}

/// [`SEEDER`] boxes each object on the frame it shows best, `ask_llm-sam2` tracks it from there both ways. Billed is the seeding.
async fn sam2(req: &Request, config: &AppConfig, dir: &Path, input: &Path, (w, h, n): (u32, u32, u32)) -> Result<Bill> {
	ensure!(req.points.is_empty(), "{} is seeded from `prompt` alone; clicks are fal's", req.model.id());
	crate::transcribe::preflight(SAM2).await?;
	let (frames, masks) = (dir.join("frames"), dir.join("masks"));
	std::fs::create_dir(&frames)?;
	let out = Command::new("ffmpeg")
		.args(["-y", "-loglevel", "error", "-i"])
		.arg(input)
		.args(["-fps_mode", "passthrough", "-q:v", "2", "-start_number", "0"])
		.arg(frames.join("%05d.jpg"))
		.output()
		.await?;
	ensure!(out.status.success(), "ffmpeg failed to decode the input: {}", String::from_utf8_lossy(&out.stderr).trim());
	let decoded = std::fs::read_dir(&frames)?.count() as u32;
	ensure!(decoded == n, "ffmpeg decoded {decoded} frames of an input of {n}");

	let (objects, bill) = seed(&req.prompt, &frames, (w, h, n), config).await?;
	tracing::info!(model = req.model.id(), ?objects, "sam 2 tracking");
	let out = Command::new(SAM2).arg("-c").arg(SAM2_PY).arg(&frames).arg(&masks).arg(objects.to_string()).output().await?;
	ensure!(
		out.status.success(),
		"{SAM2} failed: {}\n{}",
		String::from_utf8_lossy(&out.stdout).trim(),
		String::from_utf8_lossy(&out.stderr).trim()
	);
	tracing::info!(model = req.model.id(), "{}", String::from_utf8_lossy(&out.stdout).trim());

	let rate = Command::new("ffprobe")
		.args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=r_frame_rate", "-of", "csv=p=0"])
		.arg(input)
		.output()
		.await?;
	ensure!(rate.status.success(), "ffprobe could not read the input's rate: {}", String::from_utf8_lossy(&rate.stderr).trim());
	let out = Command::new("ffmpeg")
		.args(["-y", "-loglevel", "error", "-framerate", String::from_utf8(rate.stdout)?.trim(), "-i"])
		.arg(masks.join("%05d.png"))
		.args(["-vf", "format=gray", "-c:v", "libx264", "-crf", "0"])
		.arg(dir.join("mask.mp4"))
		.output()
		.await?;
	ensure!(out.status.success(), "ffmpeg failed to encode the mask: {}", String::from_utf8_lossy(&out.stderr).trim());
	Ok(bill)
}

/// `[{"frame", "box": [x0, y0, x1, y1] in pixels, "what"}]`, one per object `prompt` names.
async fn seed(prompt: &str, frames: &Path, (w, h, n): (u32, u32, u32), config: &AppConfig) -> Result<(Value, Bill)> {
	let looks = LOOKS.min(n);
	let shown: Vec<u32> = (0..looks).map(|i| i * (n - 1) / (looks - 1).max(1)).collect();
	let mut parts = Vec::new();
	for &t in &shown {
		parts.push(json!({ "text": format!("frame {t}") }));
		parts.push(json!({
			"inlineData": { "mimeType": "image/jpeg", "data": B64.encode(std::fs::read(frames.join(format!("{t:05}.jpg")))?) },
			"mediaResolution": { "level": "MEDIA_RESOLUTION_HIGH" },
		}));
	}
	parts.push(json!({ "text": format!(
		"The images above are frames of one video, each after its number. Find every separate object this names: \"{prompt}\". \
		 For each, pick the frame above where it is most fully and clearly in view, and give its box on that frame: `box_2d`, \
		 [ymin, xmin, ymax, xmax] scaled 0-1000, tight around the whole object and nothing else; `frame`, that frame's number; \
		 `what`, the object in a few words. A phrase naming two things (\"the A and the B\") is two objects; one object seen \
		 in many frames is one. None, if no frame shows it."
	) }));
	let int = json!({ "type": "INTEGER" });
	let schema = json!({ "type": "OBJECT", "required": ["objects"], "properties": { "objects": { "type": "ARRAY", "items": {
		"type": "OBJECT", "required": ["frame", "box_2d", "what"],
		"properties": { "frame": int, "box_2d": { "type": "ARRAY", "items": int }, "what": { "type": "STRING" } },
	} } } });
	let (token, project, _) = crate::video::vertex(config).await?;
	let body = json!({
		"contents": [{ "role": "user", "parts": parts }],
		"generationConfig": { "responseMimeType": "application/json", "responseSchema": schema },
	});
	// v1beta1: per-part mediaResolution
	let response = reqwest::Client::new()
		.post(format!(
			"https://aiplatform.googleapis.com/v1beta1/projects/{project}/locations/global/publishers/google/models/{SEEDER}:generateContent"
		))
		.bearer_auth(token.as_str())
		.json(&body)
		.send()
		.await?;
	let status = response.status();
	let text = response.text().await?;
	ensure!(status.is_success(), "vertex {SEEDER} {status}: {text}");
	let answer: Value = serde_json::from_str(&text)?;

	let usage = &answer["usageMetadata"];
	let count = |k: &str| usage[k].as_u64().ok_or_else(|| eyre!("{SEEDER} answered no {k}: {usage}"));
	let thought = usage["thoughtsTokenCount"].as_u64().unwrap_or(0); // absent when it thought nothing
	// ref: https://cloud.google.com/vertex-ai/generative-ai/pricing, 2026-10: $0.50/M in, $3/M out, thinking as out
	let usd = (count("promptTokenCount")? as f64 * 0.5 + (count("candidatesTokenCount")? + thought) as f64 * 3.) / 1e6;
	let candidate = &answer["candidates"][0];
	ensure!(candidate["finishReason"] == "STOP", "{SEEDER} stopped with {}: {answer}", candidate["finishReason"]);
	let said = candidate["content"]["parts"]
		.as_array()
		.into_iter()
		.flatten()
		.filter(|p| p["thought"] != true)
		.find_map(|p| p["text"].as_str())
		.ok_or_else(|| eyre!("{SEEDER} answered no text: {answer}"))?;

	#[derive(serde::Deserialize)]
	struct Seen {
		objects: Vec<Object>,
	}
	#[derive(serde::Deserialize)]
	struct Object {
		frame: u32,
		box_2d: Vec<u32>,
		what: String,
	}
	let Seen { objects } = serde_json::from_str(said).map_err(|e| eyre!("{SEEDER} answered not the json asked for: {e}\n{said}"))?;
	ensure!(!objects.is_empty(), "{SEEDER} sees \"{prompt}\" in none of frames {shown:?}");
	let mut seeds = Vec::new();
	for o in objects {
		let [y0, x0, y1, x1] = o.box_2d[..] else {
			bail!("{SEEDER} boxed {} as {:?}, not [ymin, xmin, ymax, xmax]", o.what, o.box_2d);
		};
		ensure!(shown.contains(&o.frame), "{SEEDER} boxed {} on frame {}, which it was not shown", o.what, o.frame);
		ensure!(x0 < x1 && y0 < y1 && x1 <= 1000 && y1 <= 1000, "{SEEDER} boxed {} as {:?}, out of 0-1000", o.what, o.box_2d);
		let (px, py) = (|v: u32| v as f64 * w as f64 / 1000., |v: u32| v as f64 * h as f64 / 1000.);
		seeds.push(json!({ "frame": o.frame, "box": [px(x0), py(y0), px(x1), py(y1)], "what": o.what }));
	}
	Ok((
		Value::Array(seeds),
		Bill {
			vendor: "Google",
			model: SEEDER,
			usd,
		},
	))
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
