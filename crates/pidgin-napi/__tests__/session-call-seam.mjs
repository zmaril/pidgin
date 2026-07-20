// straitjacket-allow-file:duplication
//
// This proof harness mirrors the `agent-bridge-primitive.mjs` envelope-protocol
// scaffold (the `drive` dispatcher + `assert`/`assertEq` helpers + per-condition
// test shape) on purpose, exactly as the sibling `agent-bridge-{loop,tools,
// hooks}.mjs` harnesses do (which carry the same marker); keeping each harness
// self-contained reads better than a shared helper module, at the cost of this
// intentional mirror duplication.
//
// Session `call`-seam — PROOF HARNESS: the two `session.test.ts` injected-closure
// cases (`entryTransforms` + `entryProjectors`) driven against the ALREADY-PORTED
// Rust session context builder
// (`pidgin_agent::harness::session::build_session_context`) through the BLOCKING
// `call` bridge seam (`AgentBridge::spikeSession`).
//
// This is the sibling of what PR 253 did for `call_async`: it proves the blocking
// `call` seam on a real agent-core session case. The Rust `build_context_entries`
// invokes each JS-supplied `ContextEntryTransform`, and
// `session_entry_to_context_messages` invokes each JS-supplied
// `CustomEntryProjector`, as a blocking Rust→JS→Rust round-trip: the Rust worker
// thread parks on `rx.recv()` OFF the Node event loop, JS runs the real test
// closure (`dropCompaction` / the `chat_message` projector), resolves the id, and
// the parked thread wakes with the value.
//
// The two cases reproduced verbatim from
// vendor/pi/packages/agent/test/harness/session.test.ts:
//   * ":107" projects custom entries with configured custom-entry projectors, and
//   * ":120" applies context entry transforms after default compaction selection.
// The fixtures are the exact root-to-leaf path (`Session.getBranch()`) each case
// produces; the assertions mirror the test's own `.toEqual` / `.toMatchObject`.
//
// It also exercises the bridge-family hard conditions for the session shape:
//   (G) the event loop is never starved while the Rust build thread is parked,
//   (A) a throwing JS transform surfaces cleanly (Rust's identity fallback), never
//       hangs, and
//   (B) abort mid-round-trip wakes the parked build thread.
// The process must exit 0 on its own — no lingering worker thread / TSFN handle —
// which is condition (C).
//
// PROOF-HARNESS ONLY: this exercises the `call` seam on the real session shape. It
// does NOT flip any manifest.json status to native and does NOT touch
// conformance.json — the value is proving the seam, not a native flip of the
// session logic.
//
// UNBRIDGEABLE-BY-DESIGN (documented, not wired): `jsonl-repo.ts` open()/create()
// return a live `Session` handle and `repo.test.ts:16` asserts `.toBe(session)`
// object identity. A JSON `call` boundary cannot preserve `.toBe` — V8 handles
// never cross it (the NativeAgent verdict, `native-count-honesty-no-nominal-flips`;
// same rule as pidgin-extensions/src/runtime.rs). That case stays `original`; see
// the matching note on `AgentBridge::spike_session` in
// crates/pidgin-napi/src/agent_bridge.rs.
//
// Run: node __tests__/session-call-seam.mjs   (after `napi build --platform`)

import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
const { AgentBridge } = require(join(here, "..", "index.js"));

