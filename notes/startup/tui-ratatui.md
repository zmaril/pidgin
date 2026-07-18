# TUI: ratatui Recreatability Assessment

> **Scope:** can pi's `tui` package (npm `@earendil-works/pi-tui`, 12,166 source LOC across `src/*.ts`) be rebuilt on the Rust ratatui + crossterm stack, and where is the friction?
> **Upstream pin:** `3da591ab74ab9ab407e72ed882600b2c851fae21` (`earendil-works/pi`).
> **Stack surveyed (2026-07-18):** ratatui 0.30.2 (19 Jun 2026), crossterm 0.29.0 (5 Apr 2025), unicode-width 0.2.x, ratatui-image 11.0.6 (25 Jun 2026).
> **Companion:** `notes/startup/porting-map.md` classifies `tui` as HIGH coupling / DEFER-hard and schedules it to land just before interactive mode.

## Verdict

A ratatui-plus-crossterm recreation gets you roughly 60 to 70 percent of the user-visible TUI at low cost: the input stack, Windows support, images, and the compositional widgets (`Text`, `Container`, `Box`, `Spacer`, lists, spinner). The remaining 30 to 40 percent is concentrated in one place and it is the hard part: `tui.ts`'s line-string differential renderer, the exact width contract in `utils.ts`, and the escape-level conformance tests that pin pi's rendering strategy. Those three are load-bearing product behavior, not cosmetics, and ratatui's cell-grid `Buffer` model is a poor fit for them.

Recommended path: adopt crossterm for I/O, events, and Windows, add ratatui-image for graphics, and use ratatui widgets for new or non-conformance surfaces. Do not rebuild pi's core renderer on ratatui's `Buffer`. Port `tui.ts` and `utils.ts` faithfully to Rust instead, keeping the line-diff model and the width tables, because the width-crash contract and the inline-scrollback behavior are pinned by the test suite and visible to users. In short: adopt the I/O layer, port the renderer.

## What pi's TUI actually is

pi's `tui` is a hand-rolled differential ANSI renderer, not a cell-grid framework. The shape below comes from the technical map in `pi-tui-map.md`.

### Renderer model

The whole renderer lives in `src/tui.ts` (1,714 LOC), class `TUI extends Container` (`tui.ts:295`). Components implement `render(width): string[]`, returning an array of whole ANSI-encoded strings, one per logical line (`tui.ts:64-88`). There is no cell or grid buffer. The diff unit is the string line: `doRender()` (`tui.ts:1254-1620`) scans new lines against `previousLines` (`tui.ts:297`) for the first and last changed index by plain string comparison (`tui.ts:1368-1381`), then rewrites only that band (`tui.ts:1493-1549`). A spinner tick repaints one line. Style rides inline as raw SGR escapes inside each string; there is no `(char, fg, bg, attrs, width)` cell struct. This is the core structural mismatch with ratatui.

pi renders inline in the primary screen buffer and manages its own scrollback. It never sends `\x1b[?1049h` (no alt-screen). Full redraws clear with `\x1b[2J\x1b[H\x1b[3J` (clear plus home plus clear-scrollback, `tui.ts:1289`). Viewport bookkeeping is manual: `cursorRow`, `hardwareCursorRow`, `maxLinesRendered`, `viewportTop` (`tui.ts:310-316`), scrolling by emitting `\r\n` and using relative cursor motion because it shares scrollback with the shell.

Every write batch is wrapped in DEC 2026 synchronized output, `\x1b[?2026h` to `\x1b[?2026l` (`tui.ts:1286/1308`, `1463/1570`, `1407/1439`), to prevent tearing. The repaint loop is coalesced and frame-capped: `requestRender()` schedules through `process.nextTick`, throttled to `MIN_RENDER_INTERVAL_MS = 16` (about 60fps). `fullRedrawCount` (`tui.ts:336`) is exposed for tests.

