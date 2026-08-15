# Choosh delivery plan

Status date: 2026-08-15

This is the operational status ledger. [docs/milestones/README.md](docs/milestones/README.md)
remains the source of scope and exit gates; [docs/specs/README.md](docs/specs/README.md)
remains the source of protocol and verification detail. A checked box here
means the named slice has real, independently-verified evidence (real
tests against real binaries — `jj`, `zellij`, `mise`, `russh`, a real
Genymotion Android device, a real EC2 instance — not a design sketch).

## Current position

All nine milestones (M0–M8) have real implementation and verification
evidence. This does **not** mean the product is finished or bug-free — see
"Known follow-ups" below for the real, named gaps surfaced along the way.
No live `choosh-relayd` deployment exists yet: the Android app still
defaults to `FakeChooshEngine` (an in-memory stand-in), so no genuine
phone-to-relay-to-devhost round trip has been exercised end-to-end. Every
milestone's evidence is real at its own layer (real relayd/hostd
integration tests, real device builds) but the *whole system* has never
been run together against a live deployment.

- [x] [M0 — Enrollment skeleton](docs/milestones/M0-enrollment.md)
- [x] [M1 — Workspace and jj foundation](docs/milestones/M1-workspace-and-jj.md)
- [x] [M2 — Terminal and agent presence](docs/milestones/M2-terminal-and-agents.md)
- [x] [M3 — jj diff and change graph](docs/milestones/M3-jj-diff-and-graph.md)
- [x] [M4 — Safe source editing](docs/milestones/M4-editing.md)
- [x] [M5 — Web preview and Markdown](docs/milestones/M5-web-and-markdown.md)
- [x] [M6 — Laptop proxy and Zed bridge](docs/milestones/M6-laptop-and-zed.md)
- [x] [M7 — Fleet, offload, and provisioning](docs/milestones/M7-fleet-and-provisioning.md)
- [x] [M8 — Security and release](docs/milestones/M8-security-and-release.md)

Legacy pre-reset crates (`choosh-ssh`, `choosh-host`, `choosh-core`,
`chooshd`, `choosh-testkit` — the old SSH-only, Git-based implementation,
~34,700 lines) were removed during M6 once confirmed unreferenced by any
live crate and architecturally incompatible with M6's "trust the relay
tunnel, no client-side auth" posture.

## Known follow-ups and accepted risks

Not blocking, but real and worth tracking rather than leaving implicit:

- **No live `relayd` deployment.** First real end-to-end verification
  (phone → relay → devhost, `NativeChooshEngine` instead of the fake)
  hasn't happened. `just deploy` and its rollback path are verified
  against a real EC2 instance (`devhost`, used as a stand-in), but no
  dedicated `relayd` production instance exists yet.
- **No working device- or phone-passkey-revocation mechanism** (M8 threat
  model finding): `EnrolledDevice.revoked` is checked everywhere it
  matters, but nothing in the codebase ever sets it `true`, so a stolen
  laptop-proxy/devhost credential cannot currently be revoked. The same is
  true for phone passkey sessions — there is no revoke endpoint for
  `phone_sessions` either.
- **`relayd` has no per-Identity rate limiting**, despite
  `relay-protocol.md` requiring it (M8 threat model finding) — connection/
  request-flood DoS vectors are an accepted risk, not mitigated. Related:
  there is also no idle timeout on a connection stalled mid-frame (as
  opposed to a stalled *tunnel*, which is covered) — low severity given the
  bounded per-partial-frame buffer size, but an unbounded-connection-count
  vector combined with the point above.
