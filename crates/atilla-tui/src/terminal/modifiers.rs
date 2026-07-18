//! Rust replacement for pi's `darwin-modifiers.c` native addon
//! (`vendor/pi/packages/tui/native/darwin/src/darwin-modifiers.c`) and its
//! `native-modifiers.ts` loader.
//!
//! The C addon exposes `isModifierPressed(name)` which maps a modifier name to
//! a `CGEventFlags` mask, polls the live modifier state with
//! `CGEventSourceFlagsState(kCGEventSourceStateCombinedSessionState)`, and
//! returns whether that mask is set. pi calls this on the Apple Terminal
//! Shift+Enter path (`terminal.ts` `forwardInputSequence`) because Apple
//! Terminal reports Shift+Enter as a bare `\r` with no modifier bits.
//!
//! On macOS this is reimplemented with the `core-graphics` crate's
//! `CGEventFlags` / `CGEventSourceStateID` plus a direct FFI declaration of
//! `CGEventSourceFlagsState` (the crate wraps `CGEventSourceCreate` but not the
//! stateless flags-state query the addon uses). Off macOS — including Linux CI —
//! [`is_native_modifier_pressed`] returns `false`, exactly as pi's loader does
//! when the native helper is absent (`isNativeModifierPressed` returns `false`).

/// Modifier keys the native poll understands, mirroring pi's `ModifierKey`
/// union (`"shift" | "command" | "control" | "option"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifierKey {
    /// The Shift key (`kCGEventFlagMaskShift`).
    Shift,
    /// The Command key (`kCGEventFlagMaskCommand`).
    Command,
    /// The Control key (`kCGEventFlagMaskControl`).
    Control,
    /// The Option/Alt key (`kCGEventFlagMaskAlternate`).
    Option,
}

/// Poll whether `key` is currently physically held, mirroring pi's
/// `isNativeModifierPressed`.
///
/// On macOS this queries CoreGraphics for the combined-session modifier flags.
/// On every other platform it returns `false`, matching pi's behaviour when the
/// darwin native addon is not loadable.
#[cfg(target_os = "macos")]
pub fn is_native_modifier_pressed(key: ModifierKey) -> bool {
    use core_graphics::event::CGEventFlags;
    use core_graphics::event_source::CGEventSourceStateID;

    // `CGEventSourceFlagsState` is a stateless snapshot of the current modifier
    // flags. The `core-graphics` crate does not wrap it (it only wraps the
    // instance-based `CGEventSourceCreate`), so declare it directly against the
    // already-linked CoreGraphics framework.
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceFlagsState(state_id: CGEventSourceStateID) -> CGEventFlags;
    }

    // Same name -> mask mapping as `modifier_mask_for_name` in the C addon.
    let mask = match key {
        ModifierKey::Shift => CGEventFlags::CGEventFlagShift,
        ModifierKey::Command => CGEventFlags::CGEventFlagCommand,
        ModifierKey::Control => CGEventFlags::CGEventFlagControl,
        ModifierKey::Option => CGEventFlags::CGEventFlagAlternate,
    };

    // SAFETY: `CGEventSourceFlagsState` is a pure read of process-global input
    // state and takes a plain enum; there are no pointers or ownership concerns.
    let flags = unsafe { CGEventSourceFlagsState(CGEventSourceStateID::CombinedSessionState) };
    flags.contains(mask)
}

/// Off-macOS fallback: no native modifier state is available, so report "not
/// pressed" as pi does when the addon is unavailable.
#[cfg(not(target_os = "macos"))]
pub fn is_native_modifier_pressed(_key: ModifierKey) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn returns_false_off_macos() {
        assert!(!is_native_modifier_pressed(ModifierKey::Shift));
        assert!(!is_native_modifier_pressed(ModifierKey::Command));
        assert!(!is_native_modifier_pressed(ModifierKey::Control));
        assert!(!is_native_modifier_pressed(ModifierKey::Option));
    }
}
