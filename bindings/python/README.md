<!-- straitjacket-allow-file[:duplication] — the prerequisites and build steps
     necessarily restate the sibling bindings/php README's setup (venv/build/run
     boilerplate that every native binding repeats); the overlap is intentional. -->

# pidgin-python — Python native extension

The Python binding for pidgin, built in Rust with
[PyO3](https://pyo3.rs) + [maturin](https://www.maturin.rs) as a loadable
CPython extension module. It is the **sibling of `bindings/php`**: a standalone
crate outside the root workspace that reaches the engine through path
dependencies and exposes a small, hand-written surface.

Where the PHP binding proves only `Pidgin::version()`, this binding also drives
a **real agent turn** through `pidgin_agent::agent_loop` — offline via the faux
provider (default), or live against Anthropic.

## What it exposes

```python
import pidgin

pidgin.version()                 # -> str: the pidgin engine (workspace) version

session = pidgin.Session(        # all args optional / keyword
    model=None,                  #   live model id (default "claude-sonnet-4-5")
    provider=None,               #   live provider  (default "anthropic")
    system_prompt=None,          #   system prompt for the turn
    faux=False,                  #   True = offline faux provider, no network/key
)

session.send("hello")            # -> str: one full agent turn, concatenated text
                                 #    (blocking; releases the GIL while it runs)

for delta in session.send_stream("hello"):   # -> iterator of str deltas
    print(delta, end="")                       # yields assistant text as it streams
```

- `pidgin.version()` returns `pidgin_core::version()` — the same authoritative
  version the Rust core reports, not a string baked into the binding.
- `pidgin.Session` spawns **one dedicated worker OS thread** that owns the
  provider, the resolved model, and the running conversation. Python holds only
  channels into that thread; the session state is never shared across threads
  (the session-actor pattern, mirroring the interactive shell's turn worker).
- `send` / `send_stream` blocking waits release the GIL, so other Python threads
  keep running.
- Errors surface as `RuntimeError`.

## Build and test (offline, the default)

From this directory:

```bash
python -m venv .venv
. .venv/bin/activate
pip install maturin flask         # flask only needed for the demo
maturin develop                   # compiles the cdylib, installs `pidgin` into the venv
python test_faux.py               # offline smoke test: prints PASS lines, exits 0
```

`test_faux.py` needs **no network and no API key**: it constructs
`pidgin.Session(faux=True)`, which streams a deterministic canned reply that
echoes the prompt, and exercises both `send` and `send_stream`.

To compile-check the cdylib without Python (fast):

```bash
cargo build                       # -> target/debug/libpidgin.so
```

This crate carries an **empty `[workspace]` table** so it is its own single-crate
workspace, not a member of the repository-root workspace. That keeps PyO3 /
maturin (and, under the `live` feature, reqwest + rustls) out of the shared
`cargo build` and the `rust` CI job — exactly the reasoning `bindings/php` uses
for its ext-php-rs toolchain. It still depends on the engine via ordinary path
dependencies (`pidgin-core`, `pidgin-agent`, `pidgin-ai`).

## Flask chat demo

```bash
pip install -r demo/requirements.txt
cd demo
flask --app app run
```

Open <http://127.0.0.1:5000>. The demo defaults to the **offline faux** provider
(no key needed). See its section below for live mode.

## Live (real Anthropic turn) — for Zack

The `live` feature is **on by default**, so `maturin develop` already ships the
live path; it only needs a key at runtime.

```bash
export ANTHROPIC_API_KEY=sk-ant-...          # or ANTHROPIC_OAUTH_TOKEN
export PIDGIN_MODEL=claude-sonnet-4-5        # optional; this is the default

python - <<'PY'
import pidgin
s = pidgin.Session(faux=False, model="claude-sonnet-4-5")
print(s.send("Say hello in one short sentence."))
PY
```

The Flask demo automatically switches to live when `ANTHROPIC_API_KEY` is set in
its environment (model from `PIDGIN_MODEL`, default `claude-sonnet-4-5`).

The api key is read from the environment **at Session construction** via
`pidgin_ai::get_api_key_env_vars("anthropic")`
(`ANTHROPIC_OAUTH_TOKEN`, then `ANTHROPIC_API_KEY`) and threaded into the
request's `StreamOptions.api_key`.

> **Caveat (live only):** live Anthropic calls currently return HTTP 400 (a
> missing-headers issue) until **zmaril/pidgin PR #184** merges. The offline faux
> path is unaffected and is the supported/tested path today; the live path is
> wired and compiles, ready for when #184 lands.
