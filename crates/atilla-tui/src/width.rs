//! Bit-exact Rust port of the width-related parts of pi's TUI utils module
//! (`vendor/pi/packages/tui/src/utils.ts`, pi v0.80.10, pinned submodule
//! `3da591a`).
//!
//! pi's renderer crashes on any width mismatch, so every function here is a
//! faithful reproduction of pi's logic and is validated byte-for-byte against
//! vectors extracted from pi itself (see `crates/atilla-tui/tests/vectors/` and
//! the generators under `crates/atilla-tui/vectors/gen/`).
//!
//! Divergences from pi, and from the Rust unicode crates, are documented inline
//! with `DIVERGENCE:` comments and summarised in the crate's width notes.

use unicode_segmentation::UnicodeSegmentation;

use crate::eaw_table::WIDE_OR_FULLWIDTH_RANGES;
use crate::unicode_tables::{
    CJK_BREAK, CONTROL, DEFAULT_IGNORABLE, EMOJI, EMOJI_MODIFIER, EMOJI_MODIFIER_BASE, FORMAT,
    MARK, RGI_SINGLE, RGI_VS16_BASE,
};

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const TAB: u8 = b'\t';
const ZWJ: u32 = 0x200d;
const VS16: u32 = 0xfe0f;
const KEYCAP: u32 = 0x20e3;
const TAG_END: u32 = 0xe007f;

/// Binary-search membership test over a sorted, non-overlapping range table.
fn in_ranges(cp: u32, ranges: &[(u32, u32)]) -> bool {
    ranges
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                std::cmp::Ordering::Greater
            } else if cp > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

fn is_default_ignorable(cp: u32) -> bool {
    in_ranges(cp, DEFAULT_IGNORABLE)
}
fn is_control(cp: u32) -> bool {
    in_ranges(cp, CONTROL)
}
fn is_mark(cp: u32) -> bool {
    in_ranges(cp, MARK)
}
fn is_format(cp: u32) -> bool {
    in_ranges(cp, FORMAT)
}
fn is_emoji_char(cp: u32) -> bool {
    in_ranges(cp, EMOJI)
}
/// `String.fromCodePoint(cp)` is a complete RGI emoji on its own.
fn is_rgi_single(cp: u32) -> bool {
    in_ranges(cp, RGI_SINGLE)
}
/// `String.fromCodePoint(cp) + "\u{FE0F}"` is a complete RGI emoji.
fn is_rgi_vs16_base(cp: u32) -> bool {
    in_ranges(cp, RGI_VS16_BASE)
}
fn is_emoji_modifier(cp: u32) -> bool {
    in_ranges(cp, EMOJI_MODIFIER)
}
fn is_emoji_modifier_base(cp: u32) -> bool {
    in_ranges(cp, EMOJI_MODIFIER_BASE)
}
fn is_regional_indicator(cp: u32) -> bool {
    (0x1f1e6..=0x1f1ff).contains(&cp)
}

/// `cjkBreakRegex.test(segment)` — true when any char in the segment belongs to
/// one of the CJK scripts pi breaks lines on.
fn is_cjk_break(segment: &str) -> bool {
    segment.chars().any(|c| in_ranges(c as u32, CJK_BREAK))
}

/// `eastAsianWidth(cp)` from get-east-asian-width@1.6.0 called with the default
/// `ambiguousAsWide = false`: returns 2 for FullWidth or Wide, else 1.
///
/// DIVERGENCE: pi does NOT use the `unicode-width` crate's model. `unicode-width`
/// bakes in emoji/VS handling, zero-width classes and a different (often newer)
/// Unicode revision, so its answers disagree with get-east-asian-width on
/// ambiguous, combining and emoji codepoints. We instead port
/// get-east-asian-width's exact FullWidth+Wide ranges (see `eaw_table.rs`).
fn east_asian_width(cp: u32) -> i64 {
    if in_ranges(cp, WIDE_OR_FULLWIDTH_RANGES) {
        2
    } else {
        1
    }
}

/// `couldBeEmoji` fast prefilter (utils.ts:27). Note `segment.length` in pi is a
/// UTF-16 code-unit count, so we compare against `encode_utf16().count()`.
fn could_be_emoji(segment: &str) -> bool {
    let Some(first) = segment.chars().next() else {
        return false;
    };
    let cp = first as u32;
    (0x1f000..=0x1fbff).contains(&cp)
        || (0x2300..=0x23ff).contains(&cp)
        || (0x2600..=0x27bf).contains(&cp)
        || (0x2b50..=0x2b55).contains(&cp)
        || segment.contains('\u{fe0f}')
        || segment.encode_utf16().count() > 2
}

/// `zeroWidthRegex.test(segment)` — the whole cluster is one-or-more of
/// Default_Ignorable / Control / Mark / Surrogate.
///
/// DIVERGENCE: `\p{Surrogate}` is dropped. Rust `&str` is guaranteed valid
/// UTF-8 and can never contain a lone surrogate, so that alternative is
/// unreachable in the port.
fn is_zero_width_cluster(segment: &str) -> bool {
    let mut any = false;
    for c in segment.chars() {
        any = true;
        let cp = c as u32;
        if !(is_default_ignorable(cp) || is_control(cp) || is_mark(cp)) {
            return false;
        }
    }
    any
}

