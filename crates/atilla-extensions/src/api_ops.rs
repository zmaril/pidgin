//! The `ExtensionAPI` op surface: the JS -> Rust registration boundary.
//!
//! This module binds pi's `pi` object (`ExtensionAPI`, `types.ts:1167`) to the
//! embedded runtime. The [`BOOTSTRAP_JS`] script installs `globalThis.__pi`, a
//! plain JS object whose registration methods (`registerTool`, `on`,
//! `registerCommand`, `registerShortcut`, `registerFlag`/`getFlag`,
//! `registerMessageRenderer`/`registerEntryRenderer`) each:
//!
//!   1. keep the JS closure (`tool.execute`, the hook handler, the renderer) in
//!      a JS-side `Map` keyed by name — VM handles never cross into Rust; and
//!   2. call a `deno_core` op with only the serializable *metadata*, which lands
//!      in the Rust-side [`Inventory`] (see the `inventory` module).
//!
//! This mirrors pi's `createExtensionAPI` (`loader.ts:230`) exactly: pi's
//! methods write metadata into the `extension.*` collections while the closures
//! stay live in the JS module. `JSON.stringify` drops the non-serializable
//! closure fields for us before the op is called.
//!
//! # Implemented-only exposure
//!
//! Per `notes/design.md`, only the registration subset is implemented. The
//! remaining `ExtensionAPI` methods that need a live host (`sendMessage`,
//! `sendUserMessage`, `exec`, `setModel`, `getActiveTools`, provider
//! registration, `events`, …) are present on `__pi` as documented no-op stubs
//! returning benign empty values, so a factory that calls one at load time does
//! not crash. They record nothing and belong to PR-F (hook dispatch + session
//! wiring). See [`BOOTSTRAP_JS`].

// straitjacket-allow-file:duplication -- the op bodies are deliberate parallel
// structure (borrow the shared inventory, parse the metadata payload, push one
// record); the same register-op shape recurs once per ExtensionAPI method, so it
// is mirror duplication of pi's createExtensionAPI method block, not an accident
// to hoist away.

use std::cell::RefCell;
use std::rc::Rc;

use deno_core::{extension, op2, OpState};
use serde::Deserialize;
use serde_json::Value;

use crate::inventory::{
    CommandRecord, FlagRecord, HookRecord, Inventory, RendererRecord, ShortcutRecord, ToolRecord,
};

/// The inventory shared between the ops and the loader, living in the runtime's
/// `OpState`. `Rc<RefCell<_>>` because everything on the JS thread is `!Send`
/// and single-threaded; the owned [`Inventory`] is cloned out (becoming `Send`)
/// before it crosses back over the rendezvous.
pub type SharedInventory = Rc<RefCell<Inventory>>;

/// Metadata payload for `pi.registerTool` (the serializable subset of pi's
/// `ToolDefinition`; `execute`/`prepareArguments` are dropped by `JSON.stringify`).
#[derive(Deserialize)]
struct ToolInput {
    name: String,
    label: Option<String>,
    description: Option<String>,
    parameters: Option<Value>,
    prompt_snippet: Option<String>,
    prompt_guidelines: Option<Vec<String>>,
    execution_mode: Option<String>,
    render_shell: Option<String>,
}

/// Metadata payload for `pi.registerCommand`.
#[derive(Deserialize)]
struct CommandInput {
    name: String,
    description: Option<String>,
}

/// Metadata payload for `pi.registerShortcut`.
#[derive(Deserialize)]
struct ShortcutInput {
    shortcut: String,
    description: Option<String>,
}

/// Metadata payload for `pi.registerFlag`.
#[derive(Deserialize)]
struct FlagInput {
    name: String,
    #[serde(rename = "type")]
    flag_type: String,
    default: Option<Value>,
}

/// Borrow the shared inventory out of the op state.
fn inventory(state: &mut OpState) -> SharedInventory {
    state.borrow::<SharedInventory>().clone()
}

