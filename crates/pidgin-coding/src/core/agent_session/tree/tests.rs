//! Tree-navigation and branching tests, ported from pi's
//! `test/agent-session-tree-navigation.test.ts` (10 cases) and
//! `test/agent-session-branching.test.ts` (3 cases).
//!
//! Both pi suites are `describe.skipIf(!API_KEY)` end-to-end tests that drive a
//! real LLM (both to build the conversation via `session.prompt` and to summarize
//! the abandoned branch). The Rust port rebuilds the same session trees over the
//! in-memory harness in [`super::super::test_support`]: a faux stream fn drives the
//! prompts and a [`SummaryModels`] seam (pi's custom-`streamFn` analog) supplies
//! the branch summary. The assertions on the rebuilt leaf, tree structure, editor
//! text, and summary entry are preserved.
//!
//! The pi branching suite tests `runtimeHost.fork(...)` — the `AgentSessionRuntime`
//! orchestrator, which is a **separate later slice** (fork mints a new session
//! file; it is not a method on `AgentSession`). Only the on-session surface those
//! tests exercise, [`AgentSession::get_user_messages_for_forking`], is ported here;
//! the `fork` orchestration assertions are out of scope for this slice.
//!
//! One pi case (`should handle abort during summarization`) asserts
//! `session.isCompacting === true` *while* a summarization awaits the abort signal
//! mid-flight. Under the sync/eager agent `navigate_tree` runs to completion on the
//! calling thread, so that observation window does not exist; the case is
//! `#[ignore]`d with that reason and its non-concurrent abort branch (a summarizer
//! that reports `aborted`) is covered by a dedicated test.

// straitjacket-allow-file:duplication

use std::sync::{Arc, Mutex};

use serde_json::Value;

use pidgin_ai::providers::faux::{faux_assistant_message, FauxAssistantOptions};
use pidgin_ai::{AssistantMessage, Context, Model, StopReason};

use crate::core::agent_session::test_support::{
    assistant_text, create_harness, text_block, FauxResponse, Harness, HarnessOptions,
    SummaryModels, TestExtensionRunner,
};
use crate::core::compaction::{CompletionOptions, Models};
use crate::core::extensions::events::session::{
    SessionBeforeTreeResult, SessionBeforeTreeSummary, SessionTreeEvent,
};
use crate::core::session_manager::{SessionEntry, SessionTreeNode};

use super::NavigateTreeOptions;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Drive `prompts` through the session, each producing one assistant reply, so the
/// session tree is built exactly as pi builds it with `await session.prompt(...)`.
fn drive(harness: &Harness, prompts: &[&str]) {
    let responses: Vec<FauxResponse> = prompts
        .iter()
        .enumerate()
        .map(|(index, _)| {
            FauxResponse::Message(Box::new(assistant_text(&format!("reply {index}"))))
        })
        .collect();
    harness.set_responses(responses);
    for prompt in prompts {
        harness.session.prompt(prompt, None, None).expect("prompt");
    }
}

/// The session's current entries (pi's `sessionManager.getEntries()`).
fn entries(harness: &Harness) -> Vec<SessionEntry> {
    harness.session.session_manager().get_entries()
}

/// The session's current tree (pi's `sessionManager.getTree()`).
fn tree(harness: &Harness) -> Vec<SessionTreeNode> {
    harness.session.session_manager().get_tree()
}

/// The current leaf id (pi's `sessionManager.getLeafId()`).
fn leaf_id(harness: &Harness) -> Option<String> {
    harness
        .session
        .session_manager()
        .get_leaf_id()
        .map(str::to_string)
}

/// The direct children of `parent_id` (pi's `sessionManager.getChildren(id)`).
fn children(harness: &Harness, parent_id: &str) -> Vec<SessionEntry> {
    harness.session.session_manager().get_children(parent_id)
}

/// The message role of an entry, if it is a message entry.
fn message_role(entry: &SessionEntry) -> Option<String> {
    match entry {
        SessionEntry::Message(message_entry) => message_entry
            .message
            .get("role")
            .and_then(Value::as_str)
            .map(String::from),
        _ => None,
    }
}

/// The ids of every user message entry, in order.
fn user_entry_ids(harness: &Harness) -> Vec<String> {
    entries(harness)
        .iter()
        .filter(|entry| message_role(entry).as_deref() == Some("user"))
        .map(|entry| entry.id().to_string())
        .collect()
}

