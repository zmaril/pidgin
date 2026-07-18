//! Cursor movement for the editor, ported from the movement methods in
//! `vendor/pi/packages/tui/src/components/editor.ts` (moveCursor,
//! moveToVisualLine, computeVerticalMoveColumn, sticky column, page scroll,
//! word movement, and character jump). All columns are UTF-16 offsets.

use crate::text_util::WordSegment;
use crate::word_navigation::{find_word_backward, find_word_forward, WordNavOptions};

use super::segment::{is_paste_marker, u16_len, u16_slice};
use super::{Editor, JumpDir, VisualLine};

impl Editor {
    /// Set cursor column, clearing sticky-column state (`setCursorCol`).
    pub(crate) fn set_cursor_col(&mut self, col: usize) {
        self.state.cursor_col = col;
        self.preferred_visual_col = None;
        self.snapped_from_cursor_col = None;
    }

    pub(crate) fn move_to_line_start(&mut self) {
        self.last_action = None;
        self.set_cursor_col(0);
    }

    pub(crate) fn move_to_line_end(&mut self) {
        self.last_action = None;
        let len = u16_len(&self.state.lines[self.state.cursor_line]);
        self.set_cursor_col(len);
    }

    /// Move the cursor to a target visual line applying sticky-column logic
    /// (`moveToVisualLine`).
    pub(crate) fn move_to_visual_line(
        &mut self,
        visual_lines: &[VisualLine],
        current_visual_line: usize,
        target_visual_line: usize,
    ) {
        let (Some(current_vl), Some(target_vl)) = (
            visual_lines.get(current_visual_line).copied(),
            visual_lines.get(target_visual_line).copied(),
        ) else {
            return;
        };

        // Resolve the source visual column (from a snapped position if any).
        let current_visual_col: i64 = if let Some(snapped) = self.snapped_from_cursor_col {
            let vl_index = self.find_visual_line_at(visual_lines, current_vl.logical_line, snapped);
            snapped as i64 - visual_lines[vl_index].start_col as i64
        } else {
            self.state.cursor_col as i64 - current_vl.start_col as i64
        };

        let is_last_source_segment = current_visual_line == visual_lines.len() - 1
            || visual_lines[current_visual_line + 1].logical_line != current_vl.logical_line;
        let source_max_visual_col = if is_last_source_segment {
            current_vl.length as i64
        } else {
            (current_vl.length as i64 - 1).max(0)
        };

        let is_last_target_segment = target_visual_line == visual_lines.len() - 1
            || visual_lines[target_visual_line + 1].logical_line != target_vl.logical_line;
        let target_max_visual_col = if is_last_target_segment {
            target_vl.length as i64
        } else {
            (target_vl.length as i64 - 1).max(0)
        };

        let move_to_visual_col = self.compute_vertical_move_column(
            current_visual_col,
            source_max_visual_col,
            target_max_visual_col,
        );

        // Set cursor position.
        self.state.cursor_line = target_vl.logical_line;
        let target_col = target_vl.start_col as i64 + move_to_visual_col;
        let logical_line = self.state.lines[target_vl.logical_line].clone();
        let logical_len = u16_len(&logical_line) as i64;
        self.state.cursor_col = target_col.min(logical_len).max(0) as usize;

        // Snap the cursor to an atomic segment boundary (e.g. paste markers).
        let segments = self.segment_graphemes(&logical_line);
        for seg in &segments {
            if seg.index > self.state.cursor_col {
                break;
            }
            let seg_len = u16_len(&seg.segment);
            if seg_len <= 1 {
                continue;
            }
            if self.state.cursor_col < seg.index + seg_len {
                let is_continuation = seg.index < target_vl.start_col;
                let is_moving_down = target_visual_line > current_visual_line;

                if is_continuation && is_moving_down {
                    let seg_end = seg.index + seg_len;
                    let mut next = target_visual_line + 1;
                    while next < visual_lines.len()
                        && visual_lines[next].logical_line == target_vl.logical_line
                        && visual_lines[next].start_col < seg_end
                    {
                        next += 1;
                    }
                    if next < visual_lines.len() {
                        self.move_to_visual_line(visual_lines, current_visual_line, next);
                        return;
                    }
                }

                self.snapped_from_cursor_col = Some(self.state.cursor_col);
                self.state.cursor_col = seg.index;
                return;
            }
        }

        // No snap occurred — we moved out of the atomic segment.
        self.snapped_from_cursor_col = None;
    }

