//! Observational agent hook adapters, per `docs/specs/agent-events.md`.
//! Two halves: **installation** (merge-safe patching of an agent's own
//! hook-config file to point at `choosh-hostd emit`) and **emission** (the
//! `emit` subcommand itself, invoked by the hook script per lifecycle
//! event — see `choosh-hostd emit`'s wiring in `lib.rs`).
//!
//! Hooks MUST NOT approve, deny, rewrite, block, or inject model context,
//! per the spec's non-negotiable rule — nothing in this module has any
//! capability to do so: `emit` only ever normalizes and forwards, it never
//! returns anything to the agent process beyond a plain exit code.
//!
//! **Claude Code's exact hook-config JSON schema is not verified against a
//! live payload in this pass** — this environment's own `~/.claude/settings.json`
//! has no `hooks` key configured, so there was nothing to check the
//! installer's merge logic or the `emit` payload-parsing heuristic against
//! directly. Implemented against the surface names and general
//! matcher-group shape documented in `agent-events.md` and Claude Code's
//! public hooks documentation as best understood here — flag this for
//! verification against a real Claude Code installation before relying on
//! it, per this crate's established "confirm real tool behavior, don't
//! assume the docs are complete" discipline (which this module could not
//! fully follow, for lack of a live target to confirm against).

use std::path::Path;

use choosh_protocol::agent_event::{
    AdapterContext, AgentAdapter, AgentEventKind, AgentStatus, CandidatePath, CapturedHook, NormalizedAgentEvent,
    ValidationError,
};
use choosh_protocol::relay::{WireAgentEvent, WireAgentStatus, WireInputReason};
use serde_json::Value;

/// Converts a validated [`NormalizedAgentEvent`] to its serde-friendly wire
/// sibling, per `choosh_protocol::relay::WireAgentEvent`'s own doc comment
/// ("`choosh-hostd` is responsible for producing one of these"). `AuthRequired`/
/// `EditorAttached`/`EditorDetached` aren't reachable from
/// [`AgentEventKind`] (they're M6/M7 concerns with their own producers, not
/// hook-derived) so this conversion is total over the M2 event set.
#[must_use]
pub fn to_wire_event(event: &NormalizedAgentEvent) -> WireAgentEvent {
    let workspace_id = event.workspace_id.clone();
    let item_id = event.item_id.clone();
    match &event.kind {
        AgentEventKind::InputRequired { reason, .. } => {
            WireAgentEvent::InputRequired { workspace_id, item_id, reason: to_wire_reason(*reason) }
        }
        AgentEventKind::TurnCompleted => WireAgentEvent::TurnCompleted { workspace_id, item_id },
        AgentEventKind::FilesChanged { candidates } => {
            WireAgentEvent::FilesChanged { workspace_id, item_id, paths: candidates.iter().map(path_of).collect() }
        }
        AgentEventKind::AgentStatus { status, .. } => {
            WireAgentEvent::AgentStatus { workspace_id, item_id, status: to_wire_status(*status) }
        }
    }
}

fn path_of(candidate: &CandidatePath) -> String {
    candidate.path.clone()
}

fn to_wire_reason(reason: choosh_protocol::agent_event::InputReason) -> WireInputReason {
    use choosh_protocol::agent_event::InputReason;
    match reason {
        InputReason::Approval => WireInputReason::Approval,
        InputReason::Permission => WireInputReason::Permission,
        InputReason::Question => WireInputReason::Question,
        InputReason::Elicitation => WireInputReason::Elicitation,
        InputReason::NextPrompt => WireInputReason::NextPrompt,
    }
}

fn to_wire_status(status: AgentStatus) -> WireAgentStatus {
    match status {
        AgentStatus::Starting => WireAgentStatus::Starting,
        AgentStatus::Busy => WireAgentStatus::Busy,
        AgentStatus::Waiting => WireAgentStatus::Waiting,
        AgentStatus::Stopped => WireAgentStatus::Stopped,
        AgentStatus::Failed => WireAgentStatus::Failed,
    }
}

