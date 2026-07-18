//! Byte-exact port of `vendor/pi/packages/tui/src/components/image.ts`.
//!
//! Builds on [`crate::terminal_image`] for capability detection, dimension math,
//! and the Kitty / iTerm2 encoders. pi's render cache is omitted (the render is a
//! pure function of `(state, width, globals)`); `this.imageId` mutation during
//! render is modeled with a [`Cell`].

use std::cell::Cell;

use crate::renderer::Component;
use crate::terminal_image::{
    allocate_image_id, get_capabilities, get_cell_dimensions, get_image_dimensions, image_fallback,
    render_image, ImageDimensions, ImageProtocol, ImageRenderOptions,
};

/// A color-styling function (pi's `(str: string) => string`).
pub type FallbackColorFn = Box<dyn Fn(&str) -> String>;

/// Theme for the [`Image`] fallback line (pi's `ImageTheme`).
pub struct ImageTheme {
    pub fallback_color: FallbackColorFn,
}

/// Options for [`Image`] (pi's `ImageOptions`).
#[derive(Debug, Clone, Default)]
pub struct ImageOptions {
    pub max_width_cells: Option<u32>,
    pub max_height_cells: Option<u32>,
    pub filename: Option<String>,
    /// Kitty image ID. If provided, reuses this ID (for animations/updates).
    pub image_id: Option<u64>,
}

/// Terminal image placement component.
pub struct Image {
    base64_data: String,
    mime_type: String,
    dimensions: ImageDimensions,
    theme: ImageTheme,
    options: ImageOptions,
    image_id: Cell<Option<u64>>,
}

impl Image {
    /// `new Image(base64Data, mimeType, theme, options = {}, dimensions?)`.
    pub fn new(
        base64_data: &str,
        mime_type: &str,
        theme: ImageTheme,
        options: ImageOptions,
        dimensions: Option<ImageDimensions>,
    ) -> Self {
        let dimensions = dimensions
            .or_else(|| get_image_dimensions(base64_data, mime_type))
            .unwrap_or(ImageDimensions {
                width_px: 800,
                height_px: 600,
            });
        let image_id = Cell::new(options.image_id);
        Self {
            base64_data: base64_data.to_string(),
            mime_type: mime_type.to_string(),
            dimensions,
            theme,
            options,
            image_id,
        }
    }

    /// `getImageId()`.
    pub fn get_image_id(&self) -> Option<u64> {
        self.image_id.get()
    }

    fn fallback_lines(&self) -> Vec<String> {
        let fallback = image_fallback(
            &self.mime_type,
            Some(self.dimensions),
            self.options.filename.as_deref(),
        );
        vec![(self.theme.fallback_color)(&fallback)]
    }
}

impl Component for Image {
    fn render(&self, width: usize) -> Vec<String> {
        // Math.max(1, Math.min(width - 2, maxWidthCells ?? 60)) with JS integer
        // semantics (width - 2 may go negative).
        let w2 = width as i64 - 2;
        let cap = self.options.max_width_cells.unwrap_or(60) as i64;
        let max_width = w2.min(cap).max(1);

        let cell_dimensions = get_cell_dimensions();
        let default_max_height = (((max_width as f64) * (cell_dimensions.width_px as f64))
            / (cell_dimensions.height_px as f64))
            .ceil()
            .max(1.0) as u32;
        let max_height = self.options.max_height_cells.unwrap_or(default_max_height);

        let caps = get_capabilities();
        let Some(protocol) = caps.images else {
            return self.fallback_lines();
        };

        if protocol == ImageProtocol::Kitty && self.image_id.get().is_none() {
            self.image_id.set(Some(allocate_image_id()));
        }

        let result = render_image(
            &self.base64_data,
            self.dimensions,
            ImageRenderOptions {
                max_width_cells: Some(max_width as u32),
                max_height_cells: Some(max_height),
                image_id: self.image_id.get(),
                move_cursor: Some(false),
                ..Default::default()
            },
        );

        let Some(result) = result else {
            return self.fallback_lines();
        };

        // Store the image ID for later cleanup.
        if let Some(rid) = result.image_id {
            self.image_id.set(Some(rid));
        }

        let mut lines: Vec<String>;
        if protocol == ImageProtocol::Kitty {
            // For Kitty: C=1 prevents cursor movement.
            lines = vec![result.sequence];
            // Return `rows` lines so TUI accounts for image height.
            for _ in 0..result.rows.saturating_sub(1) {
                lines.push(String::new());
            }
        } else {
            // Return `rows` lines so TUI accounts for image height.
            lines = Vec::new();
            for _ in 0..result.rows.saturating_sub(1) {
                lines.push(String::new());
            }
            let row_offset = result.rows.saturating_sub(1);
            let move_up = if row_offset > 0 {
                format!("\x1b[{row_offset}A")
            } else {
                String::new()
            };
            lines.push(format!("{move_up}{}", result.sequence));
        }

        lines
    }

    fn invalidate(&mut self) {
        // Cache omitted; nothing to invalidate.
    }
}
