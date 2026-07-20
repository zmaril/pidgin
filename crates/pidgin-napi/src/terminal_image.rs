//! Node-API surface for pi's terminal-graphics helpers (`terminal-image.ts`).
//!
//! This exposes the Rust [`pidgin_tui::terminal_image`] port — a byte-exact
//! translation of `vendor/pi/packages/tui/src/terminal-image.ts` — to pi's
//! `packages/tui` `terminal-image.test.ts`. The Rust module owns the whole
//! surface the tests exercise: the `isImageLine` scanner, the Kitty / iTerm2
//! escape-sequence encoders and delete commands, the cell/image dimension math,
//! the PNG/JPEG/GIF/WebP header parsers, the OSC 8 `hyperlink` wrapper, and the
//! `renderImage` dispatcher. The module-level capability cache and cell
//! dimensions live in Rust `thread_local`s; every accessor
//! (`get`/`set`/`resetCapabilitiesCache`) crosses here so a single addon-owned
//! state is shared across the whole run (napi calls all land on the JS main
//! thread, so the `thread_local` is one consistent instance).
//!
//! # The seam: what stays in TS
//!
//! Exactly one pi export is NOT routed here: `detectCapabilities`. Its optional
//! `tmuxForwardsHyperlink` parameter is a JS closure (defaulting to a `tmux`
//! shell-out) that pi invokes only in the tmux branch. A JS closure cannot cross
//! the addon boundary cleanly, so the shim keeps `detectCapabilities` as pi's
//! own TS (`export *` from the preserved original). It is a pure, cache-free
//! reader, so it shares no state with the native cache and the split is safe.
//! Everything else the test file touches runs in Rust.
//!
//! # Marshaling
//!
//! Every value crosses as a string, number, boolean, plain object, or `null` —
//! all clean JSON-boundary types. Base64 payloads and escape sequences cross as
//! whole JS strings (valid Unicode scalar values, no lone surrogates), and the
//! image-header parsers decode base64 → raw bytes *inside* Rust, so no byte
//! buffer and no byte-boundary/lone-surrogate corruption ever crosses. No JS
//! closures, streams, `AbortSignal`s, or stable-identity objects are required.

use napi::bindgen_prelude::*;
use napi_derive::napi;

use pidgin_tui::terminal_image::{
    self, CellDimensions, ImageDimensions, ImageProtocol, ImageRenderOptions, KittyOptions,
    RenderImageResult, TerminalCapabilities,
};

/// Pixel size of an image, mirroring pi's `ImageDimensions` (`{ widthPx, heightPx }`).
#[napi(object)]
pub struct ImageDimensionsJs {
    pub width_px: u32,
    pub height_px: u32,
}

impl From<ImageDimensions> for ImageDimensionsJs {
    fn from(d: ImageDimensions) -> Self {
        Self {
            width_px: d.width_px,
            height_px: d.height_px,
        }
    }
}

impl From<ImageDimensionsJs> for ImageDimensions {
    fn from(d: ImageDimensionsJs) -> Self {
        Self {
            width_px: d.width_px,
            height_px: d.height_px,
        }
    }
}

/// Pixel size of a single terminal cell, mirroring pi's `CellDimensions`.
#[napi(object)]
pub struct CellDimensionsJs {
    pub width_px: u32,
    pub height_px: u32,
}

impl From<CellDimensions> for CellDimensionsJs {
    fn from(d: CellDimensions) -> Self {
        Self {
            width_px: d.width_px,
            height_px: d.height_px,
        }
    }
}

impl From<CellDimensionsJs> for CellDimensions {
    fn from(d: CellDimensionsJs) -> Self {
        Self {
            width_px: d.width_px,
            height_px: d.height_px,
        }
    }
}

/// Terminal capabilities, mirroring pi's `TerminalCapabilities`
/// (`{ images: "kitty" | "iterm2" | null, trueColor, hyperlinks }`).
#[napi(object)]
pub struct TerminalCapabilitiesJs {
    pub images: Option<String>,
    pub true_color: bool,
    pub hyperlinks: bool,
}

