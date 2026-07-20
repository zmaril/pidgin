//! Port of pi's `utils/image-resize.ts`: the `resize_image` entry point and
//! `format_dimension_note`.
//!
//! pi's `resizeImage` dispatches the work into a `node:worker_threads` Worker
//! (see `image-resize-worker.ts`) so Photon's WASM decode/resize/encode does not
//! block the TUI loop, falling back to `resizeImageInProcess` when the worker
//! cannot load. The worker indirection is a Node concurrency detail, not
//! observable behavior, so this port omits it and resizes synchronously in
//! process (see the crate-level non-port note in [`super`]). `resize_image` is
//! kept as the faithful entry seam callers use.

// straitjacket-allow-file:duplication

use super::resize_core::{resize_image_in_process, ImageResizeOptions, ResizedImage};

/// Resize an image to fit within the max dimensions and encoded file size.
///
/// The worker-thread dispatch of pi's `resizeImage` is intentionally not
/// reproduced; this calls straight through to [`resize_image_in_process`].
pub fn resize_image(
    input_bytes: &[u8],
    mime_type: &str,
    options: Option<&ImageResizeOptions>,
) -> Option<ResizedImage> {
    resize_image_in_process(input_bytes, mime_type, options)
}

/// Format a dimension note for resized images so the model can map coordinates
/// back to the original. Mirrors pi's `formatDimensionNote` string exactly.
pub fn format_dimension_note(result: &ResizedImage) -> Option<String> {
    if !result.was_resized {
        return None;
    }

    let scale = result.original_width as f64 / result.width as f64;
    Some(format!(
        "[Image: original {}x{}, displayed at {}x{}. Multiply coordinates by {:.2} to map to original image.]",
        result.original_width,
        result.original_height,
        result.width,
        result.height,
        scale,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resized(original_width: u32, width: u32, was_resized: bool) -> ResizedImage {
        ResizedImage {
            data: String::new(),
            mime_type: "image/png".to_string(),
            original_width,
            original_height: 1500,
            width,
            height: 1000,
            was_resized,
        }
    }

    #[test]
    fn no_note_when_not_resized() {
        assert_eq!(format_dimension_note(&resized(3000, 2000, false)), None);
    }

    #[test]
    fn note_reports_scale_to_two_decimals() {
        let note = format_dimension_note(&resized(3000, 2000, true)).expect("note");
        assert_eq!(
            note,
            "[Image: original 3000x1500, displayed at 2000x1000. Multiply coordinates by 1.50 to map to original image.]"
        );
    }

    #[test]
    fn note_scale_rounds_to_two_decimals() {
        // 1000 / 333 = 3.003..., rendered as 3.00.
        let note = format_dimension_note(&resized(1000, 333, true)).expect("note");
        assert!(note.contains("Multiply coordinates by 3.00 "));
    }
}
