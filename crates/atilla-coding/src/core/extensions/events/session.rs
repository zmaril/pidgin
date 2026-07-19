//! Session-lifecycle extension events and their result types.
//!
//! Faithful port of the session-group event interfaces from
//! `packages/coding-agent/src/core/extensions/types.ts`: `ProjectTrustEvent`,
//! `ResourcesDiscoverEvent`, and the nine `Session*Event` interfaces, plus their
//! handler-result shapes. Payload types that belong to unported subsystems are
//! modeled as opaque [`Value`] aliases from [`super::common`].
//!
//! Non-wire runtime handles carried by some upstream events — the `signal:
//! AbortSignal` on `SessionBeforeCompactEvent` and `SessionBeforeTreeEvent` — are
//! omitted: an `AbortSignal` is a cooperative cancellation handle delivered
//! host-side, not JSON wire data, so it never round-trips through serde. See the
//! `HookContext` cancellation note in `hook.rs`.
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/types.ts`.

// straitjacket-allow-file:duplication

use serde::{Deserialize, Serialize};

use super::common::{
    BranchSummaryEntry, CompactionEntry, CompactionPreparation, CompactionResult, SessionEntry,
};

// ---------------------------------------------------------------------------
// project_trust (`types.ts:507`)
// ---------------------------------------------------------------------------

/// Fired to resolve whether the current project directory is trusted (pi's
/// `ProjectTrustEvent`, `types.ts:507`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectTrustEvent {
    /// The project directory whose trust is being resolved.
    pub cwd: String,
}

/// The trust decision an extension may return (pi's `ProjectTrustEventDecision`,
/// `types.ts:512`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectTrustEventDecision {
    /// The project is trusted.
    Yes,
    /// The project is not trusted.
    No,
    /// No decision was reached; defer to the next handler or default.
    Undecided,
}

/// Result of a `project_trust` handler (pi's `ProjectTrustEventResult`,
/// `types.ts:514`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectTrustEventResult {
    /// The trust decision.
    pub trusted: ProjectTrustEventDecision,
    /// Whether to persist the decision across sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remember: Option<bool>,
}

// ---------------------------------------------------------------------------
// resources_discover (`types.ts:532`)
// ---------------------------------------------------------------------------

/// Why a `resources_discover` event fired (pi inline union, `types.ts:535`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourcesDiscoverReason {
    /// Discovery at process startup.
    Startup,
    /// Discovery on a resource reload.
    Reload,
}

/// Fired after `session_start` so extensions can contribute resource paths (pi's
/// `ResourcesDiscoverEvent`, `types.ts:532`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourcesDiscoverEvent {
    /// The project directory being scanned.
    pub cwd: String,
    /// Whether this is a startup or reload discovery.
    pub reason: ResourcesDiscoverReason,
}

/// A single discovered resource path paired with the extension that contributed
/// it (pi's runner aggregate element `{ path, extensionPath }`,
/// `runner.ts:1129`).
///
/// `AgentSession`'s `buildExtensionResourcePaths` reads `extension_path` twice
/// per entry — for the source label (`getExtensionSourceLabel`) and for the
/// resource base dir (`dirname`) — so the contributing extension travels
/// alongside each path rather than being discarded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredResourcePath {
    /// The resource directory an extension contributed.
    pub path: String,
    /// The `path` of the extension that contributed it.
    pub extension_path: String,
}

/// Aggregate result of the runner's `emitResourcesDiscover` (pi's return shape,
/// `runner.ts:1128`): each kind is a flat list of `{ path, extensionPath }`
/// pairs collected across every `resources_discover` handler.
///
/// This is the *widened* form of pi's paths-only handler-result interface
/// (`ResourcesDiscoverResult`, `types.ts:539`): the runner keeps each
/// contributing extension's `path` alongside every discovered path so
/// `AgentSession` can attribute and resolve it. The per-handler paths-only shape
/// stays private to the deno impl, which folds it into these pairs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesDiscoverResult {
    /// Additional skill directories to load, each with its contributing
    /// extension.
    #[serde(default)]
    pub skill_paths: Vec<DiscoveredResourcePath>,
    /// Additional prompt directories to load, each with its contributing
    /// extension.
    #[serde(default)]
    pub prompt_paths: Vec<DiscoveredResourcePath>,
    /// Additional theme directories to load, each with its contributing
    /// extension.
    #[serde(default)]
    pub theme_paths: Vec<DiscoveredResourcePath>,
}

// ---------------------------------------------------------------------------
// session_start / session_info_changed (`types.ts:550`, `:559`)
// ---------------------------------------------------------------------------

/// Why a session started (pi inline union, `types.ts:553`). Also reused by
/// [`SessionShutdownEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStartReason {
    /// Process startup.
    Startup,
    /// A session reload.
    Reload,
    /// A brand-new session.
    New,
    /// Resuming a persisted session.
    Resume,
    /// Forking from an existing session.
    Fork,
}