/// `pi.registerTool(tool)` — record the tool's metadata.
#[op2(fast)]
fn op_register_tool(state: &mut OpState, #[string] payload: String) {
    let Ok(input) = serde_json::from_str::<ToolInput>(&payload) else {
        return;
    };
    let record = ToolRecord {
        label: input.label.clone().unwrap_or_else(|| input.name.clone()),
        name: input.name,
        description: input.description.unwrap_or_default(),
        parameters: input.parameters.unwrap_or(Value::Null),
        prompt_snippet: input.prompt_snippet,
        prompt_guidelines: input.prompt_guidelines,
        execution_mode: input.execution_mode,
        render_shell: input.render_shell,
    };
    inventory(state).borrow_mut().tools.push(record);
}

/// `pi.on(event, handler)` — record the hook subscription.
#[op2(fast)]
fn op_register_hook(state: &mut OpState, #[string] event: String) {
    inventory(state)
        .borrow_mut()
        .hooks
        .push(HookRecord { event });
}

/// `pi.registerCommand(name, options)` — record the command's metadata.
#[op2(fast)]
fn op_register_command(state: &mut OpState, #[string] payload: String) {
    let Ok(input) = serde_json::from_str::<CommandInput>(&payload) else {
        return;
    };
    inventory(state).borrow_mut().commands.push(CommandRecord {
        name: input.name,
        description: input.description,
    });
}

/// `pi.registerShortcut(shortcut, options)` — record the shortcut's metadata.
#[op2(fast)]
fn op_register_shortcut(state: &mut OpState, #[string] payload: String) {
    let Ok(input) = serde_json::from_str::<ShortcutInput>(&payload) else {
        return;
    };
    inventory(state)
        .borrow_mut()
        .shortcuts
        .push(ShortcutRecord {
            shortcut: input.shortcut,
            description: input.description,
        });
}

/// `pi.registerFlag(name, options)` — record the flag and initialize its runtime
/// value to the declared default (mirroring pi's `runtime.flagValues.set`).
#[op2(fast)]
fn op_register_flag(state: &mut OpState, #[string] payload: String) {
    let Ok(input) = serde_json::from_str::<FlagInput>(&payload) else {
        return;
    };
    let value = input.default.clone();
    inventory(state).borrow_mut().flags.push(FlagRecord {
        name: input.name,
        flag_type: input.flag_type,
        default: input.default,
        value,
    });
}

/// `pi.getFlag(name)` — the current value of a registered flag, or `null`
/// (mapped to `undefined` in JS) when the flag was never registered.
// The `#[serde]` return type must be spelled fully-qualified: the deno_ops
// macro pattern-matches on the literal `serde_json::Value` / `v8::Value` path
// tokens and rejects the imported `Value` alias ("Invalid or deprecated #[serde]
// type").
#[op2]
#[serde]
fn op_get_flag(state: &mut OpState, #[string] name: String) -> serde_json::Value {
    inventory(state)
        .borrow()
        .flag_value(&name)
        .unwrap_or(Value::Null)
}

/// `pi.registerMessageRenderer(customType, renderer)` — record the renderer type.
#[op2(fast)]
fn op_register_message_renderer(state: &mut OpState, #[string] custom_type: String) {
    inventory(state)
        .borrow_mut()
        .message_renderers
        .push(RendererRecord { custom_type });
}

/// `pi.registerEntryRenderer(customType, renderer)` — record the renderer type.
#[op2(fast)]
fn op_register_entry_renderer(state: &mut OpState, #[string] custom_type: String) {
    inventory(state)
        .borrow_mut()
        .entry_renderers
        .push(RendererRecord { custom_type });
}

extension!(
    atilla_api_ops,
    ops = [
        op_register_tool,
        op_register_hook,
        op_register_command,
        op_register_shortcut,
        op_register_flag,
        op_get_flag,
        op_register_message_renderer,
        op_register_entry_renderer,
    ],
);

