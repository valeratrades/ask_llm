### Lib
Provides 2 simple primitives:

`oneshot` and `conversation` functions, which follow standard logic for llm interactions, that most providers share.

Then the model is automatically chosen based on whether we care about cost/speed/quality. Currently this is expressed by choosing `Model::`{`Fast`/`Medium`/`Slow`}, from which we pick a model as hardcoded in current implementation. 

When used as a lib, import with
```toml
ask_llm = { version = "*", default-features = false }
```
as `clap` would be brought otherwise, as it is necessary for `cli` part to function.

`Client::watch` reads a recording into timed text — what is said (local whisper, unless handed over) and what is shown, off the frames where the picture changes — with `Model::Video`.

### Cli
Wraps the lib with clap. Uses `oneshot` by default, if needing `conversation` - read/write it from/to json files.
`ask_llm --watch <MEDIA> --footage <screen|filmed> --frames <DIR>` prints a recording's timed lines.
