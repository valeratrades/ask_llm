# Architecture

## Model graph
Each `Model` is a tier and stands for one `Source`: a model of a provider's. A call enters at the tier asked for and walks down until one answers, or through the chain its caller gave, and nothing else.

The source of truth is `Model::next` in `src/model.rs`: one successor per variant, so every path is a chain. The drawing below shows the *shape* only; **which deployment sits on a tier is `From<Model> for Source`**, and changes as providers ship and retire models.
```
 PriceInsensitive ─► Slow ─► Medium ─► Fast ─► Cheap
                                   Translate ─┘         Video: alone, nothing below it reads frames
```

The walk:
- a recoverable failure moves on; it never retries in place. Backoff is the caller's, and the returned `Error::Recoverable` / `Error::Unrecoverable` says whether backoff can help.
- down the tiers, an unrecoverable failure about the account (key, balance, region, policy) marks the provider dead for the rest of the call; one about the node (model not served, context too long, request shape unsupported) skips only that node.
- in a chain every source is tried, so an attempt's place is its source's: how a caller tells whose credential failed.
- nothing about a failure outlives the call.

```
 ask, conversation ─► Response
 step ─► Step ── resume ─► Step ── resume ─► …      on the Source inside, alone: Error::Interrupted when it fails
```
A refusal is an answer to `step`, with its usage; `ask` and `conversation` have no word for it and fail `Refused`.

## Wires
`src/wire.rs` holds every http call: genai writes the body and reads the answer, `src/providers/*` hold what it does not do as we need.
```
 Claude ─ OAuth token ───────────────────► http    Bearer + oauth beta; messages and system are ours
        └ device login ─ plain ask ──────► claude -p   alone may refresh the login
                       └ step, images, stop sequences ► http on its access token, until that expires
 OpenAI ─────────────────────────────────► http    Responses
 Ollama ─────────────────────────────────► http    /api/chat
```
genai's types stay out of the public API: it is pre-1.0.

## Invariants
- `Model::next` only goes down. Nothing checks this mechanically.
- `Error::Unrecoverable` means no node on the path can answer this call. If any node failed on something that clears on its own, the call is `Error::Recoverable`; an Ollama nobody serves, reached only as a stand-in, does not.
- A conversation with tool calls is answered by the source that made them.
- A failure a person has to act on (credential, balance, region, retired model) is an error event, so that the walk does not hide it; on a `Source::theirs` a warning, its owner not being ours to page. What was never set up here is info, the rest warnings.
- A price we do not hold is `None`.

## Generation and segmentation
`video` (Vertex Veo) and `segment` (fal SAM 3, local SAM 2) sit outside the graph: a generated clip or a mask is not interchangeable across models, so the caller names one model and gets it or an error. No walk, no retry. `Model::Segment` and `Model::SegmentLocal` are tiers for naming the best mask model of each kind; a conversation sent to them fails `Unsupported`.

## Usage log
Every call `Client` answers appends one JSON line to `$XDG_DATA_HOME/ask_llm/usage/<app>/<YYYY-MM>.jsonl`; failed calls are not logged, and one that cannot be written is an error event. `<app>` is the file stem of `argv[0]`, not of `current_exe()`: a `nix-run-cached` script is exec'd from a content-hashed binary with `argv[0]` set to the script path, so its stem is the script name. `requested` is the `Model` asked for, or the first model of a chain, `model` the deployment that answered.

## Claude accounts
The device's Claude logins live in `~/.claude/accounts`, owned by `providers::claude::accounts`: `<name>/.credentials.json` is the login, `<name>/meta.json` the last usage snapshot (fetched only while the access token is unexpired: refreshing another process's token would bypass its lock), `current` a relative link to the account a new claude starts on, and `running` one to the account every running claude follows. Both are relinked atomically and only by `Accounts::rotate` (both) and `Accounts::rotate_new` (`current` alone, so running claudes keep their prompt caches, which don't carry across accounts).

Everything else on the device goes through `ask_llm dev claude-account <current | running | rotate N [--new] | list [--json]>`: the `claude` launcher, `cl rotate-acc` and the rate-limit menu patched into the claude binary. Each patched claude watches `running` and follows it in place, which is the one thing that can't move here: only a process can move itself onto another account. A Claude call without a token runs on `current`.

Rotation is never a step of the model walk: rotating on a rate limit and asking again would be a retry in place.
