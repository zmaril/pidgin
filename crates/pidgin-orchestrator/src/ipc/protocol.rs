//! Newline-framed JSON wire protocol mirroring
//! `packages/orchestrator/src/ipc/protocol.ts`.
//!
//! The orchestrator and its clients speak a line-delimited JSON protocol over a
//! Unix socket: one JSON object per line, terminated by `\n`. This module
//! defines the request and response shapes and the framing helpers
//! (`encode_message`, `parse_request_line`, `parse_response_line`).
//!
//! # Relay seam
//!
//! pi's protocol references five coding-agent RPC payload types by name —
//! `RpcCommand`, `RpcResponse`, `AgentSessionEvent`, `RpcExtensionUIRequest`,
//! and `RpcExtensionUIResponse`. The orchestrator never inspects these; it only
//! relays them verbatim between a client and a spawned RPC child. Per the
//! coordinator-approved seam decision they are modelled here as
//! [`serde_json::Value`] (see the type aliases below), preserving the exact wire
//! shape without pulling the coding-agent type graph into the orchestrator.
//!
//! # serde vs. TypeScript
//!
//! pi expresses requests and responses as interfaces discriminated by a `type`
//! string, unioned into `OrchestratorRequest` / `OrchestratorResponse`. This
//! port mirrors that with serde internally-tagged enums (`tag = "type"`) whose
//! variants wrap the individual request/response structs, so the on-the-wire
//! JSON is byte-for-byte the same flat object pi emits. Field names match pi's
//! camelCase exactly, and absent optional fields are omitted from serialization
//! (mirroring how `JSON.stringify` drops `undefined` members).

// straitjacket-allow-file:duplication — the request/response struct family and
// their per-variant serde attributes are a faithful parallel mirror of pi's
// discriminated-union interfaces in protocol.ts, not extractable shared logic.

use serde::{Deserialize, Serialize};

use crate::types::InstanceStatus;

/// Relayed coding-agent command payload (`RpcCommand` in pi). Opaque to the
/// orchestrator, which only relays it — modelled as an arbitrary JSON value.
pub type RpcCommand = serde_json::Value;

/// Relayed coding-agent response payload (`RpcResponse` in pi). Opaque relay
/// value; see the module-level relay-seam note.
pub type RpcResponse = serde_json::Value;

/// Relayed streaming session event (`AgentSessionEvent` in pi). Opaque relay
/// value; see the module-level relay-seam note.
pub type AgentSessionEvent = serde_json::Value;

/// Relayed extension-UI request (`RpcExtensionUIRequest` in pi). Opaque relay
/// value; see the module-level relay-seam note.
pub type RpcExtensionUIRequest = serde_json::Value;

/// Relayed extension-UI response (`RpcExtensionUIResponse` in pi). Opaque relay
/// value; see the module-level relay-seam note.
pub type RpcExtensionUIResponse = serde_json::Value;

/// Spawn a new instance. Mirrors pi's `SpawnRequest` (`type: "spawn"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// List all instances. Mirrors pi's `ListRequest` (`type: "list"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListRequest {}

/// Stop an instance. Mirrors pi's `StopRequest` (`type: "stop"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopRequest {
    pub instance_id: String,
}

/// Query an instance's status. Mirrors pi's `StatusRequest` (`type: "status"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusRequest {
    pub instance_id: String,
}

/// Relay a single RPC command to an instance. Mirrors pi's `RpcRequest`
/// (`type: "rpc"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcRequest {
    pub instance_id: String,
    pub command: RpcCommand,
}

/// Open a bidirectional RPC stream to an instance. Mirrors pi's
/// `RpcStreamRequest` (`type: "rpc_stream"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcStreamRequest {
    pub instance_id: String,
}

/// A request from a client to the orchestrator.
///
/// Mirrors pi's `OrchestratorRequest = RequestMap[keyof RequestMap]` union,
/// discriminated by the `type` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OrchestratorRequest {
    Spawn(SpawnRequest),
    List(ListRequest),
    Stop(StopRequest),
    Status(StatusRequest),
    Rpc(RpcRequest),
    RpcStream(RpcStreamRequest),
}

/// Summary view of an instance returned in responses. Mirrors pi's
/// `InstanceSummary`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceSummary {
    pub id: String,
    pub status: InstanceStatus,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius_pi_id: Option<String>,
}

