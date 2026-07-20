// straitjacket-allow-file:duplication — this harness is the root of the
// `agent-bridge-*.mjs` / `session-call-seam.mjs` proof-harness family whose
// `drive` dispatcher + `assert`/`assertEq` scaffold is deliberately mirrored
// across each self-contained file (the siblings carry the same marker).
//
// Bridge slice 1 — STEP A: the core primitive, in isolation.
//
// Proves the NonBlocking-TSFN + resolve-channel round-trip works from a
// dedicated off-runtime Rust thread, and that the steward's hard conditions hold
// at the primitive level: rejection surfaces (A), the event loop is never
// starved (B/G), abort/double-resolve is a no-op (E), and out-of-order
// concurrent resolution routes by id (F). The process must exit 0 on its own —
// no lingering handle — which is condition (C).
//
// Run: node __tests__/agent-bridge-primitive.mjs   (after `npm run build:debug`)

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

// A dispatcher factory mirroring the JS side of the envelope protocol: it parses
// the envelope, routes by `kind` to a handler, then resolves the parked Rust id
// through the bridge (success → resolveBridge, throw/reject → resolveBridgeError).
// On the terminal `__complete__` envelope it resolves the returned Promise.
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
        bridge.join(); // reap the loop thread before we let the process settle
        resolve(payload);
        return;
      }
      const handler = handlers[kind];
      if (!handler) {
        bridge.resolveBridgeError(id, JSON.stringify({ __bridge_error: `no handler for ${kind}` }));
        return;
      }
      // Wrap sync + async handlers uniformly; any throw/rejection surfaces via
      // resolveBridgeError so the parked Rust thread is released, never hung.
      Promise.resolve()
        .then(() => handler(payload, id, bridge))
        .then((result) => {
          if (result !== undefined) bridge.resolveBridge(id, JSON.stringify(result));
        })
        .catch((e) =>
          bridge.resolveBridgeError(
            id,
            JSON.stringify({ __bridge_error: String(e?.message ?? e) }),
          ),
        );
    };
    spawn(dispatcher);
  });
}

async function testBasicRoundTrip() {
  console.log("# basic round-trip: Rust thread gets the JS-produced value");
  const bridge = new AgentBridge();
  const out = await drive(bridge, (d) => bridge.spikeEcho(d, JSON.stringify(["alpha", "beta"])), {
    echo: (payload) => ({ echoed: payload.value.toUpperCase() }),
  });
  assertEq(out.results, [{ echoed: "ALPHA" }, { echoed: "BETA" }], "serial echoes round-trip in order");
}

async function testEventLoopNotStarved() {
  console.log("# event loop keeps running while the Rust thread is parked (B/G)");
  const bridge = new AgentBridge();
  let timerFired = false;
  // A timer scheduled on the JS thread must fire while Rust blocks on rx.recv().
  const t = setTimeout(() => {
    timerFired = true;
  }, 5);
  const out = await drive(bridge, (d) => bridge.spikeEcho(d, JSON.stringify(["x"])), {
    echo: async (payload) => {
      await sleep(20); // await real async work before resolving
      return { echoed: payload.value, timerFired };
    },
  });
  clearTimeout(t);
  assert(out.results[0].timerFired === true, "setTimeout fired while the Rust thread was parked");
  assertEq(out.results[0].echoed, "x", "async handler resolved the parked round-trip");
}

async function testRejectionPath() {
  console.log("# rejection path: a throwing JS handler surfaces, never hangs (A)");
  const bridge = new AgentBridge();
  const out = await drive(bridge, (d) => bridge.spikeEcho(d, JSON.stringify(["ok", "boom"])), {
    echo: (payload) => {
      if (payload.value === "boom") throw new Error("handler exploded");
      return { echoed: payload.value };
    },
  });
  assertEq(out.results[0], { echoed: "ok" }, "first (good) request round-trips");
  assert(
    out.results[1] && out.results[1].__bridge_error === "handler exploded",
    "thrown handler delivered a clean error to Rust (no hang)",
  );
}

async function testDoubleResolveNoop() {
  console.log("# double-resolve / unknown id is a no-op, never a panic (E)");
  const bridge = new AgentBridge();
  const out = await drive(bridge, (d) => bridge.spikeEcho(d, JSON.stringify(["dup"])), {
    echo: (payload, id, br) => {
      br.resolveBridge(id, JSON.stringify({ echoed: payload.value, first: true }));
      // Second resolve of the same id + an unknown id: both must be ignored.
      br.resolveBridge(id, JSON.stringify({ echoed: "SECOND", first: false }));
      br.resolveBridge(999999, JSON.stringify({ bogus: true }));
      br.resolveBridgeError(id, JSON.stringify({ __bridge_error: "late" }));
      return undefined; // already resolved above
    },
  });
  assertEq(out.results[0], { echoed: "dup", first: true }, "only the first resolve took effect");
}

async function testOutOfOrderConcurrent() {
  console.log("# out-of-order concurrent resolution routes by id (F)");
  const bridge = new AgentBridge();
  const pending = [];
  const out = await drive(bridge, (d) => bridge.spikeConcurrent(d, 5), {
    echoConcurrent: (payload, id, br) => {
      // Collect all 5, then resolve them in REVERSE order to force out-of-order
      // delivery; correct routing means each id still gets its own index.
      pending.push({ id, index: payload.index });
      if (pending.length === 5) {
        for (const p of pending.reverse()) {
          br.resolveBridge(p.id, JSON.stringify({ index: p.index, doubled: p.index * 2 }));
        }
      }
      return undefined;
    },
  });
  assertEq(
    out.results,
    [
      { index: 0, doubled: 0 },
      { index: 1, doubled: 2 },
      { index: 2, doubled: 4 },
      { index: 3, doubled: 6 },
      { index: 4, doubled: 8 },
    ],
    "each index received its own out-of-order result",
  );
}

async function main() {
  await testBasicRoundTrip();
  await testEventLoopNotStarved();
  await testRejectionPath();
  await testDoubleResolveNoop();
  await testOutOfOrderConcurrent();

  console.log("");
  if (failures === 0) {
    console.log("STEP A: ALL PRIMITIVE CHECKS PASSED");
  } else {
    console.log(`STEP A: ${failures} CHECK(S) FAILED`);
    process.exitCode = 1;
  }
  // No explicit process.exit(): if any TSFN/thread handle leaked, Node would
  // hang here instead of exiting — a clean exit is itself condition (C).
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