One rule makes the width layer load-bearing: if any rendered line's `visibleWidth(line) > width`, the renderer writes a crash log, tears down the terminal, and throws (`tui.ts:1520-1546`). A width miscalculation is a crash, not a cosmetic drift.

### The width contract

`src/utils.ts` (1,209 LOC) is the correctness heart. Grapheme clusters come from `Intl.Segmenter` (`utils.ts:4`); East Asian width comes from the `get-east-asian-width` npm package (`utils.ts:1,196`). Per-grapheme width `graphemeWidth` (`utils.ts:167-211`) applies pi's own rules in order: tab equals 3 (not 8, `utils.ts:168`); zero-width for combining marks, control, default-ignorable, and surrogates; emoji equals 2 via a `couldBeEmoji` pre-filter confirmed by the `\p{RGI_Emoji}` regex; regional indicators U+1F1E6 to U+1F1FF equal 2 even in isolation (`utils.ts:192-194`) so streaming half-flags do not drift the diff; otherwise the East Asian width table; plus Thai and Lao SARA AM (U+0E33/U+0EB3) add 1 (`utils.ts:199-208`). `visibleWidth` (`utils.ts:216-271`) has an ASCII fast path, a 512-entry width cache, tab expansion, and strips all ANSI/OSC/APC first. The same file holds `AnsiCodeTracker` (a full SGR plus OSC 8 state machine, `utils.ts:390-610`), `wrapTextWithAnsi`, `truncateToWidth`, `sliceByColumn`, and `extractSegments` for overlay compositing.

### Input layer

Three files: `terminal.ts` (531), `keys.ts` (1,401), `stdin-buffer.ts` (434). On start, `ProcessTerminal` writes the Kitty progressive-enhancement query `\x1b[>7u\x1b[?u\x1b[c` (`terminal.ts:15-17`). A non-zero `\x1b[?<n>u` reply enables Kitty; a zero reply falls back to xterm modifyOtherKeys via `\x1b[>4;2m` (`terminal.ts:228-276`). `keys.ts` parses Kitty CSI-u, modifyOtherKeys, and an extensive legacy table, with base-layout handling for non-Latin and remapped keyboards. Bracketed paste is on (`\x1b[?2004h`); mouse sequences are framed defensively but never enabled or consumed; terminal focus events are not used. `stdin-buffer.ts` reassembles partial escape sequences across chunks with a 10ms timeout and carries terminal-specific fixes (WezTerm, Termux, Kitty echo de-dup).

### Widgets

Base contract `Component` plus `Container` (`tui.ts:256-290`). Source lines below.

| Component | File | LOC | Role |
|---|---|---|---|
| Editor | `components/editor.ts` | 2,333 | Multi-line editor: grapheme cursor, soft-wrap, undo, Emacs kill-ring, word nav, bracketed paste, autocomplete, IME marker, history |
| Markdown | `components/markdown.ts` | 858 | Markdown to styled lines via the `marked` tokenizer, themable, OSC 8 links |
| Input | `components/input.ts` | 447 | Single-line focusable input |
| SettingsList | `components/settings-list.ts` | 250 | Key/value settings rows |
| SelectList | `components/select-list.ts` | 229 | Scrollable single-select list |
| Box | `components/box.ts` | 137 | Bordered panel |
| Image | `components/image.ts` | 126 | Terminal image component |
| Text | `components/text.ts` | 106 | Static styled and wrapped text |
| Loader | `components/loader.ts` | 92 | Spinner / progress indicator |
| TruncatedText | `components/truncated-text.ts` | 65 | Single-line truncation with ellipsis |
| CancellableLoader | `components/cancellable-loader.ts` | 40 | Loader with a cancel affordance |
| Spacer | `components/spacer.ts` | 28 | Blank vertical spacing |

