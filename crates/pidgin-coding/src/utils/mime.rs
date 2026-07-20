//! Sniff a supported image MIME type from magic bytes.
//!
//! Ported from pi's `utils/mime.ts`. Recognizes JPEG, PNG, GIF, WEBP, and BMP
//! from their leading bytes. Animated PNGs (an `acTL` chunk appearing before
//! the first `IDAT`) are rejected, as is a fractional/CMYK JPEG variant
//! (`FF D8 FF F7`). BMP headers are validated (declared file size, pixel-data
//! offset, DIB header size, single color plane, and a supported bit depth).
//!
//! pi's file-reading wrapper (`detectSupportedImageMimeTypeFromFile`) is not
//! ported here; callers can read the header bytes and pass them in.

use super::bytes::{read_u16_le, read_u32_be, read_u32_le, starts_with, starts_with_ascii};

const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

/// Detect a supported image MIME type from a byte prefix, or `None`.
pub fn detect_supported_image_mime_type(buffer: &[u8]) -> Option<&'static str> {
    if starts_with(buffer, &[0xff, 0xd8, 0xff]) {
        return if buffer.get(3) == Some(&0xf7) {
            None
        } else {
            Some("image/jpeg")
        };
    }
    if starts_with(buffer, &PNG_SIGNATURE) {
        return if is_png(buffer) && !is_animated_png(buffer) {
            Some("image/png")
        } else {
            None
        };
    }
    if starts_with_ascii(buffer, 0, "GIF") {
        return Some("image/gif");
    }
    if starts_with_ascii(buffer, 0, "RIFF") && starts_with_ascii(buffer, 8, "WEBP") {
        return Some("image/webp");
    }
    if starts_with_ascii(buffer, 0, "BM") && is_bmp(buffer) {
        return Some("image/bmp");
    }
    None
}

fn is_png(buffer: &[u8]) -> bool {
    buffer.len() >= 16
        && read_u32_be(buffer, PNG_SIGNATURE.len()) == 13
        && starts_with_ascii(buffer, 12, "IHDR")
}

fn is_animated_png(buffer: &[u8]) -> bool {
    let mut offset = PNG_SIGNATURE.len();
    while offset + 8 <= buffer.len() {
        let chunk_length = read_u32_be(buffer, offset) as usize;
        let chunk_type_offset = offset + 4;
        if starts_with_ascii(buffer, chunk_type_offset, "acTL") {
            return true;
        }
        if starts_with_ascii(buffer, chunk_type_offset, "IDAT") {
            return false;
        }

        let next_offset = offset + 8 + chunk_length + 4;
        if next_offset <= offset || next_offset > buffer.len() {
            return false;
        }
        offset = next_offset;
    }
    false
}

fn is_bmp(buffer: &[u8]) -> bool {
    if buffer.len() < 26 {
        return false;
    }

    let declared_file_size = read_u32_le(buffer, 2);
    let pixel_data_offset = read_u32_le(buffer, 10);
    let dib_header_size = read_u32_le(buffer, 14);
    if declared_file_size != 0 && declared_file_size < 26 {
        return false;
    }
    if pixel_data_offset < 14 + dib_header_size {
        return false;
    }
    if declared_file_size != 0 && pixel_data_offset >= declared_file_size {
        return false;
    }

    let (color_planes, bits_per_pixel) = if dib_header_size == 12 {
        (read_u16_le(buffer, 22), read_u16_le(buffer, 24))
    } else if (40..=124).contains(&dib_header_size) {
        if buffer.len() < 30 {
            return false;
        }
        (read_u16_le(buffer, 26), read_u16_le(buffer, 28))
    } else {
        return false;
    };

    color_planes == 1 && matches!(bits_per_pixel, 1 | 4 | 8 | 16 | 24 | 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the 1x1 red 24bpp BMP that pi's image-process test constructs.
    fn tiny_bmp_1x1_red_24bpp() -> Vec<u8> {
        let mut buffer = vec![0u8; 58];
        let file_size = buffer.len() as u32;
        buffer[0] = b'B';
        buffer[1] = b'M';
        buffer[2..6].copy_from_slice(&file_size.to_le_bytes());
        buffer[10..14].copy_from_slice(&54u32.to_le_bytes());
        buffer[14..18].copy_from_slice(&40u32.to_le_bytes());
        buffer[18..22].copy_from_slice(&1i32.to_le_bytes());
        buffer[22..26].copy_from_slice(&1i32.to_le_bytes());
        buffer[26..28].copy_from_slice(&1u16.to_le_bytes());
        buffer[28..30].copy_from_slice(&24u16.to_le_bytes());
        buffer[30..34].copy_from_slice(&0u32.to_le_bytes());
        buffer[34..38].copy_from_slice(&4u32.to_le_bytes());
        buffer[56] = 0xff;
        buffer
    }

    fn base_png(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut out = PNG_SIGNATURE.to_vec();
        for (chunk_type, data) in chunks {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(*chunk_type);
            out.extend_from_slice(data);
            out.extend_from_slice(&[0, 0, 0, 0]); // CRC placeholder
        }
        out
    }

    #[test]
    fn detects_bmp_from_magic_bytes() {
        assert_eq!(
            detect_supported_image_mime_type(&tiny_bmp_1x1_red_24bpp()),
            Some("image/bmp")
        );
    }

    #[test]
    fn detects_jpeg() {
        let bytes = [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];
        assert_eq!(detect_supported_image_mime_type(&bytes), Some("image/jpeg"));
    }

    #[test]
    fn rejects_fractional_jpeg_variant() {
        let bytes = [0xff, 0xd8, 0xff, 0xf7];
        assert_eq!(detect_supported_image_mime_type(&bytes), None);
    }

    #[test]
    fn detects_static_png() {
        // IHDR (13-byte header) followed by IDAT.
        let ihdr = [0u8; 13];
        let idat = [0u8; 4];
        let png = base_png(&[(b"IHDR", &ihdr), (b"IDAT", &idat)]);
        assert_eq!(detect_supported_image_mime_type(&png), Some("image/png"));
    }

    #[test]
    fn rejects_animated_png() {
        let ihdr = [0u8; 13];
        let actl = [0u8; 8];
        let idat = [0u8; 4];
        // acTL appears before IDAT, marking the PNG as animated.
        let png = base_png(&[(b"IHDR", &ihdr), (b"acTL", &actl), (b"IDAT", &idat)]);
        assert_eq!(detect_supported_image_mime_type(&png), None);
    }

    #[test]
    fn detects_gif() {
        let bytes = b"GIF89a";
        assert_eq!(detect_supported_image_mime_type(bytes), Some("image/gif"));
    }

    #[test]
    fn detects_webp() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(b"WEBP");
        assert_eq!(detect_supported_image_mime_type(&bytes), Some("image/webp"));
    }

    #[test]
    fn returns_none_for_unknown_bytes() {
        assert_eq!(detect_supported_image_mime_type(&[0, 1, 2, 3, 4]), None);
    }
}
