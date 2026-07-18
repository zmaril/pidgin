//! Rust replacement for pi's `win32-console-mode.c` native addon
//! (`vendor/pi/packages/tui/native/win32/src/win32-console-mode.c`).
//!
//! The C addon exposes `enableVirtualTerminalInput()` which, on the stdin
//! console handle, ORs `ENABLE_VIRTUAL_TERMINAL_INPUT` (0x0200) into the console
//! mode via `GetStdHandle` / `GetConsoleMode` / `SetConsoleMode`. pi calls this
//! from `terminal.ts` (`enableWindowsVTInput`) *after* raw mode is enabled, so
//! the console emits VT escape sequences for modified keys (e.g. `\x1b[Z` for
//! Shift+Tab) instead of raw console events that discard modifier state.
//!
//! crossterm enables virtual-terminal *output* processing when it sets up the
//! screen but does not set `ENABLE_VIRTUAL_TERMINAL_INPUT` on stdin, so this
//! flag is set directly through `windows-sys`, matching the addon's exact
//! effect. Off Windows — including Linux CI — [`enable_virtual_terminal_input`]
//! is a no-op returning `false`, matching pi's early `return` when
//! `process.platform !== "win32"`.

/// Enable `ENABLE_VIRTUAL_TERMINAL_INPUT` on the stdin console handle. Returns
/// `true` on success, mirroring the boolean the C addon returns. Off Windows
/// this is a no-op returning `false`.
#[cfg(windows)]
pub fn enable_virtual_terminal_input() -> bool {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_INPUT,
        STD_INPUT_HANDLE,
    };

    // SAFETY: standard Win32 console calls. The handle from `GetStdHandle` is a
    // borrowed process handle we neither own nor free; `mode` is a stack local
    // written only on `GetConsoleMode` success.
    unsafe {
        let handle = GetStdHandle(STD_INPUT_HANDLE);
        if handle == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return false;
        }
        SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_INPUT) != 0
    }
}

/// Off-Windows fallback: nothing to do (VT input is the default on Unix ttys),
/// matching pi's early return on non-`win32` platforms.
#[cfg(not(windows))]
pub fn enable_virtual_terminal_input() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn noop_off_windows() {
        assert!(!enable_virtual_terminal_input());
    }
}
