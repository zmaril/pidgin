// straitjacket-allow-file:duplication — the "collect UTF-16 units, slice
// before/after the cursor, format! the spliced line" idiom recurs across the
// insert/delete/yank methods because it faithfully mirrors pi's repeated
// `line.slice(0, i) + x + line.slice(i)` operations; keeping each edit method a
// line-by-line mirror of its `editor.ts` counterpart is deliberate.
//! Editing operations for the editor, ported from `vendor/pi/packages/tui/src/components/editor.ts`:
//! character insertion, paste handling (bracketed paste + paste markers),
//! backspace/forward-delete, word/line deletion, kill ring yank/yank-pop, undo,
//! prompt history, and programmatic text mutation. Cursor arithmetic is in
//! UTF-16 units.

use std::sync::LazyLock;

use fancy_regex::Regex;

use crate::kill_ring::PushOpts;

use super::segment::{paste_marker_id, paste_marker_matches, u16_len, u16_slice};
use super::{Editor, EditorState, LastAction};

// `/\x1b\[(\d+);5u/g` — CSI-u Ctrl+letter sequences inside bracketed paste.
static CSI_U_CTRL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[(\d+);5u").unwrap());

fn u16_units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

impl Editor {
    // --- ordered paste map (mirrors JS `Map` insertion order) ---

    fn pastes_set(&mut self, id: u64, val: String) {
        if let Some(entry) = self.pastes.iter_mut().find(|(i, _)| *i == id) {
            entry.1 = val;
        } else {
            self.pastes.push((id, val));
        }
    }

    fn pastes_get(&self, id: u64) -> Option<String> {
        self.pastes
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, v)| v.clone())
    }

    fn pastes_delete(&mut self, id: u64) {
        self.pastes.retain(|(i, _)| *i != id);
    }

    // --- normalization ---

    /// Normalize text for storage (`normalizeText`): CRLF/CR -> LF, tab -> 4
    /// spaces.
    fn normalize_text(&self, text: &str) -> String {
        text.replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\t', "    ")
    }

    // --- insertion ---

    pub(crate) fn insert_character(&mut self, ch: &str, skip_undo_coalescing: bool) {
        self.exit_history_browsing();

        if !skip_undo_coalescing {
            if crate::text_util::is_whitespace_char(ch)
                || self.last_action != Some(LastAction::TypeWord)
            {
                self.push_undo_snapshot();
            }
            self.last_action = Some(LastAction::TypeWord);
        }

        let line = &self.state.lines[self.state.cursor_line];
        let units = u16_units(line);
        let len = units.len();
        let before = u16_slice(&units, 0, self.state.cursor_col);
        let after = u16_slice(&units, self.state.cursor_col, len);
        self.state.lines[self.state.cursor_line] = format!("{before}{ch}{after}");
        self.set_cursor_col(self.state.cursor_col + u16_len(ch));

        self.emit_change();

        // Autocomplete trigger detection is deferred to C6b (no provider here).
    }

    /// Insert text at the cursor as an atomic undo unit (`insertTextAtCursor`).
    pub fn insert_text_at_cursor(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.cancel_autocomplete();
        self.push_undo_snapshot();
        self.last_action = None;
        self.exit_history_browsing();
        self.insert_text_at_cursor_internal(text);
    }

    fn insert_text_at_cursor_internal(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let normalized = self.normalize_text(text);
        let inserted: Vec<&str> = normalized.split('\n').collect();

        let current_line = self.state.lines[self.state.cursor_line].clone();
        let units = u16_units(&current_line);
        let len = units.len();
        let before = u16_slice(&units, 0, self.state.cursor_col);
        let after = u16_slice(&units, self.state.cursor_col, len);

        if inserted.len() == 1 {
            self.state.lines[self.state.cursor_line] = format!("{before}{normalized}{after}");
            self.set_cursor_col(self.state.cursor_col + u16_len(&normalized));
        } else {
            let cl = self.state.cursor_line;
            let head = self.state.lines[..cl].to_vec();
            let tail = self.state.lines[cl + 1..].to_vec();
            let mut new_lines: Vec<String> = head;
            new_lines.push(format!("{before}{}", inserted[0]));
            for mid in &inserted[1..inserted.len() - 1] {
                new_lines.push((*mid).to_string());
            }
            new_lines.push(format!("{}{after}", inserted[inserted.len() - 1]));
            new_lines.extend(tail);
            self.state.lines = new_lines;
            self.state.cursor_line += inserted.len() - 1;
            self.set_cursor_col(u16_len(inserted[inserted.len() - 1]));
        }

        self.emit_change();
    }

    // --- paste ---

    pub(crate) fn handle_paste(&mut self, pasted_text: &str) {
        self.cancel_autocomplete();
        self.exit_history_browsing();
        self.last_action = None;
        self.push_undo_snapshot();

        let decoded = decode_csi_u(pasted_text);
        let clean_text = self.normalize_text(&decoded);

        // Filter out non-printable characters except newlines.
        let mut filtered_text: String = clean_text
            .chars()
            .filter(|c| *c == '\n' || (*c as u32) >= 32)
            .collect();

        // Path-space heuristic: prepend a space when pasting a path after a word.
        if filtered_text.starts_with(['/', '~', '.']) {
            let current_line = &self.state.lines[self.state.cursor_line];
            let units = u16_units(current_line);
            let char_before = if self.state.cursor_col > 0 {
                units.get(self.state.cursor_col - 1).copied()
            } else {
                None
            };
            if let Some(cb) = char_before {
                if is_word_unit(cb) {
                    filtered_text = format!(" {filtered_text}");
                }
            }
        }

        let pasted_lines: Vec<&str> = filtered_text.split('\n').collect();
        let total_chars = u16_len(&filtered_text);

        if pasted_lines.len() > 10 || total_chars > 1000 {
            self.paste_counter += 1;
            let paste_id = self.paste_counter as u64;
            self.pastes_set(paste_id, filtered_text.clone());
            let marker = if pasted_lines.len() > 10 {
                format!("[paste #{paste_id} +{} lines]", pasted_lines.len())
            } else {
                format!("[paste #{paste_id} {total_chars} chars]")
            };
            self.insert_text_at_cursor_internal(&marker);
            return;
        }

        // Single- and multi-line pastes both insert atomically.
        self.insert_text_at_cursor_internal(&filtered_text);
    }

    pub(crate) fn add_new_line(&mut self) {
        self.cancel_autocomplete();
        self.exit_history_browsing();
        self.last_action = None;
        self.push_undo_snapshot();

        let current_line = self.state.lines[self.state.cursor_line].clone();
        let units = u16_units(&current_line);
        let len = units.len();
        let before = u16_slice(&units, 0, self.state.cursor_col);
        let after = u16_slice(&units, self.state.cursor_col, len);

        self.state.lines[self.state.cursor_line] = before;
        self.state.lines.insert(self.state.cursor_line + 1, after);
        self.state.cursor_line += 1;
        self.set_cursor_col(0);

        self.emit_change();
    }

    pub(crate) fn submit_value(&mut self) {
        self.cancel_autocomplete();
        let result =
            super::segment::expand_paste_markers(&self.state.lines.join("\n"), &self.pastes)
                .trim()
                .to_string();

        self.state = EditorState::default();
        self.pastes.clear();
        self.paste_counter = 0;
        self.exit_history_browsing();
        self.scroll_offset = 0;
        self.undo_stack.clear();
        self.last_action = None;

        if let Some(cb) = self.on_change.as_mut() {
            cb(String::new());
        }
        if let Some(cb) = self.on_submit.as_mut() {
            cb(result);
        }
    }

    // --- deletion ---

    pub(crate) fn handle_backspace(&mut self) {
        self.exit_history_browsing();
        self.last_action = None;

        if self.state.cursor_col > 0 {
            self.push_undo_snapshot();

            let line = self.state.lines[self.state.cursor_line].clone();
            let units = u16_units(&line);
            let before_cursor = u16_slice(&units, 0, self.state.cursor_col);

            let graphemes = self.segment_graphemes(&before_cursor);
            let last_grapheme = graphemes.last().map(|g| g.segment.clone());
            let grapheme_length = last_grapheme.as_deref().map(u16_len).unwrap_or(1);

            if let Some(seg) = &last_grapheme {
                if let Some(target_id) = paste_marker_id(seg) {
                    self.pastes_delete(target_id);
                    self.paste_counter -= 1;
                    self.renumber_markers_after_delete(target_id);
                }
            }

            let line = self.state.lines[self.state.cursor_line].clone();
            let units = u16_units(&line);
            let len = units.len();
            let before = u16_slice(&units, 0, self.state.cursor_col - grapheme_length);
            let after = u16_slice(&units, self.state.cursor_col, len);
            self.state.lines[self.state.cursor_line] = format!("{before}{after}");
            self.set_cursor_col(self.state.cursor_col - grapheme_length);
        } else if self.state.cursor_line > 0 {
            self.push_undo_snapshot();
            let current_line = self.state.lines[self.state.cursor_line].clone();
            let previous_line = self.state.lines[self.state.cursor_line - 1].clone();
            self.state.lines[self.state.cursor_line - 1] = format!("{previous_line}{current_line}");
            self.state.lines.remove(self.state.cursor_line);
            self.state.cursor_line -= 1;
            self.set_cursor_col(u16_len(&previous_line));
        }

        self.emit_change();

        // Autocomplete re-trigger after backspace is deferred to C6b.
    }

    // Renumber markers whose id is greater than `target_id` (backspace pass).
    fn renumber_markers_after_delete(&mut self, target_id: u64) {
        let line_count = self.state.lines.len();
        for li in 0..line_count {
            let line = self.state.lines[li].clone();
            let matches = paste_marker_matches(&line);
            if matches.is_empty() {
                continue;
            }
            let mut out = String::new();
            let mut last = 0usize;
            for (bs, be, id, suffix) in matches {
                out.push_str(&line[last..bs]);
                if id <= target_id {
                    out.push_str(&line[bs..be]);
                } else {
                    let new_text = format!("[paste #{}{}]", id - 1, suffix);
                    let content = self.pastes_get(id).unwrap_or_else(|| new_text.clone());
                    self.pastes_set(id - 1, content);
                    self.pastes_delete(id);
                    out.push_str(&new_text);
                }
                last = be;
            }
            out.push_str(&line[last..]);
            self.state.lines[li] = out;
        }
    }

    pub(crate) fn handle_forward_delete(&mut self) {
        self.exit_history_browsing();
        self.last_action = None;

        let current_line = self.state.lines[self.state.cursor_line].clone();
        let units = u16_units(&current_line);
        let len = units.len();

        if self.state.cursor_col < len {
            self.push_undo_snapshot();
            let after_cursor = u16_slice(&units, self.state.cursor_col, len);
            let graphemes = self.segment_graphemes(&after_cursor);
            let grapheme_length = graphemes.first().map(|g| u16_len(&g.segment)).unwrap_or(1);
            let before = u16_slice(&units, 0, self.state.cursor_col);
            let after = u16_slice(&units, self.state.cursor_col + grapheme_length, len);
            self.state.lines[self.state.cursor_line] = format!("{before}{after}");
        } else if self.state.cursor_line < self.state.lines.len() - 1 {
            self.push_undo_snapshot();
            let next_line = self.state.lines[self.state.cursor_line + 1].clone();
            self.state.lines[self.state.cursor_line] = format!("{current_line}{next_line}");
            self.state.lines.remove(self.state.cursor_line + 1);
        }

        self.emit_change();

        // Autocomplete re-trigger after forward delete is deferred to C6b.
    }

    pub(crate) fn delete_to_start_of_line(&mut self) {
        self.exit_history_browsing();
        let current_line = self.state.lines[self.state.cursor_line].clone();
        let units = u16_units(&current_line);
        let len = units.len();

        if self.state.cursor_col > 0 {
            self.push_undo_snapshot();
            let deleted_text = u16_slice(&units, 0, self.state.cursor_col);
            let accumulate = self.last_action == Some(LastAction::Kill);
            self.kill_ring.push(
                &deleted_text,
                PushOpts {
                    prepend: true,
                    accumulate,
                },
            );
            self.last_action = Some(LastAction::Kill);
            self.state.lines[self.state.cursor_line] =
                u16_slice(&units, self.state.cursor_col, len);
            self.set_cursor_col(0);
        } else if self.state.cursor_line > 0 {
            self.push_undo_snapshot();
            let accumulate = self.last_action == Some(LastAction::Kill);
            self.kill_ring.push(
                "\n",
                PushOpts {
                    prepend: true,
                    accumulate,
                },
            );
            self.last_action = Some(LastAction::Kill);
            let previous_line = self.state.lines[self.state.cursor_line - 1].clone();
            self.state.lines[self.state.cursor_line - 1] = format!("{previous_line}{current_line}");
            self.state.lines.remove(self.state.cursor_line);
            self.state.cursor_line -= 1;
            self.set_cursor_col(u16_len(&previous_line));
        }

        self.emit_change();
    }

    pub(crate) fn delete_to_end_of_line(&mut self) {
        self.exit_history_browsing();
        let current_line = self.state.lines[self.state.cursor_line].clone();
        let units = u16_units(&current_line);
        let len = units.len();

        if self.state.cursor_col < len {
            self.push_undo_snapshot();
            let deleted_text = u16_slice(&units, self.state.cursor_col, len);
            let accumulate = self.last_action == Some(LastAction::Kill);
            self.kill_ring.push(
                &deleted_text,
                PushOpts {
                    prepend: false,
                    accumulate,
                },
            );
            self.last_action = Some(LastAction::Kill);
            self.state.lines[self.state.cursor_line] = u16_slice(&units, 0, self.state.cursor_col);
        } else if self.state.cursor_line < self.state.lines.len() - 1 {
            self.push_undo_snapshot();
            let accumulate = self.last_action == Some(LastAction::Kill);
            self.kill_ring.push(
                "\n",
                PushOpts {
                    prepend: false,
                    accumulate,
                },
            );
            self.last_action = Some(LastAction::Kill);
            let next_line = self.state.lines[self.state.cursor_line + 1].clone();
            self.state.lines[self.state.cursor_line] = format!("{current_line}{next_line}");
            self.state.lines.remove(self.state.cursor_line + 1);
        }

        self.emit_change();
    }

    pub(crate) fn delete_word_backwards(&mut self) {
        self.exit_history_browsing();
        let current_line = self.state.lines[self.state.cursor_line].clone();

        if self.state.cursor_col == 0 {
            if self.state.cursor_line > 0 {
                self.push_undo_snapshot();
                let accumulate = self.last_action == Some(LastAction::Kill);
                self.kill_ring.push(
                    "\n",
                    PushOpts {
                        prepend: true,
                        accumulate,
                    },
                );
                self.last_action = Some(LastAction::Kill);
                let previous_line = self.state.lines[self.state.cursor_line - 1].clone();
                self.state.lines[self.state.cursor_line - 1] =
                    format!("{previous_line}{current_line}");
                self.state.lines.remove(self.state.cursor_line);
                self.state.cursor_line -= 1;
                self.set_cursor_col(u16_len(&previous_line));
            }
        } else {
            self.push_undo_snapshot();
            let was_kill = self.last_action == Some(LastAction::Kill);
            let old_cursor_col = self.state.cursor_col;
            self.move_word_backwards();
            let delete_from = self.state.cursor_col;
            self.set_cursor_col(old_cursor_col);

            let units = u16_units(&current_line);
            let len = units.len();
            let deleted_text = u16_slice(&units, delete_from, self.state.cursor_col);
            self.kill_ring.push(
                &deleted_text,
                PushOpts {
                    prepend: true,
                    accumulate: was_kill,
                },
            );
            self.last_action = Some(LastAction::Kill);

            let before = u16_slice(&units, 0, delete_from);
            let after = u16_slice(&units, self.state.cursor_col, len);
            self.state.lines[self.state.cursor_line] = format!("{before}{after}");
            self.set_cursor_col(delete_from);
        }

        self.emit_change();
    }

    pub(crate) fn delete_word_forward(&mut self) {
        self.exit_history_browsing();
        let current_line = self.state.lines[self.state.cursor_line].clone();
        let line_len = u16_len(&current_line);

        if self.state.cursor_col >= line_len {
            if self.state.cursor_line < self.state.lines.len() - 1 {
                self.push_undo_snapshot();
                let accumulate = self.last_action == Some(LastAction::Kill);
                self.kill_ring.push(
                    "\n",
                    PushOpts {
                        prepend: false,
                        accumulate,
                    },
                );
                self.last_action = Some(LastAction::Kill);
                let next_line = self.state.lines[self.state.cursor_line + 1].clone();
                self.state.lines[self.state.cursor_line] = format!("{current_line}{next_line}");
                self.state.lines.remove(self.state.cursor_line + 1);
            }
        } else {
            self.push_undo_snapshot();
            let was_kill = self.last_action == Some(LastAction::Kill);
            let old_cursor_col = self.state.cursor_col;
            self.move_word_forwards();
            let delete_to = self.state.cursor_col;
            self.set_cursor_col(old_cursor_col);

            let units = u16_units(&current_line);
            let len = units.len();
            let deleted_text = u16_slice(&units, self.state.cursor_col, delete_to);
            self.kill_ring.push(
                &deleted_text,
                PushOpts {
                    prepend: false,
                    accumulate: was_kill,
                },
            );
            self.last_action = Some(LastAction::Kill);

            let before = u16_slice(&units, 0, self.state.cursor_col);
            let after = u16_slice(&units, delete_to, len);
            self.state.lines[self.state.cursor_line] = format!("{before}{after}");
        }

        self.emit_change();
    }

    // --- kill ring yank ---

    pub(crate) fn yank(&mut self) {
        if self.kill_ring.len() == 0 {
            return;
        }
        self.push_undo_snapshot();
        let text = self
            .kill_ring
            .peek()
            .map(str::to_string)
            .expect("kill ring non-empty");
        self.insert_yanked_text(&text);
        self.last_action = Some(LastAction::Yank);
    }

    pub(crate) fn yank_pop(&mut self) {
        if self.last_action != Some(LastAction::Yank) || self.kill_ring.len() <= 1 {
            return;
        }
        self.push_undo_snapshot();
        self.delete_yanked_text();
        self.kill_ring.rotate();
        let text = self
            .kill_ring
            .peek()
            .map(str::to_string)
            .expect("kill ring non-empty");
        self.insert_yanked_text(&text);
        self.last_action = Some(LastAction::Yank);
    }

    fn insert_yanked_text(&mut self, text: &str) {
        self.exit_history_browsing();
        let parts: Vec<&str> = text.split('\n').collect();

        if parts.len() == 1 {
            let current_line = self.state.lines[self.state.cursor_line].clone();
            let units = u16_units(&current_line);
            let len = units.len();
            let before = u16_slice(&units, 0, self.state.cursor_col);
            let after = u16_slice(&units, self.state.cursor_col, len);
            self.state.lines[self.state.cursor_line] = format!("{before}{text}{after}");
            self.set_cursor_col(self.state.cursor_col + u16_len(text));
        } else {
            let current_line = self.state.lines[self.state.cursor_line].clone();
            let units = u16_units(&current_line);
            let len = units.len();
            let before = u16_slice(&units, 0, self.state.cursor_col);
            let after = u16_slice(&units, self.state.cursor_col, len);

            self.state.lines[self.state.cursor_line] = format!("{before}{}", parts[0]);
            for (i, mid) in parts.iter().enumerate().take(parts.len() - 1).skip(1) {
                self.state
                    .lines
                    .insert(self.state.cursor_line + i, (*mid).to_string());
            }
            let last_line_index = self.state.cursor_line + parts.len() - 1;
            self.state.lines.insert(
                last_line_index,
                format!("{}{after}", parts[parts.len() - 1]),
            );
            self.state.cursor_line = last_line_index;
            self.set_cursor_col(u16_len(parts[parts.len() - 1]));
        }

        self.emit_change();
    }

    fn delete_yanked_text(&mut self) {
        let Some(yanked_text) = self.kill_ring.peek().map(str::to_string) else {
            return;
        };
        if yanked_text.is_empty() {
            return;
        }
        let yank_lines: Vec<&str> = yanked_text.split('\n').collect();

        if yank_lines.len() == 1 {
            let current_line = self.state.lines[self.state.cursor_line].clone();
            let units = u16_units(&current_line);
            let len = units.len();
            let delete_len = u16_len(&yanked_text);
            let before = u16_slice(&units, 0, self.state.cursor_col - delete_len);
            let after = u16_slice(&units, self.state.cursor_col, len);
            self.state.lines[self.state.cursor_line] = format!("{before}{after}");
            self.set_cursor_col(self.state.cursor_col - delete_len);
        } else {
            let start_line = self.state.cursor_line - (yank_lines.len() - 1);
            let start_col = u16_len(&self.state.lines[start_line]) - u16_len(yank_lines[0]);

            let cur_units = u16_units(&self.state.lines[self.state.cursor_line]);
            let after_cursor = u16_slice(&cur_units, self.state.cursor_col, cur_units.len());
            let start_units = u16_units(&self.state.lines[start_line]);
            let before_yank = u16_slice(&start_units, 0, start_col);

            let merged = format!("{before_yank}{after_cursor}");
            self.state.lines.splice(
                start_line..start_line + yank_lines.len(),
                std::iter::once(merged),
            );

            self.state.cursor_line = start_line;
            self.set_cursor_col(start_col);
        }

        self.emit_change();
    }

    // --- undo ---

    pub(crate) fn push_undo_snapshot(&mut self) {
        self.undo_stack.push(&self.state);
    }

    pub(crate) fn undo(&mut self) {
        self.exit_history_browsing();
        let Some(snapshot) = self.undo_stack.pop() else {
            return;
        };
        self.state = snapshot;
        self.last_action = None;
        self.preferred_visual_col = None;
        self.emit_change();
    }

    // --- history ---

    /// Add a prompt to history for up/down navigation (`addToHistory`).
    pub fn add_to_history(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.first().map(String::as_str) == Some(trimmed) {
            return;
        }
        self.history.insert(0, trimmed.to_string());
        if self.history.len() > 100 {
            self.history.pop();
        }
    }

    pub(crate) fn navigate_history(&mut self, direction: i64) {
        self.last_action = None;
        if self.history.is_empty() {
            return;
        }

        let new_index = self.history_index - direction;
        if new_index < -1 || new_index >= self.history.len() as i64 {
            return;
        }

        if self.history_index == -1 && new_index >= 0 {
            self.push_undo_snapshot();
            self.history_draft = Some(self.state.clone());
        }

        self.history_index = new_index;

        if self.history_index == -1 {
            let draft = self.history_draft.take();
            if let Some(draft) = draft {
                self.state = draft;
                self.preferred_visual_col = None;
                self.snapped_from_cursor_col = None;
                self.scroll_offset = 0;
                self.emit_change();
            } else {
                self.set_text_internal("", CursorPlacement::End);
            }
        } else {
            let text = self.history[self.history_index as usize].clone();
            let placement = if direction == -1 {
                CursorPlacement::Start
            } else {
                CursorPlacement::End
            };
            self.set_text_internal(&text, placement);
        }
    }

    pub(crate) fn exit_history_browsing(&mut self) {
        self.history_index = -1;
        self.history_draft = None;
    }

    fn set_text_internal(&mut self, text: &str, placement: CursorPlacement) {
        let parts: Vec<String> = text.split('\n').map(str::to_string).collect();
        self.state.lines = if parts.is_empty() {
            vec![String::new()]
        } else {
            parts
        };
        self.state.cursor_line = match placement {
            CursorPlacement::Start => 0,
            CursorPlacement::End => self.state.lines.len() - 1,
        };
        let col = match placement {
            CursorPlacement::Start => 0,
            CursorPlacement::End => u16_len(&self.state.lines[self.state.cursor_line]),
        };
        self.set_cursor_col(col);
        self.scroll_offset = 0;
        self.emit_change();
    }

    // --- programmatic text mutation ---

    /// Replace the whole document, normalizing and pushing an undo snapshot when
    /// the content changes (`setText`).
    pub fn set_text(&mut self, text: &str) {
        self.cancel_autocomplete();
        self.last_action = None;
        self.exit_history_browsing();
        self.pastes.clear();
        self.paste_counter = 0;
        let normalized = self.normalize_text(text);
        if self.get_text() != normalized {
            self.push_undo_snapshot();
        }
        self.set_text_internal(&normalized, CursorPlacement::End);
    }

    // --- autocomplete seam (C6b) ---

    /// Clear any autocomplete UI (`cancelAutocomplete`). In C6a there is never a
    /// request to cancel; this only resets the (always-false) UI flag.
    pub(crate) fn cancel_autocomplete(&mut self) {
        self.autocomplete_showing = false;
    }
}

#[derive(Clone, Copy)]
enum CursorPlacement {
    Start,
    End,
}

// Decode CSI-u Ctrl+letter sequences (`\x1b[<cp>;5u`) back to their literal
// control byte, mirroring pi's per-paste decode.
fn decode_csi_u(text: &str) -> String {
    let mut out = String::new();
    let mut last = 0usize;
    for caps in CSI_U_CTRL.captures_iter(text).flatten() {
        let Some(m0) = caps.get(0) else { continue };
        out.push_str(&text[last..m0.start()]);
        let code: u32 = caps
            .get(1)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0);
        if (97..=122).contains(&code) {
            out.push((code - 96) as u8 as char);
        } else if (65..=90).contains(&code) {
            out.push((code - 64) as u8 as char);
        } else {
            out.push_str(m0.as_str());
        }
        last = m0.end();
    }
    out.push_str(&text[last..]);
    out
}

// JS `/\w/` on a single UTF-16 unit: `[A-Za-z0-9_]`.
fn is_word_unit(u: u16) -> bool {
    matches!(u, 0x30..=0x39 | 0x41..=0x5a | 0x61..=0x7a | 0x5f)
}
