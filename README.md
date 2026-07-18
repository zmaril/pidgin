# atilla

A Rust command-line tool, laid out as a Cargo workspace so the engine and the
shell stay separate.

The workspace splits into two crates:

- **`atilla-core`** — the library. All the real work lives here so it stays
  testable without going through argv.
- **`atilla-cli`** — a thin shell that parses arguments (with
  [clap](https://docs.rs/clap)) and hands off to `atilla-core`. It builds the
  `atilla` binary.

This is early scaffolding: the CLI exposes a single `run` placeholder command
while the actual surface is designed. Everything below already works, so new
functionality slots into an established shape rather than a blank repo.

## Install

Build from a checkout with a recent stable Rust toolchain:

```sh
git clone https://github.com/zmaril/atilla
cd atilla
cargo build --release
```

The binary lands at `target/release/atilla`. To install it onto your `PATH`:

```sh
cargo install --path crates/atilla-cli
```

## Usage

```sh
atilla run       # run the engine (placeholder for now)
atilla --help    # list commands
atilla --version # print the version
```

## Development

```sh
cargo fmt --all           # format
cargo clippy --all-targets -- -D warnings  # lint
cargo test                # run the tests
```

CI runs the same three on every push and pull request, alongside the fleet
housekeeping, straitjacket, codespell, and vale checks.

## Contributing

Pull request titles follow
[Conventional Commits](https://www.conventionalcommits.org)
(`type(scope): summary`) — CI enforces it. Keep `cargo fmt`, `cargo clippy`,
and `cargo test` green before opening a PR.

## License

[MIT](LICENSE) © Zack Maril
