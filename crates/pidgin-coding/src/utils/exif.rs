//! Read the EXIF orientation tag from JPEG and WebP byte buffers, and apply the
//! corresponding pixel rotate/flip transform.
//!
//! Ported from pi's `utils/exif-orientation.ts`. The pure byte-parsing reader
//! (`getExifOrientation`) locates the EXIF/TIFF block in a JPEG (APP1 segment)
//! or WebP (`EXIF` chunk), reads the TIFF IFD, and returns the orientation tag
//! (`0x0112`) as a value in `1..=8`.
//!
//! [`apply_exif_orientation`] mirrors pi's `applyExifOrientation` (and its
//! `rotate90` helper): it physically rotates/flips the decoded pixels so the
//! image reads upright. pi drives the Photon WASM library's `fliph`/`flipv` and
//! a hand-rolled `rotate90` over raw RGBA pixel buffers; this port applies the
//! byte-identical geometric transform through the `image` crate's
//! `imageops::{flip_horizontal, flip_vertical, rotate90, rotate270}` — pi's
//! clockwise `rotate90` matches `imageops::rotate90`, and its counter-clockwise
//! variant matches `imageops::rotate270`.
//!
//! Adaptation: pi returns the number `1` as a default when no EXIF orientation
//! is present. This port returns `Option<u8>`, using `None` for buffers with no
//! usable EXIF orientation block (equivalent to pi's default of `1`, which
//! callers treat as "no transform").

use super::bytes::{byte_at, read_u16_be, read_u16_le, read_u32_be, read_u32_le};
use ::image::{imageops, RgbaImage};

fn has_exif_header(bytes: &[u8], offset: usize) -> bool {
    bytes.get(offset..offset + 6) == Some(&b"Exif\0\0"[..])
}

fn read_orientation_from_tiff(bytes: &[u8], tiff_start: usize) -> u8 {
    if tiff_start + 8 > bytes.len() {
        return 1;
    }

    let byte_order = (byte_at(bytes, tiff_start) << 8) | byte_at(bytes, tiff_start + 1);
    let le = byte_order == 0x4949;

    let read16 = |pos: usize| -> u32 {
        if le {
            read_u16_le(bytes, pos)
        } else {
            read_u16_be(bytes, pos)
        }
    };
    let read32 = |pos: usize| -> u32 {
        if le {
            read_u32_le(bytes, pos)
        } else {
            read_u32_be(bytes, pos)
        }
    };

    let ifd_offset = read32(tiff_start + 4) as usize;
    let ifd_start = tiff_start + ifd_offset;
    if ifd_start + 2 > bytes.len() {
        return 1;
    }

    let entry_count = read16(ifd_start);
    for i in 0..entry_count as usize {
        let entry_pos = ifd_start + 2 + i * 12;
        if entry_pos + 12 > bytes.len() {
            return 1;
        }
        if read16(entry_pos) == 0x0112 {
            let value = read16(entry_pos + 8);
            return if (1..=8).contains(&value) {
                value as u8
            } else {
                1
            };
        }
    }

    1
}

fn find_jpeg_tiff_offset(bytes: &[u8]) -> Option<usize> {
    let mut offset = 2usize;
    while offset + 1 < bytes.len() {
        if byte_at(bytes, offset) != 0xff {
            return None;
        }
        let marker = byte_at(bytes, offset + 1);
        if marker == 0xff {
            offset += 1;
            continue;
        }

        if marker == 0xe1 {
            if offset + 4 >= bytes.len() {
                return None;
            }
            let segment_start = offset + 4;
            if segment_start + 6 > bytes.len() {
                return None;
            }
            if !has_exif_header(bytes, segment_start) {
                return None;
            }
            return Some(segment_start + 6);
        }

        if offset + 4 > bytes.len() {
            return None;
        }
        let length = read_u16_be(bytes, offset + 2) as usize;
        offset += 2 + length;
    }

    None
}

