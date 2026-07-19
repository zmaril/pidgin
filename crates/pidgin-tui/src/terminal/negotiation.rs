//! Keyboard-protocol negotiation parsing and terminal control constants, ported
//! from pi's `terminal.ts` (`vendor/pi/packages/tui/src/terminal.ts`).
//!
//! These are the pure pieces of pi's `ProcessTerminal`: the escape-sequence
//! constants it writes/reads and the parsers that classify a terminal's reply
//! to the Kitty keyboard-protocol query. [`ProcessTerminal`](super::ProcessTerminal)
//! drives the surrounding I/O state machine; keeping the classification pure
//! means it runs and is unit-tested on Linux CI.

// --- Control constants (verbatim from terminal.ts) -------------------------

/// Keepalive cadence for the OSC 9;4 progress indicator.
pub const TERMINAL_PROGRESS_KEEPALIVE_MS: u64 = 1000;
/// OSC 9;4;3 — indeterminate progress on.
pub const TERMINAL_PROGRESS_ACTIVE_SEQUENCE: &str = "\x1b]9;4;3\x07";
/// OSC 9;4;0 — progress cleared.
pub const TERMINAL_PROGRESS_CLEAR_SEQUENCE: &str = "\x1b]9;4;0;\x07";
/// Apple Terminal reports Shift+Enter as bare `\r`; this is the CSI-u sequence
/// pi substitutes so downstream parsing sees a real Shift+Enter.
pub const APPLE_TERMINAL_SHIFT_ENTER_SEQUENCE: &str = "\x1b[13;2u";
/// Desired Kitty progressive-enhancement flags: 1 (disambiguate) | 2 (event
/// types) | 4 (alternate keys) = 7.
pub const DESIRED_KITTY_KEYBOARD_PROTOCOL_FLAGS: u32 = 7;
/// Time to wait for the rest of a split Kitty/DA reply before flushing it as
/// ordinary input.
pub const KEYBOARD_PROTOCOL_RESPONSE_FRAGMENT_TIMEOUT_MS: u64 = 150;
/// Query written at startup: request desired flags, query them, then a DA
/// sentinel that terminals without Kitty support still answer.
pub const KITTY_KEYBOARD_PROTOCOL_QUERY: &str = "\x1b[>7u\x1b[?u\x1b[c";

/// Enable bracketed paste mode.
pub const BRACKETED_PASTE_ENABLE: &str = "\x1b[?2004h";
/// Disable bracketed paste mode.
pub const BRACKETED_PASTE_DISABLE: &str = "\x1b[?2004l";
/// Enable xterm `modifyOtherKeys` level 2.
pub const MODIFY_OTHER_KEYS_ENABLE: &str = "\x1b[>4;2m";
/// Disable xterm `modifyOtherKeys`.
pub const MODIFY_OTHER_KEYS_DISABLE: &str = "\x1b[>4;0m";
/// Pop/disable the Kitty keyboard protocol.
pub const KITTY_PROTOCOL_DISABLE: &str = "\x1b[<u";
/// Bracketed paste start marker.
pub const BRACKETED_PASTE_START: &str = "\x1b[200~";
/// Bracketed paste end marker.
pub const BRACKETED_PASTE_END: &str = "\x1b[201~";

// --- Negotiation sequence classification -----------------------------------

/// A recognised terminal reply to the keyboard-protocol query, mirroring pi's
/// `KeyboardProtocolNegotiationSequence`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NegotiationSequence {
    /// A Kitty keyboard flags report `\x1b[?<flags>u`.
    KittyFlags(u32),
    /// A device-attributes reply `\x1b[?...c`.
    DeviceAttributes,
}

fn is_all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_digits_or_semicolons(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit() || b == b';')
}