/// The id of the first assistant message entry.
fn first_assistant_id(harness: &Harness) -> String {
    entries(harness)
        .iter()
        .find(|entry| message_role(entry).as_deref() == Some("assistant"))
        .expect("assistant entry")
        .id()
        .to_string()
}

/// The parent id of the entry with `id`.
fn parent_of(harness: &Harness, id: &str) -> Option<String> {
    entries(harness)
        .iter()
        .find(|entry| entry.id() == id)
        .and_then(|entry| entry.parent_id().map(str::to_string))
}

/// A harness carrying a fixed-summary [`Models`] seam (pi's custom-`streamFn`
/// summarizer).
fn harness_with_summary(summary: &str) -> Harness {
    let (models, _calls) = SummaryModels::build(summary);
    create_harness(HarnessOptions {
        summarization_models: Some(models),
        ..Default::default()
    })
}

/// A [`Models`] seam that records the serialized summarization context of its most
/// recent call, so a test can assert the custom instructions reached the prompt.
struct RecordingSummaryModels {
    summary: String,
    last_context: Arc<Mutex<String>>,
}

impl RecordingSummaryModels {
    fn build(summary: &str) -> (Box<dyn Models>, Arc<Mutex<String>>) {
        let last_context = Arc::new(Mutex::new(String::new()));
        let models = Box::new(Self {
            summary: summary.to_string(),
            last_context: Arc::clone(&last_context),
        });
        (models, last_context)
    }
}

impl Models for RecordingSummaryModels {
    fn complete_simple(
        &self,
        model: &Model,
        context: &Context,
        _options: &CompletionOptions,
    ) -> AssistantMessage {
        *self.last_context.lock().unwrap() = serde_json::to_string(context).unwrap_or_default();
        let mut message = faux_assistant_message(
            vec![text_block(&self.summary)],
            FauxAssistantOptions::default(),
            0,
        );
        message.api = model.api.clone();
        message.provider = model.provider.clone();
        message.model = model.id.clone();
        message
    }
}

/// A [`Models`] seam that always reports an aborted summarization (pi's aborted
/// branch-summary stream), exercising the non-concurrent abort path.
struct AbortingSummaryModels;

impl Models for AbortingSummaryModels {
    fn complete_simple(
        &self,
        model: &Model,
        _context: &Context,
        _options: &CompletionOptions,
    ) -> AssistantMessage {
        let mut message = faux_assistant_message(
            Vec::new(),
            FauxAssistantOptions {
                stop_reason: Some(StopReason::Aborted),
                error_message: Some("aborted".to_string()),
                ..Default::default()
            },
            0,
        );
        message.api = model.api.clone();
        message.provider = model.provider.clone();
        message.model = model.id.clone();
        message
    }
}

// ---------------------------------------------------------------------------
// Tree navigation (pi test/agent-session-tree-navigation.test.ts)
// ---------------------------------------------------------------------------

#[test]
fn navigate_to_root_user_message_puts_text_in_editor() {
    let harness = create_harness(HarnessOptions::default());
    drive(&harness, &["First message", "Second message"]);

    let nodes = tree(&harness);
    assert_eq!(nodes.len(), 1);
    let root = &nodes[0];
    assert_eq!(root.entry.type_str(), "message");
    let root_id = root.entry.id().to_string();

    let result = harness
        .session
        .navigate_tree(&root_id, NavigateTreeOptions::default())
        .expect("navigate");

    assert!(!result.cancelled);
    assert_eq!(result.editor_text.as_deref(), Some("First message"));
    // Navigating to the root user message empties the conversation.
    assert_eq!(leaf_id(&harness), None);
}

#[test]
fn navigate_to_non_user_message_without_editor_text() {
    let harness = create_harness(HarnessOptions::default());
    drive(&harness, &["Hello"]);

    let assistant_id = first_assistant_id(&harness);

    let result = harness
        .session
        .navigate_tree(&assistant_id, NavigateTreeOptions::default())
        .expect("navigate");

    assert!(!result.cancelled);
    assert_eq!(result.editor_text, None);
    assert_eq!(leaf_id(&harness).as_deref(), Some(assistant_id.as_str()));
}

