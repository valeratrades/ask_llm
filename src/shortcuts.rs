use eyre::Result;

use crate::{Client, Response};

pub async fn oneshot(message: impl Into<String>) -> Result<Response> {
	Client::new().ask(message).await
}

pub fn oneshot_blocking(message: impl Into<String>) -> Result<Response> {
	let runtime = tokio::runtime::Runtime::new()?;
	runtime.block_on(oneshot(message))
}
