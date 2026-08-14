//! Wire types for the `host-rpc.md` request/response surface `choosh-hostd`
//! exposes over an `rpc`-purpose relay tunnel (see [`crate::relay`]'s
//! `ServerPush::TunnelOffered`/tunnel-frame helpers for how these bytes
//! reach a devhost in the first place). Scoped to the M1 methods
//! (`docs/milestones/M1-workspace-and-jj.md`: `workspace.create`,
//! `workspace.list`, `workspace.status`, `workspace.tree.list`,
//! `workspace.file.read`) plus M2's Item RPCs (`docs/milestones/M2-terminal-and-agents.md`,
//! `host-rpc.md`'s "Item RPCs" section: `item.create`, `item.list`,
//! `item.stop`). Later milestones' methods (`project.*`,
//! `workspace.diff`/`log`/`op.*`, `workspace.file.write`) land alongside the
//! milestone that needs them, same discipline `crate::relay` already
//! follows for `agent-event`/`register-fcm-token`.

use serde::{Deserialize, Serialize};

/// Either half of `workspace.create`'s `project_source` field
/// (`host-rpc.md`): untagged because the wire shape is literally
/// `{"clone_url": "..."}` or `{"existing_path": "..."}` with no separate
/// discriminant key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProjectSource {
    CloneUrl { clone_url: String },
    ExistingPath { existing_path: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ByteRange {
    pub offset: u64,
    pub length: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum RpcRequest {
    WorkspaceCreate {
        request_id: String,
        devhost_id: String,
        workspace_name: String,
        project_source: ProjectSource,
        parent_workspace_id: Option<String>,
    },
    WorkspaceList {
        request_id: String,
    },
    WorkspaceStatus {
        request_id: String,
        workspace_id: String,
    },
    WorkspaceTreeList {
        request_id: String,
        workspace_id: String,
        path_prefix: String,
        revision: Option<String>,
        cursor: Option<String>,
    },
    WorkspaceFileRead {
        request_id: String,
        workspace_id: String,
        path: String,
        revision: Option<String>,
        range: Option<ByteRange>,
    },
    /// `host-rpc.md`'s fixed item-type set is `AgentTerminal`, `Shell`,
    /// `JjChangeGraph`, `JjDiff`, `SourceEditor`, `MarkdownPreview`,
    /// `WebService`, `EditorPresence` — only `AgentTerminal` and `Shell`
    /// correspond to an actual Zellij tab with a live process and an
    /// `item.create` call of their own; the rest are client-side
    /// projections over other RPCs/events, per `host-rpc.md`. `WebService`
    /// registration is real here but its tunnel-serving is a later
    /// milestone (`service-tunnels.md`).
    ItemCreate {
        request_id: String,
        workspace_id: String,
        item_type: ItemType,
        /// Unique within the workspace; becomes the Zellij tab name.
        name: String,
        /// Required when `item_type` is `AgentTerminal`.
        agent: Option<AgentKind>,
        /// Required when `item_type` is `WebService`. Fixed argv, never a
        /// shell string, per `host-rpc.md`'s "Command construction".
        command: Option<Vec<String>>,
        /// Required when `item_type` is `WebService`.
        port: Option<u16>,
    },
    ItemList {
        request_id: String,
        workspace_id: String,
    },
    ItemStop {
        request_id: String,
        item_id: String,
    },
}

impl RpcRequest {
    #[must_use]
    pub fn request_id(&self) -> &str {
        match self {
            Self::WorkspaceCreate { request_id, .. }
            | Self::WorkspaceList { request_id }
            | Self::WorkspaceStatus { request_id, .. }
            | Self::WorkspaceTreeList { request_id, .. }
            | Self::WorkspaceFileRead { request_id, .. }
            | Self::ItemCreate { request_id, .. }
            | Self::ItemList { request_id, .. }
            | Self::ItemStop { request_id, .. } => request_id,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ItemType {
    AgentTerminal,
    Shell,
    WebService,
}

/// Matches `CHOOSH_AGENT=codex|opencode|claude` per `agent-events.md`'s
/// adapter contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Codex,
    Claude,
    Opencode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Running,
    Stopped,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ItemSummary {
    pub item_id: String,
    pub item_type: ItemType,
    pub name: String,
    /// Opaque Zellij tab identifier this item is bound to.
    pub tab_target: String,
    pub status: ItemStatus,
    pub port: Option<u16>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSummary {
    pub workspace_id: String,
    pub workspace_name: String,
    pub devhost_id: String,
    pub project_id: String,
    pub created_at: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChangedPath {
    pub path: String,
    pub kind: ChangeKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TreeEntryKind {
    File,
    Directory,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TreeEntry {
    pub name: String,
    pub kind: TreeEntryKind,
    pub conflicted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum RpcResponse {
    WorkspaceCreateOk {
        request_id: String,
        workspace_id: String,
        workspace_name: String,
        project_id: String,
    },
    WorkspaceListOk {
        request_id: String,
        workspaces: Vec<WorkspaceSummary>,
    },
    WorkspaceStatusOk {
        request_id: String,
        changed: Vec<ChangedPath>,
        conflicted: Vec<String>,
    },
    WorkspaceTreeListOk {
        request_id: String,
        entries: Vec<TreeEntry>,
        next_cursor: Option<String>,
    },
    WorkspaceFileReadOk {
        request_id: String,
        /// Base64-encoded bytes of the requested (possibly ranged) content.
        content_base64: String,
        total_size: u64,
    },
    ItemCreateOk {
        request_id: String,
        item_id: String,
        item_type: ItemType,
        name: String,
        tab_target: String,
    },
    ItemListOk {
        request_id: String,
        items: Vec<ItemSummary>,
    },
    ItemStopOk {
        request_id: String,
    },
    Error {
        request_id: String,
        /// One of `host-rpc.md`'s fixed error codes: `not_found`,
        /// `out_of_root`, `invalid_argument`, `conflict`, `bound_exceeded`,
        /// `internal`.
        code: String,
        message: String,
    },
}

impl RpcResponse {
    #[must_use]
    pub fn request_id(&self) -> &str {
        match self {
            Self::WorkspaceCreateOk { request_id, .. }
            | Self::WorkspaceListOk { request_id, .. }
            | Self::WorkspaceStatusOk { request_id, .. }
            | Self::WorkspaceTreeListOk { request_id, .. }
            | Self::WorkspaceFileReadOk { request_id, .. }
            | Self::ItemCreateOk { request_id, .. }
            | Self::ItemListOk { request_id, .. }
            | Self::ItemStopOk { request_id, .. }
            | Self::Error { request_id, .. } => request_id,
        }
    }
}

/// `host-rpc.md`'s bounds: page size for `workspace.tree.list`, and the max
/// byte range for `workspace.file.read`.
pub const MAX_TREE_LIST_PAGE: usize = 500;
pub const MAX_FILE_READ_RANGE_BYTES: u64 = 4 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_source_clone_url_round_trips_untagged() {
        let source = ProjectSource::CloneUrl { clone_url: "https://example.invalid/repo.git".to_string() };
        let json = serde_json::to_string(&source).unwrap();
        assert_eq!(json, r#"{"clone_url":"https://example.invalid/repo.git"}"#);
        assert_eq!(serde_json::from_str::<ProjectSource>(&json).unwrap(), source);
    }

    #[test]
    fn project_source_existing_path_round_trips_untagged() {
        let source = ProjectSource::ExistingPath { existing_path: "/workspaces/app".to_string() };
        let json = serde_json::to_string(&source).unwrap();
        assert_eq!(serde_json::from_str::<ProjectSource>(&json).unwrap(), source);
    }

    #[test]
    fn workspace_create_request_round_trips() {
        let request = RpcRequest::WorkspaceCreate {
            request_id: "id".to_string(),
            devhost_id: "dev-1".to_string(),
            workspace_name: "app".to_string(),
            project_source: ProjectSource::ExistingPath { existing_path: "/workspaces/app".to_string() },
            parent_workspace_id: None,
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"type\":\"workspace-create\""));
        assert_eq!(serde_json::from_str::<RpcRequest>(&json).unwrap(), request);
        assert_eq!(request.request_id(), "id");
    }

    #[test]
    fn error_response_carries_request_id() {
        let response = RpcResponse::Error {
            request_id: "id".to_string(),
            code: "not_found".to_string(),
            message: "workspace not found".to_string(),
        };
        assert_eq!(response.request_id(), "id");
    }
}
