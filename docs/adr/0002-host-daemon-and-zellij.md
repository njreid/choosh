# ADR 0002: Host daemon and Zellij responsibilities

Status: Accepted

## Decision

`chooshd` owns explicit workspace registration and typed item metadata. Zellij owns PTYs, processes, scrollback, tabs, and survival across Android disconnects. Workspace name equals Zellij session name.

Agents and development services are created through `chooshd`, each in a dedicated managed tab. No Zellij WASM plugin is required for V1.

## Consequences

- Discovery is reliable and does not depend on terminal parsing or process guessing.
- Arbitrary pre-existing Zellij tabs are not first-class Choosh items.
- A future tmux backend can implement the same internal session interface.

