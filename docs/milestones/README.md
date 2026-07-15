# Delivery milestones

Milestones are vertical, testable increments. A milestone is complete only when every mandatory requirement and exit scenario passes on macOS/arm64, Linux/x86_64, and the stated Android matrix.

```mermaid
flowchart TD
 M0["M0: Foundation"] --> M1["M1: Remote workspace"]
 M1 --> M2["M2: Agents"] --> M3["M3: Pinning and services"]
 M3 --> M4["M4: Editing and diff"] --> M5["M5: Markdown review"] --> M6["M6: Release"]
```

| Milestone | Outcome | Detailed design | Channel |
|---|---|---|---|
| [M0](M0-foundation.md) | High-risk boundaries proven | [Design](../design/M0-foundation.md) | Internal |
| [M1](M1-remote-workspace.md) | Usable SSH/Zellij/file/Markdown slice | [Design](../design/M1-remote-workspace.md) | Developer preview |
| [M2](M2-agents-notifications.md) | Three agents and Android alerts | [Design](../design/M2-agents-notifications.md) | Internal alpha |
| [M3](M3-pinning-services.md) | Final pinning UX and web previews | [Design](../design/M3-pinning-services.md) | Alpha |
| [M4](M4-editing-git-diff.md) | Safe editing and native Git review | [Design](../design/M4-editing-git-diff.md) | Alpha |
| [M5](M5-markdown-review.md) | Annotatable project documents | [Design](../design/M5-markdown-review.md) | Beta |
| [M6](M6-release.md) | Hardened Obtainium-compatible release | [Design](../design/M6-release.md) | Public 1.0 |

Every milestone MUST update affected specs/ADRs, test deterministic behavior, include an end-to-end acceptance scenario, preserve SSH/root confinement, bound all resources, and leave no ignored failing tests.