/// Fields shared by every response. Mirrors pi's `ResponseBase`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseBase {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Result of a spawn. Mirrors pi's `SpawnResponse` (`type: "spawn_result"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnResponse {
    #[serde(flatten)]
    pub base: ResponseBase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<InstanceSummary>,
}

/// Result of a list. Mirrors pi's `ListResponse` (`type: "list_result"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListResponse {
    #[serde(flatten)]
    pub base: ResponseBase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instances: Option<Vec<InstanceSummary>>,
}

/// Result of a stop. Mirrors pi's `StopResponse` (`type: "stop_result"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopResponse {
    #[serde(flatten)]
    pub base: ResponseBase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
}

/// Result of a status query. Mirrors pi's `StatusResponse`
/// (`type: "status_result"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusResponse {
    #[serde(flatten)]
    pub base: ResponseBase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<InstanceSummary>,
}

/// Result of relaying a single RPC command. Mirrors pi's `RpcBridgeResponse`
/// (`type: "rpc_result"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcBridgeResponse {
    #[serde(flatten)]
    pub base: ResponseBase,
    pub response: RpcResponse,
}

/// Acknowledgement that an RPC stream is ready. Mirrors pi's `RpcReadyResponse`
/// (`type: "rpc_ready"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcReadyResponse {
    #[serde(flatten)]
    pub base: ResponseBase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<InstanceSummary>,
}

/// A protocol-level error. Mirrors pi's `ErrorResponse` (`type: "error"`,
/// `ok: false`, required `error`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub ok: bool,
    pub error: String,
}

/// A response from the orchestrator to a client.
///
/// Mirrors pi's `OrchestratorResponse = ResponseMap[keyof ResponseMap] |
/// ErrorResponse` union, discriminated by the `type` field. The response `type`
/// discriminants (`spawn_result`, `list_result`, …) are spelled out per variant
/// to match pi exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OrchestratorResponse {
    #[serde(rename = "spawn_result")]
    Spawn(SpawnResponse),
    #[serde(rename = "list_result")]
    List(ListResponse),
    #[serde(rename = "stop_result")]
    Stop(StopResponse),
    #[serde(rename = "status_result")]
    Status(StatusResponse),
    #[serde(rename = "rpc_result")]
    Rpc(RpcBridgeResponse),
    #[serde(rename = "rpc_ready")]
    RpcReady(RpcReadyResponse),
    #[serde(rename = "error")]
    Error(ErrorResponse),
}

/// A message sent by an RPC-stream client. Mirrors pi's
/// `RpcClientMessage = RpcCommand | RpcExtensionUIResponse`.
///
/// Both arms of pi's union are opaque relay payloads (see the relay-seam note),
/// so this is an arbitrary JSON value on the wire.
pub type RpcClientMessage = serde_json::Value;

/// A message sent to an RPC-stream client. Mirrors pi's
/// `RpcServerMessage = RpcReadyResponse | RpcResponse | AgentSessionEvent |
/// RpcExtensionUIRequest | ErrorResponse`.
///
/// Three of its arms are opaque relay payloads, so on the bridge these frames
/// are relayed as arbitrary JSON values.
pub type RpcServerMessage = serde_json::Value;

/// Any message that may cross the wire. Mirrors pi's
/// `ProtocolMessage = OrchestratorRequest | OrchestratorResponse |
/// RpcClientMessage | RpcServerMessage`.
///
/// [`encode_message`] frames any serializable protocol value, so this alias
/// documents the conceptual union rather than constraining the framing helper.
pub type ProtocolMessage = serde_json::Value;

/// Frame a message as a single newline-terminated JSON line.
///
/// Mirrors pi's `encodeMessage`: `` `${JSON.stringify(message)}\n` ``. Generic
/// over any serializable protocol value (requests, responses, or relayed
/// payloads), matching how pi applies `encodeMessage` across the whole
/// `ProtocolMessage` union. Serialization of these types is infallible in
/// practice, mirroring pi's non-throwing contract.
pub fn encode_message<T: Serialize + ?Sized>(message: &T) -> String {
    let json = serde_json::to_string(message).expect("serialize protocol message");
    format!("{json}\n")
}

