use eyre::Result;

use crate::{Client, Conversation, Model, Response};

pub async fn oneshot(message: impl Into<String>) -> Result<Response> {
	Client::new().ask(message).await
}

pub fn oneshot_blocking(message: impl Into<String>) -> Result<Response> {
	let runtime = tokio::runtime::Runtime::new()?;
	runtime.block_on(oneshot(message))
}

/// Legacy conversation function for backwards compatibility
pub async fn conversation<T: AsRef<str>>(conv: &Conversation, model: Model, max_tokens: Option<usize>, stop_sequences: Option<Vec<T>>) -> Result<Response> {
	let mut client = Client::new().model(model);
	if let Some(tokens) = max_tokens {
		client = client.max_tokens(tokens);
	}
	if let Some(seqs) = stop_sequences {
		client = client.stop_sequences(seqs.into_iter().map(|s| s.as_ref().to_string()).collect::<Vec<_>>());
	}
	client.conversation(conv).await
}
