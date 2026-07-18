# Shim maintenance

This document is for the shim maintainer: the person (or automation) keeping
atilla's conformance harness honest as pi evolves and as modules migrate from
pi's TypeScript to the native Rust addon.

The harness lives under `conformance/` and is driven by `scripts/conformance.sh`.
It runs pi's own test suites against a codegen-materialized module tree and
records an honest baseline in `conformance.json` at the repo root. Two distinct
signals come out of a run:

- **Module conformance.** The `manifest.json` module list classifies every pi
  source module as `original` (pi's own file, used unchanged) or `native`
  (served by the Rust addon through a hand-written shim). The `native N` count
  and the per-package `by_package` totals measure this migration.
- **CLI conformance.** A black-box check that runs pi's command-line tests
  against the compiled `atilla` binary. It is tracked on its own and never
  folded into `native N` or the per-package native counts.

## CLI conformance

Module conformance swaps a pi module for a Rust-backed shim and re-runs pi's
unit tests through it. CLI conformance is different: it leaves pi's harness in
place and points a handful of end-to-end CLI tests at the real `atilla` binary,
so the shipping executable is exercised as a user would drive it.

### The four files

Four coding-agent CLI test files are repointed (all under
`vendor/pi/packages/coding-agent/test/`):

| File | Cases |
| --- | --- |
| `stdout-cleanliness.test.ts` | 5 |
| `session-id-readonly.test.ts` | 7 |
| `startup-session-name.test.ts` | 2 |
| `session-file-invalid.test.ts` | 1 |

That is 15 cases in total. The expected result is 15 passing.

### The `ATILLA_BIN` mechanism

Each original file spawns pi's own entrypoint:

```ts
const cliPath = resolve(__dirname, "../src/cli.ts");
// ...
const child = spawn(process.execPath, [cliPath, ...args], opts);
```

A repointed copy — hand-written under
`conformance/shims/packages/coding-agent/test/` and listed in the manifest's
`cli_repoint` array — spawns the compiled binary instead:

```ts
const ATILLA_BIN = process.env.ATILLA_BIN;
// ...
const child = spawn(ATILLA_BIN, [...args], opts);
```

Only those two lines change per file; everything else stays byte-for-byte,
including the `stdout-cleanliness` fake-npm fixture, which spawns a helper
script rather than the CLI and is left untouched.

### How a run wires it together

1. `scripts/conformance.sh` builds the binary with
   `cargo build -p atilla-cli --release`, landing it at `target/release/atilla`,
   and exports its absolute path as `ATILLA_BIN`. A failed release build falls
   back to the debug binary; a total build failure records the metric as
   env-blocked rather than faking a pass.
2. `conformance/codegen.mjs` overlays each repointed file onto the vendored pi
   tree, preserving pi's original beside it as `<name>.__pi_original__.ts` —
   the same overlay contract the native module shims use. The restore trap in
   `scripts/conformance.sh` cleans both the overlays and the backups after the
   run, so nothing leaks into git status.
3. The runner spawns vitest over the four files with `ATILLA_BIN` set and the
   working directory at `vendor/pi/packages/coding-agent`.
4. `conformance/parse-results.mjs` records the outcome under a separate
   `cli_conformance` key in `conformance.json` (`total`, `passing`, `failing`,
   `skipped`, and a per-file breakdown). It does not add to
   `manifest_native_modules` or to any `by_package` total.
5. `conformance/pr-comment.mjs` renders a dedicated CLI conformance section in
   the sticky pull-request comment — for example, `CLI conformance: 15/15 pass
   against target/release/atilla` — apart from the module smoke table.

### Why it is tracked apart from `native N`

`native N` answers "how much of pi's module surface is served by Rust?" CLI
conformance answers a different question: "does the binary a user actually runs
behave like pi at the command line?" A repointed CLI file passing does not flip
a module to native, and a module flipping to native does not change the CLI
result. Keeping the two metrics separate keeps each honest and legible.
