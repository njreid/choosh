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
- **`relayd`'s registry (`devices`, `tokens`, `phone_sessions`, `fcm_tokens`)
  is in-memory only, with no disk persistence beyond the enrollment CA key
  itself** — `rust/choosh-relayd/src/state.rs`'s crate-level doc comment
  already flags this as a deliberate M0 decision, confirmed still current: a
  `relayd` restart forgets every enrolled device, phone session, and issued
  token. This is a real, live limitation, not just a documentation note:
  `authenticate_device` rejects an unknown `device_id` outright ("unknown
  device"), so any device that doesn't survive a restart must fully
  re-enroll rather than silently being trusted on reconnect — which is also
  why device/phone-session revocation (`RevokeDevice`/`RevokePhoneSession`,
  see `rust/choosh-relayd/src/ws.rs`) only needs to hold for one running
  `relayd` process's lifetime, not survive a restart: a restart already
  invalidates every credential in the fleet, a strictly stronger reset than
  any specific revoke. Not blocking today (no live `relayd` deployment
  exists yet), but worth tracking before `relayd` is trusted as a
  long-running production service — a restart mid-incident would silently
  un-revoke everything that had been revoked before it.
- **Both ends of the FCM push path now have real implementations, but
  neither has been exercised end-to-end against a real device.** `relayd`'s
  `crate::fcm::FcmClient` (`rust/choosh-relayd/src/fcm.rs`, replacing the
  old `dispatch_fcm_push_stub`) is a real FCM v1 dispatcher: service-account
  JWT-bearer OAuth2 token exchange (RS256-signed via `openssl`, since this
  workspace has no `jsonwebtoken` dependency) and the actual
  `POST .../messages:send` call via `reqwest`, falling back to the same
  logging-only behavior the old stub had when no credential is configured.
  On the Android side, `ai.choosh.notifications.AuthNotificationIntent`/
  `RenderableNotification`/`FcmNotificationParser` now give `auth_required`
  the same construct/dedup/render path `NotificationIntent` gives
  `input_required` — keyed `(host_id, provider)` per notifications.md, with
  no direct actions (always open-app-only) — and
  `ChooshFirebaseMessagingService.onMessageReceived` parses a real FCM data
  payload into one of the two shapes and projects it, instead of only
  logging receipt. Real unit tests cover construction, dedup, and (mirroring
  `choosh-hostd::auth_detect`'s no-leakage tests) that no token/session/
  credential text can reach a rendered notification.
  **Credential-availability finding, checked directly in this environment**:
  a real Firebase project (`choosh`) exists and `firebase projects:list`
  authenticates; its OAuth2 token is even `cloud-platform`-scoped. But no
  service-account JSON key is available here — `GOOGLE_APPLICATION_CREDENTIALS`
  is unset, no such file exists on disk, and this sandbox's own safety
  policy blocks using that personal-account OAuth token to call the Cloud
  IAM API and mint one (a real, consequential write against a real personal
  Google Cloud project, correctly out of bounds without explicit user
  sign-off — not a technical dead end). So `FcmClient::from_env` has never
  had a real credential to load in this environment, and no live push has
  ever reached `fcm.googleapis.com` or a device here. Separately, the
  Android app's own real device (per the M8 accessibility/adaptive-layout
  follow-ups' Genymotion notes) may lack Google Play Services, which would
  independently block obtaining a real FCM registration token to send *to*
  — not verified either way in this pass. Both gaps are about live external
  credentials/devices, not missing code.
- **`choosh-hostd` shells out to the `jj` CLI instead of using `jj-lib`'s
  programmatic API** (`rust/choosh-hostd/src/jj_ops.rs`): a deliberate,
  reported deviation from `jj-integration.md`'s "embed `jj-lib` directly"
  design — `jj-lib` is a real, compiling dependency, but assembling its
  API for clone/workspace/diff correctly was judged more work than a
  single-pass increment could responsibly cover. Every invocation still
  uses a fixed executable and fully-encoded argv, never a shell string.
  Replacing this module's internals with real `jj-lib` calls behind the
  same RPC surface is a scoped follow-up.
- **`host-deployment.md`'s macOS power-assertion requirement has a real
  implementation (`rust/choosh-hostd/src/power_assertion.rs`,
  `IOPMAssertionCreateWithName`/`IOPMAssertionRelease` against
  `kIOPMAssertionTypePreventUserIdleSystemSleep`, real hand-rolled IOKit/
  CoreFoundation `extern "C"` FFI) but two real gaps remain, both reported
  rather than papered over. First, a scope narrowing: the spec's exact
  condition is "hold while at least one PTY/agent session or registered
  service/build process is active, release as soon as none are" — this
  instead holds the assertion for `choosh-hostd serve`'s entire
  `connect_loop` lifetime (acquired once at startup, released only on
  graceful shutdown), a conservative superset that never under-holds
  relative to the spec but doesn't release during genuinely idle stretches
  either; wiring the finer-grained liveness signal through
  `pty.rs`/`agent_launch.rs`/service-tunnel tracking remains a scoped
  follow-up. Second, and more fundamentally: **this remains functionally
  unverified on real macOS** — this sandbox is Linux-only with no macOS
  SDK, so `cargo build --target x86_64-apple-darwin -p choosh-hostd` fails
  before ever reaching this module (a transitive dependency, `ring`,
  needs a real macOS C cross-compiler this sandbox doesn't have).
  Verification here was: (1) `cargo check -p choosh-hostd` and `cargo
  clippy -p choosh-hostd --all-targets` clean on Linux, where the module's
  `#[cfg(not(target_os = "macos"))]` path is a genuine, tested no-op; (2)
  a platform-independent "held/not held" state machine
  (`power_assertion.rs`'s `State`) unit-tested directly, covering
  acquire/release idempotency and drop-releases semantics, all passing;
  (3) the macOS-only IOKit/CoreFoundation `extern "C"` block, extracted
  standalone and compiled directly with `rustc --target x86_64-apple-darwin
  --emit=obj` (bypassing `cargo`'s `ring` blocker), which produced a real
  Mach-O 64-bit x86_64 object file — proving the FFI declarations and call
  sites are internally type-consistent for that target, though not
  verified against real Apple SDK headers (unavailable here) beyond
  cross-checking the signatures against the standard, widely-documented
  `IOPMLib.h`/`CFString.h` shapes from public knowledge. No real macOS
  process has ever created or released one of these assertions, and
  whether it actually prevents sleep/reconnect-loss in practice is
  untested.
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
- **Terminal accessibility and hardware keyboard input — fixed (M8
  accessibility follow-up pass).** `TerminalSurfaceView` now exposes the
  terminal's live visible-grid text to TalkBack via a real
  `ExploreByTouchHelper` virtual node (`TerminalAccessibilityHelper.kt`,
  backed by a new `Grid::visible_text`/`Engine::visible_text` Rust
  accessor and `native_terminal_get_text` JNI method), confirmed on a real
  device via `uiautomator dump` showing real rendered content (not just an
  empty node). Hardware-keyboard input is wired end-to-end
  (`TerminalKeyMapper.kt` + `TerminalSurfaceView.dispatchKeyEvent` +
  `TerminalInputConnection`, an `onCreateInputConnection` IME path), also
  confirmed on a real device via `adb shell input keyevent`/`input
  text`/`input keycombination` reaching `TerminalSession.sendKey`/
  `sendText` (confirmed via real, content-free logcat diagnostics —
  key/char counts and modifier flags only, never typed content, per the
  "terminal text MUST NOT be logged" requirement). A real, separately
  confirmed bug was fixed in the same pass:
  `TerminalSurfaceView.surfaceCreated`/`surfaceChanged` callbacks could
  race ahead of `attachSession` receiving a non-zero session handle,
  silently dropping the real surface size forever — the actual root cause
  of the DeX/tablet grid staying pinned at the hardcoded 80×24 fallback;
  `attachSession` now replays the most recent surface state. Still
  incomplete: Tab-focus/Escape-dismiss remains the pre-existing
  inconclusive item below (not touched by this pass), and the fuller
  `terminal-experience.md` IME nuances (dead-key composition preview,
  multi-stage East-Asian input) are not exhaustively verified beyond the
  `BaseInputConnection`-provided baseline.
- **Adaptive layout for tablet/DeX — implemented for
  Explorer/Fleet/Workspace and the terminal grid (M8 accessibility
  follow-up pass).** A hand-rolled `WindowWidthSizeClass` breakpoint
  (`ai/choosh/ui/WindowSizeClass.kt`, matching Material's own 600/840dp
  thresholds; no new `material3-window-size-class` dependency — see that
  file's doc comment for why) drives a multi-column grid for
  Explorer/Fleet and a real master-detail split for `WorkspaceScreen` at
  Expanded width, confirmed via real screenshots at `wm size 1920x1080`/
  `1600x2560`. The terminal grid already derived `cols`/`rows` from the
  real surface pixel size and measured font cell metrics
  (`native_terminal_surface_changed`'s `cells_for_size`) — the
  `TerminalSurfaceView` race above was the actual reason this wasn't
  visible in the M8 pass's DeX/tablet screenshots; confirmed fixed via a
  real device log line (`terminal_surface_resized`) showing `cols=134
  rows=23` at 1920x1080 and `cols=112 rows=57` at 1600x2560 (both far past
  the old hardcoded 80×24). **Still a real, tracked gap**: the terminal's
  new derived `cols`/`rows` are not forwarded to the PTY —
  `PtyWriteHalf::resize` (a real, tested `tcsetwinsize` call, new in this
  pass) exists at the `choosh-hostd` layer, but no phone-to-devhost wire
  message exists to invoke it from a real Android surface resize; wiring
  that needs a new `choosh-protocol`/`choosh-relayd` control-message path
  (mirroring `ControlRequest::AgentEvent`'s device-scoped push shape) that
  this pass did not build.
- **Custom Compose rows lack accessible labels — fixed (M8 accessibility
  follow-up pass).** Explorer's `ChangedFileRow`, `JjChangeGraphScreen`'s
  `ChangeNodeChip`/`OperationRow` Restore button, `JjDiffScreen`'s "Load
  diff" button, and `FleetDrawer`'s `FleetRowView` all now carry real,
  context-specific `contentDescription`s (e.g. "README.md, added",
  "Restore to operation: merge A and B"), confirmed via real `uiautomator
  dump`s before/after on a real device. Note:
  `Modifier.semantics(mergeDescendants = true) {}` — the fix this repo's
  own M8 accessibility report suggested as one option — was tried first
  and, on real-device verification, did **not** surface the merged
  children's text in the exposed `AccessibilityNodeInfo` on this
  device/Compose version; the explicit `contentDescription` form (the same
  mechanism already used by `FleetDrawer`'s `SortModeIcon`/`AttentionDot`)
  was used instead once confirmed working. TalkBack itself still isn't
  installable on the sandboxed test device, so `uiautomator dump` remains
  a proxy, not a literal TalkBack session.
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
- **`docs/evidence/zelland-source-audit.json` was regenerated with real,
  independently-verified provenance data** (commit, tree SHA, and blob SHAs
  for every ported path, cross-checked via two independent `gh api`
  queries and two independent fresh clones). `scripts/check-terminal-provenance.sh`
  now passes its Zelland-audit check — but the same script also requires
  **`docs/evidence/terminal-go-no-go.json`** (a 4-device-class × 9-scenario
  device-conformance decision record), which is *also* missing post-reset
  and was never resurrected (`docs/licenses/terminal-provenance.md`'s own
  addendum already says as much). `just check`/CI remain red on this
  second file until either real on-device conformance evidence is gathered
  across all 9 named scenarios or a scope decision retires that gate —
  neither was fabricated.

## Next

With every milestone's first pass landed, the natural next steps are
either (a) standing up a real `choosh-relayd` deployment and doing the
first genuine end-to-end phone/devhost round trip, or (b) working down the
list above. Neither is inherently ordered before the other — pick based on
what's actually blocking real use.

Update this ledger whenever an increment materially changes completed
evidence, remaining gates, or the ordered next steps.
