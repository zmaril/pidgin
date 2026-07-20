// Vendored verbatim from the pi project (earendil-works/pi), MIT License.
// Copyright (c) 2025 Mario Zechner. See ../NOTICE for the full license text.
// upstream: packages/coding-agent/examples/extensions/hello.ts
// Vendored verbatim; needs the pi-ai/pi-coding-agent shims + the typebox module
// loader (its `defineTool` + `Type` value imports must resolve at load time).

/**
 * Hello Tool - Minimal custom tool example
 */

import { Type } from "@earendil-works/pi-ai";
import { defineTool, type ExtensionAPI } from "@earendil-works/pi-coding-agent";

const helloTool = defineTool({
	name: "hello",
	label: "Hello",
	description: "A simple greeting tool",
	parameters: Type.Object({
		name: Type.String({ description: "Name to greet" }),
	}),

	async execute(_toolCallId, params, _signal, _onUpdate, _ctx) {
		return {
			content: [{ type: "text", text: `Hello, ${params.name}!` }],
			details: { greeted: params.name },
		};
	},
});

export default function (pi: ExtensionAPI) {
	pi.registerTool(helloTool);
}
