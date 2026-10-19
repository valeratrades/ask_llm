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
