# pidgin PHP demo

## What this is

A plain-PHP webpage that runs pi natively through the `Pidgin\Session`
extension — a chat box in the browser, each message routed through the loaded
Rust extension into the real agent loop and back.

## Build the extension

```bash
cd bindings/php && cargo build
```

That produces `target/debug/libpidgin_php.so`. The default build is offline
(faux) and needs no API key. For the live path (real Anthropic turns) build with
the transport feature:

```bash
cargo build --features native-http
```

Dependencies: PHP 8.4 (NTS, non-thread-safe), `php-dev` (headers +
`php-config`), `libclang-dev` and `clang` (ext-php-rs runs `bindgen` over the
PHP headers), and a Rust toolchain.

## Run the demo (offline / faux)

```bash
./demo/serve.sh
```

Then open http://127.0.0.1:8080. No API key needed — the badge reads FAUX and
replies are deterministic echoes from the offline faux provider (the reply
contains `You said: <your message>`).

## Run against a real key (live)

```bash
cargo build --features native-http
export ANTHROPIC_API_KEY=sk-...
./demo/serve.sh
```

The badge flips to LIVE and messages hit the real Anthropic API through the
native transport. The live path depends on the `native-http` transport; if calls
return HTTP 400 it may require the in-flight header fix (Zack knows the context).

## php.ini / loading the extension

The extension is a loadable `.so`; there are two ways to load it.

- **Per-invocation (what `serve.sh` and `test.sh` do), no ini edit** — pass the
  absolute path on the command line:

  ```bash
  php -d extension=/abs/path/to/bindings/php/target/debug/libpidgin_php.so ...
  ```

  `serve.sh` resolves that path for you (it prefers `target/release`, else
  `target/debug`).

- **Permanently, via `php.ini`** — add a line with the absolute path:

  ```ini
  extension=/abs/path/to/bindings/php/target/debug/libpidgin_php.so
  ```

  Find which ini file your PHP reads with `php --ini`. The absolute path shape is
  `<repo>/bindings/php/target/{debug,release}/libpidgin_php.so`.

## API surface

The extension exposes (documented here for the Python-binding team):

- `Pidgin::version(): string` — the pidgin engine version, read through the
  `pidgin-core` façade.
- `class Pidgin\Session` with:
  - `__construct(?string $model = null, ?string $provider = null, ?string $systemPrompt = null, ?bool $faux = null)`
    — arguments are **positional** (ext-php-rs 0.13.1 does not support named-arg
    skipping of the `Option` params). The 4th arg `$faux = true` forces the
    offline canned provider (no API key required); `false` (or a real key) uses
    the live path.
  - `->send(string $message): string` — blocking; returns the assistant's full
    reply text. Multi-turn context is retained across `send()` calls on the same
    `Session` object.
  - `->sendStream(string $message): iterable` — returns a foreach-able array of
    text deltas; concatenating them yields the full reply.

Per-Session multi-turn context (one `Session` carried across several `send()`
calls) is demonstrated in `test.php`. This web demo uses one `Session` per
message for simplicity, because the PHP built-in server isolates each request.
