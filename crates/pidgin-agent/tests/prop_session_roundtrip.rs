//! Bounded property test: any valid `SessionTreeEntry` survives a
//! serialize -> parse round-trip unchanged.
//!
//! Serialization uses `serialize_entry_line` — the exact serializer
//! `golden_vectors.rs` and the JSONL storage layer use. Parsing uses the same
//! `serde_json` deserialize path `jsonl_storage::parse_entry_line` applies.
//! A hand-rolled strategy builds entries across every variant with randomized
//! fields, optional fields present or absent, and `null`-vs-omitted where the
//! type allows. 128 cases, no on-disk failure persistence.

use pidgin_agent::harness::session::serialize_entry_line;
use pidgin_agent::harness::types::{
    ActiveToolsChangeEntry, BranchSummaryEntry, CompactionEntry, CustomEntry, CustomMessageEntry,
    LabelEntry, LeafEntry, MessageEntry, ModelChangeEntry, SessionInfoEntry, SessionTreeEntry,
    ThinkingLevelChangeEntry,
};
use proptest::prelude::*;
use serde_json::Value;

/// A short arbitrary string (may contain control chars, quotes, and unicode —
/// all of which round-trip through JSON).
fn short_str() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..12).prop_map(|cs| cs.into_iter().collect())
}

/// An optional short string: `Some(..)` or `None` (the latter is either omitted
/// or emitted as JSON `null` depending on the field).
fn opt_str() -> impl Strategy<Value = Option<String>> {
    prop::option::of(short_str())
}

/// A bounded JSON value with no floats (integers, strings, bools, null, and
/// nested arrays/objects thereof), so serialize -> parse is exact.
fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| Value::Number(n.into())),
        short_str().prop_map(Value::String),
    ];
    leaf.prop_recursive(3, 16, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::vec((short_str(), inner), 0..4)
                .prop_map(|kvs| Value::Object(kvs.into_iter().collect())),
        ]
    })
}

/// A JSON payload as pi actually stores in its optional `details` / `data`
/// fields — always a JSON object, never a bare top-level `null`.
///
/// Ground-truthed against pi (`vendor/pi/packages/agent/src/harness`): these
/// fields are declared `details?: T` / `data?: T` on `CompactionEntry`,
/// `BranchSummaryEntry`, `CustomEntry`, and `CustomMessageEntry`
/// (`types.ts`), i.e. present-object-or-omitted. Every append site
/// (`session/session.ts`, `compaction/compaction.ts`) assigns either a
/// concrete object (e.g. `{ readFiles, modifiedFiles }`) or `undefined` (from
/// optional chaining like `hookResult?.summary?.details`) — never a literal
/// `null`. Entries are written with `JSON.stringify(entry)`, which drops
/// `undefined` keys, so pi never emits an explicit top-level `"details": null`
/// / `"data": null` for these fields.
///
/// This matters because the Rust structs tag those fields with
/// `#[serde(skip_serializing_if = "Option::is_none")]`: `Some(Value::Null)`
/// serializes to `"details": null`, which parses back to `None`, so
/// `Some(Value::Null)` is not round-trippable. Since it is not a shape pi can
/// produce, the generator must not emit it — otherwise the proptest
/// intermittently fails on an input that corresponds to no real session line.
/// Nested `null`s *inside* the object are fine: the object key is present, so
/// they round-trip exactly.
fn payload_value() -> impl Strategy<Value = Value> {
    prop::collection::vec((short_str(), json_value()), 0..4)
        .prop_map(|kvs| Value::Object(kvs.into_iter().collect()))
}

fn message_entry() -> impl Strategy<Value = SessionTreeEntry> {
    (short_str(), opt_str(), short_str(), json_value()).prop_map(
        |(id, parent_id, timestamp, message)| {
            SessionTreeEntry::Message(MessageEntry {
                id,
                parent_id,
                timestamp,
                message,
            })
        },
    )
}

fn thinking_entry() -> impl Strategy<Value = SessionTreeEntry> {
    (short_str(), opt_str(), short_str(), short_str()).prop_map(
        |(id, parent_id, timestamp, thinking_level)| {
            SessionTreeEntry::ThinkingLevelChange(ThinkingLevelChangeEntry {
                id,
                parent_id,
                timestamp,
                thinking_level,
            })
        },
    )
}

fn model_change_entry() -> impl Strategy<Value = SessionTreeEntry> {
    (
        short_str(),
        opt_str(),
        short_str(),
        short_str(),
        short_str(),
    )
        .prop_map(|(id, parent_id, timestamp, provider, model_id)| {
            SessionTreeEntry::ModelChange(ModelChangeEntry {
                id,
                parent_id,
                timestamp,
                provider,
                model_id,
            })
        })
}

