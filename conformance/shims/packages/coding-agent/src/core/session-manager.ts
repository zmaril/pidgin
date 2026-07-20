// Native shim for packages/coding-agent/src/core/session-manager.ts, backed by
// the pidgin Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs
// when the module is marked `native` in conformance/manifest.json: pi's original
// file is preserved beside this shim as `session-manager.__pi_original__.ts` and
// this shim takes its place, so pi's tests (and every coding-agent importer)
// resolve `../src/core/session-manager.ts` unchanged and hit Rust.
//
// Scope of the native flip: the whole canonical CLI `SessionManager` — the
// stateful class (create / open / list / append / tree traversal / rewrite) and
// the module free functions the suite exercises (`migrateSessionEntries`,
// `buildContextEntries`, `buildSessionContext`, `findMostRecentSession`,
// `loadEntriesFromFile`) — runs in the Rust port
// (`pidgin_coding::core::session_manager`, PR #101, CLI-canonical). Delegation
// is genuine: create/open/list/append/tree/rewrite logic all executes in Rust.
//
// The module's un-flipped surface — the entry TYPES, `CURRENT_SESSION_VERSION`,
// `assertValidSessionId`, `parseSessionEntries`, `getLatestCompactionEntry`,
// `sessionEntryToContextMessages`, `getDefaultSessionDir`, `SessionListProgress`
// — is re-exported unchanged from pi's preserved original below; the explicit
// named exports in this file shadow the star-exported originals of the same
// name (ESM local-export precedence), so only the ported symbols are swapped.
//
// The boundary is JSON: napi's `.d.ts` can't express pi's discriminated-union
// entry types, so entries / headers / contexts / trees / session-info records
// cross as JSON strings (parsed here). pi `T | null` arguments are coerced to
// `undefined` before the call so the napi `Option<String>` boundary never sees a
// JS `null` (which it would reject).

export * from "./session-manager.__pi_original__.ts";

import type { AgentMessage } from "@earendil-works/pi-agent-core";
import type { ImageContent, Message, TextContent } from "@earendil-works/pi-ai";
import {
	buildContextEntries as nativeBuildContextEntries,
	buildSessionContext as nativeBuildSessionContext,
	findMostRecentSession as nativeFindMostRecentSession,
	loadEntriesFromFile as nativeLoadEntriesFromFile,
	migrateSessionEntries as nativeMigrateSessionEntries,
	SessionManagerCore,
	sessionManagerList as nativeSessionManagerList,
	sessionManagerListAll as nativeSessionManagerListAll,
} from "pidgin-napi";
import type { BashExecutionMessage, CustomMessage } from "./messages.ts";
import type {
	FileEntry,
	NewSessionOptions,
	SessionContext,
	SessionEntry,
	SessionHeader,
	SessionInfo,
	SessionListProgress,
	SessionTreeNode,
} from "./session-manager.__pi_original__.ts";

// --- module free functions --------------------------------------------------

/** `migrateSessionEntries` — Rust migrates a copy; splice it back so pi's
 * mutate-the-array-in-place contract holds for callers holding the reference. */
export function migrateSessionEntries(entries: FileEntry[]): void {
	const migrated = JSON.parse(nativeMigrateSessionEntries(JSON.stringify(entries))) as FileEntry[];
	entries.splice(0, entries.length, ...migrated);
}

export function buildContextEntries(
	entries: SessionEntry[],
	leafId?: string | null,
	// The `byId` cache is a pure optimization; the Rust port rebuilds the index.
	_byId?: Map<string, SessionEntry>,
): SessionEntry[] {
	return JSON.parse(
		nativeBuildContextEntries(JSON.stringify(entries), leafId ?? undefined),
	) as SessionEntry[];
}

export function buildSessionContext(
	entries: SessionEntry[],
	leafId?: string | null,
	_byId?: Map<string, SessionEntry>,
): SessionContext {
	return JSON.parse(
		nativeBuildSessionContext(JSON.stringify(entries), leafId ?? undefined),
	) as SessionContext;
}