/// `leadingNonPrintingRegex`: is `cp` in the leading-strip class
/// (Default_Ignorable / Control / Format / Mark / Surrogate).
fn is_leading_non_printing(cp: u32) -> bool {
    is_default_ignorable(cp) || is_control(cp) || is_format(cp) || is_mark(cp)
}

/// Structural `\p{RGI_Emoji}` confirmation. JS uses the v-flag sequence property;
/// there is no direct Rust equivalent, so we reproduce the UTS-51 sequence
/// grammar (basic emoji, presentation sequences, keycaps, flags, modifier
/// sequences, tag sequences and ZWJ sequences) over the codepoints of the
/// already-segmented grapheme cluster, using emoji base-property tables
/// extracted from the same V8/ICU engine pi runs on.
///
/// DIVERGENCE: this accepts any structurally-valid ZWJ/flag sequence rather than
/// only those on Unicode's curated RGI list. For terminal width this is
/// width-equivalent to pi: any grapheme pi's fallback would size differently
/// starts with a wide base codepoint that already resolves to 2, and every case
/// in the extracted vector corpus agrees. See width-notes.md.
fn is_rgi_emoji(cps: &[u32]) -> bool {
    if cps.is_empty() {
        return false;
    }

    // Emoji_Flag_Sequence: exactly two regional indicators.
    if cps.len() == 2 && is_regional_indicator(cps[0]) && is_regional_indicator(cps[1]) {
        return true;
    }

    // Emoji_Tag_Sequence: base emoji, tag spec chars, terminated by CANCEL TAG.
    if cps.len() >= 2
        && *cps.last().unwrap() == TAG_END
        && is_emoji_char(cps[0])
        && cps[1..cps.len() - 1]
            .iter()
            .all(|&c| (0xe0020..=0xe007e).contains(&c))
    {
        return true;
    }

    // RGI_Emoji_ZWJ_Sequence: two or more emoji elements joined by ZWJ.
    if cps.contains(&ZWJ) {
        let parts: Vec<&[u32]> = cps.split(|&c| c == ZWJ).collect();
        return parts.len() >= 2 && parts.iter().all(|p| is_zwj_element(p));
    }

    is_emoji_element(cps)
}

/// A single (non-ZWJ) emoji element: basic emoji, presentation sequence, keycap,
/// or modifier sequence.
fn is_emoji_element(cps: &[u32]) -> bool {
    match cps.len() {
        // Basic_Emoji: a single codepoint that renders as emoji by default.
        1 => is_rgi_single(cps[0]),
        2 => {
            // Basic_Emoji presentation sequence (X VS16) — only for the exact
            // set of bases V8 accepts, so `# VS16`, `* VS16` and `<digit> VS16`
            // are correctly rejected (they need the U+20E3 keycap).
            (cps[1] == VS16 && is_rgi_vs16_base(cps[0]))
                // RGI_Emoji_Modifier_Sequence.
                || (is_emoji_modifier(cps[1]) && is_emoji_modifier_base(cps[0]))
        }
        3 => {
            // Emoji_Keycap_Sequence: [0-9 # *] VS16 U+20E3.
            let keycap_base = matches!(cps[0], 0x30..=0x39 | 0x23 | 0x2a);
            (keycap_base && cps[1] == VS16 && cps[2] == KEYCAP)
                // Emoji-presentation base + VS16 + skin-tone modifier.
                || (is_rgi_vs16_base(cps[0]) && cps[1] == VS16 && is_emoji_modifier(cps[2]))
        }
        _ => false,
    }
}

/// An emoji element as it appears inside a ZWJ sequence. Slightly more lenient
/// than [`is_emoji_element`]: bare emoji chars (not only presentation-default)
/// and flag pairs may appear as ZWJ members.
fn is_zwj_element(part: &[u32]) -> bool {
    if part.len() == 2 && is_regional_indicator(part[0]) && is_regional_indicator(part[1]) {
        return true;
    }
    if part.len() == 1 && is_emoji_char(part[0]) {
        return true;
    }
    is_emoji_element(part)
}

