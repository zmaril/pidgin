//! Amazon `vnd.amazon.eventstream` binary frame decoder (buffered).
//!
//! Amazon Bedrock's `ConverseStream` response is NOT text SSE: it is the AWS
//! `application/vnd.amazon.eventstream` binary framing that the
//! `@aws-sdk/client-bedrock-runtime` SDK decodes into the `response.stream`
//! async-iterable pi consumes (`for await (const item of response.stream)`,
//! `bedrock-converse-stream.ts:250`). pi never had to write this decoder — the
//! AWS SDK did it — so the ported semantic decoder
//! ([`parse_converse_stream`](super::parse_converse_stream)) consumes
//! ALREADY-PARSED JSON event items ([`Value`]), not raw bytes. This module fills
//! that gap: it turns the raw binary body into the `Vec<Value>` items the
//! semantic decoder expects.
//!
//! # Wire format (AWS event stream message)
//!
//! Each message is a self-framed record:
//!
//! ```text
//!  ┌───────────────── Prelude (12 bytes) ─────────────────┐
//!  │ total_length  : u32 BE  (whole message, incl. these  │
//!  │                          4 bytes and the trailing CRC)│
//!  │ headers_length: u32 BE  (byte length of the headers)  │
//!  │ prelude_crc   : u32 BE  (CRC32 of the first 8 bytes)   │
//!  ├───────────────── Headers (headers_length bytes) ──────┤
//!  │ repeated: name_len:u8, name, value_type:u8, value...  │
//!  ├───────────────── Payload (rest) ──────────────────────┤
//!  │ message_crc   : u32 BE  (CRC32 of every byte before it)│
//!  └───────────────────────────────────────────────────────┘
//! ```
//!
//! A ConverseStream message carries the headers `:message-type` (`event` or
//! `exception`), `:event-type` / `:exception-type` (the union member name, e.g.
//! `contentBlockDelta` or `validationException`), and `:content-type`
//! (`application/json`). The payload is the JSON body of that union member. This
//! decoder wraps each payload under its member-name key — `{ [memberName]:
//! payload }` — which is exactly the item shape
//! [`parse_converse_stream`](super::parse_converse_stream) matches on
//! (`item.get("messageStart")`, `item.get("contentBlockDelta")`,
//! `item.get("validationException")`, ...).
//!
//! # IMPORTANT: feed RAW BYTES, never a `String`-decoded body
//!
//! The [`HttpResponse.body`](crate::seams::http::HttpResponse) seam is a
//! `String`, and the production reqwest transport reads it via
//! `response.text()` (lossy UTF-8, `seams/http_reqwest.rs`). A binary
//! eventstream body contains big-endian lengths and CRC32 words with bytes
//! `>= 0x80`, which are NOT valid UTF-8; passing it through the `String` /
//! `send()` / `.text()` path REPLACES those bytes with U+FFFD and destroys the
//! framing before this decoder ever runs. Consumers MUST therefore obtain the
//! body as raw bytes — collect the `Vec<u8>` chunks from
//! [`HttpTransport::send_streaming`](crate::seams::http::HttpTransport::send_streaming)
//! (whose reqwest override reads the body without `.text()`) — and hand those
//! bytes here. Do not "simplify" a caller back to `send()`.
//!
//! # Buffered, not incremental
//!
//! This decoder consumes the WHOLE body at once and returns every item. True
//! token-by-token streaming (feeding frames into a reader as each arrives off
//! the wire) is a separate follow-up; see the bedrock driver.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::{Map, Value};

/// The fixed prelude size: `total_length` + `headers_length` + `prelude_crc`,
/// each a big-endian `u32`.
const PRELUDE_LEN: usize = 12;
/// The trailing message-CRC size (a big-endian `u32`).
const MESSAGE_CRC_LEN: usize = 4;
/// The smallest possible message: a 12-byte prelude, no headers, no payload, and
/// the 4-byte trailing CRC.
const MIN_MESSAGE_LEN: usize = PRELUDE_LEN + MESSAGE_CRC_LEN;

