//! Core record types mirroring `packages/orchestrator/src/types.ts`.
//!
//! These are pure data definitions with no behaviour. The serde field names
//! match pi's on-disk JSON exactly (camelCase), and optional fields are omitted
//! from serialization when absent — mirroring how `JSON.stringify` drops
//! `undefined` interface members.

use serde::{Deserialize, Serialize};

/// Lifecycle state of an orchestrated instance.
///
/// Mirrors pi's `InstanceStatus` string union
/// (`"starting" | "online" | "stopping" | "stopped" | "error"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceStatus {
    Starting,
    Online,
    Stopping,
    Stopped,
    Error,
}

/// Persistent record identifying this machine. Mirrors pi's `MachineRecord`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineRecord {
    pub id: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Registration parameters returned by radius. Mirrors pi's `RadiusRegistration`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadiusRegistration {
    pub heartbeat_interval_ms: i64,
    pub expires_in_ms: i64,
}

/// Persistent record describing a single orchestrated instance.
///
/// Mirrors pi's `InstanceRecord`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceRecord {
    pub id: String,
    pub status: InstanceStatus,
    pub cwd: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius_pi_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_status_serializes_to_lowercase_literals() {
        assert_eq!(
            serde_json::to_string(&InstanceStatus::Starting).unwrap(),
            "\"starting\""
        );
        assert_eq!(
            serde_json::to_string(&InstanceStatus::Online).unwrap(),
            "\"online\""
        );
        assert_eq!(
            serde_json::to_string(&InstanceStatus::Stopping).unwrap(),
            "\"stopping\""
        );
        assert_eq!(
            serde_json::to_string(&InstanceStatus::Stopped).unwrap(),
            "\"stopped\""
        );
        assert_eq!(
            serde_json::to_string(&InstanceStatus::Error).unwrap(),
            "\"error\""
        );
    }

    #[test]
    fn machine_record_omits_absent_optionals() {
        let machine = MachineRecord {
            id: "m1".to_string(),
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            last_seen_at: None,
            label: None,
        };
        let json = serde_json::to_value(&machine).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "id": "m1",
                "createdAt": "2026-01-01T00:00:00.000Z",
            })
        );
    }

    #[test]
    fn machine_record_round_trips_with_optionals() {
        let json = serde_json::json!({
            "id": "m1",
            "createdAt": "2026-01-01T00:00:00.000Z",
            "lastSeenAt": "2026-01-02T00:00:00.000Z",
            "label": "workstation",
        });
        let machine: MachineRecord = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(
            machine.last_seen_at.as_deref(),
            Some("2026-01-02T00:00:00.000Z")
        );
        assert_eq!(machine.label.as_deref(), Some("workstation"));
        assert_eq!(serde_json::to_value(&machine).unwrap(), json);
    }

    #[test]
    fn radius_registration_uses_camel_case() {
        let reg = RadiusRegistration {
            heartbeat_interval_ms: 15_000,
            expires_in_ms: 60_000,
        };
        assert_eq!(
            serde_json::to_value(reg).unwrap(),
            serde_json::json!({
                "heartbeatIntervalMs": 15_000,
                "expiresInMs": 60_000,
            })
        );
    }

    #[test]
    fn instance_record_matches_pi_json_shape() {
        let json = serde_json::json!({
            "id": "i1",
            "status": "online",
            "cwd": "/home/user/project",
            "createdAt": "2026-01-01T00:00:00.000Z",
            "lastSeenAt": "2026-01-01T00:05:00.000Z",
            "label": "primary",
            "sessionId": "s1",
            "sessionFile": "/home/user/.pi/sessions/s1.jsonl",
            "radiusPiId": "pi-123",
        });
        let record: InstanceRecord = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(record.status, InstanceStatus::Online);
        assert_eq!(record.cwd, "/home/user/project");
        assert_eq!(record.radius_pi_id.as_deref(), Some("pi-123"));
        assert_eq!(serde_json::to_value(&record).unwrap(), json);
    }

    #[test]
    fn instance_record_omits_absent_optionals() {
        let record = InstanceRecord {
            id: "i1".to_string(),
            status: InstanceStatus::Starting,
            cwd: "/tmp".to_string(),
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            last_seen_at: None,
            label: None,
            session_id: None,
            session_file: None,
            radius_pi_id: None,
        };
        assert_eq!(
            serde_json::to_value(&record).unwrap(),
            serde_json::json!({
                "id": "i1",
                "status": "starting",
                "cwd": "/tmp",
                "createdAt": "2026-01-01T00:00:00.000Z",
            })
        );
    }
}