/// Everything `choosh-hostd emit` needs to produce one normalized event
/// from a captured hook invocation, gathered from the environment and
/// stdin by the caller (kept as plain data here so this function is
/// testable without real env vars or a real stdin).
pub struct EmitInput<'a> {
    pub workspace_id: &'a str,
    pub item_id: &'a str,
    pub root: &'a str,
    pub agent: AgentAdapter,
    pub surface: &'a str,
    pub occurred_at: &'a str,
    pub raw_payload: &'a [u8],
}

/// Best-effort extraction of file-path hints from a hook's raw JSON
/// payload, for the surfaces that carry them (`PostToolUse`/`FileChanged`
/// etc. — `agent_event::map_surface` decides which surfaces actually use
/// this). Looks for common field names (`file_path`, `path`, or a
/// `tool_input.file_path` nested shape) rather than one fixed schema,
/// since tool payloads vary by tool — an unrecognized shape yields no
/// candidates rather than a parse error, per the "hints, not authority"
/// posture `agent-events.md` already establishes for this data.
fn extract_candidate_paths(raw_payload: &[u8]) -> Vec<(String, choosh_protocol::agent_event::ChangeOperation)> {
    use choosh_protocol::agent_event::ChangeOperation;
    let Ok(value) = serde_json::from_slice::<Value>(raw_payload) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for key in ["file_path", "path", "filePath"] {
        if let Some(path) = value.get(key).and_then(Value::as_str) {
            paths.push((path.to_string(), ChangeOperation::Unknown));
        }
    }
    if let Some(nested) = value.get("tool_input").and_then(|v| v.get("file_path")).and_then(Value::as_str) {
        paths.push((nested.to_string(), ChangeOperation::Unknown));
    }
    paths
}

/// Normalizes one captured hook invocation end to end: builds the
/// `AdapterContext`/`CapturedHook` `agent_event`'s validators expect and
/// dispatches to the matching `normalize_*` function.
///
/// # Errors
///
/// Whatever `agent_event::normalize_*` returns — invalid context, an
/// oversized capture, or a surface this adapter doesn't recognize (a
/// legitimate outcome, not a bug: only a subset of an agent's hook surfaces
/// map to a normalized event, per `agent-events.md`'s table).
pub fn normalize(input: &EmitInput<'_>) -> Result<NormalizedAgentEvent, ValidationError> {
    let context =
        AdapterContext { workspace_id: input.workspace_id, item_id: input.item_id, root: input.root, adapter: input.agent };
    let candidate_paths = extract_candidate_paths(input.raw_payload);
    let borrowed: Vec<(&str, choosh_protocol::agent_event::ChangeOperation)> =
        candidate_paths.iter().map(|(p, op)| (p.as_str(), *op)).collect();
    let hook = CapturedHook {
        surface: input.surface,
        occurred_at: input.occurred_at,
        candidate_paths: &borrowed,
        captured_bytes: input.raw_payload.len(),
    };
    match input.agent {
        AgentAdapter::Codex => choosh_protocol::agent_event::normalize_codex(&context, &hook),
        AgentAdapter::Claude => choosh_protocol::agent_event::normalize_claude(&context, &hook),
        AgentAdapter::OpenCode => choosh_protocol::agent_event::normalize_opencode(&context, &hook),
    }
}

/// The choosh-owned command line each installed hook entry points at.
/// `hostd_binary` is the absolute path to this `choosh-hostd` binary
/// (`std::env::current_exe()` at install time) — not just `"choosh-hostd"`,
/// since the installed hook config may run under a shell/environment where
/// `PATH` doesn't include it.
fn emit_command(hostd_binary: &str, surface: &str) -> String {
    format!("{hostd_binary} emit --surface {surface}")
}

