//! The closure-invocation primitive: calling a registered JS hook handler from
//! Rust over the `Affinity::OwnRuntime` rendezvous.
//!
//! PR-E kept every registered handler closure inside the `JsRuntime`, keyed by
//! event name in `globalThis.__atilla.registry.hooks` (a JS-side `Map<event,
//! handler[]>`). This module is the Rust half of the dispatch seam: given an
//! event name, a handler index, a JSON event, and a JSON `ctx`, it calls the
//! `globalThis.__atilla.invokeHook` bootstrap function (see [`crate::api_ops`]),
//! awaits its (possibly async) result through the runtime's event loop, and
//! deserializes the plain-data envelope back to Rust.
//!
//! # Only JSON crosses
//!
//! Following the spike's proven `js_call_json` pattern (`throwaway/deno-hello`)
//! and PR-E's `eval` path, the call is built as a `execute_script` expression
//! whose arguments are `serde_json` string literals — `serde_json` emits valid JS
//! string literals, so JSON escaping is handled for us — and the JS side
//! `JSON.parse`s them, runs the handler, and returns a `JSON.stringify`'d
//! envelope string. No V8 handle ever crosses the boundary, exactly as
//! `notes/startup/deep-hooks.md` §5 mandates. A handler that throws is isolated
//! JS-side into an [`ok = false`](HookInvocation::ok) envelope, so a bad handler
//! surfaces as an error record here without unwinding the runtime thread.

// straitjacket-allow-file:duplication -- the execute_script / resolve /
// event-loop-promise / from_v8 marshalling here is deliberate parallel structure
// of PR-E's `eval_source` and the spike's `js_call_json`; it is the shared
// OwnRuntime rendezvous shape, not an accident to hoist away.

use anyhow::{anyhow, Result};
use deno_core::{v8, JsRuntime, PollEventLoopOptions};
use serde::Deserialize;
use serde_json::Value;

/// The plain-data envelope `globalThis.__atilla.invokeHook` returns (as a
/// `JSON.stringify`'d string) for one handler invocation.
///
/// On success [`ok`](Self::ok) is `true`, [`result`](Self::result) is the
/// handler's (JSON) return value (`null` when it returned `undefined`), and
/// [`event`](Self::event) is the possibly-mutated-in-place event object. On a
/// thrown handler `ok` is `false` and [`error`](Self::error) /
/// [`stack`](Self::stack) carry the failure.
#[derive(Debug, Clone, Deserialize)]
pub struct HookInvocation {
    /// Whether the handler ran to completion (`false` when it threw).
    pub ok: bool,
    /// The handler's JSON return value; `null` when it returned `undefined`.
    #[serde(default)]
    pub result: Value,
    /// The event object after the handler ran — carries any in-place mutations.
    #[serde(default)]
    pub event: Value,
    /// The thrown error's message, when `ok` is `false`.
    #[serde(default)]
    pub error: Option<String>,
    /// The thrown error's stack, when the handler threw an `Error`.
    #[serde(default)]
    pub stack: Option<String>,
}

/// Invoke handler `index` for `event` on the runtime thread, passing the JSON
/// `event_json` and `ctx_json`, and await the resulting [`HookInvocation`].
///
/// Must run on the runtime's owning thread (it takes `&mut JsRuntime`). The
/// arguments are marshalled as JSON strings into a `invokeHook(...)` call
/// expression, the event loop is driven until the handler's (awaited) result
/// settles, and the returned envelope string is parsed back — mirroring PR-E's
/// `eval_source` and the spike's `js_call_json`.
pub async fn invoke_hook_on_runtime(
    runtime: &mut JsRuntime,
    event: &str,
    index: usize,
    event_json: &Value,
    ctx_json: &Value,
) -> Result<HookInvocation> {
    // Marshal the four call arguments as JS string literals. `event` and the
    // index arrive as strings JS-side (a string array index coerces); the event
    // and ctx are JSON strings the handler wrapper `JSON.parse`s.
    let index = index.to_string();
    let event_str =
        serde_json::to_string(event_json).map_err(|e| anyhow!("serialize event: {e}"))?;
    let ctx_str = serde_json::to_string(ctx_json).map_err(|e| anyhow!("serialize ctx: {e}"))?;
    let args = [event, index.as_str(), event_str.as_str(), ctx_str.as_str()];
    let arg_list = args
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| anyhow!("encode call args: {e}"))?
        .join(", ");
    let code = format!("globalThis.__atilla.invokeHook({arg_list})");

    let promise = runtime
        .execute_script("<invoke-hook>", code)
        .map_err(|e| anyhow!(e.to_string()))?;
    let resolve = runtime.resolve(promise);
    let resolved = runtime
        .with_event_loop_promise(resolve, PollEventLoopOptions::default())
        .await
        .map_err(|e| anyhow!(e.to_string()))?;

    // The envelope comes back as a JSON string; deserialize it into Rust.
    let envelope = {
        deno_core::scope!(scope, runtime);
        let local = v8::Local::new(scope, resolved);
        deno_core::serde_v8::from_v8::<String>(scope, local)
            .map_err(|e| anyhow!("read hook invocation envelope: {e}"))?
    };
    serde_json::from_str::<HookInvocation>(&envelope)
        .map_err(|e| anyhow!("parse hook invocation envelope: {e}"))
}

