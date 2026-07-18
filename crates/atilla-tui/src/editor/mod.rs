//! Byte-exact port of the core of `vendor/pi/packages/tui/src/components/editor.ts`.
//!
//! Multi-line text editor with soft word-wrap, grapheme-aware cursor movement,
//! sticky-column vertical navigation, Emacs-style kill ring, undo, prompt
//! history, bracketed paste with a paste-marker subsystem, and character jump.
//!
//! pi's cursor is a JavaScript string index — a UTF-16 code unit offset — so
//! this port stores each logical line as UTF-8 but performs every cursor
//! arithmetic, slice and length computation in UTF-16 units, exactly like the
//! source (the same convention as [`crate::components::input`]).
//!
//! # Terminal-rows seam
//!
//! pi's editor reads `tui.terminal.rows` for `maxVisibleLines` and page scroll.
//! There is no ambient TUI here, so the row count is a settable value on the
//! editor ([`Editor::set_terminal_rows`], default 24) — the first component to
//! need terminal metrics.
//!
//! # Autocomplete seam (deferred to C6b)
//!
//! The async autocomplete orchestration is intentionally not implemented here.
//! [`Editor::is_showing_autocomplete`] always reports `false`, the render path
//! never appends an autocomplete overlay, and the trigger hooks in
//! `insert_character`/`handle_backspace`/`handle_forward_delete`/`move_cursor`
//! are left as no-op seams for the follow-up PR.

mod editing;
mod layout;
mod movement;
mod segment;
mod wrap;

pub use segment::Segment;
pub use wrap::{word_wrap_line, TextChunk};

use crate::components::select_list::SelectListTheme;
use crate::keybindings::{tui_keybindings, KeybindingsManager};
use crate::keys::{decode_printable_key, matches_key};
use crate::kill_ring::KillRing;
use crate::renderer::{Component, CURSOR_MARKER};
use crate::undo_stack::UndoStack;
use crate::width::{truncate_to_width, visible_width};

use segment::u16_len;

/// The last mutating action, used to coalesce undo units and accumulate kills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LastAction {
    Kill,
    Yank,
    TypeWord,
}

/// Character-jump direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JumpDir {
    Forward,
    Backward,
}

/// A snapshot of the editor state for undo/history (pi's `EditorState`).
#[derive(Debug, Clone)]
pub(crate) struct EditorState {
    pub lines: Vec<String>,
    pub cursor_line: usize,
    /// Cursor column within `lines[cursor_line]`, in UTF-16 code units.
    pub cursor_col: usize,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
        }
    }
}

/// A visual (wrapped) line: which logical line it belongs to and its UTF-16 span.
#[derive(Debug, Clone, Copy)]
pub(crate) struct VisualLine {
    pub logical_line: usize,
    pub start_col: usize,
    pub length: usize,
}

/// The cursor position returned by [`Editor::get_cursor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub line: usize,
    pub col: usize,
}

/// Theme for the editor (pi's `EditorTheme`).
pub struct EditorTheme {
    /// Applied to the horizontal border rules and scroll indicators.
    pub border_color: Box<dyn Fn(&str) -> String>,
    /// Theme for the autocomplete select list (used by the C6b integration).
    pub select_list: SelectListTheme,
}

/// Construction options for the editor (pi's `EditorOptions`).
#[derive(Debug, Clone, Copy, Default)]
pub struct EditorOptions {
    /// Horizontal padding columns (default 0).
    pub padding_x: Option<i64>,
    /// Maximum visible autocomplete rows (clamped 3..=20, default 5).
    pub autocomplete_max_visible: Option<i64>,
}

/// Multi-line text editor component.
pub struct Editor {
    pub(crate) state: EditorState,

    /// Focusable interface — set by the TUI when focus changes.
    pub focused: bool,

    theme: EditorTheme,
    pub(crate) padding_x: usize,

    /// Last render layout width, used by cursor navigation wrapping.
    pub(crate) last_width: i64,

    /// Scroll offset into the visual-line list.
    pub(crate) scroll_offset: usize,

    /// Autocomplete UI visibility (always `false` in C6a; C6b drives it).
    pub(crate) autocomplete_showing: bool,
    autocomplete_max_visible: i64,

    // Paste tracking for large pastes (insertion-ordered, mirroring JS `Map`).
    pub(crate) pastes: Vec<(u64, String)>,
    pub(crate) paste_counter: i64,

    // Bracketed paste mode buffering.
    pub(crate) paste_buffer: String,
    pub(crate) is_in_paste: bool,

