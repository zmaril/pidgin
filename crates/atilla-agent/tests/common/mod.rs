// straitjacket-allow-file:duplication — these `createUserMessage` /
// `createAssistantMessage` helpers faithfully mirror pi's
// `test/harness/session-test-utils.ts`. The AgentMessage JSON shape (all-zero
// usage/cost block) structurally matches the parallel ported builders in
// `compaction.rs` and `agent_harness` (both already marked), which keep their
// own local copies by design; the clone is intentional parallel scaffolding.
//! Shared test helpers, mirroring `test/harness/session-test-utils.ts`.
//!
//! Included by several test binaries; each uses a different subset, so unused
//! helpers are expected per binary.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use atilla_agent::harness::types::SessionError;
use serde_json::{json, Value};

/// Extract the error from a result whose `Ok` type is not `Debug` (so
/// `unwrap_err` cannot be used directly).
pub fn expect_err<T>(result: Result<T, SessionError>) -> SessionError {
    match result {
        Ok(_) => panic!("expected an error"),
        Err(error) => error,
    }
}

/// Build a user `AgentMessage`, as `createUserMessage` does.
pub fn create_user_message(text: &str) -> Value {
    json!({
        "role": "user",
        "content": [{"type": "text", "text": text}],
        "timestamp": 0,
    })
}

/// Build an assistant `AgentMessage` with the same usage/cost shape as
/// `createAssistantMessage`.
pub fn create_assistant_message(text: &str) -> Value {
    json!({
        "role": "assistant",
        "content": [{"type": "text", "text": text}],
        "api": "anthropic-messages",
        "provider": "anthropic",
        "model": "claude-sonnet-4-5",
        "usage": {
            "input": 0,
            "output": 0,
            "cacheRead": 0,
            "cacheWrite": 0,
            "totalTokens": 0,
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}
        },
        "stopReason": "stop",
        "timestamp": 0,
    })
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temporary directory removed on drop.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("atilla-agent-session-{pid}-{n}"));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn child(&self, name: &str) -> String {
        self.path.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
