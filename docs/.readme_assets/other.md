## Semver
Note that due to specifics of implementation, minor version bumps can change effective behavior by changing what model processes the request. Only boundary API changes will be marked with major versions.

`ask_llm::Error` is boundary API: adding a variant to it, or to any of `Transport`/`Api`/`Cli`, is a minor bump (all are `#[non_exhaustive]`), but moving a failure from one variant to another is a major one.
