use std::{
	path::{Path, PathBuf},
	process::Stdio,
};

use eyre::{Result, bail};
use tokio::process::Command;

/// Speech to text, locally. Unlike every other capability here this reaches no provider, so it has
/// no key, no balance and no rate limit to run out of.
///
/// Takes anything ffmpeg can decode. The model is picked up from `$WHISPER_MODEL`, else from the
/// whisper-cpp data directory.
pub async fn transcribe(audio: impl AsRef<Path>) -> Result<String> {
	let audio = audio.as_ref();
	if !audio.is_file() {
		bail!("audio file not found: {}", audio.display());
	}
	let model = whisper_model()?;
	preflight("ffmpeg").await?;
	preflight("whisper-cli").await?;

	let wav = tempfile::Builder::new().suffix(".wav").tempfile()?;
	let decode = Command::new("ffmpeg")
		.args(["-y", "-loglevel", "error", "-i"])
		.arg(audio)
		.args(["-ar", "16000", "-ac", "1"])
		.arg(wav.path())
		.output()
		.await?;
	if !decode.status.success() {
		bail!("ffmpeg failed to decode {}: {}", audio.display(), String::from_utf8_lossy(&decode.stderr).trim());
	}

	let out = Command::new("whisper-cli")
		.arg("-m")
		.arg(&model)
		.args(["-l", "auto", "-nt", "-np", "-f"])
		.arg(wav.path())
		.stdin(Stdio::null())
		.output()
		.await?;

	let text = String::from_utf8(out.stdout)?.trim().to_string();
	// whisper-cli exits 0 even when it never read the audio, so the status is worthless here and an
	// empty transcript is the only signal that anything went wrong.
	if text.is_empty() {
		bail!("whisper-cli produced no transcript for {}: {}", audio.display(), String::from_utf8_lossy(&out.stderr).trim());
	}
	Ok(text)
}
fn whisper_model() -> Result<PathBuf> {
	if let Ok(path) = std::env::var("WHISPER_MODEL") {
		let path = PathBuf::from(path);
		if !path.is_file() {
			bail!("$WHISPER_MODEL points at a missing file: {}", path.display());
		}
		return Ok(path);
	}

	let dir = PathBuf::from(v_utils::io::xdg::xdg_data_fallback()).join("whisper-cpp/models");
	let default = dir.join("ggml-base.en.bin");
	if default.is_file() {
		return Ok(default);
	}

	let available: Vec<String> = match std::fs::read_dir(&dir) {
		Ok(entries) => entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect(),
		Err(_) => Vec::new(),
	};
	bail!(
		"no whisper model at {}. Fetch one with `whisper-cpp-download-ggml-model base.en`, or point $WHISPER_MODEL at it. Present in that directory: {available:?}",
		default.display()
	)
}
async fn preflight(bin: &str) -> Result<()> {
	match Command::new(bin).arg("--help").stdout(Stdio::null()).stderr(Stdio::null()).status().await {
		Ok(_) => Ok(()),
		Err(e) => bail!("`{bin}` is required for transcription but could not be run: {e}"),
	}
}