fn find_webp_tiff_offset(bytes: &[u8]) -> Option<usize> {
    let mut offset = 12usize;
    while offset + 8 <= bytes.len() {
        let chunk_size = read_u32_le(bytes, offset + 4) as usize;
        let data_start = offset + 8;

        // The loop guard guarantees `bytes[offset..offset + 4]` is present.
        if &bytes[offset..offset + 4] == b"EXIF" {
            if data_start + chunk_size > bytes.len() {
                return None;
            }
            // Some WebP files prefix the TIFF header with "Exif\0\0".
            let tiff_start = if chunk_size >= 6 && has_exif_header(bytes, data_start) {
                data_start + 6
            } else {
                data_start
            };
            return Some(tiff_start);
        }

        // RIFF chunks are padded to an even size.
        offset = data_start + chunk_size + (chunk_size % 2);
    }

    None
}

/// Read the EXIF orientation (`1..=8`) from a JPEG or WebP byte buffer, or
/// `None` when no usable EXIF orientation is present.
pub fn get_exif_orientation(bytes: &[u8]) -> Option<u8> {
    let tiff_offset = if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0xd8 {
        // JPEG: starts with FF D8.
        find_jpeg_tiff_offset(bytes)
    } else if bytes.len() >= 12
        && bytes[0] == 0x52
        && bytes[1] == 0x49
        && bytes[2] == 0x46
        && bytes[3] == 0x46
        && bytes[8] == 0x57
        && bytes[9] == 0x45
        && bytes[10] == 0x42
        && bytes[11] == 0x50
    {
        // WebP: starts with RIFF....WEBP.
        find_webp_tiff_offset(bytes)
    } else {
        return None;
    };

    tiff_offset.map(|offset| read_orientation_from_tiff(bytes, offset))
}