#[test]
fn create_branch_summary_when_summarize_true() {
    let harness = harness_with_summary("summary of the abandoned branch");
    drive(&harness, &["What is 2+2?", "What is 3+3?"]);

    let root_id = tree(&harness)[0].entry.id().to_string();

    let result = harness
        .session
        .navigate_tree(
            &root_id,
            NavigateTreeOptions {
                summarize: true,
                ..Default::default()
            },
        )
        .expect("navigate");

    assert!(!result.cancelled);
    assert_eq!(result.editor_text.as_deref(), Some("What is 2+2?"));
    let summary = result.summary_entry.expect("summary entry");
    // The generated summary wraps the model output; the model text is present.
    assert!(summary.summary.contains("summary of the abandoned branch"));
    assert!(!summary.summary.is_empty());
    // The summary is a root entry (parentId = null) since we navigated to root.
    assert_eq!(summary.parent_id, None);
    // The leaf is the summary entry.
    assert_eq!(leaf_id(&harness).as_deref(), Some(summary.id.as_str()));
}

#[test]
fn attach_summary_to_correct_parent_when_navigating_to_nested_user() {
    let harness = harness_with_summary("nested branch summary");
    drive(&harness, &["Message one", "Message two", "Message three"]);

    let user_ids = user_entry_ids(&harness);
    assert_eq!(user_ids.len(), 3);
    let u2 = user_ids[1].clone();
    let a1 = parent_of(&harness, &u2).expect("parent of u2");

    let result = harness
        .session
        .navigate_tree(
            &u2,
            NavigateTreeOptions {
                summarize: true,
                ..Default::default()
            },
        )
        .expect("navigate");

    assert!(!result.cancelled);
    assert_eq!(result.editor_text.as_deref(), Some("Message two"));
    let summary = result.summary_entry.expect("summary entry");
    // The summary is attached to a1 (parent of u2).
    assert_eq!(summary.parent_id.as_deref(), Some(a1.as_str()));

    // a1 now has two children: u2 and the summary.
    let child_types: Vec<String> = children(&harness, &a1)
        .iter()
        .map(|entry| entry.type_str().to_string())
        .collect();
    assert_eq!(child_types.len(), 2);
    assert!(child_types.contains(&"branch_summary".to_string()));
    assert!(child_types.contains(&"message".to_string()));
}

#[test]
fn attach_summary_to_selected_node_when_navigating_to_assistant() {
    let harness = harness_with_summary("assistant-node summary");
    drive(&harness, &["Hello", "Goodbye"]);

    let a1 = first_assistant_id(&harness);

    let result = harness
        .session
        .navigate_tree(
            &a1,
            NavigateTreeOptions {
                summarize: true,
                ..Default::default()
            },
        )
        .expect("navigate");

    assert!(!result.cancelled);
    // No editor text for assistant messages.
    assert_eq!(result.editor_text, None);
    let summary = result.summary_entry.expect("summary entry");
    // The summary is attached to a1 (the selected node).
    assert_eq!(summary.parent_id.as_deref(), Some(a1.as_str()));
    assert_eq!(leaf_id(&harness).as_deref(), Some(summary.id.as_str()));
}

#[test]
#[ignore = "pi asserts isCompacting === true while a summarization awaits the abort \
             mid-flight; the sync/eager agent runs navigate_tree to completion on the \
             calling thread, so that window does not exist. The non-concurrent abort \
             branch is covered by aborted_summarizer_yields_cancelled_result."]
fn handle_abort_during_summarization() {
    let harness = harness_with_summary("unused");
    drive(&harness, &["Tell me about something", "Continue"]);
    // A faithful port would start navigate_tree, observe isCompacting == true, then
    // call abort_branch_summary concurrently. That mid-flight observation is
    // structurally impossible here (see the attribute reason).
    assert!(!harness.session.is_compacting());
}

#[test]
fn aborted_summarizer_yields_cancelled_result() {
    // The non-concurrent abort branch: a summarizer that reports `aborted` yields
    // cancelled + aborted and leaves the tree unchanged (pi's `result.aborted`).
    let harness = create_harness(HarnessOptions {
        summarization_models: Some(Box::new(AbortingSummaryModels)),
        ..Default::default()
    });
    drive(&harness, &["Tell me about something", "Continue"]);

    let entries_before = entries(&harness).len();
    let leaf_before = leaf_id(&harness);
    let root_id = tree(&harness)[0].entry.id().to_string();

    let result = harness
        .session
        .navigate_tree(
            &root_id,
            NavigateTreeOptions {
                summarize: true,
                ..Default::default()
            },
        )
        .expect("navigate");

    assert!(result.cancelled);
    assert!(result.aborted);
    assert!(result.summary_entry.is_none());
    // The session is unchanged.
    assert_eq!(entries(&harness).len(), entries_before);
    assert_eq!(leaf_id(&harness), leaf_before);
    // The branch-summary signal is cleared after the call.
    assert!(!harness.session.is_compacting());
}

