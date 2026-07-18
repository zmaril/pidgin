//! Verbatim port of pi's bespoke terminal table layout
//! (`renderTable` / `getLongestWordWidth` / `wrapCellText` in
//! `vendor/pi/packages/tui/src/components/markdown.ts`).

use super::{Markdown, StyleCtx};
use crate::markdown::lexer::{Kind, Token};
use crate::width::{visible_width, wrap_text_with_ansi};

const MAX_UNBROKEN_WORD_WIDTH: usize = 30;

impl Markdown {
    /// pi's `getLongestWordWidth`.
    fn longest_word_width(text: &str, max_width: Option<usize>) -> usize {
        let mut longest = 0usize;
        for word in text.split_whitespace() {
            if !word.is_empty() {
                longest = longest.max(visible_width(word));
            }
        }
        match max_width {
            None => longest,
            Some(m) => longest.min(m),
        }
    }

    fn wrap_cell_text(text: &str, max_width: usize) -> Vec<String> {
        wrap_text_with_ansi(text, max_width.max(1))
    }

    pub(super) fn render_table(
        &self,
        token: &Token,
        available_width: usize,
        next_type: Option<Kind>,
        ctx: StyleCtx,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let num_cols = token.header.len();
        if num_cols == 0 {
            return lines;
        }

        let border_overhead = 3 * num_cols + 1;
        // availableForCells may go negative in JS; track with i64.
        let available_for_cells = available_width as i64 - border_overhead as i64;
        if available_for_cells < num_cols as i64 {
            let mut fallback = if !token.raw.is_empty() {
                wrap_text_with_ansi(&token.raw, available_width)
            } else {
                Vec::new()
            };
            if let Some(nt) = next_type {
                if nt != Kind::Space {
                    fallback.push(String::new());
                }
            }
            return fallback;
        }
        let available_for_cells = available_for_cells as usize;

        // Natural + minimum column widths.
        let mut natural_widths: Vec<usize> = vec![0; num_cols];
        let mut min_word_widths: Vec<usize> = vec![0; num_cols];
        for (i, cell) in token.header.iter().enumerate() {
            let text = self.render_inline_tokens(&cell.tokens, ctx);
            natural_widths[i] = visible_width(&text);
            min_word_widths[i] =
                Self::longest_word_width(&text, Some(MAX_UNBROKEN_WORD_WIDTH)).max(1);
        }
        for row in &token.rows {
            for (i, cell) in row.iter().enumerate() {
                let text = self.render_inline_tokens(&cell.tokens, ctx);
                natural_widths[i] = natural_widths[i].max(visible_width(&text));
                min_word_widths[i] = min_word_widths[i]
                    .max(Self::longest_word_width(&text, Some(MAX_UNBROKEN_WORD_WIDTH)).max(1));
            }
        }

        let mut min_column_widths = min_word_widths.clone();
        let mut min_cells_width: usize = min_column_widths.iter().sum();

        if min_cells_width > available_for_cells {
            min_column_widths = vec![1; num_cols];
            let remaining = available_for_cells as i64 - num_cols as i64;
            if remaining > 0 {
                let remaining = remaining as usize;
                let total_weight: usize =
                    min_word_widths.iter().map(|&w| w.saturating_sub(1)).sum();
                let growth: Vec<usize> = min_word_widths
                    .iter()
                    .map(|&w| {
                        let weight = w.saturating_sub(1);
                        (weight * remaining).checked_div(total_weight).unwrap_or(0)
                    })
                    .collect();
                for i in 0..num_cols {
                    min_column_widths[i] += growth[i];
                }
                let allocated: usize = growth.iter().sum();
                let mut leftover = remaining as i64 - allocated as i64;
                let mut i = 0;
                while leftover > 0 && i < num_cols {
                    min_column_widths[i] += 1;
                    leftover -= 1;
                    i += 1;
                }
            }
            min_cells_width = min_column_widths.iter().sum();
        }

        let total_natural_width: usize = natural_widths.iter().sum::<usize>() + border_overhead;
        let column_widths: Vec<usize> = if total_natural_width <= available_width {
            natural_widths
                .iter()
                .enumerate()
                .map(|(i, &w)| w.max(min_column_widths[i]))
                .collect()
        } else {
            let total_grow_potential: usize = natural_widths
                .iter()
                .enumerate()
                .map(|(i, &w)| w.saturating_sub(min_column_widths[i]))
                .sum();
            let extra_width = available_for_cells.saturating_sub(min_cells_width);
            let mut widths: Vec<usize> = min_column_widths
                .iter()
                .enumerate()
                .map(|(i, &min_width)| {
                    let natural = natural_widths[i];
                    let min_delta = natural.saturating_sub(min_width);
                    let grow = (min_delta * extra_width)
                        .checked_div(total_grow_potential)
                        .unwrap_or(0);
                    min_width + grow
                })
                .collect();
            let allocated: usize = widths.iter().sum();
            let mut remaining = available_for_cells as i64 - allocated as i64;
            while remaining > 0 {
                let mut grew = false;
                for i in 0..num_cols {
                    if remaining <= 0 {
                        break;
                    }
                    if widths[i] < natural_widths[i] {
                        widths[i] += 1;
                        remaining -= 1;
                        grew = true;
                    }
                }
                if !grew {
                    break;
                }
            }
            widths
        };

        // Top border.
        let top_cells: Vec<String> = column_widths.iter().map(|&w| "─".repeat(w)).collect();
        lines.push(format!("┌─{}─┐", top_cells.join("─┬─")));

        // Header rows (with wrapping).
        let header_cell_lines: Vec<Vec<String>> = token
            .header
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let text = self.render_inline_tokens(&cell.tokens, ctx);
                Self::wrap_cell_text(&text, column_widths[i])
            })
            .collect();
        let header_line_count = header_cell_lines.iter().map(|c| c.len()).max().unwrap_or(0);
        for line_idx in 0..header_line_count {
            let parts: Vec<String> = header_cell_lines
                .iter()
                .enumerate()
                .map(|(col_idx, cell_lines)| {
                    let text = cell_lines.get(line_idx).cloned().unwrap_or_default();
                    let pad = column_widths[col_idx].saturating_sub(visible_width(&text));
                    let padded = format!("{text}{}", " ".repeat(pad));
                    (self.theme.bold)(&padded)
                })
                .collect();
            lines.push(format!("│ {} │", parts.join(" │ ")));
        }

        // Separator.
        let sep_cells: Vec<String> = column_widths.iter().map(|&w| "─".repeat(w)).collect();
        let separator = format!("├─{}─┤", sep_cells.join("─┼─"));
        lines.push(separator.clone());

        // Data rows.
        for (row_index, row) in token.rows.iter().enumerate() {
            let row_cell_lines: Vec<Vec<String>> = row
                .iter()
                .enumerate()
                .map(|(i, cell)| {
                    let text = self.render_inline_tokens(&cell.tokens, ctx);
                    Self::wrap_cell_text(&text, column_widths[i])
                })
                .collect();
            let row_line_count = row_cell_lines.iter().map(|c| c.len()).max().unwrap_or(0);
            for line_idx in 0..row_line_count {
                let parts: Vec<String> = row_cell_lines
                    .iter()
                    .enumerate()
                    .map(|(col_idx, cell_lines)| {
                        let text = cell_lines.get(line_idx).cloned().unwrap_or_default();
                        let pad = column_widths[col_idx].saturating_sub(visible_width(&text));
                        format!("{text}{}", " ".repeat(pad))
                    })
                    .collect();
                lines.push(format!("│ {} │", parts.join(" │ ")));
            }
            if row_index < token.rows.len() - 1 {
                lines.push(separator.clone());
            }
        }

        // Bottom border.
        let bottom_cells: Vec<String> = column_widths.iter().map(|&w| "─".repeat(w)).collect();
        lines.push(format!("└─{}─┘", bottom_cells.join("─┴─")));

        if let Some(nt) = next_type {
            if nt != Kind::Space {
                lines.push(String::new());
            }
        }
        lines
    }
}