const CLAUDE_SURFACES: &[&str] = &["PermissionRequest", "Notification", "FileChanged", "PostToolUse", "Stop", "UserPromptSubmit"];

/// Merge-safe install of `choosh-hostd emit` into a Claude Code
/// `settings.json`-shaped file at `settings_path`: for each surface in
/// [`CLAUDE_SURFACES`], ensures a matcher-group whose command is exactly
/// `emit_command(hostd_binary, surface)` exists, without touching any
/// existing entries (this devhost's or the user's own) for that surface.
/// Creates the file (and an empty `{}` base) if it doesn't exist yet.
///
/// # Errors
///
/// Returns an I/O error on read/write failure, or a `serde_json::Error` if
/// an existing file isn't valid JSON (a corrupt user config is a hard
/// error here, same "never silently paper over" posture `registry.rs`/
/// `credential.rs` already take for their own files) or isn't a JSON
/// object at the top level.
pub fn install_claude_hooks(settings_path: &Path, hostd_binary: &str) -> Result<(), HookInstallError> {
    let mut root: Value = match std::fs::read(settings_path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(HookInstallError::InvalidJson)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Object(serde_json::Map::new()),
        Err(error) => return Err(HookInstallError::Io(error)),
    };
    let Value::Object(root_map) = &mut root else {
        return Err(HookInstallError::NotAnObject);
    };
    let hooks = root_map.entry("hooks").or_insert_with(|| Value::Object(serde_json::Map::new()));
    let Value::Object(hooks_map) = hooks else {
        return Err(HookInstallError::NotAnObject);
    };

    for surface in CLAUDE_SURFACES {
        let command = emit_command(hostd_binary, surface);
        let entries = hooks_map.entry(*surface).or_insert_with(|| Value::Array(Vec::new()));
        let Value::Array(groups) = entries else {
            return Err(HookInstallError::NotAnObject);
        };
        let already_installed = groups.iter().any(|group| group_has_command(group, &command));
        if !already_installed {
            groups.push(serde_json::json!({
                "hooks": [ { "type": "command", "command": command } ]
            }));
        }
    }

    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(&root).map_err(HookInstallError::InvalidJson)?;
    // Same atomic write-then-rename discipline as credential.rs/registry.rs
    // — this is real user configuration, never leave it half-written.
    crate::fs_util::atomic_write(settings_path, &json, None)?;
    Ok(())
}

fn group_has_command(group: &Value, command: &str) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| hooks.iter().any(|h| h.get("command").and_then(Value::as_str) == Some(command)))
}

#[derive(Debug)]
pub enum HookInstallError {
    Io(std::io::Error),
    InvalidJson(serde_json::Error),
    NotAnObject,
}

impl std::fmt::Display for HookInstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "hook install I/O error: {error}"),
            Self::InvalidJson(error) => write!(f, "hook config is not valid JSON: {error}"),
            Self::NotAnObject => write!(f, "hook config's \"hooks\" key (or an entry within it) is not a JSON object/array as expected"),
        }
    }
}

impl std::error::Error for HookInstallError {}

