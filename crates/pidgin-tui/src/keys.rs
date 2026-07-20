// straitjacket-allow-file:duplication — bit-exact port: each byte scanner
// deliberately mirrors one of pi's distinct anchored key-sequence regexes
// one-for-one, so the parallel `:(\d+)` / `;(\d+)` optional-group scans and the
// near-identical arrow/home-end scanners are intentional structural fidelity,
// not incidental copy-paste.
//! Bit-exact Rust port of pi's terminal key parser
//! (`vendor/pi/packages/tui/src/keys.ts`, pi v0.80.10, submodule pin
//! `3da591a`).
//!
//! Supports both legacy terminal sequences and the Kitty keyboard protocol,
//! plus xterm `modifyOtherKeys`. Correctness means byte-identical results
//! versus pi: the port is validated against vectors extracted from pi itself
//! (see `tests/keys_vectors.rs`). pi is always the source of truth; where a
//! naive Rust choice would diverge, the port overrides to match pi and the
//! reason is documented inline with a `DIVERGENCE:` marker.
//!
//! The public surface mirrors pi's exports: [`matches_key`], [`parse_key`],
//! [`decode_kitty_printable`], [`decode_printable_key`], [`is_key_release`],
//! [`is_key_repeat`], and the Kitty protocol flag
//! ([`set_kitty_protocol_active`] / [`is_kitty_protocol_active`]).
//!
//! Note: pi's key parser does not use the width module, so nothing here reuses
//! it (unlike other parts of the TUI port).

use std::sync::atomic::{AtomicBool, Ordering};

// =============================================================================
// Global Kitty Protocol State
// =============================================================================

// pi keeps a module-global `_kittyProtocolActive` that many matches branch on
// and that tests flip via `setKittyProtocolActive`. We model it as an atomic so
// the same settable global semantics hold.
static KITTY_PROTOCOL_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Set the global Kitty keyboard protocol state.
pub fn set_kitty_protocol_active(active: bool) {
    KITTY_PROTOCOL_ACTIVE.store(active, Ordering::SeqCst);
}

/// Query whether Kitty keyboard protocol is currently active.
pub fn is_kitty_protocol_active() -> bool {
    KITTY_PROTOCOL_ACTIVE.load(Ordering::SeqCst)
}

// =============================================================================
// Constants
// =============================================================================

// Modifier bitmask values (as reported by Kitty/CSI-u minus 1).
const MOD_SHIFT: i64 = 1;
const MOD_ALT: i64 = 2;
const MOD_CTRL: i64 = 4;
const MOD_SUPER: i64 = 8;
const SUPPORTED_MODIFIER_MASK: i64 = MOD_SHIFT | MOD_CTRL | MOD_ALT | MOD_SUPER;

const LOCK_MASK: i64 = 64 + 128; // Caps Lock + Num Lock

// Codepoints of interest.
const CP_ESCAPE: i64 = 27;
const CP_TAB: i64 = 9;
const CP_ENTER: i64 = 13;
const CP_SPACE: i64 = 32;
const CP_BACKSPACE: i64 = 127;
const CP_KP_ENTER: i64 = 57414;

// Internal negative sentinels for arrows and functional keys.
const ARROW_UP: i64 = -1;
const ARROW_DOWN: i64 = -2;
const ARROW_RIGHT: i64 = -3;
const ARROW_LEFT: i64 = -4;

const FUNC_DELETE: i64 = -10;
const FUNC_INSERT: i64 = -11;
const FUNC_PAGE_UP: i64 = -12;
const FUNC_PAGE_DOWN: i64 = -13;
const FUNC_HOME: i64 = -14;
const FUNC_END: i64 = -15;

/// The 30 recognized symbol keys (matches pi's `SYMBOL_KEYS`).
fn is_symbol_key(c: char) -> bool {
    matches!(
        c,
        '`' | '-'
            | '='
            | '['
            | ']'
            | '\\'
            | ';'
            | '\''
            | ','
            | '.'
            | '/'
            | '!'
            | '@'
            | '#'
            | '$'
            | '%'
            | '^'
            | '&'
            | '*'
            | '('
            | ')'
            | '_'
            | '+'
            | '|'
            | '~'
            | '{'
            | '}'
            | ':'
            | '<'
            | '>'
            | '?'
    )
}

// pi builds symbol-key checks via `SYMBOL_KEYS.has(String.fromCharCode(cp))`.
// `String.fromCharCode` truncates to 16 bits (`ToUint16`), so we reproduce that
// truncation, then map to a `char`. Surrogate code units yield `None` (never a
// symbol), which matches JS since no surrogate is in `SYMBOL_KEYS`.
fn from_char_code(cp: i64) -> Option<char> {
    let unit = (cp as u32) & 0xffff;
    char::from_u32(unit)
}

fn is_symbol_codepoint(cp: i64) -> bool {
    from_char_code(cp).map(is_symbol_key).unwrap_or(false)
}

// `String.fromCodePoint`: full Unicode scalar; throws for surrogates or values
// above U+10FFFF (we return `None`, matching the caller's try/catch).
fn from_code_point(cp: i64) -> Option<char> {
    if !(0..=0x10_FFFF).contains(&cp) {
        return None;
    }
    char::from_u32(cp as u32)
}

// =============================================================================
// Kitty functional-key normalization
// =============================================================================

fn normalize_kitty_functional_codepoint(codepoint: i64) -> i64 {
    match codepoint {
        57399 => 48, // KP_0 -> 0
        57400 => 49, // KP_1 -> 1
        57401 => 50, // KP_2 -> 2
        57402 => 51, // KP_3 -> 3
        57403 => 52, // KP_4 -> 4
        57404 => 53, // KP_5 -> 5
        57405 => 54, // KP_6 -> 6
        57406 => 55, // KP_7 -> 7
        57407 => 56, // KP_8 -> 8
        57408 => 57, // KP_9 -> 9
        57409 => 46, // KP_DECIMAL -> .
        57410 => 47, // KP_DIVIDE -> /
        57411 => 42, // KP_MULTIPLY -> *
        57412 => 45, // KP_SUBTRACT -> -
        57413 => 43, // KP_ADD -> +
        57415 => 61, // KP_EQUAL -> =
        57416 => 44, // KP_SEPARATOR -> ,
        57417 => ARROW_LEFT,
        57418 => ARROW_RIGHT,
        57419 => ARROW_UP,
        57420 => ARROW_DOWN,
        57421 => FUNC_PAGE_UP,
        57422 => FUNC_PAGE_DOWN,
        57423 => FUNC_HOME,
        57424 => FUNC_END,
        57425 => FUNC_INSERT,
        57426 => FUNC_DELETE,
        other => other,
    }
}

