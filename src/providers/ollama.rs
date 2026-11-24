use std::time::Duration;

use genai::{
	adapter::AdapterKind,
	chat::{ChatMessage, MessageContent, ReasoningEffort, ToolResponse},
	resolver::AuthData,
};

use crate::{
	ContentPart, Failure, Request, Source, ThinkingLevel, Unrecoverable,
	wire::{self, Answer},
};

pub(crate) async fn call(source: &Source, request: &Request<'_>) -> Result<Answer, Failure> {
	if !request.files.is_empty() {
		return Err(Unrecoverable::new_unsupported("file attachments", "drop the attachment, or pick a remote `Model`".to_string()).into());
	}
	if !request.conversation.0.iter().all(|m| m.content.parts().iter().all(|p| matches!(p, ContentPart::Text(_)))) {
		return Err(Unrecoverable::new_unsupported("images and documents", "pick a remote `Model`".to_string()).into());
	}

	let mut chat = wire::tools(request);
	chat.messages = wire::messages(request)?;
	for round in request.rounds {
		chat.messages.push(ChatMessage::assistant(round.turn.said.clone()));
		for (call, result) in round.calls.iter().zip(&round.results) {
			let response = ToolResponse::new(&call.id, result);
			chat.messages.push(ChatMessage::tool(MessageContent::from_tool_responses(vec![response]))); // of several in one message genai keeps the last
		}
	}

	let mut options = wire::options(request).with_temperature(0.).with_reasoning_effort(match request.thinking {
		ThinkingLevel::None => ReasoningEffort::Zero,
		_ => ReasoningEffort::Budget(0), // `think: true`: most models take no level
	});
	options.max_tokens = request.max_tokens.map(|n| u32::try_from(n).expect("a token cap fits u32"));
	if let Some(sequences) = &request.stop_sequences {
		options = options.with_stop_sequences(sequences.iter().map(|s| s.to_string()).collect());
	}

	let origin = source.origin.as_deref().unwrap_or("http://localhost:11434");
	let mut response = wire::exec(source, AdapterKind::Ollama, format!("{origin}/"), AuthData::None, chat, options).await?;
	// it omits what is zero
	response.usage.prompt_tokens.get_or_insert(0);
	response.usage.completion_tokens.get_or_insert(0);
	let raw = response.captured_raw_body.as_ref().expect("asked for in `options`");
	let nanos = |field: &str| raw[field].as_u64().unwrap_or(0);
	let overhead = Duration::from_nanos(nanos("load_duration") + nanos("prompt_eval_duration"));
	let usage = wire::usage(&response)?;
	wire::answer(source, request, response, usage, overhead)
}
