use eyre::Result;

use crate::{Conversation, Model, Response};

pub fn oneshot<T: AsRef<str>>(message: T, model: Model) -> Result<Response> {
	let runtime = tokio::runtime::Runtime::new().unwrap();
	runtime.block_on(crate::oneshot(message, model))
}

pub fn conversation(conv: &Conversation, model: Model, max_tokens: Option<usize>, stop_sequences: Option<Vec<&str>>) -> Result<Response> {
	let runtime = tokio::runtime::Runtime::new().unwrap();
	runtime.block_on(crate::conversation(conv, model, max_tokens, stop_sequences))
}