// Shifted uppercase letters compare by lowercase identity when shift is set.
fn normalize_shifted_letter_identity_codepoint(codepoint: i64, modifier: i64) -> i64 {
    let effective_modifier = modifier & !LOCK_MASK;
    if (effective_modifier & MOD_SHIFT) != 0 && (65..=90).contains(&codepoint) {
        return codepoint + 32;
    }
    codepoint
}

// =============================================================================
// Legacy escape tables
// =============================================================================

// LEGACY_KEY_SEQUENCES (unmodified specials).
fn legacy_key_sequences(key: &str) -> &'static [&'static str] {
    match key {
        "up" => &["\x1b[A", "\x1bOA"],
        "down" => &["\x1b[B", "\x1bOB"],
        "right" => &["\x1b[C", "\x1bOC"],
        "left" => &["\x1b[D", "\x1bOD"],
        "home" => &["\x1b[H", "\x1bOH", "\x1b[1~", "\x1b[7~"],
        "end" => &["\x1b[F", "\x1bOF", "\x1b[4~", "\x1b[8~"],
        "insert" => &["\x1b[2~"],
        "delete" => &["\x1b[3~"],
        "pageUp" => &["\x1b[5~", "\x1b[[5~"],
        "pageDown" => &["\x1b[6~", "\x1b[[6~"],
        "clear" => &["\x1b[E", "\x1bOE"],
        "f1" => &["\x1bOP", "\x1b[11~", "\x1b[[A"],
        "f2" => &["\x1bOQ", "\x1b[12~", "\x1b[[B"],
        "f3" => &["\x1bOR", "\x1b[13~", "\x1b[[C"],
        "f4" => &["\x1bOS", "\x1b[14~", "\x1b[[D"],
        "f5" => &["\x1b[15~", "\x1b[[E"],
        "f6" => &["\x1b[17~"],
        "f7" => &["\x1b[18~"],
        "f8" => &["\x1b[19~"],
        "f9" => &["\x1b[20~"],
        "f10" => &["\x1b[21~"],
        "f11" => &["\x1b[23~"],
        "f12" => &["\x1b[24~"],
        _ => &[],
    }
}

fn legacy_shift_sequences(key: &str) -> &'static [&'static str] {
    match key {
        "up" => &["\x1b[a"],
        "down" => &["\x1b[b"],
        "right" => &["\x1b[c"],
        "left" => &["\x1b[d"],
        "clear" => &["\x1b[e"],
        "insert" => &["\x1b[2$"],
        "delete" => &["\x1b[3$"],
        "pageUp" => &["\x1b[5$"],
        "pageDown" => &["\x1b[6$"],
        "home" => &["\x1b[7$"],
        "end" => &["\x1b[8$"],
        _ => &[],
    }
}

fn legacy_ctrl_sequences(key: &str) -> &'static [&'static str] {
    match key {
        "up" => &["\x1bOa"],
        "down" => &["\x1bOb"],
        "right" => &["\x1bOc"],
        "left" => &["\x1bOd"],
        "clear" => &["\x1bOe"],
        "insert" => &["\x1b[2^"],
        "delete" => &["\x1b[3^"],
        "pageUp" => &["\x1b[5^"],
        "pageDown" => &["\x1b[6^"],
        "home" => &["\x1b[7^"],
        "end" => &["\x1b[8^"],
        _ => &[],
    }
}

// LEGACY_SEQUENCE_KEY_IDS: raw sequence -> KeyId (used by parse_key).
fn legacy_sequence_key_id(data: &str) -> Option<&'static str> {
    let id = match data {
        "\x1bOA" => "up",
        "\x1bOB" => "down",
        "\x1bOC" => "right",
        "\x1bOD" => "left",
        "\x1bOH" => "home",
        "\x1bOF" => "end",
        "\x1b[E" => "clear",
        "\x1bOE" => "clear",
        "\x1bOe" => "ctrl+clear",
        "\x1b[e" => "shift+clear",
        "\x1b[2~" => "insert",
        "\x1b[2$" => "shift+insert",
        "\x1b[2^" => "ctrl+insert",
        "\x1b[3$" => "shift+delete",
        "\x1b[3^" => "ctrl+delete",
        "\x1b[[5~" => "pageUp",
        "\x1b[[6~" => "pageDown",
        "\x1b[a" => "shift+up",
        "\x1b[b" => "shift+down",
        "\x1b[c" => "shift+right",
        "\x1b[d" => "shift+left",
        "\x1bOa" => "ctrl+up",
        "\x1bOb" => "ctrl+down",
        "\x1bOc" => "ctrl+right",
        "\x1bOd" => "ctrl+left",
        "\x1b[5$" => "shift+pageUp",
        "\x1b[6$" => "shift+pageDown",
        "\x1b[7$" => "shift+home",
        "\x1b[8$" => "shift+end",
        "\x1b[5^" => "ctrl+pageUp",
        "\x1b[6^" => "ctrl+pageDown",
        "\x1b[7^" => "ctrl+home",
        "\x1b[8^" => "ctrl+end",
        "\x1bOP" => "f1",
        "\x1bOQ" => "f2",
        "\x1bOR" => "f3",
        "\x1bOS" => "f4",
        "\x1b[11~" => "f1",
        "\x1b[12~" => "f2",
        "\x1b[13~" => "f3",
        "\x1b[14~" => "f4",
        "\x1b[[A" => "f1",
        "\x1b[[B" => "f2",
        "\x1b[[C" => "f3",
        "\x1b[[D" => "f4",
        "\x1b[[E" => "f5",
        "\x1b[15~" => "f5",
        "\x1b[17~" => "f6",
        "\x1b[18~" => "f7",
        "\x1b[19~" => "f8",
        "\x1b[20~" => "f9",
        "\x1b[21~" => "f10",
        "\x1b[23~" => "f11",
        "\x1b[24~" => "f12",
        "\x1bb" => "alt+left",
        "\x1bf" => "alt+right",
        "\x1bp" => "alt+up",
        "\x1bn" => "alt+down",
        _ => return None,
    };
    Some(id)
}