    /// Compute the target visual column for vertical movement
    /// (`computeVerticalMoveColumn`, the 7-row sticky-column decision table).
    fn compute_vertical_move_column(
        &mut self,
        current_visual_col: i64,
        source_max_visual_col: i64,
        target_max_visual_col: i64,
    ) -> i64 {
        let has_preferred = self.preferred_visual_col.is_some(); // P
        let cursor_in_middle = current_visual_col < source_max_visual_col; // S
        let target_too_short = target_max_visual_col < current_visual_col; // T

        if !has_preferred || cursor_in_middle {
            if target_too_short {
                // Cases 2 and 7.
                self.preferred_visual_col = Some(current_visual_col);
                return target_max_visual_col;
            }
            // Cases 1 and 6.
            self.preferred_visual_col = None;
            return current_visual_col;
        }

        let preferred = self.preferred_visual_col.expect("preferred set");
        let target_cant_fit_preferred = target_max_visual_col < preferred; // U
        if target_too_short || target_cant_fit_preferred {
            // Cases 4 and 5.
            return target_max_visual_col;
        }

        // Case 3.
        self.preferred_visual_col = None;
        preferred
    }

    pub(crate) fn move_cursor(&mut self, delta_line: i64, delta_col: i64) {
        self.last_action = None;
        let visual_lines = self.build_visual_line_map(self.last_width);
        let current_visual_line = self.find_current_visual_line(&visual_lines);

        if delta_line != 0 {
            let target_visual_line = current_visual_line as i64 + delta_line;
            if target_visual_line >= 0 && (target_visual_line as usize) < visual_lines.len() {
                self.move_to_visual_line(
                    &visual_lines,
                    current_visual_line,
                    target_visual_line as usize,
                );
            }
        }

        if delta_col != 0 {
            let current_line = self.state.lines[self.state.cursor_line].clone();
            let current_len = u16_len(&current_line);
            let units: Vec<u16> = current_line.encode_utf16().collect();

            if delta_col > 0 {
                if self.state.cursor_col < current_len {
                    let after_cursor = u16_slice(&units, self.state.cursor_col, current_len);
                    let graphemes = self.segment_graphemes(&after_cursor);
                    let step = graphemes.first().map(|g| u16_len(&g.segment)).unwrap_or(1);
                    self.set_cursor_col(self.state.cursor_col + step);
                } else if self.state.cursor_line < self.state.lines.len() - 1 {
                    self.state.cursor_line += 1;
                    self.set_cursor_col(0);
                } else if let Some(current_vl) = visual_lines.get(current_visual_line) {
                    self.preferred_visual_col =
                        Some(self.state.cursor_col as i64 - current_vl.start_col as i64);
                }
            } else if self.state.cursor_col > 0 {
                let before_cursor = u16_slice(&units, 0, self.state.cursor_col);
                let graphemes = self.segment_graphemes(&before_cursor);
                let step = graphemes.last().map(|g| u16_len(&g.segment)).unwrap_or(1);
                self.set_cursor_col(self.state.cursor_col - step);
            } else if self.state.cursor_line > 0 {
                self.state.cursor_line -= 1;
                let prev_len = u16_len(&self.state.lines[self.state.cursor_line]);
                self.set_cursor_col(prev_len);
            }
        }

        // Autocomplete refresh on cursor movement is deferred to C6b.
    }

    pub(crate) fn page_scroll(&mut self, direction: i64) {
        self.last_action = None;
        let page_size = ((self.terminal_rows as f64 * 0.3).floor() as i64).max(5);

        let visual_lines = self.build_visual_line_map(self.last_width);
        let current_visual_line = self.find_current_visual_line(&visual_lines);
        let target = (current_visual_line as i64 + direction * page_size)
            .clamp(0, visual_lines.len() as i64 - 1);
        self.move_to_visual_line(&visual_lines, current_visual_line, target as usize);
    }

