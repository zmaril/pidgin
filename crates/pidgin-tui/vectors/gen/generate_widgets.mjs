// straitjacket-allow-file:duplication — this generator's dump()/paths boilerplate
// intentionally mirrors generate_components.mjs / generate_keys.mjs; each
// generator is a standalone script.
// straitjacket-allow-file:emoji — the CJK/emoji literals are UTF-8 width test
// data fed to pi's own render functions, not decorative prose (mirrors
// generate_components.mjs's emoji allow-file).
//
// Vector generator for the byte-exact Rust port of pi's TUI LEAF WIDGETS (PR C2:
// box, text, loader, truncated-text, spacer, image). Runs pi's OWN component
// classes from vendor/pi/packages/tui/src/components/*.ts (Node 22 strips TS
// types natively) plus terminal-image.ts, and dumps input/props -> expected
// rendered lines that the Rust test suite asserts byte-identical.
//
// Run from this directory:  node generate_widgets.mjs
// Output is written to ../../tests/vectors/*.json
//
// pi upstream pin: vendor/pi submodule @ 3da591a (pi v0.80.10).

import { Box } from "../../../../vendor/pi/packages/tui/src/components/box.ts";
import { Image } from "../../../../vendor/pi/packages/tui/src/components/image.ts";
import { Loader } from "../../../../vendor/pi/packages/tui/src/components/loader.ts";
import { Spacer } from "../../../../vendor/pi/packages/tui/src/components/spacer.ts";
import { Text } from "../../../../vendor/pi/packages/tui/src/components/text.ts";
import { TruncatedText } from "../../../../vendor/pi/packages/tui/src/components/truncated-text.ts";
import {
	getImageDimensions,
	setCapabilities,
	setCellDimensions,
} from "../../../../vendor/pi/packages/tui/src/terminal-image.ts";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// Literal ANSI style helpers matching chalk level-3 output (the styled inputs
// truncated-text.test.ts / representative Text cases feed as data). Kept inline
// so the generator does not depend on chalk's module resolution.
const chalk = {
	red: (s) => `\x1b[31m${s}\x1b[39m`,
	blue: (s) => `\x1b[34m${s}\x1b[39m`,
	green: (s) => `\x1b[32m${s}\x1b[39m`,
};

const here = dirname(fileURLToPath(import.meta.url));
const outDir = join(here, "..", "..", "tests", "vectors");
mkdirSync(outDir, { recursive: true });

let total = 0;
function dump(name, vectors) {
	const path = join(outDir, `${name}.json`);
	writeFileSync(path, `${JSON.stringify(vectors, null, "\t")}\n`);
	total += vectors.length;
	console.log(`  ${name}.json: ${vectors.length}`);
}

// ---------------------------------------------------------------------------
// Shared, deterministic style closures. Both the generator and the Rust replay
// select the SAME function by tag so the styled output is byte-identical.
// ---------------------------------------------------------------------------
function bgFnFor(tag) {
	switch (tag) {
		case "none":
			return undefined;
		case "redbg":
			return (s) => `\x1b[41m${s}\x1b[0m`;
		case "bold":
			return (s) => `\x1b[1m${s}\x1b[0m`;
		case "cyan":
			return (s) => `\x1b[36m${s}\x1b[0m`;
		default:
			throw new Error(`unknown bg tag: ${tag}`);
	}
}

function colorFnFor(tag) {
	switch (tag) {
		case "plain":
			return (s) => s;
		case "cyan":
			return (s) => `\x1b[36m${s}\x1b[0m`;
		case "bold":
			return (s) => `\x1b[1m${s}\x1b[0m`;
		case "yellow":
			return (s) => `\x1b[33m${s}\x1b[0m`;
		default:
			throw new Error(`unknown color tag: ${tag}`);
	}
}

// ---------------------------------------------------------------------------
// spacer
// ---------------------------------------------------------------------------
{
	const vectors = [];
	for (const n of [0, 1, 2, 3, 5]) {
		for (const width of [0, 10, 40]) {
			const s = new Spacer(n);
			vectors.push({ n, width, expected: s.render(width) });
		}
	}
	dump("widget_spacer", vectors);
}

