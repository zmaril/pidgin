#!/usr/bin/env node
// Test-only shim: logs the exact `fd` invocation (args + stdout + exit code) to
// $FD_LOG, then transparently passes through the real fd binary. Used by
// generate_autocomplete.mjs to record fd's output at the host seam so the Rust
// vector replay is deterministic without needing fd installed.
import { spawnSync } from "node:child_process";
import { appendFileSync } from "node:fs";
const REAL_FD = process.env.REAL_FD || "/usr/local/bin/fd";
const args = process.argv.slice(2);
const r = spawnSync(REAL_FD, args, { encoding: "buffer", maxBuffer: 64 * 1024 * 1024 });
const stdout = r.stdout ? r.stdout.toString("utf-8") : "";
if (process.env.FD_LOG) {
	appendFileSync(process.env.FD_LOG, JSON.stringify({ args, stdout, code: r.status }) + "\n");
}
if (r.stdout) process.stdout.write(r.stdout);
if (r.stderr) process.stderr.write(r.stderr);
process.exit(r.status ?? 0);