/// Fired when a session is started, loaded, or reloaded (pi's `SessionStartEvent`,
/// `types.ts:550`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartEvent {
    /// Why the session start happened.
    pub reason: SessionStartReason,
    /// Previously active session file (present for `new`, `resume`, and `fork`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_session_file: Option<String>,
}

/// Fired when the current session metadata changes (pi's
/// `SessionInfoChangedEvent`, `types.ts:559`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfoChangedEvent {
    /// Current normalized session name; `None` when the name is cleared.
    ///
    /// pi types this `name: string | undefined` as a *required* property whose
    /// value may be `undefined`, so the field always serializes (as `null` when
    /// cleared) rather than being omitted.
    pub name: Option<String>,
}

// ---------------------------------------------------------------------------
// session_before_switch / session_before_fork (`types.ts:566`, `:573`)
// ---------------------------------------------------------------------------

/// Why a session switch was requested (pi inline union, `types.ts:568`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionBeforeSwitchReason {
    /// Switching to a new session.
    New,
    /// Switching to a resumed session.
    Resume,
}

/// Fired before switching to another session; can be cancelled (pi's
/// `SessionBeforeSwitchEvent`, `types.ts:566`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeSwitchEvent {
    /// Whether the target is a new or resumed session.
    pub reason: SessionBeforeSwitchReason,
    /// The target session file, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_session_file: Option<String>,
}

/// Result of a `session_before_switch` handler (pi's `SessionBeforeSwitchResult`,
/// `types.ts:1088`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionBeforeSwitchResult {
    /// When `Some(true)`, cancel the switch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel: Option<bool>,
}

/// Where a fork is anchored relative to an entry (pi inline union,
/// `types.ts:576`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForkPosition {
    /// Fork before the anchor entry.
    Before,
    /// Fork at the anchor entry.
    At,
}

/// Fired before forking a session; can be cancelled (pi's
/// `SessionBeforeForkEvent`, `types.ts:573`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeForkEvent {
    /// The entry the fork is anchored on.
    pub entry_id: String,
    /// Whether the fork is placed before or at the anchor entry.
    pub position: ForkPosition,
}

/// Result of a `session_before_fork` handler (pi's `SessionBeforeForkResult`,
/// `types.ts:1092`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeForkResult {
    /// When `Some(true)`, cancel the fork.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel: Option<bool>,
    /// When `Some(true)`, skip restoring the forked conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_conversation_restore: Option<bool>,
}

// ---------------------------------------------------------------------------
// session_before_compact / session_compact (`types.ts:580`, `:593`)
// ---------------------------------------------------------------------------

/// What triggered a compaction (pi inline union, `types.ts:586`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactionReason {
    /// A manual `/compact` invocation.
    Manual,
    /// The context-size threshold was crossed.
    Threshold,
    /// Context-overflow recovery.
    Overflow,
}

/// Fired before context compaction; can be cancelled or customized (pi's
/// `SessionBeforeCompactEvent`, `types.ts:580`).
///
/// pi's `signal: AbortSignal` is omitted; see the module-level note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeCompactEvent {
    /// Preparation data gathered for the compaction.
    pub preparation: CompactionPreparation,
    /// The branch entries under consideration.
    pub branch_entries: Vec<SessionEntry>,
    /// Optional custom compaction instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    /// What triggered the compaction.
    pub reason: CompactionReason,
    /// True when the aborted turn will be retried after this compaction.
    pub will_retry: bool,
}

