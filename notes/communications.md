# pi's Communication Map & the Rust Stack

*Research for the `pi` → Rust rewrite. Source traced at commit `3da591ab74ab9ab407e72ed882600b2c851fae21` (2026-07-17). This maps everything in pi that crosses a process or network boundary, and what the Rust equivalent should be.*

## TL;DR

pi is a TypeScript ESM monorepo. Its entire LLM surface lives in one package (`packages/ai`) that cleanly separates **provider config** (`src/providers/*`) from **wire-dialect drivers** (`src/api/*`), with every driver converging on one boundary-type file (`src/types.ts`) and one streaming event union (`AssistantMessageEvent`). It uses vendor SDKs for most providers but hand-rolls SSE where it needs control. It deliberately has **no MCP**. Its non-LLM boundaries are subprocess spawning (pipes, not PTYs), an optional orchestrator daemon speaking NDJSON over a unix socket, tree-structured JSONL session files, and opt-outable `pi.dev` pings.

**Recommended Rust stack, one line:** hand-roll thin `reqwest` + `eventsource-stream` provider clients (Anthropic / OpenAI-compat / Gemini / Mistral / Bedrock), adopt the official `rmcp` only if/when we want MCP, and model the boundary types as `serde` internally-tagged enums with catch-all variants. Own the wire; don't inherit someone else's provider model across the FFI boundary.

**Correction to an earlier sibling note:** pi's Google and Mistral clients are **not** stubbed — both are full ~500-660-line streaming + tool-calling drivers (`src/api/google-generative-ai.ts`, `src/api/mistral-conversations.ts`), fully wired in `src/providers/`. Treat all of Anthropic, OpenAI, Google, Mistral, and Bedrock as first-class.

---

## 1. The shape of pi's communication

```
                    pi core (packages/ai)
  Context{systemPrompt, messages, tools}  ──stream()──▶  one of 10 wire dialects
                                                              │
   AssistantMessageEventStream  ◀──(uniform events)───────────┘
   (start / *_start / *_delta / *_end / done | error)

  Non-LLM boundaries (packages/coding-agent, orchestrator):
    • subprocess spawn (bash/fd/rg/gh/npm/git) — pipes, process groups
    • RPC mode: NDJSON over stdin/stdout
    • orchestrator daemon: unix socket (NDJSON) ⇄ child `pi --mode rpc` (child stdio)
    • sessions: tree-structured JSONL at ~/.pi/agent/sessions/
    • telemetry: opt-outable version/catalog pings to pi.dev; opt-in Radius uplink
    • OAuth: loopback HTTP callback servers (127.0.0.1:1455/1456/53692)
```

pi separates two concepts a Rust core must also keep separate:
- **provider** = a vendor endpoint + credentials + model catalog (`src/providers/*.ts`, ~40 of them).
- **api / dialect** = a wire protocol (`src/api/*.ts`). Many providers reuse one dialect.

The dialects (`KnownApi`, `src/types.ts:16-26`): `openai-completions`, `mistral-conversations`, `openai-responses`, `azure-openai-responses`, `openai-codex-responses`, `anthropic-messages`, `bedrock-converse-stream`, `google-generative-ai`, `google-vertex`, `pi-messages`.

Every driver in `src/api/*` exports exactly two functions — `stream` and `streamSimple` — satisfying the `ProviderStreams` contract (`src/types.ts:228-231`). This is the seam the Rust core inherits: **a provider is a thing that turns a `Context` into a stream of uniform events.**

---

## 2. LLM provider surface

### 2.1 SDK vs raw HTTP, per dialect

pi is SDK-first but hand-rolls SSE where it needs raw control.

| Dialect | Client library (pi, TS) | Transport |
|---|---|---|
| `anthropic-messages` | `@anthropic-ai/sdk` | SDK for auth/request, **hand-written SSE parser** via `.asResponse()` |
| `openai-completions` | `openai` | SDK async-iterable stream |
| `openai-responses` | `openai` | SDK async-iterable stream |
| `azure-openai-responses` | `openai` (`AzureOpenAI`) | SDK stream |
| `openai-codex-responses` | `openai` types + raw `fetch` | custom transport, SSE **or WebSocket**, zstd |
| `google-generative-ai` | `@google/genai` | SDK `generateContentStream` |
| `google-vertex` | `@google/genai` | SDK (ADC / service-account auth) |
| `mistral-conversations` | `@mistralai/mistralai` | SDK `chat.stream` |
| `bedrock-converse-stream` | `@aws-sdk/client-bedrock-runtime` | SDK `ConverseStream`, SigV4 |
| `pi-messages` | raw `fetch` | pi's own event protocol over SSE |