/// Apply the EXIF orientation transform to a decoded RGBA image so it reads
/// upright, mirroring pi's `applyExifOrientation`.
///
/// `original_bytes` are the source-encoded bytes the orientation tag is read
/// from. Orientation `1` (and any buffer with no EXIF orientation) is returned
/// unchanged, matching pi's early return. The rotate/flip mapping for
/// orientations `2..=8` reproduces pi's switch exactly:
///
/// - `2` -> horizontal flip
/// - `3` -> horizontal flip then vertical flip
/// - `4` -> vertical flip
/// - `5` -> clockwise `rotate90` then horizontal flip
/// - `6` -> clockwise `rotate90`
/// - `7` -> counter-clockwise `rotate270` then horizontal flip
/// - `8` -> counter-clockwise `rotate270`
pub fn apply_exif_orientation(image: RgbaImage, original_bytes: &[u8]) -> RgbaImage {
    let orientation = get_exif_orientation(original_bytes).unwrap_or(1);
    match orientation {
        2 => imageops::flip_horizontal(&image),
        3 => {
            let flipped = imageops::flip_horizontal(&image);
            imageops::flip_vertical(&flipped)
        }
        4 => imageops::flip_vertical(&image),
        5 => {
            let rotated = imageops::rotate90(&image);
            imageops::flip_horizontal(&rotated)
        }
        6 => imageops::rotate90(&image),
        7 => {
            let rotated = imageops::rotate270(&image);
            imageops::flip_horizontal(&rotated)
        }
        8 => imageops::rotate270(&image),
        // Orientation 1 (and any out-of-range value) is a no-op passthrough.
        _ => image,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal JPEG with an APP1/EXIF segment carrying a big-endian
    /// TIFF IFD whose orientation tag is `orientation`.
    fn jpeg_with_orientation(orientation: u8) -> Vec<u8> {
        let mut b: Vec<u8> = vec![0xff, 0xd8]; // SOI
        b.extend_from_slice(&[0xff, 0xe1]); // APP1 marker
        b.extend_from_slice(&[0x00, 0x20]); // segment length (unused by reader)
        b.extend_from_slice(b"Exif\0\0"); // EXIF header
                                          // TIFF header (big-endian "MM", magic 42).
        b.extend_from_slice(&[0x4d, 0x4d, 0x00, 0x2a]);
        b.extend_from_slice(&[0x00, 0x00, 0x00, 0x08]); // IFD offset = 8
        b.extend_from_slice(&[0x00, 0x01]); // entry count = 1
        b.extend_from_slice(&[0x01, 0x12]); // tag = orientation
        b.extend_from_slice(&[0x00, 0x03]); // type = SHORT
        b.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // count = 1
        b.extend_from_slice(&[0x00, orientation, 0x00, 0x00]); // value + padding
        b
    }

    #[test]
    fn reads_jpeg_orientation() {
        assert_eq!(get_exif_orientation(&jpeg_with_orientation(6)), Some(6));
        assert_eq!(get_exif_orientation(&jpeg_with_orientation(1)), Some(1));
        assert_eq!(get_exif_orientation(&jpeg_with_orientation(8)), Some(8));
    }

    #[test]
    fn out_of_range_orientation_defaults_to_one() {
        // Orientation value 9 is out of the 1..=8 range and defaults to 1.
        assert_eq!(get_exif_orientation(&jpeg_with_orientation(9)), Some(1));
    }

    #[test]
    fn returns_none_for_non_image_bytes() {
        assert_eq!(get_exif_orientation(&[0, 1, 2, 3, 4]), None);
    }

    #[test]
    fn returns_none_for_jpeg_without_exif() {
        // SOI immediately followed by EOI: no APP1/EXIF segment.
        assert_eq!(get_exif_orientation(&[0xff, 0xd8, 0xff, 0xd9]), None);
    }

    #[test]
    fn returns_none_for_short_input() {
        assert_eq!(get_exif_orientation(&[0xff]), None);
    }

    /// A 2x3 image where each pixel encodes its own (x, y) in the red/green
    /// channels, so a transform can be verified pixel by pixel.
    fn coded_image() -> RgbaImage {
        RgbaImage::from_fn(2, 3, |x, y| ::image::Rgba([x as u8, y as u8, 0, 255]))
    }

    #[test]
    fn orientation_one_is_passthrough() {
        let src = coded_image();
        let out = apply_exif_orientation(src.clone(), &jpeg_with_orientation(1));
        assert_eq!(out.dimensions(), (2, 3));
        assert_eq!(out.get_pixel(1, 2), src.get_pixel(1, 2));
    }

    #[test]
    fn no_exif_bytes_leave_image_unchanged() {
        let src = coded_image();
        let out = apply_exif_orientation(src.clone(), &[0, 1, 2, 3]);
        assert_eq!(out.dimensions(), (2, 3));
        assert_eq!(out.get_pixel(0, 0), src.get_pixel(0, 0));
    }

    #[test]
    fn orientation_six_rotates_clockwise() {
        // Orientation 6 is a clockwise 90-degree rotation: a 2x3 image becomes
        // 3x2, and source pixel (x, y) lands at (h - 1 - y, x).
        let src = coded_image();
        let out = apply_exif_orientation(src.clone(), &jpeg_with_orientation(6));
        assert_eq!(out.dimensions(), (3, 2));
        assert_eq!(out.get_pixel(2, 0), src.get_pixel(0, 0));
        assert_eq!(out.get_pixel(0, 1), src.get_pixel(1, 2));
    }

    #[test]
    fn orientation_eight_rotates_counter_clockwise() {
        // Orientation 8 is a counter-clockwise rotation: source pixel (x, y)
        // lands at (y, w - 1 - x) in the swapped 3x2 output.
        let src = coded_image();
        let out = apply_exif_orientation(src.clone(), &jpeg_with_orientation(8));
        assert_eq!(out.dimensions(), (3, 2));
        assert_eq!(out.get_pixel(0, 1), src.get_pixel(0, 0));
        assert_eq!(out.get_pixel(2, 0), src.get_pixel(1, 2));
    }

    #[test]
    fn orientation_two_flips_horizontally() {
        let src = coded_image();
        let out = apply_exif_orientation(src.clone(), &jpeg_with_orientation(2));
        assert_eq!(out.dimensions(), (2, 3));
        assert_eq!(out.get_pixel(0, 0), src.get_pixel(1, 0));
        assert_eq!(out.get_pixel(1, 2), src.get_pixel(0, 2));
    }
}