    pub(crate) fn move_word_backwards(&mut self) {
        self.last_action = None;
        let current_line = self.state.lines[self.state.cursor_line].clone();

        if self.state.cursor_col == 0 {
            if self.state.cursor_line > 0 {
                self.state.cursor_line -= 1;
                let prev_len = u16_len(&self.state.lines[self.state.cursor_line]);
                self.set_cursor_col(prev_len);
            }
            return;
        }

        let new_col = {
            let opts = self.word_nav_options();
            find_word_backward(&current_line, self.state.cursor_col, &opts)
        };
        self.set_cursor_col(new_col);
    }

    pub(crate) fn move_word_forwards(&mut self) {
        self.last_action = None;
        let current_line = self.state.lines[self.state.cursor_line].clone();

        if self.state.cursor_col >= u16_len(&current_line) {
            if self.state.cursor_line < self.state.lines.len() - 1 {
                self.state.cursor_line += 1;
                self.set_cursor_col(0);
            }
            return;
        }

        let new_col = {
            let opts = self.word_nav_options();
            find_word_forward(&current_line, self.state.cursor_col, &opts)
        };
        self.set_cursor_col(new_col);
    }

    // Marker-aware word-navigation options borrowing `self` for `validPasteIds`.
    fn word_nav_options(&self) -> WordNavOptions<'_> {
        WordNavOptions {
            segment: Some(Box::new(move |text: &str| {
                self.segment_words(text)
                    .into_iter()
                    .map(|w| WordSegment {
                        segment: w.segment,
                        is_word_like: w.is_word_like,
                    })
                    .collect()
            })),
            is_atomic_segment: Some(Box::new(|s: &str| is_paste_marker(s))),
        }
    }

    /// Jump to the first occurrence of `ch` in the given direction
    /// (`jumpToChar`). Multi-line, case-sensitive, skips the cursor position.
    pub(crate) fn jump_to_char(&mut self, ch: &str, direction: JumpDir) {
        self.last_action = None;
        let is_forward = direction == JumpDir::Forward;
        let needle: Vec<u16> = ch.encode_utf16().collect();

        let line_count = self.state.lines.len() as i64;
        let end: i64 = if is_forward { line_count } else { -1 };
        let step: i64 = if is_forward { 1 } else { -1 };

        let mut line_idx = self.state.cursor_line as i64;
        while line_idx != end {
            let line = &self.state.lines[line_idx as usize];
            let units: Vec<u16> = line.encode_utf16().collect();
            let is_current_line = line_idx == self.state.cursor_line as i64;

            let idx = if is_forward {
                let from = if is_current_line {
                    self.state.cursor_col as i64 + 1
                } else {
                    0
                };
                u16_index_of(&units, &needle, from)
            } else {
                let from = if is_current_line {
                    Some(self.state.cursor_col as i64 - 1)
                } else {
                    None
                };
                u16_last_index_of(&units, &needle, from)
            };

            if idx != -1 {
                self.state.cursor_line = line_idx as usize;
                self.set_cursor_col(idx as usize);
                return;
            }

            line_idx += step;
        }
        // No match — cursor stays in place.
    }
}

// JS `String.indexOf(needle, from)` in UTF-16 units.
fn u16_index_of(hay: &[u16], needle: &[u16], from: i64) -> i64 {
    let n = needle.len();
    let h = hay.len();
    if n == 0 {
        return from.clamp(0, h as i64);
    }
    if n > h {
        return -1;
    }
    let start = from.max(0);
    let mut i = start;
    let last = (h - n) as i64;
    while i <= last {
        if &hay[i as usize..i as usize + n] == needle {
            return i;
        }
        i += 1;
    }
    -1
}

// JS `String.lastIndexOf(needle, fromIndex)` in UTF-16 units.
fn u16_last_index_of(hay: &[u16], needle: &[u16], from: Option<i64>) -> i64 {
    let n = needle.len();
    let h = hay.len();
    if n == 0 {
        return match from {
            Some(f) => f.clamp(0, h as i64),
            None => h as i64,
        };
    }
    if n > h {
        return -1;
    }
    let max_start = (h - n) as i64;
    let hi = match from {
        Some(f) => f.max(0).min(max_start),
        None => max_start,
    };
    let mut start = hi;
    while start >= 0 {
        if &hay[start as usize..start as usize + n] == needle {
            return start;
        }
        start -= 1;
    }
    -1
}
