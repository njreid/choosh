use std::collections::BTreeSet;

use choosh_protocol::agent_event::{
    AdapterContext, AgentAdapter, AgentEventKind, AgentStatus, CapturedHook, ChangeOperation,
    CoarseSummary, DiagnosticCode, InputReason, NormalizedAgentEvent, normalize_claude,
    normalize_codex, normalize_opencode,
};
use serde_json::{Map, Value, json};

const WORKSPACE: &str = "11111111-1111-4111-8111-111111111111";
const ITEM: &str = "22222222-2222-4222-8222-222222222222";

#[test]
fn captured_vendor_fixtures_normalize_to_schema_shaped_redacted_goldens() {
    for case in cases() {
        let capture: Value = serde_json::from_str(case.capture).unwrap();
        let object = capture.as_object().unwrap();
        let (surface, occurred_at, paths) = captured_fields(case.adapter, object);
        let path_refs: Vec<_> = paths
            .iter()
            .map(|(path, operation)| (path.as_str(), *operation))
            .collect();
        let context = AdapterContext {
            workspace_id: WORKSPACE,
            item_id: ITEM,
            root: "/fixture/root",
            adapter: case.adapter,
        };
        let hook = CapturedHook {
            surface,
            occurred_at,
            candidate_paths: &path_refs,
            captured_bytes: case.capture.len(),
        };
        let normalized = match case.adapter {
            AgentAdapter::Codex => normalize_codex(&context, &hook),
            AgentAdapter::Claude => normalize_claude(&context, &hook),
            AgentAdapter::OpenCode => normalize_opencode(&context, &hook),
        }
        .unwrap();
        let actual = schema_value(&normalized);
        let expected: Value = serde_json::from_str(case.expected).unwrap();

        assert_eq!(actual, expected, "{} fixture changed", case.name);
        assert_agent_event_schema_shape(&actual);
        let output = serde_json::to_string(&actual).unwrap();
        assert!(!output.contains("REDACTION_CANARY"));
    }
}

struct Case {
    name: &'static str,
    adapter: AgentAdapter,
    capture: &'static str,
    expected: &'static str,
}

fn cases() -> [Case; 3] {
    [
        Case {
            name: "codex-permission",
            adapter: AgentAdapter::Codex,
            capture: include_str!("fixtures/agent-events/codex-permission.json"),
            expected: include_str!("fixtures/agent-events/codex-permission.normalized.json"),
        },
        Case {
            name: "claude-file-change",
            adapter: AgentAdapter::Claude,
            capture: include_str!("fixtures/agent-events/claude-file-change.json"),
            expected: include_str!("fixtures/agent-events/claude-file-change.normalized.json"),
        },
        Case {
            name: "opencode-error",
            adapter: AgentAdapter::OpenCode,
            capture: include_str!("fixtures/agent-events/opencode-error.json"),
            expected: include_str!("fixtures/agent-events/opencode-error.normalized.json"),
        },
    ]
}

fn captured_fields(
    adapter: AgentAdapter,
    object: &Map<String, Value>,
) -> (&str, &str, Vec<(String, ChangeOperation)>) {
    let string = |key| object.get(key).and_then(Value::as_str).unwrap();
    match adapter {
        AgentAdapter::Codex => (string("hook"), string("occurred_at"), Vec::new()),
        AgentAdapter::Claude => (
            string("hook_event_name"),
            string("timestamp"),
            vec![(string("file_path").to_owned(), ChangeOperation::Modified)],
        ),
        AgentAdapter::OpenCode => (string("event"), string("time"), Vec::new()),
    }
}

fn schema_value(event: &NormalizedAgentEvent) -> Value {
    let base = |kind: &str| {
        json!({
            "type": kind,
            "item_id": event.item_id,
            "occurred_at": event.adapter_occurred_at,
        })
    };
    match &event.kind {
        AgentEventKind::InputRequired { reason, summary } => {
            let mut value = base("input_required");
            let object = value.as_object_mut().unwrap();
            object.insert("reason".into(), json!(input_reason(*reason)));
            object.insert("summary".into(), json!(coarse_summary(*summary)));
            value
        }
        AgentEventKind::TurnCompleted => base("turn_completed"),
        AgentEventKind::FilesChanged { candidates } => {
            let mut value = base("files_changed");
            value.as_object_mut().unwrap().insert(
                "paths".into(),
                json!(
                    candidates
                        .iter()
                        .map(|candidate| &candidate.path)
                        .collect::<Vec<_>>()
                ),
            );
            value
        }
        AgentEventKind::AgentStatus { status, diagnostic } => {
            let mut value = base("agent_status");
            let object = value.as_object_mut().unwrap();
            object.insert("status".into(), json!(agent_status(*status)));
            if let Some(diagnostic) = diagnostic {
                object.insert("message".into(), json!(diagnostic_code(*diagnostic)));
            }
            value
        }
    }
}

fn input_reason(reason: InputReason) -> &'static str {
    match reason {
        InputReason::Approval => "approval",
        InputReason::Permission => "permission",
        InputReason::Question => "question",
        InputReason::Elicitation => "elicitation",
        InputReason::NextPrompt => "next_prompt",
    }
}

fn coarse_summary(summary: CoarseSummary) -> &'static str {
    match summary {
        CoarseSummary::ApprovalRequired => "approval_required",
        CoarseSummary::PermissionRequired => "permission_required",
        CoarseSummary::QuestionWaiting => "question_waiting",
        CoarseSummary::InputRequested => "input_requested",
        CoarseSummary::ReadyForNextPrompt => "ready_for_next_prompt",
    }
}

fn agent_status(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Starting => "starting",
        AgentStatus::Busy => "busy",
        AgentStatus::Waiting => "waiting",
        AgentStatus::Stopped => "stopped",
        AgentStatus::Failed => "failed",
    }
}

fn diagnostic_code(code: DiagnosticCode) -> &'static str {
    match code {
        DiagnosticCode::AgentReportedFailure => "agent_reported_failure",
    }
}

fn assert_agent_event_schema_shape(value: &Value) {
    let object = value.as_object().unwrap();
    let kind = object.get("type").and_then(Value::as_str).unwrap();
    let required: BTreeSet<&str> = match kind {
        "input_required" => ["type", "item_id", "occurred_at", "reason"]
            .into_iter()
            .collect(),
        "turn_completed" => ["type", "item_id", "occurred_at"].into_iter().collect(),
        "files_changed" => ["type", "item_id", "occurred_at", "paths"]
            .into_iter()
            .collect(),
        "agent_status" => ["type", "item_id", "occurred_at", "status"]
            .into_iter()
            .collect(),
        _ => panic!("unknown normalized event type"),
    };
    assert!(required.iter().all(|key| object.contains_key(*key)));
    assert!(object.keys().all(|key| match kind {
        "input_required" =>
            ["type", "item_id", "occurred_at", "reason", "summary"].contains(&key.as_str()),
        "turn_completed" => ["type", "item_id", "occurred_at"].contains(&key.as_str()),
        "files_changed" => ["type", "item_id", "occurred_at", "paths"].contains(&key.as_str()),
        "agent_status" =>
            ["type", "item_id", "occurred_at", "status", "message"].contains(&key.as_str()),
        _ => false,
    }));
}
