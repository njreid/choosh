# Critique — Choosh System Design and Delivery Plan

Reviewer note: assessment of `CHOOSH_DESIGN_PLAN.md` (proposed baseline, 14 July 2026)
and its supporting specs, ADRs, and threat model.
Date: 14 July 2026.

## Verdict

This is a well-above-average design document. The system boundary is coherent, the
security posture is thought through rather than bolted on, and the "explicit
registration over discovery/inference" stance eliminates a large class of the bugs
that sink remote-dev tools. The main risks are **not** in the architecture — they are
in **sequencing, single points of failure, and two or three unvalidated assumptions
that are load-bearing for the whole product.** The concerns below are ordered by how
much they should change the plan.

## What the plan gets right (keep these)

- **One network boundary.** A single host-key-verified SSH transport carrying PTY /
  exec / SFTP / direct-tcpip is the right call — it collapses the attack surface and
  the auth story into one place.
- **Rust as durable-state authority, views as projections.** The command/event actor
  model with revisioned snapshots is the correct shape for a UI that must survive
  rotation, reconnect, and process death.
- **Explicit everything.** No filesystem workspace discovery, no port/process
  inference, explicit service launch, explicit workspace registration. This is the
  single best decision in the plan.
- **Observational-only agent hooks.** Hooks that cannot approve/deny/rewrite is the
  right safety and trust boundary, and the threat model backs it (`Hook automatically
  approves command → adapters are observational`).
- **Host-supplied git blobs + client-side diff.** Avoids shipping libgit2/JGit or a
  second checkout to the device. Elegant.
- **Threat model quality.** The abuse-case table (symlink escape, git diff helper,
  fake changed path, loopback port probing, stale terminal binding) is real threat
  modeling, not a checklist.

## Blocking concerns (resolve before committing to the delivery plan)

### B1. The native GPU terminal is the critical path, the highest-risk item, and has no concrete fallback

The terminal is the product's core surface, and the plan bets it on porting Zelland's
`wgpu` / `glyphon` / `libghostty-vt` stack off Tauri onto a raw Android surface
(ADR-0005, `terminal-experience.md`). That is release-critical Android work: surface
lifecycle, GPU device loss, IME `InputConnection`, SGR mouse guards, atlas rebuilds.
It is realistically a multi-month effort on its own.

Two things make this a *blocking* risk rather than just a hard task:

1. **The license/ownership of the Zelland source it depends on is unresolved**
   (M0-R15 lists it as a thing to "establish"). The plan builds the product's central
   pillar on code whose availability is a to-be-determined.
2. **The stated fallback is not concrete.** The plan says the renderer stays "behind an
   internal interface so a safe fallback can be introduced," but the terminal spec
   *explicitly rejects* a WebView/xterm.js terminal, and no alternative renderer is
   named. So the actual fallback today is "there is no fallback."

**Fix:** Treat the terminal as its own de-risking stream, not one bullet inside M0.
Resolve the Zelland license as an *entry* gate (it appears to be `njreid/zelland`,
same owner as this repo — if so, say so and close the question immediately). Name a
concrete plan-B renderer (e.g. a Compose/Canvas or Skia cell renderer driven by the
same Rust grid model) and cost it, or accept the single-point-of-failure risk *in
writing*.

### B2. Background notification delivery has no described mechanism — and it may be architecturally impossible under the current constraints

`input_required` notifications (M2) are a headline feature. But timely Android
notifications require one of:

- a **persistent foreground service** holding the SSH connection while backgrounded
  (Android 14+ FGS-type restrictions, Doze, and aggressive OEM process-killing all
  fight this), or
- **FCM push**, which needs a server — and the plan explicitly forbids a public server
  ("no bundled ... public server").

The plan never states *how a notification reaches the phone when the app is not in the
foreground.* As written, "no server" + "no persistent connection when backgrounded" =
no delivery path. This is a real hole in a feature that spans M0-R8, M2, and the
notification UX.