/// Build the `deno_core` extension carrying the `ExtensionAPI` ops.
pub fn extension() -> deno_core::Extension {
    atilla_api_ops::init()
}

/// The bootstrap script that installs `globalThis.__pi` (the `ExtensionAPI`
/// handle) and the loader helpers. Run once, before any extension loads.
///
/// Registration methods keep the JS closure in `__atilla.registry` and forward
/// only metadata to the ops. Action methods are documented no-op stubs (see the
/// module docs' "implemented-only exposure").
pub const BOOTSTRAP_JS: &str = r#"
globalThis.__atilla = {
  // JS-side handles the ops never see: tool.execute, hook handlers, renderers.
  // Keyed by name so PR-F can invoke them over the OwnRuntime rendezvous.
  registry: {
    tools: new Map(),
    hooks: new Map(),
    commands: new Map(),
    shortcuts: new Map(),
    messageRenderers: new Map(),
    entryRenderers: new Map(),
  },
};

// Bare deno_core exposes Deno.core.createTimer but no web-standard setTimeout;
// add a minimal shim so an extension that schedules a macrotask at load works.
globalThis.setTimeout = (cb, ms) =>
  Deno.core.createTimer(cb, ms ?? 0, undefined, false, true, false);

const ops = Deno.core.ops;
const reg = globalThis.__atilla.registry;

// A loud, documented no-op: an unimplemented capability records nothing and
// returns a benign empty value, so a factory calling it at load does not crash.
// These land in PR-F (hook dispatch + session wiring).
function unimplemented(returnValue) {
  return () => returnValue;
}

const pi = {
  // ---- Registration (implemented) --------------------------------------
  on(event, handler) {
    const list = reg.hooks.get(event) ?? [];
    list.push(handler);
    reg.hooks.set(event, list);
    ops.op_register_hook(event);
  },

  registerTool(tool) {
    reg.tools.set(tool.name, tool);
    ops.op_register_tool(JSON.stringify({
      name: tool.name,
      label: tool.label ?? tool.name,
      description: tool.description ?? "",
      parameters: tool.parameters ?? {},
      prompt_snippet: tool.promptSnippet ?? null,
      prompt_guidelines: tool.promptGuidelines ?? null,
      execution_mode: tool.executionMode ?? null,
      render_shell: tool.renderShell ?? null,
    }));
  },

  registerCommand(name, options) {
    reg.commands.set(name, { name, ...(options ?? {}) });
    ops.op_register_command(JSON.stringify({
      name,
      description: (options && options.description) ?? null,
    }));
  },

  registerShortcut(shortcut, options) {
    reg.shortcuts.set(shortcut, { shortcut, ...(options ?? {}) });
    ops.op_register_shortcut(JSON.stringify({
      shortcut,
      description: (options && options.description) ?? null,
    }));
  },

  registerFlag(name, options) {
    ops.op_register_flag(JSON.stringify({
      name,
      type: options.type,
      default: options.default ?? null,
    }));
  },

  getFlag(name) {
    const value = ops.op_get_flag(name);
    return value === null ? undefined : value;
  },

  registerMessageRenderer(customType, renderer) {
    reg.messageRenderers.set(customType, renderer);
    ops.op_register_message_renderer(customType);
  },

  registerEntryRenderer(customType, renderer) {
    reg.entryRenderers.set(customType, renderer);
    ops.op_register_entry_renderer(customType);
  },

  // ---- Action methods (stubbed; PR-F) ----------------------------------
  sendMessage: unimplemented(undefined),
  sendUserMessage: unimplemented(undefined),
  appendEntry: unimplemented(undefined),
  setSessionName: unimplemented(undefined),
  getSessionName: unimplemented(undefined),
  setLabel: unimplemented(undefined),
  exec: unimplemented(Promise.resolve({ stdout: "", stderr: "", exitCode: 0 })),
  getActiveTools: unimplemented([]),
  setActiveTools: unimplemented(undefined),
  getAllTools: unimplemented([]),
  getCommands: unimplemented([]),
  setModel: unimplemented(Promise.resolve(false)),
  getThinkingLevel: unimplemented(undefined),
  setThinkingLevel: unimplemented(undefined),
  registerProvider: unimplemented(undefined),
  unregisterProvider: unimplemented(undefined),

  // Minimal EventBus stub: on/off/emit are no-ops.
  events: { on() {}, off() {}, emit() {}, once() {} },
};

