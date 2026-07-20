// Faithful shim of the extension-facing VALUE surface of @earendil-works/pi-ai
// (pi packages/ai/src/index.ts): it re-exports `Type` from typebox and provides
// `StringEnum`. Everything else pi-ai exports at that entry is `export type`,
// erased at transpile, so tool extensions only ever pull these two values.
//
// The `typebox` specifier below nest-resolves through the SAME pidgin module
// loader to the SAME vendored TypeBox bundle that a direct `import { Type } from
// "typebox"` resolves to (deno_core dedups by resolved URL), so `Type`'s identity
// is shared across the pi-ai shim and any direct typebox importer.
import { Type } from "typebox";

export { Type };

// StringEnum copied faithfully from pi's
// packages/ai/src/utils/typebox-helpers.ts (~line 14); TS generics and the
// `as any` cast are erased at transpile, so the runtime body is identical.
export function StringEnum(values, options) {
	return Type.Unsafe({
		type: "string",
		enum: values,
		...(options?.description && { description: options.description }),
		...(options?.default && { default: options.default }),
	});
}