/// `graphemeWidth` (utils.ts:167). Not exported by pi; validated via single
/// graphemes fed through `visibleWidth`.
fn grapheme_width(segment: &str) -> i64 {
    if segment == "\t" {
        return 3;
    }

    if is_zero_width_cluster(segment) {
        return 0;
    }

    if could_be_emoji(segment) {
        let cps: Vec<u32> = segment.chars().map(|c| c as u32).collect();
        if is_rgi_emoji(&cps) {
            return 2;
        }
    }

    // Strip the leading non-printing run, then take the base codepoint.
    let base_cp = segment
        .chars()
        .map(|c| c as u32)
        .find(|&cp| !is_leading_non_printing(cp));
    let Some(cp) = base_cp else {
        return 0;
    };

    if is_regional_indicator(cp) {
        return 2;
    }

    let mut width = east_asian_width(cp);

    // Trailing halfwidth/fullwidth forms and Thai/Lao SARA AM that segment with a
    // base. pi iterates `segment.slice(1)` (drop the first UTF-16 code unit); we
    // drop the first char. When the first char is astral, pi additionally visits
    // a lone low surrogate that contributes 0, so the totals match.
    if segment.chars().count() > 1 || segment.encode_utf16().count() > 1 {
        for c in segment.chars().skip(1) {
            let c = c as u32;
            if (0xff00..=0xffef).contains(&c) {
                width += east_asian_width(c);
            } else if c == 0x0e33 || c == 0x0eb3 {
                width += 1;
            }
        }
    }

    width
}

fn is_printable_ascii(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

/// `visibleWidth` (utils.ts:216). The 512-entry width cache pi keeps is a pure
/// performance optimisation and is not observable by any test, so it is omitted.
pub fn visible_width(s: &str) -> usize {
    if s.is_empty() {
        return 0;
    }
    if is_printable_ascii(s) {
        return s.len();
    }

    // Normalize: tabs to 3 spaces, then strip ANSI escape codes.
    let tabbed;
    let after_tabs: &str = if s.contains('\t') {
        tabbed = s.replace('\t', "   ");
        &tabbed
    } else {
        s
    };

    let stripped;
    let clean: &str = if after_tabs.contains('\u{1b}') {
        stripped = strip_ansi(after_tabs);
        &stripped
    } else {
        after_tabs
    };

    let mut width: i64 = 0;
    for g in clean.graphemes(true) {
        width += grapheme_width(g);
    }
    width as usize
}

/// Strip every supported ANSI/OSC/APC escape sequence in a single pass.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if let Some((_, len)) = extract_ansi_code(s, i) {
            i += len;
            continue;
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// `extractAnsiCode` (utils.ts:311). Returns the sequence text and its byte
/// length, or `None` when there is no complete supported sequence at `pos`.
///
/// pi indexes by UTF-16 code units; we index by UTF-8 bytes. Every character
/// that terminates or delimits these sequences (ESC, `[`, `]`, `_`, the CSI
/// terminators, BEL, `\`) is ASCII, and UTF-8 continuation bytes never collide
/// with ASCII, so byte scanning yields byte-identical sequence text.
pub fn extract_ansi_code(s: &str, pos: usize) -> Option<(String, usize)> {
    let b = s.as_bytes();
    if pos >= b.len() || b[pos] != ESC {
        return None;
    }
    match b.get(pos + 1).copied() {
        // CSI: ESC [ ... one of m G K H J
        Some(b'[') => {
            let mut j = pos + 2;
            while j < b.len() && !matches!(b[j], b'm' | b'G' | b'K' | b'H' | b'J') {
                j += 1;
            }
            if j < b.len() {
                Some((s[pos..j + 1].to_string(), j + 1 - pos))
            } else {
                None
            }
        }
        // OSC / APC: ESC ] ... or ESC _ ... terminated by BEL or ST (ESC \).
        Some(b']') | Some(b'_') => {
            let mut j = pos + 2;
            while j < b.len() {
                if b[j] == BEL {
                    return Some((s[pos..j + 1].to_string(), j + 1 - pos));
                }
                if b[j] == ESC && b.get(j + 1) == Some(&b'\\') {
                    return Some((s[pos..j + 2].to_string(), j + 2 - pos));
                }
                j += 1;
            }
            None
        }
        _ => None,
    }
}

/// `normalizeTerminalOutput` (utils.ts:284).
pub fn normalize_terminal_output(s: &str) -> String {
    let mut normalized = std::borrow::Cow::Borrowed(s);
    if s.contains('\u{0e33}') || s.contains('\u{0eb3}') {
        let mut decomposed = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '\u{0e33}' => decomposed.push_str("\u{0e4d}\u{0e32}"),
                '\u{0eb3}' => decomposed.push_str("\u{0ecd}\u{0eb2}"),
                other => decomposed.push(other),
            }
        }
        normalized = std::borrow::Cow::Owned(decomposed);
    }
    if !normalized.contains('\t') {
        return normalized.into_owned();
    }

    let text = normalized.as_ref();
    let mut result = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if let Some((code, len)) = extract_ansi_code(text, i) {
            result.push_str(&code);
            i += len;
            continue;
        }
        let ch = text[i..].chars().next().unwrap();
        if ch == '\t' {
            result.push_str("   ");
        } else {
            result.push(ch);
        }
        i += ch.len_utf8();
    }
    result
}

// -------------------------------------------------------------------------
// ANSI SGR / OSC 8 state tracking (utils.ts:351-610).
// -------------------------------------------------------------------------

#[derive(Clone)]
struct ActiveHyperlink {
    params: String,
    url: String,
    /// The original terminator: BEL ("\x07") or ST ("\x1b\\").
    terminator_bel: bool,
}

