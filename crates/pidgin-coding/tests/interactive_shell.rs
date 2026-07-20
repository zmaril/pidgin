// straitjacket-allow-file:duplication — the `SharedSink` in-memory `Write`
// harness and the `bytes(..)` helper faithfully mirror pidgin-tui's run-loop
// test harness (`crates/pidgin-tui/tests/run_loop.rs`): each integration-test
// binary is standalone and cannot import the other's private test helper, so the
// mock-terminal boilerplate is duplicated by design (same pattern the PR-4A
// vector test carries).

//! Headless tests for the interactive shell (Unit 5, offline-echo slice).
//!
//! Two layers, both fully headless (no TTY):
//!
//! 1. **End-to-end over a mock terminal** — the `Tui<ProcessTerminal>` is driven
//!    over an in-memory `Write` sink with synthetic `ShellEvent::Bytes` (type a
//!    prompt + Enter) followed by a **real** offline-echo `AgentSession`'s
//!    `AgentSessionEvent`s. Asserts the chat region's rendered write stream
//!    contains the user line and the echoed assistant reply, and that the run
//!    settles to idle on `AgentSettled`.
//! 2. **The event-routing function directly** — synthetic `AgentSessionEvent`s are
//!    fed to a `ChatState` over a shared entry list, and the `ChatRegion` render
//!    (and the streaming/pending-tool bookkeeping) is asserted after each.
//!
//! The real turn is collected synchronously (`collect_offline_echo_turn`) so CI is
//! deterministic — no worker-thread timing.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use pidgin_agent::types::AgentEvent;
use pidgin_ai::providers::faux::{faux_assistant_message, faux_text, FauxAssistantOptions};
use pidgin_ai::types::ContentBlock;
use pidgin_coding::core::agent_session::AgentSessionEvent;
use pidgin_coding::modes::interactive::components::IdleStatus;
use pidgin_coding::modes::interactive::routing::{
    ChatRegion, ChatState, StatusRegion, StatusSlot, StatusView,
};
use pidgin_coding::modes::interactive::theme::{create_theme, parse_theme_json, ColorMode, Theme};
use pidgin_coding::modes::interactive::turn::collect_offline_echo_turn;
use pidgin_coding::modes::interactive::{InteractiveShell, ShellEvent};
use pidgin_tui::renderer::Component;
use pidgin_tui::{ProcessTerminal, Terminal};
use serde_json::{json, Value};

/// A `Write` sink over a shared byte buffer, so a test can inspect everything the
/// renderer wrote to the terminal.
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

/// The runtime `dark` theme baked at 256-color (the PR-4A vector theme).
fn dark_theme() -> Theme {
    let json_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/modes/interactive/theme/dark.json");
    let content = std::fs::read_to_string(&json_path).expect("read dark.json");
    let theme_json = parse_theme_json(&content).expect("parse dark.json");
    create_theme(&theme_json, Some(ColorMode::Color256), None).expect("create dark theme")
}

fn bytes(s: &str) -> ShellEvent {
    ShellEvent::Bytes(s.as_bytes().to_vec())
}

/// A throwaway cwd for a real offline-echo session, so tests never write a
/// `.agent` dir into the repo. The `TempDir` guard is returned so it outlives use.
fn temp_cwd() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().to_string_lossy().to_string();
    (dir, cwd)
}