let failures = 0;
function assert(cond, msg) {
  if (cond) {
    console.log(`  ok - ${msg}`);
  } else {
    failures += 1;
    console.log(`  NOT OK - ${msg}`);
  }
}
function assertEq(actual, expected, msg) {
  assert(
    JSON.stringify(actual) === JSON.stringify(expected),
    `${msg} (got ${JSON.stringify(actual)}, want ${JSON.stringify(expected)})`,
  );
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// --- session.test.ts fixtures ---------------------------------------------
// createUserMessage / getTextData are copied verbatim from the pi test's
// session-test-utils.ts + session.test.ts so the closures we run over the bridge
// are byte-identical to what pi's own tests inject.
function createUserMessage(text) {
  return { role: "user", content: [{ type: "text", text }], timestamp: 0 };
}
function getTextData(data) {
  if (typeof data !== "object" || data === null || !("text" in data)) return "";
  const value = data.text;
  return typeof value === "string" ? value : "";
}

const TS = "2026-01-01T00:00:00.000Z";
const msgEntry = (id, parentId, text) => ({
  type: "message",
  id,
  parentId,
  timestamp: TS,
  message: createUserMessage(text),
});

// A dispatcher factory mirroring the JS side of the envelope protocol: parse the
// envelope, route by `kind` to a handler, then resolve the parked Rust id through
// the bridge (success → resolveBridge, throw/reject → resolveBridgeError). On the
// terminal `__complete__` envelope it resolves the returned Promise.
function drive(bridge, spawn, handlers) {
  return new Promise((resolve, reject) => {
    const dispatcher = (envelopeJson) => {
      let env;
      try {
        env = JSON.parse(envelopeJson);
      } catch (e) {
        reject(e);
        return;
      }
      const { id, kind, payload } = env;
      if (kind === "__complete__") {
        bridge.join(); // reap the build thread before we let the process settle
        resolve(payload);
        return;
      }
      const handler = handlers[kind];
      if (!handler) {
        bridge.resolveBridgeError(id, JSON.stringify({ __bridge_error: `no handler for ${kind}` }));
        return;
      }
      Promise.resolve()
        .then(() => handler(payload, id, bridge))
        .then((result) => {
          if (result !== undefined) bridge.resolveBridge(id, JSON.stringify(result));
        })
        .catch((e) =>
          bridge.resolveBridgeError(id, JSON.stringify({ __bridge_error: String(e?.message ?? e) })),
        );
    };
    spawn(dispatcher);
  });
}

// The real "applies context entry transforms after default compaction selection"
// case (session.test.ts:120). Path: user "one", user "two" (kept), compaction,
// user "three". The Rust default transform selects [compaction, two, three]; the
// injected `dropCompaction` transform — run in JS over the blocking seam — must
// observe `compaction` first and drop it, leaving two user messages.
async function testEntryTransformCase() {
  console.log("# session.test.ts:120 — entryTransforms over the blocking `call` seam");
  const bridge = new AgentBridge();
  let observedFirstEntryType;
  const dropCompaction = (entries) => {
    observedFirstEntryType = entries[0]?.type;
    return entries.filter((entry) => entry.type !== "compaction");
  };
  const pathEntries = [
    msgEntry("m1", null, "one"),
    msgEntry("m2", "m1", "two"),
    {
      type: "compaction",
      id: "c1",
      parentId: "m2",
      timestamp: TS,
      summary: "summary",
      firstKeptEntryId: "m2",
      tokensBefore: 1234,
    },
    msgEntry("m3", "c1", "three"),
  ];
  const out = await drive(
    bridge,
    (d) =>
      bridge.spikeSession(
        d,
        JSON.stringify({ pathEntries, transformCount: 1, projectorTypes: [] }),
      ),
    {
      // The Rust `ContextEntryTransform` closure dispatched by index; run the real
      // test closure and hand the transformed entries back over the seam.
      entryTransform: (p) => {
        assertEq(p.index, 0, "transform dispatched by index");
        return dropCompaction(p.entries);
      },
    },
  );
  assert(observedFirstEntryType === "compaction", "transform observed compaction first (real closure ran in JS)");
  assertEq(
    out.messages.map((m) => m.role),
    ["user", "user"],
    "Rust built context has two user messages after JS dropped the compaction",
  );
}

// The real "projects custom entries with configured custom-entry projectors" case
// (session.test.ts:107). Path: user "one", custom `chat_message` {text:"hello"}.
// The Rust `session_entry_to_context_messages` invokes the JS projector over the
// blocking seam, which returns a user message "chat: hello".
async function testEntryProjectorCase() {
  console.log("# session.test.ts:107 — entryProjectors over the blocking `call` seam");
  const bridge = new AgentBridge();
  const projector = (entry) => [createUserMessage(`chat: ${getTextData(entry.data)}`)];
  const pathEntries = [
    msgEntry("m1", null, "one"),
    {
      type: "custom",
      id: "cu1",
      parentId: "m1",
      timestamp: TS,
      customType: "chat_message",
      data: { text: "hello" },
    },
  ];
  const out = await drive(
    bridge,
    (d) =>
      bridge.spikeSession(
        d,
        JSON.stringify({ pathEntries, transformCount: 0, projectorTypes: ["chat_message"] }),
      ),
    {
      entryProjector: (p) => {
        assertEq(p.customType, "chat_message", "projector dispatched by custom type");
        return projector(p.entry, p.index, p.entries);
      },
    },
  );
  assertEq(
    out.messages.map((m) => m.role),
    ["user", "user"],
    "Rust built context has the base user message plus the projected one",
  );
  assertEq(
    out.messages[1].content,
    [{ type: "text", text: "chat: hello" }],
    "the projected message carries the JS-produced content",
  );
}

// (G) The build thread parks on rx.recv() OFF the Node event loop, so a timer
// scheduled on the JS thread must fire while Rust is blocked inside the transform.
async function testEventLoopNotStarved() {
  console.log("# (G) event loop keeps running while the Rust build thread is parked");
  const bridge = new AgentBridge();
  let timerFired = false;
  const t = setTimeout(() => {
    timerFired = true;
  }, 5);
  const pathEntries = [msgEntry("m1", null, "one")];
  let observedTimerFired;
  await drive(
    bridge,
    (d) =>
      bridge.spikeSession(
        d,
        JSON.stringify({ pathEntries, transformCount: 1, projectorTypes: [] }),
      ),
    {
      entryTransform: async (p) => {
        await sleep(20); // await real async work before resolving
        observedTimerFired = timerFired;
        return p.entries;
      },
    },
  );
  clearTimeout(t);
  assert(observedTimerFired === true, "setTimeout fired while the Rust build thread was parked");
}

// (A) A throwing JS transform must surface cleanly, never hang: the Rust `call`
// returns an error and the transform seam falls back to identity (the input
// entries unchanged), so the build completes with the compaction still present.
async function testThrowingTransformFallsBack() {
  console.log("# (A) a throwing JS transform surfaces cleanly (identity fallback), never hangs");
  const bridge = new AgentBridge();
  const pathEntries = [
    msgEntry("m1", null, "one"),
    msgEntry("m2", "m1", "two"),
    {
      type: "compaction",
      id: "c1",
      parentId: "m2",
      timestamp: TS,
      summary: "summary",
      firstKeptEntryId: "m2",
      tokensBefore: 1234,
    },
  ];
  const out = await drive(
    bridge,
    (d) =>
      bridge.spikeSession(
        d,
        JSON.stringify({ pathEntries, transformCount: 1, projectorTypes: [] }),
      ),
    {
      entryTransform: () => {
        throw new Error("transform exploded");
      },
    },
  );
  // Identity fallback keeps the default selection [compaction, two] → the
  // compaction summary message survives, proving the parked thread was released.
  assert(
    out.messages[0]?.role === "compactionSummary",
    "throwing transform released the parked thread and fell back to identity",
  );
}

// (B) Abort mid-round-trip trips the cooperative signal and drains the parked id,
// so the build thread wakes (the transform seam falls back to identity) and the
// run still settles — never a deadlock.
async function testAbortReleasesParkedThread() {
  console.log("# (B) abort mid-round-trip wakes the parked build thread");
  const bridge = new AgentBridge();
  const pathEntries = [msgEntry("m1", null, "one")];
  const out = await drive(
    bridge,
    (d) =>
      bridge.spikeSession(
        d,
        JSON.stringify({ pathEntries, transformCount: 1, projectorTypes: [] }),
      ),
    {
      // Do not resolve — abort instead. Rust's `call` must return Aborted, the
      // transform falls back to identity, and the build completes.
      entryTransform: () => {
        bridge.abort();
        return undefined; // resolved by the abort-drain, not by us
      },
    },
  );
  assertEq(
    out.messages.map((m) => m.role),
    ["user"],
    "abort released the parked thread and the build completed",
  );
}

async function main() {
  await testEntryTransformCase();
  await testEntryProjectorCase();
  await testEventLoopNotStarved();
  await testThrowingTransformFallsBack();
  await testAbortReleasesParkedThread();

  console.log("");
  if (failures === 0) {
    console.log("SESSION CALL-SEAM: ALL CHECKS PASSED");
  } else {
    console.log(`SESSION CALL-SEAM: ${failures} CHECK(S) FAILED`);
    process.exitCode = 1;
  }
  // No explicit process.exit(): if any TSFN/thread handle leaked, Node would hang
  // here instead of exiting — a clean exit is itself condition (C).
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