Takeaway for Rust: the SDKs buy pi little that a thin client wouldn't — note that pi already bypasses the Anthropic SDK's stream parser and hand-rolls `fetch`/WebSocket for two dialects. In Rust there is no equivalent official Anthropic/Gemini/Mistral SDK worth adopting (see §5), so the natural design is one thin client per wire dialect.

### 2.2 Streaming mechanics

All drivers converge on `AssistantMessageEventStream` — a queue-backed async-iterable whose terminal event is `done` or `error` (`src/utils/event-stream.ts:69-83`). Each driver runs an async loop that pushes events and calls `stream.end()`.

- **Anthropic (hand-rolled SSE):** `client.messages.create({…, stream:true}).asResponse()` → `iterateSseMessages()` reads `response.body.getReader()` with a `TextDecoder`, splits on CR/LF, accumulates `event:`/`data:` fields (`src/api/anthropic-messages.ts:384-441`). Then it dispatches on the six `message_*` / `content_block_*` event names, mapping Anthropic deltas → pi events: `content_block_delta` with `text_delta` / `thinking_delta` / `input_json_delta` (tool args) / `signature_delta`. A stream that starts but never sees `message_stop` throws "Anthropic stream ended before message_stop" — which the retry classifier treats as retryable.
- **OpenAI-compat wire:** unnamed events; each `data:` line is a JSON chunk; terminated by the `data: [DONE]` sentinel.
- **Anthropic wire:** named events (`event: content_block_delta`, etc.); dispatch on the SSE event name.
- **Google / Mistral / OpenAI:** consumed as SDK async-iterables; pi walks `chunk.candidates[].content.parts` (Google), `event.data.choices[0].delta` (Mistral/OpenAI), emitting the same uniform events.
- **Streaming tool-args JSON:** partial tool-argument JSON is repaired incrementally via `parseStreamingJson` (`src/utils/json-parse.ts`, built on the `partial-json` dep), so `ToolCall.arguments` is a valid object mid-stream.

### 2.3 Tool-call wire format

Internal tool definition (`src/types.ts:445-448`): `Tool { name; description; parameters: TSchema }` — parameters are TypeBox schemas (already JSON-Schema-shaped). Each driver re-serializes to its provider's shape:
- **Anthropic:** `{ name, description, input_schema, cache_control?, defer_loading? }`; results → `{type:"tool_result", tool_use_id, content, is_error}`.
- **OpenAI:** `{ type:"function", function:{ name, description, parameters, strict? } }`.
- **Google:** `functionDeclarations`; calls arrive as `part.functionCall {id?, name, args}` (pi synthesizes an id when missing).
- **Mistral:** `{ type:"function", function:{…, strict:false} }`; results → `{role:"tool", toolCallId, name, content}`; 9-char tool-id normalization.

Coming back, every dialect normalizes provider tool-call deltas into the uniform `ToolCall` block and emits `toolcall_start` / `toolcall_delta` / `toolcall_end`.

### 2.4 Retry / backoff

Two layers:
1. **SDK retries disabled** in every driver (Anthropic `maxRetries: 0`, OpenAI same, Mistral `retries:{strategy:"none"}`).
2. **App-level retry loop** in the coding-agent (`packages/coding-agent/src/core/agent-session.ts`): exponential backoff `baseDelayMs * 2**(attempt-1)`, defaults `enabled:true, maxRetries:3, baseDelayMs:2000` → 2s/4s/8s. It strips the failed assistant message from state and re-issues.

What's retryable is decided by **regex classification over error message strings** (`src/utils/retry.ts:26-102`), not status-code dispatch: matches `overloaded`, `rate.?limit`, `429/500/502/503/504/524`, `service.?unavailable`, network/socket/timeout errors, `stream ended before message_stop`, `ResourceExhausted`, etc.; excludes `insufficient_quota` / `billing` / quota errors. A `maxRetryDelayMs` (default 60s) caps any server-requested delay.

### 2.5 Auth / env vars

