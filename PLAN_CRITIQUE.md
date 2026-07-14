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
