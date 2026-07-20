# pidgin — seam bridges

How a pi test's JavaScript-side mock reaches a Rust seam. This is the contract the ported-consumer threads (auth, exec tools) and the shim maintainer build against. It extends the one-way JSON boundary that PR #35 established for the faux provider (`crates/pidgin-ai/src/providers/faux.rs` → `crates/pidgin-napi/src/faux.rs` `FauxCore` → `conformance/shims/packages/ai/src/providers/faux.ts`) to the other four seams.

The seam traits already exist in `crates/pidgin-ai/src/seams/` with a production impl and a scripted test double each. What this doc adds is the rule that decides, for any given seam, where the JS/Rust line falls — so that pi's existing `vi.*` mock lands on the effect it already targets, with no change to pi's test.

## The boundary rule

PR #35 settled the shape of the boundary, and it dictates every bridge below:

- **JS owns orchestration and mutable per-call state** — the async loop, the response queue, factory resolution, the `AbortSignal`, callbacks, and the passage of time.
- **Rust owns deterministic transforms** — building requests, parsing responses, computing argv, computing backoff delays, cloning and stamping messages.
- **Values cross as JSON strings, one way per call.** A napi method takes JSON in and returns JSON out. Rust never calls back into JS mid-call; there is no `ThreadsafeFunction`.

The consequence is the single idea that makes the seams work across the FFI: **a bridge is never "Rust invokes your JS mock." It is a split of the ported module into a pure Rust half and a JS shim half, where the JS half performs the side effect that pi's mock already replaces.** When pi stubs global `fetch`, the shim is what calls `fetch`, so the stub intercepts it untouched. Rust sees only the request it asked the shim to send and the response the shim hands back.

Because Rust can only return, never await, any step that needs the result of a JS effect ends the Rust call. Multi-step flows (an OAuth poll, a token refresh, an SSE retry) become a state machine: Rust returns the next action, the shim performs it, the shim re-enters Rust with the result. This is the faux queue pattern generalized — faux keeps its response queue in JS and re-enters Rust once per step; these bridges do the same for network, subprocess, filesystem, and time.

## Consumer status

The contracts here are written ahead of most of their consumers. Today:

