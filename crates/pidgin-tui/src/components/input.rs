// straitjacket-allow-file:duplication — the UTF-16 "collect units, slice
// before/after the cursor, format! the spliced value" idiom recurs across the
// insert/delete/yank/paste methods because it faithfully mirrors pi's repeated
// `value.slice(0, i) + x + value.slice(i)` string operations; keeping each edit
// method a line-by-line mirror of its `input.ts` counterpart is deliberate.
//! Byte-exact port of `vendor/pi/packages/tui/src/components/input.ts`.
//!
//! Single-line text input with horizontal scrolling, grapheme-aware cursor
//! movement, Emacs-style kill ring, undo, word navigation, and bracketed paste.
//!
//! pi's cursor is a JavaScript string index — a UTF-16 code unit offset — so
//! this port keeps `value` as UTF-8 but performs every cursor arithmetic,
//! slice, and length computation in UTF-16 units, exactly like the source. Text
//! that is inserted, deleted, or measured is sliced on UTF-16 boundaries, which
//! always coincide with the grapheme boundaries pi's segmenter produces.

use unicode_segmentation::UnicodeSegmentation;

use crate::keybindings::{tui_keybindings, KeybindingsManager};
use crate::keys::decode_kitty_printable;
use crate::kill_ring::{KillRing, PushOpts};
use crate::renderer::{Component, CURSOR_MARKER};
use crate::text_util::is_whitespace_char;
use crate::undo_stack::UndoStack;
use crate::width::{slice_by_column, visible_width};
use crate::word_navigation::{find_word_backward, find_word_forward, WordNavOptions};

/// The last mutating action, used to coalesce undo units and accumulate kills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastAction {
    Kill,
    Yank,
    TypeWord,
}

/// A snapshot of the input for the undo stack (pi's `InputState`).
#[derive(Debug, Clone)]
struct InputState {
    value: String,
    cursor: usize,
}

/// UTF-16 length of a string (JS `string.length`).
fn u16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// UTF-16 slice `[start..end]` reconstructed as an owned `String`.
///
/// `start`/`end` are valid UTF-16 boundaries in every path pi exercises
/// (cursors land on grapheme boundaries, which never split a surrogate pair).
fn u16_slice(units: &[u16], start: usize, end: usize) -> String {
    String::from_utf16(&units[start..end]).expect("slice on a valid UTF-16 boundary")
}

/// Single-line text input component.
///
/// Callbacks (`on_submit`, `on_escape`) mirror pi's optional `onSubmit`/
/// `onEscape` fields; assign a closure to observe submit/escape.
pub struct Input {
    value: String,
    /// Cursor position in `value`, measured in UTF-16 code units.
    cursor: usize,
    /// Invoked on submit with the current value (pi's `onSubmit`).
    pub on_submit: Option<Box<dyn FnMut(String)>>,
    /// Invoked on escape/cancel (pi's `onEscape`).
    pub on_escape: Option<Box<dyn FnMut()>>,
    /// Focusable interface — set by the TUI when focus changes.
    pub focused: bool,

    // Bracketed paste mode buffering.
    paste_buffer: String,
    is_in_paste: bool,

    // Kill ring for Emacs-style kill/yank operations.
    kill_ring: KillRing,
    last_action: Option<LastAction>,

    // Undo support.
    undo_stack: UndoStack<InputState>,

    keybindings: KeybindingsManager,
}

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}

impl Input {
    /// Create an empty input.
    pub fn new() -> Self {
        Self {
            value: String::new(),
            cursor: 0,
            on_submit: None,
            on_escape: None,
            focused: false,
            paste_buffer: String::new(),
            is_in_paste: false,
            kill_ring: KillRing::new(),
            last_action: None,
            undo_stack: UndoStack::new(),
            keybindings: KeybindingsManager::new(tui_keybindings(), Vec::new()),
        }
    }

    /// Current value.
    pub fn get_value(&self) -> String {
        self.value.clone()
    }

    /// Set the value, clamping the cursor to the new length (`setValue`).
    pub fn set_value(&mut self, value: &str) {
        self.value = value.to_string();
        self.cursor = self.cursor.min(u16_len(value));
    }

