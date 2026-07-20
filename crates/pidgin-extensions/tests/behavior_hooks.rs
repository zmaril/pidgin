//! Offline shaped-return test for the three Python *behavior-modifying* hooks.
//!
//! Points the loader at the SHIPPED example
//! (`examples/extensions/behavior-hooks-py/index.py`) — the Python extension that
//! registers `before_agent_start`, `input`, and `message_end` hooks — and drives
//! each newly wired emitter through the real `--features python` engine, asserting
//! the shaped return carries the behavior change:
//!
//!   - `emit_before_agent_start` returns a `BeforeAgentStartCombinedResult` whose
//!     `system_prompt` is the pirate-mutated prompt;
//!   - `emit_input` returns an `InputEventResult::Transform` whose text has the
//!     leaked password redacted and the steering note appended (and
//!     `has_handlers("input")` is now truthful);
//!   - `emit_message_end` returns the replacement assistant message with the
//!     signature appended.
//!
//! No network, no API key, no V8 — libpython is embedded via PyO3, so the whole
//! file builds and runs in-sandbox. Gated on the `python` feature (the crate is
//! empty without a feature).
#![cfg(feature = "python")]

// straitjacket-allow-file:duplication -- the load-runner + example-path setup is a
// deliberate parallel of the shared `python_support` scaffolding and the
// `task_list_example` fixture; the shared helper asserts task-list specifics
// (`list_tasks` / `task`) that this behavior-hooks example does not register, so
// the load boilerplate is mirrored here rather than reused. Faithful parallel
// test scaffolding, not incidental repetition.

use std::path::PathBuf;

use serde_json::{json, Value};

use pidgin_coding::core::event_bus::EventBus;
use pidgin_coding::core::extensions::events::selection::{InputEventResult, InputSource};
use pidgin_coding::core::extensions::events::turn::MessageEndEvent;
use pidgin_coding::core::extensions::loader::ExtensionLoader;
use pidgin_coding::core::extensions::runner::ExtensionRunner;

use pidgin_extensions::{create_python_extension_runner, PythonExtensionLoader};

/// Absolute path to the shipped example, resolved relative to this crate so the
/// test is cwd-independent (works in-tree and in the CI `python` job).
fn example_path() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/extensions/behavior-hooks-py/index.py");
    path.canonicalize()
        .unwrap_or_else(|error| panic!("example path {path:?} must exist: {error}"))
}

/// Load the behavior-hooks example through the real [`ExtensionLoader`] seam and
/// build the runner via the factory (reusing the loader's runtime, no re-import).
fn load_runner(ext_path: &str, cwd: &str) -> Box<dyn ExtensionRunner> {
    let loader = PythonExtensionLoader::new();
    let bus = EventBus::new();
    let result =
        loader.load_extensions_cached(std::slice::from_ref(&ext_path.to_string()), cwd, &bus, None);

    assert!(
        result.errors.is_empty(),
        "unexpected load errors: {:?}",
        result.errors
    );
    assert_eq!(result.extensions.len(), 1, "one extension loaded");

    let runtime = result.runtime.expect("loader mints a runtime");
    create_python_extension_runner(result.extensions.clone(), runtime, cwd.to_string())
}

#[test]
fn behavior_hooks_example_mutates_a_turn() {
    let ext_path = example_path();
    let ext_path_str = ext_path.to_str().unwrap().to_string();
    let cwd = ext_path.parent().unwrap().to_str().unwrap().to_string();

    let runner = load_runner(&ext_path_str, &cwd);

    // All three behavior-modifying events are now wired, so `has_handlers` is
    // truthful for each (the turn loop gates `emit_input` on this).
    assert!(
        runner.has_handlers("before_agent_start"),
        "before_agent_start is wired"
    );
    assert!(runner.has_handlers("input"), "input is wired");
    assert!(runner.has_handlers("message_end"), "message_end is wired");

    // ---- before_agent_start: the system prompt is pirate-mutated -------------
    let before = runner
        .emit_before_agent_start("hi", None, "You are helpful.", &Value::Null)
        .expect("before_agent_start returns a combined result");
    let system_prompt = before
        .system_prompt
        .expect("before_agent_start mutated the system prompt");
    assert!(
        system_prompt.contains("pirate"),
        "system prompt carries the pirate directive, got {system_prompt:?}"
    );
    // The base prompt is chained, not discarded.
    assert!(
        system_prompt.starts_with("You are helpful."),
        "the base system prompt is preserved, got {system_prompt:?}"
    );
    // No message was injected by this example.
    assert!(before.messages.is_none(), "no messages injected");

    // ---- input: the leaked password is redacted and a note appended ----------
    let input = runner.emit_input(
        "my password is hunter2",
        None,
        InputSource::Interactive,
        None,
    );
    let InputEventResult::Transform { text, images } = input else {
        panic!("input handler transforms the text, got {input:?}");
    };
    assert!(
        text.contains("[REDACTED]"),
        "the password is redacted, got {text:?}"
    );
    assert!(
        !text.contains("hunter2"),
        "the leaked secret is gone, got {text:?}"
    );
    assert!(
        text.contains("concise"),
        "the steering note is appended, got {text:?}"
    );
    assert!(images.is_none(), "no images were supplied");

    // ---- message_end: the finalized assistant message is signed --------------
    let message = json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": "Ahoy" }],
    });
    let replacement = runner
        .emit_message_end(&MessageEndEvent {
            message: message.clone(),
        })
        .expect("message_end returns a replacement message");
    // The role is preserved (the engine's same-role guard).
    assert_eq!(replacement["role"], json!("assistant"));
    let text = replacement["content"][0]["text"]
        .as_str()
        .expect("first content block is text");
    assert!(
        text.starts_with("Ahoy"),
        "the original text is preserved, got {text:?}"
    );
    assert!(
        text.contains("behavior-hooks-py"),
        "the signature is appended, got {text:?}"
    );

    // A `message_end` with no registered replacement path still returns None when
    // the message is unchanged is covered by the engine unit tests; here we assert
    // the wired dispatch produced a real replacement (Some) above.
}