    // Prompt history for up/down navigation.
    pub(crate) history: Vec<String>,
    pub(crate) history_index: i64,
    pub(crate) history_draft: Option<EditorState>,

    // Kill ring for Emacs-style kill/yank operations.
    pub(crate) kill_ring: KillRing,
    pub(crate) last_action: Option<LastAction>,

    // Character jump mode.
    pub(crate) jump_mode: Option<JumpDir>,

    // Sticky column for vertical cursor movement.
    pub(crate) preferred_visual_col: Option<i64>,
    pub(crate) snapped_from_cursor_col: Option<usize>,

    // Undo support.
    pub(crate) undo_stack: UndoStack<EditorState>,

    pub(crate) keybindings: KeybindingsManager,

    /// Terminal row count seam (pi's `tui.terminal.rows`).
    pub(crate) terminal_rows: usize,

    /// Invoked on submit with the expanded, trimmed text (pi's `onSubmit`).
    pub on_submit: Option<Box<dyn FnMut(String)>>,
    /// Invoked whenever the text changes (pi's `onChange`).
    pub on_change: Option<Box<dyn FnMut(String)>>,
    /// When `true`, Enter does not submit (pi's `disableSubmit`).
    pub disable_submit: bool,
}

fn clamp_padding(padding: i64) -> usize {
    if padding.is_negative() {
        0
    } else {
        padding as usize
    }
}

fn clamp_max_visible(max_visible: i64) -> i64 {
    max_visible.clamp(3, 20)
}

impl Editor {
    /// Create an empty editor with the given theme and options.
    pub fn new(theme: EditorTheme, options: EditorOptions) -> Self {
        let padding_x = clamp_padding(options.padding_x.unwrap_or(0).max(0));
        let autocomplete_max_visible =
            clamp_max_visible(options.autocomplete_max_visible.unwrap_or(5));
        Self {
            state: EditorState::default(),
            focused: false,
            theme,
            padding_x,
            last_width: 80,
            scroll_offset: 0,
            autocomplete_showing: false,
            autocomplete_max_visible,
            pastes: Vec::new(),
            paste_counter: 0,
            paste_buffer: String::new(),
            is_in_paste: false,
            history: Vec::new(),
            history_index: -1,
            history_draft: None,
            kill_ring: KillRing::new(),
            last_action: None,
            jump_mode: None,
            preferred_visual_col: None,
            snapped_from_cursor_col: None,
            undo_stack: UndoStack::new(),
            keybindings: KeybindingsManager::new(tui_keybindings(), Vec::new()),
            terminal_rows: 24,
            on_submit: None,
            on_change: None,
            disable_submit: false,
        }
    }

    /// Set the terminal row count (pi's `tui.terminal.rows` seam).
    pub fn set_terminal_rows(&mut self, rows: usize) {
        self.terminal_rows = rows;
    }

    /// Current horizontal padding (`getPaddingX`).
    pub fn get_padding_x(&self) -> usize {
        self.padding_x
    }

    /// Set horizontal padding (`setPaddingX`).
    pub fn set_padding_x(&mut self, padding: i64) {
        self.padding_x = clamp_padding(padding.max(0));
    }

    /// Maximum visible autocomplete rows (`getAutocompleteMaxVisible`).
    pub fn get_autocomplete_max_visible(&self) -> i64 {
        self.autocomplete_max_visible
    }

    /// Set the maximum visible autocomplete rows (`setAutocompleteMaxVisible`).
    pub fn set_autocomplete_max_visible(&mut self, max_visible: i64) {
        self.autocomplete_max_visible = clamp_max_visible(max_visible);
    }

    /// Full document text (`getText`).
    pub fn get_text(&self) -> String {
        self.state.lines.join("\n")
    }

    /// Document text with paste markers expanded to their content
    /// (`getExpandedText`).
    pub fn get_expanded_text(&self) -> String {
        segment::expand_paste_markers(&self.state.lines.join("\n"), &self.pastes)
    }

    /// A defensive copy of the logical lines (`getLines`).
    pub fn get_lines(&self) -> Vec<String> {
        self.state.lines.clone()
    }

    /// Current cursor position (`getCursor`).
    pub fn get_cursor(&self) -> Cursor {
        Cursor {
            line: self.state.cursor_line,
            col: self.state.cursor_col,
        }
    }

    /// Whether the autocomplete menu is showing (`isShowingAutocomplete`).
    ///
    /// Always `false` in the C6a core; the C6b integration drives it.
    pub fn is_showing_autocomplete(&self) -> bool {
        self.autocomplete_showing
    }