fn protocol_to_str(p: ImageProtocol) -> &'static str {
    match p {
        ImageProtocol::Kitty => "kitty",
        ImageProtocol::ITerm2 => "iterm2",
    }
}

impl From<TerminalCapabilities> for TerminalCapabilitiesJs {
    fn from(c: TerminalCapabilities) -> Self {
        Self {
            images: c.images.map(|p| protocol_to_str(p).to_string()),
            true_color: c.true_color,
            hyperlinks: c.hyperlinks,
        }
    }
}

impl TryFrom<TerminalCapabilitiesJs> for TerminalCapabilities {
    type Error = Error;

    fn try_from(c: TerminalCapabilitiesJs) -> Result<Self> {
        let images = match c.images.as_deref() {
            None => None,
            Some("kitty") => Some(ImageProtocol::Kitty),
            Some("iterm2") => Some(ImageProtocol::ITerm2),
            Some(other) => {
                return Err(Error::new(
                    Status::InvalidArg,
                    format!("unknown image protocol {other:?} (expected \"kitty\", \"iterm2\", or null)"),
                ));
            }
        };
        Ok(Self {
            images,
            true_color: c.true_color,
            hyperlinks: c.hyperlinks,
        })
    }
}

/// Options for [`encode_kitty`], mirroring pi's `encodeKitty` options object.
#[napi(object)]
#[derive(Default)]
pub struct EncodeKittyOptionsJs {
    pub columns: Option<u32>,
    pub rows: Option<u32>,
    pub image_id: Option<u32>,
    pub move_cursor: Option<bool>,
}

/// Options for [`render_image`], mirroring pi's `ImageRenderOptions`.
#[napi(object)]
#[derive(Default)]
pub struct ImageRenderOptionsJs {
    pub max_width_cells: Option<u32>,
    pub max_height_cells: Option<u32>,
    pub preserve_aspect_ratio: Option<bool>,
    pub image_id: Option<u32>,
    pub move_cursor: Option<bool>,
}

/// Result of [`render_image`], mirroring pi's `{ sequence, rows, imageId? }`.
#[napi(object)]
pub struct RenderImageResultJs {
    pub sequence: String,
    pub rows: u32,
    pub image_id: Option<u32>,
}

impl From<RenderImageResult> for RenderImageResultJs {
    fn from(r: RenderImageResult) -> Self {
        Self {
            sequence: r.sequence,
            rows: r.rows,
            image_id: r.image_id.map(|id| id as u32),
        }
    }
}

/// pi's `isImageLine(line)`: whether a rendered line carries a Kitty or iTerm2
/// image escape sequence anywhere in it.
#[napi(js_name = "isImageLine")]
pub fn is_image_line(line: String) -> bool {
    pidgin_tui::is_image_line(&line)
}

/// pi's `allocateImageId()`: a random Kitty image ID in `[1, 0xffffffff]`.
#[napi(js_name = "allocateImageId")]
pub fn allocate_image_id() -> u32 {
    terminal_image::allocate_image_id() as u32
}

/// pi's `encodeKitty(base64Data, options)`: the Kitty graphics escape sequence
/// (chunked at 4096 bytes) for the given base64 payload.
#[napi(js_name = "encodeKitty")]
pub fn encode_kitty(base64_data: String, options: Option<EncodeKittyOptionsJs>) -> String {
    let o = options.unwrap_or_default();
    terminal_image::encode_kitty(
        &base64_data,
        KittyOptions {
            columns: o.columns,
            rows: o.rows,
            image_id: o.image_id.map(|id| id as u64),
            move_cursor: o.move_cursor,
        },
    )
}

