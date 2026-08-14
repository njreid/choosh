//! Dispatches `host-rpc.md`'s M1 request set against the registry, `jj`,
//! Zellij, and root-confined filesystem layers. Deliberately a plain
//! function over already-decoded request/response types — the
//! tunnel/frame plumbing that gets bytes here lives in `serve.rs`, kept
//! separate so this dispatch logic is testable without any WebSocket
//! machinery, the same split `choosh-relayd`'s enroll handling uses.

use std::path::PathBuf;

use choosh_protocol::host_rpc::{
    ChangedPath, ItemStatus, ItemSummary, ItemType, MAX_FILE_READ_RANGE_BYTES, MAX_TREE_LIST_PAGE, ProjectSource,
    RpcRequest, RpcResponse, WorkspaceSummary,
};
use tokio::sync::Mutex;

use crate::agent_launch::agent_launch_argv;
use crate::fs_ops::{self, FsError};
use crate::jj_ops::{self, JjError};
use crate::registry::{Registry, RegistryError};
use crate::zellij_ops::{self, ZellijError};

pub struct RpcContext {
    pub registry: Mutex<Registry>,
    pub devhost_id: String,
    /// Root confinement boundary for every `workspace.create` destination
    /// (both a fresh clone and an adopted `existing_path`) — per
    /// `host-rpc.md`'s "root-confined under the devhost's configured
    /// workspaces directory".
    pub workspaces_dir: PathBuf,
}

/// # Panics
///
/// Never in practice — `registry`'s lock is only ever held for the
/// duration of a synchronous registry read/write within this module, no
/// `.await` point holds it, so it cannot be poisoned by a panicking task
/// while locked.
pub async fn dispatch(ctx: &RpcContext, request: RpcRequest) -> RpcResponse {
    let request_id = request.request_id().to_string();
    match request {
        RpcRequest::WorkspaceCreate { workspace_name, project_source, parent_workspace_id, .. } => {
            handle_create(ctx, request_id, workspace_name, project_source, parent_workspace_id).await
        }
        RpcRequest::WorkspaceList { .. } => handle_list(ctx, request_id).await,
        RpcRequest::WorkspaceStatus { workspace_id, .. } => handle_status(ctx, request_id, &workspace_id).await,
        RpcRequest::WorkspaceTreeList { workspace_id, path_prefix, revision, cursor, .. } => {
            handle_tree_list(ctx, request_id, &workspace_id, &path_prefix, revision.as_deref(), cursor.as_deref()).await
        }
        RpcRequest::WorkspaceFileRead { workspace_id, path, revision, range, .. } => {
            handle_file_read(ctx, request_id, &workspace_id, &path, revision.as_deref(), range).await
        }
        RpcRequest::ItemCreate { workspace_id, item_type, name, agent, command, port, .. } => {
            handle_item_create(ctx, request_id, &workspace_id, item_type, name, agent, command, port).await
        }
        RpcRequest::ItemList { workspace_id, .. } => handle_item_list(ctx, request_id, &workspace_id).await,
        RpcRequest::ItemStop { item_id, .. } => handle_item_stop(ctx, request_id, &item_id).await,
    }
}

fn error(request_id: String, code: &str, message: impl Into<String>) -> RpcResponse {
    RpcResponse::Error { request_id, code: code.to_string(), message: message.into() }
}

fn valid_workspace_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && name.chars().next().is_some_and(char::is_alphanumeric)
}

