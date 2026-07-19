//! Headless run-loop tests (Units 2 & 3). These drive the interactive
//! input-dispatch loop over a real [`pidgin_tui::ProcessTerminal`] whose output
//! sink is an in-memory buffer, fed with SYNTHETIC scripted byte input instead
//! of a real stdin. That exercises the exact dispatch/resize/exit logic the live
//! loop uses (`RunLoop::run` only adds the stdin thread + timers on top) while
//! running fully headless in CI, where there is no TTY.
//!
//! The stdin *reader* thread (Unit 1) has its own loopback test using an OS
//! socket pair in `src/app.rs`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use pidgin_tui::{
    mount_focused_editor, Editor, EditorOptions, EditorTheme, InputListenerResult, LoopEvent,
    ProcessTerminal, RunLoop, SelectListTheme, SharedLines, Terminal, Tui,
};

/// A `Write` sink over a shared byte buffer, so a test can inspect everything the
/// renderer wrote to the terminal after the loop finishes.
#[derive(Clone)]
struct SharedSink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn editor_theme() -> EditorTheme {
    EditorTheme {
        border_color: Box::new(|t: &str| t.to_string()),
        select_list: SelectListTheme {
            selected_prefix: Box::new(|t: &str| t.to_string()),
            selected_text: Box::new(|t: &str| t.to_string()),
            description: Box::new(|t: &str| t.to_string()),
            scroll_info: Box::new(|t: &str| t.to_string()),
            no_match: Box::new(|t: &str| t.to_string()),
        },
    }
}

/// Build a run loop over an in-memory sink with an `Editor` composed as the
/// focused child (plus an echo buffer above it). Returns the loop, a handle to
/// the shared editor, the echo buffer, and the output byte buffer.
#[allow(clippy::type_complexity)]
fn build_echo_shell() -> (
    RunLoop<SharedSink>,
    Rc<RefCell<Editor>>,
    Rc<RefCell<Vec<String>>>,
    Arc<Mutex<Vec<u8>>>,
) {
    let buffer = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = SharedSink(Arc::clone(&buffer));
    // manage_raw_mode(false): no TTY in CI, so only the escape protocol is
    // emitted; raw mode is never toggled.
    let terminal = ProcessTerminal::with_size(sink, 40, 10).manage_raw_mode(false);
    let mut tui = Tui::new(terminal, true);

    // Echoed submitted lines, rendered above the editor.
    let echo = SharedLines::new();
    let echo_handle = echo.handle();
    tui.add_child(Box::new(echo));

    // The editor: echo submitted text on Enter, then it auto-clears.
    let mut editor = Editor::new(editor_theme(), EditorOptions::default());
    editor.set_terminal_rows(10);
    let echo_for_submit = Rc::clone(&echo_handle);
    editor.on_submit = Some(Box::new(move |line: String| {
        echo_for_submit.borrow_mut().push(format!("> {line}"));
    }));
    let editor = Rc::new(RefCell::new(editor));

    // Compose the editor as the focused child (render-tree child + focus target).
    mount_focused_editor(&mut tui, Rc::clone(&editor));

    (RunLoop::new(tui), editor, echo_handle, buffer)
}

fn bytes(s: &str) -> LoopEvent {
    LoopEvent::Bytes(s.as_bytes().to_vec())
}

#[test]
fn typed_input_reaches_focused_editor_and_renders() {
    let (mut run_loop, editor, _echo, buffer) = build_echo_shell();

    run_loop
        .run_events(vec![bytes("h"), bytes("i"), bytes("!")])
        .expect("loop runs clean");

    // (a) The focused editor received the decoded key inputs.
    assert_eq!(editor.borrow().get_text(), "hi!");

    // (b) The rendered write stream contains the typed characters.
    let written = String::from_utf8_lossy(&buffer.lock().unwrap()).into_owned();
    assert!(
        written.contains("hi!"),
        "output missing typed text: {written:?}"
    );
}

#[test]
fn enter_submits_and_echoes_a_line_then_clears_editor() {
    let (mut run_loop, editor, echo, _buffer) = build_echo_shell();

    // Type "abc" then Enter (\r): the editor submits and clears; the shell echoes.
    run_loop
        .run_events(vec![bytes("abc"), bytes("\r"), bytes("de")])
        .expect("loop runs clean");

    assert_eq!(echo.borrow().as_slice(), ["> abc".to_string()]);
    // After submit the editor cleared and accepted the next keystrokes.
    assert_eq!(editor.borrow().get_text(), "de");
}

#[test]
fn resize_forces_a_full_redraw() {
    let (mut run_loop, _editor, _echo, buffer) = build_echo_shell();

    run_loop
        .run_events(vec![bytes("x"), LoopEvent::Resize(60, 20)])
        .expect("loop runs clean");

    // A resize sets the new size and forces a full redraw.
    assert_eq!(run_loop.tui().terminal().columns(), 60);
    assert_eq!(run_loop.tui().terminal().rows(), 20);
    assert!(run_loop.tui().full_redraws() >= 1);
    let written = String::from_utf8_lossy(&buffer.lock().unwrap()).into_owned();
    assert!(written.contains('x'), "redraw should re-emit content");
}

#[test]
fn input_listener_can_consume_ctrl_c_and_request_exit() {
    let (mut run_loop, editor, _echo, _buffer) = build_echo_shell();

    // Shell-level exit policy: an input listener that consumes Ctrl-C (0x03) and
    // sets the loop's exit flag — the loop surfaces the input, the shell decides.
    let exit = run_loop.exit_flag();
    run_loop.tui_mut().add_input_listener(move |data: &str| {
        if data == "\x03" {
            exit.set(true);
            InputListenerResult::consumed()
        } else {
            InputListenerResult::pass()
        }
    });

    run_loop
        .run_events(vec![bytes("a"), bytes("\x03"), bytes("b")])
        .expect("loop runs clean");

    // "a" reached the editor; Ctrl-C was consumed (never reached the editor) and
    // stopped the loop before "b" was processed.
    assert_eq!(editor.borrow().get_text(), "a");
    assert!(run_loop.exit_flag().get(), "exit flag should be set");
}

#[test]
fn input_listener_can_rewrite_input_before_the_editor() {
    let (mut run_loop, editor, _echo, _buffer) = build_echo_shell();

    // A rewriting listener: turn every 'a' into 'z' before it reaches the editor.
    run_loop.tui_mut().add_input_listener(|data: &str| {
        if data == "a" {
            InputListenerResult {
                consume: false,
                data: Some("z".to_string()),
            }
        } else {
            InputListenerResult::pass()
        }
    });

    run_loop
        .run_events(vec![bytes("a"), bytes("b"), bytes("a")])
        .expect("loop runs clean");

    assert_eq!(editor.borrow().get_text(), "zbz");
}
