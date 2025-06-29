# ask_llm
![Minimum Supported Rust Version](https://img.shields.io/badge/nightly-1.86+-ab6000.svg)
[<img alt="crates.io" src="https://img.shields.io/crates/v/ask_llm.svg?color=fc8d62&logo=rust" height="20" style=flat-square>](https://crates.io/crates/ask_llm)
[<img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-66c2a5?style=for-the-badge&labelColor=555555&logo=docs.rs&style=flat-square" height="20">](https://docs.rs/ask_llm)
![Lines Of Code](https://img.shields.io/badge/LoC-609-lightblue)
<br>
[<img alt="ci errors" src="https://img.shields.io/github/actions/workflow/status/valeratrades/ask_llm/errors.yml?branch=master&style=for-the-badge&style=flat-square&label=errors&labelColor=420d09" height="20">](https://github.com/valeratrades/ask_llm/actions?query=branch%3Amaster) <!--NB: Won't find it if repo is private-->
[<img alt="ci warnings" src="https://img.shields.io/github/actions/workflow/status/valeratrades/ask_llm/warnings.yml?branch=master&style=for-the-badge&style=flat-square&label=warnings&labelColor=d16002" height="20">](https://github.com/valeratrades/ask_llm/actions?query=branch%3Amaster) <!--NB: Won't find it if repo is private-->

Layer for llm requests, generic over models and providers

## Usage
### Lib
Provides 2 simple primitives:

`oneshot` and `conversation` functions, which follow standard logic for llm interactions, that most providers share.

Then the model is automatically chosen based on whether we care about cost/speed/quality. Currently this is expressed by choosing `Model::`{`Fast`/`Medium`/`Slow`}, from which we pick a model as hardcoded in current implementation. 

When used as a lib, import with
```toml
ask_llm = { version = "*", default-features = false }
```
as `clap` would be brought otherwise, as it is necessary for `cli` part to function.

### Cli
Wraps the lib with clap. Uses `oneshot` by default, if needing `conversation` - read/write it from/to json files.

## Semver
Note that due to specifics of implementation, minor version bumps can change effective behavior by changing what model processes the request. Only boundary API changes will be marked with major versions.


<br>

<sup>
	This repository follows <a href="https://github.com/valeratrades/.github/tree/master/best_practices">my best practices</a> and <a href="https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md">Tiger Style</a> (except "proper capitalization for acronyms": (VsrState, not VSRState) and formatting).
</sup>

#### License

<sup>
	Licensed under <a href="LICENSE">Blue Oak 1.0.0</a>
</sup>

<br>

<sub>
	Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be licensed as above, without any additional terms or conditions.
</sub>