async fn handle_create(
    ctx: &RpcContext,
    request_id: String,
    workspace_name: String,
    project_source: ProjectSource,
    parent_workspace_id: Option<String>,
) -> RpcResponse {
    if !valid_workspace_name(&workspace_name) {
        return error(request_id, "invalid_argument", "workspace_name must be a non-empty alphanumeric/-/_ string, max 64 bytes");
    }
    {
        let registry = ctx.registry.lock().await;
        if registry.find_workspace_by_name(&workspace_name).is_some() {
            return error(request_id, "conflict", "workspace_name is already registered on this host");
        }
    }

    let outcome = if let Some(parent_id) = parent_workspace_id {
        create_agent_workspace(ctx, &parent_id, &workspace_name, &project_source).await
    } else {
        create_root_workspace(ctx, &workspace_name, &project_source).await
    };

    let (root_path, project_id, project_name) = match outcome {
        Ok(value) => value,
        Err(response) => return response,
    };

    if let Err(zellij_error) = zellij_ops::create_session(&workspace_name, &root_path).await {
        return zellij_error_response(request_id, &zellij_error);
    }

    let workspace_id = format!("ws-{}", uuid::Uuid::new_v4());
    let created_at = now_rfc3339();
    let mut registry = ctx.registry.lock().await;
    match registry.register_workspace(
        workspace_id.clone(),
        workspace_name.clone(),
        ctx.devhost_id.clone(),
        project_id.clone(),
        project_name,
        root_path,
        created_at,
    ) {
        Ok(()) => RpcResponse::WorkspaceCreateOk { request_id, workspace_id, workspace_name, project_id },
        Err(RegistryError::WorkspaceNameTaken(name)) => {
            error(request_id, "conflict", format!("workspace_name {name:?} is already registered on this host"))
        }
        Err(other) => error(request_id, "internal", other.to_string()),
    }
}

async fn create_root_workspace(
    ctx: &RpcContext,
    workspace_name: &str,
    project_source: &ProjectSource,
) -> Result<(PathBuf, String, String), RpcResponse> {
    match project_source {
        ProjectSource::CloneUrl { clone_url } => {
            let dest = ctx.workspaces_dir.join(workspace_name);
            if dest.exists() {
                return Err(error(String::new(), "conflict", "clone destination already exists"));
            }
            std::fs::create_dir_all(&ctx.workspaces_dir).map_err(|e| error(String::new(), "internal", e.to_string()))?;
            jj_ops::clone(clone_url, &dest).await.map_err(|e| jj_error_response(String::new(), &e))?;
            let canonical = jj_ops::canonicalize_prospective(&dest).map_err(|e| jj_error_response(String::new(), &e))?;
            Ok((canonical, format!("proj-{}", uuid::Uuid::new_v4()), workspace_name.to_string()))
        }
        ProjectSource::ExistingPath { existing_path } => {
            let confined = confine_workspaces_dir(ctx, existing_path)?;
            jj_ops::ensure_colocated(&confined).await.map_err(|e| jj_error_response(String::new(), &e))?;
            if let Err(rename_error) = jj_ops::rename_workspace(&confined, workspace_name).await {
                tracing::warn!(%rename_error, "jj workspace rename failed; continuing with jj's own workspace name");
            }
            let registry = ctx.registry.lock().await;
            let (project_id, project_name) = match registry.find_project_by_source(&confined) {
                Some(project) => (project.project_id.clone(), project.name.clone()),
                None => (format!("proj-{}", uuid::Uuid::new_v4()), workspace_name.to_string()),
            };
            Ok((confined, project_id, project_name))
        }
    }
}

async fn create_agent_workspace(
    ctx: &RpcContext,
    parent_workspace_id: &str,
    workspace_name: &str,
    project_source: &ProjectSource,
) -> Result<(PathBuf, String, String), RpcResponse> {
    let ProjectSource::ExistingPath { existing_path } = project_source else {
        return Err(error(
            String::new(),
            "invalid_argument",
            "parent_workspace_id requires project_source to be {existing_path: <not-yet-existing destination>}",
        ));
    };
    let (parent_root, project_id, project_name) = {
        let registry = ctx.registry.lock().await;
        let parent = registry
            .find_workspace(parent_workspace_id)
            .ok_or_else(|| error(String::new(), "not_found", "parent_workspace_id does not name a registered workspace"))?;
        let project = registry
            .find_project(&parent.project_id)
            .ok_or_else(|| error(String::new(), "internal", "parent workspace has no registered project"))?;
        (parent.root_path.clone(), project.project_id.clone(), project.name.clone())
    };

    let dest = confine_workspaces_dir(ctx, existing_path)?;
    if dest.exists() {
        return Err(error(String::new(), "conflict", "agent workspace destination already exists"));
    }
    jj_ops::workspace_add(&parent_root, &dest, workspace_name).await.map_err(|e| jj_error_response(String::new(), &e))?;
    let canonical = jj_ops::canonicalize_prospective(&dest).map_err(|e| jj_error_response(String::new(), &e))?;
    Ok((canonical, project_id, project_name))
}

