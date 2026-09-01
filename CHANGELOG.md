# Changelog

## v2.2.0

- **Breaking**: `Client::new` now requires `config::AppConfig` argument. Use `Client::default()` for environment-based config.
- **New**: `Model::Cheap` and `Model::Translate` variants backed by local Ollama (`qwen3.5:4b` and `translategemma:4b`).
- **New**: `ollama` backend module — routes `Cheap`/`Translate` models to `http://localhost:11434/api/chat`.
- **Refactor**: `Backend` is now an internal trait; `Client` holds a `Box<dyn Backend>` selected at construction time. The old free functions (`ask_claude`, etc.) are gone; use the `Client` builder instead.
- **Preserved**: `oneshot()` and `conversation()` shortcuts in `src/shortcuts.rs` still work via `Client::default()`.

### v2.2.1

- Fix `main.rs` CLI entry point broken by the 2.2.0 refactor.

### v2.2.2

- Bump `v_utils` to `^2.15.30` (required for `SettingsError` in `derive(Settings)` macro).
- Move `.readme_assets/` → `docs/.readme_assets/` (v_flakes v1.6 convention).
- Update flake to `v_flakes` v1.6.

### v2.2.3

- **New**: `Model::DeepSeek` variant, backed by a new `deepseek` backend module hitting `https://api.deepseek.com/chat/completions` with `deepseek-v4-flash`.
  upd: nuked it. Not the correct mechanic to attach it through, agent who did this was wrong.
- **New**: `config::AppConfig::deepseek_token`, falling back to the `DEEPSEEK_KEY` env var.
- `ThinkingLevel` maps onto DeepSeek's `thinking` request param (`None` disables it explicitly, since the API enables thinking by default).

## v3.0.0

Anthropic backend rewritten against the current API — the previous request shape produced unconditional 400s on every live model.

- **Breaking**: `Client::temperature()` removed. The parameter is rejected by every current Anthropic model, and the Ollama/DeepSeek backends now pin it to `0.0`.
- **Breaking**: Claude tiers are now `Fast` → `claude-sonnet-5`, `Medium` → `claude-opus-5`, `Slow` → `claude-fable-5`. `claude-opus-4-1` was retired 2026-08-05, so `Model::Slow` had been 404ing.
- Haiku 4.5 dropped: it rejects both `adaptive` thinking and `output_config.effort`, so keeping it would force a second request shape.
- `ThinkingLevel` maps onto `output_config.effort` (`low`/`low`/`medium`/`high`), with `thinking: {"type": "adaptive"}` for everything but `None`. `budget_tokens` is gone. `None` omits the `thinking` key rather than sending `{"type": "disabled"}`, which Fable rejects outright and Opus rejects above `high` effort.
- `force_json` appends an instruction to the last user message instead of prefilling an assistant turn (prefill is a 400 on all current models).
- Fix: streamed `thinking_delta` chunks could be concatenated into `Response::text`.
- Fix: `thinking` content blocks have no `text` field, which broke deserialization of the whole non-streamed response.
- `stop_reason: "refusal"` is now surfaced as an error on the streaming path too, not just the REST one.
- `max_tokens` ceiling raised to 128k on all three tiers; dropped the no-op `output-128k-2025-02-19` beta header.

### v3.0.1

- Fix: adaptive thinking is billed against `max_tokens` and can consume all of it, so a tight budget returned an empty string with `stop_reason: "max_tokens"`. That now errors instead of silently reading as "the model had nothing to say". Hit `Medium`/`Slow` first, whose thinking runs even at `ThinkingLevel::None`.

## Unreleased

