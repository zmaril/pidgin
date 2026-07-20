//! Port of pi's `utils/image-resize-core.ts`: the decode -> EXIF -> Lanczos3
//! resize -> encode loop that shrinks an image until its base64 payload fits
//! under the inline upload budget.
//!
//! pi drives this through Photon (`resize` with `SamplingFilter.Lanczos3`,
//! `get_bytes` for PNG, `get_bytes_jpeg(quality)` for JPEG). This port uses the
//! `image` crate's `imageops::resize` with `FilterType::Lanczos3` and encodes
//! PNG/JPEG through the crate's codecs. The budget constant, quality list, and
//! `0.75` shrink factor mirror pi exactly.

// straitjacket-allow-file:duplication

use ::image::imageops::FilterType;
use ::image::{DynamicImage, ImageEncoder, RgbaImage};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;

use super::decode_and_orient;

/// Resize overrides, mirroring pi's `ImageResizeOptions`. Unset fields fall back
/// to [`resolve_options`]'s defaults (max 2000x2000, 4.5MB base64, JPEG q80).
#[derive(Debug, Clone, Default)]
pub struct ImageResizeOptions {
    /// Max width in pixels. Default: 2000.
    pub max_width: Option<u32>,
    /// Max height in pixels. Default: 2000.
    pub max_height: Option<u32>,
    /// Max base64 payload size in bytes. Default: 4.5MB.
    pub max_bytes: Option<usize>,
    /// Initial JPEG quality tried first. Default: 80.
    pub jpeg_quality: Option<u8>,
}

/// A successfully resized image, mirroring pi's `ResizedImage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResizedImage {
    /// Base64-encoded image payload.
    pub data: String,
    /// MIME type of `data` (`image/png` or `image/jpeg`).
    pub mime_type: String,
    /// Width of the source image after EXIF orientation.
    pub original_width: u32,
    /// Height of the source image after EXIF orientation.
    pub original_height: u32,
    /// Width of the encoded (possibly downscaled) image.
    pub width: u32,
    /// Height of the encoded (possibly downscaled) image.
    pub height: u32,
    /// Whether the image was downscaled/re-encoded to fit the budget.
    pub was_resized: bool,
}

// 4.5MB of base64 payload. Provides headroom below Anthropic's 5MB limit.
const DEFAULT_MAX_BYTES: usize = (4.5 * 1024.0 * 1024.0) as usize;
const DEFAULT_MAX_WIDTH: u32 = 2000;
const DEFAULT_MAX_HEIGHT: u32 = 2000;
const DEFAULT_JPEG_QUALITY: u8 = 80;

struct ResolvedOptions {
    max_width: u32,
    max_height: u32,
    max_bytes: usize,
    jpeg_quality: u8,
}

fn resolve_options(options: Option<&ImageResizeOptions>) -> ResolvedOptions {
    ResolvedOptions {
        max_width: options
            .and_then(|o| o.max_width)
            .unwrap_or(DEFAULT_MAX_WIDTH),
        max_height: options
            .and_then(|o| o.max_height)
            .unwrap_or(DEFAULT_MAX_HEIGHT),
        max_bytes: options
            .and_then(|o| o.max_bytes)
            .unwrap_or(DEFAULT_MAX_BYTES),
        jpeg_quality: options
            .and_then(|o| o.jpeg_quality)
            .unwrap_or(DEFAULT_JPEG_QUALITY),
    }
}

struct EncodedCandidate {
    data: String,
    encoded_size: usize,
    mime_type: &'static str,
}

fn encode_candidate(buffer: &[u8], mime_type: &'static str) -> EncodedCandidate {
    let data = BASE64.encode(buffer);
    let encoded_size = data.len();
    EncodedCandidate {
        data,
        encoded_size,
        mime_type,
    }
}

/// Encode an RGBA image as PNG. Shared with [`super::convert`].
pub(super) fn encode_png(image: &RgbaImage) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    ::image::codecs::png::PngEncoder::new(&mut buf)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ::image::ExtendedColorType::Rgba8,
        )
        .ok()?;
    Some(buf)
}

/// Encode an RGB image as JPEG at the given quality (alpha is dropped, matching
/// Photon's `get_bytes_jpeg`).
fn encode_jpeg(image: &::image::RgbImage, quality: u8) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality)
        .encode_image(image)
        .ok()?;
    Some(buf)
}

/// Resize once and encode a PNG plus one JPEG per quality step, mirroring pi's
/// `tryEncodings`. Returns `None` if any encode fails (pi's outer try/catch
/// treats an encode throw as an overall `null`).
fn try_encodings(
    image: &RgbaImage,
    width: u32,
    height: u32,
    jpeg_qualities: &[u8],
) -> Option<Vec<EncodedCandidate>> {
    let resized = ::image::imageops::resize(image, width, height, FilterType::Lanczos3);

    let mut candidates = vec![encode_candidate(&encode_png(&resized)?, "image/png")];

    let rgb = DynamicImage::ImageRgba8(resized).into_rgb8();
    for &quality in jpeg_qualities {
        candidates.push(encode_candidate(&encode_jpeg(&rgb, quality)?, "image/jpeg"));
    }
    Some(candidates)
}

