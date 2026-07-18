//! Read the EXIF orientation tag from JPEG and WebP byte buffers.
//!
//! Ported from pi's `utils/exif-orientation.ts`. Only the pure byte-parsing
//! reader (`getExifOrientation`) is ported. It locates the EXIF/TIFF block in a
//! JPEG (APP1 segment) or WebP (`EXIF` chunk), reads the TIFF IFD, and returns
//! the orientation tag (`0x0112`) as a value in `1..=8`.
//!
//! Deferred: pi's `applyExifOrientation` (and its `rotate90` helper) mutate
//! Photon images to physically rotate/flip pixels. That depends on the Photon
//! WASM image library and is out of scope for this PR.
//!
//! Adaptation: pi returns the number `1` as a default when no EXIF orientation
//! is present. This port returns `Option<u8>`, using `None` for buffers with no
//! usable EXIF orientation block (equivalent to pi's default of `1`, which
//! callers treat as "no transform").

use super::bytes::{byte_at, read_u16_be, read_u16_le, read_u32_be, read_u32_le};

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
}
