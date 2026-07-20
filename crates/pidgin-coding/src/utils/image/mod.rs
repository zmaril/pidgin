//! Rust port of pi's image pipeline: decode raw image bytes, apply EXIF
//! orientation, resize with Lanczos3, and re-encode to PNG/JPEG under an inline
//! upload size budget.
//!
//! Mirrors pi's `utils/` image cluster, one submodule per pi file:
//!
//! - [`convert`] <- `image-convert.ts` (bytes -> PNG, base64 -> PNG)
//! - [`resize_core`] <- `image-resize-core.ts` (the decode/resize/encode loop)
//! - [`resize`] <- `image-resize.ts` (the resize entry + `formatDimensionNote`)
//! - [`process`] <- `image-process.ts` (the public `processImage` seam)
//!
//! # Dependency: `image` replaces Photon-WASM
//!
//! pi performs all pixel work through `@silvia-odwyer/photon-node`, a Rust image
//! library compiled to WASM and loaded through a Node/Bun-specific loader
//! (`photon.ts`). That loader patches `fs.readFileSync` so Bun-compiled binaries
//! find `photon_rs_bg.wasm` next to the executable. None of that WASM-loading
//! machinery has a Rust equivalent, so `photon.ts` is an intentional non-port.
//! Instead this port uses the pure-Rust [`image`] crate directly for the
//! operations pi actually performs: decode, Lanczos3 downscale, PNG/JPEG encode
//! at quality steps, and the EXIF-orientation pixel transform (in
//! [`crate::utils::exif`]).
//!
//! # Non-port: worker threads
//!
//! pi runs the resize in a `node:worker_threads` Worker (`image-resize.ts` +
//! `image-resize-worker.ts`) so WASM decode/resize/encode does not block the
//! TUI event loop, with an in-process fallback. This port does the resize
//! synchronously and does not reproduce the worker-thread indirection: it is a
//! Node concurrency detail, not observable behavior. [`resize::resize_image`]
//! is the faithful analogue of pi's `resizeImage`, calling straight through to
//! [`resize_core::resize_image_in_process`].

pub mod convert;
pub mod process;
pub mod resize;
pub mod resize_core;

pub use convert::{convert_image_bytes_to_png, convert_to_png, ConvertedImage};
pub use process::{process_image, ProcessImageOptions, ProcessImageResult};
pub use resize::{format_dimension_note, resize_image};
pub use resize_core::{resize_image_in_process, ImageResizeOptions, ResizedImage};

use ::image::RgbaImage;

/// Decode encoded image bytes and apply the EXIF orientation transform,
/// yielding an upright RGBA image. Returns `None` when the bytes cannot be
/// decoded.
///
/// This mirrors the shared prologue of pi's `image-convert.ts` and
/// `image-resize-core.ts`, both of which build a Photon image with
/// `new_from_byteslice` and then call `applyExifOrientation`.
pub(crate) fn decode_and_orient(bytes: &[u8]) -> Option<RgbaImage> {
    let decoded = ::image::load_from_memory(bytes).ok()?;
    let rgba = decoded.to_rgba8();
    Some(crate::utils::exif::apply_exif_orientation(rgba, bytes))
}