/// pi's `deleteKittyImage(imageId)`: the escape sequence that deletes one Kitty
/// image by ID (and frees its data).
#[napi(js_name = "deleteKittyImage")]
pub fn delete_kitty_image(image_id: u32) -> String {
    pidgin_tui::delete_kitty_image(image_id as u64)
}

/// pi's `deleteAllKittyImages()`: the escape sequence that deletes all visible
/// Kitty images (and frees their data).
#[napi(js_name = "deleteAllKittyImages")]
pub fn delete_all_kitty_images() -> String {
    terminal_image::delete_all_kitty_images()
}

/// pi's `getImageDimensions(base64Data, mimeType)`: parse an image header
/// (PNG/JPEG/GIF/WebP) to its pixel dimensions, or `null` on any failure.
#[napi(js_name = "getImageDimensions")]
pub fn get_image_dimensions(base64_data: String, mime_type: String) -> Option<ImageDimensionsJs> {
    terminal_image::get_image_dimensions(&base64_data, &mime_type).map(Into::into)
}

/// pi's `getCellDimensions()`: the current cell pixel size.
#[napi(js_name = "getCellDimensions")]
pub fn get_cell_dimensions() -> CellDimensionsJs {
    terminal_image::get_cell_dimensions().into()
}

/// pi's `setCellDimensions(dims)`: update the cell pixel size used by
/// `renderImage` and the `Image` component.
#[napi(js_name = "setCellDimensions")]
pub fn set_cell_dimensions(dims: CellDimensionsJs) {
    terminal_image::set_cell_dimensions(dims.into());
}

/// pi's `getCapabilities()`: the cached terminal capabilities, detecting and
/// caching on first access.
#[napi(js_name = "getCapabilities")]
pub fn get_capabilities() -> TerminalCapabilitiesJs {
    terminal_image::get_capabilities().into()
}

/// pi's `setCapabilities(caps)`: override the cached capabilities (used by tests
/// to exercise both the Kitty and iTerm2 render paths).
#[napi(js_name = "setCapabilities")]
pub fn set_capabilities(caps: TerminalCapabilitiesJs) -> Result<()> {
    terminal_image::set_capabilities(caps.try_into()?);
    Ok(())
}

/// pi's `resetCapabilitiesCache()`: clear the cached capabilities.
#[napi(js_name = "resetCapabilitiesCache")]
pub fn reset_capabilities_cache() {
    terminal_image::reset_capabilities_cache();
}

/// pi's `renderImage(base64Data, imageDimensions, options)`: the protocol
/// escape sequence plus row count for the current terminal, or `null` when the
/// terminal advertises no image support.
#[napi(js_name = "renderImage")]
pub fn render_image(
    base64_data: String,
    image_dimensions: ImageDimensionsJs,
    options: Option<ImageRenderOptionsJs>,
) -> Option<RenderImageResultJs> {
    let o = options.unwrap_or_default();
    terminal_image::render_image(
        &base64_data,
        image_dimensions.into(),
        ImageRenderOptions {
            max_width_cells: o.max_width_cells,
            max_height_cells: o.max_height_cells,
            preserve_aspect_ratio: o.preserve_aspect_ratio,
            image_id: o.image_id.map(|id| id as u64),
            move_cursor: o.move_cursor,
        },
    )
    .map(Into::into)
}

/// pi's `hyperlink(text, url)`: wrap `text` in an OSC 8 hyperlink sequence.
#[napi(js_name = "hyperlink")]
pub fn hyperlink(text: String, url: String) -> String {
    terminal_image::hyperlink(&text, &url)
}

/// pi's `imageFallback(mimeType, dimensions?, filename?)`: the `[Image: ...]`
/// text placeholder for terminals without image support.
#[napi(js_name = "imageFallback")]
pub fn image_fallback(
    mime_type: String,
    dimensions: Option<ImageDimensionsJs>,
    filename: Option<String>,
) -> String {
    terminal_image::image_fallback(&mime_type, dimensions.map(Into::into), filename.as_deref())
}