Overlays and modals are a renderer feature, not a widget: `TUI.showOverlay` (`tui.ts:493-586`) supports nine anchors, percentage or absolute placement, a focus-restore state machine (`tui.ts:241-251`), and z-ordering. Autocomplete (`autocomplete.ts`, 786 LOC) plus a fuzzy matcher (`fuzzy.ts`, 137 LOC) drive the completion menu the editor shows. There are no tables, no scrollbar widget, and no status-line widget in `tui` itself; those are assembled in the app layer.

### Images, hyperlinks, OSC, native code

`terminal-image.ts` (488 LOC) emits Kitty graphics and iTerm2 inline images (no sixel), with header-only PNG/JPEG/GIF/WebP dimension probes and env-based capability detection. The renderer carries a real Kitty image lifecycle: reserved-row expansion of the changed range, delete-before-redraw ordering, and full-redraw fallbacks when a placement would scroll (`tui.ts:1124-1159`). OSC 8 hyperlinks (`terminal-image.ts:478-480`) are modeled through the wrap and width machinery, preserving open links across wrapped lines. The renderer also emits OSC 0 title, OSC 9;4 progress, OSC 11 background query, and the DEC 2031 color-scheme protocol. There is no OSC 52 clipboard here; clipboard is a `coding-agent` concern.

Two tiny hand-rolled N-API C addons total 123 LOC: `darwin-modifiers.c` (70, polls macOS modifier state via CoreGraphics for Apple Terminal Shift+Enter) and `win32-console-mode.c` (53, sets `ENABLE_VIRTUAL_TERMINAL_INPUT`). Both resolve symbols at load time; prebuilt `.node` binaries are checked in. Both are trivial to replace in Rust.

### Public surface the app consumes

`coding-agent` imports `@earendil-works/pi-tui` in 60 files; the interactive app is 16,663 LOC. The pattern: build a tree of `Container`/`Text`/`Box`/`Spacer` plus a few interactive widgets (`Editor`, `Input`, `SelectList`), mount it on one `TUI` backed by `ProcessTerminal`, drive modals through `TUI.showOverlay`, and dispatch keys via `matchesKey`/`getKeybindings`. The most-imported exports are `Text` (32 files), `Container` (30), `Spacer` (26), `getKeybindings` (14), and `TUI` (14). A port must reproduce that surface for the app to compile.

## Gap analysis

Each capability below is classified as **maps directly** (crossterm or ratatui gives it), **needs a custom widget on ratatui**, or **hard / contract-breaking** (rebuilding on ratatui changes pinned behavior).