/// Confines a caller-supplied `existing_path` under `ctx.workspaces_dir`,
/// per `host-rpc.md`'s root-confinement requirement for
/// `project_source.existing_path`.
fn confine_workspaces_dir(ctx: &RpcContext, existing_path: &str) -> Result<PathBuf, RpcResponse> {
    if existing_path.starts_with('/') || existing_path.split('/').any(|s| s == "..") {
        // An absolute path is meaningful here (unlike fs_ops::confine's
        // RPC-relative paths) but MUST still land inside workspaces_dir;
        // reject `..` outright and resolve everything else against the
        // configured root rather than trusting the caller's own prefix.
        return Err(error(String::new(), "out_of_root", "existing_path must not contain '..' segments"));
    }
    let candidate = ctx.workspaces_dir.join(existing_path.trim_start_matches('/'));
    Ok(candidate)
}

async fn handle_list(ctx: &RpcContext, request_id: String) -> RpcResponse {
    let registry = ctx.registry.lock().await;
    let workspaces = registry
        .list_workspaces()
        .iter()
        .map(|w| WorkspaceSummary {
            workspace_id: w.workspace_id.clone(),
            workspace_name: w.workspace_name.clone(),
            devhost_id: w.devhost_id.clone(),
            project_id: w.project_id.clone(),
            created_at: w.created_at.clone(),
        })
        .collect();
    RpcResponse::WorkspaceListOk { request_id, workspaces }
}

async fn handle_status(ctx: &RpcContext, request_id: String, workspace_id: &str) -> RpcResponse {
    let root_path = match lookup_root(ctx, workspace_id).await {
        Ok(path) => path,
        Err(response) => return with_request_id(response, request_id),
    };
    match jj_ops::status(&root_path).await {
        Ok(entries) => {
            let changed = entries
                .into_iter()
                .map(|e| ChangedPath { path: e.path, kind: fs_ops::to_wire_change_kind(e.kind) })
                .collect();
            // Conflict detection is a deliberate M1 gap — see jj_ops.rs's
            // module docs. Always empty here, never guessed.
            RpcResponse::WorkspaceStatusOk { request_id, changed, conflicted: Vec::new() }
        }
        Err(jj_error) => jj_error_response(request_id, &jj_error),
    }
}

async fn handle_tree_list(
    ctx: &RpcContext,
    request_id: String,
    workspace_id: &str,
    path_prefix: &str,
    revision: Option<&str>,
    cursor: Option<&str>,
) -> RpcResponse {
    if let Some(response) = reject_non_live_revision(&request_id, revision) {
        return response;
    }
    let root_path = match lookup_root(ctx, workspace_id).await {
        Ok(path) => path,
        Err(response) => return with_request_id(response, request_id),
    };
    match fs_ops::list_dir(&root_path, path_prefix, cursor, MAX_TREE_LIST_PAGE) {
        Ok((entries, next_cursor)) => RpcResponse::WorkspaceTreeListOk { request_id, entries, next_cursor },
        Err(fs_error) => fs_error_response(request_id, &fs_error),
    }
}

