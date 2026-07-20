//! Diff computation for the edit tool.
//!
//! Ported from pi's `core/tools/edit-diff.ts`. This is the pure core: line
//! ending detection/normalization, fuzzy-match normalization, BOM stripping,
//! fuzzy text finding, multi-edit application (with unchanged-line
//! preservation), and both the display diff (`generate_diff_string`) and a
//! jsdiff-compatible unified patch (`generate_unified_patch`).
//!
//! All string offsets are UTF-8 **byte** offsets. pi uses JavaScript UTF-16
//! indices, but every offset here (match index, match length, line spans,
//! slicing) is expressed in the same unit consistently, so the resulting
//! substrings are identical to pi's.
//!
//! Line diffing uses the `similar` crate. The display format is what the pi
//! tests pin: `+N`/`-N`/` N` gutters with right-padded line numbers and `...`
//! gap collapse. The fs-reading preview wrapper (`computeEditsDiff`) is
//! deferred: it belongs with the read/edit filesystem seam, so only the pure
//! `apply_edits_to_normalized_content` + `generate_*` layer lives here.

use similar::{ChangeTag, TextDiff};
use unicode_normalization::UnicodeNormalization;

/// A line-ending style detected in or applied to file content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    /// Unix `\n`.
    Lf,
    /// Windows `\r\n`.
    Crlf,
}

/// A single exact-text replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// Text to find (must match uniquely).
    pub old_text: String,
    /// Replacement text.
    pub new_text: String,
}

/// Result of applying edits: the normalized base and the new content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEditsResult {
    /// The LF-normalized original content the diff is computed against.
    pub base_content: String,
    /// The content after applying all edits.
    pub new_content: String,
}

/// Result of a fuzzy text search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatchResult {
    /// Whether a match was found.
    pub found: bool,
    /// Byte index where the match starts in `content_for_replacement`.
    pub index: usize,
    /// Byte length of the matched text.
    pub match_length: usize,
    /// Whether fuzzy (normalized) matching was used.
    pub used_fuzzy_match: bool,
    /// Content the replacement offsets index into (original or normalized).
    pub content_for_replacement: String,
}

/// The result of [`generate_diff_string`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffStringResult {
    /// The display-oriented diff text.
    pub diff: String,
    /// 1-based line number of the first change in the new file, if any.
    pub first_changed_line: Option<usize>,
}

/// Detect the dominant line ending: `\r\n` only if the first `\r\n` precedes
/// the first bare `\n`.
pub fn detect_line_ending(content: &str) -> LineEnding {
    let lf_idx = content.find('\n');
    let lf_idx = match lf_idx {
        None => return LineEnding::Lf,
        Some(i) => i,
    };
    match content.find("\r\n") {
        None => LineEnding::Lf,
        Some(crlf_idx) => {
            if crlf_idx < lf_idx {
                LineEnding::Crlf
            } else {
                LineEnding::Lf
            }
        }
    }
}

/// Normalize all line endings to `\n`.
pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Restore `\n` to the given line ending (no-op for LF).
pub fn restore_line_endings(text: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Crlf => text.replace('\n', "\r\n"),
        LineEnding::Lf => text.to_string(),
    }
}

/// Fold smart quotes/dashes/spaces to their ASCII equivalents for fuzzy match.
fn fold_char(c: char) -> char {
    match c {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
        '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
        '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
        other => other,
    }
}

/// Normalize text for fuzzy matching: NFKC, per-line trailing-whitespace strip,
/// then smart quote/dash/space folding.
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    let nfkc: String = text.nfkc().collect();
    let stripped = nfkc
        .split('\n')
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    stripped.chars().map(fold_char).collect()
}

/// The stripped BOM (if any) and the remaining text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrippedBom {
    /// The BOM that was present (`"\u{FEFF}"`) or empty.
    pub bom: String,
    /// The content with the BOM removed.
    pub text: String,
}

/// Strip a leading UTF-8 BOM if present.
pub fn strip_bom(content: &str) -> StrippedBom {
    if let Some(rest) = content.strip_prefix('\u{FEFF}') {
        StrippedBom {
            bom: "\u{FEFF}".to_string(),
            text: rest.to_string(),
        }
    } else {
        StrippedBom {
            bom: String::new(),
            text: content.to_string(),
        }
    }
}

