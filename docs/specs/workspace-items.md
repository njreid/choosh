# Workspace and item model

Status: Draft

## Workspace invariant

A workspace is explicitly registered. It consists of:

```text
stable workspace ID
user-chosen name
canonical project root
Zellij session with exactly the same name
```

The daemon MUST NOT discover workspaces by scanning the filesystem. Registration MUST canonicalize and validate the root, enforce a unique name per host user, and either create the same-named Zellij session or require explicit adoption of an existing session.

Deleting a workspace registration and terminating its Zellij session are separate operations.

## Typed items

Every managed Zellij tab has a stable item ID and one kind:

| Kind | Purpose | Required metadata |
| --- | --- | --- |
| `agent` | Interactive coding-agent TUI | agent kind, tab target, status |
| `service` | Explicit development daemon | tab target, port, protocol, status |
| `terminal` | Ordinary managed shell | tab target, status |

Zellij owns the PTY and process. `chooshd` owns item identity, type, display name, lifecycle metadata, and the mapping to a Zellij tab/pane target.

Unknown or unmanaged Zellij tabs MUST NOT be silently classified as agents or services. They MAY appear in a diagnostic/import flow later.

## Status

Item status is one of:

```text
starting, running, waiting, stopped, failed, unknown
```

`waiting` means an agent requires input. Status transitions are events and include a monotonically increasing workspace sequence.

## Client-only pin state

Pin order is Android client state, not daemon state. A pinned descriptor contains `workspace_id`, item/file identity, view kind, and view-specific options. The client MAY synchronize pins in a later version, but V1 stores them locally.

## Reconciliation

After reconnect, Android requests a full workspace/item snapshot and then subscribes from its last acknowledged event sequence. Snapshot revision plus event sequence prevents gaps. Items missing from the new snapshot are marked unavailable before their pages are removed or offered for retry.

