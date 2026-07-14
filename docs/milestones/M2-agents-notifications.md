# M2: Agents and notifications

## Outcome
Codex, OpenCode, and Claude Code are typed persistent items and reliably alert Android when input is required.

## Requirements
- **M2-R1:** Install opt-in, merge-safe, observational adapters for all three agents.
- **M2-R2:** Launch each agent in its own managed Zellij tab.
- **M2-R3:** Normalize input-required, turn-completed, files-changed, and status events.
- **M2-R4:** Bound and sequence the event spool; support subscribe, ack, replay, and snapshot recovery.
- **M2-R5:** Maintain one redacted Android notification per waiting agent.
- **M2-R6:** Notification activation connects, opens the workspace, pins/focuses the exact terminal, and acknowledges it.
- **M2-R7:** Reconcile reported paths with Git status and reject root escapes.
- **M2-R8:** Incompatible adapters fall back to terminal-only operation.

## Exit gate
Each agent alerts while backgrounded; taps reach the correct live TUI; duplicate/replayed events do not duplicate alerts; disconnected events replay in order.

## Excluded
Native chats, automatic approvals, and agent protocol replacement.