impl From<std::io::Error> for HookInstallError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use choosh_protocol::agent_event::{AgentStatus as InternalAgentStatus, InputReason};

    #[test]
    fn input_required_converts_to_the_matching_wire_reason() {
        let event = NormalizedAgentEvent {
            workspace_id: "ws-1".to_string(),
            item_id: "item-1".to_string(),
            adapter: AgentAdapter::Claude,
            adapter_occurred_at: "2026-08-14T00:00:00Z".to_string(),
            kind: AgentEventKind::InputRequired {
                reason: InputReason::Permission,
                summary: choosh_protocol::agent_event::CoarseSummary::PermissionRequired,
            },
        };
        assert_eq!(
            to_wire_event(&event),
            WireAgentEvent::InputRequired {
                workspace_id: "ws-1".to_string(),
                item_id: "item-1".to_string(),
                reason: WireInputReason::Permission,
            }
        );
    }

    #[test]
    fn agent_status_converts_to_the_matching_wire_status() {
        let event = NormalizedAgentEvent {
            workspace_id: "ws-1".to_string(),
            item_id: "item-1".to_string(),
            adapter: AgentAdapter::OpenCode,
            adapter_occurred_at: "2026-08-14T00:00:00Z".to_string(),
            kind: AgentEventKind::AgentStatus { status: InternalAgentStatus::Failed, diagnostic: None },
        };
        assert_eq!(
            to_wire_event(&event),
            WireAgentEvent::AgentStatus {
                workspace_id: "ws-1".to_string(),
                item_id: "item-1".to_string(),
                status: WireAgentStatus::Failed,
            }
        );
    }

    fn valid_input(surface: &'static str, payload: &'static [u8]) -> EmitInput<'static> {
        EmitInput {
            workspace_id: "ws-11111111-1111-1111-1111-111111111111",
            item_id: "item-22222222-2222-2222-2222-222222222222",
            root: "/workspaces/app",
            agent: AgentAdapter::Claude,
            surface,
            occurred_at: "2026-08-14T00:00:00Z",
            raw_payload: payload,
        }
    }

    #[test]
    fn normalize_produces_files_changed_from_a_recognized_field_name() {
        let input = valid_input("PostToolUse", br#"{"file_path": "src/main.rs"}"#);
        let event = normalize(&input).unwrap();
        assert!(matches!(
            event.kind,
            AgentEventKind::FilesChanged { candidates } if candidates.len() == 1 && candidates[0].path == "src/main.rs"
        ));
    }

    #[test]
    fn normalize_produces_empty_candidates_from_an_unrecognized_payload_shape_rather_than_erroring() {
        let input = valid_input("PostToolUse", br#"{"something_else": 1}"#);
        let event = normalize(&input).unwrap();
        assert!(matches!(event.kind, AgentEventKind::FilesChanged { candidates } if candidates.is_empty()));
    }

    #[test]
    fn normalize_rejects_an_unknown_surface() {
        let input = valid_input("NotARealSurface", b"{}");
        assert!(matches!(normalize(&input), Err(ValidationError::UnknownSurface)));
    }

    #[test]
    fn install_creates_a_fresh_settings_file_with_every_surface() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        install_claude_hooks(&path, "/usr/local/bin/choosh-hostd").unwrap();

        let content: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for surface in CLAUDE_SURFACES {
            let groups = content["hooks"][surface].as_array().unwrap();
            assert!(groups.iter().any(|g| group_has_command(g, &emit_command("/usr/local/bin/choosh-hostd", surface))));
        }
    }

    #[test]
    fn install_is_merge_safe_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "theme": "dark",
                "hooks": {
                    "Stop": [ { "hooks": [ { "type": "command", "command": "some-other-users-existing-hook" } ] } ]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        install_claude_hooks(&path, "/usr/local/bin/choosh-hostd").unwrap();
        install_claude_hooks(&path, "/usr/local/bin/choosh-hostd").unwrap(); // second install must not duplicate

        let content: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(content["theme"], "dark", "unrelated existing config must survive");
        let stop_groups = content["hooks"]["Stop"].as_array().unwrap();
        assert!(stop_groups.iter().any(|g| group_has_command(g, "some-other-users-existing-hook")), "existing hook must survive");
        let choosh_command = emit_command("/usr/local/bin/choosh-hostd", "Stop");
        let choosh_count = stop_groups.iter().filter(|g| group_has_command(g, &choosh_command)).count();
        assert_eq!(choosh_count, 1, "installing twice must not duplicate the entry");
    }

    #[test]
    fn install_rejects_a_non_object_top_level() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, b"[1,2,3]").unwrap();
        assert!(matches!(install_claude_hooks(&path, "/x"), Err(HookInstallError::NotAnObject)));
    }
}
