<!-- straitjacket-allow-file[:duplication] — the prerequisites and build steps
     necessarily restate the throwaway/php-hello spike's setup; this is the
     promoted in-tree binding, and the spike stays as the historical record, so
     the overlap is intentional. -->

# atilla-php — PHP native extension (M0 scaffold)

The PHP binding for atilla, built in Rust with
[`ext-php-rs`](https://github.com/davidcole1340/ext-php-rs) as a loadable `.so`.

This is milestone **M0**: the native path proven in-tree. It is not a feature —
it exists to show that Rust compiles an ext-php-rs extension, PHP loads it, and
a PHP call reaches through the `atilla-core` façade and back. PHP goes first
because it is the weirdest host (synchronous, request-scoped, thread-bound); if
the façade survives PHP, easier hosts follow.

## What it exposes

- `class Atilla` with a static method `Atilla::version(): string`.

`Atilla::version()` returns the atilla engine version by calling
`atilla_core::version()` — a real call through the façade crate, not a string
baked into this binding. PHP therefore sees the same authoritative version as
the Rust core (the workspace version, `0.1.0` today).

`Session::open` is stubbed as a `TODO(M1)` in `src/lib.rs`, pending the sibling
session-JSONL work. M0 ships no fake session.

## Toolchain (verified)

| Component      | Version                                    |
|----------------|--------------------------------------------|
| PHP            | 8.4.19 (cli), **NTS** (non-thread-safe)    |
| PHP API        | 20240924                                   |
| ext-php-rs     | 0.13.1 (pinned exactly, `=0.13.1`)         |
| Rust / cargo   | 1.94.1                                      |
| clang/libclang | 18.1.3 (needed by ext-php-rs' bindgen)     |

**NTS vs ZTS.** This extension is built and tested against **NTS**
(non-thread-safe) PHP — `php -i | grep 'Thread Safety'` reports `disabled`. A
`.so` is locked to one PHP major.minor and to the NTS/ZTS choice it was built
against; it will refuse to load on a mismatch. ext-php-rs runs `bindgen` over
the installed PHP headers at build time, so the extension is compiled against
whatever `php-dev` headers are present.

## Prerequisites (Debian/Ubuntu)

```bash
sudo apt-get update && sudo apt-get install -y \
    php php-dev php-cli pkg-config libclang-dev clang build-essential
```

`php-dev` provides `php-config` and the headers under `/usr/include/php/<api>/`.
`libclang-dev` / `clang` are required because ext-php-rs runs `bindgen` over
`main/php.h`.

## Build and test

From this directory:

```bash
cd bindings/php
./test.sh            # builds the cdylib (debug) and runs the PHP assertions
./test.sh release    # optimized build instead
```

`test.sh` cargo-builds the `.so`, reads the real `atilla-core` version from its
Cargo metadata, then loads the extension with
`php -d extension=<abs-path>/libatilla_php.so` and runs `test.php`, which asserts
`Atilla::version()` equals that version and exits `0`. The script is
`set -euo pipefail`, so any failure — build, load, mismatch, or non-zero PHP
exit — fails loudly.

To build only:

```bash
cargo build              # -> target/debug/libatilla_php.so
```

The Done check in the milestone plan phrases this as `cargo build -p atilla-php`;
run that from inside `bindings/php` (this crate is its own workspace — see
below), where `-p atilla-php` selects it.

### Naming gotcha

The **extension name** PHP registers is the crate *package* name, `atilla-php`
(hyphenated) — that is what `extension_loaded()` and `php -m` report. The **`.so`
file** is `libatilla_php.so` (from `[lib] name = "atilla_php"`, underscored).
Loading via the full `extension=/abs/path/libatilla_php.so` works directly. To
load it by short name from `php.ini`, the `.so` must sit on `extension_dir` and
be referenced by file name.

## Workspace decision: standalone crate, not a workspace member

This crate carries an **empty `[workspace]` table** in its `Cargo.toml`, so cargo
treats it as its own single-crate workspace instead of attaching it to the
workspace rooted at the repository top level. It still depends on the engine via
a normal path dependency (`atilla-core = { path = "../../crates/atilla-core" }`),
which resolves fine across the workspace boundary — atilla-core keeps inheriting
its version and edition from the root `[workspace.package]`.

Why standalone rather than a member like `crates/atilla-napi`:

- ext-php-rs pulls in `bindgen`, which needs **libclang and the PHP dev
  headers** at build time. If this crate were a workspace member, a plain
  `cargo build` / `cargo test` / `cargo clippy` at the repository root — and the
  shared `rust` CI job — would try to compile it and fail on any machine or
  runner without PHP headers installed. napi-rs has no such system dependency,
  which is why the Node binding can live in the workspace but this one should
  not.
- Keeping it standalone means the root build stays clean and the PHP toolchain
  is required only when you actually build the PHP binding. The dedicated
  `php` CI job (`.github/workflows/php.yml`) installs the PHP headers and builds
  this crate on its own.

Verified: `cargo build` at the repository root does not touch this crate, and
`./test.sh` here builds and passes.

## Async / fork note (for later milestones)

M0 has no async surface, so there is no tokio runtime here. When one is needed,
follow the spike's rule (`throwaway/php-hello`): create **one** runtime lazily,
**per process**, and only **after** any php-fpm fork — never at module init in
a pre-forking master — then `block_on` from the calling PHP thread.
