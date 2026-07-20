// straitjacket-allow-file:duplication — the two submenu unit tests share
// near-identical SettingsList construction scaffolding (items, callbacks,
// handle_input driving), the standard per-scenario test boilerplate; factoring
// it out would add more indirection than it removes.
//! Byte-exact port of `vendor/pi/packages/tui/src/components/settings-list.ts`.
//!
//! Scrollable settings list with an optional embedded fuzzy-search [`Input`],
//! value cycling, and per-item submenus. Selected items may show a wrapped
//! description; an item with `values` cycles through them on Enter/Space; an
//! item with a `submenu` factory opens a nested [`Component`] whose result is
//! reported back through a `done` callback.
//!
//! pi's `filteredItems` is an array of the same item objects as `items` (JS
//! reference sharing). Rust models it as a list of indices into `items` so a
//! value mutated through either view is observed by both.

use std::cell::RefCell;
use std::rc::Rc;

use crate::components::input::Input;
use crate::fuzzy::fuzzy_filter;
use crate::keybindings::{tui_keybindings, KeybindingsManager};
use crate::renderer::Component;
use crate::width::{truncate_to_width, visible_width, wrap_text_with_ansi};

/// Callback the submenu invokes to report its result (`done`).
pub type SubmenuDone = Box<dyn Fn(Option<String>)>;
/// Factory producing a submenu component from the current value + a `done`
/// callback (pi's `submenu` field).
pub type SubmenuFactory = Box<dyn Fn(String, SubmenuDone) -> Box<dyn Component>>;

/// A single setting (`SettingItem`).
pub struct SettingItem {
    /// Unique identifier for this setting.
    pub id: String,
    /// Display label (left side).
    pub label: String,
    /// Optional description shown when selected.
    pub description: Option<String>,
    /// Current value to display (right side).
    pub current_value: String,
    /// If present, Enter/Space cycles through these values.
    pub values: Option<Vec<String>>,
    /// If present, Enter opens this submenu.
    pub submenu: Option<SubmenuFactory>,
}

impl SettingItem {
    /// Convenience constructor for a plain value-display item.
    pub fn new(id: &str, label: &str, current_value: &str) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            description: None,
            current_value: current_value.to_string(),
            values: None,
            submenu: None,
        }
    }
}

/// A style function taking `(text, selected)` (pi's label/value theme fns).
pub type SelectableStyleFn = Box<dyn Fn(&str, bool) -> String>;
/// A style function taking `text` (pi's description/hint theme fns).
pub type StyleFn = Box<dyn Fn(&str) -> String>;

/// Theme functions (`SettingsListTheme`).
pub struct SettingsListTheme {
    /// Style for a label (`(text, selected) => string`).
    pub label: SelectableStyleFn,
    /// Style for a value (`(text, selected) => string`).
    pub value: SelectableStyleFn,
    /// Style for the description block.
    pub description: StyleFn,
    /// Prefix string shown for the selected row (`cursor`).
    pub cursor: String,
    /// Style for hint/scroll text.
    pub hint: StyleFn,
}

/// Options (`SettingsListOptions`).
#[derive(Default)]
pub struct SettingsListOptions {
    /// Enable the embedded fuzzy-search input.
    pub enable_search: bool,
}

/// Outcome recorded by a submenu's `done` callback, applied after the
/// re-entrant `handle_input` returns (interior mutability breaks the cycle
/// between the stored submenu component and the parent it must mutate).
struct SubmenuOutcome {
    /// Items index whose value should be updated.
    item_index: usize,
    /// The selected value, or `None` if the submenu was cancelled.
    selected_value: Option<String>,
}

/// Scrollable settings list component.
pub struct SettingsList {
    items: Vec<SettingItem>,
    /// Indices into `items`, in filtered/scored order.
    filtered: Vec<usize>,
    theme: SettingsListTheme,
    selected_index: i64,
    max_visible: i64,
    on_change: Box<dyn FnMut(String, String)>,
    on_cancel: Box<dyn FnMut()>,
    search_input: Option<Input>,
    search_enabled: bool,

    // Submenu state.
    submenu_component: Option<Box<dyn Component>>,
    submenu_item_index: Option<i64>,
    submenu_outcome: Rc<RefCell<Option<SubmenuOutcome>>>,

    keybindings: KeybindingsManager,
}

