//! Port of pi's `utils/image-convert.ts`: decode arbitrary image bytes into a
//! PNG buffer (`convert_image_bytes_to_png`) or a base64 PNG payload
//! (`convert_to_png`).
//!
//! pi routes both through Photon (`new_from_byteslice` -> `applyExifOrientation`
//! -> `get_bytes`); this port uses the `image` crate for decode + PNG encode
//! and reuses [`crate::utils::exif::apply_exif_orientation`] for the EXIF pixel
//! transform. `convert_to_png` supports the terminal-display path (Kitty
//! graphics `f=100` requires PNG) and BMP -> PNG normalization.

// straitjacket-allow-file:duplication

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;

use super::decode_and_orient;

/// A base64 PNG conversion result, mirroring pi's `convertToPng` return shape
/// `{ data, mimeType }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertedImage {
    /// Base64-encoded image payload.
    pub data: String,
    /// MIME type of `data` (always `image/png` from `convert_to_png`).
    pub mime_type: String,
}

/// Decode image bytes, apply EXIF orientation, and re-encode as PNG. Returns
/// `None` when the bytes cannot be decoded or encoded, mirroring pi's `null`.
pub fn convert_image_bytes_to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let image = decode_and_orient(bytes)?;
    super::resize_core::encode_png(&image)
}

/// Convert a base64 image to a base64 PNG for terminal display. Already-PNG
/// input is returned unchanged, mirroring pi's early return.
pub fn convert_to_png(base64_data: &str, mime_type: &str) -> Option<ConvertedImage> {
    // Already PNG, no conversion needed.
    if mime_type == "image/png" {
        return Some(ConvertedImage {
            data: base64_data.to_string(),
            mime_type: mime_type.to_string(),
        });
    }

    let bytes = BASE64.decode(base64_data).ok()?;
    let png_bytes = convert_image_bytes_to_png(&bytes)?;

    Some(ConvertedImage {
        data: BASE64.encode(png_bytes),
        mime_type: "image/png".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::image::{ImageEncoder, RgbaImage};

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let img = RgbaImage::from_fn(width, height, |x, y| {
            ::image::Rgba([x as u8, y as u8, 128, 255])
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
    fn converts_png_bytes_to_png() {
        let out = convert_image_bytes_to_png(&png_bytes(4, 4)).expect("converts");
        // PNG magic number.
        assert_eq!(&out[..8], &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    }

    #[test]
    fn convert_to_png_passes_through_existing_png() {
        let out = convert_to_png("aGVsbG8=", "image/png").expect("passthrough");
        assert_eq!(out.data, "aGVsbG8=");
        assert_eq!(out.mime_type, "image/png");
    }

    #[test]
    fn convert_to_png_reencodes_non_png() {
        let base64 = BASE64.encode(png_bytes(3, 3));
        let out = convert_to_png(&base64, "image/bmp").expect("reencodes");
        assert_eq!(out.mime_type, "image/png");
        let decoded = BASE64.decode(out.data).expect("valid base64");
        assert_eq!(
            &decoded[..8],
            &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]
        );
    }

    #[test]
    fn returns_none_for_undecodable_bytes() {
        assert_eq!(convert_image_bytes_to_png(&[0, 1, 2, 3]), None);
    }
}