/// Find `old_text` in `content`, exact first then fuzzy-normalized.
pub fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatchResult {
    if let Some(idx) = content.find(old_text) {
        return FuzzyMatchResult {
            found: true,
            index: idx,
            match_length: old_text.len(),
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        };
    }

    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old = normalize_for_fuzzy_match(old_text);
    match fuzzy_content.find(&fuzzy_old) {
        None => FuzzyMatchResult {
            found: false,
            index: 0,
            match_length: 0,
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        },
        Some(fi) => FuzzyMatchResult {
            found: true,
            index: fi,
            match_length: fuzzy_old.len(),
            used_fuzzy_match: true,
            content_for_replacement: fuzzy_content,
        },
    }
}

fn count_occurrences(content: &str, old_text: &str) -> usize {
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old = normalize_for_fuzzy_match(old_text);
    if fuzzy_old.is_empty() {
        return 0;
    }
    fuzzy_content.matches(&fuzzy_old).count()
}

#[derive(Debug, Clone, Copy)]
struct LineSpan {
    start: usize,
    end: usize,
}

#[derive(Debug, Clone)]
struct Replacement {
    edit_index: usize,
    match_index: usize,
    match_length: usize,
    new_text: String,
}

/// Split content into lines that keep their trailing newline.
fn split_lines_with_endings(content: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = content.as_bytes();
    let mut start = 0;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            out.push(&content[start..=i]);
            start = i + 1;
        }
    }
    if start < bytes.len() {
        out.push(&content[start..]);
    }
    out
}

fn get_line_spans(content: &str) -> Vec<LineSpan> {
    let mut offset = 0;
    split_lines_with_endings(content)
        .iter()
        .map(|line| {
            let span = LineSpan {
                start: offset,
                end: offset + line.len(),
            };
            offset = span.end;
            span
        })
        .collect()
}

struct LineRange {
    start_line: usize,
    end_line: usize,
}

fn get_replacement_line_range(lines: &[LineSpan], r: &Replacement) -> Result<LineRange, String> {
    let replacement_start = r.match_index;
    let replacement_end = r.match_index + r.match_length;

    let mut start_line: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        if replacement_start >= line.start && replacement_start < line.end {
            start_line = Some(i);
            break;
        }
    }
    let start_line =
        start_line.ok_or_else(|| "Replacement range is outside the base content.".to_string())?;

    let mut end_line = start_line;
    while end_line < lines.len() && lines[end_line].end < replacement_end {
        end_line += 1;
    }
    if end_line >= lines.len() {
        return Err("Replacement range is outside the base content.".to_string());
    }

    Ok(LineRange {
        start_line,
        end_line: end_line + 1,
    })
}

fn apply_replacements(content: &str, replacements: &[Replacement], offset: usize) -> String {
    let mut result = content.to_string();
    for r in replacements.iter().rev() {
        let mi = r.match_index - offset;
        result = format!(
            "{}{}{}",
            &result[..mi],
            r.new_text,
            &result[mi + r.match_length..]
        );
    }
    result
}

struct Group {
    start_line: usize,
    end_line: usize,
    replacements: Vec<Replacement>,
}

/// Apply replacements matched against `base_content` (a normalized view) to
/// `original_content`, preserving unchanged line blocks from the original.
pub fn apply_replacements_preserving_unchanged_lines(
    original_content: &str,
    base_content: &str,
    replacements: &[PreservingReplacement],
) -> Result<String, String> {
    let reps: Vec<Replacement> = replacements
        .iter()
        .map(|r| Replacement {
            edit_index: r.edit_index,
            match_index: r.match_index,
            match_length: r.match_length,
            new_text: r.new_text.clone(),
        })
        .collect();
    apply_replacements_preserving_unchanged_lines_inner(original_content, base_content, &reps)
}

/// Public replacement descriptor for
/// [`apply_replacements_preserving_unchanged_lines`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreservingReplacement {
    /// Index of the originating edit.
    pub edit_index: usize,
    /// Byte offset of the match in the base content.
    pub match_index: usize,
    /// Byte length of the match.
    pub match_length: usize,
    /// Replacement text.
    pub new_text: String,
}

