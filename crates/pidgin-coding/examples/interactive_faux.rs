//! Offline interactive `pidgin` session with a canned (faux) assistant turn.
//!
//! This is the Unit-4 PR-4B milestone: a real, runnable interactive session with
//! **no network and no API key**. It composes pi's interactive container tree
//! (header / loaded-resources / chat message list / pending / status / editor /
//! footer, in pi's child order) on a `Tui<ProcessTerminal>`, mounts the editor as
//! the focused prompt, and drives turns through the agent loop with the faux
//! provider on a worker thread.
//!
//! Type a message and press Enter: the user bubble appears immediately, then the
//! canned assistant reply streams in and renders live as the faux provider emits
//! delta events. Ctrl-C twice / Esc twice / Ctrl-D tears the terminal down
//! cleanly (the run loop's teardown guard) and returns.
//!
//! Header, status, and footer are placeholder chrome (a single text line each);
//! the real components land in PR-4C. Tool panels use PR-4A's fallback render.
//!
//! Run it with:
//!
//! ```sh
//! cargo run -p pidgin-coding --example interactive_faux
//! ```

use std::io::stdout;

use pidgin_coding::modes::interactive::InteractiveShell;
use pidgin_tui::ProcessTerminal;

fn main() {
    let terminal = ProcessTerminal::new(stdout());
    let mut shell = InteractiveShell::new(terminal);

    if let Err(err) = shell.run() {
        // Teardown already happened (the run loop's RAII guard); report and exit
        // non-zero.
        eprintln!("interactive faux shell render error: {err}");
        std::process::exit(1);
    }
}