/// The plain-data envelope `globalThis.__atilla.invokeStored` returns (as a
/// `JSON.stringify`'d string) for one stored-closure invocation.
///
/// The shared, one-shot, forward-only result shape for invoking any closure a
/// registration kept live in the runtime by key — a tool's `execute`, a
/// command's `handler`, a provider's `oauth.getApiKey` / `oauth.refreshToken`.
/// On success [`ok`](Self::ok) is `true` and [`result`](Self::result) is the
/// closure's (JSON) return value (`null` when it returned `undefined`). On a
/// thrown closure — or a missing key — `ok` is `false` and
/// [`error`](Self::error) / [`stack`](Self::stack) carry the failure.
#[derive(Debug, Clone, Deserialize)]
pub struct StoredInvocation {
    /// Whether the closure ran to completion (`false` when it threw or was
    /// missing).
    pub ok: bool,
    /// The closure's JSON return value; `null` when it returned `undefined`.
    #[serde(default)]
    pub result: Value,
    /// The failure message, when `ok` is `false`.
    #[serde(default)]
    pub error: Option<String>,
    /// The thrown error's stack, when the closure threw an `Error`.
    #[serde(default)]
    pub stack: Option<String>,
}

/// Invoke the stored JS closure identified by (`kind`, `name`) on the runtime
/// thread, passing the positional JSON `args`, and await the resulting
/// [`StoredInvocation`].
///
/// This is the SHARED one-shot invoke-stored-JS-function primitive: `kind`
/// selects the registry map (`"tool"` → `reg.tools[name].execute`, `"command"`
/// → `reg.commands[name].handler`, `"providerGetApiKey"` /
/// `"providerRefreshToken"` → `reg.providers[name].oauth.*`), `args` is a JSON
/// array spread as the closure's positional arguments, and the (possibly
/// awaited) result is returned as a plain-data envelope. Like
/// [`invoke_hook_on_runtime`], it is one-shot and forward-only — JSON in, JSON
/// out, no re-entrancy — reusing the same `execute_script` / event-loop-promise
/// rendezvous shape. Must run on the runtime's owning thread.
pub async fn invoke_stored_on_runtime(
    runtime: &mut JsRuntime,
    kind: &str,
    name: &str,
    args: &Value,
) -> Result<StoredInvocation> {
    let args_str = serde_json::to_string(args).map_err(|e| anyhow!("serialize args: {e}"))?;
    let call_args = [kind, name, args_str.as_str()];
    let arg_list = call_args
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| anyhow!("encode call args: {e}"))?
        .join(", ");
    let code = format!("globalThis.__atilla.invokeStored({arg_list})");

    let promise = runtime
        .execute_script("<invoke-stored>", code)
        .map_err(|e| anyhow!(e.to_string()))?;
    let resolve = runtime.resolve(promise);
    let resolved = runtime
        .with_event_loop_promise(resolve, PollEventLoopOptions::default())
        .await
        .map_err(|e| anyhow!(e.to_string()))?;

    let envelope = {
        deno_core::scope!(scope, runtime);
        let local = v8::Local::new(scope, resolved);
        deno_core::serde_v8::from_v8::<String>(scope, local)
            .map_err(|e| anyhow!("read stored invocation envelope: {e}"))?
    };
    serde_json::from_str::<StoredInvocation>(&envelope)
        .map_err(|e| anyhow!("parse stored invocation envelope: {e}"))
}
