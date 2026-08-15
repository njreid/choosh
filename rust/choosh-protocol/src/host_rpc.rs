//! Wire types for the `host-rpc.md` request/response surface `choosh-hostd`
//! exposes over an `rpc`-purpose relay tunnel (see [`crate::relay`]'s
//! `ServerPush::TunnelOffered`/tunnel-frame helpers for how these bytes
//! reach a devhost in the first place). Scoped to the M1 methods
//! (`docs/milestones/M1-workspace-and-jj.md`: `workspace.create`,
//! `workspace.list`, `workspace.status`, `workspace.tree.list`,
//! `workspace.file.read`) plus M2's Item RPCs (`docs/milestones/M2-terminal-and-agents.md`,
//! `host-rpc.md`'s "Item RPCs" section: `item.create`, `item.list`,
//! `item.stop`) plus M3's jj diff/graph RPCs
//! (`docs/milestones/M3-jj-diff-and-graph.md`, `docs/specs/jj-integration.md`:
//! `workspace.diff`, `workspace.log`, `workspace.op.log`, `workspace.op.undo`,
//! `workspace.op.restore`) plus M4's editing RPC
//! (`docs/milestones/M4-editing.md`, `docs/specs/jj-integration.md`'s
//! `workspace.file.write` section: `WorkspaceFileWrite`/`WorkspaceFileWriteOk`/
//! `WorkspaceFileWriteStale`, plus the `revision` field M4 added to the
//! pre-existing `WorkspaceFileReadOk`). Later milestones' methods
//! (`project.*`) land alongside the milestone that needs them, same
//! discipline `crate::relay` already follows for
//! `agent-event`/`register-fcm-token`.

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
    /// `jj-integration.md`'s `workspace.diff { workspace_id, from = "@-", to
    /// = "@" }` — `from`/`to` are `None` for their documented defaults, not
    /// duplicated as literal `"@-"`/`"@"` strings on the wire.
    WorkspaceDiff {
        request_id: String,
        workspace_id: String,
        from: Option<String>,
        to: Option<String>,
    },
    /// `jj-integration.md`'s `workspace.log { workspace_id, revset?, limit
    /// }` — `revset: None` means `jj log`'s own default revset.
    WorkspaceLog {
        request_id: String,
        workspace_id: String,
        revset: Option<String>,
        limit: usize,
    },
    WorkspaceOpLog {
        request_id: String,
        workspace_id: String,
        limit: usize,
    },
    WorkspaceOpUndo {
        request_id: String,
        workspace_id: String,
        op_id: String,
    },
    WorkspaceOpRestore {
        request_id: String,
        workspace_id: String,
        op_id: String,
    },
    /// `editor-protocol.md`'s save path / `jj-integration.md`'s
    /// `workspace.file.write`. `base_revision` MUST be the `revision` the
    /// caller most recently read `path` at (from `WorkspaceFileReadOk` or a
    /// prior `WorkspaceFileWriteOk`) — `hostd` MUST reject the write with
    /// `WorkspaceFileWriteStale` rather than silently overwriting when it
    /// no longer matches the file's current on-disk revision.
    ///
    /// V1 scope reduction, deliberate and reported (per this crate's
    /// existing precedent of stating a narrowed scope rather than silently
    /// picking one): `content_base64` is always a full-content replacement,
    /// never the incremental UTF-8 range-edit list `editor-protocol.md`
    /// describes Sora emitting — jj-integration.md explicitly leaves
    /// `content_or_edits`'s wire shape "an implementation choice deferred
    /// to the editor-protocol spec, not fixed here", and a full-replacement
    /// body is the smaller, unambiguous surface for this milestone; an
    /// incremental-edit variant is a reasonable, non-breaking follow-up
    /// (it would add a sibling variant here, not change this one).
    WorkspaceFileWrite {
        request_id: String,
        workspace_id: String,
        path: String,
        base_revision: String,
        content_base64: String,
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
            | Self::ItemStop { request_id, .. }
            | Self::WorkspaceDiff { request_id, .. }
            | Self::WorkspaceLog { request_id, .. }
            | Self::WorkspaceOpLog { request_id, .. }
            | Self::WorkspaceOpUndo { request_id, .. }
            | Self::WorkspaceOpRestore { request_id, .. }
            | Self::WorkspaceFileWrite { request_id, .. } => request_id,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffSegmentKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiffSegment {
    pub kind: DiffSegmentKind,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiffHunk {
    pub old_start: u64,
    pub old_lines: u64,
    pub new_start: u64,
    pub new_lines: u64,
    pub segments: Vec<DiffSegment>,
}

/// `jj-integration.md`'s `workspace.diff` result: `old_path`/`new_path`
/// differ only when `jj-lib` has already resolved a rename pairing.
/// Binary/oversized files are `Binary` metadata, never garbled hunks.
/// `#[serde(untagged)]` picks the right shape by which fields are present
/// (`hunks` vs. `byte_size`), matching this file's existing `ProjectSource`
/// precedent rather than adding a redundant discriminant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DiffFileEntry {
    Hunks { old_path: Option<String>, new_path: Option<String>, hunks: Vec<DiffHunk> },
    Binary { path: String, status: ChangeKind, byte_size: u64 },
}

/// One `workspace.log` change-graph node, per `jj-integration.md`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChangeGraphNode {
    pub change_id: String,
    pub commit_id: String,
    pub description: String,
    pub author: String,
    pub parent_change_ids: Vec<String>,
    pub is_working_copy: bool,
    pub bookmarks: Vec<String>,
}

