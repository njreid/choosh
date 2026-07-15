# Detailed milestone designs

These documents turn each [delivery milestone](../milestones/README.md) into an
implementable and independently verifiable contract. Headless verification is the
default: deterministic fixtures, injectable clocks and transports, structured
results, explicit bounds, and negative-path assertions. Android device tests are
reserved for platform behavior that has no meaningful headless oracle, and still
produce machine-readable evidence rather than relying on visual inspection.

| Milestone | Detailed design | Primary verification focus |
| --- | --- | --- |
| M0 | [Foundation and risk spikes](M0-foundation.md) | Hermetic testkit and falsifiable boundary spikes |
| M1 | [Remote workspace](M1-remote-workspace.md) | Black-box remote vertical-slice scenarios |
| M2 | [Agents and notifications](M2-agents-notifications.md) | Sequenced replay, projection, and adapter fixtures |
| M3 | [Pinning and services](M3-pinning-services.md) | Deterministic navigation, rebinding, and tunnel harnesses |
| M4 | [Editing and Git diff](M4-editing-git-diff.md) | Revision/conflict faults and bounded diff goldens |
| M5 | [Markdown review](M5-markdown-review.md) | Re-anchoring, asset security, and export round trips |
| M6 | [Security and release](M6-release.md) | Headless release gates, fault injection, and provenance |

The concise milestone files remain the source for delivery scope and requirement
IDs. These designs refine them; they do not relax their gates or the repository's
architecture constraints.
