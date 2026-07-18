//! Byte-exact port of `vendor/pi/packages/tui/src/terminal-image.ts`.
//!
//! Provides the terminal-graphics helpers the [`Image`](crate::widgets::Image)
//! widget builds on: capability detection, cell/image dimension math, and the
//! Kitty / iTerm2 escape-sequence encoders. `isImageLine` and `deleteKittyImage`
//! were already ported in [`crate::renderer`]; this module reuses those instead
//! of declaring them again, and ports the remaining functions.
//!
//! Module-level mutable state in pi (`cachedCapabilities`, `cellDimensions`) is
//! modeled with `thread_local` cells so each Rust test thread is isolated while
//! preserving pi's set/reset semantics.

use std::cell::RefCell;

use base64::Engine as _;

pub use crate::renderer::delete_kitty_image;

/// Image protocol supported by the terminal (`caps.images`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    Kitty,
    ITerm2,
}

/// Terminal capabilities detected from the environment (pi's
/// `TerminalCapabilities`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCapabilities {
    pub images: Option<ImageProtocol>,
    pub true_color: bool,
    pub hyperlinks: bool,
}

/// Pixel size of a single terminal cell (pi's `CellDimensions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellDimensions {
    pub width_px: u32,
    pub height_px: u32,
}

/// Pixel size of an image (pi's `ImageDimensions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageDimensions {
    pub width_px: u32,
    pub height_px: u32,
}

/// Options for [`render_image`] (pi's `ImageRenderOptions`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ImageRenderOptions {
    pub max_width_cells: Option<u32>,
    pub max_height_cells: Option<u32>,
    pub preserve_aspect_ratio: Option<bool>,
    pub image_id: Option<u64>,
    pub move_cursor: Option<bool>,
}

const DEFAULT_CELL_DIMENSIONS: CellDimensions = CellDimensions {
    width_px: 9,
    height_px: 18,
};

thread_local! {
    static CACHED_CAPABILITIES: RefCell<Option<TerminalCapabilities>> = const { RefCell::new(None) };
    static CELL_DIMENSIONS: RefCell<CellDimensions> = const { RefCell::new(DEFAULT_CELL_DIMENSIONS) };
}

/// `getCellDimensions()`.
pub fn get_cell_dimensions() -> CellDimensions {
    CELL_DIMENSIONS.with(|c| *c.borrow())
}

/// `setCellDimensions(dims)`.
pub fn set_cell_dimensions(dims: CellDimensions) {
    CELL_DIMENSIONS.with(|c| *c.borrow_mut() = dims);
}

fn env_lower(key: &str) -> String {
    std::env::var(key).unwrap_or_default().to_lowercase()
}

fn env_present(key: &str) -> bool {
    std::env::var_os(key).is_some()
}

/// `detectCapabilities(tmuxForwardsHyperlink)`.
///
/// The `tmux_forwards_hyperlink` closure stands in for pi's default
/// `probeTmuxHyperlinks` (which shells out to `tmux display-message`).
pub fn detect_capabilities<F>(tmux_forwards_hyperlink: F) -> TerminalCapabilities
where
    F: FnOnce() -> bool,
{
    let term_program = env_lower("TERM_PROGRAM");
    let terminal_emulator = env_lower("TERMINAL_EMULATOR");
    let term = env_lower("TERM");
    let color_term = env_lower("COLORTERM");
    let has_true_color_hint = color_term == "truecolor" || color_term == "24bit";

    if env_present("TMUX") || term.starts_with("tmux") {
        return TerminalCapabilities {
            images: None,
            true_color: has_true_color_hint,
            hyperlinks: tmux_forwards_hyperlink(),
        };
    }

    if term.starts_with("screen") {
        return TerminalCapabilities {
            images: None,
            true_color: has_true_color_hint,
            hyperlinks: false,
        };
    }

    if env_present("KITTY_WINDOW_ID") || term_program == "kitty" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    if term_program == "ghostty" || term.contains("ghostty") || env_present("GHOSTTY_RESOURCES_DIR")
    {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    if env_present("WEZTERM_PANE") || term_program == "wezterm" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    if term_program == "warpterminal"
        || env_present("WARP_SESSION_ID")
        || env_present("WARP_TERMINAL_SESSION_UUID")
    {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    let is_iterm_program = term_program == "iterm.app"; // codespell:ignore iterm
    if env_present("ITERM_SESSION_ID") || is_iterm_program {
        return TerminalCapabilities {
            images: Some(ImageProtocol::ITerm2),
            true_color: true,
            hyperlinks: true,
        };
    }

    if env_present("WT_SESSION") {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: true,
        };
    }

    if term_program == "vscode" {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: true,
        };
    }

    if term_program == "alacritty" {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: true,
        };
    }

    if terminal_emulator == "jetbrains-jediterm" {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: false,
        };
    }

    TerminalCapabilities {
        images: None,
        true_color: has_true_color_hint,
        hyperlinks: false,
    }
}

