//! Video generation on Vertex Veo. Outside the [`Model`](crate::Model) fallback graph: a clip from one model is not
//! interchangeable with another's, so the caller names the model and gets exactly it or an error.
use std::{path::PathBuf, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use eyre::{Result, bail, ensure, eyre};
use gcp_auth::TokenProvider as _;
use serde_json::{Value, json};

use crate::config::{AppConfig, missing};

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum Model {
	#[serde(rename = "veo-2")]
	Veo2,
	#[serde(rename = "veo-3")]
	Veo3,
	#[serde(rename = "veo-3-fast")]
	Veo3Fast,
	#[serde(rename = "veo-3-1")]
	Veo3_1,
	#[serde(rename = "veo-3-1-fast")]
	Veo3_1Fast,
	#[serde(rename = "veo-3-1-lite")]
	Veo3_1Lite,
}
impl Model {
	/// Vertex's id; moves as previews go GA, while the variant stays.
	pub const fn id(self) -> &'static str {
		match self {
			Self::Veo2 => "veo-2.0-generate-001",
			Self::Veo3 => "veo-3.0-generate-001",
			Self::Veo3Fast => "veo-3.0-fast-generate-001",
			Self::Veo3_1 => "veo-3.1-generate-001",
			Self::Veo3_1Fast => "veo-3.1-fast-generate-001",
			Self::Veo3_1Lite => "veo-3.1-lite-generate-001",
		}
	}
}

#[derive(Debug)]
pub struct Request {
	pub model: Model,
	pub prompt: String,
	pub input: Input,
	pub aspect: Aspect,
	pub resolution: Resolution,
	pub secs: Option<f32>,
	pub fps: Option<u32>,
	pub audio: bool = false,
	pub seed: Option<u32>,
	pub negative: Option<String>,
}

/// What the clip is made from; the API takes these combinations and no others.
#[derive(Debug)]
pub enum Input {
	Text,
	Frames {
		first: Image,
		last: Option<Image>,
	},
	/// `video` is an mp4; the mask has its aspect ratio. Only Veo 2 took it, and Veo 2 is retired: [`edit`] instead.
	Edit {
		video: Vec<u8>,
		mask: Image,
		mode: MaskMode,
	},
	/// `video` is an mp4.
	Extend {
		video: Vec<u8>,
	},
}

#[derive(Debug)]
pub struct Image {
	pub bytes: Vec<u8>,
	/// eg `image/png`
	pub mime: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MaskMode {
	/// mask is the region of the first frame to put the prompt's object in
	Insert,
	/// mask is the object in the first frame to track and remove
	Remove,
	/// mask is a static region to remove things from
	RemoveStatic,
	/// the video goes inside the mask, the outside is generated
	Outpaint,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, derive_more::Display, serde::Serialize)]
pub enum Aspect {
	#[serde(rename = "16:9")]
	#[display("16:9")]
	Landscape,
	#[serde(rename = "9:16")]
	#[display("9:16")]
	Portrait,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, derive_more::Display, serde::Serialize)]
pub enum Resolution {
	#[serde(rename = "720p")]
	#[display("720p")]
	P720,
	#[serde(rename = "1080p")]
	#[display("1080p")]
	P1080,
	#[serde(rename = "4k")]
	#[display("4k")]
	K4,
}

const POLL: Duration = Duration::from_secs(10);

/// Vertex's Gemini Omni Flash: since Veo 2's retirement, the one Google model that re-makes given footage.
pub const EDIT_MODEL: &str = "gemini-omni-1.1-flash-preview";

/// `video`, an mp4 of at most 10 s, re-made with `instruction` applied: the same length, at 24 fps. Billed per second of
/// output; nothing is retried, and the API's own error comes back verbatim.
pub async fn edit(video: &[u8], instruction: &str, aspect: Aspect, resolution: Resolution, config: &AppConfig) -> Result<Vec<u8>> {
	let (token, project, _) = vertex(config).await?;
	let body = json!({
		"model": EDIT_MODEL,
		"input": [
			{ "type": "video", "mime_type": "video/mp4", "data": B64.encode(video) },
			{ "type": "text", "text": instruction },
		],
		"response_format": { "type": "video", "resolution": resolution.to_string(), "aspect_ratio": aspect.to_string() },
		"generation_config": { "video_config": { "task": "edit" } }, // unset, Omni guesses the task from the prompt
	});
	tracing::info!(model = EDIT_MODEL, "omni edit");
	let response = reqwest::Client::new()
		.post(format!("https://aiplatform.googleapis.com/v1beta1/projects/{project}/locations/global/interactions"))
		.bearer_auth(token.as_str())
		.json(&body)
		.send()
		.await?;
	let status = response.status();
	let text = response.text().await?;
	ensure!(status.is_success(), "vertex {EDIT_MODEL} {status}: {text}");
	let done: Value = serde_json::from_str(&text)?;
	let video = done["steps"]
		.as_array()
		.into_iter()
		.flatten()
		.flat_map(|step| step["content"].as_array().into_iter().flatten())
		.find(|c| c["type"] == "video")
		.and_then(|c| c["data"].as_str())
		.ok_or_else(|| {
			eyre!(
				"{EDIT_MODEL} finished without an inline video; its steps were {:?}",
				done["steps"].as_array().map(|s| s.iter().map(|s| &s["type"]).collect::<Vec<_>>())
			)
		})?;
	Ok(B64.decode(video)?)
}

