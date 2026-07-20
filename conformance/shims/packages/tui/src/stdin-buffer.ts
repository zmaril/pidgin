// Native shim for packages/tui/src/stdin-buffer.ts, backed by the pidgin Rust
// addon (`pidgin-napi`). Installed by conformance/codegen.mjs when the module is
// marked `native` in conformance/manifest.json: the original pi file is preserved
// alongside as `stdin-buffer.__pi_original__.ts` and this shim takes its place, so
// pi's tests import `../src/stdin-buffer.ts` unchanged and hit Rust.
//
// Scope of the native flip: pi's entire `StdinBuffer` escape-sequence splitter is
// ported bit-exactly in `crates/pidgin-tui` (`terminal/stdin_buffer.rs`) and
// exposed as `StdinBufferCore`. Every decision the buffer makes runs in Rust:
// partial-escape reassembly, CSI/OSC/DCS/APC/SS3 completion, old-style + SGR
// mouse handling, the WezTerm double-ESC split, bracketed-paste extraction, and
// the Kitty printable-codepoint dedup. `process()` returns the ordered event list
// (`{ kind: "data" | "paste", value }`) the buffer produced for the chunk; the
// buffered incomplete remainder is read back with `getBuffer()`, flushed with
// `flush()`, and reset with `clear()`.
//
// The flip boundary: pi's public `StdinBuffer` is an `EventEmitter` that fires
// `"data"` / `"paste"` synchronously during `process()` and arms a 10ms completion
// timer. Those two concerns are inherently JS-runtime — an event target's
// identity and a `setTimeout` cannot cross the addon boundary — so this shim keeps
// ONLY that plumbing in TS: it SUBCLASSES pi's `EventEmitter` surface, converts
// any `Buffer` argument to a string (including pi's single high-byte → ESC+char
// rewrite), forwards the string to the native core, and replays the returned
// events onto the emitter, arming/clearing the same completion timer pi does. No
// splitting, paste, or dedup logic is reimplemented here; the buffer's entire
// cross-chunk state lives in the wrapped `StdinBufferCore`. `StdinBufferOptions`
// and `StdinBufferEventMap` types are re-exported from the original unchanged.

export * from "./stdin-buffer.__pi_original__.ts";

import { EventEmitter } from "events";
import { StdinBufferCore } from "pidgin-napi";
import type { StdinBufferEventMap, StdinBufferOptions } from "./stdin-buffer.__pi_original__.ts";

/**
 * Native `StdinBuffer`: pi's `EventEmitter` surface and completion timer over the
 * Rust `StdinBufferCore` splitter. See the file header for the flip boundary. The
 * class keeps pi's exact public API (`process`, `flush`, `clear`, `getBuffer`,
 * `destroy`, and the `"data"` / `"paste"` events); all splitting is delegated.
 */
export class StdinBuffer extends EventEmitter<StdinBufferEventMap> {
	private core: StdinBufferCore;
	private timeout: ReturnType<typeof setTimeout> | null = null;
	private readonly timeoutMs: number;

	constructor(options: StdinBufferOptions = {}) {
		super();
		this.timeoutMs = options.timeout ?? 10;
		this.core = new StdinBufferCore(this.timeoutMs);
	}

	public process(data: string | Buffer): void {
		// Clear any pending timeout (a fresh chunk resets the completion window).
		if (this.timeout) {
			clearTimeout(this.timeout);
			this.timeout = null;
		}

		// Handle high-byte conversion (for compatibility with parseKeypress):
		// a single byte > 127 becomes ESC + (byte - 128). This is the only
		// Buffer-adaptation the string-based native core cannot see.
		let str: string;
		if (Buffer.isBuffer(data)) {
			if (data.length === 1 && data[0]! > 127) {
				const byte = data[0]! - 128;
				str = `\x1b${String.fromCharCode(byte)}`;
			} else {
				str = data.toString();
			}
		} else {
			str = data;
		}

		// Native core owns the whole splitting/paste/dedup state machine and
		// returns the ordered events this chunk produced.
		const events = this.core.process(str);
		for (const event of events) {
			this.emit(event.kind as "data" | "paste", event.value);
		}

		// pi arms a completion timer whenever an incomplete remainder is pending;
		// on fire it flushes the remainder as a single "data" event.
		if (this.core.getBuffer().length > 0) {
			this.timeout = setTimeout(() => {
				this.timeout = null;
				for (const sequence of this.core.flush()) {
					this.emit("data", sequence);
				}
			}, this.timeoutMs);
		}
	}

	flush(): string[] {
		if (this.timeout) {
			clearTimeout(this.timeout);
			this.timeout = null;
		}
		return this.core.flush();
	}

	clear(): void {
		if (this.timeout) {
			clearTimeout(this.timeout);
			this.timeout = null;
		}
		this.core.clear();
	}

	getBuffer(): string {
		return this.core.getBuffer();
	}

	destroy(): void {
		this.clear();
	}
}
