// Shared JS driver loop for the OAuth conformance shims, backed by the atilla
// Rust addon (`atilla-napi`). The four provider shims (anthropic, openai-codex,
// github-copilot, xai) and the standalone device-code shim import from here so
// the near-identical Rust-machine driver lives in exactly one place instead of
// being copied into each `.ts` shim.
//
// The boundary is one-way (see crates/atilla-napi/src/oauth.rs): the Rust
// machine yields the next Step / DevicePollStep, this driver performs the effect
// (fetch / setTimeout sleep / prompt / notify) in JS so pi's `vi.stubGlobal`
// fetch and fake timers keep intercepting, then re-enters Rust with the result.
// `Date.now()` is passed on every start/advance so pi's `vi.setSystemTime`
// controls all expiry / deadline math.
//
// This file is plain CommonJS (the addon's generated `index.js` is CJS too); the
// `.ts` shims import its named exports through vite's CJS interop, resolving via
// the `vendor/pi/node_modules/atilla-napi -> crates/atilla-napi` symlink the
// conformance runner creates.

"use strict";

const { OAuthFlowCore, DeviceCodePollCore } = require("./index.js");

// A `setTimeout` sleep that also resolves early when `signal` aborts, so a
// caller that aborts mid-wait unblocks without the fake-timer clock advancing
// (pi's `abortableSleep`, device-code.ts:26-44). Resolves rather than rejects on
// abort: the driver then feeds the machine an `aborted` input so the Rust
// machine produces the canonical "Login cancelled" terminal error.
function abortableSleep(ms, signal) {
	return new Promise((resolve) => {
		if (signal && signal.aborted) {
			resolve();
			return;
		}
		const onAbort = () => {
			clearTimeout(timer);
			resolve();
		};
		const timer = setTimeout(() => {
			if (signal) signal.removeEventListener("abort", onAbort);
			resolve();
		}, ms);
		if (signal) signal.addEventListener("abort", onAbort, { once: true });
	});
}

// The Rust `HttpRequest` seam lowercases header keys by convention, but pi's
// code sets canonical HTTP casing (`Content-Type`, `Accept`, ...) and some of
// pi's tests assert the request-init headers with that exact casing
// (`toMatchObject({ "Content-Type": ... })`). Restore the canonical casing for
// the standard headers, leaving any provider-specific header key (e.g. GitHub
// Copilot's `Editor-Version` / `X-GitHub-Api-Version` / `x-interaction-type`,
// which the Rust machine already emits verbatim) untouched.
const CANONICAL_HEADER_KEYS = {
	accept: "Accept",
	authorization: "Authorization",
	"content-type": "Content-Type",
	"user-agent": "User-Agent",
};

function canonicalizeHeaders(headers) {
	const out = {};
	for (const key of Object.keys(headers)) {
		const canonical = CANONICAL_HEADER_KEYS[key.toLowerCase()];
		out[canonical || key] = headers[key];
	}
	return out;
}

// Perform a machine `Request` via the ambient `fetch` and shape the response
// back into the `StepInput::Response` wire form the machine consumes.
async function performRequest(request) {
	const init = { method: request.method, headers: canonicalizeHeaders(request.headers) };
	if (request.body !== null && request.body !== undefined) {
		init.body = request.body;
	}
	const response = await fetch(request.url, init);
	const headers = {};
	response.headers.forEach((value, key) => {
		headers[key] = value;
	});
	const body = await response.text();
	return { kind: "response", status: response.status, headers, body };
}