/// Result of a `session_before_compact` handler (pi's
/// `SessionBeforeCompactResult`, `types.ts:1097`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionBeforeCompactResult {
    /// When `Some(true)`, cancel the compaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel: Option<bool>,
    /// A replacement compaction produced by the extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionResult>,
}

/// Fired after context compaction (pi's `SessionCompactEvent`, `types.ts:593`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCompactEvent {
    /// The compaction-boundary entry that was written.
    pub compaction_entry: CompactionEntry,
    /// Whether the compaction came from an extension.
    pub from_extension: bool,
    /// What triggered the compaction.
    pub reason: CompactionReason,
    /// True when the aborted turn will be retried after this compaction.
    pub will_retry: bool,
}

// ---------------------------------------------------------------------------
// session_shutdown (`types.ts:604`)
// ---------------------------------------------------------------------------

/// Why an extension runtime is being shut down (pi inline union,
/// `types.ts:606`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionShutdownReason {
    /// The process is quitting.
    Quit,
    /// The runtime is reloading.
    Reload,
    /// Replaced by a new session.
    New,
    /// Replaced by a resumed session.
    Resume,
    /// Replaced by a forked session.
    Fork,
}

/// Fired before an extension runtime is torn down (pi's `SessionShutdownEvent`,
/// `types.ts:604`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionShutdownEvent {
    /// Why the shutdown happened.
    pub reason: SessionShutdownReason,
    /// Destination session file when shutting down due to replacement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_session_file: Option<String>,
}

// ---------------------------------------------------------------------------
// session_before_tree / session_tree (`types.ts:627`, `:634`)
// ---------------------------------------------------------------------------

/// Preparation data for a session-tree navigation (pi's `TreePreparation`,
/// `types.ts:612`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreePreparation {
    /// The navigation target entry id.
    pub target_id: String,
    /// The leaf id being left, if any.
    pub old_leaf_id: Option<String>,
    /// The common-ancestor entry id, if any.
    pub common_ancestor_id: Option<String>,
    /// Entries to summarize during navigation.
    pub entries_to_summarize: Vec<SessionEntry>,
    /// Whether the user asked for a summary.
    pub user_wants_summary: bool,
    /// Custom instructions for summarization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    /// If true, `custom_instructions` replaces (rather than appends to) the
    /// default prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_instructions: Option<bool>,
    /// Label to attach to the branch summary entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Fired before navigating in the session tree; can be cancelled (pi's
/// `SessionBeforeTreeEvent`, `types.ts:627`).
///
/// pi's `signal: AbortSignal` is omitted; see the module-level note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionBeforeTreeEvent {
    /// The navigation preparation data.
    pub preparation: TreePreparation,
}

/// A summary an extension can supply from a tree handler (pi inline shape,
/// `types.ts:1104`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionBeforeTreeSummary {
    /// The summary text.
    pub summary: String,
    /// Optional structured details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Result of a `session_before_tree` handler (pi's `SessionBeforeTreeResult`,
/// `types.ts:1102`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeTreeResult {
    /// When `Some(true)`, cancel the navigation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel: Option<bool>,
    /// An extension-supplied summary override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<SessionBeforeTreeSummary>,
    /// Override custom instructions for summarization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    /// Override whether custom instructions replace the default prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_instructions: Option<bool>,
    /// Override the label for the branch summary entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Fired after navigating in the session tree (pi's `SessionTreeEvent`,
/// `types.ts:634`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTreeEvent {
    /// The new leaf id, if any.
    pub new_leaf_id: Option<String>,
    /// The previous leaf id, if any.
    pub old_leaf_id: Option<String>,
    /// The branch summary entry that was written, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_entry: Option<BranchSummaryEntry>,
    /// Whether the navigation came from an extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_extension: Option<bool>,
}