impl SettingsList {
    /// `new SettingsList(items, maxVisible, theme, onChange, onCancel, options)`.
    pub fn new(
        items: Vec<SettingItem>,
        max_visible: i64,
        theme: SettingsListTheme,
        on_change: Box<dyn FnMut(String, String)>,
        on_cancel: Box<dyn FnMut()>,
        options: SettingsListOptions,
    ) -> Self {
        let filtered = (0..items.len()).collect();
        let search_enabled = options.enable_search;
        let search_input = if search_enabled {
            Some(Input::new())
        } else {
            None
        };
        Self {
            items,
            filtered,
            theme,
            selected_index: 0,
            max_visible,
            on_change,
            on_cancel,
            search_input,
            search_enabled,
            submenu_component: None,
            submenu_item_index: None,
            submenu_outcome: Rc::new(RefCell::new(None)),
            keybindings: KeybindingsManager::new(tui_keybindings(), Vec::new()),
        }
    }

    /// Update an item's `current_value` (`updateValue`).
    pub fn update_value(&mut self, id: &str, new_value: &str) {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.current_value = new_value.to_string();
        }
    }

    /// Number of items currently displayed (filtered when search is enabled).
    fn display_len(&self) -> i64 {
        if self.search_enabled {
            self.filtered.len() as i64
        } else {
            self.items.len() as i64
        }
    }

    /// Resolve a display position to an index into `items`.
    fn items_index(&self, display_pos: i64) -> Option<usize> {
        if display_pos < 0 {
            return None;
        }
        let p = display_pos as usize;
        if self.search_enabled {
            self.filtered.get(p).copied()
        } else if p < self.items.len() {
            Some(p)
        } else {
            None
        }
    }

    /// Render the list (`render`).
    pub fn render_lines(&self, width: usize) -> Vec<String> {
        // If a submenu is active, render it instead.
        if let Some(submenu) = &self.submenu_component {
            return submenu.render(width);
        }
        self.render_main_list(width)
    }

    fn render_main_list(&self, width: usize) -> Vec<String> {
        let width_i = width as i64;
        let mut lines: Vec<String> = Vec::new();

        if self.search_enabled {
            if let Some(search_input) = &self.search_input {
                lines.extend(search_input.render_lines(width));
                lines.push(String::new());
            }
        }

        if self.items.is_empty() {
            lines.push((self.theme.hint)("  No settings available"));
            if self.search_enabled {
                self.add_hint_line(&mut lines, width_i);
            }
            return lines;
        }

        let display_len = self.display_len();
        if display_len == 0 {
            lines.push(truncate_to_width(
                &(self.theme.hint)("  No matching settings"),
                width_i,
                "...",
                false,
            ));
            self.add_hint_line(&mut lines, width_i);
            return lines;
        }

        // Calculate visible range with scrolling.
        let start_index =
            0.max((self.selected_index - self.max_visible / 2).min(display_len - self.max_visible));
        let end_index = (start_index + self.max_visible).min(display_len);

        // Calculate max label width for alignment (over ALL items).
        let max_label_width = 30.min(
            self.items
                .iter()
                .map(|item| visible_width(&item.label) as i64)
                .max()
                .unwrap_or(0),
        );

        // Render visible items.
        let mut i = start_index;
        while i < end_index {
            if let Some(idx) = self.items_index(i) {
                let item = &self.items[idx];
                let is_selected = i == self.selected_index;
                let prefix = if is_selected {
                    self.theme.cursor.clone()
                } else {
                    "  ".to_string()
                };
                let prefix_width = visible_width(&prefix) as i64;

                // Pad label to align values.
                let label_pad = 0.max(max_label_width - visible_width(&item.label) as i64);
                let label_padded = format!("{}{}", item.label, " ".repeat(label_pad as usize));
                let label_text = (self.theme.label)(&label_padded, is_selected);

                // Calculate space for the value.
                let separator = "  ";
                let used_width = prefix_width + max_label_width + visible_width(separator) as i64;
                let value_max_width = width_i - used_width - 2;

                let value_text = (self.theme.value)(
                    &truncate_to_width(&item.current_value, value_max_width, "", false),
                    is_selected,
                );

                lines.push(truncate_to_width(
                    &format!("{prefix}{label_text}{separator}{value_text}"),
                    width_i,
                    "...",
                    false,
                ));
            }
            i += 1;
        }

        // Add scroll indicator if needed.
        if start_index > 0 || end_index < display_len {
            let scroll_text = format!("  ({}/{})", self.selected_index + 1, display_len);
            lines.push((self.theme.hint)(&truncate_to_width(
                &scroll_text,
                width_i - 2,
                "",
                false,
            )));
        }

        // Add description for the selected item.
        if let Some(idx) = self.items_index(self.selected_index) {
            if let Some(description) = &self.items[idx].description {
                lines.push(String::new());
                let wrapped_desc = wrap_text_with_ansi(description, (width_i - 4).max(0) as usize);
                for line in wrapped_desc {
                    lines.push((self.theme.description)(&format!("  {line}")));
                }
            }
        }

        // Add hint.
        self.add_hint_line(&mut lines, width_i);

        lines
    }

    /// Handle a chunk of terminal input (`handleInput`).
    pub fn handle_input_str(&mut self, data: &str) {
        // If a submenu is active, delegate all input to it. Its `done` callback
        // records an outcome that we apply after the delegated call returns.
        if let Some(submenu) = self.submenu_component.as_mut() {
            submenu.handle_input(data);
            self.apply_submenu_outcome();
            return;
        }

        // Main list input handling.
        let display_len = self.display_len();
        if self.keybindings.matches(data, "tui.select.up") {
            if display_len == 0 {
                return;
            }
            self.selected_index = if self.selected_index == 0 {
                display_len - 1
            } else {
                self.selected_index - 1
            };
        } else if self.keybindings.matches(data, "tui.select.down") {
            if display_len == 0 {
                return;
            }
            self.selected_index = if self.selected_index == display_len - 1 {
                0
            } else {
                self.selected_index + 1
            };
        } else if self.keybindings.matches(data, "tui.select.confirm") || data == " " {
            self.activate_item();
        } else if self.keybindings.matches(data, "tui.select.cancel") {
            (self.on_cancel)();
        } else if self.search_enabled && self.search_input.is_some() {
            let sanitized = data.replace(' ', "");
            if sanitized.is_empty() {
                return;
            }
            let search_input = self.search_input.as_mut().expect("search input present");
            search_input.handle_input_str(&sanitized);
            let query = search_input.get_value();
            self.apply_filter(&query);
        }
    }

    fn activate_item(&mut self) {
        let Some(idx) = self.items_index(self.selected_index) else {
            return;
        };

        if self.items[idx].submenu.is_some() {
            // Open submenu, passing the current value so it can preselect the row.
            self.submenu_item_index = Some(self.selected_index);
            let current_value = self.items[idx].current_value.clone();

            let outcome = Rc::clone(&self.submenu_outcome);
            let done: SubmenuDone = Box::new(move |selected_value: Option<String>| {
                *outcome.borrow_mut() = Some(SubmenuOutcome {
                    item_index: idx,
                    selected_value,
                });
            });

            let factory = self.items[idx].submenu.as_ref().expect("submenu present");
            self.submenu_component = Some(factory(current_value, done));
        } else if self
            .items
            .get(idx)
            .and_then(|item| item.values.as_ref())
            .is_some_and(|values| !values.is_empty())
        {
            // Cycle through values.
            let values = self.items[idx].values.clone().expect("values present");
            let current_index = values
                .iter()
                .position(|v| *v == self.items[idx].current_value);
            // JS `indexOf` returns -1 when not found; `(-1 + 1) % n == 0`.
            let current = current_index.map(|c| c as i64).unwrap_or(-1);
            let next_index = ((current + 1) % values.len() as i64) as usize;
            let new_value = values[next_index].clone();
            self.items[idx].current_value = new_value.clone();
            let id = self.items[idx].id.clone();
            (self.on_change)(id, new_value);
        }
    }

    /// Apply and clear any outcome the active submenu's `done` recorded.
    fn apply_submenu_outcome(&mut self) {
        let outcome = self.submenu_outcome.borrow_mut().take();
        if let Some(SubmenuOutcome {
            item_index,
            selected_value,
        }) = outcome
        {
            if let Some(value) = selected_value {
                self.items[item_index].current_value = value.clone();
                let id = self.items[item_index].id.clone();
                (self.on_change)(id, value);
            }
            self.close_submenu();
        }
    }

    fn close_submenu(&mut self) {
        self.submenu_component = None;
        // Restore selection to the item that opened the submenu.
        if let Some(index) = self.submenu_item_index {
            self.selected_index = index;
            self.submenu_item_index = None;
        }
    }

    fn apply_filter(&mut self, query: &str) {
        let indices: Vec<usize> = (0..self.items.len()).collect();
        let labels: Vec<String> = self.items.iter().map(|i| i.label.clone()).collect();
        self.filtered = fuzzy_filter(indices, query, |&i| labels[i].clone());
        self.selected_index = 0;
    }

    fn add_hint_line(&self, lines: &mut Vec<String>, width: i64) {
        lines.push(String::new());
        let hint = if self.search_enabled {
            "  Type to search · Enter/Space to change · Esc to cancel"
        } else {
            "  Enter/Space to change · Esc to cancel"
        };
        lines.push(truncate_to_width(
            &(self.theme.hint)(hint),
            width,
            "...",
            false,
        ));
    }
}

