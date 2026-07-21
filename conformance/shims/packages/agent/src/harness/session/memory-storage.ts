// Native shim for packages/agent/src/harness/session/memory-storage.ts, backed
// by the pidgin Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs
// when the module is marked `native` in conformance/manifest.json: the original
// pi file is preserved beside it as `memory-storage.__pi_original__.ts` and this
// shim takes its place, so pi's tests exercise the Rust-backed
// `InMemorySessionStorage`.
//
// The in-memory store logic (entry map, label cache, leaf reconstruction, path
// walking) runs entirely in Rust (`InMemorySessionStorageCore`). Entries and
// metadata cross the boundary as JSON strings; fallible operations cross as an
// `{ok,value}` / `{ok,error}` envelope reshaped here into pi's `SessionError`.

export * from "./memory-storage.__pi_original__.ts";

import { InMemorySessionStorageCore } from "pidgin-napi";
import {
	SessionError,
	type SessionErrorCode,
	type SessionMetadata,
	type SessionStorage,
	type SessionTreeEntry,
} from "../types.ts";

/** The `{ok,value}` / `{ok,error}` envelope a fallible native op returns. */
type NativeResult<TValue> =
	| { ok: true; value: TValue }
	| { ok: false; error: { code: SessionErrorCode; message: string } };

/** Unwrap a native `{ok,value}` / `{ok,error}` envelope, throwing pi's
 * `SessionError` on the error branch. */
function unwrap<TValue>(json: string): TValue {
	const parsed = JSON.parse(json) as NativeResult<TValue>;
	if (parsed.ok) return parsed.value;
	throw new SessionError(parsed.error.code, parsed.error.message);
}

export class InMemorySessionStorage<TMetadata extends SessionMetadata = SessionMetadata>
	implements SessionStorage<TMetadata>
{
	private core: InMemorySessionStorageCore;

	constructor(options?: { entries?: SessionTreeEntry[]; metadata?: TMetadata }) {
		this.core = new InMemorySessionStorageCore(
			options?.entries ? JSON.stringify(options.entries) : undefined,
			options?.metadata ? JSON.stringify(options.metadata) : undefined,
		);
	}

	async getMetadata(): Promise<TMetadata> {
		return JSON.parse(this.core.getMetadata()) as TMetadata;
	}

	async getLeafId(): Promise<string | null> {
		return unwrap<string | null>(this.core.getLeafId());
	}

	async setLeafId(leafId: string | null): Promise<void> {
		unwrap<null>(this.core.setLeafId(leafId ?? undefined));
	}

	async createEntryId(): Promise<string> {
		return this.core.createEntryId();
	}

	async appendEntry(entry: SessionTreeEntry): Promise<void> {
		unwrap<null>(this.core.appendEntry(JSON.stringify(entry)));
	}

	async getEntry(id: string): Promise<SessionTreeEntry | undefined> {
		const json = this.core.getEntry(id);
		return json == null ? undefined : (JSON.parse(json) as SessionTreeEntry);
	}

	async findEntries<TType extends SessionTreeEntry["type"]>(
		type: TType,
	): Promise<Array<Extract<SessionTreeEntry, { type: TType }>>> {
		return JSON.parse(this.core.findEntries(type));
	}

	async getLabel(id: string): Promise<string | undefined> {
		return this.core.getLabel(id) ?? undefined;
	}

	async getPathToRoot(leafId: string | null): Promise<SessionTreeEntry[]> {
		return unwrap<SessionTreeEntry[]>(this.core.getPathToRoot(leafId ?? undefined));
	}

	async getEntries(): Promise<SessionTreeEntry[]> {
		return JSON.parse(this.core.getEntries());
	}
}