/// Resize an image to fit within the max dimensions and encoded size budget.
/// Returns `None` if the image cannot be decoded or cannot be shrunk below
/// `max_bytes`, mirroring pi's `resizeImageInProcess`.
///
/// Strategy for staying under `max_bytes`:
/// 1. Resize to `max_width`/`max_height`.
/// 2. Try PNG and JPEG at each quality step; pick the first under budget.
/// 3. If still too large, shrink dimensions by `0.75` and retry, down to 1x1.
pub fn resize_image_in_process(
    input_bytes: &[u8],
    mime_type: &str,
    options: Option<&ImageResizeOptions>,
) -> Option<ResizedImage> {
    let opts = resolve_options(options);
    let input_base64_size = input_bytes.len().div_ceil(3) * 4;

    let image = decode_and_orient(input_bytes)?;

    let original_width = image.width();
    let original_height = image.height();
    let format = mime_type.split('/').nth(1).unwrap_or("png");

    // Check if already within all limits (dimensions AND encoded size).
    if original_width <= opts.max_width
        && original_height <= opts.max_height
        && input_base64_size < opts.max_bytes
    {
        let resolved_mime = if mime_type.is_empty() {
            format!("image/{format}")
        } else {
            mime_type.to_string()
        };
        return Some(ResizedImage {
            data: BASE64.encode(input_bytes),
            mime_type: resolved_mime,
            original_width,
            original_height,
            width: original_width,
            height: original_height,
            was_resized: false,
        });
    }

    // Calculate initial dimensions respecting max limits.
    let mut target_width = original_width;
    let mut target_height = original_height;

    if target_width > opts.max_width {
        target_height = ((target_height as f64) * (opts.max_width as f64) / (target_width as f64))
            .round() as u32;
        target_width = opts.max_width;
    }
    if target_height > opts.max_height {
        target_width = ((target_width as f64) * (opts.max_height as f64) / (target_height as f64))
            .round() as u32;
        target_height = opts.max_height;
    }

    // Deduplicate quality steps, preserving insertion order (pi uses a Set).
    let mut quality_steps: Vec<u8> = Vec::new();
    for quality in [opts.jpeg_quality, 85, 70, 55, 40] {
        if !quality_steps.contains(&quality) {
            quality_steps.push(quality);
        }
    }

    let mut current_width = target_width;
    let mut current_height = target_height;

    loop {
        let candidates = try_encodings(&image, current_width, current_height, &quality_steps)?;
        for candidate in &candidates {
            if candidate.encoded_size < opts.max_bytes {
                return Some(ResizedImage {
                    data: candidate.data.clone(),
                    mime_type: candidate.mime_type.to_string(),
                    original_width,
                    original_height,
                    width: current_width,
                    height: current_height,
                    was_resized: true,
                });
            }
        }

        if current_width == 1 && current_height == 1 {
            break;
        }

        let next_width = if current_width == 1 {
            1
        } else {
            ((current_width as f64) * 0.75).floor().max(1.0) as u32
        };
        let next_height = if current_height == 1 {
            1
        } else {
            ((current_height as f64) * 0.75).floor().max(1.0) as u32
        };
        if next_width == current_width && next_height == current_height {
            break;
        }

        current_width = next_width;
        current_height = next_height;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        // A noisy pattern so JPEG cannot trivially compress it away.
        let img = RgbaImage::from_fn(width, height, |x, y| {
            let v = ((x.wrapping_mul(37) ^ y.wrapping_mul(101)) & 0xff) as u8;
            ::image::Rgba([v, v.wrapping_add(80), v.wrapping_mul(3), 255])
        });
        encode_png(&img).expect("encode png")
    }

    #[test]
    fn small_image_within_budget_is_not_resized() {
        let bytes = png_bytes(10, 10);
        let out = resize_image_in_process(&bytes, "image/png", None).expect("resizes");
        assert!(!out.was_resized);
        assert_eq!(out.mime_type, "image/png");
        assert_eq!(out.width, 10);
        assert_eq!(out.height, 10);
        assert_eq!(out.original_width, 10);
        assert_eq!(out.original_height, 10);
        // Passthrough returns the exact original bytes as base64.
        assert_eq!(out.data, BASE64.encode(&bytes));
    }

    #[test]
    fn oversized_dimensions_are_downscaled() {
        // 3000x1500 exceeds the 2000 max width, forcing a resize.
        let bytes = png_bytes(3000, 1500);
        let out = resize_image_in_process(&bytes, "image/png", None).expect("resizes");
        assert!(out.was_resized);
        assert_eq!(out.original_width, 3000);
        assert_eq!(out.original_height, 1500);
        // Aspect-preserving downscale to the 2000 max width: 1500 * 2000 / 3000.
        assert_eq!(out.width, 2000);
        assert_eq!(out.height, 1000);
        assert!(out.data.len() < DEFAULT_MAX_BYTES);
    }

    #[test]
    fn tight_byte_budget_forces_quality_loop_shrink() {
        // A large noisy image with a tiny budget: the loop must shrink well
        // below the 2000 max dimension to fit, and still succeed.
        let bytes = png_bytes(2000, 2000);
        let opts = ImageResizeOptions {
            max_bytes: Some(20_000),
            ..Default::default()
        };
        let out = resize_image_in_process(&bytes, "image/png", Some(&opts)).expect("resizes");
        assert!(out.was_resized);
        assert!(out.width < 2000);
        assert!(out.height < 2000);
        assert!(out.data.len() < 20_000);
    }

    #[test]
    fn impossible_budget_returns_none() {
        // No encoding of any image (down to 1x1) fits in 4 base64 bytes.
        let bytes = png_bytes(64, 64);
        let opts = ImageResizeOptions {
            max_bytes: Some(4),
            ..Default::default()
        };
        assert_eq!(
            resize_image_in_process(&bytes, "image/png", Some(&opts)),
            None
        );
    }

    #[test]
    fn undecodable_bytes_return_none() {
        assert_eq!(
            resize_image_in_process(&[0, 1, 2, 3], "image/png", None),
            None
        );
    }
}
