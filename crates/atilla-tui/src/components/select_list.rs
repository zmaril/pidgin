//! Byte-exact port of `vendor/pi/packages/tui/src/components/select-list.ts`.
//!
//! Scrollable single-select list (used by the editor's autocomplete dropdown
//! and slash menu). Renders a primary column (label/value) plus an optional
//! aligned description column, with a configurable primary-column width and an
//! overridable primary-truncation hook.

use crate::keybindings::{tui_keybindings, KeybindingsManager};
use crate::renderer::Component;
use crate::width::{truncate_to_width, visible_width};

const DEFAULT_PRIMARY_COLUMN_WIDTH: i64 = 32;
const PRIMARY_COLUMN_GAP: i64 = 2;
const MIN_DESCRIPTION_WIDTH: i64 = 10;

/// `normalizeToSingleLine` — collapse CR/LF runs to a single space and trim.
fn normalize_to_single_line(text: &str) -> String {
    // Replace runs of [\r\n]+ with a single space, then trim (JS String.trim,
    // which strips leading/trailing whitespace).
    let mut out = String::with_capacity(text.len());
    let mut in_run = false;
    for ch in text.chars() {
        if ch == '\r' || ch == '\n' {
            if !in_run {
                out.push(' ');
                in_run = true;
            }
        } else {
            out.push(ch);
            in_run = false;
        }
    }
    js_trim(&out).to_string()
}

/// JS `String.prototype.trim`: strips the JS whitespace set from both ends.
fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_trim_char)
}