/// Port of pi's `parseKeyboardProtocolNegotiationSequence`.
///
/// `^\x1b\[\?(\d+)u$` → [`NegotiationSequence::KittyFlags`];
/// `^\x1b\[\?[\d;]*c$` → [`NegotiationSequence::DeviceAttributes`].
pub fn parse_negotiation_sequence(sequence: &str) -> Option<NegotiationSequence> {
    if let Some(inner) = sequence
        .strip_prefix("\x1b[?")
        .and_then(|s| s.strip_suffix('u'))
    {
        if is_all_digits(inner) {
            if let Ok(flags) = inner.parse::<u32>() {
                return Some(NegotiationSequence::KittyFlags(flags));
            }
        }
        return None;
    }
    if let Some(inner) = sequence
        .strip_prefix("\x1b[?")
        .and_then(|s| s.strip_suffix('c'))
    {
        if is_digits_or_semicolons(inner) {
            return Some(NegotiationSequence::DeviceAttributes);
        }
    }
    None
}

/// Port of pi's `isKeyboardProtocolNegotiationSequencePrefix`: `sequence` is a
/// (possibly incomplete) start of a negotiation reply we should keep buffering.
///
/// `sequence === "\x1b["` or `^\x1b\[\?[\d;]*$`.
pub fn is_negotiation_sequence_prefix(sequence: &str) -> bool {
    if sequence == "\x1b[" {
        return true;
    }
    match sequence.strip_prefix("\x1b[?") {
        Some(inner) => is_digits_or_semicolons(inner),
        None => false,
    }
}

/// Port of pi's `normalizeAppleTerminalInput`: on Apple Terminal, a bare `\r`
/// with Shift held becomes the explicit Shift+Enter CSI-u sequence.
pub fn normalize_apple_terminal_input(
    data: &str,
    is_apple_terminal: bool,
    is_shift_pressed: bool,
) -> String {
    if is_apple_terminal && data == "\r" && is_shift_pressed {
        APPLE_TERMINAL_SHIFT_ENTER_SEQUENCE.to_string()
    } else {
        data.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kitty_flags() {
        assert_eq!(
            parse_negotiation_sequence("\x1b[?7u"),
            Some(NegotiationSequence::KittyFlags(7))
        );
        assert_eq!(
            parse_negotiation_sequence("\x1b[?0u"),
            Some(NegotiationSequence::KittyFlags(0))
        );
    }

    #[test]
    fn parses_device_attributes() {
        assert_eq!(
            parse_negotiation_sequence("\x1b[?62;1;6c"),
            Some(NegotiationSequence::DeviceAttributes)
        );
        assert_eq!(
            parse_negotiation_sequence("\x1b[?c"),
            Some(NegotiationSequence::DeviceAttributes)
        );
    }

    #[test]
    fn rejects_non_negotiation() {
        assert_eq!(parse_negotiation_sequence("\x1b[A"), None);
        assert_eq!(parse_negotiation_sequence("\x1b[?u"), None); // no digits before u
        assert_eq!(parse_negotiation_sequence("a"), None);
    }

    #[test]
    fn recognises_prefixes() {
        assert!(is_negotiation_sequence_prefix("\x1b["));
        assert!(is_negotiation_sequence_prefix("\x1b[?"));
        assert!(is_negotiation_sequence_prefix("\x1b[?62;1"));
        assert!(!is_negotiation_sequence_prefix("\x1b[?62;1c")); // terminated
        assert!(!is_negotiation_sequence_prefix("\x1b[A"));
    }

    #[test]
    fn apple_shift_enter_normalization() {
        assert_eq!(
            normalize_apple_terminal_input("\r", true, true),
            APPLE_TERMINAL_SHIFT_ENTER_SEQUENCE
        );
        // Not Apple Terminal -> unchanged.
        assert_eq!(normalize_apple_terminal_input("\r", false, true), "\r");
        // No shift -> unchanged.
        assert_eq!(normalize_apple_terminal_input("\r", true, false), "\r");
        // Non-CR input -> unchanged.
        assert_eq!(normalize_apple_terminal_input("a", true, true), "a");
    }
}
