// Native shim for packages/coding-agent/src/core/session-cwd.ts, backed by the
// pidgin Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs when
// the module is marked `native` in conformance/manifest.json: the original pi
// file is preserved alongside as `session-cwd.__pi_original__.ts` and this shim
// takes its place, so pi's callers (`agent-session-runtime.ts`) and
// test/session-cwd.test.ts import `../src/core/session-cwd.ts` unchanged and hit
// Rust.
//
// Scope of the native flip: every ported symbol, backed by
// `pidgin_coding::core::session_cwd`. The one decision the module makes — is a
// resumed session's stored cwd non-empty and absent on disk (pi's `existsSync`)
// — runs in Rust, as does both format strings (`formatMissingSessionCwdError` /
// `formatMissingSessionCwdPrompt`). Nothing about the missing-cwd logic is
// reimplemented here.
//
// The flip boundary: `getMissingSessionCwdIssue` reads a structural
// `SessionCwdSource` (pi's `SessionManager`) — an object whose identity and live
// `getCwd()` / `getSessionFile()` methods are inherently JS-runtime and cannot
// cross the addon boundary. So this shim keeps ONLY that plumbing in TS: it calls
// those two methods JS-side and forwards the resulting strings to the native
// core, which owns the empty-cwd guard and the filesystem probe. Likewise
// `MissingSessionCwdError`'s class identity (`instanceof`, `.name`, the `.issue`
// field) is JS-inherent, so the shim keeps the class shell in TS and routes its
// message through the native formatter; `assertSessionCwdExists` is pi's trivial
// "detect then throw" over the native decision and the JS class. The
// `SessionCwdIssue` type is re-exported from the original unchanged.

export * from "./session-cwd.__pi_original__.ts";

import {
	formatMissingSessionCwdError as nativeFormatMissingSessionCwdError,
	formatMissingSessionCwdPrompt as nativeFormatMissingSessionCwdPrompt,
	getMissingSessionCwdIssue as nativeGetMissingSessionCwdIssue,
} from "pidgin-napi";
import type { SessionCwdIssue } from "./session-cwd.__pi_original__.ts";

/**
 * pi's structural `SessionCwdSource` (not exported by the original): the
 * read-only handle inspected for a missing cwd. pi's `SessionManager` satisfies
 * it. Declared here purely so the shim can read the two strings JS-side.
 */
interface SessionCwdSource {
	getCwd(): string;
	getSessionFile(): string | undefined;
}

/**
 * pi's `getMissingSessionCwdIssue`. Reads the source's session file and cwd
 * JS-side (the object's methods cannot cross the boundary), then lets the native
 * core decide: a persisted session whose stored cwd is non-empty and absent on
 * disk yields the issue; otherwise `undefined`.
 */
export function getMissingSessionCwdIssue(
	sessionManager: SessionCwdSource,
	fallbackCwd: string,
): SessionCwdIssue | undefined {
	const issue = nativeGetMissingSessionCwdIssue(
		sessionManager.getCwd(),
		sessionManager.getSessionFile() ?? undefined,
		fallbackCwd,
	);
	return issue ?? undefined;
}

/** pi's `formatMissingSessionCwdError`: the human-readable error text. */
export function formatMissingSessionCwdError(issue: SessionCwdIssue): string {
	return nativeFormatMissingSessionCwdError(issue);
}

/** pi's `formatMissingSessionCwdPrompt`: the interactive prompt text. */
export function formatMissingSessionCwdPrompt(issue: SessionCwdIssue): string {
	return nativeFormatMissingSessionCwdPrompt(issue);
}

/**
 * pi's `MissingSessionCwdError`. The class identity (`instanceof`, `.name`, the
 * `.issue` field) is JS-inherent and stays in TS; the message is formatted
 * natively so no text is reimplemented here.
 */
export class MissingSessionCwdError extends Error {
	readonly issue: SessionCwdIssue;

	constructor(issue: SessionCwdIssue) {
		super(nativeFormatMissingSessionCwdError(issue));
		this.name = "MissingSessionCwdError";
		this.issue = issue;
	}
}

/**
 * pi's `assertSessionCwdExists`: raise `MissingSessionCwdError` when the native
 * decision reports a missing stored cwd. Pi's trivial detect-then-throw over the
 * native issue and the JS class.
 */
export function assertSessionCwdExists(sessionManager: SessionCwdSource, fallbackCwd: string): void {
	const issue = getMissingSessionCwdIssue(sessionManager, fallbackCwd);
	if (issue) {
		throw new MissingSessionCwdError(issue);
	}
}
