# atilla

A continually updating mirror of [pi](https://github.com/earendil-works/pi),
rewritten in Rust. atilla tracks upstream pi and re-implements it crate by
crate, keeping pace as pi evolves.

pi is an open-source agent harness — a self-extensible coding agent, an agent
runtime with tool calling and state management, and a unified multi-provider
LLM API — written in TypeScript and running on Node.js. atilla is the same
thing in Rust.

## Why

- **A native Rust core.** pi's runtime, tool calling, and multi-provider LLM
  surface, re-implemented in Rust so it can ship as a single static binary and
  embed anywhere Rust does.
- **Native extensions in every language.** The Rust core is meant to be
  re-exposed through first-class native extensions — PHP first (a PECL-style
  extension via [ext-php-rs](https://github.com/davidcole1340/ext-php-rs)),
  then Python, Node, Ruby, and others — with the goal of exposing pi's full API
  in each language rather than wrapping a subprocess.
- **Correctness pinned to pi.** The Rust mirror is only "done" for a given
  piece when it passes pi's own test suite. pi's tests are the specification;
  matching them is the definition of correctness.

## How the rewrite works

This is an AI-accelerated hand-rewrite, not a transpiler. pi's TypeScript
source and its test suite are used as the executable spec: read the upstream
behavior, re-implement it idiomatically in Rust, and prove it against pi's
tests. Nothing is machine-translated from TypeScript to Rust — the output is
hand-written Rust that happens to be produced quickly.

## Layout

The workspace is a Cargo workspace so the engine and the shell stay separate:

- **`atilla-core`** — the library. All the real work lives here so it stays
  testable without going through argv.
- **`atilla-cli`** — a thin shell that parses arguments (with
  [clap](https://docs.rs/clap)) and hands off to `atilla-core`. It builds the
  `atilla` binary.

The workspace has grown well past scaffolding: the crates now mirror pi's
packages (`ai`, `agent`, `coding-agent`, `tui`, `orchestrator`) behind the
`atilla-core` façade, plus the `atilla-napi` bridge that fronts the conformance
harness. New functionality slots into this established shape rather than a
blank repo.

## Status

Active port, well past research phase. All of pi's major packages are ported —
`ai` (providers, per-dialect codecs, OAuth, the model catalog, the Models
wrapper), the `agent` tier, the `coding-agent` core (glue, config, exec tools,
compaction, `SessionManager`), and the `tui` components. **`orchestrator` is the
remaining in-progress package**, alongside coding-agent's interactive mode and
the full jiti extension engine.

What is ported is the pure logic of those packages. The I/O layer is not: there
is no HTTP client in the workspace, and the CLI has no model runtime wired to
it, so **the `atilla` binary cannot currently run an agent** — see
[Usage](#usage). Treat the port as a library that passes tests, not a working
harness.

Correctness is measured by running pi's **own unmodified** test suite against
the Rust port through the `vendor/pi` overlay. The honest headline is
**rust-backed passing: 258/3777 (6.8%)** — cases in files whose module under
test is a native (Rust addon) module; raw all-pass is a secondary
**2919/3777**, inflated by unflipped TypeScript that passes without touching any
Rust. **Native modules: 21/397.** A separate black-box signal runs pi's CLI
tests, repointed at `target/release/atilla`: **CLI conformance 15/15** — note
that these cover metadata and error paths (`--version`, `--help`, unknown-model
handling), not an agent turn.

- **`notes/`** — research reports and design notes on pi's architecture and the
  port, landed via pull requests.
- **`conformance/`** — the harness (shims, codegen, manifest) that runs pi's
  suite against the Rust port; the baseline lives in `conformance.json`.

## Install

Build from a checkout with a recent stable Rust toolchain:

```sh
git clone --recurse-submodules https://github.com/zmaril/atilla
cd atilla
cargo build --release
```

The `vendor/pi` submodule is required: the skills tests load fixtures from it,
and `cargo test` fails on a clone without it. If you already cloned without
`--recurse-submodules`, run `git submodule update --init vendor/pi`.

The binary lands at `target/release/atilla`. To install it onto your `PATH`:

```sh
cargo install --path crates/atilla-cli
```

## Usage

**The binary cannot yet run an agent.** There is no HTTP client in the
workspace, so no provider call can be made; the `ai` crate is codecs, auth
token handling, and cost math without a transport underneath it. Every path
that needs a model dead-ends today:

| command | works | output |
| --- | --- | --- |
| `atilla --version` | yes | the version |
| `atilla --help` | yes | pi's full help text |
| `atilla --list-models` | no | nothing, exit 0 |
| `atilla list` | no | `No models available…` |
| `atilla -p "explain this repo"` | no | `No models available…` |
| `atilla` | no | `interactive mode is not yet implemented` |

The help text is pi's, carried over verbatim — the providers, tools, and flags
it advertises are the port's target surface, not what the binary does now. What
works today is the library: the pure-logic tiers (parsing, codecs, config,
session formats, TUI components, cost accounting) under `cargo test`.

## Development

```sh
scripts/dev.sh            # format-check + lint + test, the way CI does
```

Or run the gates individually:

```sh
cargo fmt --all           # format
cargo clippy --all-targets -- -D warnings  # lint
cargo test                # run the tests
```

CI runs the same three on every push and pull request, alongside the fleet
housekeeping, Straitjacket, codespell, and vale checks.

## Contributing

Pull request titles follow
[Conventional Commits](https://www.conventionalcommits.org)
(`type(scope): summary`) — CI enforces it. Keep `cargo fmt`, `cargo clippy`,
and `cargo test` green before opening a PR.

## Credits

atilla is a Rust port of [pi](https://github.com/earendil-works/pi) by
[earendil-works](https://github.com/earendil-works) (Mario Zechner and
contributors). All credit for the design and behavior atilla mirrors belongs to
the pi authors. pi is licensed under the MIT License.

## License

[MIT](LICENSE) © Zack Maril. pi is separately licensed under the MIT License ©
Mario Zechner.