    /// Render the editor to a list of terminal lines (`render`).
    pub fn render_lines(&mut self, width: usize) -> Vec<String> {
        let width = width as i64;
        let max_padding = ((width - 1) / 2).max(0);
        let padding_x = (self.padding_x as i64).min(max_padding);
        let content_width = (width - padding_x * 2).max(1);

        // Layout width: with padding the cursor can overflow into it; without
        // padding we reserve one column for the cursor.
        let layout_width = (content_width - if padding_x != 0 { 0 } else { 1 }).max(1);

        // Store for cursor navigation (must match wrapping width).
        self.last_width = layout_width;

        let horizontal = (self.theme.border_color)("\u{2500}");

        // Layout the text.
        let layout_lines = self.layout_text(layout_width);

        // Max visible lines: 30% of terminal height, minimum 5.
        let max_visible_lines = ((self.terminal_rows as f64 * 0.3).floor() as i64).max(5) as usize;

        // Find the cursor line index in layout_lines.
        let cursor_line_index = layout_lines.iter().position(|l| l.has_cursor).unwrap_or(0);

        // Adjust scroll offset to keep the cursor visible.
        if cursor_line_index < self.scroll_offset {
            self.scroll_offset = cursor_line_index;
        } else if cursor_line_index >= self.scroll_offset + max_visible_lines {
            self.scroll_offset = cursor_line_index - max_visible_lines + 1;
        }

        // Clamp scroll offset to the valid range.
        let max_scroll_offset = layout_lines.len().saturating_sub(max_visible_lines);
        self.scroll_offset = self.scroll_offset.min(max_scroll_offset);

        // Visible slice.
        let visible_end = (self.scroll_offset + max_visible_lines).min(layout_lines.len());
        let visible_lines = &layout_lines[self.scroll_offset..visible_end];

        let mut result: Vec<String> = Vec::new();
        let left_padding = " ".repeat(padding_x as usize);
        let right_padding = left_padding.clone();

        // Top border (with scroll-up indicator when scrolled down).
        if self.scroll_offset > 0 {
            let indicator = format!(
                "\u{2500}\u{2500}\u{2500} \u{2191} {} more ",
                self.scroll_offset
            );
            let remaining = width - visible_width(&indicator) as i64;
            if remaining >= 0 {
                let filled = format!("{indicator}{}", "\u{2500}".repeat(remaining as usize));
                result.push((self.theme.border_color)(&filled));
            } else {
                result.push((self.theme.border_color)(&truncate_to_width(
                    &indicator, width, "...", false,
                )));
            }
        } else {
            result.push(horizontal.repeat(width as usize));
        }

        // Emit the hardware cursor marker when focused (IME positioning).
        let emit_cursor_marker = self.focused;

        for layout_line in visible_lines {
            let mut display_text = layout_line.text.clone();
            let mut line_visible_width = visible_width(&layout_line.text) as i64;
            let mut cursor_in_padding = false;

            if layout_line.has_cursor {
                if let Some(cursor_pos) = layout_line.cursor_pos {
                    let units: Vec<u16> = display_text.encode_utf16().collect();
                    let len = units.len();
                    let cursor_pos = cursor_pos.min(len);
                    let before = segment::u16_slice(&units, 0, cursor_pos);
                    let after = segment::u16_slice(&units, cursor_pos, len);

                    let marker = if emit_cursor_marker {
                        CURSOR_MARKER
                    } else {
                        ""
                    };

                    if !after.is_empty() {
                        // Cursor on a grapheme — replace the first grapheme of
                        // `after` with a reverse-video copy.
                        let after_graphemes = self.segment_graphemes(&after);
                        let first_grapheme = after_graphemes
                            .first()
                            .map(|s| s.segment.clone())
                            .unwrap_or_default();
                        let first_len = u16_len(&first_grapheme);
                        let after_units: Vec<u16> = after.encode_utf16().collect();
                        let rest_after =
                            segment::u16_slice(&after_units, first_len, after_units.len());
                        let cursor = format!("\x1b[7m{first_grapheme}\x1b[0m");
                        display_text = format!("{before}{marker}{cursor}{rest_after}");
                    } else {
                        // Cursor at end — append a reverse-video space.
                        let cursor = "\x1b[7m \x1b[0m";
                        display_text = format!("{before}{marker}{cursor}");
                        line_visible_width += 1;
                        if line_visible_width > content_width && padding_x > 0 {
                            cursor_in_padding = true;
                        }
                    }
                }
            }

            let pad = " ".repeat((content_width - line_visible_width).max(0) as usize);
            let line_right_padding = if cursor_in_padding {
                right_padding.chars().skip(1).collect::<String>()
            } else {
                right_padding.clone()
            };
            result.push(format!(
                "{left_padding}{display_text}{pad}{line_right_padding}"
            ));
        }

        // Bottom border (with scroll-down indicator when more content follows).
        let lines_below =
            layout_lines.len() as i64 - (self.scroll_offset + visible_lines.len()) as i64;
        if lines_below > 0 {
            let indicator = format!("\u{2500}\u{2500}\u{2500} \u{2193} {lines_below} more ");
            let remaining = (width - visible_width(&indicator) as i64).max(0);
            let filled = format!("{indicator}{}", "\u{2500}".repeat(remaining as usize));
            result.push((self.theme.border_color)(&filled));
        } else {
            result.push(horizontal.repeat(width as usize));
        }

        // Autocomplete overlay is deferred to C6b (never appended here).

        result
    }