fn matches_legacy_sequence(data: &str, sequences: &[&str]) -> bool {
    sequences.contains(&data)
}

fn matches_legacy_modifier_sequence(data: &str, key: &str, modifier: i64) -> bool {
    if modifier == MOD_SHIFT {
        return matches_legacy_sequence(data, legacy_shift_sequences(key));
    }
    if modifier == MOD_CTRL {
        return matches_legacy_sequence(data, legacy_ctrl_sequences(key));
    }
    false
}

// =============================================================================
// Kitty protocol / CSI-u parsing
// =============================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyEventType {
    Press,
    Repeat,
    Release,
}

struct ParsedKitty {
    codepoint: i64,
    shifted_key: Option<i64>,
    base_layout_key: Option<i64>,
    modifier: i64,
    #[allow(dead_code)]
    event_type: KeyEventType,
}

struct ParsedModifyOtherKeys {
    codepoint: i64,
    modifier: i64,
}

fn event_type_from_value(value: Option<i64>) -> KeyEventType {
    match value {
        Some(2) => KeyEventType::Repeat,
        Some(3) => KeyEventType::Release,
        _ => KeyEventType::Press,
    }
}

// --- byte scanners (all structural characters are ASCII) ------------------
//
// DIVERGENCE (documented in keys-notes.md): pi drives these with JS regexes
// over UTF-16 strings; we hand-roll equivalent scanners over UTF-8 bytes. Every
// delimiter (ESC, `[`, `;`, `:`, digits, `u`/`~`/`ABCDHF`) is ASCII, and UTF-8
// continuation bytes never collide with ASCII, so a non-ASCII byte where a
// digit or terminator is required simply fails the match exactly as the anchored
// regex would. This is behaviorally identical and validated by the vectors.

// Scan one-or-more ASCII digits; returns (value, next_index) or None.
fn scan_digits1(b: &[u8], i: usize) -> Option<(i64, usize)> {
    let start = i;
    let mut j = i;
    let mut val: i64 = 0;
    while j < b.len() && b[j].is_ascii_digit() {
        val = val.saturating_mul(10).saturating_add((b[j] - b'0') as i64);
        j += 1;
    }
    if j == start {
        None
    } else {
        Some((val, j))
    }
}

// Regex: /^\x1b\[(\d+)(?::(\d*))?(?::(\d+))?(?:;(\d+))?(?::(\d+))?u$/
// Groups: 1=codepoint, 2=shifted(\d*, may be empty), 3=base, 4=mod, 5=event.
fn parse_csi_u_only(data: &str) -> Option<ParsedKitty> {
    let b = data.as_bytes();
    if b.len() < 4 || b[0] != 0x1b || b[1] != b'[' {
        return None;
    }
    let mut i = 2;
    // group 1: (\d+)
    let (codepoint, ni) = scan_digits1(b, i)?;
    i = ni;
    // group 2: (?::(\d*))?  -- colon then zero-or-more digits (may be empty).
    // A present-but-empty group (the `::base` form) yields no shifted key.
    let mut shifted_key: Option<i64> = None;
    let mut base_layout_key: Option<i64> = None;
    if i < b.len() && b[i] == b':' {
        i += 1;
        let start = i;
        let mut val: i64 = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            val = val.saturating_mul(10).saturating_add((b[i] - b'0') as i64);
            i += 1;
        }
        if i > start {
            shifted_key = Some(val);
        }
        // group 3: (?::(\d+))?  -- a second colon requires one-or-more digits.
        if i < b.len() && b[i] == b':' {
            i += 1;
            let (base, ni) = scan_digits1(b, i)?;
            base_layout_key = Some(base);
            i = ni;
        }
    }
    finish_csi_u(b, i, codepoint, shifted_key, base_layout_key)
}

// Finishes the CSI-u parse after codepoint / shifted / base groups, consuming
// the optional `;mod` and `:event` groups and the trailing `u`.
fn finish_csi_u(
    b: &[u8],
    mut i: usize,
    codepoint: i64,
    shifted_key: Option<i64>,
    base_layout_key: Option<i64>,
) -> Option<ParsedKitty> {
    // group 4: (?:;(\d+))?
    let mut mod_value: i64 = 1;
    if i < b.len() && b[i] == b';' {
        i += 1;
        let (m, ni) = scan_digits1(b, i)?;
        mod_value = m;
        i = ni;
    }
    // group 5: (?::(\d+))?
    let mut event_value: Option<i64> = None;
    if i < b.len() && b[i] == b':' {
        i += 1;
        let (e, ni) = scan_digits1(b, i)?;
        event_value = Some(e);
        i = ni;
    }
    // trailing `u`, anchored to end
    if i + 1 != b.len() || b[i] != b'u' {
        return None;
    }
    Some(ParsedKitty {
        codepoint,
        shifted_key,
        base_layout_key,
        modifier: mod_value - 1,
        event_type: event_type_from_value(event_value),
    })
}

fn parse_kitty_sequence(data: &str) -> Option<ParsedKitty> {
    if let Some(parsed) = parse_csi_u_only(data) {
        return Some(parsed);
    }

    let b = data.as_bytes();

    // Arrow keys with modifier: /^\x1b\[1;(\d+)(?::(\d+))?([ABCD])$/
    if b.len() >= 5 && b[0] == 0x1b && b[1] == b'[' && b[2] == b'1' && b[3] == b';' {
        if let Some(parsed) = parse_arrow_mod(b) {
            return Some(parsed);
        }
    }

    // Functional keys: /^\x1b\[(\d+)(?:;(\d+))?(?::(\d+))?~$/
    if let Some(parsed) = parse_functional(b) {
        return Some(parsed);
    }

    // Home/End with modifier: /^\x1b\[1;(\d+)(?::(\d+))?([HF])$/
    if b.len() >= 5 && b[0] == 0x1b && b[1] == b'[' && b[2] == b'1' && b[3] == b';' {
        if let Some(parsed) = parse_home_end_mod(b) {
            return Some(parsed);
        }
    }

    None
}