/// One `workspace.op.log` entry, per `jj-integration.md`. `tags` is jj's
/// own operation metadata (e.g. `user`, `hostname`) as a map, not a free
/// label list — mirroring what `jj op log` actually attaches to an entry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperationLogEntry {
    pub op_id: String,
    pub description: String,
    pub start_time: String,
    pub end_time: String,
    pub tags: std::collections::BTreeMap<String, String>,
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
        /// The content identity of the file's *entire* current state (not
        /// just the returned range) at read time — a hex-encoded SHA-256 of
        /// the whole file's bytes, per `jj-integration.md`'s revision-
        /// identity choice. Directly usable as `WorkspaceFileWrite`'s
        /// `base_revision`; a ranged read still reflects the whole file so a
        /// concurrent write to a part of the file the client never read is
        /// still detected as stale, per `editor-protocol.md`'s conflict
        /// model ("the file changed on disk since the client's last read",
        /// not "the part I looked at changed").
        revision: String,
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
    WorkspaceDiffOk {
        request_id: String,
        files: Vec<DiffFileEntry>,
    },
    WorkspaceLogOk {
        request_id: String,
        changes: Vec<ChangeGraphNode>,
    },
    WorkspaceOpLogOk {
        request_id: String,
        operations: Vec<OperationLogEntry>,
    },
    /// `op.undo`/`op.restore` are themselves new operations in the log
    /// (`jj-integration.md`: "MUST themselves produce a new operation-log
    /// entry"), so both `Ok`s carry the id of that new entry, not the id
    /// that was undone/restored to.
    WorkspaceOpUndoOk {
        request_id: String,
        new_op_id: String,
    },
    WorkspaceOpRestoreOk {
        request_id: String,
        new_op_id: String,
    },
    WorkspaceFileWriteOk {
        request_id: String,
        /// The file's new revision after this write, immediately usable as
        /// the next edit's `base_revision` without a follow-up
        /// `workspace.file.read`.
        revision: String,
    },
    /// A dedicated variant rather than the generic `Error`, per
    /// `jj-integration.md`: a stale write "MUST" carry the current revision
    /// and content back, which `Error`'s `{code, message}` shape has no
    /// field for — `code`/`message`-only errors are for the other,
    /// unrelated `host-rpc.md` failure modes (`not_found`, `internal`,
    /// etc.), not this one.
    WorkspaceFileWriteStale {
        request_id: String,
        current_revision: String,
        current_content_base64: String,
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
            | Self::WorkspaceDiffOk { request_id, .. }
            | Self::WorkspaceLogOk { request_id, .. }
            | Self::WorkspaceOpLogOk { request_id, .. }
            | Self::WorkspaceOpUndoOk { request_id, .. }
            | Self::WorkspaceOpRestoreOk { request_id, .. }
            | Self::WorkspaceFileWriteOk { request_id, .. }
            | Self::WorkspaceFileWriteStale { request_id, .. }
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
