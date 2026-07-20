// Native shim for packages/coding-agent/src/core/compaction/utils.ts, backed by
// the pidgin Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs
// when the module is marked `native` in conformance/manifest.json: the original
// pi file is preserved alongside as `utils.__pi_original__.ts` and this shim
// takes its place, so pi's tests import `../src/core/compaction/utils.ts`
// unchanged and hit Rust.
//
// Scope of the native flip (partial): the pure message-to-text serializer
// `serializeConversation`, ported to
// `pidgin_coding::core::compaction::serialize_conversation`. pi's messages are
// opaque JSON to the Rust port, so the shim marshals `Message[]` across the
// boundary as a JSON array string and returns the serialized string unchanged.
// The remaining exports of this module — the file-operation helpers
// (`createFileOps`, `extractFileOpsFromMessage`, `computeFileLists`,
// `formatFileOperations`, the `FileOperations` type) and
// `SUMMARIZATION_SYSTEM_PROMPT` — are NOT ported and are re-exported unchanged
// from the preserved original.

export * from "./utils.__pi_original__.ts";

import type { Message } from "@earendil-works/pi-ai";
import { serializeConversation as nativeSerializeConversation } from "pidgin-napi";

export function serializeConversation(messages: Message[]): string {
	return nativeSerializeConversation(JSON.stringify(messages));
}
