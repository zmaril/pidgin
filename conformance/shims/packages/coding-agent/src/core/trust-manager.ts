// straitjacket-allow-file:duplication — the `ProjectTrustStore` class body below
// is a faithful mirror of pi's class shape (constructor + get/getEntry/set/
// setMany), because the Rust port exposes those as stateless functions over the
// agent dir; the shim rebuilds pi's thin class around them, so the structural
// overlap with the preserved pi original is intentional.
//
// Native shim for packages/coding-agent/src/core/trust-manager.ts, backed by the
// atilla Rust addon (`atilla-napi`). Installed by conformance/codegen.mjs when
// the module is marked `native` in conformance/manifest.json: the original pi
// file is preserved alongside as `trust-manager.__pi_original__.ts` and this shim
// takes its place, so pi's callers and test/trust-manager.test.ts import
// `../src/core/trust-manager.ts` unchanged and hit Rust.
//
// Scope of the native flip: every ported symbol, backed by
// `atilla_coding::core::trust_manager`. `getProjectTrustParentPath`,
// `getProjectTrustOptions`, and `hasTrustRequiringProjectResources` route to
// Rust (the last one takes an explicit home dir so the shim supplies pi's
// `process.env.HOME || homedir()` rather than mutating process-global env in
// Rust). `ProjectTrustStore` stays a JS class holding the agent dir; each method
// delegates to the stateless native functions (whose only state is the on-disk
// `trust.json`). Structured values cross as JSON in pi's exact shape. The types
// (`ProjectTrustDecision`, `ProjectTrustStoreEntry`, `ProjectTrustUpdate`,
// `ProjectTrustOption`) are re-exported unchanged from the original.
//
// NOTE: pi guards `trust.json` with a cross-process advisory lock
// (`proper-lockfile`); the Rust port omits it (it defends against concurrent pi
// processes, not any behavior pi's tests pin), so the shim does too.

export * from "./trust-manager.__pi_original__.ts";

import { homedir } from "node:os";
import {
	getProjectTrustOptions as nativeGetProjectTrustOptions,
	getProjectTrustParentPath as nativeGetProjectTrustParentPath,
	hasTrustRequiringProjectResources as nativeHasTrustRequiringProjectResources,
	trustStoreGetEntry as nativeTrustStoreGetEntry,
	trustStoreSetMany as nativeTrustStoreSetMany,
} from "atilla-napi";
import type {
	ProjectTrustDecision,
	ProjectTrustOption,
	ProjectTrustStoreEntry,
	ProjectTrustUpdate,
} from "./trust-manager.__pi_original__.ts";

export function getProjectTrustParentPath(cwd: string): string | undefined {
	const parent = nativeGetProjectTrustParentPath(cwd);
	return parent === null ? undefined : parent;
}

export function getProjectTrustOptions(
	cwd: string,
	options?: { includeSessionOnly?: boolean },
): ProjectTrustOption[] {
	return JSON.parse(nativeGetProjectTrustOptions(cwd, options?.includeSessionOnly ?? false)) as ProjectTrustOption[];
}

export function hasTrustRequiringProjectResources(cwd: string): boolean {
	return nativeHasTrustRequiringProjectResources(cwd, process.env.HOME || homedir());
}

export class ProjectTrustStore {
	private agentDir: string;

	constructor(agentDir: string) {
		this.agentDir = agentDir;
	}

	get(cwd: string): ProjectTrustDecision {
		return this.getEntry(cwd)?.decision ?? null;
	}

	getEntry(cwd: string): ProjectTrustStoreEntry | null {
		const entry = nativeTrustStoreGetEntry(this.agentDir, cwd);
		return entry === null ? null : (JSON.parse(entry) as ProjectTrustStoreEntry);
	}

	set(cwd: string, decision: ProjectTrustDecision): void {
		this.setMany([{ path: cwd, decision }]);
	}

	setMany(decisions: ProjectTrustUpdate[]): void {
		nativeTrustStoreSetMany(this.agentDir, JSON.stringify(decisions));
	}
}