    /// Handle a chunk of terminal input (`handleInput`).
    pub fn handle_input_str(&mut self, data: &str) {
        // Handle bracketed paste mode.
        // Start of paste: \x1b[200~   End of paste: \x1b[201~
        let mut data = data.to_string();

        // Check if we're starting a bracketed paste.
        if data.contains("\x1b[200~") {
            self.is_in_paste = true;
            self.paste_buffer.clear();
            // JS `String.replace(searchString, "")` removes only the first match.
            data = data.replacen("\x1b[200~", "", 1);
        }

        // If we're in a paste, buffer the data.
        if self.is_in_paste {
            self.paste_buffer.push_str(&data);

            if let Some(end_index) = self.paste_buffer.find("\x1b[201~") {
                // Extract the pasted content (byte offsets align with pi's
                // UTF-16 offsets because the ASCII marker starts on a char
                // boundary).
                let paste_content = self.paste_buffer[..end_index].to_string();

                // Process the complete paste.
                self.handle_paste(&paste_content);

                // Reset paste state.
                self.is_in_paste = false;

                // Handle any remaining input after the paste marker (6 =
                // length of \x1b[201~).
                let remaining = self.paste_buffer[end_index + 6..].to_string();
                self.paste_buffer.clear();
                if !remaining.is_empty() {
                    self.handle_input_str(&remaining);
                }
            }
            return;
        }

        // Escape/Cancel.
        if self.keybindings.matches(&data, "tui.select.cancel") {
            if let Some(cb) = self.on_escape.as_mut() {
                cb();
            }
            return;
        }

        // Undo.
        if self.keybindings.matches(&data, "tui.editor.undo") {
            self.undo();
            return;
        }

        // Submit.
        if self.keybindings.matches(&data, "tui.input.submit") || data == "\n" {
            let value = self.value.clone();
            if let Some(cb) = self.on_submit.as_mut() {
                cb(value);
            }
            return;
        }

        // Deletion.
        if self
            .keybindings
            .matches(&data, "tui.editor.deleteCharBackward")
        {
            self.handle_backspace();
            return;
        }
        if self
            .keybindings
            .matches(&data, "tui.editor.deleteCharForward")
        {
            self.handle_forward_delete();
            return;
        }
        if self
            .keybindings
            .matches(&data, "tui.editor.deleteWordBackward")
        {
            self.delete_word_backwards();
            return;
        }
        if self
            .keybindings
            .matches(&data, "tui.editor.deleteWordForward")
        {
            self.delete_word_forward();
            return;
        }
        if self
            .keybindings
            .matches(&data, "tui.editor.deleteToLineStart")
        {
            self.delete_to_line_start();
            return;
        }
        if self
            .keybindings
            .matches(&data, "tui.editor.deleteToLineEnd")
        {
            self.delete_to_line_end();
            return;
        }

        // Kill ring actions.
        if self.keybindings.matches(&data, "tui.editor.yank") {
            self.yank();
            return;
        }
        if self.keybindings.matches(&data, "tui.editor.yankPop") {
            self.yank_pop();
            return;
        }

        // Cursor movement.
        if self.keybindings.matches(&data, "tui.editor.cursorLeft") {
            self.last_action = None;
            if self.cursor > 0 {
                let units: Vec<u16> = self.value.encode_utf16().collect();
                let before_cursor = u16_slice(&units, 0, self.cursor);
                let last = before_cursor.graphemes(true).next_back();
                self.cursor -= last.map(u16_len).unwrap_or(1);
            }
            return;
        }
        if self.keybindings.matches(&data, "tui.editor.cursorRight") {
            self.last_action = None;
            let len = u16_len(&self.value);
            if self.cursor < len {
                let units: Vec<u16> = self.value.encode_utf16().collect();
                let after_cursor = u16_slice(&units, self.cursor, len);
                let first = after_cursor.graphemes(true).next();
                self.cursor += first.map(u16_len).unwrap_or(1);
            }
            return;
        }
        if self
            .keybindings
            .matches(&data, "tui.editor.cursorLineStart")
        {
            self.last_action = None;
            self.cursor = 0;
            return;
        }
        if self.keybindings.matches(&data, "tui.editor.cursorLineEnd") {
            self.last_action = None;
            self.cursor = u16_len(&self.value);
            return;
        }
        if self.keybindings.matches(&data, "tui.editor.cursorWordLeft") {
            self.move_word_backwards();
            return;
        }
        if self
            .keybindings
            .matches(&data, "tui.editor.cursorWordRight")
        {
            self.move_word_forwards();
            return;
        }

        // Kitty CSI-u printable character (e.g. \x1b[97u for 'a'). Decode before
        // the control-char check since CSI-u sequences contain \x1b, which would
        // otherwise be rejected.
        if let Some(kitty_printable) = decode_kitty_printable(&data) {
            self.insert_character(&kitty_printable);
            return;
        }

        // Regular character input — accept printable characters including
        // Unicode, but reject control characters (C0: 0x00-0x1F, DEL: 0x7F,
        // C1: 0x80-0x9F). pi tests each code point's first UTF-16 unit; for the
        // BMP that is the code point, and astral code points (>= 0x10000) map to
        // high surrogates which fall in no rejected range — so iterating chars
        // and testing the scalar value is equivalent.
        let has_control_chars = data.chars().any(|ch| {
            let code = ch as u32;
            code < 32 || code == 0x7f || (0x80..=0x9f).contains(&code)
        });
        if !has_control_chars {
            self.insert_character(&data);
        }
    }