Central map in `src/env-api-keys.ts`. Key vars: `ANTHROPIC_API_KEY` (or `ANTHROPIC_OAUTH_TOKEN`, which wins), `OPENAI_API_KEY`, `GEMINI_API_KEY`, `GOOGLE_CLOUD_API_KEY` (vertex), `MISTRAL_API_KEY`, `XAI_API_KEY`, `GROQ_API_KEY`, `OPENROUTER_API_KEY`, `HF_TOKEN`, etc. Ambient creds: Vertex ADC, Bedrock's 6 credential sources. OAuth flows (`src/auth/oauth/`) for Anthropic, GitHub Copilot, OpenAI Codex, xAI, Radius (PKCE + device-code). An Anthropic OAuth token (`sk-ant-oat`) switches the client to Bearer auth + Claude-Code identity headers.

---

## 3. The boundary types — pi's provider-agnostic core

These `src/types.ts` types are exactly what the Rust core should model as its public, FFI-facing surface. They're already discriminated unions → natural Rust `enum`s.

**Content blocks** (`types.ts:328-355`):
```ts
TextContent    { type:"text"; text; textSignature? }
ThinkingContent{ type:"thinking"; thinking; thinkingSignature?; redacted? }
ImageContent   { type:"image"; data /*base64*/; mimeType }
ToolCall       { type:"toolCall"; id; name; arguments: Record<string,any>; thoughtSignature? }
```

**Messages** (`types.ts:383-420`):
```ts
UserMessage      { role:"user"; content: string | (Text|Image)[]; timestamp }
AssistantMessage { role:"assistant"; content:(Text|Thinking|ToolCall)[];
                   api; provider; model; responseId?; usage; stopReason; errorMessage?; timestamp }
ToolResultMessage{ role:"toolResult"; toolCallId; toolName; content:(Text|Image)[];
                   details?; isError; timestamp }
StopReason = "stop" | "length" | "toolUse" | "error" | "aborted"
```

**The streaming event union** — the single most important type for the rewrite (`types.ts:465-476`):
```ts
AssistantMessageEvent =
  | { type:"start";          partial }
  | { type:"text_start"|"text_delta"|"text_end";         contentIndex; …; partial }
  | { type:"thinking_start"|"thinking_delta"|"thinking_end"; contentIndex; …; partial }
  | { type:"toolcall_start"|"toolcall_delta"|"toolcall_end"; contentIndex; …; partial }
  | { type:"done";  reason:"stop"|"length"|"toolUse"; message }
  | { type:"error"; reason:"aborted"|"error";         error }
```
Every non-terminal event carries `partial` (the accumulating `AssistantMessage`). **Contract** (`types.ts:302-308`): once a stream is created, failures are encoded as an `error` event *in the stream*, never thrown. A Rust core should hold that same contract: `Stream<Item = AssistantMessageEvent>` where errors are values, not `Err`, once streaming has begun.

Also worth porting: `Usage` / `ModelCost` (token + cost accounting), `Model` (`types.ts:706-731`, incl. a per-provider `compat` capability map of quirk flags), `ThinkingLevel`, `CacheRetention`.

---

## 4. How streaming should flow through the Rust core (and out to PHP)

The internal Rust-facing API is idiomatic: each provider client returns `Stream<Item = AssistantMessageEvent>` (via `reqwest` bytes-stream → `eventsource-stream` parse → `serde_json` into a `StreamEvent` enum → `async_stream::stream!` yielding the uniform event). Errors after stream start are `error` **events**, matching pi's contract.

But async `Stream`/`Future` **do not cross FFI** — Rust async is poll-based and runtime-driven, and PHP has no C ABI for a `Future`. So the core's *public* boundary must erase async into a synchronous, pull-shaped API:

- Keep the `tokio` runtime and the live `Stream` **inside Rust**. Hand the host an **opaque handle**.
- Expose `start(context) -> handle`, `next_event(handle) -> Event` (blocking; internally `block_on`s the next stream item; returns the next normalized event or an end/error sentinel), and `close(handle)`.
- PHP (via `ext-php-rs`) consumes it as a `while` loop / `Iterator` / `Generator` — the natural shape for a synchronous host. Serialize each event as a small struct or JSON string across the boundary; PHP only ever sees the normalized `AssistantMessageEvent`, never a provider's raw payload.
- A push/callback model is possible but collapses back toward pull because PHP isn't thread-safe (callbacks must fire on the PHP-owning thread).

Design implication: **a blocking `next()` over an opaque handle is a first-class access mode of the core**, not an afterthought — it's the shape every non-async host (PHP first, likely others) will use. This is another reason to own the event type rather than expose a dependency's types across FFI.

---

## 5. Recommended Rust stack, per surface

