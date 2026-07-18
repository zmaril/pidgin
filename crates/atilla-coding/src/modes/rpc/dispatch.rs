//! Command dispatch, ported from pi's `runRpcMode` `handleCommand` switch.
//!
//! Each command is mapped to a response envelope. The "implementable-now"
//! subset is served for real against [`RpcSession`] (storage session tree, the
//! bash executor, the export_html renderer, in-memory settings). Commands that
//! fundamentally require the not-yet-ported agent runtime (the LLM streaming
//! loop, model catalog, extension/skill system, compaction pipeline, or the
//! session-runtime-host rebind machinery) return an honest `success:false`
//! error through pi's normal per-command error channel — a valid wire shape,
//! never a fake success and never a panic.

use serde_json::{json, Value};

use super::session::RpcSession;
use super::types::{self, RpcCommand};

/// Serialize a successful, data-less response.
fn ok(id: Option<String>, command: &str) -> Value {
    serde_json::to_value(types::success(id, command)).expect("response serializes")
}

/// Serialize a successful response carrying `data`.
fn ok_data(id: Option<String>, command: &str, data: Value) -> Value {
    serde_json::to_value(types::success_data(id, command, data)).expect("response serializes")
}

/// Serialize an error response.
fn err(id: Option<String>, command: &str, message: impl Into<String>) -> Value {
    serde_json::to_value(types::error(id, command, message)).expect("response serializes")
}