fn parse_arrow_mod(b: &[u8]) -> Option<ParsedKitty> {
    // ESC [ 1 ; already checked
    let mut i = 4;
    let (mod_value, ni) = scan_digits1(b, i)?;
    i = ni;
    let mut event_value: Option<i64> = None;
    if i < b.len() && b[i] == b':' {
        i += 1;
        let (e, ni) = scan_digits1(b, i)?;
        event_value = Some(e);
        i = ni;
    }
    if i + 1 != b.len() {
        return None;
    }
    let codepoint = match b[i] {
        b'A' => ARROW_UP,
        b'B' => ARROW_DOWN,
        b'C' => ARROW_RIGHT,
        b'D' => ARROW_LEFT,
        _ => return None,
    };
    Some(ParsedKitty {
        codepoint,
        shifted_key: None,
        base_layout_key: None,
        modifier: mod_value - 1,
        event_type: event_type_from_value(event_value),
    })
}

fn parse_functional(b: &[u8]) -> Option<ParsedKitty> {
    if b.len() < 4 || b[0] != 0x1b || b[1] != b'[' {
        return None;
    }
    let mut i = 2;
    let (key_num, ni) = scan_digits1(b, i)?;
    i = ni;
    let mut mod_value: i64 = 1;
    if i < b.len() && b[i] == b';' {
        i += 1;
        let (m, ni) = scan_digits1(b, i)?;
        mod_value = m;
        i = ni;
    }
    let mut event_value: Option<i64> = None;
    if i < b.len() && b[i] == b':' {
        i += 1;
        let (e, ni) = scan_digits1(b, i)?;
        event_value = Some(e);
        i = ni;
    }
    if i + 1 != b.len() || b[i] != b'~' {
        return None;
    }
    let codepoint = match key_num {
        2 => FUNC_INSERT,
        3 => FUNC_DELETE,
        5 => FUNC_PAGE_UP,
        6 => FUNC_PAGE_DOWN,
        7 => FUNC_HOME,
        8 => FUNC_END,
        _ => return None,
    };
    Some(ParsedKitty {
        codepoint,
        shifted_key: None,
        base_layout_key: None,
        modifier: mod_value - 1,
        event_type: event_type_from_value(event_value),
    })
}

fn parse_home_end_mod(b: &[u8]) -> Option<ParsedKitty> {
    let mut i = 4;
    let (mod_value, ni) = scan_digits1(b, i)?;
    i = ni;
    let mut event_value: Option<i64> = None;
    if i < b.len() && b[i] == b':' {
        i += 1;
        let (e, ni) = scan_digits1(b, i)?;
        event_value = Some(e);
        i = ni;
    }
    if i + 1 != b.len() {
        return None;
    }
    let codepoint = match b[i] {
        b'H' => FUNC_HOME,
        b'F' => FUNC_END,
        _ => return None,
    };
    Some(ParsedKitty {
        codepoint,
        shifted_key: None,
        base_layout_key: None,
        modifier: mod_value - 1,
        event_type: event_type_from_value(event_value),
    })
}

fn matches_kitty_sequence(data: &str, expected_codepoint: i64, expected_modifier: i64) -> bool {
    let Some(parsed) = parse_kitty_sequence(data) else {
        return false;
    };
    let actual_mod = parsed.modifier & !LOCK_MASK;
    let expected_mod = expected_modifier & !LOCK_MASK;
    if actual_mod != expected_mod {
        return false;
    }

    let normalized_codepoint = normalize_shifted_letter_identity_codepoint(
        normalize_kitty_functional_codepoint(parsed.codepoint),
        parsed.modifier,
    );
    let normalized_expected = normalize_shifted_letter_identity_codepoint(
        normalize_kitty_functional_codepoint(expected_codepoint),
        expected_modifier,
    );

    if normalized_codepoint == normalized_expected {
        return true;
    }

    // Base-layout fallback: only when the actual codepoint is NOT a recognized
    // Latin letter (a-z) or known symbol. This lets Cyrillic Ctrl+С match Ctrl+c
    // while preventing Dvorak/Colemak false matches.
    if let Some(base) = parsed.base_layout_key {
        if base == expected_codepoint {
            let cp = normalized_codepoint;
            let is_latin_letter = (97..=122).contains(&cp);
            let is_known_symbol = is_symbol_codepoint(cp);
            if !is_latin_letter && !is_known_symbol {
                return true;
            }
        }
    }

    false
}

// =============================================================================
// xterm modifyOtherKeys
// =============================================================================

// Regex: /^\x1b\[27;(\d+);(\d+)~$/
fn parse_modify_other_keys_sequence(data: &str) -> Option<ParsedModifyOtherKeys> {
    let b = data.as_bytes();
    let prefix = b"\x1b[27;";
    if b.len() < prefix.len() || &b[..prefix.len()] != prefix {
        return None;
    }
    let mut i = prefix.len();
    let (mod_value, ni) = scan_digits1(b, i)?;
    i = ni;
    if i >= b.len() || b[i] != b';' {
        return None;
    }
    i += 1;
    let (codepoint, ni) = scan_digits1(b, i)?;
    i = ni;
    if i + 1 != b.len() || b[i] != b'~' {
        return None;
    }
    Some(ParsedModifyOtherKeys {
        codepoint,
        modifier: mod_value - 1,
    })
}

fn matches_modify_other_keys(data: &str, expected_keycode: i64, expected_modifier: i64) -> bool {
    let Some(parsed) = parse_modify_other_keys_sequence(data) else {
        return false;
    };
    parsed.codepoint == expected_keycode && parsed.modifier == expected_modifier
}