    fn insert_character(&mut self, ch: &str) {
        // Undo coalescing: consecutive word chars coalesce into one undo unit.
        if is_whitespace_char(ch) || self.last_action != Some(LastAction::TypeWord) {
            self.push_undo();
        }
        self.last_action = Some(LastAction::TypeWord);

        let units: Vec<u16> = self.value.encode_utf16().collect();
        let len = units.len();
        let before = u16_slice(&units, 0, self.cursor);
        let after = u16_slice(&units, self.cursor, len);
        self.value = format!("{before}{ch}{after}");
        self.cursor += u16_len(ch);
    }

    fn handle_backspace(&mut self) {
        self.last_action = None;
        if self.cursor > 0 {
            self.push_undo();
            let units: Vec<u16> = self.value.encode_utf16().collect();
            let len = units.len();
            let before_cursor = u16_slice(&units, 0, self.cursor);
            let grapheme_length = before_cursor
                .graphemes(true)
                .next_back()
                .map(u16_len)
                .unwrap_or(1);
            let before = u16_slice(&units, 0, self.cursor - grapheme_length);
            let after = u16_slice(&units, self.cursor, len);
            self.value = format!("{before}{after}");
            self.cursor -= grapheme_length;
        }
    }

    fn handle_forward_delete(&mut self) {
        self.last_action = None;
        let len = u16_len(&self.value);
        if self.cursor < len {
            self.push_undo();
            let units: Vec<u16> = self.value.encode_utf16().collect();
            let after_cursor = u16_slice(&units, self.cursor, len);
            let grapheme_length = after_cursor
                .graphemes(true)
                .next()
                .map(u16_len)
                .unwrap_or(1);
            let before = u16_slice(&units, 0, self.cursor);
            let after = u16_slice(&units, self.cursor + grapheme_length, len);
            self.value = format!("{before}{after}");
        }
    }

    fn delete_to_line_start(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.push_undo();
        let units: Vec<u16> = self.value.encode_utf16().collect();
        let len = units.len();
        let deleted_text = u16_slice(&units, 0, self.cursor);
        self.kill_ring.push(
            &deleted_text,
            PushOpts {
                prepend: true,
                accumulate: self.last_action == Some(LastAction::Kill),
            },
        );
        self.last_action = Some(LastAction::Kill);
        self.value = u16_slice(&units, self.cursor, len);
        self.cursor = 0;
    }

    fn delete_to_line_end(&mut self) {
        let len = u16_len(&self.value);
        if self.cursor >= len {
            return;
        }
        self.push_undo();
        let units: Vec<u16> = self.value.encode_utf16().collect();
        let deleted_text = u16_slice(&units, self.cursor, len);
        self.kill_ring.push(
            &deleted_text,
            PushOpts {
                prepend: false,
                accumulate: self.last_action == Some(LastAction::Kill),
            },
        );
        self.last_action = Some(LastAction::Kill);
        self.value = u16_slice(&units, 0, self.cursor);
    }

