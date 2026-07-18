//! Synthesized context messages mirroring
//! `packages/agent/src/harness/messages.ts` (the subset used by
//! `buildSessionContext`). Messages are opaque JSON objects, matching this
//! port's [`AgentMessage`](crate::harness::types::AgentMessage) representation.

use serde_json::{json, Value};

/// `createBranchSummaryMessage`.
pub fn create_branch_summary_message(summary: &str, from_id: &str, timestamp: &str) -> Value {
    json!({
        "role": "branchSummary",
        "summary": summary,
        "fromId": from_id,
        "timestamp": parse_iso_millis(timestamp),
    })
}

/// `createCompactionSummaryMessage`.
pub fn create_compaction_summary_message(
    summary: &str,
    tokens_before: i64,
    timestamp: &str,
) -> Value {
    json!({
        "role": "compactionSummary",
        "summary": summary,
        "tokensBefore": tokens_before,
        "timestamp": parse_iso_millis(timestamp),
    })
}

/// `createCustomMessage`. `details` is included only when present, mirroring how
/// `JSON.stringify` drops an `undefined` value.
pub fn create_custom_message(
    custom_type: &str,
    content: &Value,
    display: bool,
    details: Option<&Value>,
    timestamp: &str,
) -> Value {
    let mut message = serde_json::Map::new();
    message.insert("role".into(), json!("custom"));
    message.insert("customType".into(), json!(custom_type));
    message.insert("content".into(), content.clone());
    message.insert("display".into(), json!(display));
    if let Some(details) = details {
        message.insert("details".into(), details.clone());
    }
    message.insert("timestamp".into(), json!(parse_iso_millis(timestamp)));
    Value::Object(message)
}

/// Parse an ISO-8601 timestamp (`YYYY-MM-DDTHH:MM:SS[.sss]Z`) to epoch
/// milliseconds. Mirrors `new Date(timestamp).getTime()`; unparseable input
/// yields `0`. The value is informational (no test asserts it).
pub fn parse_iso_millis(timestamp: &str) -> i64 {
    fn digits(s: &str) -> Option<i64> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    }

    fn parse(timestamp: &str) -> Option<i64> {
        let bytes = timestamp.as_bytes();
        if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
            return None;
        }
        let year = digits(&timestamp[0..4])?;
        let month = digits(&timestamp[5..7])?;
        let day = digits(&timestamp[8..10])?;
        let hour = digits(&timestamp[11..13])?;
        let minute = digits(&timestamp[14..16])?;
        let second = digits(&timestamp[17..19])?;
        let millis = if bytes.len() >= 23 && bytes[19] == b'.' {
            digits(&timestamp[20..23]).unwrap_or(0)
        } else {
            0
        };

        let days = days_from_civil(year, month, day);
        Some((((days * 24 + hour) * 60 + minute) * 60 + second) * 1000 + millis)
    }

    parse(timestamp).unwrap_or(0)
}

/// Days since the Unix epoch for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
