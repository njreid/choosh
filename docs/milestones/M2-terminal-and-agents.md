# Milestone 2 — Terminal and agent presence

Proves a live, interactive agent terminal over a relay-brokered tunnel, and
the normalized event/notification pipeline that makes background
`input_required` alerts real.

## Scope

- PTY bytes carried as relay tunnel frames (§2.3, §3): attaching to a
  Zellij pane no longer assumes a direct SSH channel.
- Native terminal renderer ported from Zelland (`wgpu`/`glyphon` +
  `libghostty-vt`) rebound to the relay transport instead of an SSH PTY
  channel; Android IME extra-keys bar.
- `AgentTerminal` item: attach/detach/reattach to a Zellij tab; multiple
  agents in one workspace each in their own tab.
- Observational hook adapters for Codex, Claude Code, and OpenCode emitting
  the normalized event set (`input_required`, `turn_completed`,
  `files_changed`, `agent_status`) over the relay event bus.
- FCM wiring: `relayd` holds the phone's registration token; an
  `input_required` event while the phone's persistent connection is closed
  triggers a high-priority FCM push.

## Exit criteria

- Attaching, backgrounding the app, and reattaching to an agent's terminal
  preserves scrollback and the agent's live state — the agent process never
  notices the phone left.
- Each of the three supported agents produces at least one real
  `input_required` and one `files_changed` event from its own hook
  surface, not a fixture.
- With the app fully backgrounded (process killed by the OS), an
  `input_required` event still produces a notification within the FCM
  delivery window, and tapping it attaches to the correct agent's terminal.
- Notification text never contains command text, file contents, or
  credentials — verified by a redaction test, not inspection.