// ---------------------------------------------------------------------------
// text
// ---------------------------------------------------------------------------
{
	const cases = [
		// text, paddingX, paddingY, bgTag
		["", 1, 1, "none"],
		["   ", 1, 1, "none"],
		["Hello world", 1, 1, "none"],
		["Hello world", 1, 0, "none"],
		["Hello world", 0, 0, "none"],
		["Hello world", 2, 2, "none"],
		["This is a longer piece of text that should wrap across several lines", 1, 0, "none"],
		["This is a longer piece of text that should wrap across several lines", 1, 1, "redbg"],
		["Line with\ttab", 1, 0, "none"],
		["First\nSecond\nThird", 1, 1, "none"],
		["Hello world", 1, 1, "redbg"],
		["Hello world", 1, 1, "cyan"],
		["Hello world", 1, 0, "bold"],
		[`${chalk.red("Hello")} ${chalk.blue("world")}`, 1, 0, "none"],
		[`${chalk.green("styled")} text on background`, 1, 1, "redbg"],
		["日本語のテキスト", 1, 0, "none"],
		["emoji 🙂 test", 1, 0, "none"],
		["narrow", 5, 0, "none"],
		["fits exactly here", 1, 0, "none"],
		["wide CJK 世界 wrap test with more words to force wrapping now", 1, 0, "redbg"],
	];
	const widths = [10, 20, 40];
	const vectors = [];
	for (const [text, paddingX, paddingY, bgTag] of cases) {
		for (const width of widths) {
			const t = new Text(text, paddingX, paddingY, bgFnFor(bgTag));
			vectors.push({ text, paddingX, paddingY, bgTag, width, expected: t.render(width) });
		}
	}
	dump("widget_text", vectors);
}

// ---------------------------------------------------------------------------
// truncated-text (covers every truncated-text.test.ts case + extras)
// ---------------------------------------------------------------------------
{
	const cases = [
		// text, paddingX, paddingY, widths
		["Hello world", 1, 0, [50, 30]],
		["Hello", 0, 2, [40]],
		["This is a very long piece of text that will definitely exceed the available width", 1, 0, [30]],
		[`${chalk.red("Hello")} ${chalk.blue("world")}`, 1, 0, [40]],
		[chalk.red("This is a very long red text that will be truncated"), 1, 0, [20]],
		["Hello world", 1, 0, [30]],
		["", 1, 0, [30]],
		["First line\nSecond line\nThird line", 1, 0, [40]],
		["This is a very long first line that needs truncation\nSecond line", 1, 0, [25]],
		["日本語の長いテキストをここで切り詰めます", 1, 0, [10, 20]],
		["tab\there", 0, 0, [20]],
		["emoji 🙂🙂🙂 overflow test string here", 1, 1, [15]],
		["exact", 0, 0, [5]],
		["ab", 3, 0, [4]],
	];
	const vectors = [];
	for (const [text, paddingX, paddingY, widths] of cases) {
		for (const width of widths) {
			const t = new TruncatedText(text, paddingX, paddingY);
			vectors.push({ text, paddingX, paddingY, width, expected: t.render(width) });
		}
	}
	dump("widget_truncated_text", vectors);
}

// ---------------------------------------------------------------------------
// box
// ---------------------------------------------------------------------------
function buildChild(spec) {
	switch (spec.kind) {
		case "text":
			return new Text(spec.text, spec.paddingX, spec.paddingY, bgFnFor(spec.bgTag ?? "none"));
		case "truncated":
			return new TruncatedText(spec.text, spec.paddingX, spec.paddingY);
		case "spacer":
			return new Spacer(spec.n);
		default:
			throw new Error(`unknown child kind: ${spec.kind}`);
	}
}
{
	const scenarios = [
		{ paddingX: 1, paddingY: 1, bgTag: "none", children: [] },
		{
			paddingX: 1,
			paddingY: 1,
			bgTag: "none",
			children: [{ kind: "text", text: "Hello", paddingX: 0, paddingY: 0 }],
		},
		{
			paddingX: 2,
			paddingY: 1,
			bgTag: "redbg",
			children: [{ kind: "text", text: "Hello world", paddingX: 1, paddingY: 0 }],
		},
		{
			paddingX: 1,
			paddingY: 0,
			bgTag: "cyan",
			children: [
				{ kind: "text", text: "Line one", paddingX: 0, paddingY: 0 },
				{ kind: "spacer", n: 1 },
				{ kind: "truncated", text: "A truncated child line that is quite long", paddingX: 0, paddingY: 0 },
			],
		},
		{
			paddingX: 3,
			paddingY: 2,
			bgTag: "none",
			children: [
				{ kind: "text", text: "Nested wrapping text that goes on for a while here", paddingX: 1, paddingY: 0 },
			],
		},
		{
			paddingX: 1,
			paddingY: 1,
			bgTag: "bold",
			children: [{ kind: "text", text: "日本語 box", paddingX: 0, paddingY: 0 }],
		},
	];
	const widths = [12, 24, 40];
	const vectors = [];
	for (const sc of scenarios) {
		for (const width of widths) {
			const box = new Box(sc.paddingX, sc.paddingY, bgFnFor(sc.bgTag));
			for (const childSpec of sc.children) {
				box.addChild(buildChild(childSpec));
			}
			vectors.push({ ...sc, width, expected: box.render(width) });
		}
	}
	dump("widget_box", vectors);
}