fn matches_printable_modify_other_keys(
    data: &str,
    expected_keycode: i64,
    expected_modifier: i64,
) -> bool {
    if expected_modifier == 0 {
        return false;
    }
    let Some(parsed) = parse_modify_other_keys_sequence(data) else {
        return false;
    };
    if parsed.modifier != expected_modifier {
        return false;
    }
    normalize_shifted_letter_identity_codepoint(parsed.codepoint, parsed.modifier)
        == normalize_shifted_letter_identity_codepoint(expected_keycode, expected_modifier)
}

// =============================================================================
// Terminal quirks
// =============================================================================

// pi computes this from `process.env` on each call. We read the same env vars
// via `std::env`; `Boolean(process.env.X)` is true only for a present, non-empty
// value, and `!process.env.X` is true when unset or empty.
fn env_truthy(name: &str) -> bool {
    std::env::var(name).map(|v| !v.is_empty()).unwrap_or(false)
}

fn is_windows_terminal_session() -> bool {
    env_truthy("WT_SESSION")
        && !env_truthy("SSH_CONNECTION")
        && !env_truthy("SSH_CLIENT")
        && !env_truthy("SSH_TTY")
}

fn matches_raw_backspace(data: &str, expected_modifier: i64) -> bool {
    if data == "\x7f" {
        return expected_modifier == 0;
    }
    if data != "\x08" {
        return false;
    }
    if is_windows_terminal_session() {
        expected_modifier == MOD_CTRL
    } else {
        expected_modifier == 0
    }
}

// =============================================================================
// Generic key matching
// =============================================================================

// code & 0x1f control char for a key, matching pi's rawCtrlChar.
fn raw_ctrl_char(key: &str) -> Option<char> {
    let ch = key.chars().next()?.to_ascii_lowercase();
    let code = ch as u32;
    if (97..=122).contains(&code) || ch == '[' || ch == '\\' || ch == ']' || ch == '_' {
        return char::from_u32(code & 0x1f);
    }
    if ch == '-' {
        return char::from_u32(31); // same as Ctrl+_
    }
    None
}

fn is_digit_key(key: &str) -> bool {
    key.len() == 1 && key.as_bytes()[0].is_ascii_digit()
}

struct ParsedKeyId {
    key: String,
    ctrl: bool,
    shift: bool,
    alt: bool,
    super_mod: bool,
}

fn parse_key_id(key_id: &str) -> Option<ParsedKeyId> {
    let lower = key_id.to_lowercase();
    let parts: Vec<&str> = lower.split('+').collect();
    let key = parts[parts.len() - 1];
    if key.is_empty() {
        return None;
    }
    Some(ParsedKeyId {
        key: key.to_string(),
        ctrl: parts.contains(&"ctrl"),
        shift: parts.contains(&"shift"),
        alt: parts.contains(&"alt"),
        super_mod: parts.contains(&"super"),
    })
}

