// Native shim for packages/coding-agent/src/utils/changelog.ts, backed by the
// atilla Rust addon (`atilla-napi`). Installed by conformance/codegen.mjs when
// the module is marked `native` in conformance/manifest.json: the original pi
// file is preserved alongside as `changelog.__pi_original__.ts` and this shim
// takes its place, so pi's tests import `../src/utils/changelog.ts` unchanged
// and hit Rust.
//
// Scope of the native flip: `normalizeChangelogLinks`, ported to
// `atilla_coding::utils::changelog`. The Rust boundary accepts the `version`
// argument (pi's `string | ChangelogEntry`) as a JSON string, branching on
// whether it is a bare string or an object. `parseChangelog` reads from disk and
// `getChangelogPath`/`ChangelogEntry` are re-exported unchanged from the
// original.

export * from "./changelog.__pi_original__.ts";

import { normalizeChangelogLinks as nativeNormalizeChangelogLinks } from "atilla-napi";
import type { ChangelogEntry } from "./changelog.__pi_original__.ts";

export function normalizeChangelogLinks(markdown: string, version: string | ChangelogEntry): string {
	return nativeNormalizeChangelogLinks(markdown, JSON.stringify(version));
}