// ---------------------------------------------------------------------------
// loader — frame advancement driven by capturing pi's setInterval callback.
// ---------------------------------------------------------------------------
{
	const stubUi = { requestRender() {} };
	const scenarios = [
		{ message: "Loading...", indicator: null, ticks: 0, setMessageTo: null, spinnerTag: "cyan", messageTag: "plain" },
		{ message: "Working", indicator: null, ticks: 0, setMessageTo: null, spinnerTag: "cyan", messageTag: "bold" },
		{ message: "Loading...", indicator: null, ticks: 3, setMessageTo: null, spinnerTag: "cyan", messageTag: "plain" },
		{ message: "Loading...", indicator: null, ticks: 12, setMessageTo: null, spinnerTag: "yellow", messageTag: "plain" },
		{
			message: "Custom",
			indicator: { frames: ["-", "\\", "|", "/"], intervalMs: 100 },
			ticks: 2,
			setMessageTo: null,
			spinnerTag: "cyan",
			messageTag: "plain",
		},
		{
			message: "Verbatim frame",
			indicator: { frames: ["A", "B"] },
			ticks: 1,
			setMessageTo: null,
			spinnerTag: "cyan",
			messageTag: "bold",
		},
		{
			message: "No spinner",
			indicator: { frames: [] },
			ticks: 0,
			setMessageTo: null,
			spinnerTag: "cyan",
			messageTag: "plain",
		},
		{
			message: "Single frame",
			indicator: { frames: ["*"] },
			ticks: 0,
			setMessageTo: null,
			spinnerTag: "cyan",
			messageTag: "plain",
		},
		{
			message: "Start",
			indicator: null,
			ticks: 1,
			setMessageTo: "Updated message",
			spinnerTag: "cyan",
			messageTag: "plain",
		},
	];
	const widths = [20, 40];
	const vectors = [];

	const realSetInterval = global.setInterval;
	const realClearInterval = global.clearInterval;
	for (const sc of scenarios) {
		for (const width of widths) {
			let captured = null;
			global.setInterval = (fn) => {
				captured = fn;
				return 0;
			};
			global.clearInterval = () => {};
			try {
				const loader = new Loader(
					stubUi,
					colorFnFor(sc.spinnerTag),
					colorFnFor(sc.messageTag),
					sc.message,
					sc.indicator ?? undefined,
				);
				for (let i = 0; i < sc.ticks; i++) {
					if (captured) captured();
				}
				if (sc.setMessageTo !== null) {
					loader.setMessage(sc.setMessageTo);
				}
				vectors.push({ ...sc, width, expected: loader.render(width) });
			} finally {
				global.setInterval = realSetInterval;
				global.clearInterval = realClearInterval;
			}
		}
	}
	dump("widget_loader", vectors);
}