**Fix:** Decide the delivery model explicitly. The most likely honest answer is a
foreground SSH-holding service with all its battery/Doze caveats documented as a
product constraint — but that decision needs to be made now, because it affects the
connection lifecycle, the engine's ownership of the socket, and user expectations.

### B3. M0 is not a skeleton — it is "prove every hard problem at once"

M0 carries 15 requirements: Gradle/Cargo workspace, 4-target CI, Sora embed, the
Kotlin/Rust bridge, SSH with four channel types, daemon RPC over stdio, the native GPU
terminal port, three agent adapters, client diff, a service tunnel, *and* a signed
release. Several of these are independently multi-week spikes; the terminal alone
could exceed the rest combined.

Bundling them makes M0 unfalsifiable (it can't "pass" until everything passes) and its
timeline unknowable. It also hides which spike killed the schedule.

**Fix:** Split M0 into independently-failable spikes with their own go/no-go gates, and
explicitly identify the 2–3 that are on the critical path (terminal, bridge, SSH
multiplexing). Let the low-risk ones (Gradle, release plumbing) run in parallel without
gating the risky ones.

## High-priority concerns

### H1. Two license questions are release-blockers treated as research tasks

- **Sora is LGPL-2.1+ inside an Apache-2.0 Android app.** LGPL relinking obligations on
  a statically packaged Android binary are non-trivial, and the plan defers the review
  to M6 — *after* M4 has built the entire editor on it. If it turns out Sora can't be
  shipped compliantly the intended way, a core pillar breaks at the very end.
- **Zelland license** (see B1).

**Fix:** Move both to *entry* gates. You do not want to discover a licensing wall after
building M4's editor and M0-M3's terminal on top of the dependency.

### H2. "Match git's diff" is a much larger surface than one crate

Computing line diffs client-side with `imara-diff` is fine for hunk formation, but
"looks like what `git diff` shows" pulls in: rename *content* pairing (status gives you
the rename; who feeds imara-diff the correct old/new pair?), `.gitattributes`
`text`/`eol`/`diff` drivers, whitespace/EOL normalization, mode changes, submodule
diffs, and binary detection. The plan handles the *policy* edges (binary/oversized →
metadata) but understates the *fidelity* work of matching user expectations.

There is also no **blob-transfer performance/caching story**: three versions per changed
file (HEAD/index/worktree) over SSH, for a branch with hundreds of changed files, with
no batching or cache mentioned.

**Fix:** Scope diff fidelity explicitly (what you will and won't match), and add a blob
cache + batched fetch to the git spec.

### H3. Offline editing conflict semantics are asserted but not specified

The single-file save story is good (temp+rename, open-time identity capture,
resync/conflict instead of silent overwrite). But M4's "offline queue policy" is where
these tools actually die: what happens when a queue of edits replays against a remote
file that changed while offline? Across *multiple* files? The design plan defines the
happy path and names the failure event, but not the resolution UX. That UX is the
feature.

**Fix:** Specify the offline-replay conflict model (per-file rebase vs. reject-all,
what the user sees, whether partial application is allowed) before M4.

### H4. Single connection = head-of-line contention on a mobile link

Multiplexing PTY + SFTP + RPC + tunnels over one SSH connection is great for the
security boundary, but a large SFTP range read (a big Markdown asset) can contend with
terminal responsiveness on a constrained cellular link. SSH per-channel flow control
mitigates but does not eliminate this.

**Fix:** Note a channel-prioritization policy, or allow a second bulk-transfer
connection under the same verified host key if measurements demand it.

## Medium-priority concerns

- **M1 "Android-only" vs. README "Android-first."** The design plan says *only* (and
  lists desktop/iOS parity as a non-goal); the README says *first* (implying later
  platforms). These imply different investments in the JNI/UniFFI boundary and core
  portability. Pick one word.
- **"Agent-neutral" overclaims.** The product is hard-coupled to three agents' young,
  changing hook APIs (`PermissionRequest`, `PostToolUse`, `permission.asked`, …). This
  is agent-*pluggable* with permanent per-agent adapter maintenance, not neutral. Also
  generalize the Codex "terminal-notification fallback" into a documented path for
  agents that expose no hooks.
- **Circular performance gate.** The terminal exit gate is "passes its device tests /
  performance budgets," but the budgets are "set from M0 measurements" — so on day one
  the gate cannot fail. Set rough target budgets up front (fps under sustained output,
  input latency, atlas/scrollback ceilings) so the gate is falsifiable.
- **No field-diagnostics story.** Correct privacy defaults (no telemetry, no content in
  logs) plus Obtainium sideloading (no store crash reporting) means near-blind field
  debugging. Add an opt-in, content-redacted local diagnostic log and a crash-capture
  plan, or accept "support from user prose only" explicitly.
- **Zellij version coupling.** Session-name==workspace-name is elegant, but there's no
  pinned/verified Zellij version or capability check, and pre-existing same-named
  user sessions need careful adopt/collision handling beyond "explicit confirmation."
- **Pinned toolchain numbers will rot in the doc.** M0-R13/R14 hard-code Kotlin 2.4.10,
  AGP 9.2.1, Compose BOM 2026.06.01, API 36/37. Keep exact versions in the version
  catalog; keep the *policy* ("latest mutually-compatible stable") in the milestone.
- **`ai.choosh` application ID.** Reverse-DNS IDs assume you own the domain. Confirm
  control of `choosh.ai`, or choose an ID you own — cheap to fix now, annoying later.

## No user-validation checkpoint

This is a large amount of engineering (M0–M6, native terminal, host daemon, three
adapters) aimed at a narrow user: someone doing serious agent-driven development from
an Android device against a persistent self-hosted daemon, willing to install per-agent
hooks. That may be a real and underserved user — but the plan has no milestone that
validates demand before the heaviest work. Consider a "thin remote terminal + one
notification" pre-M0 probe you could actually put in front of target users.

## Suggested top-of-plan changes

1. Split M0; make the terminal and the two license questions their own early,
   independently-failable gates (B1, B3, H1).
2. Decide the background-notification delivery mechanism now and write it down (B2).
3. Name a concrete fallback renderer or accept the terminal SPOF in writing (B1).
4. Specify offline-edit conflict resolution and diff fidelity scope before M4/M3
   (H2, H3).
5. Reconcile "Android-only" vs "Android-first," and downgrade "agent-neutral" to
   "agent-pluggable."

## Open questions for the authors

- If Zelland is your own repo, is B1's license question already closed? If so, delete
  it from M0-R15 and reclaim the risk budget.
- What is the intended notification transport when the app is backgrounded?
- Is there an intended target-user validation step before M0, or is demand assumed?
- Is Sora's LGPL packaging compliance a known-solved problem for you, or a genuine open
  risk?

---

# Implementation review — 20 July 2026

A second pass, this time over the **code** on `main` (152 commits since the plan
baseline), not the plan. Scope: 8 Rust crates (~35k lines across 100 files, 458 tests),
the Android/Java sources (~3.1k lines, 27 files), CI, and the evidence docs. The
workspace compiles clean (`cargo check --workspace`) and the unit tests I ran pass.
Findings below were produced by five focused reviewers plus first-hand reading, then
reconciled.

Note on where things stand per the project's own ledger (`PLAN.md`): M0 is "in
progress," M1–M6 "not started." This review is calibrated to that — the point is not
"features are missing" (expected) but *what the existing 35k lines tell us about
direction, and what to fix or simplify now while it's cheap.*

## Headline

The **primitives are genuinely good; the system is not yet composed.** Almost every
module is a deterministic, I/O-free, fail-closed state machine with bounds enforced at
construction, redacted `Debug`, opaque credential newtypes, and a real test module.
That discipline is high-quality and rare. But there is **no end-to-end path**: you
cannot yet connect to a host, run a command, open a file, or render a terminal. The
work so far is a large, well-tested kit of parts with very little wiring between them —
and a meaningful fraction of it is scaffolding that encodes the *delivery process*
rather than the product.

Three structural observations dominate everything else. Fixing them now is far cheaper
than after M1–M6 pile on top.

## S1 (structural) — The composition gap is the real state of the project

Every reviewer independently hit the same wall: the pieces don't call each other.

- **`chooshd` answers exactly one method, `host.describe`.** There is **no
  `Command::new`/`spawn`/`exec` anywhere in the entire workspace.** So `git.rs`,
  `blob.rs`, `zellij.rs`, `adapters.rs`, `project_fs.rs`, and `state.rs` — all real,
  all tested — are never invoked by the running daemon (`daemon.rs:273`; `main.rs:45`
  starts with `capabilities: Vec::new()`). Git never runs, Zellij is never launched,
  no blob is ever streamed, no hook is ever installed.
- **`choosh-core` is 48 flat modules with ~7 internal `use crate::` edges.** There is
  no orchestrator that assembles connection + actor + bridge + session into a running
  workspace. A reader cannot find "the system" because it isn't expressed anywhere.
- **The Android app is one throwaway `Activity` wired to always fail.**
  `MainActivity.java:64` injects an `unavailableCoordinator()`, so every Connect
  resolves to `TRANSPORT_UNAVAILABLE`. `WorkspaceStatusController` is fully built and
  tested but has no production caller.
- **The JNI "authenticated plan" discards its inputs.**
  `choosh-bridge_authenticated_plan_begin` (bridge `lib.rs:82`) validates five opaque
  handles are non-zero, then throws them all away and tail-calls the generic
  `request_begin`; `_open` always returns `TRANSPORT_UNAVAILABLE`. The real russh
  integration in `choosh-ssh` (which *is* real — exact host-key callback, pubkey auth,
  fixed-command exec, direct-tcpip, SFTP) is not connected to it.

**Why it matters:** 35k lines and 458 green tests can read as "most of the way there,"
but the ledger is right that M0 isn't done — the end-to-end controls the threat model
promises (env-clearing git executor, path reconciliation against the canonical root,
streaming blob bounds, peer-cred on the socket) are *unreachable* because nothing
composes them. **The dominant risk now is the executor/composition layer that will
wire these primitives — it is unwritten, untested, and is where the security
properties actually have to hold.**

**Recommendation:** make the next milestone a *vertical* composition — one real
`chooshd` method that spawns fixed-argv `git status` through an executor that asserts
`env_clear()`, reconciles the returned paths through `project_fs::prepare`, and returns
them over the real socket to a real Android connector — rather than more breadth of
isolated primitives. Prove one thread through all the layers before widening.

## S2 (structural) — ~1/5 of the core crate is compiled delivery-process ceremony

Nine `choosh-core` modules — `conformance`, `release_evidence`, `security_audit`,
`milestone_acceptance`, `provenance`, `performance`, `fault_campaign`,
`accessibility_evidence`, `toolchain_evidence` — total **~3,300 lines (~17% of the
crate)** and are **referenced by nothing** but their own tests. They encode the
project's *own* release checklist as shipped library code: `provenance.rs` hard-codes
`["Zelland","libghostty-vt","wgpu","glyphon"]`; `release_evidence.rs` hard-codes the
gate list `["unit","integration","security","sbom","notices"]`; `milestone_acceptance`
encodes M0–M6 scenario IDs.

This is bookkeeping frozen into the product binary. Every checklist tweak forces a
core-crate recompile and ships dead weight to the Android target. Related: `choosh-web`
is a 21-line intentional placeholder (fine), and `choosh-testkit` (722 lines) is a
workspace member that **no crate depends on** — it compiles on every build for zero
consumers.

**Recommendation:** move the evidence/acceptance/audit family to a separate
`choosh-release` crate, an `xtask`, CI scripts, or docs. If any of it must stay in-app
(e.g. `provenance` for an in-app licenses screen), relocate just that. Wire
`choosh-testkit` as a `dev-dependency` of the integration tests it was built for, or
drop it from default members until used. This removes ~3–4k lines of dead weight from
the shipped core.

## S3 (structural) — The same small primitives are re-implemented many times, sometimes divergently

Because there is no shared foundation (S1), the same micro-abstraction is written over
and over — and in the security-relevant cases, the copies **disagree**:

- **Path confinement is implemented ~4–8 times with subtly different policies.**
  `markdown.rs:337`, `asset.rs:41`, `explorer.rs:173`, `workspace.rs:143`,
  `git.rs:229`, `project_fs.rs:190`, `adapters.rs:144`. They diverge: `asset.rs` blocks
  `%2f`/`%5c`/`://`; `markdown.rs` blocks any `%` and leading `//`; `explorer.rs` does
  neither. This is a security footgun — the "canonicalize every path under the root"
  control is only as strong as the weakest copy.
- **Identity validation ~8 times, two as macros** (`item.rs:12 bounded_identity!`,
  `annotation.rs:8 identity!`, plus `pins`, `waiting_notification`, `explorer`,
  `asset`, `notification_activation`), and **two incompatible `WorkspaceId` types**:
  `workspace.rs:44` allows only `[A-Za-z0-9_-]`, while `item.rs:39`/`annotation.rs:37`
  allow any non-control char — an id valid in one is rejected by the other.
- **UUID validation disagrees on case** (correctness): `envelope.rs:31` requires
  lowercase hex; `agent_event.rs:322` accepts upper *or* lower via `is_ascii_hexdigit`.
  A `NormalizedAgentEvent` with uppercase-hex ids passes normalization but can never be
  wrapped in `EnvelopeId::new` when the daemon later emits it (`wire.rs:200`).
- **"Read one framed message from a stream" loop copy-pasted 4×**: `bridge.rs:170`,
  `handshake.rs:107`, `request.rs:158`, `socket_relay.rs:74`.
- **The exec wire codec is duplicated across crates**: `choosh-ssh/src/exec.rs`
  (encoder) and `choosh-host/src/exec_stdio.rs` (decoder) independently hard-code
  `WIRE_VERSION`, `MAX_*` constants and hand-mirror the framing — change one side and
  the wire silently breaks.
- **Generation-fence logic ~7×** (`actor.rs:117`, `connection.rs:156`,
  `renderer_binding.rs:62`, `bridge.rs`, `readiness`, `reconnect_recovery`, `service`),
  each with its own overflow check and `StaleGeneration` variant.
- **~4 byte-budget accumulators** (`gateway.rs:190`, `gateway_stream.rs:114`,
  `asset.rs` stream budget, `http_gateway.rs` body budget) — same checked-add-against-
  per-unit-and-total pattern.
- **Duplicated/parallel types**: `Version` struct 3× (`backup_restore`,
  `release_evidence`, `release_update`); `LineKind` 2× (`diff` / `diff_navigation`);
  `ReadOnlyReason` 2× and `RemoteIdentity` 2× *with incompatible shapes* (`document.rs`
  vs `document_save.rs`); 43 bespoke `*Error` enums despite the `ports.rs::PortError`
  seam existing for exactly this.

**Recommendation:** land a small shared foundation *before* M1 widens the surface:
one `path::confined_relative(...)` validator (single documented policy, reused
everywhere — do this first, it's the security-relevant one), one `identity::Bounded`,
one `Generation` newtype with `advance()`/`is_current()`, one `Budget` helper, one
canonical `is_uuid`, and move the exec codec + constants into `choosh-protocol` so both
sides share one definition. This deletes a large amount of copy-paste and, more
importantly, removes the divergence bugs.

## Consolidation targets (module families that should merge)

- **`document.rs` + `document_save.rs` + `document_format.rs` + `editor_document.rs`**
  are four unconnected halves of one feature — two incompatible `RemoteIdentity`, two
  `ReadOnlyReason`, and no seam wiring `format → save → begin/finish_save`. Merge into
  a `document/` module and define the seam.
- **`diff.rs` + `diff_navigation.rs`**: `diff_navigation` re-declares `LineKind` and
  the `LineMapping` model verbatim as `MappedLine`, then re-validates invariants
  `diff.rs` already guarantees by construction (~250 redundant lines). Fold navigation
  onto `diff::TextDiff`.
- **`gateway.rs` + `gateway_stream.rs` + `http_gateway.rs`**: three uncoupled models of
  the same authenticated loopback gateway; request-syntax validation (method/target/
  header-token, down to the identical token byte-set) is written twice. Extract one
  `http_syntax` helper and a `gateway/` module.
- **`pins.rs::PinSet` vs `service.rs::ServicePins`**: the latter is a strict, slightly
  inconsistent subset of the former (a `PinTarget::Service` variant already exists).
  Drop `ServicePins`. Likewise `item.rs::ItemStatus` vs `service.rs::ServiceStatus` are
  parallel status enums + transition tables — collapse to one.
- **`annotation.rs` vs `annotation_export.rs`**: two export representations
  (`ExportAnnotation` vs `ExportRecord`) with no conversion — `prepare_export` produces
  a model the codec can't consume. Make `ExportRecord` the single type.

## Correctness bugs to fix (mostly latent today because unwired — fix before they wire)

| # | Location | Bug |
|---|---|---|
| 1 | `diff.rs:216,261` | Diff is a quadratic hand-rolled LCS (`"bounded-lcs-v1"`), **not** the `imara-diff` the plan bets on. `max_cells: 4_000_000` → any pair over ~2000 changed lines/side silently degrades to metadata-only. Swap in Myers/histogram behind the existing `DiffResult` contract. |
| 2 | `diff.rs:270-303` | CRLF↔LF-only changes render as an **empty diff**: `split_lines` strips `\r` and `lines_equal` ignores terminators, so `"a\r\nb\r\n"` == `"a\nb\n"`. A user converting line endings sees "no changes." |
| 3 | `agent_event.rs:322` vs `envelope.rs:31` | UUID case mismatch (S3): an event that normalizes with uppercase-hex ids can never be re-emitted as an `Event`. |
| 4 | `NativeAuthenticatedSshConnector.java:190` | Throwing `IllegalArgumentException` inside the native RPC callback runs on the native callback thread, past the enclosing `try/catch` (which only catches `NativeBridgeException` and has already returned) → a null native result becomes an **uncaught crash** instead of a typed failure. |
| 5 | `blob.rs:224` | Byte bound is checked **after** the full `Vec<u8>` is assembled, not during read — a future streaming reader can exhaust memory before `finish` runs. Expose the bound to the reader (`take(max_bytes+1)`). |
| 6 | `handshake.rs:120`, `socket_relay.rs:86` | `MultipleReplies` guard is **dead** — the decoder's batch limit of 1 fails with `BatchLimitExceeded` first (the crate's own test asserts this). Remove the variant or raise the limit. |
| 7 | `connection.rs:46` | `ConnectionFailure::TransportFailed` is never constructed, and `retryable()` classifies the **terminal** `RetryExhausted` state as retryable. Dead + self-contradictory. |
| 8 | `socket.rs` | No `SO_PEERCRED`/`getpeereid` — access control is filesystem-mode only. Consistent with the threat model's "open question," but any non-owner process reaching the 0700 dir is admitted with no credential gate. Track explicitly. |
| 9 | `gesture.rs:161,269` | Dead `WebView` match arm (guarded + catch-all return the same owner) and an ignored `point` param — unfinished horizontal-scroll intent. |

## Plan-vs-code drift worth recording

- **UI: plan says Jetpack Compose + Kotlin; there is zero Kotlin and zero Compose.**
  No Kotlin plugin, no `androidx.compose.*`, no `setContent {}` (`build.gradle.kts:5`,
  `libs.versions.toml`). The Android side is plain-Java headless controllers plus one
  programmatic-`LinearLayout` throwaway Activity. Either correct the docs to describe
  the current M0 Java spike, or add the toolchain — today the gap is total, not
  partial, and the plan's "embed Sora in Compose via `AndroidView`" is unstarted (the
  only Sora code is an isolated `ContentChangeEvent` translator; no `CodeEditor` is
  ever mounted).
- **Diff engine: plan says `imara-diff`; code is a quadratic placeholder** (bug #1).
- **Terminal (my earlier B1 — the #1 product risk): still unimplemented.** The
  `vt.rs`/`terminal.rs` *engine* logic (VT parsing, mode-aware input encoding,
  scrollback, alt-screen) is real, well-tested, and good — but there is no renderer, no
  Android surface, no wgpu/glyphon port. Good news that resolves part of B1: the
  **Zelland source grant is now recorded** (`docs/licenses/zelland-grant.md`,
  `PLAN.md`), closing the license half of that open question. The device/renderer half
  remains the critical path.
- **Sora LGPL compliance (earlier H1) is still unaddressed in code** — Sora is pulled
  in only for a type used by a pure translator, so the packaging/relinking question
  hasn't been forced yet.

## What is genuinely good (keep, and don't let a refactor erode it)

- The pure state machines: `vt.rs`/`terminal.rs` input+screen engine, `event_spool.rs`
  retention/replay/ack, `connection.rs` generation/channel staleness, `backoff.rs`
  saturating-jitter math.
- `project_fs.rs`: a real TOCTOU-resistant design — canonicalize root, record
  dev/inode, re-`symlink_metadata` + fstat match + re-canonicalize on open. Tests
  exercise symlink-swap and cross-root reuse. No gap found.
- `git.rs` argv hardening (`--no-pager`, `core.hooksPath=/dev/null`, `diff.external=`,
  `GIT_EXTERNAL_DIFF=""`, `clear_environment: true`) — correct, though only `status`
  exists and the env-clear is an advisory field until an executor applies it.
- `adapters.rs`: observational-only capability model with exact-byte install
  preconditions — matches the threat model precisely.
- `choosh-ssh`: a real russh 0.62 integration (exact host-key, pubkey auth,
  fixed-command exec, direct-tcpip loopback, SFTP) — the hard part exists; it just
  isn't wired in yet.
- The cross-cutting discipline: redacted `Debug`, opaque newtypes (`Capability`,
  `CredentialRef`, `ConfirmationToken`, `DaemonRpcPlan`), fail-closed defaults, and a
  deterministic no-I/O core with injected adapters. This is the right shape.
- `renderer_binding`'s action-list "plan" (`Vec<BindAction>` from a pure state machine)
  is a *good* lightweight pattern — not to be confused with the JNI authenticated-plan
  ceremony (S1). Don't refactor it away.

## Suggested near-term order of work

1. **Reconcile the docs to reality** (UI is Java not Compose; diff is LCS not
   imara-diff) — cheap, stops the drift compounding.
2. **Land the shared foundation from S3** — one path-confinement validator (first),
   identity, `Generation`, `Budget`, `is_uuid`, shared exec codec. Do this before M1
   widens the surface.
3. **Move the S2 evidence/ceremony out of the shipped crates.**
4. **Build one vertical composition slice** (S1): real `git status` through an
   env-clearing executor → `project_fs` reconciliation → real socket → real Android
   connector. One thread through every layer beats more isolated primitives.
5. **Fix correctness bugs #1–#7** while the code paths are still small.

## Open questions for the authors (implementation)

- Is the `host.describe`-only daemon a deliberate checkpoint, or has the executor/
  composition layer been deferred longer than intended? It's the highest-risk unwritten
  piece.
- Is the Java UI a temporary M0 spike to be replaced by Compose, or a decision to drop
  Compose? The docs and code should agree either way.
- `imara-diff` was the stated bet — is the quadratic LCS a deliberate interim, and is
  there a tracked task to swap it before diffs are shown on real repos?
- Should `choosh-testkit` and the evidence modules be in the shipped workspace at all,
  or move to a release/CI crate?
