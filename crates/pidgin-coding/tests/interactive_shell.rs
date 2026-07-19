// straitjacket-allow-file:duplication — the `SharedSink` in-memory `Write`
// harness and the `bytes(..)` helper faithfully mirror pidgin-tui's run-loop
// test harness (`crates/pidgin-tui/tests/run_loop.rs`): each integration-test
// binary is standalone and cannot import the other's private test helper, so the
// mock-terminal boilerplate is duplicated by design (same pattern the PR-4A
// vector test carries).

//! Headless tests for the interactive shell (Unit 4, PR-4B).
//!
//! Two layers, both fully headless (no TTY):
//!
//! 1. **End-to-end over a mock terminal** — the `Tui<ProcessTerminal>` is driven
//!    over an in-memory `Write` sink with synthetic `ShellEvent::Bytes` (type a
//!    prompt + Enter) followed by a faux turn's core `AgentEvent`s. Asserts the
//!    chat region's rendered write stream contains the user line and the faux
//!    assistant reply.
//! 2. **The event-routing function directly** — synthetic `AgentEvent`s are fed
//!    to a `ChatState` over a shared entry list, and the `ChatRegion` render (and
//!    the streaming/pending-tool bookkeeping) is asserted after each.
//!
//! The faux turn is collected synchronously (`collect_faux_turn`) so CI is
//! deterministic — no worker-thread timing.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use pidgin_agent::types::AgentEvent;
use pidgin_ai::providers::faux::{faux_assistant_message, faux_text, FauxAssistantOptions};
use pidgin_ai::types::ContentBlock;
use pidgin_coding::modes::interactive::routing::{ChatRegion, ChatState};
use pidgin_coding::modes::interactive::theme::{create_theme, parse_theme_json, ColorMode, Theme};
use pidgin_coding::modes::interactive::turn::collect_faux_turn;
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
fn typed_prompt_then_faux_turn_renders_user_line_and_assistant_reply() {
    let buffer = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = SharedSink(Arc::clone(&buffer));
    let terminal = ProcessTerminal::with_size(sink, 60, 24).manage_raw_mode(false);
    assert_eq!(terminal.columns(), 60);
    let mut shell = InteractiveShell::new(terminal);

    // Type a prompt and submit it, then feed the faux turn's events.
    let prompt = "ping the faux";
    let mut events = vec![bytes(prompt), bytes("\r")];
    events.extend(collect_faux_turn(prompt).into_iter().map(ShellEvent::Agent));

    shell.run_events(events).expect("shell runs clean");

    let written = strip_ansi(&String::from_utf8_lossy(&buffer.lock().unwrap()));
    // The submitted user line rendered (the user bubble).
    assert!(
        written.contains("ping the faux"),
        "output missing user line: {written:?}"
    );
    // The faux assistant reply streamed in and rendered.
    assert!(
        written.contains("offline faux assistant"),
        "output missing assistant reply: {written:?}"
    );
}

// --- (2) the event-routing function directly --------------------------------

fn assistant_value(text: &str) -> Value {
    let message = faux_assistant_message(vec![faux_text(text)], FauxAssistantOptions::default(), 0);
    serde_json::to_value(message).expect("assistant serializes")
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
    let status = Rc::new(RefCell::new(Vec::new()));
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

    state.handle_event(&AgentEvent::MessageStart {
        message: assistant_value("partial reply"),
    });
    assert!(state.is_streaming(), "message_start opens a stream");
    assert!(
        rendered(&region).contains("partial reply"),
        "start content should render"
    );

    state.handle_event(&AgentEvent::MessageUpdate {
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
    });
    assert!(
        rendered(&region).contains("expanded"),
        "update should mutate the streaming bubble"
    );

    state.handle_event(&AgentEvent::MessageEnd {
        message: assistant_value("partial reply expanded"),
    });
    assert!(!state.is_streaming(), "message_end finalizes the stream");
}

#[test]
fn routing_ignores_a_user_message_start() {
    let (region, mut state) = router();
    state.handle_event(&AgentEvent::MessageStart {
        message: json!({ "role": "user", "content": "hi", "timestamp": 0 }),
    });
    assert!(!state.is_streaming(), "user message_start opens no stream");
    assert!(
        rendered(&region).is_empty(),
        "user message_start adds no chat entry (submit handler owns it)"
    );
}

#[test]
fn routing_creates_and_resolves_a_tool_panel() {
    let (region, mut state) = router();

    state.handle_event(&AgentEvent::ToolExecutionStart {
        tool_call_id: "call_1".to_string(),
        tool_name: "read".to_string(),
        args: json!({ "path": "a.txt" }),
    });
    assert_eq!(state.pending_tool_count(), 1, "start creates a live panel");

    state.handle_event(&AgentEvent::ToolExecutionEnd {
        tool_call_id: "call_1".to_string(),
        tool_name: "read".to_string(),
        result: tool_result_value("file contents here"),
        is_error: false,
    });
    assert_eq!(state.pending_tool_count(), 0, "end resolves the panel");
    // The panel remains rendered in the chat list after resolving.
    assert!(
        !rendered(&region).is_empty(),
        "resolved tool panel stays in the chat list"
    );
}

#[test]
fn routing_status_placeholder_tracks_turn_lifecycle() {
    let entries = Rc::new(RefCell::new(Vec::new()));
    let status = Rc::new(RefCell::new(Vec::new()));
    let mut state = ChatState::new(entries, Rc::clone(&status), dark_theme(), ".".to_string());

    state.handle_event(&AgentEvent::TurnStart);
    assert_eq!(status.borrow().len(), 1, "turn_start sets a status line");

    state.handle_event(&AgentEvent::AgentEnd {
        messages: Vec::new(),
    });
    assert!(
        status.borrow().is_empty(),
        "agent_end clears the status line"
    );
}
