# M2 detailed design: agents and notifications

Status: Draft

This design refines [M2](../milestones/M2-agents-notifications.md). It is constrained by the [agent event specification](../specs/agent-events.md), [host protocol](../specs/host-protocol.md), [workspace item model](../specs/workspace-items.md), and [threat model](../threat-model.md).

## Outcome and boundary

Codex, OpenCode, and Claude Code remain unmodified terminal applications in dedicated managed Zellij tabs. Opt-in adapters observe supported lifecycle hooks and submit normalized events to `chooshd`; they never approve, deny, rewrite, delay, or inject agent operations. An incompatible or absent adapter leaves a fully usable terminal item with notification capability disabled.

The only host network boundary remains verified SSH. `chooshd` listens on its mode-`0600` per-user Unix socket, and Android reaches it through the SSH stdio bridge. Notification text never carries prompts, commands, tool arguments, file contents, host paths, or credentials.

## Owned state and interfaces

| State | Authority | Durable | Interface |
| --- | --- | --- | --- |
| Zellij tab, pane, PTY, process | Zellij | Yes | managed target IDs |
| Item identity, adapter compatibility, agent status | `chooshd` | Yes | workspace snapshot and sequenced events |
| Per-workspace next sequence, retained spool, client ack cursor | `chooshd` | Yes | `events.subscribe`, `events.ack` |
| Active SSH connection and subscription | Android Rust engine | No | command/event API to Kotlin |
| Last applied sequence and notification projection | Android local store | Yes | immutable snapshot transaction |
| Android notification record | Android OS, projected from local store | No | stable notification key |

An agent launcher sets `CHOOSH_WORKSPACE_ID`, `CHOOSH_ITEM_ID`, `CHOOSH_ROOT`, and `CHOOSH_AGENT`. `choosh-host emit --stdin` accepts exactly one UTF-8 JSON document conforming to [the agent event schema](../../protocol/v1/agent-event.schema.json), capped at 64 KiB, and writes no payload to logs. Missing Choosh environment makes a hook a successful no-op. Malformed, oversized, mismatched-workspace, mismatched-item, unknown-item, or wrong-agent submissions fail closed and do not enter the spool.

Adapters have a versioned compatibility manifest containing adapter ID, adapter version, supported agent/version range, and supported normalized event types. Launch records `compatible`, `incompatible`, or `unavailable` plus a bounded diagnostic code. Compatibility failure must not prevent terminal launch.

## Event ingestion and state machine

The daemon serializes all mutations for a workspace. For each accepted adapter submission it:

1. authenticates the local Unix-socket peer and validates the launcher identity;
2. validates and normalizes the payload, replacing adapter-provided time with a daemon receipt time for ordering;
3. canonicalizes every reported path beneath the registered workspace root, rejecting absolute paths, NULs, root escapes, and symlink escapes;
4. applies the agent transition;
5. assigns the next unsigned 64-bit workspace sequence and commits state plus event atomically;
6. wakes subscribers only after commit.

Allowed status transitions are:

```text
starting -> busy | waiting | failed | stopped
busy     -> busy | waiting | failed | stopped
waiting  -> busy | waiting | failed | stopped
failed   -> starting | stopped
stopped  -> starting
```

`input_required` sets `waiting` and creates or replaces the single outstanding wait record for the item. `turn_completed` may produce `waiting` with reason `next_prompt` only when the adapter contract declares that semantic. Prompt submission or an explicit busy event clears the wait record. Repeated equivalent events remain sequenced but project to one notification. Events for stopped items are rejected except a launcher-authorized `starting` event.

`files_changed.paths` are hints only. Accepted canonical root-relative paths are deduplicated and bounded to 1,000; rejected paths produce a counter and bounded diagnostic, never a partial path echo. Android refreshes `git.status` and displays only Git-confirmed changes. An entirely invalid list yields no changed-file projection.

## Spool, replay, and snapshot recovery

The spool is a durable per-workspace ordered log. Implementations must publish their negotiated limits in `welcome`; the M2 test profile uses 10,000 events and 7 days, removing the oldest entries when either bound is crossed. Sequence numbers are never reused after deletion or daemon restart.

`events.subscribe(workspace_id, after_sequence)` atomically returns one of:

- `subscribed`: the current snapshot revision, retained low/high sequence, then all events greater than `after_sequence` in order followed by live events;
- `snapshot_required`: when the cursor predates the retained low sequence or is ahead of the committed high sequence.

