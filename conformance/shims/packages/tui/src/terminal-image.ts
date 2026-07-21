// Native shim for packages/tui/src/terminal-image.ts, backed by the pidgin Rust
// addon (`pidgin-napi`). Installed by conformance/codegen.mjs when the module is
// marked `native` in conformance/manifest.json: the original pi file is preserved
// alongside as `terminal-image.__pi_original__.ts` and this shim takes its place,
// so pi's tests import `../src/terminal-image.ts` unchanged and hit Rust.
//
// Scope of the native flip: pi's terminal-graphics helpers are ported byte-exact
// in `crates/pidgin-tui` (`terminal_image.rs`, plus `isImageLine`/`deleteKittyImage`
// in `renderer.rs`) and exposed from `pidgin-napi`. Every export the suite
// exercises runs in Rust: the `isImageLine` scanner, the Kitty / iTerm2 encoders
// and delete commands, the PNG/JPEG/GIF/WebP header parsers, the cell/image
// dimension math, the OSC 8 `hyperlink` wrapper, `renderImage`, and the
// module-level capability cache + cell dimensions (whose `get`/`set`/`reset`
// accessors all route to a single addon-owned state, so `renderImage` and the
// `Image` component read exactly what the tests set).
//
// The flip boundary: exactly one pi export stays in TS — `detectCapabilities`.
// Its optional `tmuxForwardsHyperlink` parameter is a JS closure (defaulting to
// a `tmux` shell-out) that pi calls only in the tmux branch; a JS closure cannot
// cross the addon boundary cleanly, so `export *` below re-exports pi's own
// `detectCapabilities` from the preserved original unchanged. It is a pure,
// cache-free reader that shares no state with the native cache, so the split is
// safe. `calculateImageCellSize`, `calculateImageRows`, `encodeITerm2`, the
// per-format dimension parsers, and every `interface`/`type` are likewise
// re-exported from the original (pure, no shared state); the named re-export from
// `pidgin-napi` below shadows the star-export for the symbols that go native.

export * from "./terminal-image.__pi_original__.ts";

// Native replacements. An explicit named re-export takes precedence over the
// `export *` above for these names, so callers importing `../src/terminal-image.ts`
// get the Rust-backed versions while everything else stays pi's TS.
export {
	allocateImageId,
	deleteAllKittyImages,
	deleteKittyImage,
	encodeKitty,
	getCapabilities,
	getCellDimensions,
	getImageDimensions,
	hyperlink,
	imageFallback,
	isImageLine,
	renderImage,
	resetCapabilitiesCache,
	setCapabilities,
	setCellDimensions,
} from "pidgin-napi";