/// Match input data against a key identifier string (e.g. `"ctrl+c"`,
/// `"escape"`). Mirrors pi's `matchesKey`.
pub fn matches_key(data: &str, key_id: &str) -> bool {
    let Some(parsed) = parse_key_id(key_id) else {
        return false;
    };
    let key = parsed.key.as_str();

    let mut modifier: i64 = 0;
    if parsed.shift {
        modifier |= MOD_SHIFT;
    }
    if parsed.alt {
        modifier |= MOD_ALT;
    }
    if parsed.ctrl {
        modifier |= MOD_CTRL;
    }
    if parsed.super_mod {
        modifier |= MOD_SUPER;
    }

    let kitty = is_kitty_protocol_active();

    match key {
        "escape" | "esc" => {
            if modifier != 0 {
                return false;
            }
            data == "\x1b"
                || matches_kitty_sequence(data, CP_ESCAPE, 0)
                || matches_modify_other_keys(data, CP_ESCAPE, 0)
        }

        "space" => {
            if !kitty {
                if modifier == MOD_CTRL && data == "\x00" {
                    return true;
                }
                if modifier == MOD_ALT && data == "\x1b " {
                    return true;
                }
            }
            if modifier == 0 {
                return data == " "
                    || matches_kitty_sequence(data, CP_SPACE, 0)
                    || matches_modify_other_keys(data, CP_SPACE, 0);
            }
            matches_kitty_sequence(data, CP_SPACE, modifier)
                || matches_modify_other_keys(data, CP_SPACE, modifier)
        }

        "tab" => {
            if modifier == MOD_SHIFT {
                return data == "\x1b[Z"
                    || matches_kitty_sequence(data, CP_TAB, MOD_SHIFT)
                    || matches_modify_other_keys(data, CP_TAB, MOD_SHIFT);
            }
            if modifier == 0 {
                return data == "\t" || matches_kitty_sequence(data, CP_TAB, 0);
            }
            matches_kitty_sequence(data, CP_TAB, modifier)
                || matches_modify_other_keys(data, CP_TAB, modifier)
        }

        "enter" | "return" => {
            if modifier == MOD_SHIFT {
                if matches_kitty_sequence(data, CP_ENTER, MOD_SHIFT)
                    || matches_kitty_sequence(data, CP_KP_ENTER, MOD_SHIFT)
                {
                    return true;
                }
                if matches_modify_other_keys(data, CP_ENTER, MOD_SHIFT) {
                    return true;
                }
                if kitty {
                    return data == "\x1b\r" || data == "\n";
                }
                return false;
            }
            if modifier == MOD_ALT {
                if matches_kitty_sequence(data, CP_ENTER, MOD_ALT)
                    || matches_kitty_sequence(data, CP_KP_ENTER, MOD_ALT)
                {
                    return true;
                }
                if matches_modify_other_keys(data, CP_ENTER, MOD_ALT) {
                    return true;
                }
                if !kitty {
                    return data == "\x1b\r";
                }
                return false;
            }
            if modifier == 0 {
                return data == "\r"
                    || (!kitty && data == "\n")
                    || data == "\x1bOM"
                    || matches_kitty_sequence(data, CP_ENTER, 0)
                    || matches_kitty_sequence(data, CP_KP_ENTER, 0);
            }
            matches_kitty_sequence(data, CP_ENTER, modifier)
                || matches_kitty_sequence(data, CP_KP_ENTER, modifier)
                || matches_modify_other_keys(data, CP_ENTER, modifier)
        }

        "backspace" => {
            if modifier == MOD_ALT {
                if data == "\x1b\x7f" || data == "\x1b\x08" {
                    return true;
                }
                return matches_kitty_sequence(data, CP_BACKSPACE, MOD_ALT)
                    || matches_modify_other_keys(data, CP_BACKSPACE, MOD_ALT);
            }
            if modifier == MOD_CTRL {
                if matches_raw_backspace(data, MOD_CTRL) {
                    return true;
                }
                return matches_kitty_sequence(data, CP_BACKSPACE, MOD_CTRL)
                    || matches_modify_other_keys(data, CP_BACKSPACE, MOD_CTRL);
            }
            if modifier == 0 {
                return matches_raw_backspace(data, 0)
                    || matches_kitty_sequence(data, CP_BACKSPACE, 0)
                    || matches_modify_other_keys(data, CP_BACKSPACE, 0);
            }
            matches_kitty_sequence(data, CP_BACKSPACE, modifier)
                || matches_modify_other_keys(data, CP_BACKSPACE, modifier)
        }

        "insert" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("insert"))
                    || matches_kitty_sequence(data, FUNC_INSERT, 0);
            }
            if matches_legacy_modifier_sequence(data, "insert", modifier) {
                return true;
            }
            matches_kitty_sequence(data, FUNC_INSERT, modifier)
        }

        "delete" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("delete"))
                    || matches_kitty_sequence(data, FUNC_DELETE, 0);
            }
            if matches_legacy_modifier_sequence(data, "delete", modifier) {
                return true;
            }
            matches_kitty_sequence(data, FUNC_DELETE, modifier)
        }

        "clear" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("clear"));
            }
            matches_legacy_modifier_sequence(data, "clear", modifier)
        }

        "home" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("home"))
                    || matches_kitty_sequence(data, FUNC_HOME, 0);
            }
            if matches_legacy_modifier_sequence(data, "home", modifier) {
                return true;
            }
            matches_kitty_sequence(data, FUNC_HOME, modifier)
        }

        "end" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("end"))
                    || matches_kitty_sequence(data, FUNC_END, 0);
            }
            if matches_legacy_modifier_sequence(data, "end", modifier) {
                return true;
            }
            matches_kitty_sequence(data, FUNC_END, modifier)
        }

        "pageup" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("pageUp"))
                    || matches_kitty_sequence(data, FUNC_PAGE_UP, 0);
            }
            if matches_legacy_modifier_sequence(data, "pageUp", modifier) {
                return true;
            }
            matches_kitty_sequence(data, FUNC_PAGE_UP, modifier)
        }

        "pagedown" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("pageDown"))
                    || matches_kitty_sequence(data, FUNC_PAGE_DOWN, 0);
            }
            if matches_legacy_modifier_sequence(data, "pageDown", modifier) {
                return true;
            }
            matches_kitty_sequence(data, FUNC_PAGE_DOWN, modifier)
        }

        "up" => {
            if modifier == MOD_ALT {
                return data == "\x1bp" || matches_kitty_sequence(data, ARROW_UP, MOD_ALT);
            }
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("up"))
                    || matches_kitty_sequence(data, ARROW_UP, 0);
            }
            if matches_legacy_modifier_sequence(data, "up", modifier) {
                return true;
            }
            matches_kitty_sequence(data, ARROW_UP, modifier)
        }

        "down" => {
            if modifier == MOD_ALT {
                return data == "\x1bn" || matches_kitty_sequence(data, ARROW_DOWN, MOD_ALT);
            }
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("down"))
                    || matches_kitty_sequence(data, ARROW_DOWN, 0);
            }
            if matches_legacy_modifier_sequence(data, "down", modifier) {
                return true;
            }
            matches_kitty_sequence(data, ARROW_DOWN, modifier)
        }

        "left" => {
            if modifier == MOD_ALT {
                return data == "\x1b[1;3D"
                    || (!kitty && data == "\x1bB")
                    || data == "\x1bb"
                    || matches_kitty_sequence(data, ARROW_LEFT, MOD_ALT);
            }
            if modifier == MOD_CTRL {
                return data == "\x1b[1;5D"
                    || matches_legacy_modifier_sequence(data, "left", MOD_CTRL)
                    || matches_kitty_sequence(data, ARROW_LEFT, MOD_CTRL);
            }
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("left"))
                    || matches_kitty_sequence(data, ARROW_LEFT, 0);
            }
            if matches_legacy_modifier_sequence(data, "left", modifier) {
                return true;
            }
            matches_kitty_sequence(data, ARROW_LEFT, modifier)
        }

        "right" => {
            if modifier == MOD_ALT {
                return data == "\x1b[1;3C"
                    || (!kitty && data == "\x1bF")
                    || data == "\x1bf"
                    || matches_kitty_sequence(data, ARROW_RIGHT, MOD_ALT);
            }
            if modifier == MOD_CTRL {
                return data == "\x1b[1;5C"
                    || matches_legacy_modifier_sequence(data, "right", MOD_CTRL)
                    || matches_kitty_sequence(data, ARROW_RIGHT, MOD_CTRL);
            }
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("right"))
                    || matches_kitty_sequence(data, ARROW_RIGHT, 0);
            }
            if matches_legacy_modifier_sequence(data, "right", modifier) {
                return true;
            }
            matches_kitty_sequence(data, ARROW_RIGHT, modifier)
        }

        "f1" | "f2" | "f3" | "f4" | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12" => {
            if modifier != 0 {
                return false;
            }
            matches_legacy_sequence(data, legacy_key_sequences(key))
        }

        _ => matches_printable_key(data, key, modifier, kitty),
    }
}