- **Both ends of the FCM push path are stubs.** `relayd`'s
  `dispatch_fcm_push_stub` (`rust/choosh-relayd/src/ws.rs`) logs exactly
  what a real Firebase Cloud Messaging v1 API dispatch would send and
  returns as if it succeeded, since this environment has no `gcloud`/
  `firebase` CLI or GCP service-account credential to make a real call
  with; on the client, `ChooshFirebaseMessagingService.onMessageReceived`
  only logs receipt. Separately, the Android app's notification model
  (`ai.choosh.notifications.NotificationIntent`) only represents the
  `input_required` shape — there is no code path anywhere in the app that
  constructs, dedups, or renders an `auth_required` notification.
  Backgrounded-phone push notifications (`notifications.md`'s FCM path) do
  not actually reach a device yet, independent of the "no live `relayd`
  deployment" point above.
- **`choosh-hostd` shells out to the `jj` CLI instead of using `jj-lib`'s
  programmatic API** (`rust/choosh-hostd/src/jj_ops.rs`): a deliberate,
  reported deviation from `jj-integration.md`'s "embed `jj-lib` directly"
  design — `jj-lib` is a real, compiling dependency, but assembling its
  API for clone/workspace/diff correctly was judged more work than a
  single-pass increment could responsibly cover. Every invocation still
  uses a fixed executable and fully-encoded argv, never a shell string.
  Replacing this module's internals with real `jj-lib` calls behind the
  same RPC surface is a scoped follow-up.
- **`host-deployment.md`'s macOS power-assertion requirement is
  unimplemented** — no `IOPMAssertionCreateWithName`/IOKit call, or any
  other power-assertion mechanism, exists anywhere in
  `rust/choosh-hostd/src` or `scripts/install.sh`. macOS sleep severing the
  outbound `relayd` connection mid-task is untested and unmitigated; this
  host's development and verification has been Linux-only so far.
- **`agent-events.md`'s replay/sequencing machinery is unimplemented**:
  `choosh-hostd` forwards agent events through a single bounded in-memory
  channel (`serve.rs`'s `agent_event_tx`/`agent_event_rx`, a plain
  `tokio::sync::mpsc::channel(256)`) with no per-event sequence number, no
  per-workspace spool, no persistence across a `serve` restart, and no
  `snapshot_required` response anywhere in the wire types — the code's own
  comment at that channel's construction site calls this out explicitly. A
  reconnect resumes the live stream from whatever arrives next rather than
  detecting or filling a gap.
- **`host-rpc.md`'s `project.list`/`project.set_primary_workspace` RPCs
  have no implementation anywhere** — no `RpcRequest`/`RpcResponse` variant
  in `rust/choosh-protocol/src/host_rpc.rs`, no handler in
  `choosh-hostd`, and no real call site in the Android app: the fleet
  drawer's Project-mode rows are rendered entirely from
  `FleetFixtures.projectsFor(devHosts)` fixture data
  (`android/app/src/main/java/ai/choosh/fleet/FleetViewModel.kt`), not a
  live RPC. `hostd`'s registry already tracks a `project_id` internally
  (surfaced via `workspace.create`/`workspace.list`), but nothing exposes
  it as its own listable/settable resource yet.
- **Terminal accessibility**: the native terminal `SurfaceView` exposes no
  accessibility content at all (TalkBack/`uiautomator` sees nothing), and
  hardware-keyboard input (`TerminalSession.sendKey/sendText/paste/sendMouse`)
  is never wired to any Kotlin input path — only the demo-output buttons
  work today (M8 accessibility pass).
- **No adaptive layout** for tablet or DeX/external-display sizes;
  Explorer/Fleet/Workspace screens don't use extra width, and the terminal
  is locked to a hardcoded 80×24 grid rather than deriving from real
  surface size (M8 accessibility pass).
- **Custom Compose rows lack accessible labels** (Explorer, JjDiff,
  JjChangeGraph, Fleet — confirmed via `uiautomator dump`; TalkBack itself
  isn't installable on the sandboxed test device, so this substitution is
  itself a verification gap to close on a real device later).
- **Tab-focus and Escape/Back dialog-dismiss behavior is inconclusive, not
  a confirmed defect** (M8 accessibility pass): external `adb`-driven key
  events didn't visibly dismiss a dialog or move focus in the `androidTest`
  harness, but an in-process Compose-click dismiss and a general external
  tap both worked, pointing to a test-harness input-targeting quirk rather
  than a demonstrated production defect. Needs a follow-up check against
  the real, fully-launched app once `Screen.Connection` is reachable (e.g.
  via a real `relayd` deployment or a credential-provider-equipped device).
- **SSO device-code detection** is verified against real `aws`/`gh` CLI
  output; `az` is pattern-matched but not tested against a real binary;
  `gcloud auth login --no-launch-browser`'s real flow is structurally
  different (no short code printed) and the `gcp` detector arm is
  documented scaffolding, not a working match (M7 SSO bridge).
- **libghostty-vt was not wired in** despite a successful Zig
  cross-compile proof — the terminal engine ships a pure-Rust `vte`-backed
  parser instead, a deliberate scope decision from M2, not a dead end.
- **`workspace.file.read`'s 4 MiB range bound exceeds `MAX_TUNNEL_FRAME_BYTES`
  (256 KiB)** — a full-range read can't fit in one tunnel frame and neither
  side does multi-frame reassembly yet (flagged by the M5 Android fork,
  unfixed — affects large-file reads over the RPC tunnel).
- **`jj`/`zellij` currency checks (M7) don't redirect the rest of
  `choosh-hostd`'s existing `Command::new("jj"/"zellij")` call sites** to
  the `mise`-resolved binary — currency is checked and logged, but not yet
  plumbed through every invocation.
- **Obtainium discovery was verified against its documented contract**
  (`scripts/check-release-discovery.sh` passes against a real signed APK)
  but never against a real cut GitHub Release — no release has been
  published.
- **APK reproducibility is byte-identical except the signature block**
  (RSA-PSS's random per-signature salt) — a platform property, not a
  project bug; every other byte matches across independent builds.
- **`docs/evidence/zelland-source-audit.json` is missing from the working
  tree**, even though `docs/licenses/terminal-provenance.md` cites it as
  the authoritative record of the pinned Zelland source's Git tree/digest,
  and `scripts/check-terminal-provenance.sh` (run by `scripts/check-specs.sh`,
  part of `just check` and CI) hard-fails without it. The file was
  committed pre-reset (`94b3553`, "Reset architecture...") but never
  regenerated after — a real, CI-breaking gap in M8's "licence closure"
  exit criterion, not something to fabricate without redoing the actual
  source audit.

## Next

With every milestone's first pass landed, the natural next steps are
either (a) standing up a real `choosh-relayd` deployment and doing the
first genuine end-to-end phone/devhost round trip, or (b) working down the
list above. Neither is inherently ordered before the other — pick based on
what's actually blocking real use.

Update this ledger whenever an increment materially changes completed
evidence, remaining gates, or the ordered next steps.
