# Choosh specifications

These documents define the first implementable Choosh contracts.

| Specification | Scope | Status |
| --- | --- | --- |
| [Host protocol](host-protocol.md) | SSH stdio framing, handshake, RPC, events, errors | Draft |
| [Workspace and item model](workspace-items.md) | Explicit workspaces and typed Zellij-backed items | Draft |
| [Agent interoperability](agent-events.md) | Codex, OpenCode, Claude hooks and notifications | Draft |
| [Client-side Git diff](git-diff.md) | Status, version retrieval, diff model, limits | Draft |
| [Development services](service-tunnels.md) | Explicit launch, lifecycle, SSH/WebView tunnel | Draft |
| [Android navigation](android-navigation.md) | Explorer, pinning, deep links, page restoration | Draft |

## Normative language

The terms **MUST**, **MUST NOT**, **SHOULD**, **SHOULD NOT**, and **MAY** are interpreted as described by RFC 2119.

## Versioning

The protocol starts at major version `1`. Additive fields are permitted within a major version and must be ignored by older readers. Removing or changing field semantics requires a new major version.