Research date 2026-07-18; versions/dates from crates.io.

| Surface | Recommendation |
|---|---|
| HTTP + SSE foundation | **Use crates:** `reqwest` (rustls, stream) + `eventsource-stream` + `tokio` + `futures` (+ `async-stream`). **Skip `reqwest-eventsource`** — its auto-reconnect is wrong for LLM streams. |
| Anthropic Messages | **Hand-roll.** No mature/official Anthropic Rust SDK; all options are pre-1.0 solo projects. |
| OpenAI-compatible | **Lean hand-roll** for one client reused across all OpenAI-dialect providers (Groq, Together, OpenRouter, Mistral, local). `async-openai` (v0.41, actively maintained, ~6.5M dl) is a defensible "use crate" if we only ever need real OpenAI. |
| Google Gemini | **Hand-roll.** `gemini-rust` (v2.0) exists but is a solo `0.x`/`2.0`; `generateContent`/`streamGenerateContent` is a small surface. |
| Mistral | **Hand-roll**, riding the OpenAI-compat client (Mistral's cloud API is OpenAI-compatible). `mistralai-client` is ~2 years stale. |
| Bedrock | Use the AWS SDK (`aws-sdk-bedrockruntime`) if we need Bedrock — SigV4 + `ConverseStream` is not worth hand-rolling. Lower priority than the direct providers. |
| Multi-provider abstraction | **Do not adopt as the core.** `genai` (jeremychone, natively covers OpenAI/Anthropic/Gemini/Mistral/Bedrock, streaming + typed tools) and `rig` (typed tool-calling, but a full agent framework) are worth **studying** — their per-provider adapter pattern is close to what we'll build — but adopting either couples our FFI surface to their evolving types. |
| MCP | **Use `rmcp`** (official Rust SDK, v2.2, conformance-tested, client + server, stdio/SSE/streamable-HTTP) — *only if/when* we add MCP. pi itself has none. |
| Boundary types / serde | **Use `serde` + `serde_json`** with internally-tagged enums (`#[serde(tag="type")]`) and a `#[serde(other)] Unknown` catch-all at every provider boundary, so a new provider block type doesn't hard-fail a live stream. Keep tool `arguments` as `serde_json::Value`. |

**Why hand-roll the providers:** LLM HTTP APIs are individually simple (one POST, one SSE stream) but change monthly and differ in fiddly ways (Anthropic's event-typed SSE vs OpenAI's `[DONE]` chunks, tool-call streaming deltas, cache-control, thinking blocks). A thin owned client is ~300-600 lines per provider and never blocks us on a maintainer's release cadence — and it keeps our own event type on the FFI boundary. The genuinely hard, *standardized* things (MCP transport negotiation, JSON) are where a dependency earns its keep.

**Prior art to study (not vendor):** `genai` (closest architectural match, all four providers), `codex-rs` (Apache-2.0; good reference for streaming/tool-call plumbing), `Dicklesworthstone/pi_agent_rust` (**non-standard license — study, don't vendor**), `c4pt0r/pie`. `rig` for its typed tool-calling approach.

---

## 6. Non-LLM boundaries

### 6.1 MCP — intentionally absent
pi is neither an MCP client nor server; there is zero MCP code. This is deliberate ("**No MCP.**", `packages/coding-agent/README.md:495`). External tools are ordinary CLI programs the model invokes via the bash tool; "tool servers" are user-installed extensions. A Rust rewrite gets MCP only if we choose to add it (via `rmcp`).

### 6.2 Subprocess spawning — pi's primary external surface
All spawns use `node:child_process` with **pipes, never PTYs**. The bash tool (`packages/coding-agent/src/core/tools/bash.ts`) spawns the shell `detached` (own process group so `kill(-pid)` reaps the tree), `stdio:[ignore|pipe, pipe, pipe]`, ANSI-stripped rolling-buffer output that spills to a tmp log past a threshold. `BashOperations` is an **interface** — extensions can swap in SSH/container backends. `fd`/`rg`/`gh`/`npm`/`git`/editors are spawned directly.

Rust equivalent: `tokio::process::Command` for the pipe-based tools; a process-group kill (`nix` `killpg` on Unix / job objects on Windows) for tree reaping; keep the "bash operations" trait so remote/containerized execution stays pluggable. PTYs are not needed to match pi.

### 6.3 IPC topology (orchestrator)
Base `pi` is single-process (interactive TUI, or `--mode rpc` reading NDJSON on stdin/stdout). Multi-process appears only with the experimental **orchestrator** daemon:
```
[CLI client] --unix socket, NDJSON--> [orchestrator daemon] --child stdio, NDJSON--> [N× pi --mode rpc]
```
Unix socket at `~/.pi/orchestrator/orchestrator.sock`; framing is newline-delimited JSON (`encodeMessage = JSON.stringify(m)+"\n"`); request types `spawn|list|stop|status|rpc|rpc_stream` (`rpc_stream` upgrades to a persistent bidirectional bridge). No TCP, no named pipes. Rust equivalent if we port this: `tokio::net::UnixListener` + a line-delimited JSON codec (`tokio-util` `LinesCodec`), `tokio::process` children. Lower priority — the orchestrator is experimental.

### 6.4 Session persistence — tree-structured JSONL v3
Append-only JSONL at `~/.pi/agent/sessions/<encoded-cwd>/<ts>_<id>.jsonl`. Line 1 is a `{type:"session", version:3, …}` header; every mutation appends an entry line or a `{type:"leaf", …}` pointer. The conversation is a **tree** (supports fork/edit/regenerate); the active conversation is the path from a leaf to root. Auth stored separately as `~/.pi/agent/auth.json` (mode `0o600`). Rust equivalent: serde structs + append-only file writes; the tree/leaf model is the non-obvious part to preserve. Lower priority for the comms core, but the on-disk format is a compatibility surface if the Rust mirror must read pi's sessions.

### 6.5 Telemetry / network pings — all opt-outable
All to `pi.dev`, version + UA only, no conversation content:
- Install ping: `GET pi.dev/api/report-install?version=…` (disable: `PI_OFFLINE=1`, `PI_TELEMETRY=0`, or `enableInstallTelemetry:false`).
- Version check: `GET pi.dev/api/latest-version` (disable: `PI_SKIP_VERSION_CHECK` / `PI_OFFLINE`).
- Remote model catalog: `GET pi.dev/api/models/providers/<id>`, 4h refresh, gated on `allowNetwork`.
- **Radius** (orchestrator, **opt-in**, only if a `radius` credential or `RADIUS_API_KEY` exists): Bearer-auth POSTs to `radius.pi.dev/v1/` — `machines/register`, `pis/register`, heartbeats — a cloud remote-control uplink. The most significant network boundary beyond LLM calls; only relevant if the rewrite reproduces the orchestrator.

No third-party analytics SDK (no Sentry/PostHog) is wired into the shipped agent.

### 6.6 Other crossings
Loopback OAuth callback servers (the only HTTP servers pi starts: `127.0.0.1:1455/1456/53692`, bound loopback only); `fs.watch` on `.git/HEAD` and the theme file; clipboard via native addon → platform tools → OSC 52 escape; `gh gist create` for session sharing (data goes to a private GitHub gist, not pi servers); optional local llama.cpp client (HTTP + SSE) and HuggingFace model search; managed `fd`/`rg` binary downloads from GitHub releases. Most are agent-tooling concerns rather than core-communication concerns.

---

## 7. Open questions

1. **Scope of the first Rust milestone.** The comms core = provider clients + the `AssistantMessageEvent` stream + FFI. Do we port the orchestrator/RPC topology and JSONL session format now, or defer until the single-process core + PHP bindings work end-to-end? (Recommend: defer; ship the streaming core first.)
2. **Provider priority.** Anthropic + OpenAI-compat cover most of pi's ~40 providers via two clients. Confirm Google (native Gemini dialect) and Bedrock (AWS SDK) are in the first cut, or fast-follows.
3. **`async-openai` vs hand-rolled OpenAI client.** Hand-rolling gives one client across all OpenAI-compat providers with our own event type; `async-openai` is more mature but models only OpenAI's dialect. Leaning hand-roll — worth a spike to confirm the compat-provider quirks (tool-call deltas, `reasoning` fields) are manageable.
4. **Session-format compatibility.** Must the Rust mirror read/write pi's exact JSONL v3 tree files, or is a fresh format acceptable for the rewrite? Affects whether we port the leaf/tree model verbatim.
5. **FFI event serialization.** Struct-across-boundary vs JSON-string-per-event for `next_event()`. JSON is simplest to start and language-agnostic for hosts beyond PHP; revisit if per-token overhead matters.
6. **MCP.** pi has none by design. Is MCP client support a goal for the Rust core (then adopt `rmcp`), or do we mirror pi's "tools are CLI programs" stance?
