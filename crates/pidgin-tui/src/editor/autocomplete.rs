//! Autocomplete integration for the editor — a byte-exact port of the
//! autocomplete orchestration in `vendor/pi/packages/tui/src/components/editor.ts`.
//!
//! # The async two-phase machine (flush seam)
//!
//! pi's `getSuggestions` is `async` and is never settled synchronously inside
//! `handleInput`: a request is either scheduled behind a debounce `setTimeout`
//! or dispatched on the microtask queue (an `async` body suspends at its first
//! `await`). Every keystroke calls `cancelAutocompleteRequest` first (bumping a
//! monotonic `startToken` and aborting any in-flight request), so N keystrokes
//! collapse to a single surviving request. pi's own tests serialize with
//! `await flushAutocomplete()` at each assertion point.
//!
//! This port models that as an explicit two-phase state machine:
//!
//! * [`Editor::handle_input_str`] only *enqueues* a single superseding pending
//!   request ([`Editor::request_autocomplete`]); it never settles one. This
//!   reproduces "4 keystrokes → 1 call": each keystroke overwrites the pending
//!   slot, so only the last survives.
//! * [`Editor::flush_autocomplete`] is the settle seam the replay harness calls
//!   wherever pi's tests `await flushAutocomplete()`. It starts the surviving
//!   request, queries the provider, and applies the result exactly like pi's
//!   `runAutocompleteRequest`.
//!
//! The debounce distinction (0ms vs the 20ms attachment debounce) is a pure
//! function of the text before the cursor; because both settle at the next
//! flush it has no observable effect in this model and is intentionally omitted.
//!
//! Providers sit behind the [`AutocompleteProvider`] trait. The vector replay
//! injects a recorded `(text, cursorLine, cursorCol, force) -> suggestions`
//! table (no fd / timers / network), while the host-backed implementation the
//! steward will flip in later plugs into the same seam.

use std::rc::Rc;

use fancy_regex::Regex;

// The editor's provider trait reuses C5's autocomplete data types so the
// host-backed provider the steward flips in later plugs into the same seam.
use crate::autocomplete::{AppliedCompletion, AutocompleteItem, AutocompleteSuggestions};
use crate::components::select_list::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
};
use crate::text_util::is_whitespace_char;

use super::Editor;

/// The outcome of a provider `getSuggestions` call.
///
/// `Ready` mirrors a resolved promise (the recorded-table replay always returns
/// this). `Pending` mirrors an unresolved promise that only settles on abort —
/// used by the abort-count unit test to exercise the superseding path.
pub enum SuggestionOutcome {
    /// The request resolved with suggestions (or `None` for no suggestions).
    Ready(Option<AutocompleteSuggestions>),
    /// The request has not resolved; it stays in-flight until aborted.
    Pending,
}

/// A source of autocomplete suggestions (pi's `AutocompleteProvider`).
pub trait AutocompleteProvider {
    /// Characters that naturally trigger this provider at token boundaries
    /// (`triggerCharacters`). Defaults to none.
    fn trigger_characters(&self) -> Vec<String> {
        Vec::new()
    }

    /// Get suggestions for the current text and cursor (`getSuggestions`).
    fn get_suggestions(
        &mut self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        force: bool,
    ) -> SuggestionOutcome;

    /// Apply the selected item, returning the new text/cursor (`applyCompletion`).
    fn apply_completion(
        &mut self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> AppliedCompletion;

    /// Whether explicit-Tab file completion should trigger
    /// (`shouldTriggerFileCompletion`). `None` means the provider does not
    /// define the hook (treated as "always trigger").
    fn should_trigger_file_completion(
        &mut self,
        _lines: &[String],
        _cursor_line: usize,
        _cursor_col: usize,
    ) -> Option<bool> {
        None
    }
}

/// The autocomplete UI state (`autocompleteState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutoState {
    Regular,
    Force,
}

/// A request enqueued by `handle_input`, awaiting a `flush_autocomplete`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingRequest {
    pub start_token: i64,
    pub force: bool,
    pub explicit_tab: bool,
}

/// An in-flight (started, not yet settled) request — the abort controller.
#[derive(Debug, Clone, Copy)]
pub(crate) struct InFlight {
    pub aborted: bool,
}

const DEFAULT_TRIGGER_CHARACTERS: [&str; 2] = ["@", "#"];