/// `getCapabilities()` — detect-and-cache on first access.
pub fn get_capabilities() -> TerminalCapabilities {
    CACHED_CAPABILITIES.with(|c| {
        let mut slot = c.borrow_mut();
        if slot.is_none() {
            *slot = Some(detect_capabilities(|| false));
        }
        slot.unwrap()
    })
}

/// `resetCapabilitiesCache()`.
pub fn reset_capabilities_cache() {
    CACHED_CAPABILITIES.with(|c| *c.borrow_mut() = None);
}

/// `setCapabilities(caps)` — override the cached capabilities.
pub fn set_capabilities(caps: TerminalCapabilities) {
    CACHED_CAPABILITIES.with(|c| *c.borrow_mut() = Some(caps));
}

/// `allocateImageId()` — a random ID in `[1, 0xffffffff]`.
///
/// Non-deterministic like pi (`Math.random`). Deterministic vectors always pass
/// an explicit `image_id` so this is never on the asserted path.
pub fn allocate_image_id() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    // Mimic `Math.floor(Math.random() * 0xfffffffe) + 1` range.
    (nanos % 0xffff_fffe) + 1
}

const CHUNK_SIZE: usize = 4096;

/// Options for [`encode_kitty`].
#[derive(Debug, Clone, Copy, Default)]
pub struct KittyOptions {
    pub columns: Option<u32>,
    pub rows: Option<u32>,
    pub image_id: Option<u64>,
    pub move_cursor: Option<bool>,
}

/// `encodeKitty(base64Data, options)`.
pub fn encode_kitty(base64_data: &str, options: KittyOptions) -> String {
    let mut params: Vec<String> = vec!["a=T".to_string(), "f=100".to_string(), "q=2".to_string()];

    if options.move_cursor == Some(false) {
        params.push("C=1".to_string());
    }
    if let Some(columns) = options.columns {
        params.push(format!("c={columns}"));
    }
    if let Some(rows) = options.rows {
        params.push(format!("r={rows}"));
    }
    if let Some(image_id) = options.image_id {
        params.push(format!("i={image_id}"));
    }

    if base64_data.len() <= CHUNK_SIZE {
        return format!("\x1b_G{};{base64_data}\x1b\\", params.join(","));
    }

    let mut chunks: Vec<String> = Vec::new();
    let bytes = base64_data.as_bytes();
    let mut offset = 0usize;
    let mut is_first = true;

    while offset < bytes.len() {
        let end = (offset + CHUNK_SIZE).min(bytes.len());
        let chunk = &base64_data[offset..end];
        let is_last = offset + CHUNK_SIZE >= bytes.len();

        if is_first {
            chunks.push(format!("\x1b_G{},m=1;{chunk}\x1b\\", params.join(",")));
            is_first = false;
        } else if is_last {
            chunks.push(format!("\x1b_Gm=0;{chunk}\x1b\\"));
        } else {
            chunks.push(format!("\x1b_Gm=1;{chunk}\x1b\\"));
        }

        offset += CHUNK_SIZE;
    }

    chunks.join("")
}

/// `deleteAllKittyImages()`.
pub fn delete_all_kitty_images() -> String {
    "\x1b_Ga=d,d=A,q=2\x1b\\".to_string()
}

/// A dimension value for iTerm2 (`width`/`height`), which may be a cell count or
/// a keyword like `"auto"`.
#[derive(Debug, Clone)]
pub enum ITerm2Dimension {
    Cells(u32),
    Keyword(String),
}

impl std::fmt::Display for ITerm2Dimension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ITerm2Dimension::Cells(n) => write!(f, "{n}"),
            ITerm2Dimension::Keyword(s) => write!(f, "{s}"),
        }
    }
}

/// Options for [`encode_iterm2`].
#[derive(Debug, Clone, Default)]
pub struct ITerm2Options {
    pub width: Option<ITerm2Dimension>,
    pub height: Option<ITerm2Dimension>,
    pub name: Option<String>,
    pub preserve_aspect_ratio: Option<bool>,
    pub inline: Option<bool>,
}