// Drive an OAuth login/refresh flow to a credential (or throw its error).
//
// `mode` is "login" or "refresh"; for "refresh", `credential` is serialized as
// the machine's starting credential. `interaction` is pi's `AuthInteraction`
// ({ signal?, prompt, notify }); refresh flows never prompt/notify.
async function driveOAuthFlow(provider, mode, credential, interaction) {
	const credentialJson = mode === "refresh" ? JSON.stringify(credential) : undefined;
	const core = new OAuthFlowCore(provider, mode, credentialJson);
	const signal = interaction && interaction.signal;
	// One synthesized AbortController per prompt: pi does not serialize the live
	// per-prompt signal, so the driver attaches a fresh one and aborts them all
	// once the flow settles (anthropic asserts `prompt.signal.aborted === true`).
	const promptControllers = [];
	try {
		let step = JSON.parse(core.start(Date.now()));
		while (true) {
			if (signal && signal.aborted && step.kind !== "done" && step.kind !== "error") {
				step = JSON.parse(core.advance(JSON.stringify({ kind: "aborted" }), Date.now()));
				continue;
			}
			switch (step.kind) {
				case "request": {
					const input = await performRequest(step.request);
					step = JSON.parse(core.advance(JSON.stringify(input), Date.now()));
					break;
				}
				case "wait": {
					await abortableSleep(step.delay_ms, signal);
					if (signal && signal.aborted) {
						step = JSON.parse(core.advance(JSON.stringify({ kind: "aborted" }), Date.now()));
						break;
					}
					const input = await performRequest(step.request);
					step = JSON.parse(core.advance(JSON.stringify(input), Date.now()));
					break;
				}
				case "prompt": {
					// pi attaches a live per-prompt AbortSignal only to the
					// `manual_code` prompt (the anthropic callback-server race), and
					// asserts it is aborted once login settles. Other prompt kinds
					// (`select`, `text`) carry no signal, and pi's tests assert their
					// exact shape (`toEqual`), so only synthesize a signal for
					// `manual_code` and pass every other prompt through verbatim.
					let prompt = step.prompt;
					if (prompt.type === "manual_code") {
						const controller = new AbortController();
						promptControllers.push(controller);
						prompt = Object.assign({}, prompt, { signal: controller.signal });
					}
					const value = await interaction.prompt(prompt);
					step = JSON.parse(core.advance(JSON.stringify({ kind: "input", value }), Date.now()));
					break;
				}
				case "notify": {
					interaction.notify(step.event);
					step = JSON.parse(core.advance(JSON.stringify({ kind: "ack" }), Date.now()));
					break;
				}
				case "done":
					return Object.assign({}, step.credential, { type: "oauth" });
				case "error":
					throw new Error(step.message);
				default:
					throw new Error(`Unknown OAuth flow step: ${step.kind}`);
			}
		}
	} finally {
		for (const controller of promptControllers) {
			controller.abort();
		}
	}
}

// Map pi's `OAuthDeviceCodePollResult` onto the machine's `DevicePollInput`
// (same `status` strings; `intervalSeconds` -> `interval_seconds`).
function mapPollResult(result) {
	switch (result.status) {
		case "pending":
			return { status: "pending" };
		case "slow_down":
			return result.intervalSeconds !== undefined && result.intervalSeconds !== null
				? { status: "slow_down", interval_seconds: result.intervalSeconds }
				: { status: "slow_down" };
		case "failed":
			return { status: "failed", message: result.message };
		case "complete":
			return { status: "complete", value: result.value };
		default:
			throw new Error(`Unknown poll result status: ${result.status}`);
	}
}

// Drive the standalone `pollOAuthDeviceCodeFlow(options)` device-code poll loop.
async function driveDevicePoll(options) {
	const core = new DeviceCodePollCore(
		JSON.stringify({
			intervalSeconds: options.intervalSeconds,
			expiresInSeconds: options.expiresInSeconds,
			waitBeforeFirstPoll: options.waitBeforeFirstPoll === true,
		}),
	);
	const signal = options.signal;

	async function pollAndAdvance() {
		if (signal && signal.aborted) {
			return JSON.parse(core.advance(JSON.stringify({ status: "aborted" }), Date.now()));
		}
		const result = await options.poll();
		return JSON.parse(core.advance(JSON.stringify(mapPollResult(result)), Date.now()));
	}

	let step = JSON.parse(core.start(Date.now()));
	while (true) {
		if (signal && signal.aborted && step.kind !== "done" && step.kind !== "error") {
			step = JSON.parse(core.advance(JSON.stringify({ status: "aborted" }), Date.now()));
			continue;
		}
		switch (step.kind) {
			case "poll":
				step = await pollAndAdvance();
				break;
			case "wait":
				await abortableSleep(step.delay_ms, signal);
				step = await pollAndAdvance();
				break;
			case "done":
				return step.value;
			case "error":
				throw new Error(step.message);
			default:
				throw new Error(`Unknown device poll step: ${step.kind}`);
		}
	}
}

module.exports = { driveOAuthFlow, driveDevicePoll };
