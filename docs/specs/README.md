# Specifications

Normative detail for the architecture in [DESIGN.md](../../DESIGN.md).
[docs/milestones/](../milestones/README.md) says what gets built in what
order and how each slice is proven; these documents say precisely how each
piece behaves so two independent implementations of the same spec would
interoperate. RFC 2119 terms are normative here; DESIGN.md's prose is not.

## Trust boundary and transport

- [relay-protocol.md](relay-protocol.md) — the wire protocol between
  `choosh-relayd` and every Identity: framing, control frames, presence,
  and tunnel lifecycle. Every other spec's transport assumptions trace back
  to this one.
- [auth-and-enrollment.md](auth-and-enrollment.md) — passkeys for humans,
  device credentials for machines: the WebAuthn RP flow, enrollment-token
  issuance, the devhost/laptop enrollment chain, and revocation. See also
  [relayd-threat-model.md](#related-verification-reports) below.

## Host daemon

- [host-rpc.md](host-rpc.md) — the RPC surface `choosh-hostd` exposes to
  the Android app: workspace/item registry, bounds, error model, and
  command-construction discipline.
- [jj-integration.md](jj-integration.md) — browsing and editing a `jj`
  workspace: revision resolution, the `workspace.tree.*`/`workspace.file.*`/
  `workspace.diff`/`workspace.log`/`workspace.op.*` RPCs, conflict
  representation, and one-workspace-per-agent.
- [agent-events.md](agent-events.md) — the observational hook adapter
  contract and normalized event set (`input_required`, `turn_completed`,
  `files_changed`, `agent_status`, `resource_reauth_required`,
  `editor_attached`/`editor_detached`).
- [notifications.md](notifications.md) — FCM delivery, redaction, and
  dedup rules for those events once they need to reach a backgrounded
  phone.
- [resources-and-reauth.md](resources-and-reauth.md) — the `Resource`
  entity (named, typed, devhost-attached references to external
  infrastructure needing occasional re-authentication), the re-auth
  interaction patterns (a/b/c/d) it generalizes, and the
  `resource_reauth_required` event above.
- [service-tunnels.md](service-tunnels.md) — explicit dev-service launch
  and the `WebService` tunnel, including the Zellij-web-client break-glass
  path.
- [ssh-bridge-and-zed.md](ssh-bridge-and-zed.md) — `choosh-hostd`'s
  loopback SSH server, the laptop `choosh-hostd proxy` mode, and Zed
  remote-editing attachment.
- [toolchain-provisioning.md](toolchain-provisioning.md) — project-pinned
  vs. host-managed `mise` tooling, and the `ubi` backend path for
  GitHub-release binaries like `zed-remote-server`.
- [host-deployment.md](host-deployment.md) — the bootstrap install script,
  platform service lifecycle (`systemd --user` / `launchd`), and
  `choosh-hostd` self-update.

## Android app

- [android-navigation.md](android-navigation.md) — the
  `Explorer → PinnedItem*` shell and the fixed item-type set.
- [terminal-experience.md](terminal-experience.md) — the Zelland-derived
  native terminal renderer, rebound to relay tunnel frames.
- [editor-protocol.md](editor-protocol.md) — Sora's revisioned edit
  protocol against the jj-backed file RPCs.
- [android-native-runtime.md](android-native-runtime.md) — the JNI
  boundary's bounded-capability contract, admitted by relay session
  credentials rather than SSH host-key verification.
- [android-toolchain.md](android-toolchain.md) — the pinned-stable-release
  policy for the Android/Kotlin build.

## Related verification reports

These aren't specs — they're evidence documents written against the specs
above, reviewing how well the real implementation matches them. See
[PLAN.md](../../PLAN.md) for the status ledger these feed into.

- [../security/relayd-threat-model.md](../security/relayd-threat-model.md) —
  the M8 `choosh-relayd` threat-model review: identity, enrollment,
  credential compromise, tunnel isolation, and availability, each traced to
  specific code and tested (or named as an accepted risk).
- [../accessibility-device-report.md](../accessibility-device-report.md) —
  the M8 hands-on accessibility/device pass against a real Android device:
  screen reader, hardware keyboard, DeX/tablet layout, and low-memory
  behavior.

## Conventions

- Every spec starts with a `Status:` line.
- Links between repository-owned documents are relative.
- Examples MUST NOT contain real credentials, hostnames, or user paths.
- A spec that depends on another's wire shape links to it rather than
  redefining it — `relay-protocol.md` and `jj-integration.md` in
  particular are linked from most other documents here and are the ones
  most likely to need a coordinated update if they change.