fn apply_replacements_preserving_unchanged_lines_inner(
    original_content: &str,
    base_content: &str,
    replacements: &[Replacement],
) -> Result<String, String> {
    let original_lines = split_lines_with_endings(original_content);
    let base_lines = get_line_spans(base_content);
    if original_lines.len() != base_lines.len() {
        return Err(
            "Cannot preserve unchanged lines because the base content has a different line count."
                .to_string(),
        );
    }

    let mut sorted = replacements.to_vec();
    sorted.sort_by_key(|r| r.match_index);

    let mut groups: Vec<Group> = Vec::new();
    for r in sorted {
        let range = get_replacement_line_range(&base_lines, &r)?;
        if let Some(cur) = groups.last_mut() {
            if range.start_line < cur.end_line {
                cur.end_line = cur.end_line.max(range.end_line);
                cur.replacements.push(r);
                continue;
            }
        }
        groups.push(Group {
            start_line: range.start_line,
            end_line: range.end_line,
            replacements: vec![r],
        });
    }

    let mut original_line_index = 0;
    let mut result = String::new();
    for g in &groups {
        result.push_str(&original_lines[original_line_index..g.start_line].concat());
        let group_start_offset = base_lines[g.start_line].start;
        let group_end_offset = base_lines[g.end_line - 1].end;
        result.push_str(&apply_replacements(
            &base_content[group_start_offset..group_end_offset],
            &g.replacements,
            group_start_offset,
        ));
        original_line_index = g.end_line;
    }
    result.push_str(&original_lines[original_line_index..].concat());

    Ok(result)
}

fn not_found_error(path: &str, edit_index: usize, total_edits: usize) -> String {
    if total_edits == 1 {
        format!(
            "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
        )
    } else {
        format!(
            "Could not find edits[{edit_index}] in {path}. The oldText must match exactly including all whitespace and newlines."
        )
    }
}

fn duplicate_error(
    path: &str,
    edit_index: usize,
    total_edits: usize,
    occurrences: usize,
) -> String {
    if total_edits == 1 {
        format!(
            "Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
        )
    } else {
        format!(
            "Found {occurrences} occurrences of edits[{edit_index}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
        )
    }
}

fn empty_old_text_error(path: &str, edit_index: usize, total_edits: usize) -> String {
    if total_edits == 1 {
        format!("oldText must not be empty in {path}.")
    } else {
        format!("edits[{edit_index}].oldText must not be empty in {path}.")
    }
}

fn no_change_error(path: &str, total_edits: usize) -> String {
    if total_edits == 1 {
        format!(
            "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
        )
    } else {
        format!("No changes made to {path}. The replacements produced identical content.")
    }
}

/// Apply one or more exact-text replacements to LF-normalized content.
///
/// All edits are matched against the same original content; replacements are
/// applied in reverse order so offsets stay stable. If any edit needs fuzzy
/// matching, the operation runs in fuzzy-normalized space and overlays the
/// line-level changes onto the original so unchanged blocks keep their bytes.
pub fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[Edit],
    path: &str,
) -> Result<AppliedEditsResult, String> {
    let normalized_edits: Vec<Edit> = edits
        .iter()
        .map(|e| Edit {
            old_text: normalize_to_lf(&e.old_text),
            new_text: normalize_to_lf(&e.new_text),
        })
        .collect();

    for (i, e) in normalized_edits.iter().enumerate() {
        if e.old_text.is_empty() {
            return Err(empty_old_text_error(path, i, normalized_edits.len()));
        }
    }

    let initial_matches: Vec<FuzzyMatchResult> = normalized_edits
        .iter()
        .map(|e| fuzzy_find_text(normalized_content, &e.old_text))
        .collect();
    let used_fuzzy_match = initial_matches.iter().any(|m| m.used_fuzzy_match);
    let replacement_base_content = if used_fuzzy_match {
        normalize_for_fuzzy_match(normalized_content)
    } else {
        normalized_content.to_string()
    };

    let mut matched_edits: Vec<Replacement> = Vec::new();
    for (i, e) in normalized_edits.iter().enumerate() {
        let match_result = fuzzy_find_text(&replacement_base_content, &e.old_text);
        if !match_result.found {
            return Err(not_found_error(path, i, normalized_edits.len()));
        }
        let occurrences = count_occurrences(&replacement_base_content, &e.old_text);
        if occurrences > 1 {
            return Err(duplicate_error(
                path,
                i,
                normalized_edits.len(),
                occurrences,
            ));
        }
        matched_edits.push(Replacement {
            edit_index: i,
            match_index: match_result.index,
            match_length: match_result.match_length,
            new_text: e.new_text.clone(),
        });
    }

    matched_edits.sort_by_key(|r| r.match_index);
    for i in 1..matched_edits.len() {
        let previous = &matched_edits[i - 1];
        let current = &matched_edits[i];
        if previous.match_index + previous.match_length > current.match_index {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                previous.edit_index, current.edit_index
            ));
        }
    }

    let base_content = normalized_content.to_string();
    let new_content = if used_fuzzy_match {
        apply_replacements_preserving_unchanged_lines_inner(
            normalized_content,
            &replacement_base_content,
            &matched_edits,
        )?
    } else {
        apply_replacements(&replacement_base_content, &matched_edits, 0)
    };

    if base_content == new_content {
        return Err(no_change_error(path, normalized_edits.len()));
    }

    Ok(AppliedEditsResult {
        base_content,
        new_content,
    })
}