fn active_tools_entry() -> impl Strategy<Value = SessionTreeEntry> {
    (
        short_str(),
        opt_str(),
        short_str(),
        prop::collection::vec(short_str(), 0..5),
    )
        .prop_map(|(id, parent_id, timestamp, active_tool_names)| {
            SessionTreeEntry::ActiveToolsChange(ActiveToolsChangeEntry {
                id,
                parent_id,
                timestamp,
                active_tool_names,
            })
        })
}

fn compaction_entry() -> impl Strategy<Value = SessionTreeEntry> {
    (
        short_str(),
        opt_str(),
        short_str(),
        short_str(),
        short_str(),
        any::<i64>(),
        prop::option::of(payload_value()),
        prop::option::of(any::<bool>()),
    )
        .prop_map(
            |(
                id,
                parent_id,
                timestamp,
                summary,
                first_kept_entry_id,
                tokens_before,
                details,
                from_hook,
            )| {
                SessionTreeEntry::Compaction(CompactionEntry {
                    id,
                    parent_id,
                    timestamp,
                    summary,
                    first_kept_entry_id,
                    tokens_before,
                    details,
                    from_hook,
                })
            },
        )
}

fn branch_summary_entry() -> impl Strategy<Value = SessionTreeEntry> {
    (
        short_str(),
        opt_str(),
        short_str(),
        short_str(),
        short_str(),
        prop::option::of(payload_value()),
        prop::option::of(any::<bool>()),
    )
        .prop_map(
            |(id, parent_id, timestamp, from_id, summary, details, from_hook)| {
                SessionTreeEntry::BranchSummary(BranchSummaryEntry {
                    id,
                    parent_id,
                    timestamp,
                    from_id,
                    summary,
                    details,
                    from_hook,
                })
            },
        )
}

fn custom_entry() -> impl Strategy<Value = SessionTreeEntry> {
    (
        short_str(),
        opt_str(),
        short_str(),
        short_str(),
        prop::option::of(payload_value()),
    )
        .prop_map(|(id, parent_id, timestamp, custom_type, data)| {
            SessionTreeEntry::Custom(CustomEntry {
                id,
                parent_id,
                timestamp,
                custom_type,
                data,
            })
        })
}

fn custom_message_entry() -> impl Strategy<Value = SessionTreeEntry> {
    (
        short_str(),
        opt_str(),
        short_str(),
        short_str(),
        json_value(),
        any::<bool>(),
        prop::option::of(payload_value()),
    )
        .prop_map(
            |(id, parent_id, timestamp, custom_type, content, display, details)| {
                SessionTreeEntry::CustomMessage(CustomMessageEntry {
                    id,
                    parent_id,
                    timestamp,
                    custom_type,
                    content,
                    display,
                    details,
                })
            },
        )
}

fn label_entry() -> impl Strategy<Value = SessionTreeEntry> {
    (short_str(), opt_str(), short_str(), short_str(), opt_str()).prop_map(
        |(id, parent_id, timestamp, target_id, label)| {
            SessionTreeEntry::Label(LabelEntry {
                id,
                parent_id,
                timestamp,
                target_id,
                label,
            })
        },
    )
}

fn session_info_entry() -> impl Strategy<Value = SessionTreeEntry> {
    (short_str(), opt_str(), short_str(), opt_str()).prop_map(|(id, parent_id, timestamp, name)| {
        SessionTreeEntry::SessionInfo(SessionInfoEntry {
            id,
            parent_id,
            timestamp,
            name,
        })
    })
}

fn leaf_entry() -> impl Strategy<Value = SessionTreeEntry> {
    (short_str(), opt_str(), short_str(), opt_str()).prop_map(
        |(id, parent_id, timestamp, target_id)| {
            SessionTreeEntry::Leaf(LeafEntry {
                id,
                parent_id,
                timestamp,
                target_id,
            })
        },
    )
}

fn session_tree_entry() -> impl Strategy<Value = SessionTreeEntry> {
    prop_oneof![
        message_entry(),
        thinking_entry(),
        model_change_entry(),
        active_tools_entry(),
        compaction_entry(),
        branch_summary_entry(),
        custom_entry(),
        custom_message_entry(),
        label_entry(),
        session_info_entry(),
        leaf_entry(),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, failure_persistence: None, ..ProptestConfig::default() })]

    /// Any valid entry serialized with `serialize_entry_line` parses back to an
    /// equal value.
    #[test]
    fn entry_serialize_parse_round_trips(entry in session_tree_entry()) {
        let line = serialize_entry_line(&entry);
        prop_assert!(line.ends_with('\n'), "serialized line must end with LF");
        let parsed: SessionTreeEntry = serde_json::from_str(line.trim_end())
            .expect("serialized entry must parse back");
        prop_assert_eq!(parsed, entry);
    }
}