// Single letter/digit/symbol keys.
fn matches_printable_key(data: &str, key: &str, modifier: i64, kitty: bool) -> bool {
    if key.chars().count() != 1 {
        return false;
    }
    let key_char = key.chars().next().unwrap();
    let is_letter = key_char.is_ascii_lowercase();
    let is_digit = is_digit_key(key);
    let is_symbol = is_symbol_key(key_char);
    if !(is_letter || is_digit || is_symbol) {
        return false;
    }

    let codepoint = key_char as i64;
    let raw_ctrl = raw_ctrl_char(key);

    if modifier == MOD_CTRL + MOD_ALT && !kitty {
        if let Some(rc) = raw_ctrl {
            // Legacy: ctrl+alt+key is ESC followed by the control character.
            let mut expected = String::from('\x1b');
            expected.push(rc);
            if data == expected {
                return true;
            }
        }
    }

    if modifier == MOD_ALT && !kitty {
        let mut expected = String::from('\x1b');
        expected.push(key_char);
        if data == expected {
            return true;
        }
    }

    if modifier == MOD_CTRL {
        if let Some(rc) = raw_ctrl {
            if data.chars().count() == 1 && data.starts_with(rc) {
                return true;
            }
        }
        return matches_kitty_sequence(data, codepoint, MOD_CTRL)
            || matches_printable_modify_other_keys(data, codepoint, MOD_CTRL);
    }

    if modifier == MOD_SHIFT + MOD_CTRL {
        return matches_kitty_sequence(data, codepoint, MOD_SHIFT + MOD_CTRL)
            || matches_printable_modify_other_keys(data, codepoint, MOD_SHIFT + MOD_CTRL);
    }

    if modifier == MOD_SHIFT {
        if is_letter && data == key.to_uppercase() {
            return true;
        }
        return matches_kitty_sequence(data, codepoint, MOD_SHIFT)
            || matches_printable_modify_other_keys(data, codepoint, MOD_SHIFT);
    }

    if modifier != 0 {
        return matches_kitty_sequence(data, codepoint, modifier)
            || matches_printable_modify_other_keys(data, codepoint, modifier);
    }

    data == key || matches_kitty_sequence(data, codepoint, 0)
}

// =============================================================================
// parseKey
// =============================================================================

fn format_key_name_with_modifiers(key_name: &str, modifier: i64) -> Option<String> {
    let effective_mod = modifier & !LOCK_MASK;
    if (effective_mod & !SUPPORTED_MODIFIER_MASK) != 0 {
        return None;
    }
    let mut mods: Vec<&str> = Vec::new();
    if effective_mod & MOD_SHIFT != 0 {
        mods.push("shift");
    }
    if effective_mod & MOD_CTRL != 0 {
        mods.push("ctrl");
    }
    if effective_mod & MOD_ALT != 0 {
        mods.push("alt");
    }
    if effective_mod & MOD_SUPER != 0 {
        mods.push("super");
    }
    if mods.is_empty() {
        Some(key_name.to_string())
    } else {
        Some(format!("{}+{}", mods.join("+"), key_name))
    }
}

fn format_parsed_key(
    codepoint: i64,
    modifier: i64,
    base_layout_key: Option<i64>,
) -> Option<String> {
    let normalized_codepoint = normalize_kitty_functional_codepoint(codepoint);
    let identity_codepoint =
        normalize_shifted_letter_identity_codepoint(normalized_codepoint, modifier);

    let is_latin_letter = (97..=122).contains(&identity_codepoint);
    let is_digit = (48..=57).contains(&identity_codepoint);
    let is_known_symbol = is_symbol_codepoint(identity_codepoint);
    let effective_codepoint = if is_latin_letter || is_digit || is_known_symbol {
        identity_codepoint
    } else {
        base_layout_key.unwrap_or(identity_codepoint)
    };

    let key_name: Option<String> = if effective_codepoint == CP_ESCAPE {
        Some("escape".to_string())
    } else if effective_codepoint == CP_TAB {
        Some("tab".to_string())
    } else if effective_codepoint == CP_ENTER || effective_codepoint == CP_KP_ENTER {
        Some("enter".to_string())
    } else if effective_codepoint == CP_SPACE {
        Some("space".to_string())
    } else if effective_codepoint == CP_BACKSPACE {
        Some("backspace".to_string())
    } else if effective_codepoint == FUNC_DELETE {
        Some("delete".to_string())
    } else if effective_codepoint == FUNC_INSERT {
        Some("insert".to_string())
    } else if effective_codepoint == FUNC_HOME {
        Some("home".to_string())
    } else if effective_codepoint == FUNC_END {
        Some("end".to_string())
    } else if effective_codepoint == FUNC_PAGE_UP {
        Some("pageUp".to_string())
    } else if effective_codepoint == FUNC_PAGE_DOWN {
        Some("pageDown".to_string())
    } else if effective_codepoint == ARROW_UP {
        Some("up".to_string())
    } else if effective_codepoint == ARROW_DOWN {
        Some("down".to_string())
    } else if effective_codepoint == ARROW_LEFT {
        Some("left".to_string())
    } else if effective_codepoint == ARROW_RIGHT {
        Some("right".to_string())
    } else if (48..=57).contains(&effective_codepoint)
        || (97..=122).contains(&effective_codepoint)
        || is_symbol_codepoint(effective_codepoint)
    {
        from_char_code(effective_codepoint).map(|c| c.to_string())
    } else {
        None
    };

    let key_name = key_name?;
    format_key_name_with_modifiers(&key_name, modifier)
}

