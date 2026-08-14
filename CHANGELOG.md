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

---

## v2.1.x and earlier

No changelog kept prior to v2.2.0.
