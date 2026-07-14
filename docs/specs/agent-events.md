# Agent interoperability and events

Status: Draft

## Scope

Codex, OpenCode, and Claude Code run as unmodified interactive TUIs in dedicated managed Zellij tabs. Choosh adapters observe lifecycle events; they do not replace chat rendering or permission UX.

## Adapter contract

Agent launchers set:

```text
CHOOSH_WORKSPACE_ID
CHOOSH_ITEM_ID
CHOOSH_ROOT
CHOOSH_AGENT=codex|opencode|claude
```

Hooks ignore sessions without a complete Choosh environment. Hook installation is explicit, user-level, and merge-safe. Existing configuration MUST be preserved.

| Agent | Observed surfaces |
| --- | --- |
| Codex | `PermissionRequest`, `PostToolUse`, `Stop`, `UserPromptSubmit` |
| Claude Code | `PermissionRequest`, `Notification`, `FileChanged`, `PostToolUse`, `Stop`, `UserPromptSubmit` |
| OpenCode | `permission.asked`, `permission.replied`, `file.edited`, `session.diff`, `session.idle`, `session.error` |

Adapters MUST NOT approve, deny, rewrite, block, or inject model context. Agent-specific payloads are normalized before entering the daemon event spool.

## Normalized events

### `input_required`

Reasons: `approval`, `permission`, `question`, `elicitation`, or `next_prompt`. It contains the agent item ID and a coarse, non-sensitive summary. Commands, prompts, tool arguments, and file contents MUST NOT be forwarded to Android notifications.

### `turn_completed`

Marks the transition from busy to waiting for the next prompt. It MAY trigger Git reconciliation but is not itself a changed-file claim.

### `files_changed`

Contains root-relative candidate paths and optional operation hints. Paths are untrusted hints. The daemon canonicalizes them, and Android reconciles them with Git status where available.

### `agent_status`

Status: `starting`, `busy`, `waiting`, `stopped`, or `failed`. Failure details are bounded and redacted.

## Delivery and replay

Events receive a monotonically increasing sequence per workspace and are retained in a bounded spool. Android subscribes after its last acknowledged sequence. If the requested sequence is older than the retained window, the daemon returns `snapshot_required` and the client refreshes the full workspace/item state.

## Android notifications

- Only `input_required` produces a notification.
- Notifications are keyed by `(host_id, workspace_id, item_id)` and updated in place.
- Tapping connects if necessary, opens the workspace, ensures the agent is pinned, focuses its terminal page, and acknowledges the notification.
- Stale notifications are cleared when the agent leaves `waiting`, the item stops, or the workspace is terminated.

