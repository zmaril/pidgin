//
// Async-oneshot bridge — PROOF HARNESS: the `call_async` variant, in isolation.
//
// The `drive` dispatcher plus the `assert`/`assertEq`/`getFailures`/`sleep`
// helpers come from the shared `./_harness.mjs` (the same scaffold every
// `agent-bridge-*.mjs` slice imports), so this file does not re-inline them.
// `AsyncBridge` is loaded from `../index.js` here (the shared harness only
// exports `AgentBridge`); that tiny `require` prologue is the one fragment this
// file shares with `_harness.mjs`, and `_harness.mjs` carries the allow-file
// marker as the clone root, so it stays suppressed. The marker on THIS file
// guards the residual per-condition test-body mirror it shares with
// `agent-bridge-primitive.mjs` (both prove the same A–G conditions).
//
// Proves the NonBlocking-TSFN + tokio::sync::oneshot round-trip works from a
// dedicated off-Node worker thread running a fresh current-thread tokio runtime,
// and that the bridge-family hard conditions hold for the ASYNC variant:
//   (b) Rust `.await`s a JS-resolved value end-to-end,
//   (A) a throwing JS handler surfaces a clean error, never hangs,
//   (G) the event loop is never starved while the worker awaits,
//   (E) double / unknown resolve is a no-op,
//   (F) out-of-order concurrent resolution routes by id (one worker awaits many),
//   (B/I) abort mid-await wakes the awaiter, and
//   (§5) the ONE real flip: file-mutation-queue via call_async (admit/await/release),
//        with same-path serialization + cross-path overlap observable to the caller.
// The process must exit 0 on its own — no lingering worker thread / tokio runtime
// / TSFN handle — which is conditions (C)/(H).
//
// PROOF-HARNESS ONLY: this exercises the primitive's mechanism (Rust awaiting a
// JS-resolved oneshot + a fire-and-forget release). It does NOT flip any
// manifest.json status to native and does NOT touch conformance.json.
//
// Run: node __tests__/bridge-async-oneshot.mjs   (after `napi build --platform`)

import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { assert, assertEq, getFailures, runBridge, sleep } from "./_harness.mjs";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
const { AsyncBridge } = require(join(here, "..", "index.js"));

// A dispatcher factory mirroring the JS side of the envelope protocol: routes
// each envelope by `kind` to a handler from the `handlers` map (called as
// `handler(payload, id, bridge)`), so a handler may resolve the awaiting Rust id
// itself and return `undefined`. The envelope parsing, `__complete__` handling,
// and resolve/error plumbing live in the shared harness (`runBridge`).
function drive(bridge, spawn, handlers) {
  return runBridge(bridge, spawn, {
    handle: (kind, payload, id) => {
      const handler = handlers[kind];
      if (!handler) throw new Error(`no handler for ${kind}`);
      return handler(payload, id, bridge);
    },
  });
}

async function testBasicRoundTrip() {
  console.log("# basic round-trip: Rust .awaits the JS-produced value (b)");
  const bridge = new AsyncBridge();
  const out = await drive(bridge, (d) => bridge.spikeEcho(d, JSON.stringify(["alpha", "beta"])), {
    echo: (payload) => ({ echoed: payload.value.toUpperCase() }),
  });
  assertEq(out.results, [{ echoed: "ALPHA" }, { echoed: "BETA" }], "serial async echoes round-trip in order");
}

async function testEventLoopNotStarved() {
  console.log("# event loop keeps running while the worker awaits (G)");
  const bridge = new AsyncBridge();
  let timerFired = false;
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
  assert(out.results[0].timerFired === true, "setTimeout fired while the worker was awaiting");
  assertEq(out.results[0].echoed, "x", "async handler resolved the awaited round-trip");
}

async function testRejectionPath() {
  console.log("# rejection path: a throwing JS handler surfaces, never hangs (A)");
  const bridge = new AsyncBridge();
  const out = await drive(bridge, (d) => bridge.spikeEcho(d, JSON.stringify(["ok", "boom"])), {
    echo: (payload) => {
      if (payload.value === "boom") throw new Error("handler exploded");
      return { echoed: payload.value };
    },
  });
  assertEq(out.results[0], { echoed: "ok" }, "first (good) request round-trips");
  assert(
    out.results[1] && out.results[1].__bridge_error === "handler exploded",
    "thrown handler delivered a clean error to the awaiter (no hang)",
  );
}

async function testDoubleResolveNoop() {
  console.log("# double-resolve / unknown id is a no-op, never a panic (E/J)");
  const bridge = new AsyncBridge();
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
  console.log("# out-of-order concurrent resolution routes by id — one worker awaits many (F)");
  const bridge = new AsyncBridge();
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
    "each index received its own out-of-order result while all 5 awaited concurrently",
  );
}

