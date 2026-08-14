# Milestones

Ordered delivery slices for the architecture in [DESIGN.md](../../DESIGN.md).
Each milestone names the working, demonstrable slice it adds and the exit
criteria that prove it — not a time estimate. [PLAN.md](../../PLAN.md) is the
status ledger that tracks evidence against these.

Milestones build strictly on their predecessors: nothing later works without
the relay-brokered connection (M0) and workspace/jj foundation (M1) in place
first. Within a milestone, independently-failable slices should still be
split into separate increments per [AGENTS.md](../../AGENTS.md)'s increment
workflow — a milestone is a checkpoint, not a single commit.

| # | Milestone | Adds |
| --- | --- | --- |
| [M0](M0-enrollment.md) | Enrollment skeleton | `relayd` up, passkey login, a devhost dials in and appears on the phone |
| [M1](M1-workspace-and-jj.md) | Workspace and jj foundation | Register a workspace, browse its file tree, backed by `jj-lib` |
| [M2](M2-terminal-and-agents.md) | Terminal and agent presence | A live agent terminal over the relay, normalized events, FCM `input_required` |
| [M3](M3-jj-diff-and-graph.md) | jj diff and change graph | Native diff and change-graph views, one-tap `jj undo` |
| [M4](M4-editing.md) | Safe source editing | Sora editing against the live working copy, conflict-safe saves |
| [M5](M5-web-and-markdown.md) | Web preview and Markdown | Tunneled dev servers, rendered Markdown |
| [M6](M6-laptop-and-zed.md) | Laptop proxy and Zed bridge | `ssh <devhost>` and Zed remote editing with zero manual trust steps |
| [M7](M7-fleet-and-provisioning.md) | Fleet, offload, and provisioning | Multi-devhost fleet view, cross-host task offload, SSO bridge, `mise` provisioning |
| [M8](M8-security-and-release.md) | Security and release | Threat-model pass, self-update, signed Obtainium releases |