    fn delete_word_backwards(&mut self) {
        if self.cursor == 0 {
            return;
        }
        // Save lastAction before cursor movement (moveWordBackwards resets it).
        let was_kill = self.last_action == Some(LastAction::Kill);

        self.push_undo();

        let old_cursor = self.cursor;
        self.move_word_backwards();
        let delete_from = self.cursor;
        self.cursor = old_cursor;

        let units: Vec<u16> = self.value.encode_utf16().collect();
        let len = units.len();
        let deleted_text = u16_slice(&units, delete_from, self.cursor);
        self.kill_ring.push(
            &deleted_text,
            PushOpts {
                prepend: true,
                accumulate: was_kill,
            },
        );
        self.last_action = Some(LastAction::Kill);

        let before = u16_slice(&units, 0, delete_from);
        let after = u16_slice(&units, self.cursor, len);
        self.value = format!("{before}{after}");
        self.cursor = delete_from;
    }

    fn delete_word_forward(&mut self) {
        let len = u16_len(&self.value);
        if self.cursor >= len {
            return;
        }
        // Save lastAction before cursor movement (moveWordForwards resets it).
        let was_kill = self.last_action == Some(LastAction::Kill);

        self.push_undo();

        let old_cursor = self.cursor;
        self.move_word_forwards();
        let delete_to = self.cursor;
        self.cursor = old_cursor;

        let units: Vec<u16> = self.value.encode_utf16().collect();
        let deleted_text = u16_slice(&units, self.cursor, delete_to);
        self.kill_ring.push(
            &deleted_text,
            PushOpts {
                prepend: false,
                accumulate: was_kill,
            },
        );
        self.last_action = Some(LastAction::Kill);

        let before = u16_slice(&units, 0, self.cursor);
        let after = u16_slice(&units, delete_to, units.len());
        self.value = format!("{before}{after}");
    }

    fn yank(&mut self) {
        let Some(text) = self.kill_ring.peek().map(str::to_string) else {
            return;
        };
        if text.is_empty() {
            return;
        }

        self.push_undo();

        let units: Vec<u16> = self.value.encode_utf16().collect();
        let len = units.len();
        let before = u16_slice(&units, 0, self.cursor);
        let after = u16_slice(&units, self.cursor, len);
        self.value = format!("{before}{text}{after}");
        self.cursor += u16_len(&text);
        self.last_action = Some(LastAction::Yank);
    }

    fn yank_pop(&mut self) {
        if self.last_action != Some(LastAction::Yank) || self.kill_ring.len() <= 1 {
            return;
        }

        self.push_undo();

        // Delete the previously yanked text (still at end of ring before
        // rotation).
        let prev_text = self
            .kill_ring
            .peek()
            .map(str::to_string)
            .unwrap_or_default();
        {
            let units: Vec<u16> = self.value.encode_utf16().collect();
            let len = units.len();
            let before = u16_slice(&units, 0, self.cursor - u16_len(&prev_text));
            let after = u16_slice(&units, self.cursor, len);
            self.value = format!("{before}{after}");
            self.cursor -= u16_len(&prev_text);
        }

        // Rotate and insert new entry.
        self.kill_ring.rotate();
        let text = self
            .kill_ring
            .peek()
            .map(str::to_string)
            .unwrap_or_default();
        {
            let units: Vec<u16> = self.value.encode_utf16().collect();
            let len = units.len();
            let before = u16_slice(&units, 0, self.cursor);
            let after = u16_slice(&units, self.cursor, len);
            self.value = format!("{before}{text}{after}");
            self.cursor += u16_len(&text);
        }
        self.last_action = Some(LastAction::Yank);
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(&InputState {
            value: self.value.clone(),
            cursor: self.cursor,
        });
    }

    fn undo(&mut self) {
        let Some(snapshot) = self.undo_stack.pop() else {
            return;
        };
        self.value = snapshot.value;
        self.cursor = snapshot.cursor;
        self.last_action = None;
    }