/// Parse input data and return the key identifier if recognized. Mirrors pi's
/// `parseKey`.
pub fn parse_key(data: &str) -> Option<String> {
    if let Some(kitty) = parse_kitty_sequence(data) {
        return format_parsed_key(kitty.codepoint, kitty.modifier, kitty.base_layout_key);
    }

    if let Some(mok) = parse_modify_other_keys_sequence(data) {
        return format_parsed_key(mok.codepoint, mok.modifier, None);
    }

    let kitty = is_kitty_protocol_active();

    if kitty && (data == "\x1b\r" || data == "\n") {
        return Some("shift+enter".to_string());
    }

    if let Some(id) = legacy_sequence_key_id(data) {
        return Some(id.to_string());
    }

    if data == "\x1b" {
        return Some("escape".to_string());
    }
    if data == "\x1c" {
        return Some("ctrl+\\".to_string());
    }
    if data == "\x1d" {
        return Some("ctrl+]".to_string());
    }
    if data == "\x1f" {
        return Some("ctrl+-".to_string());
    }
    if data == "\x1b\x1b" {
        return Some("ctrl+alt+[".to_string());
    }
    if data == "\x1b\x1c" {
        return Some("ctrl+alt+\\".to_string());
    }
    if data == "\x1b\x1d" {
        return Some("ctrl+alt+]".to_string());
    }
    if data == "\x1b\x1f" {
        return Some("ctrl+alt+-".to_string());
    }
    if data == "\t" {
        return Some("tab".to_string());
    }
    if data == "\r" || (!kitty && data == "\n") || data == "\x1bOM" {
        return Some("enter".to_string());
    }
    if data == "\x00" {
        return Some("ctrl+space".to_string());
    }
    if data == " " {
        return Some("space".to_string());
    }
    if data == "\x7f" {
        return Some("backspace".to_string());
    }
    if data == "\x08" {
        return Some(if is_windows_terminal_session() {
            "ctrl+backspace".to_string()
        } else {
            "backspace".to_string()
        });
    }
    if data == "\x1b[Z" {
        return Some("shift+tab".to_string());
    }
    if !kitty && data == "\x1b\r" {
        return Some("alt+enter".to_string());
    }
    if !kitty && data == "\x1b " {
        return Some("alt+space".to_string());
    }
    if data == "\x1b\x7f" || data == "\x1b\x08" {
        return Some("alt+backspace".to_string());
    }
    if !kitty && data == "\x1bB" {
        return Some("alt+left".to_string());
    }
    if !kitty && data == "\x1bF" {
        return Some("alt+right".to_string());
    }
    if !kitty && data.encode_utf16().count() == 2 && data.starts_with('\x1b') {
        // Second char is BMP (UTF-16 length 2 total), so `char as u32` equals
        // JS `charCodeAt(1)`.
        let code = data.chars().nth(1).unwrap() as i64;
        if (1..=26).contains(&code) {
            let letter = from_char_code(code + 96).unwrap();
            return Some(format!("ctrl+alt+{letter}"));
        }
        if let Some(ch) = from_char_code(code) {
            if (97..=122).contains(&code) || (48..=57).contains(&code) || is_symbol_key(ch) {
                return Some(format!("alt+{ch}"));
            }
        }
    }
    if data == "\x1b[A" {
        return Some("up".to_string());
    }
    if data == "\x1b[B" {
        return Some("down".to_string());
    }
    if data == "\x1b[C" {
        return Some("right".to_string());
    }
    if data == "\x1b[D" {
        return Some("left".to_string());
    }
    if data == "\x1b[H" || data == "\x1bOH" {
        return Some("home".to_string());
    }
    if data == "\x1b[F" || data == "\x1bOF" {
        return Some("end".to_string());
    }
    if data == "\x1b[3~" {
        return Some("delete".to_string());
    }
    if data == "\x1b[5~" {
        return Some("pageUp".to_string());
    }
    if data == "\x1b[6~" {
        return Some("pageDown".to_string());
    }

    // Raw Ctrl+letter / printable.
    if data.encode_utf16().count() == 1 {
        let code = data.chars().next().unwrap() as i64;
        if (1..=26).contains(&code) {
            let letter = from_char_code(code + 96).unwrap();
            return Some(format!("ctrl+{letter}"));
        }
        if (32..=126).contains(&code) {
            return Some(data.to_string());
        }
    }

    None
}

// =============================================================================
// Event-type detection (substring scans, protocol-state independent)
// =============================================================================

/// Whether the input encodes a Kitty key-release event (flag 3). Mirrors pi.
pub fn is_key_release(data: &str) -> bool {
    // Never treat bracketed-paste content as a release event.
    if data.contains("\x1b[200~") {
        return false;
    }
    data.contains(":3u")
        || data.contains(":3~")
        || data.contains(":3A")
        || data.contains(":3B")
        || data.contains(":3C")
        || data.contains(":3D")
        || data.contains(":3H")
        || data.contains(":3F")
}

/// Whether the input encodes a Kitty key-repeat event (flag 2). Mirrors pi.
pub fn is_key_repeat(data: &str) -> bool {
    if data.contains("\x1b[200~") {
        return false;
    }
    data.contains(":2u")
        || data.contains(":2~")
        || data.contains(":2A")
        || data.contains(":2B")
        || data.contains(":2C")
        || data.contains(":2D")
        || data.contains(":2H")
        || data.contains(":2F")
}

// =============================================================================
// Kitty CSI-u printable decoding
// =============================================================================

const KITTY_PRINTABLE_ALLOWED_MODIFIERS: i64 = MOD_SHIFT | LOCK_MASK;

/// Decode a Kitty CSI-u sequence into a printable character, if applicable.
/// Only accepts plain or Shift-modified keys. Mirrors pi's
/// `decodeKittyPrintable`.
pub fn decode_kitty_printable(data: &str) -> Option<String> {
    let parsed = parse_csi_u_only(data)?;
    let codepoint = parsed.codepoint;
    let modifier = parsed.modifier;

    if (modifier & !KITTY_PRINTABLE_ALLOWED_MODIFIERS) != 0 {
        return None;
    }
    if modifier & (MOD_ALT | MOD_CTRL) != 0 {
        return None;
    }

    let mut effective_codepoint = codepoint;
    if modifier & MOD_SHIFT != 0 {
        if let Some(shifted) = parsed.shifted_key {
            effective_codepoint = shifted;
        }
    }
    effective_codepoint = normalize_kitty_functional_codepoint(effective_codepoint);
    if effective_codepoint < 32 {
        return None;
    }
    from_code_point(effective_codepoint).map(|c| c.to_string())
}

fn decode_modify_other_keys_printable(data: &str) -> Option<String> {
    let parsed = parse_modify_other_keys_sequence(data)?;
    let modifier = parsed.modifier & !LOCK_MASK;
    if (modifier & !MOD_SHIFT) != 0 {
        return None;
    }
    if parsed.codepoint < 32 {
        return None;
    }
    from_code_point(parsed.codepoint).map(|c| c.to_string())
}

/// Decode a printable character from either a Kitty CSI-u or an xterm
/// modifyOtherKeys sequence. Mirrors pi's `decodePrintableKey`.
pub fn decode_printable_key(data: &str) -> Option<String> {
    decode_kitty_printable(data).or_else(|| decode_modify_other_keys_printable(data))
}
