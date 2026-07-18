// Native shim for packages/agent/src/harness/env/nodejs.ts, backed by the
// atilla Rust addon (`atilla-napi`). Installed by conformance/codegen.mjs when
// the module is marked `native` in conformance/manifest.json: the original pi
// file is preserved alongside as `nodejs.__pi_original__.ts` and this shim takes
// its place, so pi's tests hit the Rust-backed `NodeExecutionEnv`.
//
// Hybrid routing. The Rust `NodeExecutionEnv` port is synchronous and drops
// pi's `AbortSignal`, streaming stdout/stderr callbacks, and callback-error
// surface. So this shim composes TWO backends behind pi's async `ExecutionEnv`
// surface: a native `NodeExecutionEnvCore` handle for the 15 clean cases, and a
// private pi-original `NodeExecutionEnv` for the 5 cases that need pi's async
// behaviour:
//   - `exec` with `onStdout`/`onStderr` (streaming) or `abortSignal` (abort) or
//     the callback-error path  -> pi-original.
//   - cancellable file ops (`readTextFile`/`readTextLines`/`readBinaryFile`/
//     `writeFile`/`listDir`) called with an `AbortSignal`  -> pi-original.
//   - everything else  -> native.
// Rich results cross as JSON strings (`{ok,value}` / `{ok,error}`) and are
// reshaped here into pi's `Result`/`FileError`/`ExecutionError`; raw bytes from
// `readBinaryFile` cross as a `Buffer` (never through a Rust `String`).

export * from "./nodejs.__pi_original__.ts";

import { NodeExecutionEnv as PiNodeExecutionEnv } from "./nodejs.__pi_original__.ts";
import { NodeExecutionEnvCore } from "atilla-napi";
import {
	type ExecutionEnv,
	ExecutionError,
	type ExecutionErrorCode,
	err,
	FileError,
	type FileErrorCode,
	type FileInfo,
	ok,
	type Result,
} from "../types.ts";

/** The `{ok,value}` / `{ok,error}` JSON shape the native handle returns. */
type NativeFileResult<TValue> =
	| { ok: true; value: TValue }
	| { ok: false; error: { code: FileErrorCode; message: string; path?: string } };

type NativeExecResult =
	| { ok: true; value: { stdout: string; stderr: string; exitCode: number } }
	| { ok: false; error: { code: ExecutionErrorCode; message: string } };

/** Rebuild a `Result<TValue, FileError>` from a native crossing. */
function toFileResult<TValue>(json: string): Result<TValue, FileError> {
	const parsed = JSON.parse(json) as NativeFileResult<TValue>;
	if (parsed.ok) return ok(parsed.value);
	return err(new FileError(parsed.error.code, parsed.error.message, parsed.error.path));
}

/** Rebuild a `FileError` from a thrown native `readBinaryFile` error. */
function fileErrorFromThrow(error: unknown): FileError {
	const message = error instanceof Error ? error.message : String(error);
	const parsed = JSON.parse(message) as { code: FileErrorCode; message: string; path?: string };
	return new FileError(parsed.code, parsed.message, parsed.path);
}

export class NodeExecutionEnv implements ExecutionEnv {
	cwd: string;
	private core: NodeExecutionEnvCore;
	private pi: PiNodeExecutionEnv;

	constructor(options: { cwd: string; shellPath?: string; shellEnv?: NodeJS.ProcessEnv }) {
		this.cwd = options.cwd;
		this.core = new NodeExecutionEnvCore(
			options.cwd,
			options.shellPath ?? undefined,
			options.shellEnv ? JSON.stringify(options.shellEnv) : undefined,
		);
		this.pi = new PiNodeExecutionEnv(options);
	}

	async absolutePath(path: string): Promise<Result<string, FileError>> {
		return toFileResult<string>(this.core.absolutePath(path));
	}

	async joinPath(parts: string[]): Promise<Result<string, FileError>> {
		return toFileResult<string>(this.core.joinPath(parts));
	}

