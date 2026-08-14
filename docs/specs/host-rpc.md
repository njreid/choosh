# Host RPC

Status: Draft

## Purpose

`choosh-hostd` exposes one RPC surface to the Android app, and a narrower
slice of it to the laptop proxy where noted. RPC requests are not a
separate connection: they are one control-frame class multiplexed over the
same relay-brokered channel as everything else, scoped to a single devhost
by the tunnel they ride. See [relay-protocol.md](relay-protocol.md) for the
outer envelope, frame types, and how a phone Identity opens the tunnel that
carries this traffic in the first place. See [jj-integration.md](jj-integration.md)
for the `workspace.tree.*`, `workspace.file.*`, `workspace.diff`,
`workspace.log`, and `workspace.op.*` methods — this document does not
redefine them. See [service-tunnels.md](service-tunnels.md) for the tunnel
side of a `WebService` item — this document defines only its registration
RPC.

## Transport

Each RPC request is a control frame: a 4-byte unsigned big-endian length
prefix followed by a UTF-8 JSON payload, per the envelope in
[relay-protocol.md](relay-protocol.md). A request MUST carry a
client-generated request ID; the matching response MUST echo it. Requests
and responses for one phone Identity's connection to one devhost are
ordered per-tunnel but MAY interleave with PTY and other tunnel traffic on
the same underlying relay connection — `hostd` MUST NOT assume RPC frames
arrive back-to-back.

- Maximum control-frame payload: 1 MiB, matching the bound the old SSH-era
  RPC channel used. A request or response that would exceed this MUST be
  rejected/truncated at the boundary, never silently split across frames.
- A zero-length frame, invalid UTF-8, malformed JSON, or an unknown
  envelope kind terminates the RPC channel for that tunnel.

## Registry RPCs

### `workspace.create`

Registers a Project (if not already known) and a Workspace: creates a `jj`
workspace and a same-named Zellij session on the target devhost.

Request fields:

| Field | Required | Description |
| --- | --- | --- |
| `devhost_id` | yes | Target devhost's Identity, from the fleet list. |
| `workspace_name` | yes | MUST satisfy Zellij's session-name rules and be unique on the devhost. Becomes the `jj` workspace name and the Zellij session name. |
| `project_source` | yes | One of `{ "clone_url": <string> }` (fresh clone) or `{ "existing_path": <string> }` (adopt an already-cloned repo on that devhost, root-confined under the devhost's configured workspaces directory). |
| `parent_workspace_id` | no | See [jj-integration.md](jj-integration.md) for the "one workspace per agent" mechanism this drives. |

`hostd` MUST validate `workspace_name` and reject collisions with an
existing, differently-sourced workspace rather than silently adopting it —
adoption of a pre-existing same-named Zellij session (e.g. left over from a
previous `hostd` restart) requires the same explicit-confirmation posture
the pre-relay design applied to Git workspaces.

### `workspace.list`

Returns every registered Workspace on the requesting Identity's reachable
devhost set (for the Android fleet view, this is called once per devhost
after `list-devhosts`, per [relay-protocol.md](relay-protocol.md)). Each
entry: `{ workspace_id, workspace_name, devhost_id, project_id, created_at }`.

### `workspace.status`

Returns the current changed-files summary and conflict flags for a
Workspace's live working copy (`@`). Full shape defined in
[jj-integration.md](jj-integration.md); referenced here because it is also
the primary signal the Android explorer's changed-files section polls.

## Item RPCs

Items are typed things living inside a Workspace's Zellij session. The
fixed set of item type names, used verbatim across every spec and the
Android client, is: `AgentTerminal`, `Shell`, `WebService`,
`JjChangeGraph`, `JjDiff`, `SourceEditor`, `MarkdownPreview`,
`EditorPresence`. Of these, only `AgentTerminal`, `Shell`, and `WebService`
correspond to an actual Zellij tab with a live process; the rest are
client-side projections over other RPCs/events and have no `item.create`
call of their own.

### `item.create`

| Field | Required | Description |
| --- | --- | --- |
| `workspace_id` | yes | |
| `item_type` | yes | `AgentTerminal`, `Shell`, or `WebService`. |
| `name` | yes | Unique within the workspace; becomes the Zellij tab name. |
| `agent` | if `AgentTerminal` | `codex`, `claude`, or `opencode` — selects the launcher that sets `CHOOSH_WORKSPACE_ID`/`CHOOSH_ITEM_ID`/`CHOOSH_ROOT`/`CHOOSH_AGENT` per [agent-events.md](agent-events.md). |
| `command` | if `WebService` | Fixed argv, never a shell string. See "Command construction" below. |
| `port` | if `WebService` | The port the launched process is declared to listen on. `hostd` does not infer this. |

`hostd` MUST create a dedicated Zellij tab for the item before returning
success; a request that fails to create the tab MUST NOT leave a
partially-registered item record.

### `item.list`

Returns every registered item for a Workspace: `{ item_id, item_type,
name, tab_target, status, port? }`. `status` for `AgentTerminal` mirrors
the `agent_status` values in [agent-events.md](agent-events.md); for
`WebService` it is `running` or `stopped`, set only by an explicit
`item.stop`, never inferred from process/port state.

### `item.stop`

Explicit, separate from unpinning (which only closes the client's tunnel
to the item, per [service-tunnels.md](service-tunnels.md)). Stops the
underlying process and marks the item `stopped`; the item record and its
Zellij tab's scrollback are retained until explicitly removed.

## Root confinement

Every RPC that names a filesystem path (all of `workspace.tree.*`,
`workspace.file.*`, plus `project_source.existing_path` above) MUST
canonicalize the resulting absolute path and verify it falls under the
Workspace's registered root before performing any read, write, or
directory listing. A path that resolves outside the root — via `..`
traversal, a symlink, or otherwise — MUST be rejected with a typed error,
never silently clamped to the root boundary. This is the same discipline
the pre-relay design applied to SFTP; it is restated normatively here
because it now also governs the `jj`-backed file APIs in
[jj-integration.md](jj-integration.md).

## Command construction

`hostd` MUST build every subprocess invocation — `jj`, `mise`, Zellij
control, and any `WebService` launch command — from a fixed executable
path and a separately encoded argument vector. User- or agent-supplied
text (workspace names, file paths, service commands, agent prompts) MUST
NOT be interpolated into a shell string at any point. A launcher that
needs a shell for legitimate reasons (e.g. a `WebService`'s `command` is
itself `["sh", "-c", "npm run dev"]`) MUST treat that as the caller's
explicit, singular argv entry — `hostd` still never performs its own
string interpolation to construct it.

## Bounds

| Limit | Value |
| --- | --- |
| Control-frame payload | 1 MiB |
| `workspace.tree.list` page size | 500 entries, cursor-paginated |
| `workspace.file.read` range | 4 MiB per request; larger files require multiple ranged requests |
| Directory traversal depth per `tree.list` call | one level; recursion is client-driven |

A request exceeding a bound MUST be rejected with a typed, bounded error
response — never partially served and never causing unbounded host-side
allocation.

## Error model

RPC errors are typed, not free-text: `{ request_id, error: { code,
message } }` where `code` is a fixed enum (`not_found`,
`out_of_root`, `invalid_argument`, `conflict`, `revision_stale`,
`bound_exceeded`, `internal`). `message` MUST NOT contain file contents,
command text, or credentials — the same redaction discipline
[agent-events.md](agent-events.md) applies to notifications applies here
too, since RPC errors can end up surfaced in the UI. A failed request MUST
NOT leave partial state: `workspace.create`, `item.create`, and
`workspace.file.write` are all effectively transactional from the caller's
perspective — either the full effect happened, or none of it did.