fn is_js_trim_char(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

fn clamp(value: i64, min: i64, max: i64) -> i64 {
    max.min(value).max(min)
}

/// A selectable item (`SelectItem`).
#[derive(Debug, Clone)]
pub struct SelectItem {
    /// Underlying value.
    pub value: String,
    /// Display label.
    pub label: String,
    /// Optional description shown to the right.
    pub description: Option<String>,
}

/// Theme functions (`SelectListTheme`); each maps text to styled text.
pub struct SelectListTheme {
    /// Style for the selected prefix.
    pub selected_prefix: Box<dyn Fn(&str) -> String>,
    /// Style for the whole selected line.
    pub selected_text: Box<dyn Fn(&str) -> String>,
    /// Style for the description column.
    pub description: Box<dyn Fn(&str) -> String>,
    /// Style for the scroll-position indicator.
    pub scroll_info: Box<dyn Fn(&str) -> String>,
    /// Style for the "no match" message.
    pub no_match: Box<dyn Fn(&str) -> String>,
}

/// Context passed to a `truncate_primary` override.
#[derive(Debug, Clone)]
pub struct SelectListTruncatePrimaryContext {
    /// The text to truncate (item's display value).
    pub text: String,
    /// Maximum visible width available.
    pub max_width: i64,
    /// The full primary column width.
    pub column_width: i64,
    /// The item being rendered.
    pub item: SelectItem,
    /// Whether the item is selected.
    pub is_selected: bool,
}

/// Override for primary-column truncation (`truncatePrimary`).
pub type TruncatePrimaryFn = Box<dyn Fn(&SelectListTruncatePrimaryContext) -> String>;

/// Layout options (`SelectListLayoutOptions`).
#[derive(Default)]
pub struct SelectListLayoutOptions {
    /// Minimum primary column width.
    pub min_primary_column_width: Option<i64>,
    /// Maximum primary column width.
    pub max_primary_column_width: Option<i64>,
    /// Optional override for primary truncation.
    pub truncate_primary: Option<TruncatePrimaryFn>,
}

/// Scrollable single-select list component.
pub struct SelectList {
    items: Vec<SelectItem>,
    filtered_items: Vec<SelectItem>,
    selected_index: i64,
    max_visible: i64,
    theme: SelectListTheme,
    layout: SelectListLayoutOptions,

    /// Invoked when an item is confirmed (`onSelect`).
    pub on_select: Option<Box<dyn FnMut(SelectItem)>>,
    /// Invoked on cancel (`onCancel`).
    pub on_cancel: Option<Box<dyn FnMut()>>,
    /// Invoked when the selection changes (`onSelectionChange`).
    pub on_selection_change: Option<Box<dyn FnMut(SelectItem)>>,

    keybindings: KeybindingsManager,
}

impl SelectList {
    /// `new SelectList(items, maxVisible, theme, layout = {})`.
    pub fn new(
        items: Vec<SelectItem>,
        max_visible: i64,
        theme: SelectListTheme,
        layout: SelectListLayoutOptions,
    ) -> Self {
        Self {
            filtered_items: items.clone(),
            items,
            selected_index: 0,
            max_visible,
            theme,
            layout,
            on_select: None,
            on_cancel: None,
            on_selection_change: None,
            keybindings: KeybindingsManager::new(tui_keybindings(), Vec::new()),
        }
    }

    /// Filter items by a case-insensitive `value` prefix (`setFilter`).
    pub fn set_filter(&mut self, filter: &str) {
        let needle = filter.to_lowercase();
        self.filtered_items = self
            .items
            .iter()
            .filter(|item| item.value.to_lowercase().starts_with(&needle))
            .cloned()
            .collect();
        // Reset selection when filter changes.
        self.selected_index = 0;
    }

    /// Set the selected index, clamped to the filtered range (`setSelectedIndex`).
    pub fn set_selected_index(&mut self, index: i64) {
        self.selected_index = 0.max(index.min(self.filtered_items.len() as i64 - 1));
    }

    /// Render the list (`render`).
    pub fn render_lines(&self, width: usize) -> Vec<String> {
        let width = width as i64;
        let mut lines: Vec<String> = Vec::new();

        // If no items match the filter, show a message.
        if self.filtered_items.is_empty() {
            lines.push((self.theme.no_match)("  No matching commands"));
            return lines;
        }

        let primary_column_width = self.get_primary_column_width();

        // Calculate visible range with scrolling.
        let start_index = 0.max(
            (self.selected_index - self.max_visible / 2)
                .min(self.filtered_items.len() as i64 - self.max_visible),
        );
        let end_index = (start_index + self.max_visible).min(self.filtered_items.len() as i64);

        // Render visible items.
        let mut i = start_index;
        while i < end_index {
            if let Some(item) = self.filtered_items.get(i as usize) {
                let is_selected = i == self.selected_index;
                let description_single_line = item
                    .description
                    .as_ref()
                    .map(|d| normalize_to_single_line(d));
                lines.push(self.render_item(
                    item,
                    is_selected,
                    width,
                    description_single_line.as_deref(),
                    primary_column_width,
                ));
            }
            i += 1;
        }

        // Add scroll indicators if needed.
        if start_index > 0 || end_index < self.filtered_items.len() as i64 {
            let scroll_text = format!(
                "  ({}/{})",
                self.selected_index + 1,
                self.filtered_items.len()
            );
            // Truncate if too long for the terminal.
            lines.push((self.theme.scroll_info)(&truncate_to_width(
                &scroll_text,
                width - 2,
                "",
                false,
            )));
        }

        lines
    }

    /// Handle a chunk of terminal input (`handleInput`).
    pub fn handle_input_str(&mut self, key_data: &str) {
        // Up arrow — wrap to bottom when at top.
        if self.keybindings.matches(key_data, "tui.select.up") {
            self.selected_index = if self.selected_index == 0 {
                self.filtered_items.len() as i64 - 1
            } else {
                self.selected_index - 1
            };
            self.notify_selection_change();
        }
        // Down arrow — wrap to top when at bottom.
        else if self.keybindings.matches(key_data, "tui.select.down") {
            self.selected_index = if self.selected_index == self.filtered_items.len() as i64 - 1 {
                0
            } else {
                self.selected_index + 1
            };
            self.notify_selection_change();
        }
        // Enter.
        else if self.keybindings.matches(key_data, "tui.select.confirm") {
            if let Some(selected_item) = self.filtered_items.get(self.selected_index as usize) {
                let item = selected_item.clone();
                if let Some(cb) = self.on_select.as_mut() {
                    cb(item);
                }
            }
        }
        // Escape or Ctrl+C.
        else if self.keybindings.matches(key_data, "tui.select.cancel") {
            if let Some(cb) = self.on_cancel.as_mut() {
                cb();
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_item(
        &self,
        item: &SelectItem,
        is_selected: bool,
        width: i64,
        description_single_line: Option<&str>,
        primary_column_width: i64,
    ) -> String {
        let prefix = if is_selected { "→ " } else { "  " };
        let prefix_width = visible_width(prefix) as i64;

        if let Some(description_single_line) = description_single_line {
            if width > 40 {
                let effective_primary_column_width =
                    1.max(primary_column_width.min(width - prefix_width - 4));
                let max_primary_width = 1.max(effective_primary_column_width - PRIMARY_COLUMN_GAP);
                let truncated_value = self.truncate_primary(
                    item,
                    is_selected,
                    max_primary_width,
                    effective_primary_column_width,
                );
                let truncated_value_width = visible_width(&truncated_value) as i64;
                let spacing_len = 1.max(effective_primary_column_width - truncated_value_width);
                let spacing = " ".repeat(spacing_len as usize);
                let description_start = prefix_width + truncated_value_width + spacing_len;
                let remaining_width = width - description_start - 2; // -2 for safety

                if remaining_width > MIN_DESCRIPTION_WIDTH {
                    let truncated_desc =
                        truncate_to_width(description_single_line, remaining_width, "", false);
                    if is_selected {
                        return (self.theme.selected_text)(&format!(
                            "{prefix}{truncated_value}{spacing}{truncated_desc}"
                        ));
                    }

                    let desc_text = (self.theme.description)(&format!("{spacing}{truncated_desc}"));
                    return format!("{prefix}{truncated_value}{desc_text}");
                }
            }
        }

        let max_width = width - prefix_width - 2;
        let truncated_value = self.truncate_primary(item, is_selected, max_width, max_width);
        if is_selected {
            return (self.theme.selected_text)(&format!("{prefix}{truncated_value}"));
        }

        format!("{prefix}{truncated_value}")
    }

    fn get_primary_column_width(&self) -> i64 {
        let (min, max) = self.get_primary_column_bounds();
        let widest_primary = self.filtered_items.iter().fold(0_i64, |widest, item| {
            widest.max(visible_width(&self.get_display_value(item)) as i64 + PRIMARY_COLUMN_GAP)
        });

        clamp(widest_primary, min, max)
    }

    fn get_primary_column_bounds(&self) -> (i64, i64) {
        let raw_min = self
            .layout
            .min_primary_column_width
            .or(self.layout.max_primary_column_width)
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);
        let raw_max = self
            .layout
            .max_primary_column_width
            .or(self.layout.min_primary_column_width)
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);

        (1.max(raw_min.min(raw_max)), 1.max(raw_min.max(raw_max)))
    }

    fn truncate_primary(
        &self,
        item: &SelectItem,
        is_selected: bool,
        max_width: i64,
        column_width: i64,
    ) -> String {
        let display_value = self.get_display_value(item);
        let truncated_value = match &self.layout.truncate_primary {
            Some(f) => f(&SelectListTruncatePrimaryContext {
                text: display_value,
                max_width,
                column_width,
                item: item.clone(),
                is_selected,
            }),
            None => truncate_to_width(&display_value, max_width, "", false),
        };

        truncate_to_width(&truncated_value, max_width, "", false)
    }

    fn get_display_value(&self, item: &SelectItem) -> String {
        // pi: `item.label || item.value` — empty label falls back to value.
        if item.label.is_empty() {
            item.value.clone()
        } else {
            item.label.clone()
        }
    }

    fn notify_selection_change(&mut self) {
        if let Some(selected_item) = self.filtered_items.get(self.selected_index as usize) {
            let item = selected_item.clone();
            if let Some(cb) = self.on_selection_change.as_mut() {
                cb(item);
            }
        }
    }

    /// The currently selected item, if any (`getSelectedItem`).
    pub fn get_selected_item(&self) -> Option<SelectItem> {
        self.filtered_items
            .get(self.selected_index as usize)
            .cloned()
    }
}

impl Component for SelectList {
    fn render(&self, width: usize) -> Vec<String> {
        self.render_lines(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.handle_input_str(data);
    }

    fn invalidate(&mut self) {
        // No cached state to invalidate currently.
    }
}