- **Fix**: cost read `prompt_tokens` flat, at the uncached rate, on every backend. Cached input is a tenth of that rate and a cache write is 1.25x it, so a long reused prefix was reported wrong in both directions — measured against gpt-5.6-luna at 7614 prompt tokens, the write turn was under-reported 20% and the hit turn over-reported 9.7x. `Cost` now carries all four rates and `Usage` normalizes the token counts, which each provider splits differently: Anthropic's `input_tokens` excludes the cache tiers, OpenAI's `prompt_tokens` and DeepSeek's hit/miss pair include them.
- **Fix**: DeepSeek was priced at 0.14/0.28. Current rates are 0.22 input / 0.007 cached / 0.66 output off-peak, doubling 01:00-04:00 and 06:00-10:00 UTC on weekdays. Nothing in the response says which window a request landed in, so it is taken from the clock.
- OpenAI cache rates: sol 0.4 read / 5.0 write, terra 0.2 / 2.5, luna 0.02 / 0.25. Anthropic's are uniformly 0.1x and 1.25x of base input. A 1-hour Anthropic cache write bills at 2x instead and `Cost` has no rate for it, so it asserts rather than under-reporting; nothing sets `cache_control` today, so it cannot fire yet.
- Long-context pricing is still not handled: OpenAI bills roughly double above ~272k input tokens, and the published table gives the rates but not the per-tier threshold.
- **Refactor**: what the four backends had each copied now lives once in `lib.rs` — `Cost` (declared twice, imported across a module boundary a third time) plus a `cents()` that owns the per-1M-to-cents arithmetic, `Role → &'static str`, the `force_json` prompt suffix, the 128k `max_tokens` ceiling, and `json_response`, which does the non-2xx-bail / debug-dump / deserialize sequence for every backend. Net −65 lines.
- **Fix**: the Anthropic streaming path divided by `1_000_000` where the field is cents, so every streamed response under-reported its cost 100×. Only the rest path was right; both go through `Cost::cents` now.
- `ClaudeModel::max_tokens`/`OpenAiModel::max_tokens` are gone. Both ignored `self` and returned the same constant, so the per-model shape was fiction; reintroduce a method when a model actually differs.
- Deepseek and Ollama now send the same `force_json` suffix as the others, which additionally names markdown fences.
- **New**: `openai` backend module, hitting `https://api.openai.com/v1/chat/completions` with the three GPT-5.6 tiers (`sol`/`terra`/`luna`). Only `terra` and `luna` are reachable through `Model`; `Sol` exists so a response can still be priced if one comes back.
- **Breaking**: `Model::Fast` → `gpt-5.6-luna` and `Model::Medium` → `gpt-5.6-terra`. `Medium` and `Slow` had both resolved to `claude-opus-5`, so the ladder had no middle rung. Both tiers now need `OPENAI_API_KEY` (or `config.openai_token`).
- **New**: `config::AppConfig::openai_token`.
- `Model::Fast` regains file/image support, walking back the note under v3.1.0 — file attachments go up as data URIs (`image_url` for images, `file` for PDFs, decoded text for everything else).
- `Client::stop_sequences` is applied to the returned text rather than the request: gpt-5.6 rejects the `stop` param outright. Output is the same, but generation past the cut is still billed.
- `ThinkingLevel` maps onto the flat `reasoning_effort` param (`none`/`low`/`medium`/`high`). The `xhigh`/`max` efforts and `reasoning.mode: "pro"` are unreachable — `ThinkingLevel` can't express either.
- The DeepSeek backend is kept but unreachable, so it builds with dead-code warnings.
- **Breaking**: a model whose provider has no key no longer panics. Key resolution moved off `Client::new`/`Client::model` and onto the request, and reports [`MissingToken`] — a `thiserror` + `miette` diagnostic naming the provider, the model that wanted it, and the three places the key can come from.
- **New**: `Client::claude_token`/`deepseek_token`/`openai_token` builders, so a consumer hands over keys for the providers it plans to use instead of assembling an `AppConfig`.
- **Fix**: the Anthropic backend never looked at the HTTP status. A rejected key returned `Ok` with an empty string — an invalid key was indistinguishable from a model with nothing to say. Both the streaming and rest paths now fail on non-2xx, as the OpenAI and Ollama backends already did.

## v3.1.0

- **Breaking**: `Model::DeepSeek` is gone. A provider is not a tier — `Model::Fast` now points at `deepseek-v4-flash`, so DeepSeek is reached by asking for the cheap remote tier rather than by naming it.
- **Breaking**: `Model` is `#[non_exhaustive]`.
- `Model::Fast` needs `DEEPSEEK_KEY` (or `config.deepseek_token`) instead of `CLAUDE_TOKEN`, and drops file/image support — the DeepSeek backend is text-only.
- Not published: the account 402s on every call until it is funded.
- **New**: `transcribe(path)` — speech to text, run locally through `whisper-cli`. The first capability here that reaches no provider, so it has no key, no balance and no rate limit; it sits outside `Client`/`Model` because it is audio-in/text-out rather than a conversation. Takes any format ffmpeg decodes, model from `$WHISPER_MODEL` or the whisper-cpp data dir.
- **New**: `ask_llm --transcribe <AUDIO>` on the CLI. `QUESTION` became optional to allow it, and is still required without the flag.
- **New**: `tts::run` — text to speech, moved here wholesale from book_parser so both audio directions live in one place. The progress bar did not come with it: the lib reports `(done, total)` through a callback and the caller renders it, so `indicatif` stays out of the dependency tree. Non-progress script output goes to `tracing` rather than stdout.

---

## v2.1.x and earlier

No changelog kept prior to v2.2.0.