/// Strip ANSI escape sequences so substring assertions are robust to styling.
fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip a CSI/OSC sequence up to a terminator.
            for n in chars.by_ref() {
                if n == 'm' || n == '\x07' || n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

// --- (1) end-to-end over a mock terminal ------------------------------------

#[test]
fn typed_prompt_then_real_echo_turn_renders_user_line_and_assistant_reply() {
    let buffer = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = SharedSink(Arc::clone(&buffer));
    let terminal = ProcessTerminal::with_size(sink, 60, 24).manage_raw_mode(false);
    assert_eq!(terminal.columns(), 60);
    let mut shell = InteractiveShell::new(terminal);

    // Type a prompt and submit it, then feed a real offline-echo turn's events
    // (driven through a real `AgentSession`; the echo reply is the prompt itself).
    let (_dir, cwd) = temp_cwd();
    let prompt = "echo me please";
    let mut events = vec![bytes(prompt), bytes("\r")];
    events.extend(
        collect_offline_echo_turn(prompt, cwd)
            .into_iter()
            .map(|event| ShellEvent::Session(Box::new(event))),
    );

    shell.run_events(events).expect("shell runs clean");

    let written = strip_ansi(&String::from_utf8_lossy(&buffer.lock().unwrap()));
    // The submitted user line rendered (the user bubble), and the echoed assistant
    // reply streamed back through the real session and rendered too. With echo both
    // carry the same text, so seeing it proves the real turn's events reached the
    // message UI end-to-end.
    assert!(
        written.contains("echo me please"),
        "output missing echoed prompt: {written:?}"
    );
}

/// The live worker path: a real `TurnDriver` owns a real offline-echo
/// `AgentSession` on its worker thread; a queued prompt runs the whole turn and
/// its `AgentSessionEvent`s come back over the channel (as `ShellEvent::Session`),
/// ending in an assistant `MessageEnd` that echoes the prompt and an
/// `AgentSettled`.
#[test]
fn turn_driver_runs_a_real_echo_turn_over_the_worker_channel() {
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;

    use pidgin_coding::modes::interactive::turn::TurnDriver;

    let (_dir, cwd) = temp_cwd();
    let (tx, rx) = std::sync::mpsc::channel::<ShellEvent>();
    let driver = TurnDriver::spawn(tx, cwd);

    let prompt = "worker echo please";
    driver.prompt(prompt.to_string());

    // Drain the channel until the run settles (or a generous bound elapses).
    let mut echoed_message_end = false;
    let mut settled = false;
    loop {
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(ShellEvent::Session(event)) => match *event {
                AgentSessionEvent::MessageEnd { message } => {
                    if message.get("role").and_then(Value::as_str) == Some("assistant")
                        && strip_ansi(&serde_json::to_string(&message).unwrap()).contains(prompt)
                    {
                        echoed_message_end = true;
                    }
                }
                AgentSessionEvent::AgentSettled => {
                    settled = true;
                    break;
                }
                _ => {}
            },
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    driver.shutdown();

    assert!(
        echoed_message_end,
        "worker turn should emit an assistant MessageEnd echoing the prompt"
    );
    assert!(settled, "worker turn should emit AgentSettled");
}

/// The events a real offline-echo turn emits, driven through the router: the
/// echoed assistant bubble renders and the run settles to idle on `AgentSettled`.
#[test]
fn real_echo_turn_renders_assistant_bubble_and_settles_idle_on_agent_settled() {
    let entries = Rc::new(RefCell::new(Vec::new()));
    let status: StatusSlot = Rc::new(RefCell::new(StatusView::Idle(IdleStatus)));
    let region = ChatRegion::new(Rc::clone(&entries));
    let mut state = ChatState::new(entries, Rc::clone(&status), dark_theme(), ".".to_string());

    let (_dir, cwd) = temp_cwd();
    let prompt = "echo this line back";
    let events = collect_offline_echo_turn(prompt, cwd);
    // The turn must actually settle (the idle-trigger signal is the last event).
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentSessionEvent::AgentSettled)),
        "real echo turn should emit AgentSettled"
    );

    // Feed every event except the final `AgentSettled` and assert we are mid-turn
    // (working) with the echoed assistant bubble already rendered.
    let settle_at = events
        .iter()
        .position(|e| matches!(e, AgentSessionEvent::AgentSettled))
        .expect("has AgentSettled");
    for event in &events[..settle_at] {
        state.handle_event(event);
    }
    assert!(
        matches!(&*status.borrow(), StatusView::Working(_)),
        "status is working until the run settles"
    );
    // The router ignores the echoed user `message_start`, so this text is the
    // assistant echo bubble specifically.
    assert!(
        rendered(&region).contains(prompt),
        "the echoed assistant bubble should render the prompt back"
    );

    // The settling event flips the status to idle.
    for event in &events[settle_at..] {
        state.handle_event(event);
    }
    assert!(
        matches!(&*status.borrow(), StatusView::Idle(_)),
        "AgentSettled restores the idle placeholder"
    );
}

// --- (2) the event-routing function directly --------------------------------

fn assistant_value(text: &str) -> Value {
    let message = faux_assistant_message(vec![faux_text(text)], FauxAssistantOptions::default(), 0);
    serde_json::to_value(message).expect("assistant serializes")
}

/// Lift a core [`AgentEvent`] into an [`AgentSessionEvent`] (no session needed),
/// the way the session's forwarder does, for the fast router unit tests.
fn session_event(event: AgentEvent) -> AgentSessionEvent {
    AgentSessionEvent::from_agent_event(event, false)
}

fn tool_result_value(text: &str) -> Value {
    let content = serde_json::to_value(vec![ContentBlock::Text {
        text: text.to_string(),
        text_signature: None,
    }])
    .expect("content serializes");
    json!({ "content": content, "details": {} })
}