#[test]
fn no_summary_created_without_summarize_option() {
    let harness = create_harness(HarnessOptions::default());
    drive(&harness, &["First", "Second"]);

    let entries_before = entries(&harness).len();
    let root_id = tree(&harness)[0].entry.id().to_string();

    harness
        .session
        .navigate_tree(&root_id, NavigateTreeOptions::default())
        .expect("navigate");

    // No new entries are created.
    assert_eq!(entries(&harness).len(), entries_before);
    // No branch_summary entries exist.
    let summaries = entries(&harness)
        .iter()
        .filter(|entry| entry.type_str() == "branch_summary")
        .count();
    assert_eq!(summaries, 0);
}

#[test]
fn navigate_to_same_position_is_noop() {
    let harness = create_harness(HarnessOptions::default());
    drive(&harness, &["Hello"]);

    let leaf_before = leaf_id(&harness).expect("leaf");
    let entries_before = entries(&harness).len();

    let result = harness
        .session
        .navigate_tree(&leaf_before, NavigateTreeOptions::default())
        .expect("navigate");

    assert!(!result.cancelled);
    assert_eq!(leaf_id(&harness).as_deref(), Some(leaf_before.as_str()));
    assert_eq!(entries(&harness).len(), entries_before);
}

#[test]
fn supports_custom_summarization_instructions() {
    let (models, last_context) = RecordingSummaryModels::build("summary body");
    let harness = create_harness(HarnessOptions {
        summarization_models: Some(models),
        ..Default::default()
    });
    drive(&harness, &["What is TypeScript?", "Explain more"]);

    let root_id = tree(&harness)[0].entry.id().to_string();
    let result = harness
        .session
        .navigate_tree(
            &root_id,
            NavigateTreeOptions {
                summarize: true,
                custom_instructions: Some(
                    "After the summary, you MUST end with exactly: MONKEY MONKEY MONKEY."
                        .to_string(),
                ),
                ..Default::default()
            },
        )
        .expect("navigate");

    assert!(result.summary_entry.is_some());
    // The custom instructions were threaded into the summarization prompt.
    let context = last_context.lock().unwrap().clone();
    assert!(context.contains("MONKEY MONKEY MONKEY"));
    assert!(context.contains("Additional focus"));
}

#[test]
fn navigate_between_branches_summarizes_abandoned_branch() {
    let harness = harness_with_summary("summary of the branch we left");
    // Main path: u1 -> a1 -> u2 -> a2.
    drive(&harness, &["Main branch start", "Main branch continue"]);

    let a1 = first_assistant_id(&harness);
    // Create a branch from a1: a1 -> u3 -> a3.
    harness
        .session
        .session_manager()
        .branch(&a1)
        .expect("branch");
    harness.set_responses(vec![FauxResponse::Message(Box::new(assistant_text(
        "branch reply",
    )))]);
    harness
        .session
        .prompt("Branch path", None, None)
        .expect("prompt");

    // Navigate back to u2 (on the main branch) with summarization.
    let user_ids = user_entry_ids(&harness);
    // u2 is the "Main branch continue" user message (second user message created).
    let u2 = user_ids[1].clone();

    let result = harness
        .session
        .navigate_tree(
            &u2,
            NavigateTreeOptions {
                summarize: true,
                ..Default::default()
            },
        )
        .expect("navigate");

    assert!(!result.cancelled);
    assert_eq!(result.editor_text.as_deref(), Some("Main branch continue"));
    let summary = result.summary_entry.expect("summary entry");
    assert!(!summary.summary.is_empty());
}

// ---------------------------------------------------------------------------
// session_before_tree / session_tree extension dispatch (net-new coverage)
// ---------------------------------------------------------------------------

#[test]
fn session_before_tree_can_cancel_navigation() {
    let harness = create_harness(HarnessOptions {
        make_runner: Some(Box::new(|_agent| {
            Box::new(
                TestExtensionRunner::new().with_before_tree(Arc::new(|_event| {
                    SessionBeforeTreeResult {
                        cancel: Some(true),
                        ..Default::default()
                    }
                })),
            )
        })),
        ..Default::default()
    });
    drive(&harness, &["First", "Second"]);

    let leaf_before = leaf_id(&harness);
    let entries_before = entries(&harness).len();
    let root_id = tree(&harness)[0].entry.id().to_string();

    let result = harness
        .session
        .navigate_tree(&root_id, NavigateTreeOptions::default())
        .expect("navigate");

    assert!(result.cancelled);
    assert!(result.summary_entry.is_none());
    // The navigation was cancelled, so the tree is unchanged.
    assert_eq!(leaf_id(&harness), leaf_before);
    assert_eq!(entries(&harness).len(), entries_before);
}