/// Parse one line into an [`OrchestratorRequest`].
///
/// Mirrors pi's `parseRequestLine` (`JSON.parse(line) as OrchestratorRequest`).
/// Malformed or non-conforming JSON yields an error, mirroring how `JSON.parse`
/// throws.
pub fn parse_request_line(line: &str) -> serde_json::Result<OrchestratorRequest> {
    serde_json::from_str(line)
}

/// Parse one line into an [`OrchestratorResponse`].
///
/// Mirrors pi's `parseResponseLine` (`JSON.parse(line) as OrchestratorResponse`).
/// Malformed or non-conforming JSON yields an error, mirroring how `JSON.parse`
/// throws.
pub fn parse_response_line(line: &str) -> serde_json::Result<OrchestratorResponse> {
    serde_json::from_str(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- encode_message framing -------------------------------------------

    #[test]
    fn encode_message_appends_single_newline() {
        let request = OrchestratorRequest::List(ListRequest {});
        let framed = encode_message(&request);
        assert!(framed.ends_with('\n'));
        assert_eq!(framed.matches('\n').count(), 1);
        assert_eq!(framed, "{\"type\":\"list\"}\n");
    }

    #[test]
    fn encode_message_round_trips_request() {
        let request = OrchestratorRequest::Spawn(SpawnRequest {
            cwd: "/home/user/project".to_string(),
            label: Some("primary".to_string()),
            provider: None,
            model: Some("claude".to_string()),
        });
        let framed = encode_message(&request);
        let parsed = parse_request_line(framed.trim_end_matches('\n')).unwrap();
        assert_eq!(parsed, request);
    }

    #[test]
    fn encode_message_round_trips_response() {
        let response = OrchestratorResponse::Stop(StopResponse {
            base: ResponseBase {
                ok: true,
                error: None,
            },
            instance_id: Some("i1".to_string()),
        });
        let framed = encode_message(&response);
        let parsed = parse_response_line(framed.trim_end_matches('\n')).unwrap();
        assert_eq!(parsed, response);
    }

    #[test]
    fn encode_message_frames_a_relayed_value() {
        // A relayed RPC payload is an opaque JSON value (the relay seam).
        let payload: ProtocolMessage = json!({ "id": "1", "method": "ping" });
        assert_eq!(
            encode_message(&payload),
            "{\"id\":\"1\",\"method\":\"ping\"}\n"
        );
    }

    // ---- request wire shapes ----------------------------------------------

    #[test]
    fn spawn_request_matches_pi_json_and_omits_absent_optionals() {
        let request = OrchestratorRequest::Spawn(SpawnRequest {
            cwd: "/tmp".to_string(),
            label: None,
            provider: None,
            model: None,
        });
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({ "type": "spawn", "cwd": "/tmp" })
        );
    }

    #[test]
    fn spawn_request_serializes_all_fields() {
        let request = OrchestratorRequest::Spawn(SpawnRequest {
            cwd: "/tmp".to_string(),
            label: Some("l".to_string()),
            provider: Some("anthropic".to_string()),
            model: Some("claude".to_string()),
        });
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({
                "type": "spawn",
                "cwd": "/tmp",
                "label": "l",
                "provider": "anthropic",
                "model": "claude",
            })
        );
    }

    #[test]
    fn list_request_is_just_the_type_tag() {
        let request = OrchestratorRequest::List(ListRequest {});
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({ "type": "list" })
        );
    }

    #[test]
    fn stop_and_status_requests_use_camel_case_instance_id() {
        let stop = OrchestratorRequest::Stop(StopRequest {
            instance_id: "i1".to_string(),
        });
        assert_eq!(
            serde_json::to_value(&stop).unwrap(),
            json!({ "type": "stop", "instanceId": "i1" })
        );
        let status = OrchestratorRequest::Status(StatusRequest {
            instance_id: "i2".to_string(),
        });
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            json!({ "type": "status", "instanceId": "i2" })
        );
    }

    #[test]
    fn rpc_request_carries_relayed_command_verbatim() {
        let request = OrchestratorRequest::Rpc(RpcRequest {
            instance_id: "i1".to_string(),
            command: json!({ "id": "7", "method": "prompt", "params": { "text": "hi" } }),
        });
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(
            value,
            json!({
                "type": "rpc",
                "instanceId": "i1",
                "command": { "id": "7", "method": "prompt", "params": { "text": "hi" } },
            })
        );
        assert_eq!(parse_request_line(&value.to_string()).unwrap(), request);
    }

    #[test]
    fn rpc_stream_request_type_tag_is_snake_case() {
        let request = OrchestratorRequest::RpcStream(RpcStreamRequest {
            instance_id: "i1".to_string(),
        });
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({ "type": "rpc_stream", "instanceId": "i1" })
        );
    }

    // ---- response wire shapes ---------------------------------------------

    #[test]
    fn spawn_response_flattens_base_and_matches_pi_json() {
        let response = OrchestratorResponse::Spawn(SpawnResponse {
            base: ResponseBase {
                ok: true,
                error: None,
            },
            instance: Some(InstanceSummary {
                id: "i1".to_string(),
                status: InstanceStatus::Online,
                cwd: "/tmp".to_string(),
                label: None,
                session_id: Some("s1".to_string()),
                session_file: None,
                radius_pi_id: None,
            }),
        });
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            json!({
                "type": "spawn_result",
                "ok": true,
                "instance": {
                    "id": "i1",
                    "status": "online",
                    "cwd": "/tmp",
                    "sessionId": "s1",
                },
            })
        );
    }

    #[test]
    fn list_response_round_trips() {
        let response = OrchestratorResponse::List(ListResponse {
            base: ResponseBase {
                ok: true,
                error: None,
            },
            instances: Some(vec![InstanceSummary {
                id: "i1".to_string(),
                status: InstanceStatus::Starting,
                cwd: "/a".to_string(),
                label: Some("primary".to_string()),
                session_id: None,
                session_file: None,
                radius_pi_id: Some("pi-1".to_string()),
            }]),
        });
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(
            value,
            json!({
                "type": "list_result",
                "ok": true,
                "instances": [{
                    "id": "i1",
                    "status": "starting",
                    "cwd": "/a",
                    "label": "primary",
                    "radiusPiId": "pi-1",
                }],
            })
        );
        assert_eq!(parse_response_line(&value.to_string()).unwrap(), response);
    }

    #[test]
    fn rpc_bridge_response_relays_response_value() {
        let response = OrchestratorResponse::Rpc(RpcBridgeResponse {
            base: ResponseBase {
                ok: true,
                error: None,
            },
            response: json!({ "id": "7", "result": { "ok": true } }),
        });
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            json!({
                "type": "rpc_result",
                "ok": true,
                "response": { "id": "7", "result": { "ok": true } },
            })
        );
    }

    #[test]
    fn error_response_carries_ok_false_and_error() {
        let response = OrchestratorResponse::Error(ErrorResponse {
            ok: false,
            error: "unknown instance".to_string(),
        });
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(
            value,
            json!({
                "type": "error",
                "ok": false,
                "error": "unknown instance",
            })
        );
        assert_eq!(parse_response_line(&value.to_string()).unwrap(), response);
    }

    #[test]
    fn response_base_omits_absent_error() {
        let base = ResponseBase {
            ok: true,
            error: None,
        };
        assert_eq!(serde_json::to_value(&base).unwrap(), json!({ "ok": true }));
    }

    #[test]
    fn response_with_error_message_serializes_error_field() {
        let response = OrchestratorResponse::Status(StatusResponse {
            base: ResponseBase {
                ok: false,
                error: Some("no such instance".to_string()),
            },
            instance: None,
        });
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            json!({
                "type": "status_result",
                "ok": false,
                "error": "no such instance",
            })
        );
    }

    // ---- parse* on malformed input ----------------------------------------

    #[test]
    fn parse_request_line_rejects_malformed_json() {
        assert!(parse_request_line("{not json").is_err());
        assert!(parse_request_line("").is_err());
    }

    #[test]
    fn parse_request_line_rejects_unknown_type_tag() {
        // A well-formed object whose discriminant is not a known request type.
        assert!(parse_request_line("{\"type\":\"teleport\"}").is_err());
    }

    #[test]
    fn parse_request_line_rejects_missing_required_field() {
        // `spawn` requires `cwd`.
        assert!(parse_request_line("{\"type\":\"spawn\"}").is_err());
    }

    #[test]
    fn parse_response_line_rejects_malformed_json() {
        assert!(parse_response_line("garbage").is_err());
        assert!(parse_response_line("{\"type\":").is_err());
    }

    #[test]
    fn parse_response_line_rejects_unknown_type_tag() {
        assert!(parse_response_line("{\"type\":\"mystery_result\",\"ok\":true}").is_err());
    }
}
