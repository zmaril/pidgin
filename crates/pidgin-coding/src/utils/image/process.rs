//! Port of pi's `utils/image-process.ts`: the public `process_image` seam that
//! normalizes an image to a supported inline format, optionally resizes it under
//! the upload budget, and returns the base64 image-block payload plus hint
//! strings.
//!
//! This is the entry the attach/paste lane calls to turn raw image bytes into a
//! base64 image block before it enters a message. The result shape and every
//! hint string mirror pi exactly.

// straitjacket-allow-file:duplication

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;

use super::convert::convert_image_bytes_to_png;
use super::resize::{format_dimension_note, resize_image};
use super::resize_core::ImageResizeOptions;

/// Options for [`process_image`], mirroring pi's `ProcessImageOptions`.
#[derive(Debug, Clone, Default)]
pub struct ProcessImageOptions {
    /// Whether to resize images to inline provider limits. Default: `true`.
    pub auto_resize_images: Option<bool>,
    /// Optional resize overrides. Uses [`resize_image`] defaults when omitted.
    pub resize_options: Option<ImageResizeOptions>,
}

/// Result of [`process_image`], mirroring pi's discriminated union
/// `{ ok: true, data, mimeType, hints } | { ok: false, message }`. `data` is the
/// base64 image-block payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessImageResult {
    /// The image was normalized (and possibly resized) successfully.
    Ok {
        /// Base64-encoded image payload.
        data: String,
        /// MIME type of `data`.
        mime_type: String,
        /// Human-readable notes (conversion + dimension hints).
        hints: Vec<String>,
    },
    /// The image could not be produced; `message` explains why.
    Err {
        /// User-facing explanation.
        message: String,
    },
}

struct NormalizedImage {
    bytes: Vec<u8>,
    mime_type: String,
    converted_from: Option<String>,
}

fn base_mime_type(mime_type: &str) -> String {
    mime_type
        .split(';')
        .next()
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_else(|| mime_type.to_lowercase())
}

fn normalize_supported_image_mime_type(mime_type: &str) -> Option<&'static str> {
    match base_mime_type(mime_type).as_str() {
        "image/png" => Some("image/png"),
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        _ => None,
    }
}

fn normalize_image(bytes: &[u8], mime_type: &str) -> Option<NormalizedImage> {
    if let Some(normalized_mime_type) = normalize_supported_image_mime_type(mime_type) {
        return Some(NormalizedImage {
            bytes: bytes.to_vec(),
            mime_type: normalized_mime_type.to_string(),
            converted_from: None,
        });
    }

    let png_bytes = convert_image_bytes_to_png(bytes)?;

    Some(NormalizedImage {
        bytes: png_bytes,
        mime_type: "image/png".to_string(),
        converted_from: Some(base_mime_type(mime_type)),
    })
}

fn conversion_hint(from: Option<&str>, to: &str) -> Option<String> {
    let from = from?;
    if from == to {
        return None;
    }
    Some(format!("[Image converted from {from} to {to}.]"))
}

/// Normalize and optionally resize an image, returning its base64 image block
/// plus hint strings. Mirrors pi's `processImage`.
pub fn process_image(
    bytes: &[u8],
    mime_type: &str,
    options: Option<&ProcessImageOptions>,
) -> ProcessImageResult {
    let auto_resize_images = options.and_then(|o| o.auto_resize_images).unwrap_or(true);
    let Some(normalized) = normalize_image(bytes, mime_type) else {
        return ProcessImageResult::Err {
            message: "[Image omitted: could not be converted to a supported inline image format.]"
                .to_string(),
        };
    };

    if auto_resize_images {
        let resize_options = options.and_then(|o| o.resize_options.as_ref());
        let Some(resized) = resize_image(&normalized.bytes, &normalized.mime_type, resize_options)
        else {
            return ProcessImageResult::Err {
                message: "[Image omitted: could not be resized below the inline image size limit.]"
                    .to_string(),
            };
        };

        let mut hints: Vec<String> = Vec::new();
        if let Some(converted_hint) =
            conversion_hint(normalized.converted_from.as_deref(), &resized.mime_type)
        {
            hints.push(converted_hint);
        }
        if let Some(dimension_note) = format_dimension_note(&resized) {
            hints.push(dimension_note);
        }

        return ProcessImageResult::Ok {
            data: resized.data,
            mime_type: resized.mime_type,
            hints,
        };
    }

    let mut hints: Vec<String> = Vec::new();
    if let Some(converted_hint) =
        conversion_hint(normalized.converted_from.as_deref(), &normalized.mime_type)
    {
        hints.push(converted_hint);
    }

    ProcessImageResult::Ok {
        data: BASE64.encode(&normalized.bytes),
        mime_type: normalized.mime_type,
        hints,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::image::{ImageEncoder, RgbaImage};

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let img = RgbaImage::from_fn(width, height, |x, y| {
            let v = ((x.wrapping_mul(37) ^ y.wrapping_mul(101)) & 0xff) as u8;
            ::image::Rgba([v, v.wrapping_add(80), v.wrapping_mul(3), 255])
        });
        let mut buf = Vec::new();
        ::image::codecs::png::PngEncoder::new(&mut buf)
            .write_image(
                img.as_raw(),
                img.width(),
                img.height(),
                ::image::ExtendedColorType::Rgba8,
            )
            .expect("encode png");
        buf
    }

    #[test]
    fn small_png_stays_under_budget_with_no_hints() {
        let bytes = png_bytes(16, 16);
        match process_image(&bytes, "image/png", None) {
            ProcessImageResult::Ok {
                data,
                mime_type,
                hints,
            } => {
                assert_eq!(mime_type, "image/png");
                assert!(hints.is_empty());
                assert_eq!(data, BASE64.encode(&bytes));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn oversized_png_emits_dimension_hint() {
        let bytes = png_bytes(3000, 1500);
        match process_image(&bytes, "image/png", None) {
            ProcessImageResult::Ok { hints, .. } => {
                assert_eq!(hints.len(), 1);
                assert!(hints[0].starts_with("[Image: original 3000x1500, displayed at 2000x1000."));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_format_is_converted_with_hint() {
        // A BMP mime is not directly supported, so it is converted to PNG. A
        // small image stays PNG through the resize (it is under budget), so the
        // final block is PNG and the conversion hint targets PNG.
        let bytes = png_bytes(16, 16);
        match process_image(&bytes, "image/bmp", None) {
            ProcessImageResult::Ok {
                mime_type, hints, ..
            } => {
                assert_eq!(mime_type, "image/png");
                assert_eq!(
                    hints,
                    vec!["[Image converted from image/bmp to image/png.]"]
                );
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn no_resize_returns_raw_base64() {
        let bytes = png_bytes(3000, 1500);
        let opts = ProcessImageOptions {
            auto_resize_images: Some(false),
            ..Default::default()
        };
        match process_image(&bytes, "image/png", Some(&opts)) {
            ProcessImageResult::Ok {
                data,
                mime_type,
                hints,
            } => {
                assert_eq!(mime_type, "image/png");
                assert!(hints.is_empty());
                assert_eq!(data, BASE64.encode(&bytes));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn undecodable_unsupported_bytes_are_omitted() {
        match process_image(&[0, 1, 2, 3], "application/octet-stream", None) {
            ProcessImageResult::Err { message } => {
                assert_eq!(
                    message,
                    "[Image omitted: could not be converted to a supported inline image format.]"
                );
            }
            other => panic!("expected Err, got {other:?}"),
        }
    }
}
