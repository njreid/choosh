# Agent interoperability and events

Status: Draft

See [DESIGN.md](../../DESIGN.md) §6 ("Agent event bridge") and §7 for the
surrounding architecture. This spec covers the adapter contract and the
normalized event set only.

## Scope

Codex, OpenCode, and Claude Code run as unmodified interactive TUIs in
dedicated managed Zellij tabs. Choosh adapters observe lifecycle events;
they do not replace chat rendering or permission UX.

## Adapter contract

Agent launchers set:

```text
CHOOSH_WORKSPACE_ID
CHOOSH_ITEM_ID
CHOOSH_ROOT
CHOOSH_AGENT=codex|opencode|claude
```

Hooks MUST ignore sessions without a complete Choosh environment. Hook
installation is explicit, user-level, and merge-safe. Existing
configuration MUST be preserved.

| Agent | Observed surfaces |
| --- | --- |
| Codex | `PermissionRequest`, `PostToolUse`, `Stop`, `UserPromptSubmit` |
| Claude Code | `PermissionRequest`, `Notification`, `FileChanged`, `PostToolUse`, `Stop`, `UserPromptSubmit` |
| OpenCode | `permission.asked`, `permission.replied`, `file.edited`, `session.diff`, `session.idle`, `session.error` |

Adapters MUST NOT approve, deny, rewrite, block, or inject model context.
Agent-specific payloads are normalized by `choosh-hostd` before entering the
event bus below.

## Normalized events

### `input_required`

Reasons: `approval`, `permission`, `question`, `elicitation`, or
`next_prompt`. Contains the agent item ID and a coarse, non-sensitive
summary. Commands, prompts, tool arguments, and file contents MUST NOT be
forwarded to Android notifications — see [notifications.md](notifications.md)
for the redaction rule this feeds.

### `turn_completed`

Marks the transition from busy to waiting for the next prompt. MAY trigger
`jj`-backed status reconciliation (`workspace.status`, see
[jj-integration.md](jj-integration.md)) but is not itself a changed-file
claim.

### `files_changed`

Contains root-relative candidate paths (`WireAgentEvent::FilesChanged`'s
only field beyond `workspace_id`/`item_id` is `paths: Vec<String>` — no
separate operation-hint field exists on the wire). Paths are untrusted
hints: `choosh-hostd` canonicalizes them under the workspace
root, and Android reconciles them against `workspace.status`
([jj-integration.md](jj-integration.md)) rather than trusting the agent's
claim directly — jj replaces Git here, but the "hints, not authority"
posture is unchanged.

### `agent_status`

Status: `starting`, `busy`, `waiting`, `stopped`, or `failed`. Failure
details are bounded and redacted.

### `auth_required`

A headless devhost detected a device-code SSO/cloud-CLI auth flow (`aws sso
login`, `gcloud auth login`, `az login`, `gh auth login`) with no local
browser to hand off to (see [ssh-bridge-and-zed.md](ssh-bridge-and-zed.md)'s
sibling concerns around headless-vs-local-display detection are out of
scope for this file; `choosh-hostd`'s provisioning behavior owns that
decision). Payload is exactly:

```json
{ "provider": "aws|gcp|azure|github", "user_code": "WDJB-MJHT", "verification_uri": "https://..." }
```

No token, credential, or session identifier MUST ever appear in this event.

### `editor_attached` / `editor_detached`

A laptop-side Zed remote session attached to or detached from a workspace
(see [ssh-bridge-and-zed.md](ssh-bridge-and-zed.md) §"Handoff" for when
`choosh-hostd` emits this). Payload is exactly:

```json
{ "workspace_id": "...", "editor": "zed" }
```

Purely presence information for Android's read-only `EditorPresence` item
(DESIGN.md §7) — it MUST NOT carry file paths, buffer contents, or anything
else Zed's own protocol exchanges over the tunnel.

## Delivery and replay

Events are `agent-event` control frames on the Identity's relay connection
(see [relay-protocol.md](relay-protocol.md) for frame shape) — there is no
longer a host-side spool drained over an SSH-stdio RPC channel. Each event
MUST receive a monotonically increasing sequence number per workspace and
MUST be retained in a bounded per-workspace spool inside `choosh-hostd`.
Android MUST subscribe to the relay's `agent-event` stream for a workspace
starting after its last acknowledged sequence. If the requested sequence is
older than the retained window, `choosh-hostd` MUST return
`snapshot_required` and the client refreshes full workspace/item state via
`host-rpc.md`'s `workspace.status`/`workspace.list` instead of replaying
stale events.

A reconnect after any gap — network loss, app backgrounding, or the relay
connection itself cycling — MUST resume from the last acknowledged sequence
or fall back to `snapshot_required`; it MUST NOT silently drop events.

**Not yet implemented**: `choosh-hostd` currently forwards agent events
through a single bounded, in-memory channel
(`rust/choosh-hostd/src/serve.rs`'s `agent_event_tx`/`agent_event_rx`, a
plain `tokio::sync::mpsc::channel(256)`) with no per-event sequence number,
no per-workspace spool, no on-disk persistence across a `serve` restart,
and no `snapshot_required` response anywhere in the wire types — the code's
own comment at that channel's construction site calls this out explicitly
as a real, documented gap versus the replay/sequence machinery described
above. A reconnect today simply resumes the live event stream from
whatever arrives next; it does not detect or fill a gap.

## Android notifications

Notification behavior (redaction, dedup, delivery mechanism) is specified
in full in [notifications.md](notifications.md). In short: only
`input_required` and `auth_required` produce a notification; both are keyed
by `(host_id, workspace_id, item_id)` and updated in place; tapping
connects if necessary, opens the workspace, ensures the relevant item is
pinned, focuses it, and acknowledges the notification.