    fn move_word_backwards(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.last_action = None;
        self.cursor = find_word_backward(&self.value, self.cursor, &WordNavOptions::default());
    }

    fn move_word_forwards(&mut self) {
        if self.cursor >= u16_len(&self.value) {
            return;
        }
        self.last_action = None;
        self.cursor = find_word_forward(&self.value, self.cursor, &WordNavOptions::default());
    }

    fn handle_paste(&mut self, pasted_text: &str) {
        self.last_action = None;
        self.push_undo();

        // Clean the pasted text — remove newlines and carriage returns.
        let clean_text = pasted_text
            .replace("\r\n", "")
            .replace(['\r', '\n'], "")
            .replace('\t', "    ");

        // Insert at cursor position.
        let units: Vec<u16> = self.value.encode_utf16().collect();
        let len = units.len();
        let before = u16_slice(&units, 0, self.cursor);
        let after = u16_slice(&units, self.cursor, len);
        self.value = format!("{before}{clean_text}{after}");
        self.cursor += u16_len(&clean_text);
    }

    /// Render the input to a single line (`render`).
    pub fn render_lines(&self, width: usize) -> Vec<String> {
        // Calculate visible window.
        let prompt = "> ";
        let prompt_len = 2_i64; // prompt.length
        let available_width = width as i64 - prompt_len;

        if available_width <= 0 {
            return vec![prompt.to_string()];
        }

        let visible_text: String;
        let mut cursor_display = self.cursor;
        let total_width = visible_width(&self.value) as i64;
        let value_len = u16_len(&self.value);

        if total_width < available_width {
            // Everything fits (leave room for cursor at end).
            visible_text = self.value.clone();
        } else {
            // Need horizontal scrolling. Reserve one column for the cursor if it
            // is at the end.
            let scroll_width = if self.cursor == value_len {
                available_width - 1
            } else {
                available_width
            };
            let cursor_col = {
                let units: Vec<u16> = self.value.encode_utf16().collect();
                visible_width(&u16_slice(&units, 0, self.cursor)) as i64
            };

            if scroll_width > 0 {
                let half_width = scroll_width / 2; // Math.floor
                let start_col: i64 = if cursor_col < half_width {
                    // Cursor near start.
                    0
                } else if cursor_col > total_width - half_width {
                    // Cursor near end.
                    (total_width - scroll_width).max(0)
                } else {
                    // Cursor in middle.
                    (cursor_col - half_width).max(0)
                };

                visible_text = slice_by_column(&self.value, start_col, scroll_width, true);
                let before_cursor = slice_by_column(
                    &self.value,
                    start_col,
                    (cursor_col - start_col).max(0),
                    true,
                );
                cursor_display = u16_len(&before_cursor);
            } else {
                visible_text = String::new();
                cursor_display = 0;
            }
        }

        // Build line with fake cursor. Insert cursor character at cursor
        // position.
        let vt_units: Vec<u16> = visible_text.encode_utf16().collect();
        let vt_len = vt_units.len();
        let cursor_display = cursor_display.min(vt_len);
        let after_slice = u16_slice(&vt_units, cursor_display, vt_len);
        let cursor_grapheme = after_slice.graphemes(true).next();

        let before_cursor = u16_slice(&vt_units, 0, cursor_display);
        // Character at cursor, or space if at end.
        let at_cursor = cursor_grapheme.unwrap_or(" ");
        let at_len = u16_len(at_cursor);
        let after_cursor = u16_slice(&vt_units, (cursor_display + at_len).min(vt_len), vt_len);

        // Hardware cursor marker (zero-width, emitted before the fake cursor for
        // IME positioning).
        let marker = if self.focused { CURSOR_MARKER } else { "" };

        // Use inverse video to show the cursor.
        let cursor_char = format!("\x1b[7m{at_cursor}\x1b[27m");
        let text_with_cursor = format!("{before_cursor}{marker}{cursor_char}{after_cursor}");

        // Calculate visual width.
        let visual_length = visible_width(&text_with_cursor) as i64;
        let padding = " ".repeat((available_width - visual_length).max(0) as usize);
        let line = format!("{prompt}{text_with_cursor}{padding}");

        vec![line]
    }
}

impl Component for Input {
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
