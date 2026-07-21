// straitjacket-allow-file:duplication — shared bridge-test scaffolding; the
// import prologue + dispatcher plumbing it consolidates necessarily still
// resembles the sibling `session-call-seam.mjs` harness (which carries the same
// marker). This is the first-sorting file of each clone pair, so the marker
// must live here to take effect.
// Shared scaffolding for the agent-bridge-*.mjs tests.
//
// These bridge tests each drove the native `AgentBridge` addon with the same
// copied prologue: load `../index.js`, an `assert`/`assertEq` pair backed by a
// failure counter, a `sleep` helper, and the `MODEL` / `zeroUsage` fixtures plus
// the faux message + faux-stream builders. That scaffolding is pidgin-napi's own
// (it does not mirror any pi source), so it is consolidated here and imported by
// each slice. Behavior is unchanged — the exports are the exact same values the
// per-file copies produced.

import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
export const { AgentBridge } = require(join(here, "..", "index.js"));

// --- assertions -----------------------------------------------------------
// A module-level failure counter, mirroring the per-file `let failures = 0`.
// Each test file runs as its own node process, so this counter is per-run.
let failures = 0;
export function assert(cond, msg) {
  if (cond) {
    console.log(`  ok - ${msg}`);
  } else {
    failures += 1;
    console.log(`  NOT OK - ${msg}`);
  }
}
export function assertEq(actual, expected, msg) {
  assert(
    JSON.stringify(actual) === JSON.stringify(expected),
    `${msg} (got ${JSON.stringify(actual)}, want ${JSON.stringify(expected)})`,
  );
}
export function getFailures() {
  return failures;
}

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// --- fixtures -------------------------------------------------------------
export const MODEL = {
  id: "faux-model",
  name: "Faux Model",
  api: "faux",
  provider: "faux",
  baseUrl: "http://localhost",
  reasoning: false,
  input: ["text"],
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
  contextWindow: 128000,
  maxTokens: 4096,
};

export const zeroUsage = {
  input: 0,
  output: 0,
  cacheRead: 0,
  cacheWrite: 0,
  totalTokens: 0,
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
};

export function assistantText(text, stopReason = "stop") {
  return {
    role: "assistant",
    content: [{ type: "text", text }],
    api: "faux",
    provider: "faux",
    model: "faux-model",
    usage: zeroUsage,
    stopReason,
    timestamp: 0,
  };
}

export function assistantToolCall(id, name, args) {
  return {
    role: "assistant",
    content: [{ type: "toolCall", id, name, arguments: args }],
    api: "faux",
    provider: "faux",
    model: "faux-model",
    usage: zeroUsage,
    stopReason: "toolUse",
    timestamp: 0,
  };
}

// A tiny async stream (mirrors pi's AssistantMessageEventStream contract):
// async-iterable of events + a `.result()` promise for the final message.
export function fakeStream(events, message) {
  return {
    async *[Symbol.asyncIterator]() {
      for (const ev of events) {
        await sleep(1); // force real async scheduling between events
        yield ev;
      }
    },
    async result() {
      await sleep(1);
      return message;
    },
  };
}

export function doneStream(message) {
  return fakeStream(
    [{ type: "done", reason: message.stopReason, message }],
    message,
  );
}

// Serialize a JS tool to the wire metadata the Rust `run` payload expects.
export function toolMeta(tool) {
  return {
    name: tool.name,
    label: tool.label ?? tool.name,
    description: tool.description ?? "",
    parameters: tool.parameters ?? {},
    executionMode: tool.executionMode ?? null,
    hasPrepareArguments: typeof tool.prepareArguments === "function",
  };
}

// A streamFn that returns a tool call on the first turn, then a plain text turn
// (`then`, default "done") on every turn after. The returned function carries a
// `.calls()` accessor reporting how many turns the loop actually requested, so a
// test can assert the loop stopped early.
export function toolThenDoneStreamFn(toolCallId, toolName, args, then = "done") {
  let callIndex = 0;
  const fn = async () => {
    const i = callIndex++;
    if (i === 0) return doneStream(assistantToolCall(toolCallId, toolName, args));
    return doneStream(assistantText(then));
  };
  fn.calls = () => callIndex;
  return fn;
}

// --- the JS side of the envelope protocol (the _bridge/dispatcher.ts shape) --
// Drain a JS async stream (a streamFn's return value) into the eager
// StreamResult JSON the Rust loop consumes: fully iterate its events, then take
// the final `.result()` message.
export async function drainStream(streamFn, p) {
  const stream = await streamFn(p.model, p.context, p.options);
  const events = [];
  for await (const ev of stream) events.push(ev);
  const message = await stream.result();
  return { events, message };
}

// The dispatcher plumbing shared by every bridge test: `spawn` starts the run
// with the built dispatcher, which parses each envelope, short-circuits the
// terminal `__complete__` (→ resolve) and fire-and-forget `event` kinds, and
// otherwise routes to `handle(kind, payload, id)` — resolving the parked Rust id
// with the handler's return value (a returned `undefined` means the handler
// already resolved the id itself), or surfacing a throw/rejection as a clean
// bridge error so the thread is released, never hung. The returned Promise
// settles with the `__complete__` payload.
// A `runBridge` specialized for the agent-loop tests: it starts the loop via
// `bridge.run(payload)` and handles the two kinds every slice needs — `streamFn`
// (drained) and `convertToLlm` — delegating every other kind to the slice's own
// `handle(kind, payload)` (a slice with no extra kinds omits it, and an unknown
// kind throws). Keeps each `agent-bridge-*.mjs` down to just its extra hooks.
export function runLoopBridge(bridge, payload, { streamFn, convertToLlm, onEvent, handle } = {}) {
  return runBridge(bridge, (d) => bridge.run(d, JSON.stringify(payload)), {
    onEvent,
    handle: async (kind, p, id) => {
      switch (kind) {
        case "streamFn":
          return await drainStream(streamFn, p);
        case "convertToLlm":
          return await convertToLlm(p.messages);
        default:
          if (handle) return handle(kind, p, id);
          throw new Error(`unhandled kind: ${kind}`);
      }
    },
  });
}

export function runBridge(bridge, spawn, { handle, onEvent } = {}) {
  return new Promise((resolve, reject) => {
    const dispatcher = (envelopeJson) => {
      let env;
      try {
        env = JSON.parse(envelopeJson);
      } catch (e) {
        reject(e);
        return;
      }
      const { id, kind, payload: p } = env;
      if (kind === "__complete__") {
        bridge.join();
        resolve(p);
        return;
      }
      if (kind === "event") {
        if (onEvent) onEvent(p);
        return; // fire-and-forget: no resolve
      }
      Promise.resolve()
        .then(() => handle(kind, p, id))
        .then((result) => {
          if (result !== undefined) bridge.resolveBridge(id, JSON.stringify(result ?? null));
        })
        .catch((e) =>
          bridge.resolveBridgeError(id, JSON.stringify({ __bridge_error: String(e?.message ?? e) })),
        );
    };
    spawn(dispatcher);
  });
}
