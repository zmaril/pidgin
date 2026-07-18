# php-hello — a PHP native extension written in Rust (spike)

Throwaway spike for the **pi → Rust rewrite** project. Proves we can build a
loadable PHP extension in Rust with [`ext-php-rs`](https://github.com/davidcole1340/ext-php-rs)
and call it from PHP.

## What it exposes

- `pi_hello(string $name): string` — returns `"Hello, {name}, from Rust!"`
- `pi_add(int $a, int $b): int` — integer add
- `class PiGreeter { __construct(string $prefix); greet(string $name): string }`

## Toolchain used (this spike)

| Component    | Version                          |
|--------------|----------------------------------|
| PHP          | 8.4.19 (cli), **NTS** (non-thread-safe) |
| PHP API      | 20240924                         |
| ext-php-rs   | 0.13.1                           |
| Rust / cargo | 1.94.1                           |
| clang/libclang | 18.1.3 (needed by bindgen)     |

`ext-php-rs` uses `bindgen` against the installed PHP headers, so the extension
is compiled against whatever PHP dev headers are present at build time. The
resulting `.so` is tied to that PHP major.minor + NTS/ZTS combo.

## Prerequisites (Debian/Ubuntu)

```bash
sudo apt-get update && sudo apt-get install -y \
    php php-dev php-cli pkg-config libclang-dev clang build-essential
```

`php-dev` provides `php-config` and the headers under
`/usr/include/php/<api>/`. `libclang-dev`/`clang` are required because
`ext-php-rs` runs `bindgen` over `main/php.h`.

## Build

```bash
cd throwaway/php-hello
cargo build            # debug is fine for a spike; add --release for optimized
```

Output: `target/debug/libphp_hello.so`
(the file name comes from `[lib] name = "php_hello"`).

## Run the test

```bash
php -d extension=$(pwd)/target/debug/libphp_hello.so test.php
```

Expected output (exit code 0):

```
PASS  pi_hello => 'Hello, Zack, from Rust!'
PASS  pi_add => 42
PASS  PiGreeter::greet => 'spike: hello world (from Rust)'

ALL TESTS PASSED
```

### Naming gotcha

The **extension name** registered with PHP is the crate *package* name,
`php-hello` (hyphenated) — that is what `extension_loaded()` and `php -m`
report. The **`.so` file** is `libphp_hello.so` (from the `[lib] name`,
underscored). Loading via the full `extension=/abs/path/libphp_hello.so` works
directly; no rename/symlink needed. If you want to load it by short name from
`php.ini`, the `.so` must be on the `extension_dir` and referenced by file name.

## Standalone crate note

This crate carries an empty `[workspace]` table in its `Cargo.toml` so cargo
treats it as its own workspace and never tries to attach it to a workspace at
the repo root. That keeps the spike self-contained under `throwaway/` without
editing shared files.

---

## Findings

### a. Where a tokio async runtime lives inside a PHP process

PHP execution is synchronous and request-scoped: a request runs on one thread,
top to bottom, then tears down. A tokio runtime does **not** map onto that
model cleanly. The workable pattern is:

- Create **one** multi-threaded `tokio::runtime::Runtime` **lazily, once per OS
  process** (e.g. a `OnceCell`/`OnceLock` in module globals), *not* per request.
  Tokio worker threads live in the background for the life of the process.
- Each PHP call that needs async work does `runtime.block_on(future)` — the
  calling PHP thread blocks until the future resolves. From PHP's point of view
  the function is still synchronous; concurrency only helps if a single call
  fans out to many awaits internally.
- **ZTS implications**: this spike's PHP is NTS, so there is exactly one
  interpreter per process and sharing a single runtime is safe. Under ZTS
  (thread-safe PHP, e.g. some Apache/worker MPMs) multiple interpreters share
  the process; the runtime is still per-process, but anything touching PHP
  values must respect thread-local storage and never move `zval`s across
  threads. Keep the runtime doing pure-Rust work and marshal results back on
  the PHP thread.
- **fork hazard (php-fpm)**: php-fpm pre-forks worker processes. A tokio runtime
  created in the master **before** fork is broken in the children (worker
  threads/epoll fds don't survive fork). Always create the runtime lazily on
  first use *inside the worker*, after fork — never at module init in the
  master. Same caution for any other tool that pre-forks.

### b. ext-php-rs maturity and PHP 8.x support

- **Support matrix**: 0.13.x targets PHP **8.0–8.4**. This spike built and ran
  clean against **PHP 8.4.19** with **ext-php-rs 0.13.1**, so 8.4 works today.
- **API stability**: pre-1.0 and still moving. The attribute API churns between
  minor versions — e.g. the class-rename attribute is `#[php(name = "…")]` on
  the 0.14/git line but is **not** available in 0.13.1 (we hit
  "cannot find attribute `php`" and dropped it, taking the struct name). Expect
  to pin an exact version and to touch macro call-sites on upgrades.
- Actively maintained, real-world users, good docs, but treat it as a moving
  target: pin the version, and budget for small migration work each bump. There
  is no stability guarantee until 1.0.

### c. Prebuilt-binary distribution: PECL vs Composer

- **PECL** expects a C extension built through `phpize`/`configure`/`make`. A
  Rust `.so` doesn't fit that pipeline without wrapping the cargo build in a
  fake C build harness — awkward and fragile. PECL is really not designed to
  ship a Rust-produced binary.
- **Composer** is a PHP-userland package manager and **cannot install native
  extensions** at all — it can ship PHP glue/stubs but not the `.so`.
- **Realistic story**: distribute **prebuilt `.so` per (platform × PHP
  major.minor × NTS/ZTS)** — that's a real build/release matrix (linux glibc vs
  musl, macOS arm64/x86, each PHP 8.0–8.4, NTS and ZTS). ext-php-rs supports a
  "static php-cli" style build (embed into a self-contained PHP binary), which
  sidesteps the extension-loading problem for a CLI tool by shipping our own
  PHP. **Risk**: the combinatorial matrix is the main cost; a `.so` built for
  8.3-NTS will refuse to load on 8.4 or on a ZTS build, so the release
  automation and version detection are where the real work is.

### Top 3 risks (one line each)

1. Distribution matrix: a `.so` is locked to one PHP major.minor × NTS/ZTS ×
   platform — no clean PECL/Composer path, so you ship a build matrix or a
   static php-cli.
2. ext-php-rs is pre-1.0: pinned versions and macro-API churn between minors
   mean recurring migration cost.
3. Async/fork correctness: the tokio runtime must be created per-process and
   *after* php-fpm fork, blocking the PHP thread on `block_on` — easy to get
   subtly wrong.