async fn handle_file_read(
    ctx: &RpcContext,
    request_id: String,
    workspace_id: &str,
    path: &str,
    revision: Option<&str>,
    range: Option<choosh_protocol::host_rpc::ByteRange>,
) -> RpcResponse {
    if let Some(response) = reject_non_live_revision(&request_id, revision) {
        return response;
    }
    let root_path = match lookup_root(ctx, workspace_id).await {
        Ok(path) => path,
        Err(response) => return with_request_id(response, request_id),
    };
    let range_tuple = range.map(|r| (r.offset, r.length));
    match fs_ops::read_file_range(&root_path, path, range_tuple, MAX_FILE_READ_RANGE_BYTES) {
        Ok((bytes, total_size)) => {
            use base64::Engine;
            RpcResponse::WorkspaceFileReadOk {
                request_id,
                content_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                total_size,
            }
        }
        Err(fs_error) => fs_error_response(request_id, &fs_error),
    }
}

fn valid_item_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && name.chars().next().is_some_and(char::is_alphanumeric)
}

#[allow(clippy::too_many_arguments)]
async fn handle_item_create(
    ctx: &RpcContext,
    request_id: String,
    workspace_id: &str,
    item_type: ItemType,
    name: String,
    agent: Option<choosh_protocol::host_rpc::AgentKind>,
    command: Option<Vec<String>>,
    port: Option<u16>,
) -> RpcResponse {
    if !valid_item_name(&name) {
        return error(request_id, "invalid_argument", "name must be a non-empty alphanumeric/-/_ string, max 64 bytes");
    }
    let (workspace_name, root_path) = {
        let registry = ctx.registry.lock().await;
        let Some(workspace) = registry.find_workspace(workspace_id) else {
            return error(request_id, "not_found", "workspace_id is not registered on this host");
        };
        if registry.find_item_by_name(workspace_id, &name).is_some() {
            return error(request_id, "conflict", "item name is already registered in this workspace");
        }
        (workspace.workspace_name.clone(), workspace.root_path.clone())
    };

    let initial_command: Vec<String> = match item_type {
        ItemType::AgentTerminal => {
            let Some(agent) = agent else {
                return error(request_id, "invalid_argument", "item_type AgentTerminal requires agent");
            };
            let Some(root_str) = root_path.to_str() else {
                return error(request_id, "internal", "workspace root is not valid UTF-8");
            };
            agent_launch_argv(agent, workspace_id, "pending", root_str)
        }
        ItemType::Shell => Vec::new(),
        ItemType::WebService => {
            let Some(command) = command else {
                return error(request_id, "invalid_argument", "item_type WebService requires command");
            };
            if command.is_empty() {
                return error(request_id, "invalid_argument", "command must not be empty");
            }
            command
        }
    };
    if item_type == ItemType::WebService && port.is_none() {
        return error(request_id, "invalid_argument", "item_type WebService requires port");
    }

    let item_id = format!("item-{}", uuid::Uuid::new_v4());
    // The launched agent's CHOOSH_ITEM_ID must be this item_id, but the
    // item_id doesn't exist until after a successful tab creation — a
    // chicken-and-egg problem inherent to "the ID identifies the tab, but
    // the tab's own launch command needs to know its ID". Resolved by
    // reserving the ID first (it's only ever used as an opaque env var
    // value, never checked against the registry until after this call
    // returns) and rebuilding the launch argv with the real ID before
    // spawning — the throwaway "pending" placeholder above never reaches a
    // real process.
    let initial_command = if item_type == ItemType::AgentTerminal {
        let Some(root_str) = root_path.to_str() else {
            return error(request_id, "internal", "workspace root is not valid UTF-8");
        };
        agent_launch_argv(agent.expect("checked above"), workspace_id, &item_id, root_str)
    } else {
        initial_command
    };

    if let Err(zellij_error) = zellij_ops::new_tab(&workspace_name, &name, &root_path, &initial_command).await {
        return zellij_error_response(request_id, &zellij_error);
    }

    let mut registry = ctx.registry.lock().await;
    match registry.register_item(item_id.clone(), workspace_id.to_string(), item_type, name.clone(), name.clone(), agent, port) {
        Ok(()) => RpcResponse::ItemCreateOk { request_id, item_id, item_type, name: name.clone(), tab_target: name },
        Err(RegistryError::ItemNameTaken(taken)) => {
            error(request_id, "conflict", format!("item name {taken:?} is already registered in this workspace"))
        }
        Err(other) => error(request_id, "internal", other.to_string()),
    }
}

