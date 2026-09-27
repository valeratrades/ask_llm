# Architecture

## Model graph
A `Model` tier names an entry node, not a deployment. A call walks up from the entry until a node answers.

The source of truth is `Node` in `src/graph.rs`: `Node::next` is the edge table, an exhaustive `match` with successors ordered cheapest first. The drawing below shows the *shape* only. **Every model name in it is an example of what sat on that node when this was written; the deployments get replaced as providers ship and retire models, and this file is not updated when they do.**
```
 Translate ─► [local translation model] ─┐                  (alternate path: nothing transitions into it)
                                         ▼
 Cheap ─────► [local small model] ─► [cheap remote model] ─► [Claude, small] ─► [Claude, mid] ─► [Claude, top]
                                        ▲ Fast                                   ▲ Medium, Slow   ▲ PriceInsensitive
 Video ─────────────────────────────────┘  (alternate path; enters above the base, which can't read frames)

 e.g. local translation = Ollama translategemma, local small = Ollama qwen, cheap remote = OpenAI luna,
      Claude small/mid/top = sonnet/opus/fable
```

The walk:
- a recoverable failure moves on to the next node; it never retries in place. Backoff is the caller's, and the returned `Error::Recoverable` / `Error::Unrecoverable` says whether backoff can help.
- an unrecoverable failure about the account (key, balance, region, policy) marks the provider dead for the rest of the call; one about the node (model not served, context too long, request shape unsupported) skips only that node.
- nothing about a failure outlives the call.

## Invariants
- Edges only go up in capability. A call is never answered by a weaker model than the one asked for.
- `Error::Unrecoverable` means no node on the path can answer this call. If any node failed on something that clears on its own, the call is `Error::Recoverable`.