/// A token for the service account, its project and the configured location.
pub(crate) async fn vertex(config: &AppConfig) -> Result<(std::sync::Arc<gcp_auth::Token>, String, String)> {
	let credentials = config
		.google_credentials
		.clone()
		.or_else(|| std::env::var_os("GOOGLE_APPLICATION_CREDENTIALS").map(PathBuf::from))
		.ok_or_else(|| missing("google_credentials", "GOOGLE_APPLICATION_CREDENTIALS"))?;
	let location = config
		.google_location
		.clone()
		.or_else(|| std::env::var("GOOGLE_CLOUD_LOCATION").ok())
		.ok_or_else(|| missing("google_location", "GOOGLE_CLOUD_LOCATION"))?;
	let account = gcp_auth::CustomServiceAccount::from_file(&credentials).map_err(|e| eyre!("{}: {e}", credentials.display()))?;
	let project = account.project_id().ok_or_else(|| eyre!("{} names no project_id", credentials.display()))?.to_owned();
	let token = account.token(&["https://www.googleapis.com/auth/cloud-platform"]).await?;
	Ok((token, project, location))
}

/// One mp4. Billed per second of output; nothing is retried, and the API's own error comes back verbatim.
pub async fn generate(req: &Request, config: &AppConfig) -> Result<Vec<u8>> {
	let (token, project, location) = vertex(config).await?;
	let base = format!(
		"https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}/publishers/google/models/{}",
		req.model.id()
	);
	let http = reqwest::Client::new();
	let call = async |url: String, body: Value| -> Result<Value> {
		let response = http.post(url).bearer_auth(token.as_str()).json(&body).send().await?;
		let status = response.status();
		let text = response.text().await?;
		match status.is_success() {
			true => Ok(serde_json::from_str(&text)?),
			false => bail!("vertex {} {status}: {text}", req.model.id()),
		}
	};

	tracing::info!(model = req.model.id(), "veo submit");
	let op = call(format!("{base}:predictLongRunning"), body(req)).await?;
	let name = op["name"].as_str().ok_or_else(|| eyre!("predictLongRunning returned no operation name: {op}"))?.to_owned();
	let done = loop {
		tokio::time::sleep(POLL).await;
		let op = call(format!("{base}:fetchPredictOperation"), json!({ "operationName": name })).await?;
		if op["done"].as_bool() == Some(true) {
			break op;
		}
	};
	if !done["error"].is_null() {
		bail!("veo {}: {}", req.model.id(), done["error"]);
	}
	let video = done["response"]["videos"][0]["bytesBase64Encoded"]
		.as_str()
		.ok_or_else(|| eyre!("veo {} finished without a video: {}", req.model.id(), done["response"]))?;
	Ok(B64.decode(video)?)
}

fn body(req: &Request) -> Value {
	let image = |i: &Image| json!({ "bytesBase64Encoded": B64.encode(&i.bytes), "mimeType": i.mime });
	let video = |v: &[u8]| json!({ "bytesBase64Encoded": B64.encode(v), "mimeType": "video/mp4" });
	let mut instance = json!({ "prompt": req.prompt });
	let mut parameters = json!({
		"sampleCount": 1,
		"aspectRatio": req.aspect.to_string(),
		"resolution": req.resolution.to_string(),
		"generateAudio": req.audio,
	});
	match &req.input {
		Input::Text => {}
		Input::Frames { first, last } => {
			instance["image"] = image(first);
			if let Some(last) = last {
				instance["lastFrame"] = image(last);
			}
		}
		Input::Edit { video: v, mask, mode } => {
			instance["video"] = video(v);
			instance["mask"] = image(mask);
			instance["mask"]["maskMode"] = serde_json::to_value(mode).expect("a unit variant serializes");
			parameters["task"] = json!("edit");
		}
		Input::Extend { video: v } => instance["video"] = video(v),
	}
	if let Some(secs) = req.secs {
		parameters["durationSeconds"] = json!(secs);
	}
	if let Some(fps) = req.fps {
		parameters["fps"] = json!(fps);
	}
	if let Some(seed) = req.seed {
		parameters["seed"] = json!(seed);
	}
	if let Some(negative) = &req.negative {
		parameters["negativePrompt"] = json!(negative);
	}
	json!({ "instances": [instance], "parameters": parameters })
}
