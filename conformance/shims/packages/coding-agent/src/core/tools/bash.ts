//
// Native shim for packages/coding-agent/src/core/tools/bash.ts, backed by the
// pidgin Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs when
// the module is marked `native` in conformance/manifest.json: pi's original file
// is preserved alongside as `bash.__pi_original__.ts` and this shim takes its
// place.
//
// Scope of the native flip (HYBRID / default-path only): the bash tool's DEFAULT
// (local-shell) `execute` runs through `pidgin_coding::core::tools::bash`'s
// `BashTool` with the local-shell operations, streaming into the
// truncation/temp-file layer. The addon returns pi's `{ content, details:
// { truncation?, fullOutputPath? } }`; non-zero exit / timeout / abort cross as
// thrown JS `Error`s with pi's exact tail message.
//
// The injected-`operations` cases stay TS-backed. pi's `tools.test.ts` injects a
// custom `operations.exec` with chunk-boundary-sensitive assertions (split-UTF-8
// decode, 5000-call coalescing to <25 updates, trailing-newline line counting,
// timeout/abort after 3000 chunks). A batch native result cannot reproduce those
// streaming semantics; a faithful crossing would need a bidirectional
// `ThreadsafeFunction` (Rust→JS `exec`, JS→Rust `on_data`), the exact seam the
// house rule (faux.rs) avoids. So this shim DELEGATES to pi's original whenever a
// custom `operations`, `commandPrefix`, `shellPath`, or `spawnHook` is supplied;
// only the bare default path is native. Full-native injected bash is a follow-up
// once a TSFN seam lands (see the ThreadsafeFunction PR cohort). Per the
// coordinator ruling this is a hybrid flip.

export * from "./bash.__pi_original__.ts";

import type { AgentTool } from "@earendil-works/pi-agent-core";
import { bashToolExecute } from "pidgin-napi";
import type { ToolDefinition } from "../extensions/types.ts";
import {
	type BashToolDetails,
	type BashToolOptions,
	createBashToolDefinition as originalCreateBashToolDefinition,
} from "./bash.__pi_original__.ts";
import { wrapToolDefinition } from "./tool-definition-wrapper.ts";

export function createBashToolDefinition(
	cwd: string,
	options?: BashToolOptions,
): ToolDefinition<any, BashToolDetails | undefined> {
	const definition = originalCreateBashToolDefinition(cwd, options);

	// Anything beyond the bare local-shell default (injected operations, a command
	// prefix, an explicit shell path, or a spawn hook) keeps pi's TS execute so
	// the streaming/backend/shell-resolution behavior stays faithful.
	if (options?.operations || options?.commandPrefix || options?.shellPath || options?.spawnHook) {
		return definition;
	}

	return {
		...definition,
		async execute(
			_toolCallId: string,
			{ command, timeout }: { command: string; timeout?: number },
			signal?: AbortSignal,
			_onUpdate?: unknown,
			_ctx?: unknown,
		) {
			if (signal?.aborted) {
				throw new Error("Command aborted");
			}
			const resultJson = bashToolExecute(cwd, JSON.stringify({ command, timeout }));
			return JSON.parse(resultJson);
		},
	};
}

export function createBashTool(cwd: string, options?: BashToolOptions): AgentTool<any> {
	return wrapToolDefinition(createBashToolDefinition(cwd, options));
}