async fn handle_item_list(ctx: &RpcContext, request_id: String, workspace_id: &str) -> RpcResponse {
    let registry = ctx.registry.lock().await;
    if registry.find_workspace(workspace_id).is_none() {
        return error(request_id, "not_found", "workspace_id is not registered on this host");
    }
    let items = registry
        .list_items(workspace_id)
        .into_iter()
        .map(|i| ItemSummary {
            item_id: i.item_id.clone(),
            item_type: i.item_type,
            name: i.name.clone(),
            tab_target: i.tab_target.clone(),
            status: i.status,
            port: i.port,
        })
        .collect();
    RpcResponse::ItemListOk { request_id, items }
}

async fn handle_item_stop(ctx: &RpcContext, request_id: String, item_id: &str) -> RpcResponse {
    let (workspace_name, tab_target) = {
        let registry = ctx.registry.lock().await;
        let Some(item) = registry.find_item(item_id) else {
            return error(request_id, "not_found", "item_id is not registered on this host");
        };
        if item.status == ItemStatus::Stopped {
            return RpcResponse::ItemStopOk { request_id };
        }
        let Some(workspace) = registry.find_workspace(&item.workspace_id) else {
            return error(request_id, "internal", "item references a workspace that no longer exists");
        };
        (workspace.workspace_name.clone(), item.tab_target.clone())
    };

    if let Err(zellij_error) = zellij_ops::close_tab(&workspace_name, &tab_target).await {
        // A stop request is explicit intent; still record the item as
        // stopped even if the underlying tab close is best-effort (the
        // process may have already exited on its own, per host-rpc.md's
        // item.stop stopping "the underlying process", not asserting it
        // was still running) — but a hard Zellij spawn failure (the binary
        // itself couldn't run) is still surfaced, not silently swallowed.
        if matches!(zellij_error, ZellijError::Spawn(_)) {
            return zellij_error_response(request_id, &zellij_error);
        }
    }

    let mut registry = ctx.registry.lock().await;
    match registry.mark_item_stopped(item_id) {
        Ok(()) => RpcResponse::ItemStopOk { request_id },
        Err(other) => error(request_id, "internal", other.to_string()),
    }
}

/// M1 only reads the live working copy — a non-`@`/`None` `revision`
/// would need `jj-lib`'s content-addressed store, which is out of scope
/// here (see `jj_ops.rs`'s module docs), so it's rejected cleanly rather
/// than silently served against `@` instead.
fn reject_non_live_revision(request_id: &str, revision: Option<&str>) -> Option<RpcResponse> {
    match revision {
        None | Some("@") => None,
        Some(_) => Some(error(
            request_id.to_string(),
            "invalid_argument",
            "only the live working copy (revision omitted or \"@\") is supported in this increment",
        )),
    }
}

async fn lookup_root(ctx: &RpcContext, workspace_id: &str) -> Result<PathBuf, RpcResponse> {
    let registry = ctx.registry.lock().await;
    registry
        .find_workspace(workspace_id)
        .map(|w| w.root_path.clone())
        .ok_or_else(|| error(String::new(), "not_found", "workspace_id is not registered on this host"))
}

fn with_request_id(response: RpcResponse, request_id: String) -> RpcResponse {
    match response {
        RpcResponse::Error { code, message, .. } => RpcResponse::Error { request_id, code, message },
        other => other,
    }
}

fn zellij_error_response(request_id: String, cause: &ZellijError) -> RpcResponse {
    error(request_id, "internal", format!("zellij session creation failed: {cause}"))
}

fn jj_error_response(request_id: String, cause: &JjError) -> RpcResponse {
    error(request_id, "internal", format!("jj operation failed: {cause}"))
}