/// `encodeITerm2(base64Data, options)`.
pub fn encode_iterm2(base64_data: &str, options: &ITerm2Options) -> String {
    let inline_val = if options.inline != Some(false) { 1 } else { 0 };
    let mut params: Vec<String> = vec![format!("inline={inline_val}")];

    if let Some(width) = &options.width {
        params.push(format!("width={width}"));
    }
    if let Some(height) = &options.height {
        params.push(format!("height={height}"));
    }
    if let Some(name) = &options.name {
        let name_base64 = base64::engine::general_purpose::STANDARD.encode(name.as_bytes());
        params.push(format!("name={name_base64}"));
    }
    if options.preserve_aspect_ratio == Some(false) {
        params.push("preserveAspectRatio=0".to_string());
    }

    format!("\x1b]1337;File={}:{base64_data}\x07", params.join(";"))
}

/// Cell footprint of an image (pi's `ImageCellSize`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageCellSize {
    pub columns: u32,
    pub rows: u32,
}

/// `calculateImageCellSize(imageDimensions, maxWidthCells, maxHeightCells?, cellDimensions?)`.
pub fn calculate_image_cell_size(
    image_dimensions: ImageDimensions,
    max_width_cells: f64,
    max_height_cells: Option<f64>,
    cell_dimensions: CellDimensions,
) -> ImageCellSize {
    let max_width = (max_width_cells.floor()).max(1.0);
    let max_height = max_height_cells.map(|h| h.floor().max(1.0));
    let image_width = (image_dimensions.width_px as f64).max(1.0);
    let image_height = (image_dimensions.height_px as f64).max(1.0);

    let cell_w = cell_dimensions.width_px as f64;
    let cell_h = cell_dimensions.height_px as f64;

    let width_scale = (max_width * cell_w) / image_width;
    let height_scale = match max_height {
        None => width_scale,
        Some(mh) => (mh * cell_h) / image_height,
    };
    let scale = width_scale.min(height_scale);

    let scaled_width_px = image_width * scale;
    let scaled_height_px = image_height * scale;
    let columns = (scaled_width_px / cell_w).ceil();
    let rows = (scaled_height_px / cell_h).ceil();

    let out_columns = max_width.min(columns).max(1.0);
    let out_rows = match max_height {
        None => rows.max(1.0),
        Some(mh) => mh.min(rows).max(1.0),
    };

    ImageCellSize {
        columns: out_columns as u32,
        rows: out_rows as u32,
    }
}

/// `calculateImageRows(imageDimensions, targetWidthCells, cellDimensions?)`.
pub fn calculate_image_rows(
    image_dimensions: ImageDimensions,
    target_width_cells: f64,
    cell_dimensions: CellDimensions,
) -> u32 {
    calculate_image_cell_size(image_dimensions, target_width_cells, None, cell_dimensions).rows
}

fn decode_base64(base64_data: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(base64_data.as_bytes())
        .ok()
}

fn read_u32_be(buf: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

fn read_u16_be(buf: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([buf[offset], buf[offset + 1]])
}

fn read_u16_le(buf: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
}

fn read_u32_le(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

/// `getPngDimensions(base64Data)`.
pub fn get_png_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data)?;
    if buffer.len() < 24 {
        return None;
    }
    if buffer[0] != 0x89 || buffer[1] != 0x50 || buffer[2] != 0x4e || buffer[3] != 0x47 {
        return None;
    }
    let width = read_u32_be(&buffer, 16);
    let height = read_u32_be(&buffer, 20);
    Some(ImageDimensions {
        width_px: width,
        height_px: height,
    })
}

/// `getJpegDimensions(base64Data)`.
pub fn get_jpeg_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data)?;
    if buffer.len() < 2 {
        return None;
    }
    if buffer[0] != 0xff || buffer[1] != 0xd8 {
        return None;
    }

    let mut offset = 2usize;
    while offset + 9 < buffer.len() {
        if buffer[offset] != 0xff {
            offset += 1;
            continue;
        }

        let marker = buffer[offset + 1];

        if (0xc0..=0xc2).contains(&marker) {
            let height = read_u16_be(&buffer, offset + 5);
            let width = read_u16_be(&buffer, offset + 7);
            return Some(ImageDimensions {
                width_px: width as u32,
                height_px: height as u32,
            });
        }

        if offset + 3 >= buffer.len() {
            return None;
        }
        let length = read_u16_be(&buffer, offset + 2);
        if length < 2 {
            return None;
        }
        offset += 2 + length as usize;
    }

    None
}