export function findMostRecentSession(sessionDir: string, cwd?: string): string | null {
	return nativeFindMostRecentSession(sessionDir, cwd ?? undefined) ?? null;
}

export function loadEntriesFromFile(filePath: string): FileEntry[] {
	return JSON.parse(nativeLoadEntriesFromFile(filePath)) as FileEntry[];
}

/** Rehydrate a Rust `SessionInfo` JSON record: `created`/`modified` cross as ISO
 * strings and become `Date` here, matching pi's `SessionInfo` shape. */
function reviveSessionInfo(raw: Record<string, unknown>): SessionInfo {
	return {
		...raw,
		created: new Date(raw.created as string),
		modified: new Date(raw.modified as string),
	} as unknown as SessionInfo;
}

// --- the SessionManager class -----------------------------------------------

/**
 * Native `SessionManager`: a thin façade over the Rust-backed
 * `SessionManagerCore`. Every method delegates to Rust; complex values cross the
 * boundary as JSON. Construction goes through the static factories (the Rust
 * core is created there), mirroring pi's private constructor.
 */
export class SessionManager {
	private core: SessionManagerCore;

	private constructor(core: SessionManagerCore) {
		this.core = core;
	}

	// --- static factories ---------------------------------------------------

	static create(cwd: string, sessionDir?: string, options?: NewSessionOptions): SessionManager {
		return new SessionManager(
			SessionManagerCore.create(
				cwd,
				sessionDir ?? undefined,
				options ? JSON.stringify(options) : undefined,
			),
		);
	}

	static open(path: string, sessionDir?: string, cwdOverride?: string): SessionManager {
		return new SessionManager(
			SessionManagerCore.open(path, sessionDir ?? undefined, cwdOverride ?? undefined),
		);
	}

	static continueRecent(cwd: string, sessionDir?: string): SessionManager {
		return new SessionManager(SessionManagerCore.continueRecent(cwd, sessionDir ?? undefined));
	}

	static inMemory(cwd: string = process.cwd(), options?: NewSessionOptions): SessionManager {
		return new SessionManager(
			SessionManagerCore.inMemory(cwd, options ? JSON.stringify(options) : undefined),
		);
	}

	static forkFrom(
		sourcePath: string,
		targetCwd: string,
		sessionDir?: string,
		options?: NewSessionOptions,
	): SessionManager {
		return new SessionManager(
			SessionManagerCore.forkFrom(
				sourcePath,
				targetCwd,
				sessionDir ?? undefined,
				options ? JSON.stringify(options) : undefined,
			),
		);
	}

	static async list(
		cwd: string,
		sessionDir?: string,
		onProgress?: SessionListProgress,
	): Promise<SessionInfo[]> {
		void onProgress;
		const infos = JSON.parse(
			nativeSessionManagerList(cwd, sessionDir ?? undefined),
		) as Record<string, unknown>[];
		return infos.map(reviveSessionInfo);
	}

	static async listAll(
		sessionDirOrOnProgress?: string | SessionListProgress,
		onProgress?: SessionListProgress,
	): Promise<SessionInfo[]> {
		void onProgress;
		const sessionDir =
			typeof sessionDirOrOnProgress === "string" ? sessionDirOrOnProgress : undefined;
		const infos = JSON.parse(
			nativeSessionManagerListAll(sessionDir ?? undefined),
		) as Record<string, unknown>[];
		return infos.map(reviveSessionInfo);
	}

	// --- session lifecycle --------------------------------------------------

	newSession(options?: NewSessionOptions): string | undefined {
		return this.core.newSession(options ? JSON.stringify(options) : undefined) ?? undefined;
	}

	// --- accessors ----------------------------------------------------------

	isPersisted(): boolean {
		return this.core.isPersisted();
	}

	getCwd(): string {
		return this.core.getCwd();
	}

	getSessionDir(): string {
		return this.core.getSessionDir();
	}

	usesDefaultSessionDir(): boolean {
		return this.core.usesDefaultSessionDir();
	}

	getSessionId(): string {
		return this.core.getSessionId();
	}

