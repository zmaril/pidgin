//! Open a URL or file in the platform browser / default handler.
//!
//! Ported from pi's `utils/open-browser.ts`. Best-effort and injection-safe:
//! the target is always passed as a single argv element, never through a shell.
//!
//! On Windows this intentionally does *not* use `cmd /c start`: `cmd.exe`
//! re-parses metacharacters (`&`, `|`, `^`, …) before `start` runs, which would
//! make an attacker-controlled URL injectable. It uses
//! `rundll32 url.dll,FileProtocolHandler <target>` instead.
//!
//! Launch failures (e.g. missing `xdg-open`) are swallowed: callers still
//! present the target to the user, so a launcher failure must not crash.

use std::process::{Command, Stdio};

/// Best-effort open of `target` (a URL or file path) in the platform's default
/// handler. Never invokes a shell; never propagates errors.
pub fn open_browser(target: &str) {
    let (cmd, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![target])
    } else if cfg!(target_os = "windows") {
        ("rundll32", vec!["url.dll,FileProtocolHandler", target])
    } else {
        ("xdg-open", vec![target])
    };

    let mut command = Command::new(cmd);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // Detach the launcher into its own process group so it outlives us,
    // mirroring Node's `detached: true` + `unref()`.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    // Best-effort: drop the child handle and swallow any spawn error.
    let _ = command.spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_browser_never_panics() {
        // We can't assert a browser opened, but the call must be infallible.
        open_browser("https://example.com/?a=1&b=2");
        open_browser("/tmp/some-nonexistent-file-xyz.txt");
    }
}
