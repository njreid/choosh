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

**Implementation note**: the sequencing/spool/resume machinery above is
implemented in `choosh-hostd` (`rust/choosh-hostd/src/agent_event_spool.rs`
for the per-workspace sequencer/spool; `rust/choosh-hostd/src/serve.rs`'s
`agent_event_rx` branch of `serve_dispatch` for where every locally-emitted
event gets sequenced before its live `agent-event` control frame is sent).
The `agent_event_tx`/`agent_event_rx` channel this paragraph used to
describe as the *entire* delivery mechanism still exists, unchanged — it's
still how every event producer (`choosh-hostd emit`, the pty auth-code
detector, the SSH bridge's editor-presence hooks, the self-update failure
report) reaches `serve_dispatch` — but it now feeds the spool as well as
the live send, so a reconnect never has to rely on "whatever arrives next"
alone.

The resume request/response ride their own `"agent-events"`-purpose
`open-tunnel` (relay-protocol.md's tunnel mechanism), opened by a phone
directly to the devhost that owns the workspace being resumed — the same
shape a phone already uses to open an `"rpc"`-purpose tunnel for
`host-rpc.md` traffic, just a distinct purpose tag and payload shape
(`choosh_protocol::relay::AgentEventsResumeRequest`/
`AgentEventsResumeResponse`), handled directly in `choosh-hostd::serve`
rather than through `choosh-hostd::rpc`'s `host-rpc.md` dispatch. See
`rust/choosh-protocol/src/relay.rs`'s module doc comment for the fuller
rationale (this vs. a new `host-rpc.md` RPC method vs. a new
`relayd`-side workspace-to-devhost routing capability).

**Real, remaining gap**: still no on-disk persistence across a `serve`
restart (`PLAN.md`) — the spool is in-memory only, scoped to one `serve`
process's lifetime, same as `agent_event_tx`/`agent_event_rx` always were.
A restart is answered by `snapshot_required` (the spool has no record of
any workspace immediately after starting), not a silent gap, but it is not
the persistence a genuine multi-day event history would need.

## Android notifications

Notification behavior (redaction, dedup, delivery mechanism) is specified
in full in [notifications.md](notifications.md). In short: only
`input_required` and `auth_required` produce a notification; both are keyed
by `(host_id, workspace_id, item_id)` and updated in place; tapping
connects if necessary, opens the workspace, ensures the relevant item is
pinned, focuses it, and acknowledges the notification.