// ---------------------------------------------------------------------------
// image — getImageDimensions header parsing.
// ---------------------------------------------------------------------------
function pngBase64(width, height) {
	const buf = Buffer.alloc(24);
	buf[0] = 0x89;
	buf[1] = 0x50;
	buf[2] = 0x4e;
	buf[3] = 0x47;
	buf.writeUInt32BE(width, 16);
	buf.writeUInt32BE(height, 20);
	return buf.toString("base64");
}
function jpegBase64(width, height) {
	// SOI (FFD8) + SOF0 marker (FFC0) + length + precision + height + width.
	// pi reads height at offset+5 and width at offset+7 (offset = 2).
	const buf = Buffer.from([0xff, 0xd8, 0xff, 0xc0, 0x00, 0x11, 0x08, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
	buf.writeUInt16BE(height, 7);
	buf.writeUInt16BE(width, 9);
	return buf.toString("base64");
}
function gifBase64(width, height) {
	const buf = Buffer.alloc(10);
	buf.write("GIF89a", 0, "ascii");
	buf.writeUInt16LE(width, 6);
	buf.writeUInt16LE(height, 8);
	return buf.toString("base64");
}
function webpVp8xBase64(width, height) {
	const buf = Buffer.alloc(30);
	buf.write("RIFF", 0, "ascii");
	buf.write("WEBP", 8, "ascii");
	buf.write("VP8X", 12, "ascii");
	const w = width - 1;
	const h = height - 1;
	buf[24] = w & 0xff;
	buf[25] = (w >> 8) & 0xff;
	buf[26] = (w >> 16) & 0xff;
	buf[27] = h & 0xff;
	buf[28] = (h >> 8) & 0xff;
	buf[29] = (h >> 16) & 0xff;
	return buf.toString("base64");
}
{
	const cases = [
		{ mime: "image/png", data: pngBase64(800, 600) },
		{ mime: "image/png", data: pngBase64(1, 1) },
		{ mime: "image/png", data: pngBase64(1920, 1080) },
		{ mime: "image/jpeg", data: jpegBase64(640, 480) },
		{ mime: "image/jpeg", data: jpegBase64(100, 200) },
		{ mime: "image/gif", data: gifBase64(320, 240) },
		{ mime: "image/webp", data: webpVp8xBase64(256, 128) },
		{ mime: "image/png", data: Buffer.from("not a png").toString("base64") },
		{ mime: "image/bmp", data: pngBase64(10, 10) },
	];
	const vectors = [];
	for (const c of cases) {
		const dims = getImageDimensions(c.data, c.mime);
		vectors.push({ mime: c.mime, data: c.data, dims: dims ?? null });
	}
	dump("widget_image_dimensions", vectors);
}

// ---------------------------------------------------------------------------
// image — render across capability paths (deterministic: explicit imageId +
// forced capabilities + fixed cell dimensions).
// ---------------------------------------------------------------------------
{
	const smallData = Buffer.from("small-image-bytes").toString("base64");
	const bigData = "Q".repeat(9000); // > CHUNK_SIZE, exercises kitty chunking
	const capsNull = { images: null, trueColor: true, hyperlinks: false };
	const capsKitty = { images: "kitty", trueColor: true, hyperlinks: true };
	const capsIterm = { images: "iterm2", trueColor: true, hyperlinks: true };
	const cell = { widthPx: 9, heightPx: 18 };

	const scenarios = [
		// caps, cell, base64, mime, dims, options, fallbackTag, width
		{ caps: capsNull, base64: smallData, mime: "image/png", dims: { widthPx: 800, heightPx: 600 }, options: {}, fallbackTag: "yellow", width: 80 },
		{ caps: capsNull, base64: smallData, mime: "image/png", dims: { widthPx: 800, heightPx: 600 }, options: { filename: "cat.png" }, fallbackTag: "yellow", width: 80 },
		{ caps: capsNull, base64: smallData, mime: "image/jpeg", dims: { widthPx: 100, heightPx: 50 }, options: { filename: "photo.jpg" }, fallbackTag: "bold", width: 40 },
		{ caps: capsKitty, base64: smallData, mime: "image/png", dims: { widthPx: 800, heightPx: 600 }, options: { imageId: 42, maxWidthCells: 60 }, fallbackTag: "yellow", width: 80 },
		{ caps: capsKitty, base64: smallData, mime: "image/png", dims: { widthPx: 400, heightPx: 400 }, options: { imageId: 7, maxWidthCells: 20 }, fallbackTag: "yellow", width: 40 },
		{ caps: capsKitty, base64: smallData, mime: "image/png", dims: { widthPx: 1000, heightPx: 200 }, options: { imageId: 99, maxWidthCells: 60, maxHeightCells: 3 }, fallbackTag: "yellow", width: 100 },
		{ caps: capsKitty, base64: bigData, mime: "image/png", dims: { widthPx: 800, heightPx: 600 }, options: { imageId: 5, maxWidthCells: 60 }, fallbackTag: "yellow", width: 80 },
		{ caps: capsIterm, base64: smallData, mime: "image/png", dims: { widthPx: 800, heightPx: 600 }, options: { maxWidthCells: 60 }, fallbackTag: "yellow", width: 80 },
		{ caps: capsIterm, base64: smallData, mime: "image/png", dims: { widthPx: 400, heightPx: 400 }, options: { maxWidthCells: 20 }, fallbackTag: "yellow", width: 40 },
		{ caps: capsKitty, base64: smallData, mime: "image/png", dims: { widthPx: 800, heightPx: 600 }, options: { imageId: 3 }, fallbackTag: "yellow", width: 3 },
	];
	const vectors = [];
	for (const sc of scenarios) {
		setCellDimensions(cell);
		setCapabilities(sc.caps);
		const theme = { fallbackColor: colorFnFor(sc.fallbackTag) };
		const img = new Image(sc.base64, sc.mime, theme, sc.options, sc.dims);
		const expected = img.render(sc.width);
		vectors.push({
			caps: sc.caps.images,
			cell,
			base64: sc.base64,
			mime: sc.mime,
			dims: sc.dims,
			options: sc.options,
			fallbackTag: sc.fallbackTag,
			width: sc.width,
			expected,
		});
	}
	dump("widget_image_render", vectors);
}

console.log(`\ntotal widget vectors: ${total}`);