    /// Handle a chunk of terminal input (`handleInput`).
    pub fn handle_input_str(&mut self, data: &str) {
        let mut data = data.to_string();

        // Character jump mode (awaiting the next character to jump to).
        if self.jump_mode.is_some() {
            if self.keybindings.matches(&data, "tui.editor.jumpForward")
                || self.keybindings.matches(&data, "tui.editor.jumpBackward")
            {
                self.jump_mode = None;
                return;
            }

            let first_code = data.chars().next().map(|c| c as u32).unwrap_or(0);
            let printable = decode_printable_key(&data).or_else(|| {
                if first_code >= 32 {
                    Some(data.clone())
                } else {
                    None
                }
            });
            if let Some(printable) = printable {
                let direction = self.jump_mode.expect("jump mode set");
                self.jump_mode = None;
                self.jump_to_char(&printable, direction);
                return;
            }

            // Control character — cancel and fall through to normal handling.
            self.jump_mode = None;
        }

        // Bracketed paste mode.
        if data.contains("\x1b[200~") {
            self.is_in_paste = true;
            self.paste_buffer.clear();
            data = data.replacen("\x1b[200~", "", 1);
        }

        if self.is_in_paste {
            self.paste_buffer.push_str(&data);
            if let Some(end_index) = self.paste_buffer.find("\x1b[201~") {
                let paste_content = self.paste_buffer[..end_index].to_string();
                if !paste_content.is_empty() {
                    self.handle_paste(&paste_content);
                }
                self.is_in_paste = false;
                let remaining = self.paste_buffer[end_index + 6..].to_string();
                self.paste_buffer.clear();
                if !remaining.is_empty() {
                    self.handle_input_str(&remaining);
                }
                return;
            }
            return;
        }

        // Ctrl+C — let the parent handle exit/clear.
        if self.keybindings.matches(&data, "tui.input.copy") {
            return;
        }

        // Undo.
        if self.keybindings.matches(&data, "tui.editor.undo") {
            self.undo();
            return;
        }

        // Autocomplete-mode keys are deferred to C6b (never showing in C6a).

        // Tab — trigger completion (no-op without a provider in C6a).
        if self.keybindings.matches(&data, "tui.input.tab") && !self.autocomplete_showing {
            // C6b: handle_tab_completion() when a provider is set.
            return;
        }

        // Deletion actions.
        if self
            .keybindings
            .matches(&data, "tui.editor.deleteToLineEnd")
        {
            self.delete_to_end_of_line();
            return;
        }
        if self
            .keybindings
            .matches(&data, "tui.editor.deleteToLineStart")
        {
            self.delete_to_start_of_line();
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
            .matches(&data, "tui.editor.deleteCharBackward")
            || matches_key(&data, "shift+backspace")
        {
            self.handle_backspace();
            return;
        }
        if self
            .keybindings
            .matches(&data, "tui.editor.deleteCharForward")
            || matches_key(&data, "shift+delete")
        {
            self.handle_forward_delete();
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

        // Cursor movement actions.
        if self
            .keybindings
            .matches(&data, "tui.editor.cursorLineStart")
        {
            self.move_to_line_start();
            return;
        }
        if self.keybindings.matches(&data, "tui.editor.cursorLineEnd") {
            self.move_to_line_end();
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

        // New line.
        let first_code = data.chars().next().map(|c| c as u32).unwrap_or(0);
        if self.keybindings.matches(&data, "tui.input.newLine")
            || (first_code == 10 && data.chars().count() > 1)
            || data == "\x1b\r"
            || data == "\x1b[13;2~"
            || (data.chars().count() > 1 && data.contains('\x1b') && data.contains('\r'))
            || (data == "\n")
        {
            if self.should_submit_on_backslash_enter(&data) {
                self.handle_backspace();
                self.submit_value();
                return;
            }
            self.add_new_line();
            return;
        }

        // Submit (Enter).
        if self.keybindings.matches(&data, "tui.input.submit") {
            if self.disable_submit {
                return;
            }
            // Workaround for terminals without Shift+Enter: if the char before
            // the cursor is `\`, delete it and insert a newline instead.
            let current_line = self.state.lines[self.state.cursor_line].clone();
            let units: Vec<u16> = current_line.encode_utf16().collect();
            if self.state.cursor_col > 0 && units[self.state.cursor_col - 1] == u16::from(b'\\') {
                self.handle_backspace();
                self.add_new_line();
                return;
            }
            self.submit_value();
            return;
        }

        // Arrow key navigation (with history support).
        if self.keybindings.matches(&data, "tui.editor.cursorUp") {
            if self.is_on_first_visual_line()
                && (self.is_editor_empty() || self.history_index > -1 || self.state.cursor_col == 0)
            {
                self.navigate_history(-1);
            } else if self.is_on_first_visual_line() {
                self.move_to_line_start();
            } else {
                self.move_cursor(-1, 0);
            }
            return;
        }
        if self.keybindings.matches(&data, "tui.editor.cursorDown") {
            if self.history_index > -1 && self.is_on_last_visual_line() {
                self.navigate_history(1);
            } else if self.is_on_last_visual_line() {
                self.move_to_line_end();
            } else {
                self.move_cursor(1, 0);
            }
            return;
        }
        if self.keybindings.matches(&data, "tui.editor.cursorRight") {
            self.move_cursor(0, 1);
            return;
        }
        if self.keybindings.matches(&data, "tui.editor.cursorLeft") {
            self.move_cursor(0, -1);
            return;
        }

        // Page up/down.
        if self.keybindings.matches(&data, "tui.editor.pageUp") {
            self.page_scroll(-1);
            return;
        }
        if self.keybindings.matches(&data, "tui.editor.pageDown") {
            self.page_scroll(1);
            return;
        }

        // Character jump mode triggers.
        if self.keybindings.matches(&data, "tui.editor.jumpForward") {
            self.jump_mode = Some(JumpDir::Forward);
            return;
        }
        if self.keybindings.matches(&data, "tui.editor.jumpBackward") {
            self.jump_mode = Some(JumpDir::Backward);
            return;
        }

        // Shift+Space — insert a regular space.
        if matches_key(&data, "shift+space") {
            self.insert_character(" ", false);
            return;
        }

        if let Some(printable) = decode_printable_key(&data) {
            self.insert_character(&printable, false);
            return;
        }

        // Regular characters (printable code points >= 32).
        if first_code >= 32 {
            self.insert_character(&data, false);
        }
    }

    // --- helpers shared across submodules ---

    pub(crate) fn emit_change(&mut self) {
        if self.on_change.is_some() {
            let text = self.get_text();
            if let Some(cb) = self.on_change.as_mut() {
                cb(text);
            }
        }
    }

    fn should_submit_on_backslash_enter(&self, data: &str) -> bool {
        if self.disable_submit {
            return false;
        }
        if !matches_key(data, "enter") {
            return false;
        }
        let submit_keys = self.keybindings.get_keys("tui.input.submit");
        let has_shift_enter = submit_keys
            .iter()
            .any(|k| k == "shift+enter" || k == "shift+return");
        if !has_shift_enter {
            return false;
        }
        let current_line = &self.state.lines[self.state.cursor_line];
        let units: Vec<u16> = current_line.encode_utf16().collect();
        self.state.cursor_col > 0 && units[self.state.cursor_col - 1] == u16::from(b'\\')
    }
}

impl Component for Editor {
    fn render(&self, _width: usize) -> Vec<String> {
        // The editor's render mutates scroll state; callers needing output use
        // `render_lines`. The immutable `Component::render` is not exercised by
        // the editor's own tests.
        Vec::new()
    }

    fn handle_input(&mut self, data: &str) {
        self.handle_input_str(data);
    }

    fn invalidate(&mut self) {}
}