// pi's `escapeCharacterClass`: escape regex metacharacters for a `[...]` class.
fn escape_character_class(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        if matches!(
            c,
            '\\' | '^'
                | '$'
                | '.'
                | '*'
                | '+'
                | '?'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '|'
                | '-'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// pi's `buildTriggerPattern`.
pub(crate) fn build_trigger_pattern(trigger_characters: &[String]) -> Regex {
    let class: String = trigger_characters
        .iter()
        .map(|c| escape_character_class(c))
        .collect();
    Regex::new(&format!(r"(?:^|[\s])[{class}][^\s]*$")).expect("valid trigger pattern")
}

/// Default trigger characters as owned strings.
pub(crate) fn default_trigger_characters() -> Vec<String> {
    DEFAULT_TRIGGER_CHARACTERS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

impl Editor {
    /// Set the autocomplete provider (`setAutocompleteProvider`).
    pub fn set_autocomplete_provider(&mut self, provider: Box<dyn AutocompleteProvider>) {
        self.cancel_autocomplete();
        let triggers = provider.trigger_characters();
        self.autocomplete_provider = Some(provider);
        self.set_autocomplete_trigger_characters(&triggers);
    }

    /// The number of aborts recorded (for the abort-count behavioral test).
    pub fn autocomplete_abort_count(&self) -> u64 {
        self.autocomplete_aborts
    }

    fn set_autocomplete_trigger_characters(&mut self, trigger_characters: &[String]) {
        let mut next = default_trigger_characters();
        for character in trigger_characters {
            if character.encode_utf16().count() != 1
                || character == "/"
                || is_whitespace_char(character)
                || next.contains(character)
            {
                continue;
            }
            next.push(character.clone());
        }
        self.autocomplete_trigger_pattern = build_trigger_pattern(&next);
        self.autocomplete_trigger_characters = next;
    }

    // --- slash-command context helpers ---

    fn is_slash_menu_allowed(&self) -> bool {
        self.state.cursor_line == 0
    }

    pub(crate) fn is_at_start_of_message(&self) -> bool {
        if !self.is_slash_menu_allowed() {
            return false;
        }
        let before = self.text_before_cursor();
        let trimmed = before.trim();
        trimmed.is_empty() || trimmed == "/"
    }

    pub(crate) fn is_in_slash_command_context(&self, text_before_cursor: &str) -> bool {
        self.is_slash_menu_allowed() && text_before_cursor.trim_start().starts_with('/')
    }

    /// The text on the current line up to the cursor (UTF-16 slice).
    pub(crate) fn text_before_cursor(&self) -> String {
        let line = &self.state.lines[self.state.cursor_line];
        let units: Vec<u16> = line.encode_utf16().collect();
        super::segment::u16_slice(&units, 0, self.state.cursor_col)
    }

    // --- trigger detection (called from insert/backspace/forward-delete/move) ---

    /// Trigger/update autocomplete after a character insertion
    /// (pi's `insertCharacter` tail).
    pub(crate) fn autocomplete_after_insert(&mut self, ch: &str) {
        if self.autocomplete_state.is_none() {
            if ch == "/" && self.is_at_start_of_message() {
                self.try_trigger_autocomplete(false);
            } else if self.autocomplete_trigger_characters.iter().any(|c| c == ch) {
                let before = self.text_before_cursor();
                let units: Vec<u16> = before.encode_utf16().collect();
                let len = units.len();
                let char_before_symbol = if len >= 2 {
                    units.get(len - 2).copied()
                } else {
                    None
                };
                if len == 1
                    || char_before_symbol == Some(u16::from(b' '))
                    || char_before_symbol == Some(u16::from(b'\t'))
                {
                    self.try_trigger_autocomplete(false);
                }
            } else if is_symbol_context_char(ch) {
                // Slash-command context, or a symbol-completion context (@, #, or
                // a provider trigger). Both re-query the same way.
                let before = self.text_before_cursor();
                if self.is_in_slash_command_context(&before)
                    || pattern_matches(&self.autocomplete_trigger_pattern, &before)
                {
                    self.try_trigger_autocomplete(false);
                }
            }
        } else {
            self.update_autocomplete();
        }
    }

    /// Update/re-trigger autocomplete after a deletion (backspace/forward delete).
    pub(crate) fn autocomplete_after_delete(&mut self) {
        if self.autocomplete_state.is_some() {
            self.update_autocomplete();
        } else {
            // Re-trigger if the deletion left the cursor in a completable
            // context (slash command, or a symbol / provider trigger).
            let before = self.text_before_cursor();
            if self.is_in_slash_command_context(&before)
                || pattern_matches(&self.autocomplete_trigger_pattern, &before)
            {
                self.try_trigger_autocomplete(false);
            }
        }
    }

    /// Keep an open picker in sync after a cursor move (pi's `moveCursor` tail).
    pub(crate) fn autocomplete_after_move(&mut self) {
        if self.autocomplete_state.is_some() {
            self.update_autocomplete();
        }
    }

    // --- Tab dispatch (called from handle_input when the menu is not showing) ---

    pub(crate) fn handle_tab_completion(&mut self) {
        if self.autocomplete_provider.is_none() {
            return;
        }
        let before = self.text_before_cursor();
        if self.is_in_slash_command_context(&before) && !before.trim_start().contains(' ') {
            // handleSlashCommandCompletion
            self.request_autocomplete(false, true);
        } else {
            // forceFileAutocomplete(true)
            self.request_autocomplete(true, true);
        }
    }

    fn try_trigger_autocomplete(&mut self, explicit_tab: bool) {
        self.request_autocomplete(false, explicit_tab);
    }

    fn update_autocomplete(&mut self) {
        if self.autocomplete_state.is_none() || self.autocomplete_provider.is_none() {
            return;
        }
        let force = self.autocomplete_state == Some(AutoState::Force);
        self.request_autocomplete(force, false);
    }

    // --- request enqueue (phase 1) ---

    fn request_autocomplete(&mut self, force: bool, explicit_tab: bool) {
        if self.autocomplete_provider.is_none() {
            return;
        }

        if force {
            // shouldTriggerFileCompletion gate (absent hook => trigger).
            let lines = self.state.lines.clone();
            let cl = self.state.cursor_line;
            let cc = self.state.cursor_col;
            let mut provider = self.autocomplete_provider.take().expect("provider present");
            let gate = provider.should_trigger_file_completion(&lines, cl, cc);
            self.autocomplete_provider = Some(provider);
            if gate == Some(false) {
                return;
            }
        }

        self.cancel_autocomplete_request();
        self.autocomplete_start_token += 1;
        let start_token = self.autocomplete_start_token;
        // The debounce (0ms vs 20ms) only changes *when* pi's timer would fire;
        // both settle at the next flush, so it is not modeled here.
        self.autocomplete_pending = Some(PendingRequest {
            start_token,
            force,
            explicit_tab,
        });
    }

    // --- flush / settle (phase 2) ---

    /// Settle the single surviving pending autocomplete request
    /// (`startAutocompleteRequest` + `runAutocompleteRequest`).
    ///
    /// This is the explicit seam pi's tests drive with `await flushAutocomplete()`.
    pub fn flush_autocomplete(&mut self) {
        let Some(pending) = self.autocomplete_pending.take() else {
            return;
        };
        // Only the latest startToken is current (task-chain collapse).
        if pending.start_token != self.autocomplete_start_token {
            return;
        }
        if self.autocomplete_provider.is_none() {
            return;
        }

        // Start: allocate a request id + abort controller and snapshot state.
        self.autocomplete_request_id += 1;
        let request_id = self.autocomplete_request_id;
        self.autocomplete_in_flight = Some(InFlight { aborted: false });
        let snapshot_text = self.get_text();
        let snapshot_line = self.state.cursor_line;
        let snapshot_col = self.state.cursor_col;

        // Query the provider.
        let lines = self.state.lines.clone();
        let mut provider = self.autocomplete_provider.take().expect("provider present");
        let outcome = provider.get_suggestions(&lines, snapshot_line, snapshot_col, pending.force);
        self.autocomplete_provider = Some(provider);

        let suggestions = match outcome {
            // Unresolved: stays in-flight (only a superseding abort clears it).
            SuggestionOutcome::Pending => return,
            SuggestionOutcome::Ready(s) => s,
        };

        if !self.is_autocomplete_request_current(
            request_id,
            &snapshot_text,
            snapshot_line,
            snapshot_col,
        ) {
            return;
        }
        self.autocomplete_in_flight = None;

        let suggestions = match suggestions {
            Some(s) if !s.items.is_empty() => s,
            _ => {
                self.cancel_autocomplete();
                return;
            }
        };

        if pending.force && pending.explicit_tab && suggestions.items.len() == 1 {
            // Single force-file result: auto-apply without showing the menu.
            let item = suggestions.items[0].clone();
            let prefix = suggestions.prefix.clone();
            self.push_undo_snapshot();
            self.last_action = None;
            self.apply_completion_item(&item, &prefix);
            self.emit_change();
            return;
        }

        let state = if pending.force {
            AutoState::Force
        } else {
            AutoState::Regular
        };
        self.apply_autocomplete_suggestions(suggestions, state);
    }

    fn is_autocomplete_request_current(
        &self,
        request_id: i64,
        snapshot_text: &str,
        snapshot_line: usize,
        snapshot_col: usize,
    ) -> bool {
        let aborted = self
            .autocomplete_in_flight
            .as_ref()
            .map(|f| f.aborted)
            .unwrap_or(true);
        !aborted
            && request_id == self.autocomplete_request_id
            && self.get_text() == snapshot_text
            && self.state.cursor_line == snapshot_line
            && self.state.cursor_col == snapshot_col
    }

    fn apply_autocomplete_suggestions(
        &mut self,
        suggestions: AutocompleteSuggestions,
        state: AutoState,
    ) {
        self.autocomplete_prefix = suggestions.prefix.clone();
        let mut list = self.create_autocomplete_list(&suggestions.prefix, &suggestions.items);
        let best = get_best_autocomplete_match_index(&suggestions.items, &suggestions.prefix);
        if best >= 0 {
            list.set_selected_index(best);
        }
        self.autocomplete_list = Some(list);
        self.autocomplete_state = Some(state);
    }

    fn create_autocomplete_list(&self, prefix: &str, items: &[AutocompleteItem]) -> SelectList {
        let layout = if prefix.starts_with('/') {
            // SLASH_COMMAND_SELECT_LIST_LAYOUT
            SelectListLayoutOptions {
                min_primary_column_width: Some(12),
                max_primary_column_width: Some(32),
                truncate_primary: None,
            }
        } else {
            SelectListLayoutOptions::default()
        };
        let select_items: Vec<SelectItem> = items
            .iter()
            .map(|it| SelectItem {
                value: it.value.clone(),
                label: it.label.clone(),
                description: it.description.clone(),
            })
            .collect();
        SelectList::new(
            select_items,
            self.get_autocomplete_max_visible(),
            clone_select_list_theme(&self.select_list_theme),
            layout,
        )
    }

    // --- accept / cancel ---

    /// Handle a key while the autocomplete menu is showing (the
    /// `autocompleteState && autocompleteList` block of `handleInput`).
    ///
    /// Returns `true` when the key was fully handled (the caller should return);
    /// `false` to fall through to normal input handling — including a confirmed
    /// slash-command selection, where the Enter must also submit.
    pub(crate) fn handle_autocomplete_mode_key(&mut self, data: &str) -> bool {
        if self.autocomplete_state.is_none() || self.autocomplete_list.is_none() {
            return false;
        }

        if self.keybindings.matches(data, "tui.select.cancel") {
            self.cancel_autocomplete();
            return true;
        }

        if self.keybindings.matches(data, "tui.select.up")
            || self.keybindings.matches(data, "tui.select.down")
        {
            if let Some(list) = self.autocomplete_list.as_mut() {
                list.handle_input_str(data);
            }
            return true;
        }

        if self.keybindings.matches(data, "tui.input.tab") {
            if self.apply_selected_completion().is_some() {
                self.cancel_autocomplete();
                self.emit_change();
            }
            return true;
        }

        if self.keybindings.matches(data, "tui.select.confirm") {
            if let Some(prefix) = self.apply_selected_completion() {
                self.cancel_autocomplete();
                if prefix.starts_with('/') {
                    // Fall through to submit handling.
                    return false;
                }
                self.emit_change();
                return true;
            }
        }

        false
    }

    /// Apply the currently selected menu item (`applyCompletion` from the
    /// autocomplete-mode Tab/Enter handlers). Returns the applied prefix, or
    /// `None` when there is no selected item or provider.
    pub(crate) fn apply_selected_completion(&mut self) -> Option<String> {
        let selected = self.autocomplete_list.as_ref()?.get_selected_item()?;
        self.autocomplete_provider.as_ref()?;
        self.push_undo_snapshot();
        self.last_action = None;
        let item = AutocompleteItem {
            value: selected.value,
            label: selected.label,
            description: selected.description,
        };
        let prefix = self.autocomplete_prefix.clone();
        self.apply_completion_item(&item, &prefix);
        Some(prefix)
    }

    fn apply_completion_item(&mut self, item: &AutocompleteItem, prefix: &str) {
        let lines = self.state.lines.clone();
        let cl = self.state.cursor_line;
        let cc = self.state.cursor_col;
        let mut provider = self.autocomplete_provider.take().expect("provider present");
        let result = provider.apply_completion(&lines, cl, cc, item, prefix);
        self.autocomplete_provider = Some(provider);
        self.state.lines = result.lines;
        self.state.cursor_line = result.cursor_line;
        self.set_cursor_col(result.cursor_col);
    }

    pub(crate) fn cancel_autocomplete_request(&mut self) {
        self.autocomplete_start_token += 1;
        // Drop any scheduled (debounce) request.
        self.autocomplete_pending = None;
        // Abort any in-flight request (increments the abort counter once).
        if let Some(flight) = self.autocomplete_in_flight.take() {
            if !flight.aborted {
                self.autocomplete_aborts += 1;
            }
        }
    }

    fn clear_autocomplete_ui(&mut self) {
        self.autocomplete_state = None;
        self.autocomplete_list = None;
        self.autocomplete_prefix.clear();
    }

    /// Cancel any active request and clear the menu (`cancelAutocomplete`).
    pub(crate) fn cancel_autocomplete(&mut self) {
        self.cancel_autocomplete_request();
        self.clear_autocomplete_ui();
    }

    // --- render overlay ---

    /// Append the autocomplete menu below the editor if active. `content_width`
    /// is the editor content width; `left`/`right` are the padding strings.
    pub(crate) fn append_autocomplete_overlay(
        &self,
        result: &mut Vec<String>,
        content_width: i64,
        left_padding: &str,
        right_padding: &str,
    ) {
        if self.autocomplete_state.is_none() {
            return;
        }
        let Some(list) = &self.autocomplete_list else {
            return;
        };
        for line in list.render_lines(content_width.max(0) as usize) {
            let line_width = crate::width::visible_width(&line) as i64;
            let line_padding = " ".repeat((content_width - line_width).max(0) as usize);
            result.push(format!("{left_padding}{line}{line_padding}{right_padding}"));
        }
    }

    pub(crate) fn autocomplete_is_showing(&self) -> bool {
        self.autocomplete_state.is_some()
    }
}

// pi's `getBestAutocompleteMatchIndex`.
fn get_best_autocomplete_match_index(items: &[AutocompleteItem], prefix: &str) -> i64 {
    if prefix.is_empty() {
        return -1;
    }
    let mut first_prefix_index: i64 = -1;
    for (i, item) in items.iter().enumerate() {
        if item.value == prefix {
            return i as i64;
        }
        if first_prefix_index == -1 && item.value.starts_with(prefix) {
            first_prefix_index = i as i64;
        }
    }
    first_prefix_index
}

// JS `/[a-zA-Z0-9.\-_]/.test(char)` — true if any char in `ch` matches.
fn is_symbol_context_char(ch: &str) -> bool {
    ch.chars()
        .any(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

pub(crate) fn pattern_matches(pattern: &Regex, text: &str) -> bool {
    pattern.is_match(text).unwrap_or(false)
}

// Build a fresh `SelectListTheme` that delegates to the editor's shared theme,
// so each autocomplete `SelectList` (which owns its theme) reuses the closures
// pi shares by reference.
fn clone_select_list_theme(theme: &Rc<SelectListTheme>) -> SelectListTheme {
    let a = Rc::clone(theme);
    let b = Rc::clone(theme);
    let c = Rc::clone(theme);
    let d = Rc::clone(theme);
    let e = Rc::clone(theme);
    SelectListTheme {
        selected_prefix: Box::new(move |t| (a.selected_prefix)(t)),
        selected_text: Box::new(move |t| (b.selected_text)(t)),
        description: Box::new(move |t| (c.description)(t)),
        scroll_info: Box::new(move |t| (d.scroll_info)(t)),
        no_match: Box::new(move |t| (e.no_match)(t)),
    }
}
