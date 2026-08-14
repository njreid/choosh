# Choosh delivery plan

Status date: 2026-08-14

This is the operational status ledger. [docs/milestones/README.md](docs/milestones/README.md)
remains the source of scope and exit gates; [docs/specs/README.md](docs/specs/README.md)
(being written alongside this ledger) remains the source of protocol and
verification detail. A checked box here means the named slice has evidence,
**not** that its enclosing milestone is complete.

## Current position

The architecture reset in [DESIGN.md](DESIGN.md) is written and the
milestone plan (M0–M8) is in place. There is no implementation evidence yet
against this architecture — the prior SSH-only, Git-based implementation
predated this design and was superseded, not carried forward or partially
credited here.

- [ ] [M0 — Enrollment skeleton](docs/milestones/M0-enrollment.md)
- [ ] [M1 — Workspace and jj foundation](docs/milestones/M1-workspace-and-jj.md)
- [ ] [M2 — Terminal and agent presence](docs/milestones/M2-terminal-and-agents.md)
- [ ] [M3 — jj diff and change graph](docs/milestones/M3-jj-diff-and-graph.md)
- [ ] [M4 — Safe source editing](docs/milestones/M4-editing.md)
- [ ] [M5 — Web preview and Markdown](docs/milestones/M5-web-and-markdown.md)
- [ ] [M6 — Laptop proxy and Zed bridge](docs/milestones/M6-laptop-and-zed.md)
- [ ] [M7 — Fleet, offload, and provisioning](docs/milestones/M7-fleet-and-provisioning.md)
- [ ] [M8 — Security and release](docs/milestones/M8-security-and-release.md)

## Next

M0 is the immediate target — nothing later is buildable before the
relay-brokered trust boundary exists. First concrete increments, in order:

1. `choosh-relayd`: WebAuthn RP (`webauthn-rs`) wired for a single
   passkey-registered identity, plus the presence registry and
   enrollment-token issuance it depends on.
2. `choosh-hostd`: enrollment-token exchange for a long-lived device
   credential, and the outbound dial-with-backoff connection to `relayd`.
3. Android: passkey registration/login via Credential Manager against
   `relayd`, and a minimal fleet list proving a connected devhost is
   visible.

Update this ledger whenever an increment materially changes completed
evidence, remaining gates, or the ordered next increments.
