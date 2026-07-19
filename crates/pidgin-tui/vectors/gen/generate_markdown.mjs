// Generates the byte-exact markdown render vectors consumed by
// `tests/markdown_vectors.rs`. It imports pi's OWN `Markdown` renderer +
// `defaultMarkdownTheme` and replays every `markdown.test.ts` input, dumping
// the raw (ANSI-bearing) and ANSI-stripped output lines. pi is the source of
// truth; the Rust port asserts byte-identical output against this file.
//
//   node crates/pidgin-tui/vectors/gen/generate_markdown.mjs
//
// The one-per-case `style`/`caps`/`opts` descriptors are reproduced exactly by
// the Rust test's chalk-equivalent theme + capability seam.
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { Chalk } from "../../../../vendor/pi/node_modules/chalk/source/index.js";
import { Markdown } from "../../../../vendor/pi/packages/tui/src/components/markdown.ts";
import {
	resetCapabilitiesCache,
	setCapabilities,
} from "../../../../vendor/pi/packages/tui/src/terminal-image.ts";
import { defaultMarkdownTheme } from "../../../../vendor/pi/packages/tui/test/test-themes.ts";

const chalk = new Chalk({ level: 3 });
const strip = (l) => l.replace(/\x1b\[[0-9;]*m/g, "");

// Style descriptors map to a Rust chalk-equivalent in the vector test.
const styleFns = {
	"gray-italic": { color: (t) => chalk.gray(t), italic: true },
	magenta: { color: (t) => chalk.magenta(t) },
	cyan: { color: (t) => chalk.cyan(t) },
	"yellow-italic": { color: (t) => chalk.yellow(t), italic: true },
};

// Each case mirrors an `it(...)` in markdown.test.ts (name, input, width, and
// any padding / style / options / capability overrides that test applies).
const cases = [
	{ n: "nested-list", input: "- Item 1\n  - Nested 1.1\n  - Nested 1.2\n- Item 2", w: 80 },
	{ n: "deep-nested", input: "- Level 1\n  - Level 2\n    - Level 3\n      - Level 4", w: 80 },
	{ n: "ordered-nested", input: "1. First\n   1. Nested first\n   2. Nested second\n2. Second", w: 80 },
	{ n: "normalize-ordered", input: "1. alpha\n1. beta\n1. gamma", w: 80 },
	{ n: "preserve-markers", input: "  4. forth\n  3. third\n\n10) ten\n7) seven\n\n+ plus\n* star\n- minus\n+", w: 80, opts: { preserveOrderedListMarkers: true } },
	{ n: "mixed-nested", input: "1. Ordered item\n   - Unordered nested\n   - Another nested\n2. Second ordered\n   - More nested", w: 80 },
	{ n: "loose-list", input: "1. Lorem ipsum dolor sit amet.\n\n   Ut enim ad minim veniam.\n\n2. Duis aute irure dolor.\n\n   Excepteur sint occaecat cupidatat.\n\n3. Beep boop", w: 80 },
	{ n: "task-list", input: "- [ ] beep\n- [x] boop", w: 80 },
	{ n: "llm-numbering", input: "1. First item\n\n```typescript\n// code block\n```\n\n2. Second item\n\n```typescript\n// another code block\n```\n\n3. Third item", w: 80 },
	{ n: "wrap-unordered", input: "- alpha beta gamma delta epsilon", w: 20 },
	{ n: "wrap-ordered", input: "1. alpha beta gamma delta epsilon", w: 20 },
	{ n: "wrap-multidigit", input: "10. alpha beta gamma delta epsilon", w: 21 },
	{ n: "wrap-nested", input: "- parent\n  - alpha beta gamma delta epsilon", w: 24 },
	{ n: "wrap-nested-ordered", input: "1. parent\n   - alpha beta gamma delta epsilon", w: 24 },
	{ n: "blockquote-in-list", input: "- > alpha beta gamma delta epsilon zeta", w: 24 },
	{ n: "code-in-list", input: "- ```ts\n  alpha beta gamma delta epsilon zeta\n  ```", w: 24 },

	{ n: "table-simple", input: "| Name | Age |\n| --- | --- |\n| Alice | 30 |\n| Bob | 25 |", w: 80 },
	{ n: "table-longword", input: "| Column One | Column Two |\n| --- | --- |\n| superlongword short | otherword |\n| small | tiny |", w: 32 },
	{ n: "table-align", input: "| Left | Center | Right |\n| :--- | :---: | ---: |\n| A | B | C |\n| Long text | Middle | End |", w: 80 },
	{ n: "table-varying", input: "| Short | Very long column header |\n| --- | --- |\n| A | This is a much longer cell content |\n| B | Short |", w: 80 },
	{ n: "table-wrap-cells", input: "| Command | Description | Example |\n| --- | --- | --- |\n| npm install | Install all dependencies | npm install |\n| npm run build | Build the project | npm run build |", w: 50 },
	{ n: "table-wrap-long-cell", input: "| Header |\n| --- |\n| This is a very long cell content that should wrap |", w: 25 },
	{ n: "table-wrap-url", input: "| Value |\n| --- |\n| prefix https://example.com/this/is/a/very/long/url/that/should/wrap |", w: 30, caps: { images: null, trueColor: false, hyperlinks: false } },
	{ n: "table-wrap-inline-code", input: "| Code |\n| --- |\n| `averyveryveryverylongidentifier` |", w: 20 },
	{ n: "table-narrow", input: "| A | B | C |\n| --- | --- | --- |\n| 1 | 2 | 3 |", w: 15 },
	{ n: "table-fits", input: "| A | B |\n| --- | --- |\n| 1 | 2 |", w: 80 },
	{ n: "table-paddingx", input: "| Column One | Column Two |\n| --- | --- |\n| Data 1 | Data 2 |", w: 40, px: 2 },
	{ n: "table-no-trailing", input: "| Name |\n| --- |\n| Alice |", w: 80 },

	{ n: "combined", input: "# Test Document\n\n- Item 1\n  - Nested item\n- Item 2\n\n| Col1 | Col2 |\n| --- | --- |\n| A | B |", w: 80 },

	{ n: "escape-normalize", input: '"\\"', w: 80 },
	{ n: "escape-preserve", input: '"\\"', w: 80, opts: { preserveBackslashEscapes: true } },

	{ n: "prestyled-code", input: "This is thinking with `inline code` and more text after", w: 80, px: 1, style: "gray-italic" },
	{ n: "prestyled-bold", input: "This is thinking with **bold text** and more after", w: 80, px: 1, style: "gray-italic" },

	{ n: "code-one-blank", input: 'hello world\n\n```js\nconst hello = "world";\n```\n\nagain, hello world', w: 80 },
	{ n: "code-norm-a", input: "hello this is text\n```\ncode block\n```\nmore text", w: 80 },
	{ n: "code-norm-b", input: "hello this is text\n\n```\ncode block\n```\n\nmore text", w: 80 },
	{ n: "code-no-trail-a", input: "```js\nconst hello = 'world';\n```", w: 80 },
	{ n: "code-no-trail-b", input: "hello world\n\n```js\nconst hello = 'world';\n```", w: 80 },

	{ n: "divider-one-blank", input: "hello world\n\n---\n\nagain, hello world", w: 80 },
	{ n: "divider-no-trail", input: "---", w: 80 },

	{ n: "heading-one-blank", input: "# Hello\n\nThis is a paragraph", w: 80 },
	{ n: "heading-no-trail", input: "# Hello", w: 80 },

	{ n: "bq-one-blank", input: "hello world\n\n> This is a quote\n\nagain, hello world", w: 80 },
	{ n: "bq-no-trail", input: "> This is a quote", w: 80 },

	{ n: "bq-lazy", input: ">Foo\nbar", w: 80, style: "magenta" },
	{ n: "bq-explicit", input: ">Foo\n>bar", w: 80, style: "cyan" },
	{ n: "bq-list-content", input: "> 1. bla bla\n> - nested bullet", w: 80 },
	{ n: "bq-wrap-long", input: "> This is a very long blockquote line that should wrap to multiple lines when rendered", w: 30 },
	{ n: "bq-wrap-styled", input: "> This is styled text that is long enough to wrap", w: 25, style: "yellow-italic" },
	{ n: "bq-inline-fmt", input: "> Quote with **bold** and `code`", w: 80 },

	{ n: "heading-code-h3", input: "### Why `sourceInfo` should not be optional", w: 80 },
	{ n: "heading-code-h1", input: "# Title with `code` inside", w: 80 },
	{ n: "heading-bold-h2", input: "## Heading with **bold** and more", w: 80 },

	{ n: "strike-double", input: "Use ~~strikethrough~~ here", w: 80 },
	{ n: "strike-single", input: "Use ~strikethrough~ literally", w: 80 },

	{ n: "link-email-nodup", input: "Contact user@example.com for help", w: 80, caps: { images: null, trueColor: false, hyperlinks: false } },
	{ n: "link-bareurl-nodup", input: "Visit https://example.com for more", w: 80, caps: { images: null, trueColor: false, hyperlinks: false } },
	{ n: "link-parens", input: "[click here](https://example.com)", w: 80, caps: { images: null, trueColor: false, hyperlinks: false } },
	{ n: "link-mailto-parens", input: "[Email me](mailto:test@example.com)", w: 80, caps: { images: null, trueColor: false, hyperlinks: false } },
	{ n: "link-osc8", input: "[click here](https://example.com)", w: 80, caps: { images: null, trueColor: false, hyperlinks: true } },
	{ n: "link-osc8-mailto", input: "[Email me](mailto:test@example.com)", w: 80, caps: { images: null, trueColor: false, hyperlinks: true } },
	{ n: "link-osc8-bareurl", input: "Visit https://example.com for more", w: 80, caps: { images: null, trueColor: false, hyperlinks: true } },

	{ n: "html-inline", input: "This is text with <thinking>hidden content</thinking> that should be visible", w: 80 },
	{ n: "html-code-block", input: "```html\n<div>Some HTML</div>\n```", w: 80 },
];

// Streaming fence sub-cases (one `it` block with 6 inputs).
const streaming = [
	{ input: "```ts\nconst x = 1;\n``", w: 80 },
	{ input: "```md\nnot a closing fence:\n``\n```", w: 80 },
	{ input: "```ts\n``", w: 80 },
	{ input: "````\n```", w: 80 },
	{ input: "~~~~~\n~~~~", w: 80 },
	{ input: "```md\nnot a closing fence:\n``\n```\n\nafter", w: 80 },
];

const out = [];
for (const c of cases) {
	if (c.caps) setCapabilities(c.caps);
	const md = new Markdown(c.input, c.px ?? 0, c.py ?? 0, defaultMarkdownTheme, c.style ? styleFns[c.style] : undefined, c.opts);
	const raw = md.render(c.w);
	if (c.caps) resetCapabilitiesCache();
	out.push({
		name: c.n,
		input: c.input,
		width: c.w,
		paddingX: c.px ?? 0,
		paddingY: c.py ?? 0,
		style: c.style ?? null,
		opts: c.opts ?? null,
		hyperlinks: c.caps ? c.caps.hyperlinks : false,
		raw,
		stripped: raw.map((l) => strip(l).trimEnd()),
	});
}

let idx = 0;
for (const s of streaming) {
	const md = new Markdown(s.input, 0, 0, defaultMarkdownTheme);
	const raw = md.render(s.w);
	out.push({
		name: `stream-${idx++}`,
		input: s.input,
		width: s.w,
		paddingX: 0,
		paddingY: 0,
		style: null,
		opts: null,
		hyperlinks: false,
		raw,
		stripped: raw.map((l) => strip(l).trimEnd()),
	});
}

const here = dirname(fileURLToPath(import.meta.url));
const dest = resolve(here, "../../tests/vectors/markdown_render.json");
writeFileSync(dest, `${JSON.stringify({ cases: out }, null, "\t")}\n`);
console.log(`markdown render vectors written: ${out.length} -> ${dest}`);
