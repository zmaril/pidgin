// Native shim for packages/coding-agent/src/utils/mime.ts, backed by the atilla
// Rust addon (`atilla-napi`). Installed by conformance/codegen.mjs when the
// module is marked `native` in conformance/manifest.json: the original pi file
// is preserved alongside as `mime.__pi_original__.ts` and this shim takes its
// place, so pi's tests import `../src/utils/mime.ts` unchanged and hit Rust.
//
// Scope of the native flip: the pure byte-sniffer `detectSupportedImageMimeType`,
// ported to `atilla_coding::utils::mime`. pi's file-reading wrapper
// `detectSupportedImageMimeTypeFromFile` (which opens a file and reads header
// bytes) is not ported and is re-exported unchanged from the original.

export * from "./mime.__pi_original__.ts";

import { detectSupportedImageMimeType as nativeDetectSupportedImageMimeType } from "atilla-napi";

export function detectSupportedImageMimeType(buffer: Uint8Array): string | null {
	return nativeDetectSupportedImageMimeType(buffer) ?? null;
}