/// Handle a single command, returning the response envelope to emit.
pub fn handle_command(session: &mut RpcSession, id: Option<String>, command: RpcCommand) -> Value {
    let cmd = command.type_str();
    match command {
        // ----------------------------------------------------------------
        // Implementable now: in-memory settings
        // ----------------------------------------------------------------
        RpcCommand::GetState => {
            let state = serde_json::to_value(session.state()).expect("state serializes");
            ok_data(id, cmd, state)
        }
        RpcCommand::SetThinkingLevel { level } => {
            session.set_thinking_level(level);
            ok(id, cmd)
        }
        RpcCommand::CycleThinkingLevel => {
            let level = session.cycle_thinking_level();
            ok_data(id, cmd, json!({ "level": level }))
        }
        RpcCommand::SetSteeringMode { mode } => {
            session.set_steering_mode(mode);
            ok(id, cmd)
        }
        RpcCommand::SetFollowUpMode { mode } => {
            session.set_follow_up_mode(mode);
            ok(id, cmd)
        }
        RpcCommand::SetAutoCompaction { enabled } => {
            session.set_auto_compaction(enabled);
            ok(id, cmd)
        }
        RpcCommand::SetAutoRetry { enabled } => {
            session.set_auto_retry(enabled);
            ok(id, cmd)
        }

        // ----------------------------------------------------------------
        // Implementable now: session tree / bash / export
        // ----------------------------------------------------------------
        RpcCommand::GetEntries { since } => match session.get_entries(since.as_deref()) {
            Ok(data) => ok_data(id, cmd, data),
            Err(message) => err(id, cmd, message),
        },
        RpcCommand::GetTree => ok_data(id, cmd, session.get_tree()),
        RpcCommand::GetLastAssistantText => ok_data(id, cmd, session.get_last_assistant_text()),
        RpcCommand::GetMessages => ok_data(id, cmd, session.get_messages()),
        RpcCommand::GetForkMessages => ok_data(id, cmd, session.get_fork_messages()),
        RpcCommand::SetSessionName { name } => match session.set_session_name(&name) {
            Ok(()) => ok(id, cmd),
            Err(message) => err(id, cmd, message),
        },
        RpcCommand::Bash { command, .. } => {
            let result = session.run_bash(&command);
            let data = serde_json::to_value(result).expect("bash result serializes");
            ok_data(id, cmd, data)
        }
        RpcCommand::ExportHtml { output_path } => {
            match session.export_html(output_path.as_deref()) {
                Ok(data) => ok_data(id, cmd, data),
                Err(message) => err(id, cmd, message),
            }
        }

        // ----------------------------------------------------------------
        // Stubbed: require the not-yet-ported agent runtime
        // ----------------------------------------------------------------
        RpcCommand::Prompt { .. } => err(
            id,
            cmd,
            "prompt is not implemented: the agent runtime (AgentSession) is not ported yet",
        ),
        RpcCommand::Steer { .. } => err(
            id,
            cmd,
            "steer is not implemented: the agent runtime is not ported yet",
        ),
        RpcCommand::FollowUp { .. } => err(
            id,
            cmd,
            "follow_up is not implemented: the agent runtime is not ported yet",
        ),
        RpcCommand::Abort => err(
            id,
            cmd,
            "abort is not implemented: the agent runtime is not ported yet",
        ),
        RpcCommand::NewSession { .. } => err(
            id,
            cmd,
            "new_session is not implemented: the session runtime host is not ported yet",
        ),
        RpcCommand::SetModel { .. } => err(
            id,
            cmd,
            "set_model is not implemented: the model runtime is not ported yet",
        ),
        RpcCommand::CycleModel => err(
            id,
            cmd,
            "cycle_model is not implemented: the model runtime is not ported yet",
        ),
        RpcCommand::GetAvailableModels => err(
            id,
            cmd,
            "get_available_models is not implemented: the model runtime is not ported yet",
        ),
        RpcCommand::Compact { .. } => err(
            id,
            cmd,
            "compact is not implemented: the compaction pipeline is not ported yet",
        ),
        RpcCommand::AbortRetry => err(
            id,
            cmd,
            "abort_retry is not implemented: the retry loop is not ported yet",
        ),
        RpcCommand::AbortBash => err(
            id,
            cmd,
            "abort_bash is not implemented: bash abort is not ported yet",
        ),
        RpcCommand::GetSessionStats => err(
            id,
            cmd,
            "get_session_stats is not implemented: session stats accounting is not ported yet",
        ),
        RpcCommand::SwitchSession { .. } => err(
            id,
            cmd,
            "switch_session is not implemented: the session runtime host is not ported yet",
        ),
        RpcCommand::Fork { .. } => err(
            id,
            cmd,
            "fork is not implemented: the session runtime host is not ported yet",
        ),
        RpcCommand::Clone => err(
            id,
            cmd,
            "clone is not implemented: the session runtime host is not ported yet",
        ),
        RpcCommand::GetCommands => err(
            id,
            cmd,
            "get_commands is not implemented: the extension/skill/prompt-template system is not ported yet",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_state_omits_absent_optionals() {
        let mut s = RpcSession::new();
        let resp = handle_command(&mut s, Some("1".into()), RpcCommand::GetState);
        assert_eq!(resp["type"], "response");
        assert_eq!(resp["command"], "get_state");
        assert_eq!(resp["success"], true);
        let data = &resp["data"];
        // Key-absence for the optional fields (never `null`).
        assert!(data.get("sessionName").is_none());
        assert!(data.get("model").is_none());
        assert!(data.get("sessionFile").is_none());
        assert_eq!(data["messageCount"], 0);
        assert_eq!(data["isStreaming"], false);
    }

    #[test]
    fn stub_prompt_is_honest_failure() {
        let mut s = RpcSession::new();
        let resp = handle_command(
            &mut s,
            Some("x".into()),
            RpcCommand::Prompt {
                message: "hi".into(),
                images: None,
                streaming_behavior: None,
            },
        );
        assert_eq!(resp["success"], false);
        assert_eq!(resp["command"], "prompt");
        assert!(resp["error"]
            .as_str()
            .unwrap()
            .contains("not implemented"));
    }

    #[test]
    fn dataless_success_omits_data_key() {
        let mut s = RpcSession::new();
        let resp = handle_command(
            &mut s,
            None,
            RpcCommand::SetThinkingLevel {
                level: types::ThinkingLevel::High,
            },
        );
        assert_eq!(resp["success"], true);
        assert!(resp.get("data").is_none());
        assert!(resp.get("id").is_none());
    }
}