impl Component for SettingsList {
    fn render(&self, width: usize) -> Vec<String> {
        self.render_lines(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.handle_input_str(data);
    }

    fn invalidate(&mut self) {
        if let Some(submenu) = self.submenu_component.as_mut() {
            submenu.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::select_list::{SelectItem, SelectList, SelectListTheme};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn identity_settings_theme() -> SettingsListTheme {
        SettingsListTheme {
            label: Box::new(|t, _s| t.to_string()),
            value: Box::new(|t, _s| t.to_string()),
            description: Box::new(|t| t.to_string()),
            cursor: "\u{2192} ".to_string(),
            hint: Box::new(|t| t.to_string()),
        }
    }

    fn identity_select_theme() -> SelectListTheme {
        SelectListTheme {
            selected_prefix: Box::new(|t| t.to_string()),
            selected_text: Box::new(|t| t.to_string()),
            description: Box::new(|t| t.to_string()),
            scroll_info: Box::new(|t| t.to_string()),
            no_match: Box::new(|t| t.to_string()),
        }
    }

    // A submenu factory returning a SelectList whose confirmation reports the
    // chosen value through `done`. This mirrors how pi wires a nested component.
    fn select_submenu(options: Vec<&str>) -> SubmenuFactory {
        let options: Vec<String> = options.into_iter().map(String::from).collect();
        Box::new(move |current: String, done: SubmenuDone| {
            let items: Vec<SelectItem> = options
                .iter()
                .map(|v| SelectItem {
                    value: v.clone(),
                    label: v.clone(),
                    description: None,
                })
                .collect();
            let start = options.iter().position(|v| *v == current).unwrap_or(0) as i64;
            let mut list = SelectList::new(items, 5, identity_select_theme(), Default::default());
            list.set_selected_index(start);
            let done = Rc::new(done);
            {
                let done = Rc::clone(&done);
                list.on_select = Some(Box::new(move |item: SelectItem| done(Some(item.value))));
            }
            {
                let done = Rc::clone(&done);
                list.on_cancel = Some(Box::new(move || done(None)));
            }
            Box::new(list)
        })
    }

    #[test]
    fn submenu_render_delegates_and_done_applies() {
        let changes: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
        let items = vec![SettingItem {
            id: "theme".to_string(),
            label: "Theme".to_string(),
            description: None,
            current_value: "dark".to_string(),
            values: None,
            submenu: Some(select_submenu(vec!["dark", "light", "system"])),
        }];
        let on_change: Box<dyn FnMut(String, String)> = {
            let changes = Rc::clone(&changes);
            Box::new(move |id, v| changes.borrow_mut().push((id, v)))
        };
        let mut list = SettingsList::new(
            items,
            5,
            identity_settings_theme(),
            on_change,
            Box::new(|| {}),
            SettingsListOptions::default(),
        );

        // Open the submenu (Enter on the only item).
        list.handle_input_str("\r");

        // render() now delegates to the SelectList submenu: its lines start with
        // the "dark" option preselected (preselection matches current value).
        let rendered = list.render_lines(40);
        assert_eq!(
            rendered,
            vec![
                "\u{2192} dark".to_string(),
                "  light".to_string(),
                "  system".to_string(),
            ]
        );

        // Navigate down twice inside the submenu and confirm -> done("system").
        list.handle_input_str("\x1b[B");
        list.handle_input_str("\x1b[B");
        list.handle_input_str("\r");

        // Submenu closed; onChange fired; value updated; render is the main list.
        assert_eq!(
            *changes.borrow(),
            vec![("theme".to_string(), "system".to_string())]
        );
        let main = list.render_lines(40);
        assert!(main[0].contains("Theme"));
        assert!(main[0].contains("system"));
    }

    #[test]
    fn submenu_cancel_closes_without_change() {
        let changes: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
        let items = vec![SettingItem {
            id: "theme".to_string(),
            label: "Theme".to_string(),
            description: None,
            current_value: "dark".to_string(),
            values: None,
            submenu: Some(select_submenu(vec!["dark", "light"])),
        }];
        let on_change: Box<dyn FnMut(String, String)> = {
            let changes = Rc::clone(&changes);
            Box::new(move |id, v| changes.borrow_mut().push((id, v)))
        };
        let mut list = SettingsList::new(
            items,
            5,
            identity_settings_theme(),
            on_change,
            Box::new(|| {}),
            SettingsListOptions::default(),
        );

        list.handle_input_str("\r"); // open submenu
        list.handle_input_str("\x1b"); // cancel inside submenu -> done(None)

        // No change recorded; submenu closed; selection restored.
        assert!(changes.borrow().is_empty());
        let main = list.render_lines(40);
        assert!(main[0].contains("Theme"));
        assert!(main[0].contains("dark"));
    }
}