	async exec(
		command: string,
		options?: {
			cwd?: string;
			env?: Record<string, string>;
			timeout?: number;
			abortSignal?: AbortSignal;
			onStdout?: (chunk: string) => void;
			onStderr?: (chunk: string) => void;
		},
	): Promise<Result<{ stdout: string; stderr: string; exitCode: number }, ExecutionError>> {
		if (options?.onStdout || options?.onStderr || options?.abortSignal) {
			return this.pi.exec(command, options);
		}
		const nativeOptions = JSON.stringify({
			cwd: options?.cwd,
			env: options?.env,
			timeout: options?.timeout,
		});
		const parsed = JSON.parse(this.core.exec(command, nativeOptions)) as NativeExecResult;
		if (parsed.ok) return ok(parsed.value);
		return err(new ExecutionError(parsed.error.code, parsed.error.message));
	}

	async readTextFile(path: string, abortSignal?: AbortSignal): Promise<Result<string, FileError>> {
		if (abortSignal) return this.pi.readTextFile(path, abortSignal);
		return toFileResult<string>(this.core.readTextFile(path));
	}

	async readTextLines(
		path: string,
		options?: { maxLines?: number; abortSignal?: AbortSignal },
	): Promise<Result<string[], FileError>> {
		if (options?.abortSignal) return this.pi.readTextLines(path, options);
		return toFileResult<string[]>(this.core.readTextLines(path, options?.maxLines ?? -1));
	}

	async readBinaryFile(path: string, abortSignal?: AbortSignal): Promise<Result<Uint8Array, FileError>> {
		if (abortSignal) return this.pi.readBinaryFile(path, abortSignal);
		try {
			return ok(this.core.readBinaryFile(path));
		} catch (error) {
			return err(fileErrorFromThrow(error));
		}
	}

	async writeFile(
		path: string,
		content: string | Uint8Array,
		abortSignal?: AbortSignal,
	): Promise<Result<void, FileError>> {
		if (abortSignal || typeof content !== "string") return this.pi.writeFile(path, content, abortSignal);
		return toFileResult<void>(this.core.writeFile(path, content));
	}

	async appendFile(path: string, content: string | Uint8Array): Promise<Result<void, FileError>> {
		if (typeof content !== "string") return this.pi.appendFile(path, content);
		return toFileResult<void>(this.core.appendFile(path, content));
	}

	async fileInfo(path: string): Promise<Result<FileInfo, FileError>> {
		return toFileResult<FileInfo>(this.core.fileInfo(path));
	}

	async listDir(path: string, abortSignal?: AbortSignal): Promise<Result<FileInfo[], FileError>> {
		if (abortSignal) return this.pi.listDir(path, abortSignal);
		return toFileResult<FileInfo[]>(this.core.listDir(path));
	}

	async canonicalPath(path: string): Promise<Result<string, FileError>> {
		return toFileResult<string>(this.core.canonicalPath(path));
	}

	async exists(path: string): Promise<Result<boolean, FileError>> {
		return toFileResult<boolean>(this.core.exists(path));
	}

	async createDir(path: string, options?: { recursive?: boolean }): Promise<Result<void, FileError>> {
		return toFileResult<void>(this.core.createDir(path, options?.recursive ?? true));
	}

	async remove(path: string, options?: { recursive?: boolean; force?: boolean }): Promise<Result<void, FileError>> {
		return toFileResult<void>(this.core.remove(path, options?.recursive ?? false, options?.force ?? false));
	}

	async createTempDir(prefix: string = "tmp-"): Promise<Result<string, FileError>> {
		return toFileResult<string>(this.core.createTempDir(prefix));
	}

	async createTempFile(options?: { prefix?: string; suffix?: string }): Promise<Result<string, FileError>> {
		return toFileResult<string>(this.core.createTempFile(options?.prefix ?? "", options?.suffix ?? ""));
	}

	async cleanup(): Promise<void> {
		// Best-effort; the native host env has nothing to release.
	}
}