/// Event-stream header value-type discriminants (the AWS event-stream header
/// spec). Only the string type carries data this decoder retains; the rest are
/// skipped with the correct width so header parsing stays aligned.
mod header_type {
    pub const BOOL_TRUE: u8 = 0;
    pub const BOOL_FALSE: u8 = 1;
    pub const BYTE: u8 = 2;
    pub const SHORT: u8 = 3;
    pub const INTEGER: u8 = 4;
    pub const LONG: u8 = 5;
    pub const BYTE_ARRAY: u8 = 6;
    pub const STRING: u8 = 7;
    pub const TIMESTAMP: u8 = 8;
    pub const UUID: u8 = 9;
}

/// A failure decoding a `vnd.amazon.eventstream` body. Every variant is a clean,
/// reportable error — the decoder never panics on malformed input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventStreamError {
    /// The body ended mid-message: fewer bytes remain than the framing declares.
    Truncated {
        /// Bytes still available at the point the read ran out.
        available: usize,
        /// Bytes the framing required.
        needed: usize,
    },
    /// A message's declared `total_length` is smaller than the minimum framing.
    MalformedLength {
        /// The declared `total_length`.
        total_length: usize,
    },
    /// A message's `headers_length` does not fit within its `total_length`.
    MalformedHeadersLength {
        /// The declared `headers_length`.
        headers_length: usize,
        /// The declared `total_length`.
        total_length: usize,
    },
    /// The prelude CRC32 did not match the CRC of the first 8 prelude bytes.
    PreludeCrcMismatch {
        /// The CRC the message carried.
        expected: u32,
        /// The CRC computed over the prelude.
        actual: u32,
    },
    /// The trailing message CRC32 did not match the CRC of the message body.
    MessageCrcMismatch {
        /// The CRC the message carried.
        expected: u32,
        /// The CRC computed over the message.
        actual: u32,
    },
    /// A header's declared width overran the headers block.
    MalformedHeader,
    /// A header carried a value-type discriminant this decoder does not know.
    UnknownHeaderType(u8),
    /// A message had neither an `:event-type` nor an `:exception-type` header, so
    /// no ConverseStream member name could be derived.
    MissingEventType,
    /// A message payload was not valid JSON.
    InvalidPayloadJson(String),
}

impl fmt::Display for EventStreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EventStreamError::Truncated { available, needed } => write!(
                f,
                "truncated eventstream: {available} bytes available, {needed} needed"
            ),
            EventStreamError::MalformedLength { total_length } => write!(
                f,
                "malformed eventstream message: total_length {total_length} below minimum {MIN_MESSAGE_LEN}"
            ),
            EventStreamError::MalformedHeadersLength {
                headers_length,
                total_length,
            } => write!(
                f,
                "malformed eventstream message: headers_length {headers_length} does not fit total_length {total_length}"
            ),
            EventStreamError::PreludeCrcMismatch { expected, actual } => write!(
                f,
                "eventstream prelude CRC mismatch: expected {expected:#010x}, computed {actual:#010x}"
            ),
            EventStreamError::MessageCrcMismatch { expected, actual } => write!(
                f,
                "eventstream message CRC mismatch: expected {expected:#010x}, computed {actual:#010x}"
            ),
            EventStreamError::MalformedHeader => write!(f, "malformed eventstream header"),
            EventStreamError::UnknownHeaderType(t) => {
                write!(f, "unknown eventstream header value type {t}")
            }
            EventStreamError::MissingEventType => write!(
                f,
                "eventstream message has no :event-type or :exception-type header"
            ),
            EventStreamError::InvalidPayloadJson(err) => {
                write!(f, "invalid eventstream payload JSON: {err}")
            }
        }
    }
}

impl std::error::Error for EventStreamError {}

/// Compute the IEEE 802.3 CRC-32 (the same polynomial `zlib`/`gzip` use, the one
/// AWS event streams checksum with) over `data`.
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            // Branchless reflected-polynomial step: subtract 1 from (crc & 1) to
            // get an all-ones mask when the low bit is set, all-zeros otherwise.
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Read a big-endian `u32` at `offset` in `bytes`, checking bounds.
fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, EventStreamError> {
    let end = offset + 4;
    if end > bytes.len() {
        return Err(EventStreamError::Truncated {
            available: bytes.len().saturating_sub(offset),
            needed: 4,
        });
    }
    Ok(u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]))
}

