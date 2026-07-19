// Native shim for packages/ai/src/auth/oauth/anthropic.ts, backed by the atilla
// Rust addon (`atilla-napi`). Installed by conformance/codegen.mjs when the
// module is marked `native` in conformance/manifest.json: the original pi file is
// preserved alongside as `anthropic.__pi_original__.ts` and this shim takes its
// place, so pi's tests import `../src/auth/oauth/anthropic.ts` unchanged and hit
// Rust.
//
// Scope of the native flip: the multi-step `login` / `refresh` OAuth flows, ported
// to the Rust `OAuthFlowMachine` (`atilla_ai::auth::oauth`). The Rust machine is
// the single source of truth for the flow logic (PKCE, the manual_code redirect
// parse, the token exchange, expiry math); the shared `driveOAuthFlow` helper
// performs the effects (fetch / prompt / notify) in JS so pi's `vi.stubGlobal`
// fetch and system-time control still apply. Everything else the module exports
// (the `name`/`toAuth` surface and any constants) is re-exported from the original
// unchanged.

export * from "./anthropic.__pi_original__.ts";

import { anthropicOAuth as piAnthropicOAuth } from "./anthropic.__pi_original__.ts";
import type { AuthInteraction, OAuthAuth, OAuthCredential } from "../types.ts";
import { driveOAuthFlow } from "atilla-napi/oauth-flow-driver.js";

export const anthropicOAuth: OAuthAuth = {
	...piAnthropicOAuth,
	login(interaction: AuthInteraction): Promise<OAuthCredential> {
		return driveOAuthFlow("anthropic", "login", undefined, interaction) as Promise<OAuthCredential>;
	},
	refresh(credential: OAuthCredential, signal?: AbortSignal): Promise<OAuthCredential> {
		return driveOAuthFlow("anthropic", "refresh", credential, { signal }) as Promise<OAuthCredential>;
	},
};
