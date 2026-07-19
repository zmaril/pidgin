// Native shim for packages/coding-agent/src/core/tools/write.ts, backed by the
// atilla Rust addon (`atilla-napi`). Installed by conformance/codegen.mjs when
// the module is marked `native` in conformance/manifest.json: pi's original file
// is preserved alongside as `write.__pi_original__.ts` and this shim takes its
// place.
//
// Scope of the native flip (HYBRID / default-path only): the write tool's
// DEFAULT (local-filesystem) `execute` runs through
// `atilla_coding::core::tools::write::run_write` with the local operations and
// the native file-mutation queue (`mkdir` → `write_file`, abort re-checked after
// each await). The addon returns pi's `Successfully wrote N bytes to <path>`
// result with `details: undefined`; errors cross as thrown JS `Error`s.
//
// The injected-`operations` cases stay TS-backed. pi's file-mutation-queue tests
// (`test/file-mutation-queue.test.ts`) inject a custom `operations` backend with
// a caller-owned barrier to exercise the queue lock across a suspended,
// then-aborted write (`keeps write queue locked while an aborted write is still
// in flight`) and the shared edit/write queue. Reproducing that faithfully would
// require the codebase's first `ThreadsafeFunction` (a JS `write_file` Promise
// awaited inside the async native lock), which the house rule (faux.rs)
// deliberately avoids — so when `options.operations` is supplied this shim
// DELEGATES to pi's original TypeScript. Full-native injected write is a
// follow-up once a TSFN seam lands (see PR #114). Per the coordinator ruling
// this is a hybrid flip.

export * from "./write.__pi_original__.ts";

import type { AgentTool } from "@earendil-works/pi-agent-core";
import { writeToolExecute } from "atilla-napi";
import type { ToolDefinition } from "../extensions/types.ts";
import {
	createWriteToolDefinition as originalCreateWriteToolDefinition,
	type WriteToolOptions,
} from "./write.__pi_original__.ts";
import { wrapToolDefinition } from "./tool-definition-wrapper.ts";

export function createWriteToolDefinition(
	cwd: string,
	options?: WriteToolOptions,
): ToolDefinition<any, undefined> {
	const definition = originalCreateWriteToolDefinition(cwd, options);

	// Injected operations (pi's mutation-queue barrier/abort tests) keep pi's TS
	// execute so the JS-side barrier and the async lock interleaving are faithful.
	if (options?.operations) {
		return definition;
	}

	return {
		...definition,
		async execute(
			_toolCallId: string,
			{ path, content }: { path: string; content: string },
			signal?: AbortSignal,
			_onUpdate?: unknown,
			_ctx?: unknown,
		) {
			// pi checks `signal.aborted` after each await inside the queue; the
			// native run does the same. Mirror the pre-work fast path here.
			if (signal?.aborted) {
				throw new Error("Operation aborted");
			}
			const resultJson = writeToolExecute(cwd, JSON.stringify({ path, content }));
			return JSON.parse(resultJson);
		},
	};
}

export function createWriteTool(cwd: string, options?: WriteToolOptions): AgentTool<any> {
	return wrapToolDefinition(createWriteToolDefinition(cwd, options));
}