fn fs_error_response(request_id: String, cause: &FsError) -> RpcResponse {
    let code = match cause {
        FsError::OutOfRoot => "out_of_root",
        FsError::NotFound | FsError::NotADirectory | FsError::NotAFile => "not_found",
        FsError::BoundExceeded(_) => "bound_exceeded",
        FsError::Io(_) => "internal",
    };
    error(request_id, code, cause.to_string())
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use choosh_protocol::host_rpc::ByteRange;
    use std::path::Path;

    fn ctx_with_tempdir() -> (tempfile::TempDir, RpcContext) {
        let dir = tempfile::tempdir().unwrap();
        let workspaces_dir = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspaces_dir).unwrap();
        let registry_path = dir.path().join("registry.json");
        let ctx = RpcContext {
            registry: Mutex::new(Registry::load(&registry_path).unwrap()),
            devhost_id: "dev-1".to_string(),
            workspaces_dir,
        };
        (dir, ctx)
    }

    fn init_git_repo(dir: &Path) {
        std::process::Command::new("git").arg("init").arg("-q").current_dir(dir).status().unwrap();
        std::process::Command::new("git").args(["config", "user.email", "a@b.c"]).current_dir(dir).status().unwrap();
        std::process::Command::new("git").args(["config", "user.name", "a"]).current_dir(dir).status().unwrap();
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        std::process::Command::new("git").args(["add", "a.txt"]).current_dir(dir).status().unwrap();
        std::process::Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(dir).status().unwrap();
    }

    #[tokio::test]
    async fn full_create_list_status_cycle_against_a_real_repo() {
        // Zellij's session namespace is global to the machine, not scoped
        // per test — Rust's test harness runs these functions in parallel,
        // so a name shared across tests (this used to be a literal "app")
        // races with whichever other test also happens to be using it,
        // corrupting both. A per-test unique name, matching the pattern
        // `zellij_ops`'s own tests already use, is a hard requirement here.
        let name = format!("app-{}", uuid::Uuid::new_v4());
        let (_dir, ctx) = ctx_with_tempdir();
        let existing = ctx.workspaces_dir.join(&name);
        std::fs::create_dir_all(&existing).unwrap();
        init_git_repo(&existing);

        let create_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceCreate {
                request_id: "r1".to_string(),
                devhost_id: "dev-1".to_string(),
                workspace_name: name.clone(),
                project_source: ProjectSource::ExistingPath { existing_path: name.clone() },
                parent_workspace_id: None,
            },
        )
        .await;
        let RpcResponse::WorkspaceCreateOk { workspace_id, .. } = create_response else {
            panic!("expected WorkspaceCreateOk, got {create_response:?}");
        };

        let list_response = dispatch(&ctx, RpcRequest::WorkspaceList { request_id: "r2".to_string() }).await;
        let RpcResponse::WorkspaceListOk { workspaces, .. } = list_response else {
            panic!("expected WorkspaceListOk, got {list_response:?}");
        };
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].workspace_id, workspace_id);

        std::fs::write(existing.join("new.txt"), "content").unwrap();
        let status_response =
            dispatch(&ctx, RpcRequest::WorkspaceStatus { request_id: "r3".to_string(), workspace_id: workspace_id.clone() }).await;
        let RpcResponse::WorkspaceStatusOk { changed, .. } = status_response else {
            panic!("expected WorkspaceStatusOk, got {status_response:?}");
        };
        assert!(changed.iter().any(|c| c.path == "new.txt"));

        let tree_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceTreeList {
                request_id: "r4".to_string(),
                workspace_id: workspace_id.clone(),
                path_prefix: String::new(),
                revision: None,
                cursor: None,
            },
        )
        .await;
        let RpcResponse::WorkspaceTreeListOk { entries, .. } = tree_response else {
            panic!("expected WorkspaceTreeListOk, got {tree_response:?}");
        };
        assert!(entries.iter().any(|e| e.name == "a.txt"));

        let file_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceFileRead {
                request_id: "r5".to_string(),
                workspace_id,
                path: "a.txt".to_string(),
                revision: None,
                range: None,
            },
        )
        .await;
        let RpcResponse::WorkspaceFileReadOk { content_base64, .. } = file_response else {
            panic!("expected WorkspaceFileReadOk, got {file_response:?}");
        };
        let bytes = base64::engine::general_purpose::STANDARD.decode(content_base64).unwrap();
        assert_eq!(bytes, b"hello\n");

        zellij_ops::kill_session(&name).await.ok();
    }

    #[tokio::test]
    async fn duplicate_workspace_name_is_a_conflict() {
        let name = format!("app-{}", uuid::Uuid::new_v4());
        let (_dir, ctx) = ctx_with_tempdir();
        let existing = ctx.workspaces_dir.join(&name);
        std::fs::create_dir_all(&existing).unwrap();
        init_git_repo(&existing);

        let request = || RpcRequest::WorkspaceCreate {
            request_id: "r".to_string(),
            devhost_id: "dev-1".to_string(),
            workspace_name: name.clone(),
            project_source: ProjectSource::ExistingPath { existing_path: name.clone() },
            parent_workspace_id: None,
        };
        dispatch(&ctx, request()).await;
        let second = dispatch(&ctx, request()).await;
        assert!(matches!(second, RpcResponse::Error { code, .. } if code == "conflict"));

        zellij_ops::kill_session(&name).await.ok();
    }

    #[tokio::test]
    async fn tree_list_rejects_a_root_escape_attempt() {
        let name = format!("escapetest-{}", uuid::Uuid::new_v4());
        let (_dir, ctx) = ctx_with_tempdir();
        let existing = ctx.workspaces_dir.join(&name);
        std::fs::create_dir_all(&existing).unwrap();
        init_git_repo(&existing);
        let create_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceCreate {
                request_id: "r1".to_string(),
                devhost_id: "dev-1".to_string(),
                workspace_name: name.clone(),
                project_source: ProjectSource::ExistingPath { existing_path: name.clone() },
                parent_workspace_id: None,
            },
        )
        .await;
        let RpcResponse::WorkspaceCreateOk { workspace_id, .. } = &create_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("setup failed: {create_response:?}");
        };

        let response = dispatch(
            &ctx,
            RpcRequest::WorkspaceFileRead {
                request_id: "r2".to_string(),
                workspace_id: workspace_id.clone(),
                path: "../../../etc/passwd".to_string(),
                revision: None,
                range: None,
            },
        )
        .await;
        let out_of_root = matches!(response, RpcResponse::Error { code, .. } if code == "out_of_root");

        zellij_ops::kill_session(&name).await.ok();
        assert!(out_of_root);
    }

    #[tokio::test]
    async fn bound_exceeded_range_is_rejected_cleanly() {
        let name = format!("boundtest-{}", uuid::Uuid::new_v4());
        let (_dir, ctx) = ctx_with_tempdir();
        let existing = ctx.workspaces_dir.join(&name);
        std::fs::create_dir_all(&existing).unwrap();
        init_git_repo(&existing);
        let create_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceCreate {
                request_id: "r1".to_string(),
                devhost_id: "dev-1".to_string(),
                workspace_name: name.clone(),
                project_source: ProjectSource::ExistingPath { existing_path: name.clone() },
                parent_workspace_id: None,
            },
        )
        .await;
        let RpcResponse::WorkspaceCreateOk { workspace_id, .. } = &create_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("setup failed: {create_response:?}");
        };

        let response = dispatch(
            &ctx,
            RpcRequest::WorkspaceFileRead {
                request_id: "r2".to_string(),
                workspace_id: workspace_id.clone(),
                path: "a.txt".to_string(),
                revision: None,
                range: Some(ByteRange { offset: 0, length: MAX_FILE_READ_RANGE_BYTES + 1 }),
            },
        )
        .await;
        let bound_exceeded = matches!(response, RpcResponse::Error { code, .. } if code == "bound_exceeded");

        zellij_ops::kill_session(&name).await.ok();
        assert!(bound_exceeded);
    }
}
