// Native shim for packages/agent/src/harness/session/jsonl-storage.ts, backed by
// the pidgin Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs
// when the module is marked `native` in conformance/manifest.json: the original
// pi file is preserved beside it as `jsonl-storage.__pi_original__.ts` and this
// shim takes its place, so pi's tests exercise the Rust-backed JSONL storage.
//
// JSONL parse/serialize/append/leaf-reconstruction runs in Rust
// (`JsonlSessionStorageCore`), reading and writing the session file directly on
// the host disk. pi's storage/repo tests inject a `NodeExecutionEnv` over that
// same real disk, so the injected `fs` carries no behaviour the native path
// needs — we route native whenever `fs` is our Rust-backed `NodeExecutionEnv`
// (detected via `nativeExecutionCore`), and delegate to pi's original for any
// other `fs` (e.g. the one storage.test.ts case that injects a bare async
// `FileSystem` object into `loadJsonlSessionMetadata` — the Rust port cannot
// call back into that JS-async closure, so pi drives it).
//
// Entries/metadata cross as JSON strings; fallible ops cross as an
// `{ok,value}` / `{ok,error}` envelope; `open`/`create` throw a `{code,message}`
// JSON reason. All are reshaped here into pi's `Result`/`SessionError`.

export * from "./jsonl-storage.__pi_original__.ts";

import {
	JsonlSessionStorage as PiJsonlSessionStorage,
	loadJsonlSessionMetadata as piLoadJsonlSessionMetadata,
} from "./jsonl-storage.__pi_original__.ts";
import { nativeExecutionCore } from "../env/nodejs.ts";
import { JsonlSessionStorageCore, loadJsonlSessionMetadataNative } from "pidgin-napi";
import {
	SessionError,
	type SessionErrorCode,
	type JsonlSessionMetadata,
	type SessionStorage,
	type SessionTreeEntry,
} from "../types.ts";

/** The `fs` surface pi's `JsonlSessionStorage` duck-types. Kept loose (the
 * concrete arg is either our `NodeExecutionEnv` or a bare injected object). */
type JsonlFs = Parameters<typeof PiJsonlSessionStorage.open>[0];

type JsonlCreateOptions = {
	cwd: string;
	sessionId: string;
	parentSessionPath?: string;
	metadata?: Record<string, unknown>;
};

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

/** Rebuild pi's `SessionError` from a thrown native `{code,message}` reason. */
function sessionErrorFromThrow(error: unknown): SessionError {
	const message = error instanceof Error ? error.message : String(error);
	try {
		const parsed = JSON.parse(message) as { code: SessionErrorCode; message: string };
		return new SessionError(parsed.code, parsed.message);
	} catch {
		return new SessionError("storage", message);
	}
}

/** A `SessionStorage` handle backed by a Rust `JsonlSessionStorageCore`. */
class NativeJsonlSessionStorage implements SessionStorage<JsonlSessionMetadata> {
	constructor(private core: JsonlSessionStorageCore) {}

	async getMetadata(): Promise<JsonlSessionMetadata> {
		return JSON.parse(this.core.getMetadata()) as JsonlSessionMetadata;
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

export class JsonlSessionStorage {
	static async open(fs: JsonlFs, filePath: string): Promise<SessionStorage<JsonlSessionMetadata>> {
		if (!nativeExecutionCore(fs)) return PiJsonlSessionStorage.open(fs, filePath);
		try {
			return new NativeJsonlSessionStorage(JsonlSessionStorageCore.open(filePath));
		} catch (error) {
			throw sessionErrorFromThrow(error);
		}
	}

	static async create(
		fs: JsonlFs,
		filePath: string,
		options: JsonlCreateOptions,
	): Promise<SessionStorage<JsonlSessionMetadata>> {
		if (!nativeExecutionCore(fs)) return PiJsonlSessionStorage.create(fs, filePath, options);
		try {
			const core = JsonlSessionStorageCore.create(
				filePath,
				JSON.stringify({
					cwd: options.cwd,
					sessionId: options.sessionId,
					parentSessionPath: options.parentSessionPath,
					metadata: options.metadata,
				}),
			);
			return new NativeJsonlSessionStorage(core);
		} catch (error) {
			throw sessionErrorFromThrow(error);
		}
	}
}

export async function loadJsonlSessionMetadata(
	fs: JsonlFs,
	filePath: string,
): Promise<JsonlSessionMetadata> {
	if (!nativeExecutionCore(fs)) return piLoadJsonlSessionMetadata(fs, filePath);
	try {
		return JSON.parse(loadJsonlSessionMetadataNative(filePath)) as JsonlSessionMetadata;
	} catch (error) {
		throw sessionErrorFromThrow(error);
	}
}
