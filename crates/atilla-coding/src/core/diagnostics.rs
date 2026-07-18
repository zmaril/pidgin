//! Resource-resolution diagnostics.
//!
//! Ported from pi's `core/diagnostics.ts` — a pair of plain data types that
//! report problems (and winner/loser collisions) encountered while resolving
//! extensions, skills, prompts, and themes from multiple sources.

use serde::{Deserialize, Serialize};

/// The kind of resource a collision or diagnostic concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceType {
    /// An extension.
    Extension,
    /// A skill.
    Skill,
    /// A prompt.
    Prompt,
    /// A theme.
    Theme,
}

/// Two sources supplied the same named resource; the winner shadowed the loser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceCollision {
    /// The kind of resource that collided.
    pub resource_type: ResourceType,
    /// The colliding name (skill/command/tool/flag/prompt/theme name).
    pub name: String,
    /// Path of the resource that won.
    pub winner_path: String,
    /// Path of the resource that was shadowed.
    pub loser_path: String,
    /// Source of the winner (e.g. `"npm:foo"`, `"git:..."`, `"local"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub winner_source: Option<String>,
    /// Source of the loser.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loser_source: Option<String>,
}

/// The severity/category of a [`ResourceDiagnostic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticType {
    /// A non-fatal warning.
    Warning,
    /// A fatal error.
    Error,
    /// A shadowing collision (details in [`ResourceDiagnostic::collision`]).
    Collision,
}

/// A single diagnostic emitted during resource resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceDiagnostic {
    /// Warning / error / collision.
    #[serde(rename = "type")]
    pub diagnostic_type: DiagnosticType,
    /// Human-readable message.
    pub message: String,
    /// Optional path the diagnostic concerns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Collision details, present when `diagnostic_type` is `Collision`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collision: Option<ResourceCollision>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_collision() -> ResourceCollision {
        ResourceCollision {
            resource_type: ResourceType::Skill,
            name: "build".to_string(),
            winner_path: "/a/build".to_string(),
            loser_path: "/b/build".to_string(),
            winner_source: Some("npm:foo".to_string()),
            loser_source: Some("local".to_string()),
        }
    }

    #[test]
    fn diagnostic_type_serializes_under_type_key() {
        let diag = ResourceDiagnostic {
            diagnostic_type: DiagnosticType::Warning,
            message: "heads up".to_string(),
            path: None,
            collision: None,
        };
        let json = serde_json::to_string(&diag).unwrap();
        assert!(json.contains("\"type\":\"warning\""), "got: {json}");
        assert!(!json.contains("\"path\""), "got: {json}");
        assert!(!json.contains("\"collision\""), "got: {json}");
    }

    #[test]
    fn collision_round_trips_through_json() {
        let diag = ResourceDiagnostic {
            diagnostic_type: DiagnosticType::Collision,
            message: "shadowed".to_string(),
            path: Some("/a/build".to_string()),
            collision: Some(base_collision()),
        };
        let json = serde_json::to_string(&diag).unwrap();
        let back: ResourceDiagnostic = serde_json::from_str(&json).unwrap();
        assert_eq!(back, diag);
    }

    #[test]
    fn resource_type_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&ResourceType::Extension).unwrap(),
            "\"extension\""
        );
        assert_eq!(
            serde_json::to_string(&ResourceType::Theme).unwrap(),
            "\"theme\""
        );
    }

    #[test]
    fn collision_omits_none_sources() {
        let json = serde_json::to_string(&ResourceCollision {
            winner_source: None,
            loser_source: None,
            ..base_collision()
        })
        .unwrap();
        assert!(!json.contains("winner_source"), "got: {json}");
        assert!(!json.contains("loser_source"), "got: {json}");
    }
}
