// Native shim for packages/coding-agent/src/utils/git.ts, backed by the atilla
// Rust addon (`atilla-napi`). Installed by conformance/codegen.mjs when the
// module is marked `native` in conformance/manifest.json: the original pi file
// is preserved alongside as `git.__pi_original__.ts` and this shim takes its
// place, so pi's tests import `../src/utils/git.ts` unchanged and hit Rust.
//
// Scope of the native flip: `parseGitUrl`, ported to
// `atilla_coding::utils::git_url`. The native function returns pi's exact
// `GitSource` JSON shape (`{ type, repo, host, path, ref?, pinned }`) as a
// string, or `null`; the shim `JSON.parse`s it. The `GitSource` type is
// re-exported unchanged from the original.

export * from "./git.__pi_original__.ts";

import { parseGitUrl as nativeParseGitUrl } from "atilla-napi";
import type { GitSource } from "./git.__pi_original__.ts";

export function parseGitUrl(source: string): GitSource | null {
	const result = nativeParseGitUrl(source);
	return result === null ? null : (JSON.parse(result) as GitSource);
}
