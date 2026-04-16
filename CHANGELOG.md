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

---

## v2.1.x and earlier

No changelog kept prior to v2.2.0.
