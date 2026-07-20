//! Text layout and visual-line mapping for the editor, ported from
//! `layoutText` / `buildVisualLineMap` / `findVisualLineAt` in
//! `vendor/pi/packages/tui/src/components/editor.ts`.

use std::collections::BTreeSet;

use crate::width::visible_width;

use super::segment::{
    segment_graphemes_with_markers, segment_words_with_markers, u16_len, Segment, WordSeg,
};
use super::wrap::word_wrap_line;
use super::{Editor, VisualLine};

/// A laid-out visual line with optional cursor placement (pi's `LayoutLine`).
pub(crate) struct LayoutLine {
    pub text: String,
    pub has_cursor: bool,
    pub cursor_pos: Option<usize>,
}

impl Editor {
    /// Set of currently valid paste ids (`validPasteIds`).
    pub(crate) fn valid_paste_ids(&self) -> BTreeSet<u64> {
        self.pastes.iter().map(|(id, _)| *id).collect()
    }

    /// Grapheme segmentation of `text` with valid paste markers merged
    /// (`segment(text, "grapheme")`).
    pub(crate) fn segment_graphemes(&self, text: &str) -> Vec<Segment> {
        segment_graphemes_with_markers(text, &self.valid_paste_ids())
    }

    /// Word segmentation of `text` with valid paste markers merged
    /// (`segment(text, "word")`).
    pub(crate) fn segment_words(&self, text: &str) -> Vec<WordSeg> {
        segment_words_with_markers(text, &self.valid_paste_ids())
    }

    /// Lay out the logical lines into visual lines with cursor placement
    /// (`layoutText`).
    pub(crate) fn layout_text(&self, content_width: i64) -> Vec<LayoutLine> {
        let mut layout_lines: Vec<LayoutLine> = Vec::new();

        if self.state.lines.is_empty()
            || (self.state.lines.len() == 1 && self.state.lines[0].is_empty())
        {
            layout_lines.push(LayoutLine {
                text: String::new(),
                has_cursor: true,
                cursor_pos: Some(0),
            });
            return layout_lines;
        }

        for i in 0..self.state.lines.len() {
            let line = &self.state.lines[i];
            let is_current_line = i == self.state.cursor_line;
            let line_visible_width = visible_width(line) as i64;

            if line_visible_width <= content_width {
                if is_current_line {
                    layout_lines.push(LayoutLine {
                        text: line.clone(),
                        has_cursor: true,
                        cursor_pos: Some(self.state.cursor_col),
                    });
                } else {
                    layout_lines.push(LayoutLine {
                        text: line.clone(),
                        has_cursor: false,
                        cursor_pos: None,
                    });
                }
            } else {
                let segments = self.segment_graphemes(line);
                let chunks = word_wrap_line(line, content_width, Some(&segments));
                let chunk_count = chunks.len();

                for (chunk_index, chunk) in chunks.iter().enumerate() {
                    let cursor_pos = self.state.cursor_col;
                    let is_last_chunk = chunk_index == chunk_count - 1;

                    let mut has_cursor_in_chunk = false;
                    let mut adjusted_cursor_pos = 0usize;

                    if is_current_line {
                        if is_last_chunk {
                            has_cursor_in_chunk = cursor_pos >= chunk.start_index;
                            adjusted_cursor_pos = cursor_pos.saturating_sub(chunk.start_index);
                        } else {
                            has_cursor_in_chunk =
                                cursor_pos >= chunk.start_index && cursor_pos < chunk.end_index;
                            if has_cursor_in_chunk {
                                adjusted_cursor_pos = cursor_pos - chunk.start_index;
                                let text_len = u16_len(&chunk.text);
                                if adjusted_cursor_pos > text_len {
                                    adjusted_cursor_pos = text_len;
                                }
                            }
                        }
                    }

                    layout_lines.push(LayoutLine {
                        text: chunk.text.clone(),
                        has_cursor: has_cursor_in_chunk,
                        cursor_pos: if has_cursor_in_chunk {
                            Some(adjusted_cursor_pos)
                        } else {
                            None
                        },
                    });
                }
            }
        }

        layout_lines
    }

    /// Build a mapping from visual lines to logical positions
    /// (`buildVisualLineMap`).
    pub(crate) fn build_visual_line_map(&self, width: i64) -> Vec<VisualLine> {
        let mut visual_lines: Vec<VisualLine> = Vec::new();

        for i in 0..self.state.lines.len() {
            let line = &self.state.lines[i];
            let line_vis_width = visible_width(line) as i64;
            if line.is_empty() {
                visual_lines.push(VisualLine {
                    logical_line: i,
                    start_col: 0,
                    length: 0,
                });
            } else if line_vis_width <= width {
                visual_lines.push(VisualLine {
                    logical_line: i,
                    start_col: 0,
                    length: u16_len(line),
                });
            } else {
                let segments = self.segment_graphemes(line);
                let chunks = word_wrap_line(line, width, Some(&segments));
                for chunk in chunks {
                    visual_lines.push(VisualLine {
                        logical_line: i,
                        start_col: chunk.start_index,
                        length: chunk.end_index - chunk.start_index,
                    });
                }
            }
        }

        visual_lines
    }

    /// Find the visual line index containing the logical position
    /// (`findVisualLineAt`).
    pub(crate) fn find_visual_line_at(
        &self,
        visual_lines: &[VisualLine],
        line: usize,
        col: usize,
    ) -> usize {
        for i in 0..visual_lines.len() {
            let vl = &visual_lines[i];
            if vl.logical_line != line {
                continue;
            }
            if col < vl.start_col {
                continue;
            }
            let offset = col - vl.start_col;
            let is_last_segment_of_line =
                i == visual_lines.len() - 1 || visual_lines[i + 1].logical_line != vl.logical_line;
            if offset < vl.length || (is_last_segment_of_line && offset == vl.length) {
                return i;
            }
        }
        visual_lines.len() - 1
    }

    /// Find the visual line index for the current cursor position
    /// (`findCurrentVisualLine`).
    pub(crate) fn find_current_visual_line(&self, visual_lines: &[VisualLine]) -> usize {
        self.find_visual_line_at(visual_lines, self.state.cursor_line, self.state.cursor_col)
    }

    pub(crate) fn is_editor_empty(&self) -> bool {
        self.state.lines.len() == 1 && self.state.lines[0].is_empty()
    }

    pub(crate) fn is_on_first_visual_line(&self) -> bool {
        let visual_lines = self.build_visual_line_map(self.last_width);
        self.find_current_visual_line(&visual_lines) == 0
    }

    pub(crate) fn is_on_last_visual_line(&self) -> bool {
        let visual_lines = self.build_visual_line_map(self.last_width);
        self.find_current_visual_line(&visual_lines) == visual_lines.len() - 1
    }
}