| Capability | pi mechanism | ratatui / crossterm state | Verdict |
|---|---|---|---|
| Diff and flush | line-string diff, first/last changed line | immediate-mode cell-grid `Buffer` diff, sparse changed cells | Hard: different diff granularity and byte output |
| Synchronized output | `?2026h`/`?2026l` around every batch | crossterm has `BeginSynchronizedUpdate`; not auto-emitted per frame | Maps directly plus a few lines of glue |
| Inline viewport plus scrollback | manual, no alt-screen, self-managed | `Viewport::Inline` plus `insert_before`, `scrolling-regions` feature | Maps directly for the common case; dynamic resize is unsolved (#984, #2086) |
| Grapheme and width fidelity | `Intl.Segmenter` plus `get-east-asian-width` plus pi rules; crash on mismatch | `unicode-segmentation` plus unicode-width 0.2.x; no width-function hook | Hard: tables disagree on emoji, ZWJ, variation selectors, ambiguous width, tab, newline (#1271 open) |
| Input, Kitty fidelity | full CSI-u plus modifyOtherKeys plus legacy tables | crossterm decodes to its own `KeyEvent`; enhancement flags exposed | Maps directly for standard keys; full CSI-u payload fidelity needs custom work |
| Bracketed paste, mouse, focus, resize | paste on, mouse framed only | `EnableBracketedPaste`, `EnableMouseCapture`, `EnableFocusChange`, `Event::Resize` | Maps directly |
| Images | Kitty plus iTerm2, no sixel | ratatui-image 11.0.6 (Kitty, iTerm2, sixel, half-block) | Needs a crate, well supported |
| OSC 8 hyperlinks | modeled in wrap and width machinery | no core support; #1227 open; `tui-link` / `hyperrat` | Needs a custom widget |
| OSC 52 clipboard | not in `tui` | crossterm 0.29 has OSC 52 set | Maps directly (set only) |
| OSC 0 title | `\x1b]0;...` | crossterm `SetTitle` | Maps directly |
| Editor | 2,333 LOC bespoke | `tui-textarea` (pre-1.0) | Needs a crate or a faithful port |
| Markdown | `marked` tokenizer, 858 LOC | `tui-markdown` (pre-1.0, lossy) | Needs a crate or a faithful port |
| Autocomplete | 786 LOC plus fuzzy | compose `List` plus floating block | Needs a custom widget |
| Scrollbar and spinner | pi has no scrollbar; `Loader` spinner | core `Scrollbar`; `throbber-widgets-tui` | Maps directly |
| Windows | native `SetConsoleMode` addon | crossterm cross-platform down to Windows 7 | Maps directly and strictly better |
| Scrollback and copy | inline shared scrollback | inline mode gives native scrollback and copy | Maps directly |

The pattern is clear. The event, I/O, and platform layer maps well; crossterm covers Kitty flags, paste, mouse, focus, resize, OSC 52 set, and title, and it hands you Windows and native scrollback at no extra cost. The rendering core does not map. ratatui measures width with unicode-width 0.2.x through `unicode-segmentation` and offers no supported hook to swap the width function, so pi's own tables (tab equals 3, regional-indicator equals 2 in isolation, the Thai/Lao rule, the emoji pre-filter) will disagree and shift column positions. Because width threads through `Buffer`, layout, truncation, and the diff invariant, that disagreement is not local. And ratatui's diff emits only changed cells against the prior frame, so there is no stable full-frame-to-bytes mapping that pi's escape-substring tests expect.

## Test conformance

pi's tests run on `node --test` (node:test), not vitest, across 27 `*.test.ts` files. The harness `test/virtual-terminal.ts` (218 LOC) backs the `Terminal` interface with `@xterm/headless`, a real VT emulator: tests write pi's output into xterm and read back the emulated screen grid cell by cell. This is the key enabler for a port. Most assertions target the resulting screen, so a different backend that yields the same screen can pass.

The nuance that matters: some tests do not stop at the screen. `tui-render.test.ts` (767 LOC) asserts that raw writes contain `\x1b[2J`, that an image delete index precedes its draw index, that reserved-row changes do not force `\x1b[2J`, and that `fullRedraws` increments on unsafe image cases. The width tests assert exact integers: a lone regional-indicator letter measures 2, every regional-indicator singleton measures 2, tab measures 3, and wrap splits land at specific shapes. Those escape-substring and exact-width assertions encode pi's rendering strategy, not just its output.

ratatui's `TestBackend` renders to a `Buffer` of cells, not to a raw ANSI byte stream, and its snapshots serialize the grid as text without SGR bytes. Byte-identical ANSI snapshots are therefore unattainable on a ratatui backend: the emitter diffs against the prior frame, coalesces style runs, and picks its own cursor-move and clear strategy. So the conformance suite tiers cleanly:

- **Pure-function tests port cleanly if the width module is reimplemented to match.** Width, key parsing, wrapping, kill-ring, undo, and color/image parsing are pure functions; they are the highest-value first targets. Their value depends entirely on the Rust width module reproducing `utils.ts:167-211` exactly, validated against the width fixtures before anything else.
- **Renderer and strategy tests need their own conformance definition.** Screen-equivalence (read back through an emulator, compare the grid) is portable and worth keeping. Byte parity is not. Assertions that pin `\x1b[2J`, sync markers, image delete/draw ordering, or `fullRedraws` counts must be relaxed or re-expressed for whatever backend the port uses.

On the planned napi bridge: because the node:tests are the conformance bar, a Rust implementation can expose a thin JS shim so the existing node:tests drive it directly for the pure-function layer (width, keys, wrap, kill-ring). That reuse is real and valuable. The renderer-strategy assertions cannot ride the same bridge unchanged; they would need re-expression as screen-equivalence, or the Rust renderer would need to reproduce pi's exact escape output, which a faithful port of `tui.ts` (rather than a ratatui rebuild) can actually do.

## Recommendation

Adopt a hybrid. Take crossterm for the I/O, event, and platform layer; it is a strong fit and a clear win. It covers raw mode, the Kitty enhancement flags, bracketed paste, mouse, focus, resize, OSC 52 set, and the window title, and it hands you Windows support that a Unix-termios renderer cannot offer plus native inline scrollback. Add ratatui-image 11.0.6 for graphics; it unifies Kitty, iTerm2, and sixel and is actively maintained. Use ratatui's built-in and third-party widgets (`Scrollbar`, a throbber, `List`) for new surfaces and anything that does not have to pass pi's escape-level tests.

Do not rebuild pi's core renderer on ratatui's `Buffer`. Two pieces of pinned behavior break if you do. First, the width contract: unicode-width 0.2.x disagrees with pi's tables on emoji, ZWJ, variation selectors, ambiguous width, tab, and newline, and ratatui offers no supported hook to replace its width function. Since pi crashes on a width mismatch and its tests assert exact integers, an approximate width model is a correctness regression, not a rough edge. Second, the strategy tests: ratatui's cell-diff emits only changed cells against the prior frame, so the escape-substring and `fullRedraws` assertions in `tui-render.test.ts` cannot pass byte-for-byte, and the inline no-alt-screen shared-scrollback behavior that users rely on is not what an alt-screen cell grid produces by default.

Instead, port `tui.ts` and `utils.ts` faithfully to Rust: keep the line-string diff, keep pi's width tables, keep the inline viewport and self-managed scrollback, and use crossterm only as the ANSI sink and event source. Replace the two C addons with `core-graphics` (macOS modifier state) and `windows`/`winapi` (`SetConsoleMode`). Port `keys.ts` directly rather than leaning on crossterm's decoded `KeyEvent`, because pi's `KeyId` semantics (base-layout remapping, event-type filtering, terminal quirks) exceed what crossterm surfaces and are pinned by `keys.test.ts`. The `Markdown` component is its own sub-project (858 LOC plus 1,442 LOC of tests): `marked` has no drop-in Rust twin, so `pulldown-cmark` with a custom terminal renderer needs revalidation against `markdown.test.ts`.

### Effort shape (relative, not calendar time)

- **Low, high-leverage:** crossterm I/O and event wiring, native-addon replacement, ratatui-image integration, the compositional widgets (`Text`, `Container`, `Box`, `Spacer`, `Input`, `SelectList`), spinner, scrollbar. These are the roughly 60 to 70 percent of the user-visible TUI that a ratatui-backed recreation gets you quickly.
- **Medium:** the input port (`keys.ts` plus `stdin-buffer.ts`, large but mechanical), overlays and focus, autocomplete and fuzzy.
- **High, and the crux:** the width module (`utils.ts`, ported bit-exactly and validated against the width fixtures first), the renderer (`tui.ts`, line-diff plus overlay compositing plus the Kitty image lifecycle plus inline scrollback), the `Editor` (2,333 LOC), and the `Markdown` renderer. This is the 30 to 40 percent that is custom or contract-breaking, and it concentrates in `tui.ts`, the width exactness in `utils.ts`, and the escape-level tests.

The headline: crossterm yes, ratatui for the periphery, but the renderer and width module are a faithful Rust port of pi, not a rebuild on ratatui's `Buffer`.