/// Decode a whole buffered `vnd.amazon.eventstream` body into the ConverseStream
/// items [`parse_converse_stream`](super::parse_converse_stream) consumes.
///
/// Each item is `{ [memberName]: payload }`, where `memberName` is the message's
/// `:event-type` (for `event` messages) or `:exception-type` (for `exception`
/// messages) and `payload` is the parsed JSON body. Messages are validated
/// (prelude + message CRC32, framing bounds) as they are read; any malformed or
/// truncated input yields a clean [`EventStreamError`] rather than a panic.
pub fn decode_event_stream(bytes: &[u8]) -> Result<Vec<Value>, EventStreamError> {
    let mut items = Vec::new();
    let mut offset = 0;

    while offset < bytes.len() {
        let rest = &bytes[offset..];
        if rest.len() < PRELUDE_LEN {
            return Err(EventStreamError::Truncated {
                available: rest.len(),
                needed: PRELUDE_LEN,
            });
        }

        let total_length = read_u32(rest, 0)? as usize;
        let headers_length = read_u32(rest, 4)? as usize;
        let prelude_crc = read_u32(rest, 8)?;

        if total_length < MIN_MESSAGE_LEN {
            return Err(EventStreamError::MalformedLength { total_length });
        }
        if rest.len() < total_length {
            return Err(EventStreamError::Truncated {
                available: rest.len(),
                needed: total_length,
            });
        }
        // Headers must fit between the prelude and the trailing message CRC.
        if PRELUDE_LEN + headers_length + MESSAGE_CRC_LEN > total_length {
            return Err(EventStreamError::MalformedHeadersLength {
                headers_length,
                total_length,
            });
        }

        let message = &rest[..total_length];

        // Prelude CRC covers the first 8 bytes (total_length + headers_length).
        let actual_prelude_crc = crc32(&message[..8]);
        if actual_prelude_crc != prelude_crc {
            return Err(EventStreamError::PreludeCrcMismatch {
                expected: prelude_crc,
                actual: actual_prelude_crc,
            });
        }

        // Message CRC covers every byte from the message start up to (but not
        // including) the trailing 4-byte CRC.
        let crc_offset = total_length - MESSAGE_CRC_LEN;
        let message_crc = read_u32(message, crc_offset)?;
        let actual_message_crc = crc32(&message[..crc_offset]);
        if actual_message_crc != message_crc {
            return Err(EventStreamError::MessageCrcMismatch {
                expected: message_crc,
                actual: actual_message_crc,
            });
        }

        let headers_bytes = &message[PRELUDE_LEN..PRELUDE_LEN + headers_length];
        let payload = &message[PRELUDE_LEN + headers_length..crc_offset];
        let headers = parse_headers(headers_bytes)?;
        items.push(build_item(&headers, payload)?);

        offset += total_length;
    }

    Ok(items)
}

/// Parse the headers block into the string-valued headers this decoder needs
/// (`:event-type`, `:exception-type`, `:message-type`, `:content-type`).
/// Non-string header values are skipped with the correct width so parsing stays
/// aligned; only string (type 7) values are retained.
fn parse_headers(bytes: &[u8]) -> Result<BTreeMap<String, String>, EventStreamError> {
    let mut headers = BTreeMap::new();
    let mut i = 0;

    while i < bytes.len() {
        // name_len (u8) + name.
        let name_len = bytes[i] as usize;
        i += 1;
        let name_end = i + name_len;
        if name_end > bytes.len() {
            return Err(EventStreamError::MalformedHeader);
        }
        let name = String::from_utf8_lossy(&bytes[i..name_end]).into_owned();
        i = name_end;

        // value_type (u8).
        if i >= bytes.len() {
            return Err(EventStreamError::MalformedHeader);
        }
        let value_type = bytes[i];
        i += 1;

        match value_type {
            header_type::BOOL_TRUE | header_type::BOOL_FALSE => {}
            header_type::BYTE => i = advance(i, 1, bytes)?,
            header_type::SHORT => i = advance(i, 2, bytes)?,
            header_type::INTEGER => i = advance(i, 4, bytes)?,
            header_type::LONG => i = advance(i, 8, bytes)?,
            header_type::BYTE_ARRAY | header_type::STRING => {
                // 2-byte big-endian length prefix, then that many bytes.
                if i + 2 > bytes.len() {
                    return Err(EventStreamError::MalformedHeader);
                }
                let value_len = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as usize;
                i += 2;
                let value_end = i + value_len;
                if value_end > bytes.len() {
                    return Err(EventStreamError::MalformedHeader);
                }
                if value_type == header_type::STRING {
                    let value = String::from_utf8_lossy(&bytes[i..value_end]).into_owned();
                    headers.insert(name, value);
                }
                i = value_end;
            }
            header_type::TIMESTAMP => i = advance(i, 8, bytes)?,
            header_type::UUID => i = advance(i, 16, bytes)?,
            other => return Err(EventStreamError::UnknownHeaderType(other)),
        }
    }

    Ok(headers)
}

