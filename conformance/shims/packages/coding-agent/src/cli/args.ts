// Native shim for packages/coding-agent/src/cli/args.ts, backed by the pidgin
// Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs when the
// module is marked `native` in conformance/manifest.json: the original pi file
// is preserved alongside as `args.__pi_original__.ts` and this shim takes its
// place, so pi's tests import `../src/cli/args.ts` unchanged and hit Rust.
//
// Scope of the native flip: pi's `parseArgs` is a pure argv loop — it walks the
// token list once, consuming flag values, capturing unknown `--long` flags,
// collecting `@files` and bare messages, and recording diagnostics, with no env,
// argv, or I/O access. The whole function is ported to Rust
// (`crates/pidgin-napi/src/cli_args.rs`, a faithful mirror of pi's loop) and
// exposed as `parseArgsNative`. Every one of the module's ~72 test cases exercises
// `parseArgs` alone, so the flip is entirely native — no case delegates to pi.
//
// The flip boundary: the native fn returns pi's `Args` as a JSON string in pi's
// exact camelCase shape. This shim parses it and rebuilds the single non-JSON
// member — `unknownFlags`, which pi models as a `Map<string, boolean | string>` —
// from an insertion-ordered array of `[key, value]` pairs (so `.get()`, `.size`,
// and iteration order all match pi's `Map`). pi's `Args` has no `T | null`
// fields: every optional it declares is absent-as-`undefined`, which the native
// side reproduces by omitting unset fields from the JSON object (reading them in
// JS yields `undefined`). The `Args` / `Mode` types and the `isValidThinkingLevel`
// helper are re-exported from the original unchanged.

export * from "./args.__pi_original__.ts";

import { parseArgsNative } from "pidgin-napi";
import type { Args } from "./args.__pi_original__.ts";

/** Wire shape of the JSON the native parser returns: pi's `Args` with
 * `unknownFlags` carried as an ordered `[key, value]` pair array. */
type NativeArgs = Omit<Args, "unknownFlags"> & {
	unknownFlags: Array<[string, boolean | string]>;
};

export function parseArgs(args: string[]): Args {
	const { unknownFlags, ...rest } = JSON.parse(parseArgsNative(args)) as NativeArgs;
	return {
		...rest,
		unknownFlags: new Map<string, boolean | string>(unknownFlags),
	};
}