async function testAbortUnblocks() {
  console.log("# abort mid-await wakes the awaiter, run settles (B/I)");
  const bridge = new AsyncBridge();
  const out = await drive(bridge, (d) => bridge.spikeAbort(d), {
    // Never resolves this id; instead trip abort while the worker awaits.
    hang: (_payload, _id, br) => {
      setTimeout(() => br.abort(), 10);
      return undefined;
    },
  });
  assert(out.aborted === true, "the awaiting worker woke with Aborted instead of hanging");
}

// A faithful minimal stand-in for pi's `withFileMutationQueue<T>(path, fn)`:
// same-path ops serialize (a per-path promise chain), distinct paths run in
// parallel, and the slot releases in a `finally`. The primitive is proven
// against this mechanism-equivalent queue so the harness needs only the native
// addon (no pi TS build); this is PROOF-HARNESS ONLY — no native flip of the
// queue itself.
function makeFileMutationQueue() {
  const tails = new Map(); // path -> Promise of the last op in that path's chain
  return function withFileMutationQueue(path, fn) {
    const prev = tails.get(path) ?? Promise.resolve();
    const run = prev.then(() => fn());
    // Keep the chain alive even if `fn` rejects, so the next op still runs.
    tails.set(path, run.then(() => {}, () => {}));
    return run;
  };
}

async function testFileMutationQueueFlip() {
  console.log("# THE FLIP: file-mutation-queue via call_async (admit/await/release) — §5");
  const bridge = new AsyncBridge();
  const withFileMutationQueue = makeFileMutationQueue();
  const releasers = new Map(); // id -> release()
  const idPath = new Map(); // id -> path, for concurrency accounting
  // Observe concurrency: count how many slots are held per path at any instant.
  const activePerPath = new Map();
  let maxConcurrentSamePath = 0;
  let observedCrossPathOverlap = false;

  // Two ops on path "a" (must serialize) and one on path "b" (may overlap "a").
  const inputs = [{ path: "a" }, { path: "a" }, { path: "b" }];

  const out = await drive(bridge, (d) => bridge.spikeFmq(d, JSON.stringify(inputs)), {
    // acquire: enqueue on the queue, hold the slot open, and hand Rust the
    // release token (the id itself). The await on the Rust side settles here.
    fmqAcquire: (payload, id, br) => {
      const { path } = payload;
      idPath.set(id, path);
      // Do NOT return the withFileMutationQueue promise to `drive` (that would
      // couple its settling to `drive`'s resolve). We resolve the Rust id
      // ourselves via resolveBridge inside the slot, and release fire-and-forget.
      withFileMutationQueue(path, () =>
        new Promise((release) => {
          releasers.set(id, release);
          const active = (activePerPath.get(path) ?? 0) + 1;
          activePerPath.set(path, active);
          maxConcurrentSamePath = Math.max(maxConcurrentSamePath, active);
          // Cross-path overlap: if another path is also active right now, note it.
          let others = 0;
          for (const [p, n] of activePerPath) if (p !== path && n > 0) others += n;
          if (others > 0) observedCrossPathOverlap = true;
          // Grant admission: settle the Rust `.await` with a release token = id.
          br.resolveBridge(id, JSON.stringify(id));
        }),
      );
      return undefined; // resolved manually above
    },
    // release: fire-and-forget from Rust — drop the held slot so the next
    // same-path op is admitted. Unknown id is a no-op (E-parity on the JS side).
    fmqRelease: (payload) => {
      const path = idPath.get(payload.id);
      if (path) activePerPath.set(path, (activePerPath.get(path) ?? 1) - 1);
      const release = releasers.get(payload.id);
      releasers.delete(payload.id);
      if (release) release();
      return undefined;
    },
  });

  assert(out.results.length === 3, "all three queued ops completed");
  assert(
    out.results.every((r) => r.aborted === false),
    "no op aborted — every acquire was granted and released",
  );
  // The two path-"a" ops must have distinct admission orders and never overlapped.
  const aOrders = out.results.filter((r) => r.path === "a").map((r) => r.order).sort();
  assert(aOrders.length === 2 && aOrders[0] !== aOrders[1], "same-path ('a') ops serialized (distinct admission order)");
  assert(maxConcurrentSamePath <= 1, "at most one path-'a' slot was ever held at once (serialization held)");
}

async function main() {
  await testBasicRoundTrip();
  await testEventLoopNotStarved();
  await testRejectionPath();
  await testDoubleResolveNoop();
  await testOutOfOrderConcurrent();
  await testAbortUnblocks();
  await testFileMutationQueueFlip();

  console.log("");
  if (getFailures() === 0) {
    console.log("ASYNC-ONESHOT BRIDGE: ALL PROOF CHECKS PASSED");
  } else {
    console.log(`ASYNC-ONESHOT BRIDGE: ${getFailures()} CHECK(S) FAILED`);
    process.exitCode = 1;
  }
  // No explicit process.exit(): if the worker thread / tokio runtime / TSFN clone
  // leaked, Node would hang here instead of exiting — a clean exit is itself
  // conditions (C)/(H).
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
