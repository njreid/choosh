//! Dispatches every `RpcRequest` variant — `host-rpc.md`'s original
//! registry/item RPCs (M1) plus the `workspace.diff`/`workspace.log`/
//! `workspace.op.*` (M3) and `workspace.file.write` (M4) additions
//! `jj-integration.md` describes — against the registry, `jj`, Zellij, and
//! root-confined filesystem layers. Deliberately a plain function over
//! already-decoded request/response types — the tunnel/frame plumbing that
//! gets bytes here lives in `serve.rs`, kept separate so this dispatch
//! logic is testable without any WebSocket machinery, the same split
//! `choosh-relayd`'s enroll handling uses.

use std::path::PathBuf;
use std::sync::Arc;

use choosh_protocol::host_rpc::{
    ChangeGraphNode, ChangedPath, DiffFileEntry, DiffHunk, DiffSegment, DiffSegmentKind, ItemStatus, ItemSummary, ItemType,
    MAX_FILE_READ_RANGE_BYTES, MAX_TREE_LIST_PAGE, OperationLogEntry, ProjectSource, ProjectSummary, RpcRequest, RpcResponse,
    WorkspaceSummary,
};
use tokio::sync::Mutex;

use crate::agent_launch::agent_launch_argv;
use crate::fs_ops::{self, FsError};
use crate::jj_ops::{self, JjError};
use crate::mise_ops::{self, MiseError};
use crate::readiness;
use crate::registry::{Registry, RegistryError};
use crate::zellij_ops::{self, ZellijError};

pub struct RpcContext {
    /// `Arc`-wrapped (not a bare `Mutex<Registry>`) specifically so the
    /// `WebService` readiness prober (`crate::readiness`) can hold an
    /// owned, `'static` handle to it — a plain `&RpcContext` borrow, as
    /// `dispatch` itself takes, isn't `'static` and can't be captured by a
    /// `tokio::spawn`ed task that needs to keep running after `dispatch`
    /// returns.
    pub registry: Arc<Mutex<Registry>>,
    pub devhost_id: String,
    /// Root confinement boundary for every `workspace.create` destination
    /// (both a fresh clone and an adopted `existing_path`) — per
    /// `host-rpc.md`'s "root-confined under the devhost's configured
    /// workspaces directory".
    pub workspaces_dir: PathBuf,
    /// The `mise` binary this context's project-pinned toolchain resolution
    /// (`mise_ops::ensure_project_toolchain`/`project_env`) invokes — plumbed
    /// through rather than read from the environment at each call site so
    /// tests can point it at a fixture, the same reason `SshBridgeConfig`
    /// carries its own `mise_bin` (see `serve.rs`'s `mise_ops::mise_bin_from_env`
    /// call site).
    pub mise_bin: String,
    /// `choosh-hostd`-owned directory for Project-pinned `mise` installed-
    /// tool payloads (`mise_ops`'s doc comment) — shared across every
    /// workspace on this host, but never the same tree
    /// `SshBridgeConfig::mise_host_tools_dir` (host-managed tools) uses, per
    /// toolchain-provisioning.md's tier-isolation requirement.
    pub mise_project_tools_dir: PathBuf,
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
        RpcRequest::WorkspaceDiff { workspace_id, from, to, .. } => {
            handle_diff(ctx, request_id, &workspace_id, from.as_deref(), to.as_deref()).await
        }
        RpcRequest::WorkspaceLog { workspace_id, revset, limit, .. } => {
            handle_log(ctx, request_id, &workspace_id, revset.as_deref(), limit).await
        }
        RpcRequest::WorkspaceOpLog { workspace_id, limit, .. } => handle_op_log(ctx, request_id, &workspace_id, limit).await,
        RpcRequest::WorkspaceOpUndo { workspace_id, op_id, .. } => handle_op_undo(ctx, request_id, &workspace_id, &op_id).await,
        RpcRequest::WorkspaceOpRestore { workspace_id, op_id, .. } => {
            handle_op_restore(ctx, request_id, &workspace_id, &op_id).await
        }
        RpcRequest::WorkspaceFileWrite { workspace_id, path, base_revision, content_base64, .. } => {
            handle_file_write(ctx, request_id, &workspace_id, &path, &base_revision, &content_base64).await
        }
        RpcRequest::ProjectList { .. } => handle_project_list(ctx, request_id).await,
        RpcRequest::ProjectSetPrimaryWorkspace { project_id, workspace_id, .. } => {
            handle_project_set_primary_workspace(ctx, request_id, &project_id, &workspace_id).await
        }
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

    // Reserves `workspace_name` atomically, *before* any of this request's
    // expensive real work (clone/`jj workspace add`, Zellij session
    // creation) starts — closing a real race the old code had: an early,
    // non-authoritative `find_workspace_by_name` read here (with the lock
    // then released) let two concurrent `workspace.create`s for the same
    // name both pass, both do the expensive work, and only one win the
    // *authoritative* uniqueness check that used to run only at the very
    // end, inside `Registry::register_workspace` — orphaning the loser's
    // clone/session with no cleanup. See `Registry::reserve_workspace_name`'s
    // doc comment for the full mechanism.
    {
        let mut registry = ctx.registry.lock().await;
        if let Err(RegistryError::WorkspaceNameTaken(name)) = registry.reserve_workspace_name(&workspace_name) {
            return error(request_id, "conflict", format!("workspace_name {name:?} is already registered on this host"));
        }
    }

    let response = handle_create_after_reservation(ctx, request_id, workspace_name.clone(), project_source, parent_workspace_id).await;

    // Released unconditionally, on every path: on success `register_workspace`
    // has already promoted this name to a real registered entry (so this is
    // just bookkeeping), and on every failure path this is what actually
    // frees the name back up for a future `workspace.create` — never left
    // held forever by a failed request.
    ctx.registry.lock().await.release_workspace_name_reservation(&workspace_name);

    response
}

