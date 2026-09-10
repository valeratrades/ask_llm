//! The status/body → [`Api`] mapping, driven off fixture bodies rather than the network.
//!
//! Providers move their error shapes around; this is the check that fails when the table rots.
use ask_llm::Api;

fn main() {
	let openai_401 = r#"{"error":{"message":"Incorrect API key provided: sk-not-a****key.","type":"invalid_request_error","param":null,"code":"invalid_api_key"}}"#;
	let err = Api::classify("OpenAI", 401, None, openai_401);
	assert!(matches!(err, Api::Auth { .. }), "401 is the key, got {err:?}");
	println!("{:?}\n", miette::Report::new(err));

	let openai_404 = r#"{"error":{"message":"The model `gpt-5.6-terra` does not exist","type":"invalid_request_error","param":null,"code":"model_not_found"}}"#;
	let err = Api::classify("OpenAI", 404, None, openai_404);
	assert!(matches!(err, Api::ModelUnavailable { .. }), "404 is a retired model, got {err:?}");
	println!("{:?}\n", miette::Report::new(err));

	let openai_429 = r#"{"error":{"message":"Rate limit reached for gpt-5.6-luna","type":"requests","param":null,"code":"rate_limit_exceeded"}}"#;
	let err = Api::classify("OpenAI", 429, Some("20"), openai_429);
	assert!(
		matches!(err, Api::RateLimited { retry_after: Some(d), .. } if d.as_secs() == 20),
		"the `retry-after` header carries the provider's own wait, got {err:?}"
	);
	println!("{:?}\n", miette::Report::new(err));

	// 429 is also how a spent account reads, and that one is not worth backing off on
	let openai_quota = r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","param":null,"code":"insufficient_quota"}}"#;
	let err = Api::classify("OpenAI", 429, None, openai_quota);
	assert!(matches!(err, Api::Quota { .. }), "a spent account is not a rate limit, got {err:?}");
	println!("{:?}\n", miette::Report::new(err));

	// Ollama's envelope is a bare string where OpenAI's is an object
	let ollama_404 = r#"{"error":"model \"qwen3.5:4b\" not found, try pulling it first"}"#;
	let err = Api::classify("Ollama", 404, None, ollama_404);
	let Api::ModelUnavailable { message, .. } = &err else {
		panic!("a plain-string envelope must not land in Api::Other, got {err:?}");
	};
	assert!(message.starts_with("model"), "the provider's own words survive: {message}");
	println!("{:?}\n", miette::Report::new(err));

	// nothing parses: a CDN answering with html still has to classify off the status alone
	let err = Api::classify("OpenAI", 503, None, "<html><body>502 Bad Gateway</body></html>");
	assert!(matches!(err, Api::Overloaded { .. }), "an unparseable body still classifies, got {err:?}");
	println!("{:?}", miette::Report::new(err));
}

#[cfg(test)]
#[test]
fn test_main() {
	main();
}