fn parse_osc8_hyperlink(ansi_code: &str) -> OscParse {
    if !ansi_code.starts_with("\u{1b}]8;") {
        return OscParse::NotOsc8;
    }
    let terminator_bel = ansi_code.ends_with('\u{07}');
    let body = if terminator_bel {
        &ansi_code[4..ansi_code.len() - 1]
    } else {
        &ansi_code[4..ansi_code.len() - 2]
    };
    let Some(sep) = body.find(';') else {
        return OscParse::NotOsc8;
    };
    let params = &body[..sep];
    let url = &body[sep + 1..];
    if url.is_empty() {
        return OscParse::Close;
    }
    OscParse::Open(ActiveHyperlink {
        params: params.to_string(),
        url: url.to_string(),
        terminator_bel,
    })
}

/// Mirrors pi's tri-state: `undefined` (not an OSC 8 code / malformed),
/// `null` (an OSC 8 close), or a parsed hyperlink.
enum OscParse {
    NotOsc8,
    Close,
    Open(ActiveHyperlink),
}

fn format_osc8_hyperlink(link: &ActiveHyperlink) -> String {
    let term = if link.terminator_bel {
        "\u{07}"
    } else {
        "\u{1b}\\"
    };
    format!("\u{1b}]8;{};{}{}", link.params, link.url, term)
}

fn format_osc8_close(terminator_bel: bool) -> String {
    let term = if terminator_bel { "\u{07}" } else { "\u{1b}\\" };
    format!("\u{1b}]8;;{}", term)
}

#[derive(Default)]
struct AnsiCodeTracker {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    blink: bool,
    inverse: bool,
    hidden: bool,
    strikethrough: bool,
    fg_color: Option<String>,
    bg_color: Option<String>,
    active_hyperlink: Option<ActiveHyperlink>,
}

impl AnsiCodeTracker {
    fn new() -> Self {
        Self::default()
    }