/// `getGifDimensions(base64Data)`.
pub fn get_gif_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data)?;
    if buffer.len() < 10 {
        return None;
    }
    let sig = &buffer[0..6];
    if sig != b"GIF87a" && sig != b"GIF89a" {
        return None;
    }
    let width = read_u16_le(&buffer, 6);
    let height = read_u16_le(&buffer, 8);
    Some(ImageDimensions {
        width_px: width as u32,
        height_px: height as u32,
    })
}

/// `getWebpDimensions(base64Data)`.
pub fn get_webp_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data)?;
    if buffer.len() < 30 {
        return None;
    }
    if &buffer[0..4] != b"RIFF" || &buffer[8..12] != b"WEBP" {
        return None;
    }

    let chunk = &buffer[12..16];
    if chunk == b"VP8 " {
        if buffer.len() < 30 {
            return None;
        }
        let width = read_u16_le(&buffer, 26) & 0x3fff;
        let height = read_u16_le(&buffer, 28) & 0x3fff;
        Some(ImageDimensions {
            width_px: width as u32,
            height_px: height as u32,
        })
    } else if chunk == b"VP8L" {
        if buffer.len() < 25 {
            return None;
        }
        let bits = read_u32_le(&buffer, 21);
        let width = (bits & 0x3fff) + 1;
        let height = ((bits >> 14) & 0x3fff) + 1;
        Some(ImageDimensions {
            width_px: width,
            height_px: height,
        })
    } else if chunk == b"VP8X" {
        if buffer.len() < 30 {
            return None;
        }
        let width =
            ((buffer[24] as u32) | ((buffer[25] as u32) << 8) | ((buffer[26] as u32) << 16)) + 1;
        let height =
            ((buffer[27] as u32) | ((buffer[28] as u32) << 8) | ((buffer[29] as u32) << 16)) + 1;
        Some(ImageDimensions {
            width_px: width,
            height_px: height,
        })
    } else {
        None
    }
}

/// `getImageDimensions(base64Data, mimeType)`.
pub fn get_image_dimensions(base64_data: &str, mime_type: &str) -> Option<ImageDimensions> {
    match mime_type {
        "image/png" => get_png_dimensions(base64_data),
        "image/jpeg" => get_jpeg_dimensions(base64_data),
        "image/gif" => get_gif_dimensions(base64_data),
        "image/webp" => get_webp_dimensions(base64_data),
        _ => None,
    }
}

/// Result of [`render_image`] (pi's inline object literal).
#[derive(Debug, Clone)]
pub struct RenderImageResult {
    pub sequence: String,
    pub rows: u32,
    pub image_id: Option<u64>,
}

/// `renderImage(base64Data, imageDimensions, options)`.
pub fn render_image(
    base64_data: &str,
    image_dimensions: ImageDimensions,
    options: ImageRenderOptions,
) -> Option<RenderImageResult> {
    let caps = get_capabilities();
    let protocol = caps.images?;

    let max_width = options.max_width_cells.unwrap_or(80) as f64;
    let size = calculate_image_cell_size(
        image_dimensions,
        max_width,
        options.max_height_cells.map(|h| h as f64),
        get_cell_dimensions(),
    );

    match protocol {
        ImageProtocol::Kitty => {
            let sequence = encode_kitty(
                base64_data,
                KittyOptions {
                    columns: Some(size.columns),
                    rows: Some(size.rows),
                    image_id: options.image_id,
                    move_cursor: options.move_cursor,
                },
            );
            Some(RenderImageResult {
                sequence,
                rows: size.rows,
                image_id: options.image_id,
            })
        }
        ImageProtocol::ITerm2 => {
            let sequence = encode_iterm2(
                base64_data,
                &ITerm2Options {
                    width: Some(ITerm2Dimension::Cells(size.columns)),
                    height: Some(ITerm2Dimension::Keyword("auto".to_string())),
                    preserve_aspect_ratio: Some(options.preserve_aspect_ratio.unwrap_or(true)),
                    ..Default::default()
                },
            );
            Some(RenderImageResult {
                sequence,
                rows: size.rows,
                image_id: None,
            })
        }
    }
}

/// `hyperlink(text, url)` — wrap text in an OSC 8 hyperlink sequence.
pub fn hyperlink(text: &str, url: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

/// `imageFallback(mimeType, dimensions?, filename?)`.
pub fn image_fallback(
    mime_type: &str,
    dimensions: Option<ImageDimensions>,
    filename: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(filename) = filename {
        parts.push(filename.to_string());
    }
    parts.push(format!("[{mime_type}]"));
    if let Some(dims) = dimensions {
        parts.push(format!("{}x{}", dims.width_px, dims.height_px));
    }
    format!("[Image: {}]", parts.join(" "))
}
