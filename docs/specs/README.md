# Choosh specifications

These documents define the first implementable Choosh contracts.

Delivery sequencing and exit gates are defined in the [milestone plan](../milestones/README.md).
Implementation-level state machines, failure semantics, fixtures, and headless
acceptance harnesses are defined in the [detailed milestone designs](../design/README.md).

| Specification | Scope | Status |
| --- | --- | --- |
| [Android and Kotlin toolchain](android-toolchain.md) | Stable SDK/toolchain baseline, compatibility and update policy | Draft |
| [Android native runtime callbacks](android-native-runtime.md) | JNI socket/signer lease ownership, bounds and failure contract | Draft |
| [Terminal rendering and input](terminal-experience.md) | Zelland GPU port, Android IME, extra keys, gestures and recovery | Draft |
| [Host protocol](host-protocol.md) | SSH stdio framing, handshake, RPC, events, errors | Draft |
| [Native SSH reconnect](native-ssh-reconnect.md) | Re-admission, retry, generation invalidation, and recovery after network loss | Draft |
| [Workspace and item model](workspace-items.md) | Explicit workspaces and typed Zellij-backed items | Draft |
| [Agent interoperability](agent-events.md) | Codex, OpenCode, Claude hooks and notifications | Draft |
| [Client-side Git diff](git-diff.md) | Status, version retrieval, diff model, limits | Draft |
| [Development services](service-tunnels.md) | Explicit launch, lifecycle, SSH/WebView tunnel | Draft |
| [Android navigation](android-navigation.md) | Explorer, pinning, deep links, page restoration | Draft |
| [Diagnostics and support bundles](diagnostics.md) | Opt-in redacted local diagnostics and crash/support evidence | Draft |

## Normative language

The terms **MUST**, **MUST NOT**, **SHOULD**, **SHOULD NOT**, and **MAY** are interpreted as described by RFC 2119.

## Versioning

The protocol starts at major version `1`. Additive fields are permitted within a major version and must be ignored by older readers. Removing or changing field semantics requires a new major version.