The client commits an event's state projection, last-applied sequence, and notification intent in one local transaction, then batches `events.ack` with the highest contiguous committed sequence. Ack is monotonic and idempotent; it is not permission to discard events needed by another registered client cursor. Duplicate `(workspace_id, sequence)` events are ignored. A gap stops application and triggers snapshot recovery rather than speculative reordering.

Snapshot recovery replaces the workspace/item projection, clears notifications not justified by a current wait record, stores the snapshot's high-water sequence, and subscribes after that sequence. Disconnect never changes host agent status.

## Background delivery and notification state

Timely background alerts require an explicit, user-started Android foreground service holding the verified SSH connection and event subscription. Its persistent system notification explains the connected host and stop action. Choosh does not claim delivery after the user stops it, Android/OEM kills it, the network is unavailable, or Doze suspends transport; on reconnection, retained events replay and current snapshot state repairs notifications. No FCM or public relay is introduced.

One notification is keyed by `(host_id, workspace_id, item_id)`. It exists exactly when the locally committed projection says the item is waiting with an outstanding `input_required`. Updates replace content in place. Only the workspace display name, agent display name, and enumerated coarse reason are rendered; adapter summaries are excluded from the system notification.

Activation carries stable IDs only and performs: connect and verify host key, open workspace, reconcile snapshot/events, ensure the exact item is pinned, focus its current Zellij target, and acknowledge the local notification intent. If host verification fails, no fallback connection occurs. If the item is absent or no longer waiting, the explorer opens with a stale-request message and the notification clears. Activation never sends terminal input or approves a request.

## Failure behavior

| Failure | Required behavior |
| --- | --- |
| Hook timeout/daemon unavailable | Hook exits within 250 ms in the test profile; agent continues; bounded local diagnostic only |
| Unsupported adapter version | Mark notification capability unavailable; preserve terminal operation |
| Spool full | Evict oldest committed entries; old cursors receive `snapshot_required` |
| Invalid path/event | Reject before sequencing; never expose raw payload in Android or logs |
| Duplicate/reordered frame | Ignore duplicate; recover snapshot on gap or regression |
| Android disconnect | Keep host state and spool; replay/reconcile after reconnect |
| Notification permission denied | Maintain waiting projection and in-app indicator; expose a machine-readable capability state |
| Agent/Zellij target gone | Mark item stopped or unknown through reconciliation; clear waiting notification |

## Headless verification

The repository test harness must run without Android UI, an installed agent, network access, wall-clock sleeps, or human input. It supplies fake adapters, an in-process daemon with a temporary state directory, a fake monotonic clock, a fake Git-status provider, a scripted Zellij façade, and a notification sink recording create/update/clear operations.

Deterministic fixtures cover every normalized event for every adapter, invalid/oversized payloads, redaction canaries, path traversal and symlink escape, duplicate delivery, cursor gaps, spool eviction, daemon restart, and adapter-version mismatch. Fixture timestamps, UUIDs, roots, and sequences are fixed. Golden output contains normalized events and notification intents, never platform-formatted notification text.

The minimum commands are:

```sh
cargo test -p chooshd agent_events
cargo test -p choosh-host adapters
cargo test -p choosh-protocol event_fixtures
./gradlew :app:testDebugUnitTest --tests 'ai.choosh.notifications.*'
```

Until those packages exist, CI must provide one equivalently named aggregate target and document the mapping. Tests must not invoke real agent executables or depend on real user configuration.

## Acceptance criteria

- Each supported adapter maps fixed permission, completion, file-change, and status fixtures to schema-valid normalized events; config installation is idempotent and preserves unrelated bytes/keys.
- A fake hook cannot block or alter the fake agent operation, and unavailable daemon delivery returns within the hook timeout.
- Across disconnect, spool eviction, duplicate replay, and daemon restart, the client projection equals a fresh snapshot and has no sequence gaps.
- Ten repeated `input_required` events for one item produce one create followed only by updates; a busy/stopped transition produces one clear.
- Redaction canaries placed in every adapter field, path, error, and prompt never occur in notification or diagnostic golden output.
- Absolute paths, `..`, NUL, and symlink escapes never reach Git reconciliation; valid paths are shown only after fake Git confirmation.
- Notification activation resolves the exact host/workspace/item and terminal target; stale IDs open the explorer and never focus a substitute.
- Adapter incompatibility passes a terminal attach/input transcript test while reporting notification capability unavailable.
- The foreground-service lifecycle test proves subscription ownership, explicit stop behavior, replay after forced disconnect, and the documented no-delivery state while stopped.