/// Advance the header cursor by `width` bytes, checking it stays in bounds.
fn advance(i: usize, width: usize, bytes: &[u8]) -> Result<usize, EventStreamError> {
    let next = i + width;
    if next > bytes.len() {
        return Err(EventStreamError::MalformedHeader);
    }
    Ok(next)
}

/// Wrap a message payload under its ConverseStream member-name key, deriving the
/// key from `:event-type` (events) or `:exception-type` (exceptions). An empty
/// payload becomes an empty JSON object (some exception frames carry no body).
fn build_item(
    headers: &BTreeMap<String, String>,
    payload: &[u8],
) -> Result<Value, EventStreamError> {
    let member = headers
        .get(":event-type")
        .or_else(|| headers.get(":exception-type"))
        .ok_or(EventStreamError::MissingEventType)?;

    let payload_value = if payload.is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_slice(payload)
            .map_err(|err| EventStreamError::InvalidPayloadJson(err.to_string()))?
    };

    let mut item = Map::new();
    item.insert(member.clone(), payload_value);
    Ok(Value::Object(item))
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Frame-encoding helpers and a raw-bytes transport shared by this module's
    //! tests and the bedrock driver/backend tests (which build eventstream
    //! fixtures over the same framing). Test-only: compiled only under
    //! `cfg(test)`.

    use std::collections::{BTreeMap, VecDeque};
    use std::io;
    use std::sync::{Arc, Mutex};

    use serde_json::Value;

    use crate::seams::http::{HttpRequest, HttpResponse, HttpStreamResponse, HttpTransport};

    use super::{crc32, header_type, MESSAGE_CRC_LEN, PRELUDE_LEN};

    /// Encode one `event` message: a `:message-type: event` + `:event-type:
    /// <member>` + `:content-type: application/json` header set wrapping the JSON
    /// `payload`, with correctly computed prelude and message CRC32s.
    pub(crate) fn encode_event(member: &str, payload: &Value) -> Vec<u8> {
        encode_message("event", ":event-type", member, payload)
    }

    /// Encode one modeled-`exception` message (`:message-type: exception` +
    /// `:exception-type: <member>`), as Bedrock emits for in-stream errors.
    pub(crate) fn encode_exception(member: &str, payload: &Value) -> Vec<u8> {
        encode_message("exception", ":exception-type", member, payload)
    }

    fn encode_message(
        message_type: &str,
        type_header: &str,
        member: &str,
        payload: &Value,
    ) -> Vec<u8> {
        let mut headers = Vec::new();
        push_string_header(&mut headers, ":message-type", message_type);
        push_string_header(&mut headers, type_header, member);
        push_string_header(&mut headers, ":content-type", "application/json");

        let payload_bytes = serde_json::to_vec(payload).expect("serialize payload");
        frame(&headers, &payload_bytes)
    }

    /// Assemble a full message from an already-encoded headers block and payload,
    /// computing both CRCs.
    pub(crate) fn frame(headers: &[u8], payload: &[u8]) -> Vec<u8> {
        let total_length = PRELUDE_LEN + headers.len() + payload.len() + MESSAGE_CRC_LEN;
        let mut message = Vec::with_capacity(total_length);
        message.extend_from_slice(&(total_length as u32).to_be_bytes());
        message.extend_from_slice(&(headers.len() as u32).to_be_bytes());
        let prelude_crc = crc32(&message[..8]);
        message.extend_from_slice(&prelude_crc.to_be_bytes());
        message.extend_from_slice(headers);
        message.extend_from_slice(payload);
        let message_crc = crc32(&message);
        message.extend_from_slice(&message_crc.to_be_bytes());
        message
    }

    /// Append one `type 7` (string) header: `name_len:u8, name, 0x07,
    /// value_len:u16 BE, value`.
    fn push_string_header(out: &mut Vec<u8>, name: &str, value: &str) {
        out.push(name.len() as u8);
        out.extend_from_slice(name.as_bytes());
        out.push(header_type::STRING);
        out.extend_from_slice(&(value.len() as u16).to_be_bytes());
        out.extend_from_slice(value.as_bytes());
    }

    #[derive(Default)]
    struct BytesState {
        responses: VecDeque<(u16, Vec<u8>)>,
        requests: Vec<HttpRequest>,
    }

    /// A scripted [`HttpTransport`] that returns RAW BYTES over
    /// [`send_streaming`](HttpTransport::send_streaming) — the path the bedrock
    /// driver uses. Unlike [`ScriptedTransport`](crate::seams::http::ScriptedTransport),
    /// whose `String` body cannot hold a binary eventstream (CRC/length bytes
    /// `>= 0x80` are not valid UTF-8), this preserves the exact fixture bytes.
    /// Records every request for assertions.
    #[derive(Clone, Default)]
    pub(crate) struct ScriptedBytesTransport {
        state: Arc<Mutex<BytesState>>,
    }

    impl ScriptedBytesTransport {
        pub(crate) fn new() -> Self {
            Self::default()
        }

        /// Queue a `(status, body-bytes)` response.
        pub(crate) fn push(&self, status: u16, body: Vec<u8>) -> &Self {
            self.state
                .lock()
                .unwrap()
                .responses
                .push_back((status, body));
            self
        }

        /// Every request performed so far, in order.
        pub(crate) fn requests(&self) -> Vec<HttpRequest> {
            self.state.lock().unwrap().requests.clone()
        }

        fn next(&self, request: &HttpRequest) -> io::Result<(u16, Vec<u8>)> {
            let mut state = self.state.lock().unwrap();
            state.requests.push(request.clone());
            state
                .responses
                .pop_front()
                .ok_or_else(|| io::Error::other("ScriptedBytesTransport: no scripted response"))
        }
    }

    impl HttpTransport for ScriptedBytesTransport {
        fn send(&self, request: &HttpRequest) -> io::Result<HttpResponse> {
            // The bedrock driver never calls `send` (binary bodies must not go
            // through the String path); provided only for trait completeness, as
            // a lossy view.
            let (status, body) = self.next(request)?;
            Ok(HttpResponse {
                status,
                headers: BTreeMap::new(),
                body: String::from_utf8_lossy(&body).into_owned(),
            })
        }

        fn send_streaming(&self, request: &HttpRequest) -> io::Result<HttpStreamResponse<'_>> {
            let (status, body) = self.next(request)?;
            Ok(HttpStreamResponse {
                status,
                headers: BTreeMap::new(),
                chunks: Box::new(std::iter::once(Ok(body))),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{encode_event, encode_exception, frame};
    use super::*;

    use serde_json::json;

    #[test]
    fn crc32_matches_known_check_value() {
        // The canonical CRC-32 check value for the ASCII string "123456789".
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn decodes_a_single_event_message() {
        let bytes = encode_event("messageStart", &json!({ "role": "assistant" }));
        let items = decode_event_stream(&bytes).expect("decode");
        assert_eq!(
            items,
            vec![json!({ "messageStart": { "role": "assistant" } })]
        );
    }

    #[test]
    fn decodes_multiple_concatenated_messages_in_order() {
        let mut bytes = Vec::new();
        bytes.extend(encode_event(
            "messageStart",
            &json!({ "role": "assistant" }),
        ));
        bytes.extend(encode_event(
            "contentBlockDelta",
            &json!({ "contentBlockIndex": 0, "delta": { "text": "Hi" } }),
        ));
        bytes.extend(encode_event(
            "contentBlockStop",
            &json!({ "contentBlockIndex": 0 }),
        ));
        bytes.extend(encode_event(
            "messageStop",
            &json!({ "stopReason": "end_turn" }),
        ));

        let items = decode_event_stream(&bytes).expect("decode");
        assert_eq!(
            items,
            vec![
                json!({ "messageStart": { "role": "assistant" } }),
                json!({ "contentBlockDelta": { "contentBlockIndex": 0, "delta": { "text": "Hi" } } }),
                json!({ "contentBlockStop": { "contentBlockIndex": 0 } }),
                json!({ "messageStop": { "stopReason": "end_turn" } }),
            ]
        );
    }

    #[test]
    fn decodes_exception_message_under_member_key() {
        let bytes = encode_exception("validationException", &json!({ "message": "bad input" }));
        let items = decode_event_stream(&bytes).expect("decode");
        assert_eq!(
            items,
            vec![json!({ "validationException": { "message": "bad input" } })]
        );
    }

    #[test]
    fn empty_body_decodes_to_no_items() {
        assert_eq!(
            decode_event_stream(&[]).expect("decode"),
            Vec::<Value>::new()
        );
    }

    #[test]
    fn empty_payload_becomes_empty_object() {
        let bytes = encode_event("contentBlockStop", &json!({}));
        let items = decode_event_stream(&bytes).expect("decode");
        assert_eq!(items, vec![json!({ "contentBlockStop": {} })]);
    }

    #[test]
    fn truncated_body_is_a_clean_error() {
        let bytes = encode_event("messageStart", &json!({ "role": "assistant" }));
        // Drop the final byte: the declared total_length now overruns the buffer.
        let truncated = &bytes[..bytes.len() - 1];
        let err = decode_event_stream(truncated).expect_err("must error");
        assert!(
            matches!(err, EventStreamError::Truncated { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn short_prelude_is_a_clean_error() {
        // Fewer than 12 bytes cannot even hold a prelude.
        let err = decode_event_stream(&[0, 0, 0, 1]).expect_err("must error");
        assert!(
            matches!(err, EventStreamError::Truncated { needed, .. } if needed == PRELUDE_LEN),
            "got {err:?}"
        );
    }

    #[test]
    fn prelude_crc_mismatch_is_a_clean_error() {
        let mut bytes = encode_event("messageStart", &json!({ "role": "assistant" }));
        // Corrupt a prelude CRC byte (offset 8..12) without touching the framing.
        bytes[8] ^= 0xFF;
        let err = decode_event_stream(&bytes).expect_err("must error");
        assert!(
            matches!(err, EventStreamError::PreludeCrcMismatch { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn message_crc_mismatch_is_a_clean_error() {
        let mut bytes = encode_event("messageStart", &json!({ "role": "assistant" }));
        // Corrupt a payload byte: the prelude still checks out, but the trailing
        // message CRC no longer matches.
        let payload_byte = bytes.len() - MESSAGE_CRC_LEN - 1;
        bytes[payload_byte] ^= 0xFF;
        let err = decode_event_stream(&bytes).expect_err("must error");
        assert!(
            matches!(err, EventStreamError::MessageCrcMismatch { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn missing_event_type_header_is_a_clean_error() {
        // A framed message whose only header is :content-type — no member name.
        let mut headers = Vec::new();
        let name = ":content-type";
        headers.push(name.len() as u8);
        headers.extend_from_slice(name.as_bytes());
        headers.push(7); // string
        let value = "application/json";
        headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
        headers.extend_from_slice(value.as_bytes());
        let bytes = frame(&headers, br#"{"role":"assistant"}"#);

        let err = decode_event_stream(&bytes).expect_err("must error");
        assert_eq!(err, EventStreamError::MissingEventType);
    }

    #[test]
    fn invalid_payload_json_is_a_clean_error() {
        // Hand-frame an event whose payload is not valid JSON.
        let mut headers = Vec::new();
        for (name, value) in [(":message-type", "event"), (":event-type", "messageStart")] {
            headers.push(name.len() as u8);
            headers.extend_from_slice(name.as_bytes());
            headers.push(7);
            headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
            headers.extend_from_slice(value.as_bytes());
        }
        let bytes = frame(&headers, b"not json");
        let err = decode_event_stream(&bytes).expect_err("must error");
        assert!(
            matches!(err, EventStreamError::InvalidPayloadJson(_)),
            "got {err:?}"
        );
    }
}