| Seam | Trait | Consumer in Rust today | Bridged across FFI |
|---|---|---|---|
| Provider | `Provider` | `FauxProvider` | yes (PR #35; flip rides PR #44) |
| Clock | `Clock` / `Timers` | `FauxProvider` (field) | not yet |
| HTTP transport | `HttpTransport` | none | not yet |
| Command runner | `CommandRunner` | none | not yet |
| Storage / env | `ExecutionEnv` | none | not yet |

Only the faux provider consumes any seam, and it reads the clock only on its empty-queue and aborted paths, neither of which a pi test asserts. So no pi test exercises these seams against Rust today; each goes green when its consumer module is ported and flipped native. A bridge is proven in two earlier steps first: a Rust test over the pure half (this thread), and an FFI test over the shim once the napi core and a consumer exist (with the shim maintainer's harness). The real pi file is the third step, owned by the consumer thread.

---

## Bridge 1 — Clock

**pi mock:** `vi.useFakeTimers`, `vi.setSystemTime`, `vi.advanceTimersByTime` (29 files); assertions on message timestamps, token expiry, SSE retry-after delays, elapsed-gated reconnects, and the timestamp embedded in a uuidv7.

**Rule:** JS owns the passage of time. Under vitest, `Date.now()` and `setTimeout` are already faked; Rust in bridged code neither sleeps nor reads a wall clock. Two sub-cases cover every site:

1. **Now as a value.** Any Rust computation that needs the current time takes it as a parameter. The shim reads `Date.now()` (faked by vitest) and passes it in. This covers token-expiry comparison, the uuidv7 timestamp, and a retry-after HTTP-date turned into a delay.
2. **Scheduling.** Rust computes the delay — a pure function of the retry-after header and the current time — and returns it. The shim calls `setTimeout(delay)` (faked by vitest) and re-enters Rust for the retry. Rust owns no timer. This is what makes `expect(setTimeoutSpy).toHaveBeenCalledWith(fn, expectedDelay)` in `openai-codex-stream.test.ts` pass: the asserted delay is the one Rust computed.

**Injectable Rust type:** `seams::clock::FakeClock` (settable `now`, cloneable shared state) for pure-Rust tests. Across the FFI, prefer passing `now` as a method parameter over holding a clock — it keeps each call one-way and stateless. A consumer that must hold `Arc<dyn Clock>` internally can take a `ClockCore` handle whose `now` the shim sets before the call.

**napi core (spec for the shim maintainer):** `ClockCore` — `new(nowMs: i64)`, `setNowMs(nowMs: i64)`, `nowMs() -> i64`, wrapping a shared `FakeClock`. Most consumers will not need it; the parameter form is enough.

**Reference proof (rides PR #44's faux flip):** the faux provider stamps its empty-queue and aborted messages from the clock. Add a `nowMs` parameter to `FauxCore.streamResolved` and `FauxCore.emptyQueueResult`; the shim passes `Date.now()`. An FFI test then sets a fake time, drives an empty-queue stream, and asserts the returned message `timestamp` equals it — proving a JS time value reaches Rust and back. This does not change any pi test; pi's faux tests stay green because they do not assert that field.

**Plug your consumer in (auth token refresh):**

```text
// Rust (pure): decide, given now, whether the token is still valid.
core.tokenState(credsJson, nowMs) -> { "valid": bool, "refreshInMs": i64 }
```

```text
// shim
const nowMs = Date.now();                       // vitest-faked
const state = JSON.parse(core.tokenState(creds, nowMs));
if (!state.valid) { /* run the refresh exchange (Bridge 2) */ }
```

---

## Bridge 2 — HTTP transport

**pi mock:** `vi.stubGlobal("fetch", fn)` (80 sites across 12 files — OAuth, token refresh, provider request shaping), plus a WebSocket for `openai-codex-stream`.

**Rule:** `fetch` stays in JS. The shim calls the global `fetch`, so the stub intercepts it with no change. Rust is a request-builder and a response-parser. The ported module splits into:

- `buildRequest(args) -> { url, method, headers, body }`
- `consumeResponse(status, headers, bodyText) -> result | nextAction`

For a single request that is one of each. For a multi-step flow (OAuth authorization poll, refresh-then-retry), `consumeResponse` returns a next-action tag — `done`, `request` with the next request descriptor, or `error` — and the shim loops. The loop, the `await fetch`, and any `setTimeout` between attempts live in JS; the decision of what to send and what a response means lives in Rust.

**Streaming (SSE):** already solved for anthropic in this shape — the shim reads the response body stream and Rust parses each chunk (`anthropicParseSseStream`, `crates/pidgin-napi/src/lib.rs`). A retry on a streamed 429 combines with Bridge 1: Rust computes the delay, the shim schedules it.

**WebSocket:** JS owns the socket object; Rust builds the connect parameters and outgoing frames and parses incoming frames. The `connect_websocket` method on `HttpTransport` is for a future pure-Rust transport; the bridged path keeps the socket in JS, matching how `openai-codex-stream.test.ts` drives a mock WebSocket.

**Injectable Rust type:** `seams::http::ScriptedTransport` (queued responses, recorded requests) for pure-Rust tests; `HostTransport` (delegates to injected fetch closures) is the production analog for a pure-Rust caller. Across the FFI, do not hold `Arc<dyn HttpTransport>` inside a bridged loop — restructure to build/consume, because a held transport would have to await JS.

**napi core (spec for the shim maintainer):** no generic `HttpCore`. Request and response shapes are domain-specific, and a generic byte-level transport would force Rust to await JS, which the boundary forbids. Put the `buildX`/`consumeX` method pairs on the consumer's own core (for example `AnthropicOAuthCore`). The shim maintainer and the consumer thread own that core; this thread owns the `HttpRequest`/`HttpResponse` seam types they cross.

**Plug your consumer in (anthropic OAuth login):**

```text
// Rust (pure)
core.buildTokenRequest(codeJson) -> { url, method, headers, body }
core.consumeTokenResponse(status, headersJson, bodyText) -> credentialsJson
```

```text
// shim — vi.stubGlobal("fetch") lands on this fetch call
const req = JSON.parse(core.buildTokenRequest(code));
const res = await fetch(req.url, { method: req.method, headers: req.headers, body: req.body });
const creds = JSON.parse(core.consumeTokenResponse(res.status, headersToJson(res.headers), await res.text()));
```

`anthropic-oauth.test.ts` passes unchanged: its `fetchMock` asserts the URL, method, and body that `buildTokenRequest` produced, and returns the token body that `consumeTokenResponse` reads.

### Multi-step flows: the OAuth state machine

Device-code polling, refresh-then-retry, and chained logins (a Copilot login is device-poll, then token exchange, then list models, then enable each) need more than one build/consume pair, and a later request depends on an earlier response. Model the whole flow as one resumable machine whose phase lives in the Rust core object, the way `FauxCore` holds its call count. The core reuses the seam `HttpRequest`/`HttpResponse` types verbatim — they serialize to the shim's `fetch` shape (camelCase, text body, headers as a JSON object) as of PR #63.

```rust
/// One action yielded by an OAuth flow, serialized across the one-way boundary.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Step {
    Request { request: HttpRequest },
    Wait    { delay_ms: u64, request: HttpRequest },
    Prompt  { prompt: AuthPrompt },
    Notify  { event: AuthEvent },
    Done    { credential: OAuthCredential },
    Error   { message: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepInput {
    Response(HttpResponse),   // {"kind":"response","status":..,"headers":..,"body":..}
    Input { value: String },  // {"kind":"input","value":".."}
    Ack,                      // {"kind":"ack"}
    Aborted,                  // {"kind":"aborted"} -> advance() returns Error{"Login cancelled"}
}

pub trait OAuthFlowMachine {
    fn start(&mut self, now_ms: i64) -> Step;
    fn advance(&mut self, input: StepInput, now_ms: i64) -> Step;
}
```

The shim drives the loop until `Done` or `Error`:

- `Request` — `fetch`, then `advance(Response)`.
- `Wait` — `setTimeout(delayMs)` (vitest-faked), then `fetch(request)`, then `advance(Response)`. The delay is a pure function of the retry-after header and `now` (RFC 8628 backoff for device-code), so it lives in the step and the shim never computes timing.
- `Prompt` — call the caller's `prompt()`, then `advance(Input)`. `Notify` — call `notify()`, then `advance(Ack)`. These carry pi's `AuthInteraction` callbacks, which the test supplies on the JS side.
- `Done` — return the credential. `Error` — throw.

`now_ms` is passed to both `start` and `advance` (expiry math and the device-code deadline). Cancellation feeds an `Aborted` input, and `advance(Aborted, _)` returns `Error{ "Login cancelled" }`; the shim feeds it when the caller's abort signal fires during a `Wait` or the caller's `prompt()` rejects. Two adjacent terminal errors are ordinary `Error` steps the machine computes from `now` or the response, not inputs: a device-poll deadline timeout ("Device flow timed out") and a provider denial ("device authorization was denied"). One case is purely shim-side: after `Done`, the shim aborts the interactive prompt's signal so a UI dismisses it. The loopback-server and browser login paths are not `fetch` and are not stubbed by pi's `*-oauth.test.ts`, so they stay a native path outside this bridge.

One edge the machine does not model: pi's RFC 8628 poller sleeps `min(interval, remaining)` and then rechecks the deadline, so at the exact boundary it can stop without a final poll. `Wait` couples a delay with a poll, so a device-code flow instead returns `Error{ "Device flow timed out" }` on the `advance` where `now` crosses the deadline, with no trailing poll — matching pi's no-final-poll behavior; only the final sleep's exact instant differs, which pi's `*-oauth.test.ts` fixtures never observe. If a test or client ever pins that instant, add a bare `Wait { delay_ms }` step (no request) — a purely additive change.

---

## Bridge 3 — Command runner

**pi mock:** `vi.spyOn(packageManager, "runCommand" | "runCommandCapture")` asserting exact argv (43 sites in `package-manager.test.ts`), plus one `child_process` spy for a git `symbolic-ref`.

**Rule:** argv is deterministic, so Rust builds it; execution stays in JS, where the spy sits. The ported module splits into:

- `plan(spec) -> { program, args, cwd }`
- `consumeOutput(code, stdout, stderr) -> result`

The shim's `runCommand(program, args, cwd)` performs the spawn — or, in a test, the spy replaces it. `expect(runCommandSpy).toHaveBeenCalledWith("mise", ["exec", "node@20", "--", "npm", "install", "@scope/pkg"], undefined)` passes because the argv is exactly what `plan` returned.

**Injectable Rust type:** `seams::subprocess::ScriptedCommandRunner` (queued replies, recorded argv) for pure-Rust tests. Across the FFI, use the plan/consume split.

**napi core (spec for the shim maintainer):** `CommandCore` (or methods on the package-manager core) with `planX` methods returning argv JSON and `consumeX` methods parsing captured output.

**Plug your consumer in (package-manager install):**

```text
// Rust (pure)
core.planInstall(specJson) -> { program, args, cwd }
```

```text
// shim
const plan = JSON.parse(core.planInstall(spec));
await runCommand(plan.program, plan.args, plan.cwd);   // vi.spyOn(pkgMgr, "runCommand") lands here
```

### Re-entrant command flows

`npm install` is a single `plan`/`consumeOutput`. Several package-manager flows are not: a git upstream probe reads `rev-parse --abbrev-ref @{upstream}`, parses the branch, then runs `ls-remote origin refs/heads/<branch>` — the second argv comes from the first command's stdout. A version check runs `npm view … version` and installs only when the versions differ (the test asserts `runCommand` is not called when they match). These use the same resumable machine as Bridge 2:

```rust
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandStep {
    Run  { request: CommandRequest },
    Done { result: CommandResult },   // CommandResult is the flow's own result type
}

pub trait CommandFlowMachine {
    fn start(&mut self) -> CommandStep;
    fn advance(&mut self, output: CommandOutput) -> CommandStep;
}
```

The shim runs each `Run.request`, feeds the `CommandOutput` back through `advance`, and loops until `Done`. A flow that decides no command is needed returns `Done` from `start` with no `Run` — which is how the version-check test sees `runCommand` never called. `CommandRequest` carries `env` and `timeout_ms` (`GIT_TERMINAL_PROMPT`, npm-view timeouts) as of PR #63.

`Done.result` crosses as JSON — each machine serializes its own result — so a single `CommandCore` wraps every op as `Box<dyn CommandFlowMachine>`. Keep the trait object-safe: no per-op associated output type, and every result type derives `Serialize`.

### Injected-collaborator variant

Some tool tests do not spy the spawn; they inject a fake operations object (`createBashTool({ operations })`) and assert against it. That is a collaborator mock. Under the one-way boundary the mapping is the same build/consume split — Rust decides the operation, the shim's operations object performs it — not a Rust tool holding a JS-backed trait object, which would need a callback into JS mid-call. A Rust tool that keeps its operations as a pluggable trait backs either shape: the scripted double for pure-Rust tests, the shim-driven split for conformance.

### Shim reconstruction rules

`CommandRequest` is a flat struct, and pi's three runners take different option shapes, so the shim reconstructs pi's exact call: `runCommand(program, args, { cwd? })`, `runCommandCapture(program, args, { cwd?, timeoutMs?, env? })` where `env` is a `Record`, and `runCommandSync(program, args)` with no options. Three rules keep the calls byte-identical to pi:

- Emit the options argument as `undefined` when `cwd`, `env`, and `timeout_ms` are all empty — never `{}`. A test that deep-equals `{}` against `undefined` treats them as unequal, so `toHaveBeenCalledWith(program, args, undefined)` (npm install, uninstall, `npm root -g`) fails against a `{}`.
- Build the options object from present keys only, so exact `{ cwd }` assertions hold.
- Convert `env` from the wire's array of pairs (`[["GIT_TERMINAL_PROMPT", "0"]]`) to a `Record` with `Object.fromEntries`.

### Host preconditions

Two behaviors stay with the host, not the machine:

- Bulk npm updates issue one batched install per scope. Probe each source with the machine or the standalone version parser, collect the specs, then run a single batch install — not a per-source install machine.
- The offline gate short-circuits update checks before the first command, so apply it before `start()`; the machine always plans its first `Run`.

---

## Bridge 4 — Storage and env

**pi mock:** filesystem stubs and `process.env` reads (`auth-storage.test.ts`, `first-time-setup-fork.test.ts`).

**Rule:** two options, chosen per test.

1. **Real files.** When the pi test writes to a real temp directory, Rust reads it directly through `SystemEnv`. No bridge state is needed.
2. **Seeded memory.** When the pi test stubs `fs` or `process.env`, the shim seeds an in-memory env across the FFI: it passes `{ "env": { ... }, "files": { "<path>": "<contents>" } }` into the core constructor, and Rust backs `ExecutionEnv` with a `MemoryEnv` built from it. Reads and writes hit the in-memory map; the shim reads back mutations to assert on them. Prefer this when the test stubs `fs`.

**Injectable Rust type:** `seams::storage::MemoryEnv` (`with_env`, `with_file` seeders, cloneable shared state) — already the right shape.

**napi core (spec for the shim maintainer):** `StorageCore` — `new(seedJson)`, `readFile(path) -> string`, `writeFile(path, contents)`, `exists(path) -> bool`, `envVar(key) -> string | null`, and `dumpJson() -> string` to read written state back into JS for assertions.

**Plug your consumer in (auth storage):**

```text
// shim seeds the env, runs the ported read, asserts the write
const core = new StorageCore(JSON.stringify({ env: { HOME: "/home/u" }, files: {} }));
core.saveCredentials(JSON.stringify(creds));                 // ported Rust write
const written = JSON.parse(core.dumpJson()).files["/home/u/.pi/auth.json"];
expect(JSON.parse(written).access).toBe("access-token");
```

---

## Proving a bridge

Each bridge is proven in three steps, and the first two are gate-free:

1. **Rust half** — a unit or integration test in `crates/pidgin-ai` over the injectable double (`FakeClock`, `ScriptedTransport`, `ScriptedCommandRunner`, `MemoryEnv`). Owned by this thread.
2. **FFI half** — once the napi core exists, a focused JS test through the shim that injects a value and asserts it reached Rust and back. Run with the shim maintainer's single-file harness. Owned jointly with the shim maintainer.
3. **Real pi file** — flips native when the consumer module is ported; the pi test then runs unchanged against Rust. Owned by the consumer thread (auth, exec tools).

### Single-file harness loop

From the shim maintainer, verified end to end. Rebuild the addon every time — do not trust an existing `.node`:

```bash
cd /workspace/pidgin; REPO_ROOT="$(pwd)"; PI_ROOT="$REPO_ROOT/vendor/pi"
( cd crates/pidgin-napi && npx napi build --platform --release )
ln -sfn "$REPO_ROOT/crates/pidgin-napi" "$PI_ROOT/node_modules/pidgin-napi"
node "$REPO_ROOT/conformance/codegen.mjs"                       # must print "missing": 0
( cd "$PI_ROOT/packages/ai" && npx vitest run test/<file>.test.ts --reporter=dot )
find "$PI_ROOT/packages" -name '*.__pi_original__.ts' -delete && git -C "$PI_ROOT" checkout -- packages
```

Gotchas: codegen aborts on manifest drift (a new module file needs a manifest row); vitest is cwd-sensitive (run from the package dir); flipping an already-listed row's status needs no topology change.

## Ownership

- **Seam traits and doubles** — `crates/pidgin-ai/src/seams/` — this thread.
- **napi cores, shim glue, manifest flips** — the shim maintainer.
- **Ported consumers (the build/consume splits)** — the auth thread and the exec-tools thread. Code to the per-bridge contract above; the seam types you cross are stable.
