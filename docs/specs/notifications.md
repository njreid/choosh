# Notifications

Status: Draft

See [DESIGN.md](../../DESIGN.md) §7 ("Push setup" and the FCM-vs-foreground-
service discussion). This spec covers delivery, redaction, dedup, and
actionability for events that reach the phone as Android notifications.

## Delivery mechanism

The Android app maintains a persistent relay connection only while its
process is alive (foreground or lightly backgrounded); it does not run a
foreground service to hold that connection open indefinitely. This is a
deliberate departure from the previous design, which required a persistent
foreground-service notification just to keep an SSH connection alive in the
background — a model that fights Android 14+ foreground-service-type
restrictions, Doze, and OEM process killers, and forces a permanent,
unwanted notification onto the user merely to stay reachable.

Instead:

- While the persistent connection is open, `choosh-relayd` delivers
  `agent-event` control frames (see [relay-protocol.md](relay-protocol.md))
  directly over it, and the app renders/dedupes notifications locally.
- When the connection is closed — app backgrounded long enough for the OS
  to have torn it down, or the process killed outright — `choosh-relayd`
  holds the phone's FCM registration token (registered at enrollment, see
  [auth-and-enrollment.md](auth-and-enrollment.md)) and sends a
  high-priority FCM data message for any event listed under "Notifying
  events" below. The data message MUST carry the same redacted payload
  shape the app would have rendered locally, not a bare "open the app" ping
  — Android can construct and show the notification, or wake the app to
  reconnect, without an extra round-trip to `relayd` to learn what happened.

`relayd` MUST NOT send an FCM message while it holds an open persistent
connection to that phone Identity — the persistent path is authoritative
when present, and duplicate delivery over both paths would double-notify.
(See the end of this document for the current implementation status of
this path.)

## Notifying events

Only two normalized events (see [agent-events.md](agent-events.md)) produce
a notification:

- `input_required` — an agent is blocked on the user.
- `auth_required` — a headless devhost needs the user to complete an SSO
  device-code flow.

`turn_completed`, `files_changed`, `agent_status`, and `editor_attached`/
`editor_detached` MUST NOT produce a notification; they update in-app state
silently.

## Redaction (normative)

A notification payload — whether rendered from the local relay connection
or reconstructed from an FCM data message — MUST contain only:

- workspace id and display name;
- agent id (for `input_required`) or provider name (for `auth_required`);
- a coarse enum reason: for `input_required`, one of `approval`,
  `permission`, `question`, `elicitation`, `next_prompt` (per
  [agent-events.md](agent-events.md)); for `auth_required`, the provider's
  `user_code` and `verification_uri` only, since those are meant to be
  shown to the user and are not secrets on their own.

A notification payload MUST NOT contain command text, tool arguments, file
contents, prompts, tokens, session identifiers, or any other credential
material, for either event type. This is a hard boundary, not a UX
preference — treat any code path that would place free-form agent or CLI
output into a notification string as a defect.

## Dedup

Notifications are keyed by `(host_id, workspace_id, item_id)` for
`input_required` and `(host_id, provider)` for `auth_required`. A new event
for the same key MUST update the existing notification in place rather than
creating an additional one. A notification is cleared when:

- the user opens the relevant terminal/auth flow from the notification
  (acknowledging it), or
- the underlying condition resolves on its own — the agent leaves
  `waiting` (per `agent_status`), or the SSO flow completes — even if the
  user never touched the notification.

## Actionability

`input_required` notifications MUST offer direct approve/reject actions
from the notification shade when the originating agent's hook surface
supports a structured response to that effect (see the per-agent adapter
table in [agent-events.md](agent-events.md)); acting on the notification
action MUST NOT require opening the app. Where the agent's hook surface has
no structured response path, the notification degrades to open-app-only —
tapping it still connects, pins, and focuses the right terminal per
[agent-events.md](agent-events.md)'s tap behavior, it just can't resolve
the block without the user typing into the agent directly.

`auth_required` notifications are always open-app-only: tapping opens the
`verification_uri` in a Custom Tab, per DESIGN.md §6.

**Not yet implemented**: the Android app's notification model
(`ai.choosh.notifications.NotificationIntent` and everything downstream of
it) only represents the `input_required` shape (mandatory `workspaceId`/
`itemId`/`agentName`, keyed `(host_id, workspace_id, item_id)`) — there is
no code path anywhere in the app that constructs, dedups, or renders an
`auth_required` notification (`(host_id, provider)`-keyed, no workspace/item
at all). Separately, both ends of the FCM path are stubs today: `relayd`'s
dispatch is a logged no-op (`rust/choosh-relayd/src/ws.rs`'s
`dispatch_fcm_push_stub`) and `ChooshFirebaseMessagingService.onMessageReceived`
only logs receipt — see [PLAN.md](../../PLAN.md)'s Known follow-ups.
