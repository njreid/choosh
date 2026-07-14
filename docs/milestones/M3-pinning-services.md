# M3: Explorer, pinning, and services

## Outcome
Explorer is page zero; agents, documents, diffs, and declared web services form an ordered swipeable pin set.

## Requirements
- **M3-R1:** Explorer sections are agents, services, changed files, project tree.
- **M3-R2:** Row taps toggle stable pins; page interaction never unpins.
- **M3-R3:** Restore pin order after rotation/process death/reconnect without substituting unavailable targets.
- **M3-R4:** Rebind one retained native terminal renderer while every remote TUI continues in Zellij; clear composition/modifiers and prevent stale frames or input crossing targets.
- **M3-R5:** Arbitrate horizontal gestures with terminal, Sora, and WebViews.
- **M3-R6:** `choosh service run` requires workspace/name/protocol/port/command and creates a typed tab.
- **M3-R7:** Pin/unpin is separate from start/stop; show readiness/status.
- **M3-R8:** Authenticated loopback gateway supports bounded HTTP, WebSocket, and SSE over direct-tcpip.
- **M3-R9:** Service WebView shares no bridge, files, cookies, or token with internal Markdown.

## Exit gate
Two agents remain interactive while switching; pins restore in order; a web service survives unpin/reconnect with hot reload; requests without the gateway cookie fail before SSH forwarding.

## Excluded
Process/port inference, arbitrary browsing, HTTPS certificate policy, and cross-device pin sync.