	getSessionFile(): string | undefined {
		return this.core.getSessionFile() ?? undefined;
	}

	getLeafId(): string | null {
		return this.core.getLeafId() ?? null;
	}

	getHeader(): SessionHeader | null {
		const raw = this.core.getHeader();
		return raw == null ? null : (JSON.parse(raw) as SessionHeader);
	}

	getSessionName(): string | undefined {
		return this.core.getSessionName() ?? undefined;
	}

	// --- append operations --------------------------------------------------

	appendMessage(message: Message | CustomMessage | BashExecutionMessage): string {
		return this.core.appendMessage(JSON.stringify(message));
	}

	appendThinkingLevelChange(thinkingLevel: string): string {
		return this.core.appendThinkingLevelChange(thinkingLevel);
	}

	appendModelChange(provider: string, modelId: string): string {
		return this.core.appendModelChange(provider, modelId);
	}

	appendCompaction<T = unknown>(
		summary: string,
		firstKeptEntryId: string,
		tokensBefore: number,
		details?: T,
		fromHook?: boolean,
	): string {
		return this.core.appendCompaction(
			summary,
			firstKeptEntryId,
			tokensBefore,
			details === undefined ? undefined : JSON.stringify(details),
			fromHook ?? undefined,
		);
	}

	appendCustomEntry(customType: string, data?: unknown): string {
		return this.core.appendCustomEntry(
			customType,
			data === undefined ? undefined : JSON.stringify(data),
		);
	}

	appendSessionInfo(name: string): string {
		return this.core.appendSessionInfo(name);
	}

	appendCustomMessageEntry<T = unknown>(
		customType: string,
		content: string | (TextContent | ImageContent)[],
		display: boolean,
		details?: T,
	): string {
		return this.core.appendCustomMessageEntry(
			customType,
			JSON.stringify(content),
			display,
			details === undefined ? undefined : JSON.stringify(details),
		);
	}

	appendLabelChange(targetId: string, label: string | undefined): string {
		return this.core.appendLabelChange(targetId, label ?? undefined);
	}

	// --- tree navigation ----------------------------------------------------

	getLeafEntry(): SessionEntry | undefined {
		const raw = this.core.getLeafEntry();
		return raw == null ? undefined : (JSON.parse(raw) as SessionEntry);
	}

	getEntry(id: string): SessionEntry | undefined {
		const raw = this.core.getEntry(id);
		return raw == null ? undefined : (JSON.parse(raw) as SessionEntry);
	}

	getChildren(parentId: string): SessionEntry[] {
		return JSON.parse(this.core.getChildren(parentId)) as SessionEntry[];
	}

	getLabel(id: string): string | undefined {
		return this.core.getLabel(id) ?? undefined;
	}

	getBranch(fromId?: string): SessionEntry[] {
		return JSON.parse(this.core.getBranch(fromId ?? undefined)) as SessionEntry[];
	}

	buildContextEntries(): SessionEntry[] {
		return JSON.parse(this.core.buildContextEntries()) as SessionEntry[];
	}

	buildSessionContext(): SessionContext {
		return JSON.parse(this.core.buildSessionContext()) as SessionContext;
	}

	getEntries(): SessionEntry[] {
		return JSON.parse(this.core.getEntries()) as SessionEntry[];
	}

	getTree(): SessionTreeNode[] {
		return JSON.parse(this.core.getTree()) as SessionTreeNode[];
	}

	// --- branching ----------------------------------------------------------

	branch(branchFromId: string): void {
		this.core.branch(branchFromId);
	}

	resetLeaf(): void {
		this.core.resetLeaf();
	}

	branchWithSummary(
		branchFromId: string | null,
		summary: string,
		details?: unknown,
		fromHook?: boolean,
	): string {
		return this.core.branchWithSummary(
			branchFromId ?? undefined,
			summary,
			details === undefined ? undefined : JSON.stringify(details),
			fromHook ?? undefined,
		);
	}

	createBranchedSession(leafId: string): string | undefined {
		return this.core.createBranchedSession(leafId) ?? undefined;
	}
}