#[test]
fn session_before_tree_can_supply_summary() {
    let harness = create_harness(HarnessOptions {
        // No summarization_models: the extension supplies the summary instead.
        make_runner: Some(Box::new(|_agent| {
            Box::new(
                TestExtensionRunner::new().with_before_tree(Arc::new(|_event| {
                    SessionBeforeTreeResult {
                        summary: Some(SessionBeforeTreeSummary {
                            summary: "extension supplied summary".to_string(),
                            details: None,
                        }),
                        ..Default::default()
                    }
                })),
            )
        })),
        ..Default::default()
    });
    drive(&harness, &["First", "Second"]);

    let root_id = tree(&harness)[0].entry.id().to_string();
    let result = harness
        .session
        .navigate_tree(
            &root_id,
            NavigateTreeOptions {
                summarize: true,
                ..Default::default()
            },
        )
        .expect("navigate");

    assert!(!result.cancelled);
    let summary = result.summary_entry.expect("summary entry");
    assert_eq!(summary.summary, "extension supplied summary");
    // The extension-supplied summary is marked as coming from a hook.
    assert_eq!(summary.from_hook, Some(true));
    assert_eq!(leaf_id(&harness).as_deref(), Some(summary.id.as_str()));
}

#[test]
fn session_tree_event_fires_with_leaf_ids() {
    let sink: Arc<Mutex<Vec<SessionTreeEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_for_runner = Arc::clone(&sink);
    let harness = create_harness(HarnessOptions {
        make_runner: Some(Box::new(move |_agent| {
            Box::new(TestExtensionRunner::new().with_tree_recording(sink_for_runner))
        })),
        ..Default::default()
    });
    // Two turns so the first assistant message (a1) is not the current leaf (a2),
    // making the navigation a real move rather than a no-op.
    drive(&harness, &["Hello", "Goodbye"]);

    let a1 = first_assistant_id(&harness);
    let leaf_before = leaf_id(&harness);

    harness
        .session
        .navigate_tree(&a1, NavigateTreeOptions::default())
        .expect("navigate");

    let events = sink.lock().unwrap();
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.new_leaf_id.as_deref(), Some(a1.as_str()));
    assert_eq!(event.old_leaf_id, leaf_before);
    assert!(event.summary_entry.is_none());
}

#[test]
fn is_compacting_is_false_when_idle() {
    let harness = create_harness(HarnessOptions::default());
    assert!(!harness.session.is_compacting());
    drive(&harness, &["Hello"]);
    assert!(!harness.session.is_compacting());
}

// ---------------------------------------------------------------------------
// Fork-selector (pi test/agent-session-branching.test.ts — getUserMessagesForForking)
// ---------------------------------------------------------------------------

#[test]
fn user_messages_for_forking_from_single_message() {
    let harness = create_harness(HarnessOptions::default());
    drive(&harness, &["Say hello"]);

    let user_messages = harness.session.get_user_messages_for_forking();
    assert_eq!(user_messages.len(), 1);
    assert_eq!(user_messages[0].text, "Say hello");
}

#[test]
fn user_messages_for_forking_in_memory_mode() {
    // The harness session manager is in-memory (pi's --no-session mode).
    let harness = create_harness(HarnessOptions::default());
    drive(&harness, &["Say hi"]);

    assert!(!harness.session.messages().is_empty());
    let user_messages = harness.session.get_user_messages_for_forking();
    assert_eq!(user_messages.len(), 1);
    assert_eq!(user_messages[0].text, "Say hi");
}

#[test]
fn user_messages_for_forking_from_middle_of_conversation() {
    let harness = create_harness(HarnessOptions::default());
    drive(&harness, &["Say one", "Say two", "Say three"]);

    let user_messages = harness.session.get_user_messages_for_forking();
    assert_eq!(user_messages.len(), 3);
    assert_eq!(user_messages[0].text, "Say one");
    assert_eq!(user_messages[1].text, "Say two");
    assert_eq!(user_messages[2].text, "Say three");

    // The middle user message can be located for forking (pi selects it by
    // entryId); the fork orchestration itself is the AgentSessionRuntime slice.
    let middle = &user_messages[1];
    assert_eq!(middle.text, "Say two");
    assert!(!middle.entry_id.is_empty());
}