/// The rest of `handle_create`, run only once `workspace_name` is held
/// under an exclusive reservation (see `handle_create`'s own comment) — kept
/// as a separate function so `handle_create` can guarantee the reservation
/// is released on every one of this function's return paths without each
/// one having to remember to do it itself.
async fn handle_create_after_reservation(
    ctx: &RpcContext,
    request_id: String,
    workspace_name: String,
    project_source: ProjectSource,
    parent_workspace_id: Option<String>,
) -> RpcResponse {
    let outcome = if let Some(parent_id) = parent_workspace_id {
        create_agent_workspace(ctx, &parent_id, &workspace_name, &project_source).await
    } else {
        create_root_workspace(ctx, &workspace_name, &project_source).await
    };

    let (root_path, project_id, project_name) = match outcome {
        Ok(value) => value,
        // `create_root_workspace`/`create_agent_workspace` (and
        // `confine_workspaces_dir`, which both call) build every `Err`
        // response with an empty placeholder `request_id` — they have no
        // way to know the real one at their own call sites deep inside a
        // helper — so it MUST be filled in here before this reaches the
        // caller, the same way every other early-return error path in this
        // module (`lookup_root`'s callers, etc.) uses `with_request_id`.
        Err(response) => return with_request_id(response, request_id),
    };

    // Project-pinned toolchains (toolchain-provisioning.md's first tier):
    // if this workspace's root declares a `mise.toml`, `mise install` MUST
    // succeed before the workspace is reported ready. Run before the
    // Zellij session is created and before anything is registered, so a
    // failure here (a real, plausible one — a typo'd tool name, a network
    // hiccup, an unresolvable version) leaves nothing partially created:
    // no Zellij session, no registry entry — this whole `workspace.create`
    // fails cleanly rather than reporting a workspace that's up but
    // missing its pinned toolchain. `internal` rather than
    // `invalid_argument`: the failing input is the *Project's* own
    // `mise.toml` content, not anything the RPC caller supplied in this
    // request, so `invalid_argument` (about this request's own arguments)
    // would misattribute the failure; `internal` reflects "the host
    // couldn't complete this operation" without implying the caller did
    // anything wrong.
    if mise_ops::has_mise_toml(&root_path)
        && let Err(mise_error) = mise_ops::ensure_project_toolchain(&ctx.mise_bin, &root_path, &ctx.mise_project_tools_dir).await
    {
        tracing::warn!(%mise_error, workspace_name, "mise install failed for a workspace's mise.toml; failing workspace.create");
        return mise_error_response(request_id, &mise_error);
    }

    if let Err(zellij_error) = zellij_ops::create_session(&workspace_name, &root_path).await {
        return zellij_error_response(request_id, &zellij_error);
    }

    let workspace_id = format!("ws-{}", uuid::Uuid::new_v4());
    let created_at = now_rfc3339();
    let mut registry = ctx.registry.lock().await;
    match registry.register_workspace(
        workspace_id.clone(),
        workspace_name.clone(),
        project_id.clone(),
        project_name,
        root_path,
        created_at,
    ) {
        Ok(()) => RpcResponse::WorkspaceCreateOk { request_id, workspace_id, workspace_name, project_id },
        Err(RegistryError::WorkspaceNameTaken(name)) => {
            // Should be unreachable in production: `workspace_name` was
            // held under an exclusive reservation (`handle_create`) for
            // this entire function's duration, so no concurrent
            // `workspace.create` could have registered it out from under
            // us. Kept as real defense-in-depth rather than an
            // `unwrap`/`assert!` — and, unlike the old code's equivalent
            // branch, actually cleans up the Zellij session this call just
            // created rather than orphaning it, on the chance this is ever
            // reached (e.g. a future direct `register_workspace` caller
            // that bypasses the reservation).
            drop(registry);
            zellij_ops::kill_session(&workspace_name).await.ok();
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
    jj_ops::workspace_add(&parent_root, &dest, workspace_name, None).await.map_err(|e| jj_error_response(String::new(), &e))?;
    let canonical = jj_ops::canonicalize_prospective(&dest).map_err(|e| jj_error_response(String::new(), &e))?;
    Ok((canonical, project_id, project_name))
}

/// Confines a caller-supplied `existing_path` under `ctx.workspaces_dir`,
/// per `host-rpc.md`'s root-confinement requirement for
/// `project_source.existing_path`. Canonicalizes the resolved path and
/// verifies it's actually under `ctx.workspaces_dir`'s own canonical form —
/// so a symlink planted under `workspaces_dir` that points elsewhere is
/// rejected, not silently followed (this is exactly the check `fs_ops::confine`
/// performs for every other root-confined path in this crate; see that
/// function's own doc comment and `confine_rejects_a_symlink_escaping_the_root`
/// for the same guarantee proven against it directly).
fn confine_workspaces_dir(ctx: &RpcContext, existing_path: &str) -> Result<PathBuf, RpcResponse> {
    if existing_path.starts_with('/') || existing_path.split('/').any(|s| s == "..") {
        // An absolute path is meaningful here (unlike fs_ops::confine's
        // RPC-relative paths, which reject a leading '/' outright) but MUST
        // still land inside workspaces_dir; reject `..` outright and resolve
        // everything else against the configured root rather than trusting
        // the caller's own prefix.
        return Err(error(String::new(), "out_of_root", "existing_path must not contain '..' segments"));
    }
    // Reuses fs_ops::confine's canonicalize-and-verify logic rather than
    // reimplementing it a second time: strip the leading '/' this
    // function alone accepts (already proven `..`-free above), then hand
    // the rest to the same symlink-safe resolution every other
    // root-confined path in this crate goes through. Works unchanged for
    // both of confine_workspaces_dir's two callers: an existing_path that
    // must already exist (create_root_workspace's adopt-existing-repo
    // case) and one that must NOT yet exist (create_agent_workspace's
    // fresh `jj workspace add` destination) — fs_ops::confine only
    // requires the parent to exist and resolve under the root; a leaf that
    // doesn't exist yet is accepted with nothing left to canonicalize.
    let relative = existing_path.trim_start_matches('/');
    fs_ops::confine(&ctx.workspaces_dir, relative).map_err(|fs_error| fs_error_response(String::new(), &fs_error))
}

async fn handle_list(ctx: &RpcContext, request_id: String) -> RpcResponse {
    let registry = ctx.registry.lock().await;
    let workspaces = registry
        .list_workspaces()
        .iter()
        .map(|w| WorkspaceSummary {
            workspace_id: w.workspace_id.clone(),
            workspace_name: w.workspace_name.clone(),
            // `ctx.devhost_id`, not a value stored on `w` at creation time:
            // every workspace this registry lists belongs to this one
            // process's own devhost, and `ctx.devhost_id` is this
            // connection's current, relayd-assigned identity. A workspace
            // never legitimately outlives the devhost's own enrollment, but
            // relayd's registry is in-memory-only (a restart forgets every
            // enrolled device, PLAN.md's documented accepted risk) — so a
            // long-lived devhost process routinely re-enrolls under a new
            // device_id over its lifetime. A value captured once at
            // `workspace.create` time would go stale on every one of those
            // re-enrollments, breaking every tunnel-based RPC a phone makes
            // against that workspace with `not_permitted` (the phone would
            // be targeting a `target_device_id` relayd no longer
            // recognizes) — confirmed the hard way, driving a real
            // multi-hunk `jj diff` through a real device.
            devhost_id: ctx.devhost_id.clone(),
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
        Ok((bytes, total_size, revision)) => {
            use base64::Engine;
            RpcResponse::WorkspaceFileReadOk {
                request_id,
                content_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                total_size,
                revision,
            }
        }
        Err(fs_error) => fs_error_response(request_id, &fs_error),
    }
}

/// `workspace.file.write` (`docs/milestones/M4-editing.md`,
/// `jj-integration.md`'s `workspace.file.write` section). No `jj`
/// invocation happens here — per `jj-integration.md`, "jj snapshots the new
/// working-copy state automatically" the next time anything invokes `jj`
/// against this workspace, so writing the bytes to `@`'s checkout is the
/// entire effect this RPC needs to produce (verified against the real `jj`
/// binary in this module's tests below, not just trusted from the spec's
/// prose).
///
/// V1 scope note (mirrors `RpcRequest::WorkspaceFileWrite`'s doc comment):
/// a write always targets a path that already exists — it always follows a
/// prior `workspace.file.read` of that same path, per `jj-integration.md`
/// ("`base_revision` MUST be the revision the client last read `path`
/// at"). A `path` that doesn't yet exist maps to `not_found` via
/// [`fs_ops::FsError::NotFound`], the same as every other path-bearing RPC;
/// creating a brand-new file is not a case this RPC is asked to support.
async fn handle_file_write(
    ctx: &RpcContext,
    request_id: String,
    workspace_id: &str,
    path: &str,
    base_revision: &str,
    content_base64: &str,
) -> RpcResponse {
    use base64::Engine;
    let root_path = match lookup_root(ctx, workspace_id).await {
        Ok(path) => path,
        Err(response) => return with_request_id(response, request_id),
    };
    let Ok(content) = base64::engine::general_purpose::STANDARD.decode(content_base64) else {
        return error(request_id, "invalid_argument", "content_base64 is not valid base64");
    };
    match fs_ops::write_file(&root_path, path, base_revision, &content, MAX_FILE_READ_RANGE_BYTES) {
        Ok(fs_ops::WriteOutcome::Written { new_revision }) => RpcResponse::WorkspaceFileWriteOk { request_id, revision: new_revision },
        Ok(fs_ops::WriteOutcome::Stale { current_revision, current_content }) => RpcResponse::WorkspaceFileWriteStale {
            request_id,
            current_revision,
            current_content_base64: base64::engine::general_purpose::STANDARD.encode(current_content),
        },
        Err(fs_error) => fs_error_response(request_id, &fs_error),
    }
}

/// `handle_project_list`'s "recent event" bound — see that function's doc
/// comment for the full reasoning. Five minutes: long enough to survive a
/// normal reconnect/relay hiccup without a Project's active state
/// flickering off (per PLAN.md's noted `agent-events.md` gap, a reconnect
/// resumes the live stream rather than replaying any gap, so a too-short
/// window would be actively misleading around an ordinary brief
/// disconnect) while still being short enough that "active" means
/// "something happened recently", not "sometime today". Not pinned by
/// `host-rpc.md`/`android-navigation.md` — both explicitly leave this
/// bound as a `hostd`-side implementation choice.
const PROJECT_ACTIVE_EVENT_WINDOW: time::Duration = time::Duration::minutes(5);

/// `host-rpc.md`'s `project.list`: every registered Project on this host,
/// with `active` computed here — never by the client, per `host-rpc.md`:
/// "hostd computes it, the client does not".
///
/// `active` definition (a bounded, defensible choice; `android-navigation.md`
/// deliberately leaves "the exact staleness bound" as "an implementation
/// choice, not a wire contract"): a Project counts as active if any of its
/// Workspaces has —
///
/// 1. a **connected agent/service Item**: a registered `AgentTerminal` or
///    `WebService` item (the "agent"/"service" halves of the phrase, per
///    `host-rpc.md`'s Item RPCs section — `Shell` is neither) whose status
///    is `Running` or `Starting`. `Starting` counts as "connected" too: a
///    `WebService` mid-launch isn't yet probeable, but it's unambiguously
///    live work, not idle — `Stopped`/`Failed`/`Unknown` are the only
///    statuses that mean "no live process" for this purpose; or
/// 2. a **recent event**: [`Registry::has_recent_event`] within
///    [`PROJECT_ACTIVE_EVENT_WINDOW`] of now, fed by every `WireAgentEvent`
///    that carries a `workspace_id` — `serve.rs`'s `serve_dispatch` records
///    one the moment it's about to forward the event onward (see that
///    module's `agent_event_rx.recv()` branch for the exact call site).
async fn handle_project_list(ctx: &RpcContext, request_id: String) -> RpcResponse {
    let now = time::OffsetDateTime::now_utc();
    let registry = ctx.registry.lock().await;
    let projects = registry
        .list_projects()
        .iter()
        .map(|project| {
            let active = registry.list_workspaces().iter().filter(|workspace| workspace.project_id == project.project_id).any(
                |workspace| {
                    registry.list_items(&workspace.workspace_id).into_iter().any(|item| {
                        matches!(item.item_type, ItemType::AgentTerminal | ItemType::WebService)
                            && matches!(item.status, ItemStatus::Running | ItemStatus::Starting)
                    }) || registry.has_recent_event(&workspace.workspace_id, now, PROJECT_ACTIVE_EVENT_WINDOW)
                },
            );
            ProjectSummary {
                project_id: project.project_id.clone(),
                name: project.name.clone(),
                primary_workspace_id: project.primary_workspace_id.clone(),
                active,
            }
        })
        .collect();
    RpcResponse::ProjectListOk { request_id, projects }
}

/// `host-rpc.md`'s `project.set_primary_workspace`: `workspace_id` "MUST
/// already belong to `project_id`" — rejected as a real, typed error rather
/// than silently accepted-then-wrong or silently no-op'd.
///
/// Error code choice, documented per this task's own ask: an unknown
/// `workspace_id`/`project_id` (`RegistryError::WorkspaceNotFound`/
/// `ProjectNotFound`) maps to `not_found`, matching every other unknown-id
/// RPC error in this module (see e.g. `lookup_root`'s callers). A
/// `workspace_id` that resolves fine but names a Workspace under a
/// *different* Project (`RegistryError::WorkspaceNotInProject`) maps to
/// `invalid_argument` instead — the id itself is real, but this request's
/// particular `project_id`/`workspace_id` combination is what's invalid,
/// the same category `handle_item_create`'s argument-validation failures
/// already use in this module.
async fn handle_project_set_primary_workspace(
    ctx: &RpcContext,
    request_id: String,
    project_id: &str,
    workspace_id: &str,
) -> RpcResponse {
    let mut registry = ctx.registry.lock().await;
    match registry.set_primary_workspace(project_id, workspace_id) {
        Ok(()) => RpcResponse::ProjectSetPrimaryWorkspaceOk { request_id },
        Err(RegistryError::WorkspaceNotFound(_)) => error(request_id, "not_found", "workspace_id is not registered on this host"),
        Err(RegistryError::ProjectNotFound(_)) => error(request_id, "not_found", "project_id is not registered on this host"),
        Err(RegistryError::WorkspaceNotInProject { .. }) => {
            error(request_id, "invalid_argument", "workspace_id does not belong to project_id")
        }
        Err(other) => error(request_id, "internal", other.to_string()),
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

    // Reserves `(workspace_id, name)` atomically, before any of this
    // request's expensive real work (Zellij tab creation) starts — the
    // `item.create` analog of `handle_create`'s own reservation; see
    // `Registry::reserve_item_name`'s doc comment for the race this closes.
    let (workspace_name, root_path) = {
        let mut registry = ctx.registry.lock().await;
        let Some(workspace) = registry.find_workspace(workspace_id) else {
            return error(request_id, "not_found", "workspace_id is not registered on this host");
        };
        let workspace_name = workspace.workspace_name.clone();
        let root_path = workspace.root_path.clone();
        if let Err(RegistryError::ItemNameTaken(taken)) = registry.reserve_item_name(workspace_id, &name) {
            return error(request_id, "conflict", format!("item name {taken:?} is already registered in this workspace"));
        }
        (workspace_name, root_path)
    };

    let response =
        handle_item_create_after_reservation(ctx, request_id, workspace_id, item_type, name.clone(), agent, command, port, workspace_name, root_path)
            .await;

    // Released unconditionally, same reasoning as `handle_create`'s own
    // release call: frees the name back up on every failure path, and is
    // just bookkeeping on the success path where `register_item` already
    // promoted it to a real registered entry.
    ctx.registry.lock().await.release_item_name_reservation(workspace_id, &name);

    response
}

/// The rest of `handle_item_create`, run only once `(workspace_id, name)` is
/// held under an exclusive reservation — see `handle_item_create`'s own
/// comment and `handle_create_after_reservation`'s matching doc comment for
/// why this is split out.
#[allow(clippy::too_many_arguments)]
async fn handle_item_create_after_reservation(
    ctx: &RpcContext,
    request_id: String,
    workspace_id: &str,
    item_type: ItemType,
    name: String,
    agent: Option<choosh_protocol::host_rpc::AgentKind>,
    command: Option<Vec<String>>,
    port: Option<u16>,
    workspace_name: String,
    root_path: PathBuf,
) -> RpcResponse {
    // Project-pinned toolchains, second half (see `handle_create`'s own
    // mise-related comment for the first half): resolve this workspace's
    // `mise env` fresh for *this* spawn, per toolchain-provisioning.md
    // ("Every agent, service, and shell process... MUST have `mise env`...
    // injected"). Empty for a workspace with no `mise.toml` — every
    // `initial_command` builder below treats an empty `mise_env` as "add
    // nothing", so this item type's argv is byte-for-byte what it was
    // before project-pinned toolchains existed. `mise install` already ran
    // at `workspace.create` time, so this is just a (fast, no-network)
    // resolve — a failure here is rare enough (e.g. an installed tool
    // payload disappeared out from under `choosh-hostd` between workspace
    // creation and now) that failing this specific `item.create` is the
    // right, narrow blast radius rather than silently spawning a process
    // missing its pinned toolchain's environment.
    let mise_env = if mise_ops::has_mise_toml(&root_path) {
        match mise_ops::project_env(&ctx.mise_bin, &root_path, &ctx.mise_project_tools_dir).await {
            Ok(env) => env,
            Err(mise_error) => return mise_error_response(request_id, &mise_error),
        }
    } else {
        Vec::new()
    };

    // For `AgentTerminal`, only validate `agent` here — the real argv needs
    // this call's freshly reserved `item_id` (see the comment below), so
    // building it twice (once with a throwaway placeholder ID, once for
    // real) would just be wasted work with the exact same validation
    // already done here.
    let initial_command: Vec<String> = match item_type {
        ItemType::AgentTerminal => {
            if agent.is_none() {
                return error(request_id, "invalid_argument", "item_type AgentTerminal requires agent");
            }
            Vec::new()
        }
        ItemType::Shell => shell_launch_argv(&mise_env),
        ItemType::WebService => {
            let Some(command) = command else {
                return error(request_id, "invalid_argument", "item_type WebService requires command");
            };
            if command.is_empty() {
                return error(request_id, "invalid_argument", "command must not be empty");
            }
            prefix_with_mise_env(&mise_env, command)
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
    // returns) and building the real launch argv with it here, the only
    // place `AgentTerminal`'s `initial_command` is ever actually built.
    let initial_command = if item_type == ItemType::AgentTerminal {
        let Some(root_str) = root_path.to_str() else {
            return error(request_id, "internal", "workspace root is not valid UTF-8");
        };
        agent_launch_argv(agent.expect("checked above"), workspace_id, &item_id, root_str, &mise_env)
    } else {
        initial_command
    };

    if let Err(zellij_error) = zellij_ops::new_tab(&workspace_name, &name, &root_path, &initial_command).await {
        return zellij_error_response(request_id, &zellij_error);
    }

    // `WebService` items start `starting` and get a real readiness prober
    // (below); every other item type's Zellij tab already exists
    // synchronously by this point, so `running` is immediately accurate
    // for them, same as before this change — see `service-tunnels.md`'s
    // Lifecycle section and `registry::Registry::register_item`'s doc
    // comment.
    let initial_status = if item_type == ItemType::WebService { ItemStatus::Starting } else { ItemStatus::Running };

    let registry_handle = Arc::clone(&ctx.registry);
    let mut registry = ctx.registry.lock().await;
    match registry.register_item(item_id.clone(), workspace_id.to_string(), item_type, name.clone(), name.clone(), agent, port, initial_status)
    {
        Ok(()) => {
            if item_type == ItemType::WebService {
                // `port` is guaranteed `Some` here — checked above before
                // any of this function's side effects began.
                let port = port.expect("WebService port validated earlier in this function");
                readiness::spawn(registry_handle, item_id.clone(), workspace_name, name.clone(), port);
            }
            RpcResponse::ItemCreateOk { request_id, item_id, item_type, name: name.clone(), tab_target: name }
        }
        Err(RegistryError::ItemNameTaken(taken)) => {
            // Should be unreachable in production for the same reason
            // `handle_create_after_reservation`'s analogous branch is:
            // `(workspace_id, name)` was held under an exclusive
            // reservation (`handle_item_create`) for this entire function's
            // duration. Kept as real defense-in-depth: clean up the Zellij
            // tab this call just created rather than orphaning it.
            drop(registry);
            zellij_ops::close_tab(&workspace_name, &name).await.ok();
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

fn to_wire_diff_segment_kind(kind: jj_ops::DiffSegmentKind) -> DiffSegmentKind {
    match kind {
        jj_ops::DiffSegmentKind::Context => DiffSegmentKind::Context,
        jj_ops::DiffSegmentKind::Added => DiffSegmentKind::Added,
        jj_ops::DiffSegmentKind::Removed => DiffSegmentKind::Removed,
    }
}

fn to_wire_diff_hunk(hunk: jj_ops::DiffHunk) -> DiffHunk {
    DiffHunk {
        old_start: hunk.old_start,
        old_lines: hunk.old_lines,
        new_start: hunk.new_start,
        new_lines: hunk.new_lines,
        segments: hunk
            .segments
            .into_iter()
            .map(|s| DiffSegment { kind: to_wire_diff_segment_kind(s.kind), text: s.text })
            .collect(),
    }
}

fn to_wire_diff_file_entry(entry: jj_ops::DiffFileEntry) -> DiffFileEntry {
    match entry {
        jj_ops::DiffFileEntry::Hunks { old_path, new_path, hunks } => {
            DiffFileEntry::Hunks { old_path, new_path, hunks: hunks.into_iter().map(to_wire_diff_hunk).collect() }
        }
        jj_ops::DiffFileEntry::Binary { path, status, byte_size } => {
            DiffFileEntry::Binary { path, status: fs_ops::to_wire_change_kind(status), byte_size }
        }
    }
}

fn to_wire_change_graph_node(node: jj_ops::ChangeGraphNode) -> ChangeGraphNode {
    ChangeGraphNode {
        change_id: node.change_id,
        commit_id: node.commit_id,
        description: node.description,
        author: node.author,
        parent_change_ids: node.parent_change_ids,
        is_working_copy: node.is_working_copy,
        bookmarks: node.bookmarks,
    }
}

fn to_wire_operation_log_entry(entry: jj_ops::OperationLogEntry) -> OperationLogEntry {
    OperationLogEntry {
        op_id: entry.op_id,
        description: entry.description,
        start_time: entry.start_time,
        end_time: entry.end_time,
        tags: entry.tags,
    }
}

async fn handle_diff(
    ctx: &RpcContext,
    request_id: String,
    workspace_id: &str,
    from: Option<&str>,
    to: Option<&str>,
) -> RpcResponse {
    let root_path = match lookup_root(ctx, workspace_id).await {
        Ok(path) => path,
        Err(response) => return with_request_id(response, request_id),
    };
    match jj_ops::diff(&root_path, from, to).await {
        Ok(entries) => RpcResponse::WorkspaceDiffOk { request_id, files: entries.into_iter().map(to_wire_diff_file_entry).collect() },
        // `from`/`to` are the only caller-supplied strings `jj diff` can
        // fail on once the workspace itself is known good — a bad/unparseable
        // revision, never an internal fault of this host.
        Err(jj_error) => jj_revision_error_response(request_id, &jj_error),
    }
}

async fn handle_log(
    ctx: &RpcContext,
    request_id: String,
    workspace_id: &str,
    revset: Option<&str>,
    limit: usize,
) -> RpcResponse {
    let root_path = match lookup_root(ctx, workspace_id).await {
        Ok(path) => path,
        Err(response) => return with_request_id(response, request_id),
    };
    match jj_ops::log(&root_path, revset, limit).await {
        Ok(changes) => RpcResponse::WorkspaceLogOk { request_id, changes: changes.into_iter().map(to_wire_change_graph_node).collect() },
        // As with `handle_diff`: the only way this fails against a known-good
        // workspace is a bad `revset`.
        Err(jj_error) => jj_revision_error_response(request_id, &jj_error),
    }
}

async fn handle_op_log(ctx: &RpcContext, request_id: String, workspace_id: &str, limit: usize) -> RpcResponse {
    let root_path = match lookup_root(ctx, workspace_id).await {
        Ok(path) => path,
        Err(response) => return with_request_id(response, request_id),
    };
    match jj_ops::op_log(&root_path, limit).await {
        Ok(operations) => {
            RpcResponse::WorkspaceOpLogOk { request_id, operations: operations.into_iter().map(to_wire_operation_log_entry).collect() }
        }
        Err(jj_error) => jj_error_response(request_id, &jj_error),
    }
}

async fn handle_op_undo(ctx: &RpcContext, request_id: String, workspace_id: &str, op_id: &str) -> RpcResponse {
    if op_id.is_empty() {
        return error(request_id, "invalid_argument", "op_id must not be empty");
    }
    let root_path = match lookup_root(ctx, workspace_id).await {
        Ok(path) => path,
        Err(response) => return with_request_id(response, request_id),
    };
    match jj_ops::op_undo(&root_path, op_id).await {
        Ok(new_op_id) => RpcResponse::WorkspaceOpUndoOk { request_id, new_op_id },
        // The only caller-supplied input to `jj operation revert` is
        // `op_id` — a failure here almost always means it doesn't name a
        // real operation, which is `not_found`, not `internal`.
        Err(jj_error) => jj_op_id_error_response(request_id, &jj_error),
    }
}

async fn handle_op_restore(ctx: &RpcContext, request_id: String, workspace_id: &str, op_id: &str) -> RpcResponse {
    if op_id.is_empty() {
        return error(request_id, "invalid_argument", "op_id must not be empty");
    }
    let root_path = match lookup_root(ctx, workspace_id).await {
        Ok(path) => path,
        Err(response) => return with_request_id(response, request_id),
    };
    match jj_ops::op_restore(&root_path, op_id).await {
        Ok(new_op_id) => RpcResponse::WorkspaceOpRestoreOk { request_id, new_op_id },
        Err(jj_error) => jj_op_id_error_response(request_id, &jj_error),
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

/// See `handle_create`'s call site for why this is always `internal`, not
/// `invalid_argument`.
fn mise_error_response(request_id: String, cause: &MiseError) -> RpcResponse {
    error(request_id, "internal", format!("mise provisioning failed: {cause}"))
}

/// `Shell` items' `initial_command` is empty by default (an ordinary
/// `zellij` default-shell tab, per `zellij_ops::new_tab`'s doc comment) —
/// but env vars can't ride along with an empty `initial_command` either
/// (`new_tab` only ever prepends `env KEY=VALUE ...` in front of an
/// explicit command, per `agent_launch.rs`'s pattern), so a non-empty
/// `mise_env` needs an explicit command to attach itself to. Resolves the
/// shell the same way an interactive login would (`$SHELL`, falling back
/// to `/bin/sh`) rather than hardcoding one. Returns an empty `Vec`
/// (unchanged `Shell` behavior) when `mise_env` is empty, so a workspace
/// with no `mise.toml` gets exactly the same `Shell` argv as before
/// project-pinned toolchains existed.
fn shell_launch_argv(mise_env: &[(String, String)]) -> Vec<String> {
    if mise_env.is_empty() {
        return Vec::new();
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    prefix_with_mise_env(mise_env, vec![shell])
}

/// Prepends `env KEY=VALUE ...` in front of `argv`, the same convention
/// `agent_launch.rs::agent_launch_argv` uses to inject `mise env` into a
/// spawned process — shared by [`shell_launch_argv`] (its own resolved
/// `$SHELL` argv) and `handle_item_create`'s `WebService` branch (the
/// caller-supplied `command`, per `host-rpc.md`'s "Command construction").
/// Returns `argv` unchanged when `mise_env` is empty (no `mise.toml`),
/// matching both call sites' pre-project-pinned-toolchains behavior
/// exactly.
fn prefix_with_mise_env(mise_env: &[(String, String)], argv: Vec<String>) -> Vec<String> {
    if mise_env.is_empty() {
        return argv;
    }
    let mut prefixed = vec!["env".to_string()];
    prefixed.extend(mise_env.iter().map(|(key, value)| format!("{key}={value}")));
    prefixed.extend(argv);
    prefixed
}

fn jj_error_response(request_id: String, cause: &JjError) -> RpcResponse {
    error(request_id, "internal", format!("jj operation failed: {cause}"))
}

/// For `workspace.diff`/`workspace.log`: once a workspace's root is known
/// good, the only caller-supplied input left that a `jj diff`/`jj log`
/// invocation can fail on is `from`/`to`/`revset` — a revision or revset
/// string that doesn't parse or doesn't resolve. `JjError::CommandFailed`
/// here is that case, so it maps to `invalid_argument` rather than the
/// generic `internal` `jj_error_response` above uses for every other
/// caller (where a `CommandFailed` more plausibly reflects something wrong
/// on the host side, not a bad caller argument).
fn jj_revision_error_response(request_id: String, cause: &JjError) -> RpcResponse {
    match cause {
        JjError::CommandFailed { .. } => error(request_id, "invalid_argument", format!("jj operation failed: {cause}")),
        JjError::Spawn(_) | JjError::UnparseableOutput(_) => error(request_id, "internal", format!("jj operation failed: {cause}")),
    }
}

/// For `workspace.op.undo`/`workspace.op.restore`: the only caller-supplied
/// input is `op_id`, an entry from `workspace.op.log`'s own listing — a
/// `CommandFailed` here means `op_id` no longer names a real operation
/// (verified against real `jj 0.44.0` output: `Error: No operation ID
/// matching "..."`), which is `not_found`, not `internal`.
fn jj_op_id_error_response(request_id: String, cause: &JjError) -> RpcResponse {
    match cause {
        JjError::CommandFailed { .. } => error(request_id, "not_found", format!("jj operation failed: {cause}")),
        JjError::Spawn(_) | JjError::UnparseableOutput(_) => error(request_id, "internal", format!("jj operation failed: {cause}")),
    }
}

fn fs_error_response(request_id: String, cause: &FsError) -> RpcResponse {
    let code = match cause {
        FsError::OutOfRoot => "out_of_root",
        FsError::NotFound | FsError::NotADirectory | FsError::NotAFile => "not_found",
        FsError::BoundExceeded(_) => "bound_exceeded",
        // A binary write body isn't a size problem, so it doesn't share
        // BoundExceeded's `bound_exceeded` code — it fails a content-shape
        // check, the same category `invalid_argument` covers elsewhere in
        // this module (e.g. a malformed `content_base64`).
        FsError::BinaryContent => "invalid_argument",
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
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn ctx_with_tempdir() -> (tempfile::TempDir, RpcContext) {
        let dir = tempfile::tempdir().unwrap();
        let workspaces_dir = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspaces_dir).unwrap();
        let registry_path = dir.path().join("registry.json");
        let ctx = RpcContext {
            registry: Arc::new(Mutex::new(Registry::load(&registry_path).unwrap())),
            devhost_id: "dev-1".to_string(),
            workspaces_dir,
            // Deliberately a path nothing can execute: none of this
            // module's tests register a workspace with a `mise.toml`
            // unless they explicitly override this field (see the
            // `mise`-tagged tests below) — if `has_mise_toml`'s gate ever
            // regressed to firing unconditionally, every other test in
            // this module would start failing loudly (a `MiseError::Spawn`
            // surfaced as an `internal` RPC error) instead of silently
            // passing on cargo-culted mocked behavior.
            mise_bin: "/nonexistent/choosh-hostd-tests-must-not-invoke-mise".to_string(),
            mise_project_tools_dir: dir.path().join("mise-project-tools"),
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

    /// Regression test for a real, previously-shipped bug: `workspace.list`
    /// used to echo back whatever `devhost_id` was current at
    /// `workspace.create` time (stored on the `Workspace` record itself),
    /// rather than this connection's own current `ctx.devhost_id` — so a
    /// workspace created before a devhost's relayd re-enrollment (which
    /// reassigns a new `device_id`; relayd's registry is in-memory-only per
    /// PLAN.md's documented accepted risk, so any restart forces this) would
    /// report a `devhost_id` no phone tunnel could actually reach, breaking
    /// every RPC against it with `not_permitted` on the relay side. A real
    /// devhost process never changes identity mid-connection, so the
    /// correct value is always "whatever this `RpcContext` was built with",
    /// never a value captured once and never revisited.
    #[tokio::test]
    async fn workspace_list_reports_this_connections_current_devhost_id_not_the_one_active_at_creation() {
        let name = format!("app-{}", uuid::Uuid::new_v4());
        let (_dir, ctx) = ctx_with_tempdir();
        let existing = ctx.workspaces_dir.join(&name);
        std::fs::create_dir_all(&existing).unwrap();
        init_git_repo(&existing);

        assert_eq!(ctx.devhost_id, "dev-1", "test assumes ctx_with_tempdir's fixed devhost_id");
        let create_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceCreate {
                request_id: "r1".to_string(),
                workspace_name: name.clone(),
                project_source: ProjectSource::ExistingPath { existing_path: name.clone() },
                parent_workspace_id: None,
            },
        )
        .await;
        assert!(matches!(create_response, RpcResponse::WorkspaceCreateOk { .. }), "{create_response:?}");

        // Simulates the same devhost reconnecting under a freshly
        // relayd-assigned identity — same registry (a real re-enrollment
        // never touches choosh-hostd's own on-disk workspace registry),
        // different `devhost_id`.
        let reenrolled_ctx = RpcContext { devhost_id: "dev-2-after-reenrollment".to_string(), ..ctx };
        let list_response =
            dispatch(&reenrolled_ctx, RpcRequest::WorkspaceList { request_id: "r2".to_string() }).await;
        let RpcResponse::WorkspaceListOk { workspaces, .. } = list_response else {
            panic!("expected WorkspaceListOk, got {list_response:?}");
        };
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].devhost_id, "dev-2-after-reenrollment");

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

    /// Sets up a fresh registered workspace with one uncommitted change on
    /// disk, for the `workspace.diff`/`workspace.log`/`workspace.op.*`
    /// dispatch tests below.
    async fn setup_m3_workspace(prefix: &str) -> (tempfile::TempDir, RpcContext, String, String) {
        let name = format!("{prefix}-{}", uuid::Uuid::new_v4());
        let (dir, ctx) = ctx_with_tempdir();
        let existing = ctx.workspaces_dir.join(&name);
        std::fs::create_dir_all(&existing).unwrap();
        init_git_repo(&existing);

        let create_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceCreate {
                request_id: "r1".to_string(),
                workspace_name: name.clone(),
                project_source: ProjectSource::ExistingPath { existing_path: name.clone() },
                parent_workspace_id: None,
            },
        )
        .await;
        let RpcResponse::WorkspaceCreateOk { workspace_id, .. } = create_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected WorkspaceCreateOk, got {create_response:?}");
        };
        std::fs::write(existing.join("new.txt"), "hello from the diff RPC\n").unwrap();
        (dir, ctx, name, workspace_id)
    }

    #[tokio::test]
    async fn diff_and_log_round_trip_through_dispatch() {
        let (_dir, ctx, name, workspace_id) = setup_m3_workspace("m3diff").await;

        // workspace.diff, default from=@- to=@.
        let diff_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceDiff { request_id: "r2".to_string(), workspace_id: workspace_id.clone(), from: None, to: None },
        )
        .await;
        let RpcResponse::WorkspaceDiffOk { files, .. } = diff_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected WorkspaceDiffOk, got {diff_response:?}");
        };
        assert!(
            files.iter().any(|f| matches!(f, DiffFileEntry::Hunks { new_path: Some(p), .. } if p == "new.txt")),
            "expected new.txt in the diff, got {files:?}"
        );

        // workspace.log, default revset.
        let log_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceLog { request_id: "r3".to_string(), workspace_id: workspace_id.clone(), revset: None, limit: 20 },
        )
        .await;
        let RpcResponse::WorkspaceLogOk { changes, .. } = log_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected WorkspaceLogOk, got {log_response:?}");
        };
        assert!(changes.iter().any(|c| c.is_working_copy));

        // workspace.log with an invalid revset maps to invalid_argument, not internal.
        let bad_revset_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceLog {
                request_id: "r4".to_string(),
                workspace_id: workspace_id.clone(),
                revset: Some("not a valid revset (((".to_string()),
                limit: 20,
            },
        )
        .await;
        assert!(
            matches!(&bad_revset_response, RpcResponse::Error { code, .. } if code == "invalid_argument"),
            "expected invalid_argument, got {bad_revset_response:?}"
        );

        // An unregistered workspace_id is not_found.
        let unknown_ws_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceDiff { request_id: "r9".to_string(), workspace_id: "ws-does-not-exist".to_string(), from: None, to: None },
        )
        .await;
        assert!(matches!(unknown_ws_response, RpcResponse::Error { code, .. } if code == "not_found"));

        zellij_ops::kill_session(&name).await.ok();
    }

    #[tokio::test]
    async fn op_log_and_op_undo_restore_round_trip_through_dispatch() {
        let (_dir, ctx, name, workspace_id) = setup_m3_workspace("m3op").await;

        // workspace.op.log.
        let op_log_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceOpLog { request_id: "r5".to_string(), workspace_id: workspace_id.clone(), limit: 50 },
        )
        .await;
        let RpcResponse::WorkspaceOpLogOk { operations, .. } = op_log_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected WorkspaceOpLogOk, got {op_log_response:?}");
        };
        assert!(!operations.is_empty());
        let latest_op_id = operations[0].op_id.clone();

        // workspace.op.undo returns the NEW operation's id, never the id it undid.
        let undo_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceOpUndo { request_id: "r6".to_string(), workspace_id: workspace_id.clone(), op_id: latest_op_id.clone() },
        )
        .await;
        let RpcResponse::WorkspaceOpUndoOk { new_op_id, .. } = undo_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected WorkspaceOpUndoOk, got {undo_response:?}");
        };
        assert_ne!(new_op_id, latest_op_id);

        // workspace.op.restore likewise returns a fresh id.
        let restore_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceOpRestore {
                request_id: "r7".to_string(),
                workspace_id: workspace_id.clone(),
                op_id: latest_op_id.clone(),
            },
        )
        .await;
        let RpcResponse::WorkspaceOpRestoreOk { new_op_id: restore_new_op_id, .. } = restore_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected WorkspaceOpRestoreOk, got {restore_response:?}");
        };
        assert_ne!(restore_new_op_id, latest_op_id);
        assert_ne!(restore_new_op_id, new_op_id);

        // workspace.op.undo of an op_id that doesn't exist maps to not_found.
        let bad_op_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceOpUndo { request_id: "r8".to_string(), workspace_id: workspace_id.clone(), op_id: "deadbeef1234".to_string() },
        )
        .await;
        assert!(
            matches!(&bad_op_response, RpcResponse::Error { code, .. } if code == "not_found"),
            "expected not_found, got {bad_op_response:?}"
        );

        zellij_ops::kill_session(&name).await.ok();
    }

    #[tokio::test]
    async fn file_write_updates_content_and_is_visible_to_jj_without_a_commit_step() {
        let (_dir, ctx, name, workspace_id) = setup_m3_workspace("m4write").await;
        let existing = ctx.workspaces_dir.join(&name);

        let read_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceFileRead {
                request_id: "r1".to_string(),
                workspace_id: workspace_id.clone(),
                path: "a.txt".to_string(),
                revision: None,
                range: None,
            },
        )
        .await;
        let RpcResponse::WorkspaceFileReadOk { revision, .. } = read_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected WorkspaceFileReadOk, got {read_response:?}");
        };

        let write_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceFileWrite {
                request_id: "r2".to_string(),
                workspace_id: workspace_id.clone(),
                path: "a.txt".to_string(),
                base_revision: revision.clone(),
                content_base64: base64::engine::general_purpose::STANDARD.encode(b"hello, edited\n"),
            },
        )
        .await;
        let RpcResponse::WorkspaceFileWriteOk { revision: new_revision, .. } = write_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected WorkspaceFileWriteOk, got {write_response:?}");
        };
        assert_ne!(new_revision, revision, "a successful write must return a revision that differs from the pre-write one");
        assert_eq!(std::fs::read(existing.join("a.txt")).unwrap(), b"hello, edited\n");

        // Confirm real jj visibility with NO explicit "commit" step from
        // this code: `handle_file_write` above only wrote raw bytes to
        // disk. Invoking `jj diff --summary` directly here (bypassing this
        // crate's dispatch/jj_ops entirely) must already see the write as
        // part of `@`'s snapshot, per jj-integration.md: "jj snapshots the
        // new working-copy state automatically."
        let diff_output = std::process::Command::new("jj")
            .args(["diff", "--summary", "-r", "@", "--no-pager", "--color=never"])
            .current_dir(&existing)
            .output()
            .unwrap();
        assert!(diff_output.status.success(), "jj diff --summary failed: {}", String::from_utf8_lossy(&diff_output.stderr));
        let diff_text = String::from_utf8_lossy(&diff_output.stdout);
        assert!(diff_text.contains("a.txt"), "expected a.txt to show as changed in `jj diff --summary`, got: {diff_text:?}");

        zellij_ops::kill_session(&name).await.ok();
    }

    #[tokio::test]
    async fn file_write_rejects_a_stale_base_revision_and_leaves_content_unchanged() {
        let (_dir, ctx, name, workspace_id) = setup_m3_workspace("m4stale").await;
        let existing = ctx.workspaces_dir.join(&name);

        let read_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceFileRead {
                request_id: "r1".to_string(),
                workspace_id: workspace_id.clone(),
                path: "a.txt".to_string(),
                revision: None,
                range: None,
            },
        )
        .await;
        let RpcResponse::WorkspaceFileReadOk { revision: original_revision, .. } = read_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected WorkspaceFileReadOk, got {read_response:?}");
        };

        // First write succeeds and moves the file's revision forward.
        let first_write = dispatch(
            &ctx,
            RpcRequest::WorkspaceFileWrite {
                request_id: "r2".to_string(),
                workspace_id: workspace_id.clone(),
                path: "a.txt".to_string(),
                base_revision: original_revision.clone(),
                content_base64: base64::engine::general_purpose::STANDARD.encode(b"first edit\n"),
            },
        )
        .await;
        assert!(matches!(first_write, RpcResponse::WorkspaceFileWriteOk { .. }), "expected WorkspaceFileWriteOk, got {first_write:?}");

        // Second write reuses the now-stale ORIGINAL revision — must be
        // rejected, not silently applied on top of the first edit.
        let second_write = dispatch(
            &ctx,
            RpcRequest::WorkspaceFileWrite {
                request_id: "r3".to_string(),
                workspace_id: workspace_id.clone(),
                path: "a.txt".to_string(),
                base_revision: original_revision,
                content_base64: base64::engine::general_purpose::STANDARD.encode(b"conflicting edit\n"),
            },
        )
        .await;
        let RpcResponse::WorkspaceFileWriteStale { current_revision, current_content_base64, .. } = second_write else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected WorkspaceFileWriteStale, got {second_write:?}");
        };
        let current_content = base64::engine::general_purpose::STANDARD.decode(current_content_base64).unwrap();
        assert_eq!(current_content, b"first edit\n", "the stale response must carry the file's ACTUAL current content");
        assert_eq!(current_revision, fs_ops::content_revision(b"first edit\n"));
        // The rejected write must not have touched the file on disk.
        assert_eq!(std::fs::read(existing.join("a.txt")).unwrap(), b"first edit\n");

        zellij_ops::kill_session(&name).await.ok();
    }

    #[tokio::test]
    async fn file_write_rejects_binary_and_oversized_content_without_touching_the_file() {
        let (_dir, ctx, name, workspace_id) = setup_m3_workspace("m4bounds").await;
        let existing = ctx.workspaces_dir.join(&name);

        let read_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceFileRead {
                request_id: "r1".to_string(),
                workspace_id: workspace_id.clone(),
                path: "a.txt".to_string(),
                revision: None,
                range: None,
            },
        )
        .await;
        let RpcResponse::WorkspaceFileReadOk { revision, .. } = read_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected WorkspaceFileReadOk, got {read_response:?}");
        };

        // Binary content (contains a null byte) is rejected as invalid_argument.
        let binary_write = dispatch(
            &ctx,
            RpcRequest::WorkspaceFileWrite {
                request_id: "r2".to_string(),
                workspace_id: workspace_id.clone(),
                path: "a.txt".to_string(),
                base_revision: revision.clone(),
                content_base64: base64::engine::general_purpose::STANDARD.encode(b"bin\0ary"),
            },
        )
        .await;
        assert!(
            matches!(&binary_write, RpcResponse::Error { code, .. } if code == "invalid_argument"),
            "expected invalid_argument, got {binary_write:?}"
        );

        // Oversized content is rejected as bound_exceeded.
        let oversized = vec![b'x'; usize::try_from(MAX_FILE_READ_RANGE_BYTES + 1).unwrap()];
        let oversized_write = dispatch(
            &ctx,
            RpcRequest::WorkspaceFileWrite {
                request_id: "r3".to_string(),
                workspace_id: workspace_id.clone(),
                path: "a.txt".to_string(),
                base_revision: revision,
                content_base64: base64::engine::general_purpose::STANDARD.encode(oversized),
            },
        )
        .await;
        assert!(
            matches!(&oversized_write, RpcResponse::Error { code, .. } if code == "bound_exceeded"),
            "expected bound_exceeded, got {oversized_write:?}"
        );

        // Neither rejected write may have touched the file on disk.
        assert_eq!(std::fs::read(existing.join("a.txt")).unwrap(), b"hello\n");

        zellij_ops::kill_session(&name).await.ok();
    }

    #[tokio::test]
    async fn file_write_round_trips_mixed_line_endings_through_read_write_read() {
        let (_dir, ctx, name, workspace_id) = setup_m3_workspace("m4eol").await;
        let existing = ctx.workspaces_dir.join(&name);
        let mixed: &[u8] = b"line1\r\nline2\nline3\r\nline4\n";
        std::fs::write(existing.join("a.txt"), mixed).unwrap();

        let read_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceFileRead {
                request_id: "r1".to_string(),
                workspace_id: workspace_id.clone(),
                path: "a.txt".to_string(),
                revision: None,
                range: None,
            },
        )
        .await;
        let RpcResponse::WorkspaceFileReadOk { content_base64, revision, .. } = read_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected WorkspaceFileReadOk, got {read_response:?}");
        };
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(&content_base64).unwrap(), mixed);

        let write_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceFileWrite {
                request_id: "r2".to_string(),
                workspace_id: workspace_id.clone(),
                path: "a.txt".to_string(),
                base_revision: revision,
                content_base64,
            },
        )
        .await;
        assert!(matches!(write_response, RpcResponse::WorkspaceFileWriteOk { .. }), "expected WorkspaceFileWriteOk, got {write_response:?}");

        let reread_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceFileRead {
                request_id: "r3".to_string(),
                workspace_id: workspace_id.clone(),
                path: "a.txt".to_string(),
                revision: None,
                range: None,
            },
        )
        .await;
        let RpcResponse::WorkspaceFileReadOk { content_base64: reread_base64, .. } = reread_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected WorkspaceFileReadOk, got {reread_response:?}");
        };
        assert_eq!(
            base64::engine::general_purpose::STANDARD.decode(&reread_base64).unwrap(),
            mixed,
            "mixed line endings must round-trip byte-identical through read -> write -> read"
        );

        zellij_ops::kill_session(&name).await.ok();
    }

    /// `workspace.create`'s `existing_path` root-confinement rejection
    /// (`confine_workspaces_dir`), driven all the way through `dispatch`
    /// rather than unit-tested against `confine_workspaces_dir` directly —
    /// this is also the regression test for a real bug found while
    /// reviewing this function: every error `create_root_workspace`/
    /// `create_agent_workspace`/`confine_workspaces_dir` returns is built
    /// with an empty placeholder `request_id` (they have no way to know the
    /// caller's real one), and `handle_create` used to return that response
    /// as-is instead of filling the real `request_id` back in via
    /// `with_request_id` — silently dropping it from every one of these
    /// error responses. Asserted here alongside the `out_of_root` code
    /// itself so a regression on either would fail this test.
    #[tokio::test]
    async fn workspace_create_rejects_an_existing_path_traversal_and_preserves_the_request_id() {
        let (_dir, ctx) = ctx_with_tempdir();
        let response = dispatch(
            &ctx,
            RpcRequest::WorkspaceCreate {
                request_id: "the-real-request-id".to_string(),
                workspace_name: "escapetest".to_string(),
                project_source: ProjectSource::ExistingPath { existing_path: "../../../etc".to_string() },
                parent_workspace_id: None,
            },
        )
        .await;
        assert!(
            matches!(&response, RpcResponse::Error { code, request_id, .. } if code == "out_of_root" && request_id == "the-real-request-id"),
            "expected an out_of_root error carrying the caller's real request_id, got {response:?}"
        );
    }

    /// `create_agent_workspace`'s own `not_found` path (an unregistered
    /// `parent_workspace_id`) — a second, independent proof that these
    /// helper-returned errors' `request_id` is preserved, this time through
    /// the `parent_workspace_id`-set branch of `handle_create` rather than
    /// `create_root_workspace`'s.
    #[tokio::test]
    async fn workspace_create_with_an_unregistered_parent_workspace_id_is_not_found() {
        let (_dir, ctx) = ctx_with_tempdir();
        let response = dispatch(
            &ctx,
            RpcRequest::WorkspaceCreate {
                request_id: "r1".to_string(),
                workspace_name: "agentws".to_string(),
                project_source: ProjectSource::ExistingPath { existing_path: "agentws".to_string() },
                parent_workspace_id: Some("ws-does-not-exist".to_string()),
            },
        )
        .await;
        assert!(
            matches!(&response, RpcResponse::Error { code, request_id, .. } if code == "not_found" && request_id == "r1"),
            "expected a not_found error carrying the caller's real request_id, got {response:?}"
        );
    }

    /// Regression test for a real bug: `confine_workspaces_dir` used to only
    /// reject literal `..` segments and a leading `/`, never canonicalizing
    /// the resolved path to verify it actually stayed under
    /// `ctx.workspaces_dir` — unlike its sibling `fs_ops::confine` (whose own
    /// `confine_rejects_a_symlink_escaping_the_root` test proves the same
    /// guarantee directly against `confine`). A symlink planted under
    /// `workspaces_dir` that points somewhere else would have been silently
    /// followed. Driven end to end through `dispatch`, mirroring
    /// `fs_ops.rs`'s own symlink test's shape (a symlink named `escape`
    /// pointing at `/etc`, then a path through it).
    #[tokio::test]
    #[cfg(unix)]
    async fn workspace_create_rejects_an_existing_path_that_is_a_symlink_escaping_workspaces_dir() {
        let (_dir, ctx) = ctx_with_tempdir();
        std::os::unix::fs::symlink("/etc", ctx.workspaces_dir.join("escape")).unwrap();

        let response = dispatch(
            &ctx,
            RpcRequest::WorkspaceCreate {
                request_id: "r1".to_string(),
                workspace_name: "escapetest2".to_string(),
                project_source: ProjectSource::ExistingPath { existing_path: "escape/passwd".to_string() },
                parent_workspace_id: None,
            },
        )
        .await;
        assert!(
            matches!(&response, RpcResponse::Error { code, .. } if code == "out_of_root"),
            "a symlink under workspaces_dir escaping its root must be rejected as out_of_root, got {response:?}"
        );
    }

    /// Direct proof that `workspace.create`'s name reservation
    /// (`Registry::reserve_workspace_name`, taken by `handle_create` before
    /// any expensive work) actually gates the expensive work itself, not
    /// just the final registration: with `workspace_name` already held
    /// under a reservation — simulating a concurrent in-flight
    /// `workspace.create` for the same name that reserved it first — a
    /// second `workspace.create` for that name must fail immediately as
    /// `conflict`, and critically must never get as far as creating a
    /// Zellij session for it. Before this fix, the analogous early check
    /// only looked at *registered* names (never reserved-but-not-yet-
    /// registered ones), so a second caller racing a first one that hadn't
    /// finished registering yet would sail past it and do the real
    /// clone/session work anyway, only to lose the authoritative check at
    /// the very end and leave that work orphaned with no cleanup.
    #[tokio::test]
    async fn workspace_create_is_rejected_immediately_while_the_name_is_reserved_without_doing_any_real_work() {
        let name = format!("reservetest-{}", uuid::Uuid::new_v4());
        let (_dir, ctx) = ctx_with_tempdir();
        let existing = ctx.workspaces_dir.join(&name);
        std::fs::create_dir_all(&existing).unwrap();
        init_git_repo(&existing);

        // Simulates another in-flight `workspace.create` for this exact
        // name that has already passed its own reservation but hasn't
        // finished the expensive work / registration yet.
        ctx.registry.lock().await.reserve_workspace_name(&name).unwrap();

        let response = dispatch(
            &ctx,
            RpcRequest::WorkspaceCreate {
                request_id: "r1".to_string(),
                workspace_name: name.clone(),
                project_source: ProjectSource::ExistingPath { existing_path: name.clone() },
                parent_workspace_id: None,
            },
        )
        .await;
        assert!(
            matches!(&response, RpcResponse::Error { code, .. } if code == "conflict"),
            "expected conflict while the name is reserved, got {response:?}"
        );

        // The real proof: no Zellij session was ever created for the
        // rejected caller — it never got past the reservation check to do
        // any of workspace.create's expensive work.
        assert!(
            !zellij_ops::list_sessions().await.unwrap().contains(&name),
            "a workspace.create rejected at the reservation stage must never create a Zellij session"
        );

        ctx.registry.lock().await.release_workspace_name_reservation(&name);
    }

    /// `WorkspaceTreeList`/`WorkspaceFileRead`'s shared `reject_non_live_revision`
    /// gate: a `revision` other than `None`/`Some("@")` must be
    /// `invalid_argument`, never silently served against `@` instead.
    #[tokio::test]
    async fn tree_list_rejects_a_non_live_revision() {
        let (_dir, ctx, name, workspace_id) = setup_m3_workspace("m1revision").await;
        let response = dispatch(
            &ctx,
            RpcRequest::WorkspaceTreeList {
                request_id: "r1".to_string(),
                workspace_id,
                path_prefix: String::new(),
                revision: Some("abc123".to_string()),
                cursor: None,
            },
        )
        .await;
        zellij_ops::kill_session(&name).await.ok();
        assert!(
            matches!(&response, RpcResponse::Error { code, .. } if code == "invalid_argument"),
            "expected invalid_argument for a non-live revision, got {response:?}"
        );
    }

    /// `item.create`'s two workspace/name-shape error paths, neither of
    /// which had direct dispatch-level coverage before this test: an
    /// unregistered `workspace_id` is `not_found`, and a malformed `name`
    /// is `invalid_argument` — both checked before anything about
    /// `item_type` is even looked at.
    #[tokio::test]
    async fn item_create_rejects_an_unregistered_workspace_and_a_malformed_name() {
        let (_dir, ctx, name, workspace_id) = setup_m3_workspace("m2itembasics").await;

        let unknown_workspace = dispatch(
            &ctx,
            RpcRequest::ItemCreate {
                request_id: "r1".to_string(),
                workspace_id: "ws-does-not-exist".to_string(),
                item_type: ItemType::Shell,
                name: "sh".to_string(),
                agent: None,
                command: None,
                port: None,
            },
        )
        .await;
        assert!(
            matches!(&unknown_workspace, RpcResponse::Error { code, .. } if code == "not_found"),
            "expected not_found for an unregistered workspace_id, got {unknown_workspace:?}"
        );

        let bad_name = dispatch(
            &ctx,
            RpcRequest::ItemCreate {
                request_id: "r2".to_string(),
                workspace_id: workspace_id.clone(),
                item_type: ItemType::Shell,
                name: "not valid!".to_string(),
                agent: None,
                command: None,
                port: None,
            },
        )
        .await;

        zellij_ops::kill_session(&name).await.ok();
        assert!(
            matches!(&bad_name, RpcResponse::Error { code, .. } if code == "invalid_argument"),
            "expected invalid_argument for a malformed item name, got {bad_name:?}"
        );
    }

    /// `item.create`'s per-`item_type` required-field validations, none of
    /// which had direct dispatch-level coverage before this test:
    /// `AgentTerminal` needs `agent`; `WebService` needs a non-empty
    /// `command` and a `port`. Each is `invalid_argument`, and none
    /// registers a partial item along the way.
    #[tokio::test]
    async fn item_create_type_specific_required_fields_are_validated() {
        let (_dir, ctx, name, workspace_id) = setup_m3_workspace("m2itemfields").await;

        let missing_agent = dispatch(
            &ctx,
            RpcRequest::ItemCreate {
                request_id: "r1".to_string(),
                workspace_id: workspace_id.clone(),
                item_type: ItemType::AgentTerminal,
                name: "agent1".to_string(),
                agent: None,
                command: None,
                port: None,
            },
        )
        .await;
        assert!(
            matches!(&missing_agent, RpcResponse::Error { code, .. } if code == "invalid_argument"),
            "expected invalid_argument for AgentTerminal with no agent, got {missing_agent:?}"
        );

        let missing_command = dispatch(
            &ctx,
            RpcRequest::ItemCreate {
                request_id: "r2".to_string(),
                workspace_id: workspace_id.clone(),
                item_type: ItemType::WebService,
                name: "web1".to_string(),
                agent: None,
                command: None,
                port: Some(reserve_ephemeral_port()),
            },
        )
        .await;
        assert!(
            matches!(&missing_command, RpcResponse::Error { code, .. } if code == "invalid_argument"),
            "expected invalid_argument for WebService with no command, got {missing_command:?}"
        );

        let empty_command = dispatch(
            &ctx,
            RpcRequest::ItemCreate {
                request_id: "r3".to_string(),
                workspace_id: workspace_id.clone(),
                item_type: ItemType::WebService,
                name: "web2".to_string(),
                agent: None,
                command: Some(Vec::new()),
                port: Some(reserve_ephemeral_port()),
            },
        )
        .await;
        assert!(
            matches!(&empty_command, RpcResponse::Error { code, .. } if code == "invalid_argument"),
            "expected invalid_argument for WebService with an empty command, got {empty_command:?}"
        );

        let missing_port = dispatch(
            &ctx,
            RpcRequest::ItemCreate {
                request_id: "r4".to_string(),
                workspace_id: workspace_id.clone(),
                item_type: ItemType::WebService,
                name: "web3".to_string(),
                agent: None,
                command: Some(vec!["cat".to_string()]),
                port: None,
            },
        )
        .await;

        zellij_ops::kill_session(&name).await.ok();
        assert!(
            matches!(&missing_port, RpcResponse::Error { code, .. } if code == "invalid_argument"),
            "expected invalid_argument for WebService with no port, got {missing_port:?}"
        );
    }

    /// `item.create`'s `conflict` path: a second `item.create` reusing an
    /// already-registered `name` within the same workspace is rejected,
    /// never silently adopted or overwritten.
    #[tokio::test]
    async fn item_create_duplicate_name_is_a_conflict() {
        let (_dir, ctx, name, workspace_id) = setup_m3_workspace("m2itemdup").await;

        let first = dispatch(
            &ctx,
            RpcRequest::ItemCreate {
                request_id: "r1".to_string(),
                workspace_id: workspace_id.clone(),
                item_type: ItemType::Shell,
                name: "sh".to_string(),
                agent: None,
                command: None,
                port: None,
            },
        )
        .await;
        assert!(matches!(first, RpcResponse::ItemCreateOk { .. }), "expected ItemCreateOk, got {first:?}");

        let duplicate = dispatch(
            &ctx,
            RpcRequest::ItemCreate {
                request_id: "r2".to_string(),
                workspace_id: workspace_id.clone(),
                item_type: ItemType::Shell,
                name: "sh".to_string(),
                agent: None,
                command: None,
                port: None,
            },
        )
        .await;

        zellij_ops::kill_session(&name).await.ok();
        assert!(
            matches!(&duplicate, RpcResponse::Error { code, .. } if code == "conflict"),
            "expected conflict for a duplicate item name, got {duplicate:?}"
        );
    }

    /// The `item.create` analog of `workspace_create_is_rejected_immediately_
    /// while_the_name_is_reserved_without_doing_any_real_work`: with
    /// `(workspace_id, name)` already reserved — simulating a concurrent
    /// in-flight `item.create` for that exact pair — a second `item.create`
    /// is rejected immediately as `conflict`, and critically never creates
    /// a Zellij tab for it. Direct proof that `Registry::reserve_item_name`
    /// gates `item.create`'s expensive work (Zellij tab creation), not just
    /// its final registration.
    #[tokio::test]
    async fn item_create_is_rejected_immediately_while_the_name_is_reserved_without_doing_any_real_work() {
        let (_dir, ctx, name, workspace_id) = setup_m3_workspace("m2itemreserve").await;

        ctx.registry.lock().await.reserve_item_name(&workspace_id, "sh").unwrap();

        let response = dispatch(
            &ctx,
            RpcRequest::ItemCreate {
                request_id: "r1".to_string(),
                workspace_id: workspace_id.clone(),
                item_type: ItemType::Shell,
                name: "sh".to_string(),
                agent: None,
                command: None,
                port: None,
            },
        )
        .await;
        assert!(
            matches!(&response, RpcResponse::Error { code, .. } if code == "conflict"),
            "expected conflict while the item name is reserved, got {response:?}"
        );

        let tabs = zellij_ops::list_tabs(&name).await.unwrap();
        zellij_ops::kill_session(&name).await.ok();
        assert!(
            !tabs.contains(&"sh".to_string()),
            "an item.create rejected at the reservation stage must never create a Zellij tab, got tabs {tabs:?}"
        );
    }

    /// `workspace.op.undo`/`workspace.op.restore`'s two shared caller-input
    /// validations that `op_log_and_op_undo_restore_round_trip_through_dispatch`
    /// doesn't already cover: an empty `op_id` is `invalid_argument` for
    /// both RPCs (checked before either ever invokes `jj`), and
    /// `workspace.op.restore` maps an `op_id` that doesn't resolve to
    /// `not_found`, exactly like `workspace.op.undo` already does.
    #[tokio::test]
    async fn op_undo_and_op_restore_reject_empty_and_unresolvable_op_ids() {
        let (_dir, ctx, name, workspace_id) = setup_m3_workspace("m3opids").await;

        let empty_undo = dispatch(
            &ctx,
            RpcRequest::WorkspaceOpUndo { request_id: "r1".to_string(), workspace_id: workspace_id.clone(), op_id: String::new() },
        )
        .await;
        assert!(
            matches!(&empty_undo, RpcResponse::Error { code, .. } if code == "invalid_argument"),
            "expected invalid_argument for an empty op_id, got {empty_undo:?}"
        );

        let empty_restore = dispatch(
            &ctx,
            RpcRequest::WorkspaceOpRestore { request_id: "r2".to_string(), workspace_id: workspace_id.clone(), op_id: String::new() },
        )
        .await;
        assert!(
            matches!(&empty_restore, RpcResponse::Error { code, .. } if code == "invalid_argument"),
            "expected invalid_argument for an empty op_id, got {empty_restore:?}"
        );

        let unresolvable_restore = dispatch(
            &ctx,
            RpcRequest::WorkspaceOpRestore {
                request_id: "r3".to_string(),
                workspace_id: workspace_id.clone(),
                op_id: "deadbeef1234".to_string(),
            },
        )
        .await;

        zellij_ops::kill_session(&name).await.ok();
        assert!(
            matches!(&unresolvable_restore, RpcResponse::Error { code, .. } if code == "not_found"),
            "expected not_found for an unresolvable op_id, got {unresolvable_restore:?}"
        );
    }

    /// A free-but-unused ephemeral port: binds to port 0 to let the OS
    /// assign one, then immediately releases it so a caller can declare it
    /// to `item.create` before anything is actually listening. Carries the
    /// same small, accepted re-use race any "reserve a port, use it later"
    /// pattern does.
    fn reserve_ephemeral_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    /// `service-tunnels.md`'s Lifecycle + Readiness sections, end to end
    /// through the real `item.create` dispatch path: a `WebService` item
    /// is `starting` immediately after creation (never `running` before
    /// its port is even checked), and transitions to `running` on its own,
    /// within [`readiness::READINESS_TIMEOUT`], once something starts
    /// listening on the declared port — no polling of the port by this
    /// test other than observing the registry's own status field, which is
    /// the real signal `item.list`/`ItemSummary::status` exposes to
    /// callers.
    #[tokio::test]
    async fn web_service_item_becomes_running_once_its_port_starts_accepting_connections() {
        let (_dir, ctx, name, workspace_id) = setup_m3_workspace("m5websvc-run").await;
        let port = reserve_ephemeral_port();

        let create_response = dispatch(
            &ctx,
            RpcRequest::ItemCreate {
                request_id: "r1".to_string(),
                workspace_id: workspace_id.clone(),
                item_type: ItemType::WebService,
                name: "web".to_string(),
                agent: None,
                command: Some(vec!["cat".to_string()]),
                port: Some(port),
            },
        )
        .await;
        let RpcResponse::ItemCreateOk { item_id, .. } = create_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected ItemCreateOk, got {create_response:?}");
        };

        // Immediately after creation, before anything is listening: must
        // be `starting`, never `running` — `item.create` itself never
        // blocks on readiness.
        {
            let registry = ctx.registry.lock().await;
            assert_eq!(registry.find_item(&item_id).unwrap().status, ItemStatus::Starting);
        }

        // Now bring the port up for real.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        tokio::spawn(async move {
            loop {
                if listener.accept().await.is_err() {
                    return;
                }
            }
        });

        let deadline = std::time::Instant::now() + readiness::READINESS_TIMEOUT + std::time::Duration::from_secs(2);
        let mut observed = None;
        while std::time::Instant::now() < deadline {
            let status = ctx.registry.lock().await.find_item(&item_id).unwrap().status;
            if status != ItemStatus::Starting {
                observed = Some(status);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        zellij_ops::kill_session(&name).await.ok();
        assert_eq!(observed, Some(ItemStatus::Running), "expected the item to become running within the readiness bound");
    }

    /// The other half of the same lifecycle: a port that never comes up at
    /// all must transition to `failed`, not sit `starting` forever, and
    /// must do so within the documented bound.
    #[tokio::test]
    async fn web_service_item_fails_if_its_port_never_comes_up_within_the_readiness_bound() {
        let (_dir, ctx, name, workspace_id) = setup_m3_workspace("m5websvc-fail").await;
        let port = reserve_ephemeral_port(); // reserved, then deliberately never listened on.

        let create_response = dispatch(
            &ctx,
            RpcRequest::ItemCreate {
                request_id: "r1".to_string(),
                workspace_id: workspace_id.clone(),
                item_type: ItemType::WebService,
                name: "web".to_string(),
                agent: None,
                command: Some(vec!["cat".to_string()]),
                port: Some(port),
            },
        )
        .await;
        let RpcResponse::ItemCreateOk { item_id, .. } = create_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected ItemCreateOk, got {create_response:?}");
        };

        let deadline = std::time::Instant::now() + readiness::READINESS_TIMEOUT + std::time::Duration::from_secs(2);
        let mut observed = None;
        while std::time::Instant::now() < deadline {
            let status = ctx.registry.lock().await.find_item(&item_id).unwrap().status;
            if status != ItemStatus::Starting {
                observed = Some(status);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        zellij_ops::kill_session(&name).await.ok();
        assert_eq!(observed, Some(ItemStatus::Failed), "expected the item to fail within the readiness bound when its port never comes up");
    }

    // -------------------------------------------------------------
    // M7 two-tier `mise` provisioning: Project-pinned toolchains
    // (toolchain-provisioning.md's first tier), end to end through
    // `dispatch` itself — a real `WorkspaceCreate` then a real
    // `ItemCreate`, exactly as `serve.rs` drives this module in
    // production. `ctx.mise_bin` is pointed at a fixture script (same
    // approach `mise_ops::tests` uses for `ensure_zed_remote_server`),
    // not the real `mise` binary — deterministic and network-free, while
    // still exercising the real `Command`/`tokio::process` plumbing and
    // the real `zellij` binary for tab creation.
    // -------------------------------------------------------------

    /// A fake `mise` standing in for the real binary in these `rpc.rs`
    /// integration tests: `install` succeeds unless a `FAIL_INSTALL`
    /// marker file exists in the directory it's invoked against (the
    /// workspace root, since `mise_ops::run_mise_project` sets that as
    /// `current_dir`), and `env --json` echoes back that same directory's
    /// `env.json` file — a caller-controlled stand-in for "whatever this
    /// workspace's real `mise.toml` would resolve to", exactly mirroring
    /// `mise_ops::tests::write_fake_project_mise`'s shape.
    fn write_fake_project_mise(dir: &Path) -> PathBuf {
        let script_path = dir.join("mise");
        let body = "#!/bin/sh\nset -e\ncase \"$1\" in\n  install)\n    if [ -f \"$(pwd)/FAIL_INSTALL\" ]; then echo 'boom: cannot resolve tool' >&2; exit 1; fi\n    exit 0\n    ;;\n  env)\n    cat \"$(pwd)/env.json\"\n    ;;\n  *)\n    echo \"unknown subcommand: $1\" >&2\n    exit 1\n    ;;\nesac\n";
        std::fs::write(&script_path, body).unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        script_path
    }

    /// Creates a real git repo at `ctx.workspaces_dir/<name>` with both a
    /// (fixture-only-meaningful) `mise.toml` and an `env.json` describing
    /// what the fake `mise` from [`write_fake_project_mise`] should
    /// resolve for it.
    fn init_git_repo_with_mise_toml(ctx: &RpcContext, name: &str, marker_value: &str) -> PathBuf {
        let existing = ctx.workspaces_dir.join(name);
        std::fs::create_dir_all(&existing).unwrap();
        init_git_repo(&existing);
        std::fs::write(existing.join("mise.toml"), b"[tools]\nnode = \"20\"\n").unwrap();
        std::fs::write(existing.join("env.json"), format!(r#"{{"CHOOSH_MISE_TEST_MARKER": "{marker_value}"}}"#)).unwrap();
        existing
    }

    #[tokio::test]
    async fn mise_toml_pinned_tool_is_installed_and_its_env_reaches_a_spawned_item_process() {
        let name = format!("mise-e2e-{}", uuid::Uuid::new_v4());
        let (dir, mut ctx) = ctx_with_tempdir();
        let mise_bin = write_fake_project_mise(dir.path());
        ctx.mise_bin = mise_bin.to_str().unwrap().to_string();
        init_git_repo_with_mise_toml(&ctx, &name, "workspace-a-value");

        let create_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceCreate {
                request_id: "r1".to_string(),
                workspace_name: name.clone(),
                project_source: ProjectSource::ExistingPath { existing_path: name.clone() },
                parent_workspace_id: None,
            },
        )
        .await;
        let RpcResponse::WorkspaceCreateOk { workspace_id, .. } = create_response else {
            panic!("expected WorkspaceCreateOk (mise install against a real mise.toml should succeed), got {create_response:?}");
        };

        let output_file = dir.path().join("web-service-env-dump.txt");
        let item_response = dispatch(
            &ctx,
            RpcRequest::ItemCreate {
                request_id: "r2".to_string(),
                workspace_id: workspace_id.clone(),
                item_type: ItemType::WebService,
                name: "dumpenv".to_string(),
                agent: None,
                command: Some(vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    format!("echo \"marker=$CHOOSH_MISE_TEST_MARKER\" > {}", output_file.display()),
                ]),
                port: Some(reserve_ephemeral_port()),
            },
        )
        .await;
        let RpcResponse::ItemCreateOk { .. } = item_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected ItemCreateOk, got {item_response:?}");
        };

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut content = String::new();
        while std::time::Instant::now() < deadline {
            if let Ok(text) = std::fs::read_to_string(&output_file) {
                content = text;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        zellij_ops::kill_session(&name).await.ok();
        assert_eq!(content.trim(), "marker=workspace-a-value", "the mise-resolved CHOOSH_MISE_TEST_MARKER env var must be visible inside the spawned item process");
    }

    #[tokio::test]
    async fn workspace_with_no_mise_toml_registers_normally_with_no_mise_install_attempted() {
        // `ctx_with_tempdir`'s default `mise_bin` points at a nonexistent
        // path: if `handle_create`/`handle_item_create` ever called
        // `mise` despite this workspace having no `mise.toml`, the very
        // first such call would fail to spawn and this whole test would
        // fail — this is exactly the assertion this test needs, made
        // automatically true by never overriding `mise_bin`.
        let name = format!("no-mise-{}", uuid::Uuid::new_v4());
        let (_dir, ctx) = ctx_with_tempdir();
        let existing = ctx.workspaces_dir.join(&name);
        std::fs::create_dir_all(&existing).unwrap();
        init_git_repo(&existing);
        assert!(!mise_ops::has_mise_toml(&existing), "test setup bug: this workspace must not have a mise.toml");

        let create_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceCreate {
                request_id: "r1".to_string(),
                workspace_name: name.clone(),
                project_source: ProjectSource::ExistingPath { existing_path: name.clone() },
                parent_workspace_id: None,
            },
        )
        .await;
        let RpcResponse::WorkspaceCreateOk { workspace_id, .. } = create_response else {
            panic!("expected WorkspaceCreateOk for a workspace with no mise.toml, got {create_response:?}");
        };

        // A `Shell` item must also work normally — no injected env, no
        // attempted mise invocation.
        let item_response = dispatch(
            &ctx,
            RpcRequest::ItemCreate {
                request_id: "r2".to_string(),
                workspace_id: workspace_id.clone(),
                item_type: ItemType::Shell,
                name: "sh".to_string(),
                agent: None,
                command: None,
                port: None,
            },
        )
        .await;
        zellij_ops::kill_session(&name).await.ok();
        assert!(matches!(item_response, RpcResponse::ItemCreateOk { .. }), "expected ItemCreateOk for a Shell item in a workspace with no mise.toml, got {item_response:?}");
    }

    #[tokio::test]
    async fn mise_toml_with_an_unresolvable_tool_fails_workspace_create_and_leaves_nothing_registered() {
        let name = format!("mise-bad-{}", uuid::Uuid::new_v4());
        let (dir, mut ctx) = ctx_with_tempdir();
        let mise_bin = write_fake_project_mise(dir.path());
        ctx.mise_bin = mise_bin.to_str().unwrap().to_string();
        let existing = init_git_repo_with_mise_toml(&ctx, &name, "unused");
        // Triggers the fake mise's `install` failure path (this module's
        // own equivalent of "a typo'd tool name" / "a version mise/ubi
        // can't resolve" from toolchain-provisioning.md's Failure
        // handling section).
        std::fs::write(existing.join("FAIL_INSTALL"), b"").unwrap();

        let create_response = dispatch(
            &ctx,
            RpcRequest::WorkspaceCreate {
                request_id: "r1".to_string(),
                workspace_name: name.clone(),
                project_source: ProjectSource::ExistingPath { existing_path: name.clone() },
                parent_workspace_id: None,
            },
        )
        .await;
        match &create_response {
            RpcResponse::Error { code, message, .. } => {
                assert_eq!(code, "internal");
                assert!(message.contains("boom"), "expected the fake mise's stderr to surface in the error message, got: {message}");
            }
            other => panic!("expected an Error response for an unresolvable mise.toml, got {other:?}"),
        }

        // No partial/broken workspace left registered, and no Zellij
        // session created for it either — `handle_create` must fail
        // before either side effect, not after.
        {
            let registry = ctx.registry.lock().await;
            assert!(registry.find_workspace_by_name(&name).is_none(), "a failed mise install must not leave a registered workspace behind");
        }
        assert!(!zellij_ops::list_sessions().await.unwrap().contains(&name), "a failed mise install must not leave a Zellij session behind");
    }

    #[tokio::test]
    async fn concurrent_workspaces_injected_mise_envs_do_not_leak_into_each_other() {
        let name_a = format!("mise-leak-a-{}", uuid::Uuid::new_v4());
        let name_b = format!("mise-leak-b-{}", uuid::Uuid::new_v4());
        let (dir, mut ctx) = ctx_with_tempdir();
        let mise_bin = write_fake_project_mise(dir.path());
        ctx.mise_bin = mise_bin.to_str().unwrap().to_string();
        init_git_repo_with_mise_toml(&ctx, &name_a, "value-a");
        init_git_repo_with_mise_toml(&ctx, &name_b, "value-b");

        let create_a = dispatch(
            &ctx,
            RpcRequest::WorkspaceCreate {
                request_id: "ra".to_string(),
                workspace_name: name_a.clone(),
                project_source: ProjectSource::ExistingPath { existing_path: name_a.clone() },
                parent_workspace_id: None,
            },
        )
        .await;
        let create_b = dispatch(
            &ctx,
            RpcRequest::WorkspaceCreate {
                request_id: "rb".to_string(),
                workspace_name: name_b.clone(),
                project_source: ProjectSource::ExistingPath { existing_path: name_b.clone() },
                parent_workspace_id: None,
            },
        )
        .await;
        let RpcResponse::WorkspaceCreateOk { workspace_id: workspace_id_a, .. } = create_a else {
            panic!("expected WorkspaceCreateOk for workspace A, got {create_a:?}");
        };
        let RpcResponse::WorkspaceCreateOk { workspace_id: workspace_id_b, .. } = create_b else {
            panic!("expected WorkspaceCreateOk for workspace B, got {create_b:?}");
        };

        let output_a = dir.path().join("dump-a.txt");
        let output_b = dir.path().join("dump-b.txt");
        for (workspace_id, output_file) in [(&workspace_id_a, &output_a), (&workspace_id_b, &output_b)] {
            let response = dispatch(
                &ctx,
                RpcRequest::ItemCreate {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    workspace_id: workspace_id.clone(),
                    item_type: ItemType::WebService,
                    name: "dumpenv".to_string(),
                    agent: None,
                    command: Some(vec![
                        "sh".to_string(),
                        "-c".to_string(),
                        format!("echo \"marker=$CHOOSH_MISE_TEST_MARKER\" > {}", output_file.display()),
                    ]),
                    port: Some(reserve_ephemeral_port()),
                },
            )
            .await;
            assert!(matches!(response, RpcResponse::ItemCreateOk { .. }), "expected ItemCreateOk, got {response:?}");
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let (mut content_a, mut content_b) = (String::new(), String::new());
        while std::time::Instant::now() < deadline && (content_a.is_empty() || content_b.is_empty()) {
            content_a = std::fs::read_to_string(&output_a).unwrap_or_default();
            content_b = std::fs::read_to_string(&output_b).unwrap_or_default();
            if content_a.is_empty() || content_b.is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }

        zellij_ops::kill_session(&name_a).await.ok();
        zellij_ops::kill_session(&name_b).await.ok();
        assert_eq!(content_a.trim(), "marker=value-a", "workspace A's spawned process must see only workspace A's mise env");
        assert_eq!(content_b.trim(), "marker=value-b", "workspace B's spawned process must see only workspace B's mise env");
    }

    #[tokio::test]
    async fn project_list_reflects_registered_projects_and_computes_active_from_a_connected_item() {
        let (_dir, ctx, name, workspace_id) = setup_m3_workspace("m-projlist").await;
        let project_id = ctx.registry.lock().await.find_workspace(&workspace_id).unwrap().project_id.clone();

        // No items, no recorded event yet: not active.
        let list_response = dispatch(&ctx, RpcRequest::ProjectList { request_id: "r1".to_string() }).await;
        let RpcResponse::ProjectListOk { projects, .. } = list_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected ProjectListOk, got {list_response:?}");
        };
        let project = projects.iter().find(|p| p.project_id == project_id).expect("project should be listed");
        assert!(!project.active, "a project with no items and no recent events must not be active");
        assert_eq!(project.primary_workspace_id.as_deref(), Some(workspace_id.as_str()));

        // A registered WebService item makes the project active — `Starting`
        // counts as "connected" (see `handle_project_list`'s doc comment),
        // so this doesn't need to wait for the readiness prober.
        let create_item = dispatch(
            &ctx,
            RpcRequest::ItemCreate {
                request_id: "r2".to_string(),
                workspace_id: workspace_id.clone(),
                item_type: ItemType::WebService,
                name: "web".to_string(),
                agent: None,
                command: Some(vec!["cat".to_string()]),
                port: Some(reserve_ephemeral_port()),
            },
        )
        .await;
        assert!(matches!(create_item, RpcResponse::ItemCreateOk { .. }), "expected ItemCreateOk, got {create_item:?}");

        let list_response = dispatch(&ctx, RpcRequest::ProjectList { request_id: "r3".to_string() }).await;
        let RpcResponse::ProjectListOk { projects, .. } = list_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected ProjectListOk, got {list_response:?}");
        };
        let project = projects.iter().find(|p| p.project_id == project_id).expect("project should still be listed");
        zellij_ops::kill_session(&name).await.ok();
        assert!(project.active, "a project with a connected WebService item must be active");
    }

    #[tokio::test]
    async fn project_list_recent_event_alone_makes_a_project_active() {
        let (_dir, ctx, name, workspace_id) = setup_m3_workspace("m-projlist-event").await;
        let project_id = ctx.registry.lock().await.find_workspace(&workspace_id).unwrap().project_id.clone();

        // No items registered at all — only an event was seen (the same
        // signal `serve.rs`'s `serve_dispatch` records for a real agent
        // event on its way out to the phone).
        ctx.registry.lock().await.record_event(&workspace_id, time::OffsetDateTime::now_utc());

        let list_response = dispatch(&ctx, RpcRequest::ProjectList { request_id: "r1".to_string() }).await;
        let RpcResponse::ProjectListOk { projects, .. } = list_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected ProjectListOk, got {list_response:?}");
        };
        let project = projects.iter().find(|p| p.project_id == project_id).expect("project should be listed");
        zellij_ops::kill_session(&name).await.ok();
        assert!(project.active, "a recent event with no items must still make a project active");
    }

    #[tokio::test]
    async fn project_set_primary_workspace_succeeds_reflects_in_list_and_rejects_a_mismatched_project() {
        let (_dir, ctx, name, workspace_id_a) = setup_m3_workspace("m-projprimary").await;
        let project_id = ctx.registry.lock().await.find_workspace(&workspace_id_a).unwrap().project_id.clone();

        // Register a second workspace directly under the same project (no
        // need for a real second `jj`/Zellij session — this exercises the
        // registry/RPC logic under test, not `jj workspace add`'s own
        // mechanics, which `jj_ops.rs`'s own tests already cover).
        let workspace_id_b = format!("ws-{}", uuid::Uuid::new_v4());
        ctx.registry
            .lock()
            .await
            .register_workspace(
                workspace_id_b.clone(),
                format!("agent-b-{}", uuid::Uuid::new_v4()),
                project_id.clone(),
                "app".to_string(),
                PathBuf::from(format!("/workspaces/{workspace_id_b}")),
                "2026-08-14T00:00:01Z".to_string(),
            )
            .unwrap();

        let switch_response = dispatch(
            &ctx,
            RpcRequest::ProjectSetPrimaryWorkspace {
                request_id: "r1".to_string(),
                project_id: project_id.clone(),
                workspace_id: workspace_id_b.clone(),
            },
        )
        .await;
        assert!(
            matches!(switch_response, RpcResponse::ProjectSetPrimaryWorkspaceOk { .. }),
            "expected ProjectSetPrimaryWorkspaceOk, got {switch_response:?}"
        );

        let list_response = dispatch(&ctx, RpcRequest::ProjectList { request_id: "r2".to_string() }).await;
        let RpcResponse::ProjectListOk { projects, .. } = list_response else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected ProjectListOk, got {list_response:?}");
        };
        let project = projects.iter().find(|p| p.project_id == project_id).expect("project should be listed");
        assert_eq!(
            project.primary_workspace_id.as_deref(),
            Some(workspace_id_b.as_str()),
            "project.list must reflect the new primary workspace"
        );

        // A workspace_id that belongs to a different project is rejected —
        // `invalid_argument`, not silently applied — and the earlier switch
        // is left untouched.
        let workspace_id_other_project = format!("ws-{}", uuid::Uuid::new_v4());
        ctx.registry
            .lock()
            .await
            .register_workspace(
                workspace_id_other_project.clone(),
                format!("unrelated-{}", uuid::Uuid::new_v4()),
                "proj-unrelated".to_string(),
                "unrelated".to_string(),
                PathBuf::from(format!("/workspaces/{workspace_id_other_project}")),
                "2026-08-14T00:00:02Z".to_string(),
            )
            .unwrap();
        let reject_response = dispatch(
            &ctx,
            RpcRequest::ProjectSetPrimaryWorkspace {
                request_id: "r3".to_string(),
                project_id: project_id.clone(),
                workspace_id: workspace_id_other_project,
            },
        )
        .await;
        assert!(
            matches!(&reject_response, RpcResponse::Error { code, .. } if code == "invalid_argument"),
            "expected invalid_argument for a workspace belonging to a different project, got {reject_response:?}"
        );

        let list_after_reject = dispatch(&ctx, RpcRequest::ProjectList { request_id: "r4".to_string() }).await;
        let RpcResponse::ProjectListOk { projects, .. } = list_after_reject else {
            zellij_ops::kill_session(&name).await.ok();
            panic!("expected ProjectListOk, got {list_after_reject:?}");
        };
        let project = projects.iter().find(|p| p.project_id == project_id).unwrap();
        assert_eq!(
            project.primary_workspace_id.as_deref(),
            Some(workspace_id_b.as_str()),
            "a rejected switch must not clobber the earlier successful one"
        );

        // An unknown workspace_id is not_found, not invalid_argument.
        let not_found_response = dispatch(
            &ctx,
            RpcRequest::ProjectSetPrimaryWorkspace {
                request_id: "r5".to_string(),
                project_id: project_id.clone(),
                workspace_id: "ws-does-not-exist".to_string(),
            },
        )
        .await;
        zellij_ops::kill_session(&name).await.ok();
        assert!(matches!(&not_found_response, RpcResponse::Error { code, .. } if code == "not_found"), "expected not_found, got {not_found_response:?}");
    }
}