/// Generate a jsdiff-compatible unified patch (`--- `/`+++ ` headers, `@@`
/// hunks, and `\ No newline at end of file` markers).
pub fn generate_unified_patch(
    path: &str,
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> String {
    let diff = TextDiff::from_lines(old_content, new_content);
    let mut out: Vec<String> = Vec::new();
    out.push(format!("--- {path}"));
    out.push(format!("+++ {path}"));

    for group in diff.grouped_ops(context_lines) {
        if group.is_empty() {
            continue;
        }
        let old_start = group.first().unwrap().old_range().start;
        let new_start = group.first().unwrap().new_range().start;
        let old_end = group.last().unwrap().old_range().end;
        let new_end = group.last().unwrap().new_range().end;
        let old_len = old_end - old_start;
        let new_len = new_end - new_start;

        let o_disp = if old_len == 0 {
            old_start
        } else {
            old_start + 1
        };
        let n_disp = if new_len == 0 {
            new_start
        } else {
            new_start + 1
        };
        let old_hdr = if old_len == 1 {
            format!("{o_disp}")
        } else {
            format!("{o_disp},{old_len}")
        };
        let new_hdr = if new_len == 1 {
            format!("{n_disp}")
        } else {
            format!("{n_disp},{new_len}")
        };
        out.push(format!("@@ -{old_hdr} +{new_hdr} @@"));

        for op in &group {
            for change in diff.iter_changes(op) {
                let sign = match change.tag() {
                    ChangeTag::Equal => ' ',
                    ChangeTag::Delete => '-',
                    ChangeTag::Insert => '+',
                };
                let val = change.value();
                let text = val.strip_suffix('\n').unwrap_or(val);
                out.push(format!("{sign}{text}"));
                if change.missing_newline() {
                    out.push("\\ No newline at end of file".to_string());
                }
            }
        }
    }

    let mut joined = out.join("\n");
    joined.push('\n');
    joined
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartKind {
    Equal,
    Added,
    Removed,
}

struct DiffPart {
    kind: PartKind,
    lines: Vec<String>,
}

fn diff_line_parts(old_content: &str, new_content: &str) -> Vec<DiffPart> {
    let diff = TextDiff::from_lines(old_content, new_content);
    let mut parts: Vec<DiffPart> = Vec::new();
    for change in diff.iter_all_changes() {
        let kind = match change.tag() {
            ChangeTag::Equal => PartKind::Equal,
            ChangeTag::Insert => PartKind::Added,
            ChangeTag::Delete => PartKind::Removed,
        };
        let val = change.value();
        let text = val.strip_suffix('\n').unwrap_or(val).to_string();
        if let Some(last) = parts.last_mut() {
            if last.kind == kind {
                last.lines.push(text);
                continue;
            }
        }
        parts.push(DiffPart {
            kind,
            lines: vec![text],
        });
    }
    parts
}

/// Generate a display-oriented diff with line numbers and collapsed context.
pub fn generate_diff_string(
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> DiffStringResult {
    let parts = diff_line_parts(old_content, new_content);
    let mut output: Vec<String> = Vec::new();

    let old_lines_count = old_content.split('\n').count();
    let new_lines_count = new_content.split('\n').count();
    let max_line_num = old_lines_count.max(new_lines_count);
    let width = max_line_num.to_string().len();

    let mut old_line_num = 1usize;
    let mut new_line_num = 1usize;
    let mut last_was_change = false;
    let mut first_changed_line: Option<usize> = None;

    for i in 0..parts.len() {
        let part = &parts[i];
        let raw = &part.lines;

        if part.kind == PartKind::Added || part.kind == PartKind::Removed {
            if first_changed_line.is_none() {
                first_changed_line = Some(new_line_num);
            }
            for line in raw {
                if part.kind == PartKind::Added {
                    output.push(format!("+{:>width$} {line}", new_line_num, width = width));
                    new_line_num += 1;
                } else {
                    output.push(format!("-{:>width$} {line}", old_line_num, width = width));
                    old_line_num += 1;
                }
            }
            last_was_change = true;
        } else {
            let next_is_change = i < parts.len() - 1
                && matches!(parts[i + 1].kind, PartKind::Added | PartKind::Removed);
            let has_leading_change = last_was_change;
            let has_trailing_change = next_is_change;

            if has_leading_change && has_trailing_change {
                if raw.len() <= context_lines * 2 {
                    for line in raw {
                        output.push(format!(" {:>width$} {line}", old_line_num, width = width));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                } else {
                    let leading = &raw[..context_lines];
                    let trailing = &raw[raw.len() - context_lines..];
                    let skipped = raw.len() - leading.len() - trailing.len();
                    for line in leading {
                        output.push(format!(" {:>width$} {line}", old_line_num, width = width));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                    output.push(format!(" {} ...", " ".repeat(width)));
                    old_line_num += skipped;
                    new_line_num += skipped;
                    for line in trailing {
                        output.push(format!(" {:>width$} {line}", old_line_num, width = width));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                }
            } else if has_leading_change {
                let shown = raw.len().min(context_lines);
                let skipped = raw.len() - shown;
                for line in &raw[..shown] {
                    output.push(format!(" {:>width$} {line}", old_line_num, width = width));
                    old_line_num += 1;
                    new_line_num += 1;
                }
                if skipped > 0 {
                    output.push(format!(" {} ...", " ".repeat(width)));
                    old_line_num += skipped;
                    new_line_num += skipped;
                }
            } else if has_trailing_change {
                let skipped = raw.len().saturating_sub(context_lines);
                if skipped > 0 {
                    output.push(format!(" {} ...", " ".repeat(width)));
                    old_line_num += skipped;
                    new_line_num += skipped;
                }
                for line in &raw[skipped..] {
                    output.push(format!(" {:>width$} {line}", old_line_num, width = width));
                    old_line_num += 1;
                    new_line_num += 1;
                }
            } else {
                old_line_num += raw.len();
                new_line_num += raw.len();
            }

            last_was_change = false;
        }
    }

    DiffStringResult {
        diff: output.join("\n"),
        first_changed_line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(old: &str, new: &str) -> Edit {
        Edit {
            old_text: old.to_string(),
            new_text: new.to_string(),
        }
    }

    #[test]
    fn detect_line_ending_variants() {
        assert_eq!(detect_line_ending("a\nb"), LineEnding::Lf);
        assert_eq!(detect_line_ending("a\r\nb"), LineEnding::Crlf);
        assert_eq!(detect_line_ending("no newline"), LineEnding::Lf);
        // Bare \n before the first \r\n -> LF wins.
        assert_eq!(detect_line_ending("a\nb\r\nc"), LineEnding::Lf);
    }

    #[test]
    fn normalize_and_restore_round_trip() {
        assert_eq!(normalize_to_lf("a\r\nb\rc"), "a\nb\nc");
        assert_eq!(restore_line_endings("a\nb", LineEnding::Crlf), "a\r\nb");
        assert_eq!(restore_line_endings("a\nb", LineEnding::Lf), "a\nb");
    }

    #[test]
    fn strip_bom_detects_and_removes() {
        let r = strip_bom("\u{FEFF}hello");
        assert_eq!(r.bom, "\u{FEFF}");
        assert_eq!(r.text, "hello");
        let r2 = strip_bom("hello");
        assert_eq!(r2.bom, "");
        assert_eq!(r2.text, "hello");
    }

    #[test]
    fn fuzzy_normalize_folds_quotes_dashes_spaces() {
        assert_eq!(normalize_for_fuzzy_match("\u{2018}a\u{2019}"), "'a'");
        assert_eq!(normalize_for_fuzzy_match("\u{201C}a\u{201D}"), "\"a\"");
        assert_eq!(normalize_for_fuzzy_match("a\u{2013}b\u{2014}c"), "a-b-c");
        assert_eq!(normalize_for_fuzzy_match("a\u{00A0}b"), "a b");
    }

    #[test]
    fn fuzzy_normalize_strips_trailing_ws_per_line() {
        assert_eq!(normalize_for_fuzzy_match("a   \nb  "), "a\nb");
    }

    #[test]
    fn fuzzy_normalize_nfkc_fullwidth_and_combining() {
        assert_eq!(normalize_for_fuzzy_match("ＡＢＣ１２３"), "ABC123");
        assert_eq!(normalize_for_fuzzy_match("cafe\u{0301}"), "café");
        assert_eq!(normalize_for_fuzzy_match("你好，世界"), "你好,世界");
    }

    #[test]
    fn exact_match_preferred_over_fuzzy() {
        let r = fuzzy_find_text("const x = 'exact';", "const x = 'exact';");
        assert!(r.found);
        assert!(!r.used_fuzzy_match);
    }

    #[test]
    fn fuzzy_match_when_exact_absent() {
        let r = fuzzy_find_text("console.log(\u{2018}hi\u{2019});", "console.log('hi');");
        assert!(r.found);
        assert!(r.used_fuzzy_match);
    }

    // --- apply_edits_to_normalized_content: pi tools.test.ts edit block ---

    #[test]
    fn replaces_text() {
        let r = apply_edits_to_normalized_content(
            "Hello, world!",
            &[edit("world", "testing")],
            "f.txt",
        )
        .unwrap();
        assert_eq!(r.new_content, "Hello, testing!");
        let diff = generate_diff_string(&r.base_content, &r.new_content, 4);
        assert!(diff.diff.contains("testing"));
    }

    #[test]
    fn fails_when_text_not_found() {
        let err = apply_edits_to_normalized_content(
            "Hello, world!",
            &[edit("nonexistent", "x")],
            "f.txt",
        )
        .unwrap_err();
        assert!(err.contains("Could not find the exact text"));
    }

    #[test]
    fn fails_on_duplicate() {
        let err = apply_edits_to_normalized_content("foo foo foo", &[edit("foo", "bar")], "f.txt")
            .unwrap_err();
        assert!(err.contains("Found 3 occurrences"));
    }

    #[test]
    fn replaces_multiple_disjoint_regions() {
        let r = apply_edits_to_normalized_content(
            "alpha\nbeta\ngamma\ndelta\n",
            &[edit("alpha\n", "ALPHA\n"), edit("gamma\n", "GAMMA\n")],
            "f.txt",
        )
        .unwrap();
        assert_eq!(r.new_content, "ALPHA\nbeta\nGAMMA\ndelta\n");
        let diff = generate_diff_string(&r.base_content, &r.new_content, 4);
        assert!(diff.diff.contains("ALPHA"));
        assert!(diff.diff.contains("GAMMA"));
    }

    #[test]
    fn matches_against_original_not_incrementally() {
        let r = apply_edits_to_normalized_content(
            "foo\nbar\nbaz\n",
            &[edit("foo\n", "foo bar\n"), edit("bar\n", "BAR\n")],
            "f.txt",
        )
        .unwrap();
        assert_eq!(r.new_content, "foo bar\nBAR\nbaz\n");
    }

    #[test]
    fn fails_when_regions_overlap() {
        let err = apply_edits_to_normalized_content(
            "one\ntwo\nthree\n",
            &[
                edit("one\ntwo\n", "ONE\nTWO\n"),
                edit("two\nthree\n", "TWO\nTHREE\n"),
            ],
            "f.txt",
        )
        .unwrap_err();
        assert!(err.contains("overlap"));
    }

    #[test]
    fn empty_old_text_errors() {
        let err = apply_edits_to_normalized_content("x\n", &[edit("", "y")], "f.txt").unwrap_err();
        assert!(err.contains("oldText must not be empty"));
    }

    #[test]
    fn collapses_large_gaps_under_50_lines() {
        let lines: Vec<String> = (1..=600).map(|i| format!("line {i:03}")).collect();
        let content = format!("{}\n", lines.join("\n"));
        let r = apply_edits_to_normalized_content(
            &content,
            &[
                edit("line 100\n", "LINE 100\n"),
                edit("line 300\n", "LINE 300\n"),
                edit("line 500\n", "LINE 500\n"),
            ],
            "f.txt",
        )
        .unwrap();
        let diff = generate_diff_string(&r.base_content, &r.new_content, 4);
        assert!(diff.diff.contains("LINE 100"));
        assert!(diff.diff.contains("LINE 300"));
        assert!(diff.diff.contains("LINE 500"));
        assert!(diff.diff.contains("..."));
        assert!(!diff.diff.contains("line 250"));
        assert!(diff.diff.split('\n').count() < 50);
    }

    // --- fuzzy application behaviors ---

    #[test]
    fn fuzzy_trailing_whitespace() {
        let r = apply_edits_to_normalized_content(
            "line one   \nline two  \nline three\n",
            &[edit("line one\nline two\n", "replaced\n")],
            "f.txt",
        )
        .unwrap();
        assert_eq!(r.new_content, "replaced\nline three\n");
    }

    #[test]
    fn fuzzy_smart_quotes() {
        let r = apply_edits_to_normalized_content(
            "console.log(\u{2018}hello\u{2019});\n",
            &[edit("console.log('hello');", "console.log('world');")],
            "f.txt",
        )
        .unwrap();
        assert!(r.new_content.contains("world"));
    }

    #[test]
    fn fuzzy_detects_duplicates_after_normalization() {
        let err = apply_edits_to_normalized_content(
            "hello world   \nhello world\n",
            &[edit("hello world", "replaced")],
            "f.txt",
        )
        .unwrap_err();
        assert!(err.contains("Found 2 occurrences"));
    }

    #[test]
    fn fuzzy_multi_edit() {
        let r = apply_edits_to_normalized_content(
            "console.log(\u{2018}hello\u{2019});\nhello\u{00A0}world\n",
            &[
                edit("console.log('hello');\n", "console.log('world');\n"),
                edit("hello world\n", "hello universe\n"),
            ],
            "f.txt",
        )
        .unwrap();
        assert_eq!(r.new_content, "console.log('world');\nhello universe\n");
    }

    #[test]
    fn fuzzy_preserves_correct_occurrence_when_replacement_equals_nearby_line() {
        let original = "replace me   \nafter   \n";
        let r = apply_edits_to_normalized_content(
            original,
            &[edit("replace me\n", "after\n")],
            "f.txt",
        )
        .unwrap();
        assert_eq!(r.new_content, "after\nafter   \n");
    }

    #[test]
    fn fuzzy_preserves_untouched_lines_multi() {
        let original = [
            "keep before  ",
            "first target  ",
            "first after",
            "keep middle   ",
            "second target  ",
            "second after",
            "keep after  ",
            "",
        ]
        .join("\n");
        let r = apply_edits_to_normalized_content(
            &original,
            &[
                edit("first target\nfirst after", "FIRST\nFIRST2"),
                edit("second target\nsecond after", "SECOND\nSECOND2"),
            ],
            "f.txt",
        )
        .unwrap();
        let expected = [
            "keep before  ",
            "FIRST",
            "FIRST2",
            "keep middle   ",
            "SECOND",
            "SECOND2",
            "keep after  ",
            "",
        ]
        .join("\n");
        assert_eq!(r.new_content, expected);
    }

    // --- unified patch ---

    #[test]
    fn unified_patch_has_headers_and_changes() {
        let patch = generate_unified_patch("f.txt", "Hello, world!", "Hello, testing!", 4);
        assert!(patch.contains("--- f.txt"));
        assert!(patch.contains("+++ f.txt"));
        assert!(patch.contains("@@"));
        assert!(patch.contains("-Hello, world!"));
        assert!(patch.contains("+Hello, testing!"));
        assert!(patch.contains("\\ No newline at end of file"));
    }

    #[test]
    fn diff_string_reports_first_changed_line() {
        let r = generate_diff_string("a\nb\nc\n", "a\nB\nc\n", 4);
        assert_eq!(r.first_changed_line, Some(2));
    }
}
