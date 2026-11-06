# Architecture

## Model graph
Each `Model` variant owns one deployment and is a node. A call enters at the `Model` asked for and walks up until one answers.

The source of truth is `Model::next` in `src/model.rs`: one successor per variant, so every path is a chain. The drawing below shows the *shape* only. **Every deployment name in it is an example of what sat on that variant when this was written; deployments get replaced as providers ship and retire models, and this file is not updated when they do.**
```
 Translate ─► Cheap ─► Fast ─► Medium ─► Slow ─► PriceInsensitive
                        Video ─┘                  (alternate entry; reads frames, rejoins at Medium)
```

The walk:
- a recoverable failure moves on to the next node; it never retries in place. Backoff is the caller's, and the returned `Error::Recoverable` / `Error::Unrecoverable` says whether backoff can help.
- an unrecoverable failure about the account (key, balance, region, policy) marks the provider dead for the rest of the call; one about the node (model not served, context too long, request shape unsupported) skips only that node.
- nothing about a failure outlives the call.

## Invariants
- `Model::next` only goes up in capability. A call is never answered by a weaker model than the one asked for. Nothing checks this mechanically.
- `Error::Unrecoverable` means no node on the path can answer this call. If any node failed on something that clears on its own, the call is `Error::Recoverable`.

## Generation and segmentation
`video` (Vertex Veo) and `segment` (fal SAM 3, local SAM 2) sit outside the graph: a generated clip or a mask is not interchangeable across models, so the caller names one model and gets it or an error. No walk, no retry. `Model::Segment` and `Model::SegmentLocal` are tiers for naming the best mask model of each kind; their `next` is `None`, and a conversation sent to them fails `Unsupported`.

## Usage log
Every call `Client` answers appends one JSON line to `$XDG_DATA_HOME/ask_llm/usage/<app>/<YYYY-MM>.jsonl`; failed calls are not logged. `<app>` is the file stem of `argv[0]`, not of `current_exe()`: a `nix-run-cached` script is exec'd from a content-hashed binary with `argv[0]` set to the script path, so its stem is the script name. `requested` is the `Model` asked for, `model` the deployment that answered.

## Claude accounts
The device's Claude logins live in `~/.claude/accounts`, owned by `providers::claude::accounts`: `<name>/.credentials.json` is the login, `<name>/meta.json` the last usage snapshot (fetched only while the access token is unexpired: refreshing another process's token would bypass its lock), `current` a relative link to the account a new claude starts on, and `running` one to the account every running claude follows. Both are relinked atomically and only by `Accounts::rotate` (both) and `Accounts::rotate_new` (`current` alone, so running claudes keep their prompt caches, which don't carry across accounts).

Everything else on the device goes through `ask_llm dev claude-account <current | running | rotate N [--new] | list [--json]>`: the `claude` launcher, `cl rotate-acc` and the rate-limit menu patched into the claude binary. Each patched claude watches `running` and follows it in place, which is the one thing that can't move here: only a process can move itself onto another account. A Claude call without `claude_token` runs `claude -p` on `current`.

Rotation is never a step of the model walk: rotating on a rate limit and asking again would be a retry in place.
