// straitjacket-allow-file:duplication — the three tool shims (ls/write/bash)
// share pi's native-flip overlay shape (`export *` the original, rebuild the
// tool factory, route the default path to the addon, delegate to the original
// when a custom `operations` backend is injected); the structural overlap
// mirrors pi's own parallel tool factories and is intentional/load-bearing.
//
// Native shim for packages/coding-agent/src/core/tools/ls.ts, backed by the
// pidgin Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs when
// the module is marked `native` in conformance/manifest.json: pi's original file
// is preserved alongside as `ls.__pi_original__.ts` and this shim takes its
// place, so pi's tools (and every importer that builds the default tool set)
// hit the Rust `run_ls` port.
//
// Scope of the native flip (FULL): the ls tool's `execute` runs through
// `pidgin_coding::core::tools::ls::run_ls` with the local-filesystem operations
// (resolve → exists → stat → readdir → case-insensitive sort → per-entry `/`
// suffix → entry cap → byte truncation → notices). The addon returns pi's
// `AgentToolResult` JSON (`{ content: [{ type: "text", text }], details? }`),
// with `details` omitted when empty so `result.details === undefined`; errors
// (`Path not found`, `Not a directory`, `Operation aborted`) cross as thrown JS
// `Error`s with pi's exact messages, matching pi's `reject(...)`.
//
// pi's `ls.ts` exposes an injectable `operations` seam (for a future SSH-backed
// listing), but NO pi test injects it. For symmetry and cross-suite safety this
// shim still delegates to pi's original when `options.operations` is supplied;
// the default (local) path — the only one any suite exercises — is native.
//
// The TUI `renderCall`/`renderResult` hooks and all type/interface exports come
// straight from the preserved original via `export *` and the spread below.

export * from "./ls.__pi_original__.ts";

import type { AgentTool } from "@earendil-works/pi-agent-core";
import { lsToolExecute } from "pidgin-napi";
import type { ToolDefinition } from "../extensions/types.ts";
import {
	createLsToolDefinition as originalCreateLsToolDefinition,
	type LsToolDetails,
	type LsToolOptions,
} from "./ls.__pi_original__.ts";
import { wrapToolDefinition } from "./tool-definition-wrapper.ts";

export function createLsToolDefinition(
	cwd: string,
	options?: LsToolOptions,
): ToolDefinition<any, LsToolDetails | undefined> {
	const definition = originalCreateLsToolDefinition(cwd, options);

	// A custom operations backend (unused by any pi test) keeps pi's TS execute.
	if (options?.operations) {
		return definition;
	}

	return {
		...definition,
		async execute(
			_toolCallId: string,
			{ path, limit }: { path?: string; limit?: number },
			signal?: AbortSignal,
			_onUpdate?: unknown,
			_ctx?: unknown,
		) {
			// pi's fast path: reject before doing any work if already aborted.
			if (signal?.aborted) {
				throw new Error("Operation aborted");
			}
			const resultJson = lsToolExecute(cwd, JSON.stringify({ path, limit }));
			return JSON.parse(resultJson);
		},
	};
}

export function createLsTool(cwd: string, options?: LsToolOptions): AgentTool<any> {
	return wrapToolDefinition(createLsToolDefinition(cwd, options));
}