globalThis.__pi = pi;

// ---- Hook DISPATCH surface (PR-F) --------------------------------------
// The Rust ExtensionRunner drives the dispatch loop and result-shaping; JS only
// runs one handler at a time over the OwnRuntime rendezvous. These helpers are
// the JS half: enumerate a hook's handlers, build the `ctx` passed to a handler,
// and invoke handler N with a JSON event + ctx, returning a plain-data envelope.

// The number of handlers registered for an event, across all loaded extensions
// in load-then-registration order (the order Rust indexes into).
globalThis.__atilla.handlerCount = (event) => (reg.hooks.get(event) ?? []).length;

// Build the `ctx` object handed to a handler. Only the data GETTERS the
// acceptance suite reads are live: getSystemPrompt() returns the value Rust
// threads in (kept in sync with the chained before_agent_start prompt). The
// action methods (sendMessage/appendEntry/setModel/exec/setActiveTools/…) are
// present-but-no-op — no acceptance fixture calls one (see the dispatch-boundary
// analysis), so they exist only so a handler that touches ctx does not crash.
globalThis.__atilla.makeContext = (data) => {
  data = data ?? {};
  const noop = () => {};
  return {
    getSystemPrompt: () => data.systemPrompt ?? "",
    cwd: data.cwd ?? "",
    mode: data.mode ?? "print",
    hasUI: data.hasUI ?? false,
    isProjectTrusted: () => data.projectTrusted ?? true,
    sendMessage: noop,
    sendUserMessage: noop,
    appendEntry: noop,
    setSessionName: noop,
    getSessionName: () => undefined,
    setLabel: noop,
    exec: async () => ({ stdout: "", stderr: "", exitCode: 0 }),
    getActiveTools: () => [],
    setActiveTools: noop,
    getAllTools: () => [],
    setModel: async () => false,
    getThinkingLevel: () => undefined,
    setThinkingLevel: noop,
    abort: noop,
    compact: async () => {},
  };
};

// Invoke handler `index` for `event`, given the event + ctx as JSON strings.
// Only JSON crosses the boundary: Rust passes `eventJson`/`ctxJson` as JS string
// literals (see crate::dispatch), this parses them, runs the (awaited) handler,
// and returns a JSON.stringify'd envelope string. A thrown handler is isolated
// into an error envelope (never propagated), so one bad handler cannot kill the
// runtime. The returned `event` is the (possibly mutated-in-place) event object,
// so Rust can observe in-place mutations (e.g. before_provider_headers writing
// event.headers). `index` arrives as a string; array indexing coerces it.
globalThis.__atilla.invokeHook = async (event, index, eventJson, ctxJson) => {
  const eventObj = JSON.parse(eventJson);
  const handlers = reg.hooks.get(event) ?? [];
  const handler = handlers[index];
  if (typeof handler !== "function") {
    return JSON.stringify({ ok: true, result: null, event: eventObj });
  }
  const ctx = globalThis.__atilla.makeContext(JSON.parse(ctxJson));
  try {
    const result = await handler(eventObj, ctx);
    return JSON.stringify({
      ok: true,
      result: result === undefined ? null : result,
      event: eventObj,
    });
  } catch (err) {
    return JSON.stringify({
      ok: false,
      result: null,
      event: eventObj,
      error: err instanceof Error ? err.message : String(err),
      stack: err instanceof Error ? err.stack : undefined,
    });
  }
};
"#;
