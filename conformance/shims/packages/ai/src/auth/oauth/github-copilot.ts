// Native shim for packages/ai/src/auth/oauth/github-copilot.ts, backed by the
// pidgin Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs when the
// module is marked `native` in conformance/manifest.json: the original pi file is
// preserved alongside as `github-copilot.__pi_original__.ts` and this shim takes
// its place, so pi's tests import `../src/auth/oauth/github-copilot.ts` unchanged
// and hit Rust.
//
// Scope of the native flip: the multi-step `login` / `refresh` OAuth flows, ported
// to the Rust `OAuthFlowMachine` (`pidgin_ai::auth::oauth`). The Rust machine owns
// the flow logic (the enterprise-URL text prompt, the device-code poll loop with
// wait-before-first-poll + server slow_down intervals, the verification_uri
// normalization/validation, the copilot-token exchange, the per-model policy POSTs
// and available-model filtering); the shared `driveOAuthFlow` helper performs the
// effects (fetch / sleep / prompt / notify) in JS so pi's `vi.stubGlobal` fetch and
// fake timers still apply. Everything else the module exports (the `name`/`toAuth`
// surface and any constants) is re-exported from the original unchanged.

export * from "./github-copilot.__pi_original__.ts";

import { githubCopilotOAuth as piGithubCopilotOAuth } from "./github-copilot.__pi_original__.ts";
import type { AuthInteraction, OAuthAuth, OAuthCredential } from "../types.ts";
import { driveOAuthFlow } from "pidgin-napi/oauth-flow-driver.js";

export const githubCopilotOAuth: OAuthAuth = {
	...piGithubCopilotOAuth,
	login(interaction: AuthInteraction): Promise<OAuthCredential> {
		return driveOAuthFlow("github-copilot", "login", undefined, interaction) as Promise<OAuthCredential>;
	},
	refresh(credential: OAuthCredential, signal?: AbortSignal): Promise<OAuthCredential> {
		return driveOAuthFlow("github-copilot", "refresh", credential, { signal }) as Promise<OAuthCredential>;
	},
};