    fn process(&mut self, ansi_code: &str) {
        match parse_osc8_hyperlink(ansi_code) {
            OscParse::Open(link) => {
                self.active_hyperlink = Some(link);
                return;
            }
            OscParse::Close => {
                self.active_hyperlink = None;
                return;
            }
            OscParse::NotOsc8 => {}
        }

        if !ansi_code.ends_with('m') {
            return;
        }
        // Extract the parameters between "\x1b[" and "m".
        let Some(body) = ansi_code
            .strip_prefix("\u{1b}[")
            .and_then(|r| r.strip_suffix('m'))
        else {
            return;
        };
        if !body.chars().all(|c| c.is_ascii_digit() || c == ';') {
            return;
        }
        if body.is_empty() || body == "0" {
            self.reset();
            return;
        }

        let parts: Vec<&str> = body.split(';').collect();
        let mut i = 0;
        while i < parts.len() {
            let code: i64 = parts[i].parse().unwrap_or(i64::MIN);

            if code == 38 || code == 48 {
                if parts.get(i + 1) == Some(&"5") && parts.get(i + 2).is_some() {
                    let color = format!("{};{};{}", parts[i], parts[i + 1], parts[i + 2]);
                    if code == 38 {
                        self.fg_color = Some(color);
                    } else {
                        self.bg_color = Some(color);
                    }
                    i += 3;
                    continue;
                } else if parts.get(i + 1) == Some(&"2") && parts.get(i + 4).is_some() {
                    let color = format!(
                        "{};{};{};{};{}",
                        parts[i],
                        parts[i + 1],
                        parts[i + 2],
                        parts[i + 3],
                        parts[i + 4]
                    );
                    if code == 38 {
                        self.fg_color = Some(color);
                    } else {
                        self.bg_color = Some(color);
                    }
                    i += 5;
                    continue;
                }
            }

            match code {
                0 => self.reset(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => self.underline = true,
                5 => self.blink = true,
                7 => self.inverse = true,
                8 => self.hidden = true,
                9 => self.strikethrough = true,
                21 => self.bold = false,
                22 => {
                    self.bold = false;
                    self.dim = false;
                }
                23 => self.italic = false,
                24 => self.underline = false,
                25 => self.blink = false,
                27 => self.inverse = false,
                28 => self.hidden = false,
                29 => self.strikethrough = false,
                39 => self.fg_color = None,
                49 => self.bg_color = None,
                c if (30..=37).contains(&c) || (90..=97).contains(&c) => {
                    self.fg_color = Some(c.to_string());
                }
                c if (40..=47).contains(&c) || (100..=107).contains(&c) => {
                    self.bg_color = Some(c.to_string());
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn reset(&mut self) {
        self.bold = false;
        self.dim = false;
        self.italic = false;
        self.underline = false;
        self.blink = false;
        self.inverse = false;
        self.hidden = false;
        self.strikethrough = false;
        self.fg_color = None;
        self.bg_color = None;
        // SGR reset does not affect OSC 8 hyperlink state.
    }

    fn clear(&mut self) {
        self.reset();
        self.active_hyperlink = None;
    }

    fn get_active_codes(&self) -> String {
        let mut codes: Vec<String> = Vec::new();
        if self.bold {
            codes.push("1".into());
        }
        if self.dim {
            codes.push("2".into());
        }
        if self.italic {
            codes.push("3".into());
        }
        if self.underline {
            codes.push("4".into());
        }
        if self.blink {
            codes.push("5".into());
        }
        if self.inverse {
            codes.push("7".into());
        }
        if self.hidden {
            codes.push("8".into());
        }
        if self.strikethrough {
            codes.push("9".into());
        }
        if let Some(fg) = &self.fg_color {
            codes.push(fg.clone());
        }
        if let Some(bg) = &self.bg_color {
            codes.push(bg.clone());
        }

        let mut result = if codes.is_empty() {
            String::new()
        } else {
            format!("\u{1b}[{}m", codes.join(";"))
        };
        if let Some(link) = &self.active_hyperlink {
            result.push_str(&format_osc8_hyperlink(link));
        }
        result
    }

    fn get_line_end_reset(&self) -> String {
        let mut result = String::new();
        if self.underline {
            result.push_str("\u{1b}[24m");
        }
        if let Some(link) = &self.active_hyperlink {
            result.push_str(&format_osc8_close(link.terminator_bel));
        }
        result
    }
}

fn update_tracker_from_text(text: &str, tracker: &mut AnsiCodeTracker) {
    let mut i = 0;
    while i < text.len() {
        if let Some((code, len)) = extract_ansi_code(text, i) {
            tracker.process(&code);
            i += len;
        } else {
            i += 1;
        }
    }
}

// -------------------------------------------------------------------------
// Wrapping (utils.ts:628-819).
// -------------------------------------------------------------------------

fn split_into_tokens_with_ansi(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut pending_ansi = String::new();
    let mut current_kind: Option<bool> = None; // Some(true) = space, Some(false) = word

    let mut i = 0;
    while i < text.len() {
        if let Some((code, len)) = extract_ansi_code(text, i) {
            pending_ansi.push_str(&code);
            i += len;
            continue;
        }

        let mut end = i;
        while end < text.len() && extract_ansi_code(text, end).is_none() {
            let ch = text[end..].chars().next().unwrap();
            end += ch.len_utf8();
        }

        for segment in text[i..end].graphemes(true) {
            let segment_is_space = segment == " ";
            if !segment_is_space && is_cjk_break(segment) {
                if !current.is_empty() {
                    // `current_kind` is only consulted while `current` is
                    // non-empty, so it needs no reset here (pi's flushCurrent
                    // clears it, but the emptied buffer already makes it moot).
                    tokens.push(std::mem::take(&mut current));
                }
                let mut token = std::mem::take(&mut pending_ansi);
                token.push_str(segment);
                tokens.push(token);
                continue;
            }

            let segment_kind = Some(segment_is_space);
            if !current.is_empty() && current_kind != segment_kind {
                tokens.push(std::mem::take(&mut current));
            }

            if !pending_ansi.is_empty() {
                current.push_str(&pending_ansi);
                pending_ansi.clear();
            }

            current_kind = segment_kind;
            current.push_str(segment);
        }

        i = end;
    }

    if !pending_ansi.is_empty() {
        if !current.is_empty() {
            current.push_str(&pending_ansi);
        } else if let Some(last) = tokens.last_mut() {
            last.push_str(&pending_ansi);
        } else {
            current = pending_ansi.clone();
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

fn break_long_word(word: &str, width: usize, tracker: &mut AnsiCodeTracker) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current_line = tracker.get_active_codes();
    let mut current_width: usize = 0;

    enum Seg {
        Ansi(String),
        Grapheme(String),
    }

    let mut segments: Vec<Seg> = Vec::new();
    let mut i = 0;
    while i < word.len() {
        if let Some((code, len)) = extract_ansi_code(word, i) {
            segments.push(Seg::Ansi(code));
            i += len;
        } else {
            let mut end = i;
            while end < word.len() && extract_ansi_code(word, end).is_none() {
                let ch = word[end..].chars().next().unwrap();
                end += ch.len_utf8();
            }
            for g in word[i..end].graphemes(true) {
                segments.push(Seg::Grapheme(g.to_string()));
            }
            i = end;
        }
    }

    for seg in segments {
        match seg {
            Seg::Ansi(code) => {
                current_line.push_str(&code);
                tracker.process(&code);
            }
            Seg::Grapheme(grapheme) => {
                if grapheme.is_empty() {
                    continue;
                }
                let gw = visible_width(&grapheme);
                if current_width + gw > width {
                    let line_end_reset = tracker.get_line_end_reset();
                    if !line_end_reset.is_empty() {
                        current_line.push_str(&line_end_reset);
                    }
                    lines.push(std::mem::take(&mut current_line));
                    current_line = tracker.get_active_codes();
                    current_width = 0;
                }
                current_line.push_str(&grapheme);
                current_width += gw;
            }
        }
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

fn trim_end(s: &str) -> &str {
    // JS String.prototype.trimEnd removes ECMAScript whitespace + line
    // terminators, including U+FEFF (ZWNBSP). Rust `trim_end` uses the Unicode
    // White_Space property, which excludes U+FEFF, so we handle it explicitly.
    s.trim_end_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

fn wrap_single_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }

    if visible_width(line) <= width {
        return vec![line.to_string()];
    }

    let mut wrapped: Vec<String> = Vec::new();
    let mut tracker = AnsiCodeTracker::new();
    let tokens = split_into_tokens_with_ansi(line);

    let mut current_line = String::new();
    let mut current_visible_length: usize = 0;

    for token in tokens {
        let token_visible_length = visible_width(&token);
        let is_whitespace = token.trim().is_empty();

        if token_visible_length > width && !is_whitespace {
            if !current_line.is_empty() {
                let line_end_reset = tracker.get_line_end_reset();
                if !line_end_reset.is_empty() {
                    current_line.push_str(&line_end_reset);
                }
                wrapped.push(std::mem::take(&mut current_line));
                // current_visible_length is unconditionally recomputed below
                // from the final broken line, so no reset is needed here.
            }

            let broken = break_long_word(&token, width, &mut tracker);
            let n = broken.len();
            for line in broken.iter().take(n - 1) {
                wrapped.push(line.clone());
            }
            current_line = broken[n - 1].clone();
            current_visible_length = visible_width(&current_line);
            continue;
        }

        let total_needed = current_visible_length + token_visible_length;

        if total_needed > width && current_visible_length > 0 {
            let mut line_to_wrap = trim_end(&current_line).to_string();
            let line_end_reset = tracker.get_line_end_reset();
            if !line_end_reset.is_empty() {
                line_to_wrap.push_str(&line_end_reset);
            }
            wrapped.push(line_to_wrap);
            if is_whitespace {
                current_line = tracker.get_active_codes();
                current_visible_length = 0;
            } else {
                current_line = tracker.get_active_codes();
                current_line.push_str(&token);
                current_visible_length = token_visible_length;
            }
        } else {
            current_line.push_str(&token);
            current_visible_length += token_visible_length;
        }

        update_tracker_from_text(&token, &mut tracker);
    }

    if !current_line.is_empty() {
        wrapped.push(current_line);
    }

    if wrapped.is_empty() {
        vec![String::new()]
    } else {
        wrapped.iter().map(|l| trim_end(l).to_string()).collect()
    }
}

/// `wrapTextWithAnsi` (utils.ts:715).
pub fn wrap_text_with_ansi(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }

    let input_lines = split_lines(text);
    let mut result: Vec<String> = Vec::new();
    let mut tracker = AnsiCodeTracker::new();

    for input_line in input_lines {
        let prefix = if result.is_empty() {
            String::new()
        } else {
            tracker.get_active_codes()
        };
        let combined = format!("{}{}", prefix, input_line);
        for wrapped_line in wrap_single_line(&combined, width) {
            result.push(wrapped_line);
        }
        update_tracker_from_text(input_line, &mut tracker);
    }

    if result.is_empty() {
        vec![String::new()]
    } else {
        result
    }
}

/// Split on `\r\n`, `\r`, or `\n` — matching pi's `text.split(/\r\n|\r|\n/)`.
fn split_lines(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' => {
                lines.push(&text[start..i]);
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 2;
                } else {
                    i += 1;
                }
                start = i;
            }
            b'\n' => {
                lines.push(&text[start..i]);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    lines.push(&text[start..]);
    lines
}

// -------------------------------------------------------------------------
// Truncation (utils.ts:61-160, 936-1072).
// -------------------------------------------------------------------------

fn truncate_fragment_to_width(text: &str, max_width: i64) -> (String, i64) {
    if max_width <= 0 || text.is_empty() {
        return (String::new(), 0);
    }

    if is_printable_ascii(text) {
        let take = (max_width as usize).min(text.len());
        let clipped = &text[..take];
        return (clipped.to_string(), clipped.len() as i64);
    }

    let has_ansi = text.contains('\u{1b}');
    let has_tabs = text.contains('\t');
    if !has_ansi && !has_tabs {
        let mut result = String::new();
        let mut width: i64 = 0;
        for segment in text.graphemes(true) {
            let w = grapheme_width(segment);
            if width + w > max_width {
                break;
            }
            result.push_str(segment);
            width += w;
        }
        return (result, width);
    }

    let mut result = String::new();
    let mut width: i64 = 0;
    let mut i = 0;
    let mut pending_ansi = String::new();

    while i < text.len() {
        if let Some((code, len)) = extract_ansi_code(text, i) {
            pending_ansi.push_str(&code);
            i += len;
            continue;
        }

        if text.as_bytes()[i] == TAB {
            if width + 3 > max_width {
                break;
            }
            if !pending_ansi.is_empty() {
                result.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            result.push('\t');
            width += 3;
            i += 1;
            continue;
        }

        let mut end = i;
        while end < text.len() && text.as_bytes()[end] != TAB {
            if extract_ansi_code(text, end).is_some() {
                break;
            }
            let ch = text[end..].chars().next().unwrap();
            end += ch.len_utf8();
        }

        let mut broke = false;
        for segment in text[i..end].graphemes(true) {
            let w = grapheme_width(segment);
            if width + w > max_width {
                broke = true;
                break;
            }
            if !pending_ansi.is_empty() {
                result.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            result.push_str(segment);
            width += w;
        }
        if broke {
            return (result, width);
        }
        i = end;
    }

    (result, width)
}

fn finalize_truncated_result(
    prefix: &str,
    prefix_width: i64,
    ellipsis: &str,
    ellipsis_width: i64,
    max_width: i64,
    pad: bool,
) -> String {
    let reset = "\u{1b}[0m";
    let visible_width = prefix_width + ellipsis_width;
    let result = if !ellipsis.is_empty() {
        format!("{}{}{}{}", prefix, reset, ellipsis, reset)
    } else {
        format!("{}{}", prefix, reset)
    };

    if pad {
        let padding = (max_width - visible_width).max(0) as usize;
        format!("{}{}", result, " ".repeat(padding))
    } else {
        result
    }
}

/// `truncateToWidth` (utils.ts:936).
pub fn truncate_to_width(text: &str, max_width: i64, ellipsis: &str, pad: bool) -> String {
    if max_width <= 0 {
        return String::new();
    }

    if text.is_empty() {
        return if pad {
            " ".repeat(max_width as usize)
        } else {
            String::new()
        };
    }

    let ellipsis_width = visible_width(ellipsis) as i64;
    if ellipsis_width >= max_width {
        let text_width = visible_width(text) as i64;
        if text_width <= max_width {
            return if pad {
                format!("{}{}", text, " ".repeat((max_width - text_width) as usize))
            } else {
                text.to_string()
            };
        }

        let (clipped_text, clipped_width) = truncate_fragment_to_width(ellipsis, max_width);
        if clipped_width == 0 {
            return if pad {
                " ".repeat(max_width as usize)
            } else {
                String::new()
            };
        }
        return finalize_truncated_result("", 0, &clipped_text, clipped_width, max_width, pad);
    }

    if is_printable_ascii(text) {
        if (text.len() as i64) <= max_width {
            return if pad {
                format!("{}{}", text, " ".repeat(max_width as usize - text.len()))
            } else {
                text.to_string()
            };
        }
        let target_width = max_width - ellipsis_width;
        let prefix = &text[..target_width as usize];
        return finalize_truncated_result(
            prefix,
            target_width,
            ellipsis,
            ellipsis_width,
            max_width,
            pad,
        );
    }

    let target_width = max_width - ellipsis_width;
    let mut result = String::new();
    let mut pending_ansi = String::new();
    let mut visible_so_far: i64 = 0;
    let mut kept_width: i64 = 0;
    let mut keep_contiguous_prefix = true;
    let mut overflowed = false;
    let exhausted_input;
    let has_ansi = text.contains('\u{1b}');
    let has_tabs = text.contains('\t');

    if !has_ansi && !has_tabs {
        for segment in text.graphemes(true) {
            let width = grapheme_width(segment);
            if keep_contiguous_prefix && kept_width + width <= target_width {
                result.push_str(segment);
                kept_width += width;
            } else {
                keep_contiguous_prefix = false;
            }
            visible_so_far += width;
            if visible_so_far > max_width {
                overflowed = true;
                break;
            }
        }
        exhausted_input = !overflowed;
    } else {
        let mut i = 0;
        while i < text.len() {
            if let Some((code, len)) = extract_ansi_code(text, i) {
                pending_ansi.push_str(&code);
                i += len;
                continue;
            }

            if text.as_bytes()[i] == TAB {
                if keep_contiguous_prefix && kept_width + 3 <= target_width {
                    if !pending_ansi.is_empty() {
                        result.push_str(&pending_ansi);
                        pending_ansi.clear();
                    }
                    result.push('\t');
                    kept_width += 3;
                } else {
                    keep_contiguous_prefix = false;
                    pending_ansi.clear();
                }
                visible_so_far += 3;
                if visible_so_far > max_width {
                    overflowed = true;
                    break;
                }
                i += 1;
                continue;
            }

            let mut end = i;
            while end < text.len() && text.as_bytes()[end] != TAB {
                if extract_ansi_code(text, end).is_some() {
                    break;
                }
                let ch = text[end..].chars().next().unwrap();
                end += ch.len_utf8();
            }

            for segment in text[i..end].graphemes(true) {
                let width = grapheme_width(segment);
                if keep_contiguous_prefix && kept_width + width <= target_width {
                    if !pending_ansi.is_empty() {
                        result.push_str(&pending_ansi);
                        pending_ansi.clear();
                    }
                    result.push_str(segment);
                    kept_width += width;
                } else {
                    keep_contiguous_prefix = false;
                    pending_ansi.clear();
                }

                visible_so_far += width;
                if visible_so_far > max_width {
                    overflowed = true;
                    break;
                }
            }
            if overflowed {
                break;
            }
            i = end;
        }
        exhausted_input = i >= text.len();
    }

    if !overflowed && exhausted_input {
        return if pad {
            format!(
                "{}{}",
                text,
                " ".repeat((max_width - visible_so_far).max(0) as usize)
            )
        } else {
            text.to_string()
        };
    }

    finalize_truncated_result(
        &result,
        kept_width,
        ellipsis,
        ellipsis_width,
        max_width,
        pad,
    )
}

// -------------------------------------------------------------------------
// Column slicing and overlay extraction (utils.ts:1078-1209).
// -------------------------------------------------------------------------

/// `sliceByColumn` (utils.ts:1078).
pub fn slice_by_column(line: &str, start_col: i64, length: i64, strict: bool) -> String {
    slice_with_width(line, start_col, length, strict).0
}

/// `sliceWithWidth` (utils.ts:1083).
pub fn slice_with_width(line: &str, start_col: i64, length: i64, strict: bool) -> (String, i64) {
    if length <= 0 {
        return (String::new(), 0);
    }
    let end_col = start_col + length;
    let mut result = String::new();
    let mut result_width: i64 = 0;
    let mut current_col: i64 = 0;
    let mut i = 0;
    let mut pending_ansi = String::new();

    while i < line.len() {
        if let Some((code, len)) = extract_ansi_code(line, i) {
            if current_col >= start_col && current_col < end_col {
                result.push_str(&code);
            } else if current_col < start_col {
                pending_ansi.push_str(&code);
            }
            i += len;
            continue;
        }

        let mut text_end = i;
        while text_end < line.len() && extract_ansi_code(line, text_end).is_none() {
            let ch = line[text_end..].chars().next().unwrap();
            text_end += ch.len_utf8();
        }

        for segment in line[i..text_end].graphemes(true) {
            let w = grapheme_width(segment);
            let in_range = current_col >= start_col && current_col < end_col;
            let fits = !strict || current_col + w <= end_col;
            if in_range && fits {
                if !pending_ansi.is_empty() {
                    result.push_str(&pending_ansi);
                    pending_ansi.clear();
                }
                result.push_str(segment);
                result_width += w;
            }
            current_col += w;
            if current_col >= end_col {
                break;
            }
        }
        i = text_end;
        if current_col >= end_col {
            break;
        }
    }

    (result, result_width)
}

/// `extractSegments` (utils.ts:1138). The pooled tracker is a per-call local here
/// (the pooling in pi is purely an allocation optimisation).
#[allow(clippy::too_many_arguments)]
pub fn extract_segments(
    line: &str,
    before_end: i64,
    after_start: i64,
    after_len: i64,
    strict_after: bool,
) -> ExtractSegments {
    let mut before = String::new();
    let mut before_width: i64 = 0;
    let mut after = String::new();
    let mut after_width: i64 = 0;
    let mut current_col: i64 = 0;
    let mut i = 0;
    let mut pending_ansi_before = String::new();
    let mut after_started = false;
    let after_end = after_start + after_len;

    let mut tracker = AnsiCodeTracker::new();
    tracker.clear();

    while i < line.len() {
        if let Some((code, len)) = extract_ansi_code(line, i) {
            tracker.process(&code);
            if current_col < before_end {
                pending_ansi_before.push_str(&code);
            } else if current_col >= after_start && current_col < after_end && after_started {
                after.push_str(&code);
            }
            i += len;
            continue;
        }

        let mut text_end = i;
        while text_end < line.len() && extract_ansi_code(line, text_end).is_none() {
            let ch = line[text_end..].chars().next().unwrap();
            text_end += ch.len_utf8();
        }

        for segment in line[i..text_end].graphemes(true) {
            let w = grapheme_width(segment);

            if current_col < before_end && current_col + w <= before_end {
                if !pending_ansi_before.is_empty() {
                    before.push_str(&pending_ansi_before);
                    pending_ansi_before.clear();
                }
                before.push_str(segment);
                before_width += w;
            } else if current_col >= after_start && current_col < after_end {
                let fits = !strict_after || current_col + w <= after_end;
                if fits {
                    if !after_started {
                        after.push_str(&tracker.get_active_codes());
                        after_started = true;
                    }
                    after.push_str(segment);
                    after_width += w;
                }
            }

            current_col += w;
            let done = if after_len <= 0 {
                current_col >= before_end
            } else {
                current_col >= after_end
            };
            if done {
                break;
            }
        }
        i = text_end;
        let done = if after_len <= 0 {
            current_col >= before_end
        } else {
            current_col >= after_end
        };
        if done {
            break;
        }
    }

    ExtractSegments {
        before,
        before_width,
        after,
        after_width,
    }
}

/// Result of [`extract_segments`].
pub struct ExtractSegments {
    pub before: String,
    pub before_width: i64,
    pub after: String,
    pub after_width: i64,
}
