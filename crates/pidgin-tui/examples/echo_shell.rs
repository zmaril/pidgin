//! Offline echo shell: a real, runnable interactive session with no agent.
//!
//! This is the minimum runnable milestone of the interactive run loop. It
//! composes an [`Editor`] as the focused child of a `Tui<ProcessTerminal>` and
//! drives the live loop ([`RunLoop::run`]): typing edits and re-renders the
//! editor line in place, Enter echoes the submitted line above the prompt, and
//! the exit policy (double Ctrl-C, double Escape, or Ctrl-D) tears the terminal
//! down cleanly and returns. No provider, no model, no network — just the
//! stdin -> feed -> dispatch -> render loop proving the live path end to end.
//!
//! Run it with:
//!
//! ```sh
//! cargo run -p pidgin-tui --example echo_shell
//! ```

use std::cell::RefCell;
use std::io::stdout;
use std::rc::Rc;
use std::time::{Duration, Instant};

use pidgin_tui::{
    mount_focused_editor, Editor, EditorOptions, EditorTheme, InputListenerResult, ProcessTerminal,
    RunLoop, SelectListTheme, SharedLines, Terminal, Tui,
};

/// Time window within which a repeated Ctrl-C / Escape counts as a double press.
const DOUBLE_PRESS_WINDOW: Duration = Duration::from_millis(1000);

fn editor_theme() -> EditorTheme {
    // Dim border, plain autocomplete styling — enough for a real prompt.
    EditorTheme {
        border_color: Box::new(|t: &str| format!("\x1b[2m{t}\x1b[22m")),
        select_list: SelectListTheme {
            selected_prefix: Box::new(|t: &str| format!("\x1b[36m{t}\x1b[39m")),
            selected_text: Box::new(|t: &str| format!("\x1b[36m{t}\x1b[39m")),
            description: Box::new(|t: &str| format!("\x1b[2m{t}\x1b[22m")),
            scroll_info: Box::new(|t: &str| format!("\x1b[2m{t}\x1b[22m")),
            no_match: Box::new(|t: &str| format!("\x1b[2m{t}\x1b[22m")),
        },
    }
}

fn main() {
    let terminal = ProcessTerminal::new(stdout());
    let rows = terminal.rows();
    let mut tui = Tui::new(terminal, true);

    // A static header, the echoed transcript, then the live editor prompt.
    let header = SharedLines::new();
    header.set(vec![
        "pidgin-tui echo shell (offline, agent-free)".to_string(),
        "type and press Enter to echo; Ctrl-C twice / Esc twice / Ctrl-D to exit".to_string(),
        String::new(),
    ]);
    tui.add_child(Box::new(header));

    let echo = SharedLines::new();
    let echo_handle = echo.handle();
    tui.add_child(Box::new(echo));

    let mut editor = Editor::new(editor_theme(), EditorOptions::default());
    editor.set_terminal_rows(rows);
    let echo_for_submit = Rc::clone(&echo_handle);
    editor.on_submit = Some(Box::new(move |line: String| {
        if !line.is_empty() {
            echo_for_submit.borrow_mut().push(format!("> {line}"));
        }
    }));
    let editor = Rc::new(RefCell::new(editor));

    // Compose the editor as the focused child (render-tree child + focus target).
    mount_focused_editor(&mut tui, Rc::clone(&editor));

    let mut run_loop = RunLoop::new(tui);

    // Shell-level exit policy as an input listener (pi's `addInputListener`): the
    // loop surfaces every input; the shell decides when to exit. Ctrl-C and Esc
    // require a double press within the window; Ctrl-D exits immediately. All are
    // consumed so they never reach the editor.
    let exit = run_loop.exit_flag();
    let hint = Rc::clone(&echo_handle);
    let mut last_ctrl_c: Option<Instant> = None;
    let mut last_escape: Option<Instant> = None;
    run_loop.tui_mut().add_input_listener(move |data: &str| {
        match data {
            "\x03" => {
                // Ctrl-C: exit on a second press within the window.
                let now = Instant::now();
                let doubled = last_ctrl_c
                    .map(|t| now.duration_since(t) <= DOUBLE_PRESS_WINDOW)
                    .unwrap_or(false);
                if doubled {
                    exit.set(true);
                } else {
                    last_ctrl_c = Some(now);
                    hint.borrow_mut()
                        .push("(press Ctrl-C again to exit)".to_string());
                }
                InputListenerResult::consumed()
            }
            "\x1b" => {
                // Escape: exit on a second press within the window.
                let now = Instant::now();
                let doubled = last_escape
                    .map(|t| now.duration_since(t) <= DOUBLE_PRESS_WINDOW)
                    .unwrap_or(false);
                if doubled {
                    exit.set(true);
                } else {
                    last_escape = Some(now);
                }
                InputListenerResult::consumed()
            }
            "\x04" => {
                // Ctrl-D: exit immediately (EOF).
                exit.set(true);
                InputListenerResult::consumed()
            }
            _ => InputListenerResult::pass(),
        }
    });

    if let Err(err) = run_loop.run() {
        // Teardown already happened (RAII + panic hook); report and exit non-zero.
        eprintln!("echo shell render error: {err}");
        std::process::exit(1);
    }
}