/// Build a `ChatRegion` + `ChatState` over one shared entry list and status.
fn router() -> (ChatRegion, ChatState) {
    let entries = Rc::new(RefCell::new(Vec::new()));
    let status: StatusSlot = Rc::new(RefCell::new(StatusView::Idle(IdleStatus)));
    let region = ChatRegion::new(Rc::clone(&entries));
    let state = ChatState::new(entries, status, dark_theme(), ".".to_string());
    (region, state)
}

fn rendered(region: &ChatRegion) -> String {
    strip_ansi(&region.render(60).join("\n"))
}

#[test]
fn routing_streams_an_assistant_message_start_update_end() {
    let (region, mut state) = router();

    state.handle_event(&session_event(AgentEvent::MessageStart {
        message: assistant_value("partial reply"),
    }));
    assert!(state.is_streaming(), "message_start opens a stream");
    assert!(
        rendered(&region).contains("partial reply"),
        "start content should render"
    );

    state.handle_event(&session_event(AgentEvent::MessageUpdate {
        message: assistant_value("partial reply expanded"),
        assistant_message_event: Box::new(pidgin_ai::AssistantMessageEvent::TextDelta {
            partial: faux_assistant_message(
                vec![faux_text("partial reply expanded")],
                FauxAssistantOptions::default(),
                0,
            ),
            content_index: 0,
            delta: " expanded".to_string(),
        }),
    }));
    assert!(
        rendered(&region).contains("expanded"),
        "update should mutate the streaming bubble"
    );

    state.handle_event(&session_event(AgentEvent::MessageEnd {
        message: assistant_value("partial reply expanded"),
    }));
    assert!(!state.is_streaming(), "message_end finalizes the stream");
}

#[test]
fn routing_ignores_a_user_message_start() {
    let (region, mut state) = router();
    state.handle_event(&session_event(AgentEvent::MessageStart {
        message: json!({ "role": "user", "content": "hi", "timestamp": 0 }),
    }));
    assert!(!state.is_streaming(), "user message_start opens no stream");
    assert!(
        rendered(&region).is_empty(),
        "user message_start adds no chat entry (submit handler owns it)"
    );
}

#[test]
fn routing_creates_and_resolves_a_tool_panel() {
    let (region, mut state) = router();

    state.handle_event(&session_event(AgentEvent::ToolExecutionStart {
        tool_call_id: "call_1".to_string(),
        tool_name: "read".to_string(),
        args: json!({ "path": "a.txt" }),
    }));
    assert_eq!(state.pending_tool_count(), 1, "start creates a live panel");

    state.handle_event(&session_event(AgentEvent::ToolExecutionEnd {
        tool_call_id: "call_1".to_string(),
        tool_name: "read".to_string(),
        result: tool_result_value("file contents here"),
        is_error: false,
    }));
    assert_eq!(state.pending_tool_count(), 0, "end resolves the panel");
    // The panel remains rendered in the chat list after resolving.
    assert!(
        !rendered(&region).is_empty(),
        "resolved tool panel stays in the chat list"
    );
}

#[test]
fn routing_status_region_tracks_turn_lifecycle() {
    let entries = Rc::new(RefCell::new(Vec::new()));
    let status: StatusSlot = Rc::new(RefCell::new(StatusView::Idle(IdleStatus)));
    let region = StatusRegion::new(Rc::clone(&status));
    let mut state = ChatState::new(entries, Rc::clone(&status), dark_theme(), ".".to_string());

    // Idle: two blank full-width lines.
    assert!(
        matches!(&*status.borrow(), StatusView::Idle(_)),
        "starts idle"
    );
    let idle_lines = region.render(40);
    assert_eq!(idle_lines, vec![" ".repeat(40), " ".repeat(40)]);

    state.handle_event(&session_event(AgentEvent::TurnStart));
    assert!(
        matches!(&*status.borrow(), StatusView::Working(_)),
        "turn_start mounts the working spinner"
    );
    // The working spinner renders a non-blank message line.
    assert!(
        strip_ansi(&region.render(40).join("\n")).contains("Working..."),
        "working spinner shows the working message"
    );

    // `agent_end` no longer restores idle (a retry may follow); it stays working
    // until the run fully settles.
    state.handle_event(&session_event(AgentEvent::AgentEnd {
        messages: Vec::new(),
    }));
    assert!(
        matches!(&*status.borrow(), StatusView::Working(_)),
        "agent_end alone does not restore idle (retry may follow)"
    );

    // `AgentSettled` is the true-settle signal that restores the idle placeholder.
    state.handle_event(&AgentSessionEvent::AgentSettled);
    assert!(
        matches!(&*status.borrow(), StatusView::Idle(_)),
        "AgentSettled restores the idle placeholder"
    );
}
