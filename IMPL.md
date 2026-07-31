# Implementation review against PLAN.md

Reviewed commit: `a805502` ("Add Android host deployment envelope"), branch `main`.
Review and remediation: 2026-07-30 to 2026-07-31.

This document records an independent audit of what is actually implemented in the
repository against the claims in [PLAN.md](PLAN.md), the requirement IDs in
[the milestone plan](docs/milestones/README.md), and the constraints in
[AGENTS.md](AGENTS.md). It is a review artifact, not a status ledger; `PLAN.md`
remains the ledger.

Every finding below has been resolved or explicitly deferred; each carries its
outcome. The findings are kept rather than deleted so the next review can tell
what was already looked at.

## Method

Rust gates were run with `CARGO_TARGET_DIR=/tmp/choosh-target` (the Justfile
convention) because the machine's root filesystem is full.

| Gate | At `a805502` | After remediation |
|---|---|---|
| `cargo test --workspace` | pass (525) | pass (528) |
| `cargo fmt --all -- --check` | pass | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | **FAIL** (29 issues) | pass |
| Android Java sources compile | **FAIL** (2 errors) | pass |
| `scripts/check-specs.sh` | pass | pass |
| `scripts/check-android.sh` | pass | pass |
| `scripts/check-android-sources.sh` | did not exist | pass |
| `scripts/check-m6-release-readiness.sh` | pass | pass |
| `scripts/check-release-discovery.sh` | pass | pass |
| `scripts/check-release-reproducibility.sh` | pass | pass |
| `scripts/check-terminal-provenance.sh` | pass (decision `blocked`) | pass |
| `scripts/check-ssh-admission-fixtures.sh` | pass | pass |
| `scripts/test-rpc-socket.sh` | pass | pass |
| `scripts/test-host-deployment.sh` | pass | pass |
| `scripts/test-zellij-smoke.sh` | pass | pass |
| `scripts/test-host-acceptance-runner.sh` | pass | pass |
| `scripts/test-host-acceptance-local-openssh.sh` | pass | pass |
| `./gradlew :app:testDebugUnitTest` | **not runnable here** (see F5) | still not runnable |

Static inspection covered every Rust crate's module list and dependency graph,
every Java source file, the Gradle build and lockfiles, both CI workflows, and
the merged Android manifests from the last local build.

## Overall assessment

`PLAN.md` is unusually well calibrated **in its individual claims**. Roughly a
dozen specific statements were spot-checked against code and every one was either
accurate or hedged more conservatively than the code warranted:

- The `git.status` vertical evidence is real, not a fixture illusion. The test at
  `rust/choosh-android-transport/src/lib.rs:1281` drives generated-key Russh
  authentication, the real `choosh-host rpc --stdio` relay, and a real `chooshd`
  private Unix socket, and asserts a byte-exact terminal envelope.
- The loopback OpenSSH lane genuinely creates an ephemeral `sshd`, real keys, and
  a real daemon socket, and cleans up after itself.
- There are no `todo!()`, `unimplemented!()`, `TODO`, or `FIXME` markers anywhere.
- `choosh-core` (19,934 lines) has **zero** external dependencies, which is the
  strongest structural evidence that the platform-neutral boundary in AGENTS.md
  has been held.

Two things the review changes about that picture.

**First, the gap is one of shape, not honesty.** The project has a very large,
well-tested policy core and a small set of outer composition roots. Most milestone
requirements have a deterministic headless decision function but no process,
socket, or Activity that calls it. `PLAN.md` said this for individual slices but
never said how systemic it is — and phrases like "passes headlessly" in the
milestone table read as "works" to anyone not reading the code. That is now
recorded (F2, F7).

**Second, the verification discipline had a real hole.** Two gates were failing on
`main` and nobody knew: clippy for roughly two dozen commits, and the Android Java
compile for twelve. Both were caught here only by running them. The remediation
adds the missing local gate and closes the loop between local and CI (F1, F12).

## Findings

### F1 — `cargo clippy -D warnings` failed on `main` — **fixed** (`671df07`, `d87b0ec`)

29 findings across six files, not the five I first reported. My initial count came
from truncated output: clippy stops dependent crates after the first failure, and
I had cut the log at the first crate. Corrected inventory:

| File | Count |
|---|---|
| `rust/choosh-android-bridge/src/lib.rs` | 13 |
| `rust/chooshd/src/annotation_store.rs` | 4 |
| `rust/choosh-host/src/deployment.rs` | 5 |
| `rust/choosh-android-transport/src/lib.rs` | 4 |
| `rust/chooshd/src/diagnostics.rs` | 3 |
| `rust/chooshd/src/{daemon,adapters,project_fs,zellij}.rs` | 5 |

Dating the offending lines shows the gate has been red since **2026-07-25**
(`6fdb916`, `88401ac`), not since the two most recent commits. Toolchain matched
CI exactly (`rustc 1.96.1`, `clippy 0.1.96`), so this was not a local artifact.

Two of the fixes are more than cosmetic:

- `choosh_bridge_authenticated_plan_open` held an `.expect()` on its chunk limits.
  That is an `extern "C"` entry point, so a panic would have unwound across the
  JNI boundary. It now fails closed to `STATUS_TRANSPORT_UNAVAILABLE` against a
  named constant.
- The Android callback contract returned `Result<_, ()>`. It now returns an
  explicit `AndroidIoFailure`, which preserves the deliberate content-free
  boundary while naming it.

`just check` did not include clippy, so the default local gate could pass while CI
failed. It does now, and `release` no longer names clippy separately.

### F12 — The Android sources did not compile on `main` — **fixed** (`f40b4ca`, `abc4753`)

Found while verifying F8, and the more serious of the two build breaks. Two
independent errors:

1. `MainActivity.java:48` called `TextView.setLiveRegion(int)`. That is not an
   Android API; the method is `setAccessibilityLiveRegion(int)`. Introduced by
   `d2290af` ("Add headless accessibility semantics contract"), twelve commits
   before HEAD.
2. `HostDeploymentEnvelope` called `plan.version()`, but `ReleaseUpdatePlanner.StagingPlan`
   had no such accessor — `version` was a private field of a private type.
   Introduced by `a805502`, the HEAD commit.

Either alone breaks `:app:testDebugUnitTest` and `:app:assembleDebug`, so the
`android-pre-device` CI job must have been failing since `d2290af`. Nothing local
caught it because Gradle cannot resolve AGP 9.3.0 here (F5).

Note the interaction with `PLAN.md`'s recorded device evidence: the emulator run
of 2026-07-25 is still valid evidence, because it predates `d2290af`. But it
describes an APK that the current tree cannot build, so it should not be read as
covering HEAD.

Remediation adds `scripts/check-android-sources.sh`, which compiles the Android
Java sources against the pinned platform SDK using a generated `R` stub. It is
deliberately not a Gradle replacement: it skips the one Sora consumer and
degrades to main-only when JUnit is unresolved. It runs from `just android-check`
and in CI. It catches exactly this class of error — a source referencing an API
that does not exist — without needing a resolvable AGP.

Also removed the dead pre-O `Notification.Builder` branch in
`AndroidNotificationSink`, unreachable under `minSdk = 26` and the only
deprecated-API use in the tree (`lint { warningsAsErrors = true }` would flag it).

### F2 — The shipped `chooshd` binary serves only `host.describe` — **recorded** (`7355af3`)

`rust/chooshd/src/main.rs:52` calls `serve(...)`, which delegates to
`DaemonRpc::new()` (`rust/chooshd/src/daemon.rs:377-383`) — an RPC graph with no
registered workspaces and no event coordinator. The CLI accepts only
`--state-dir` and `--socket` (`main.rs:98-118`); there is no way to register a
workspace, load persisted state, or attach an events coordinator.

In a real deployment today:

- `host.describe` works.
- `git.status` always returns `not_found` (`daemon.rs:137`).
- `events.subscribe-v1` / `events.ack-v1` always return `unsupported` (`daemon.rs:168`).

Every passing `git.status` and events test constructs the handler graph inside the
test. That is legitimate evidence for the protocol and adapter seams. But the M1
and M2 ledger rows read as though the daemon has these capabilities, and nothing
recorded that `chooshd`'s own outer composition root does not exist.

`PLAN.md` now carries this as an unchecked item, and "give `chooshd` an outer
composition root" is now increment 3 in the ordered list — ahead of further
domain work, because it gates M1 and M2 more than any remaining policy module.

Related: `chooshd::diagnostics` is declared in `lib.rs:8` and referenced nowhere
else, matching the pre-existing open item.

### F3 — Stale, contradictory peer-credential bullet — **fixed** (`7355af3`)

`PLAN.md` said "Non-Linux Unix builds fail closed until an equivalent credential
adapter is implemented", while a later bullet said macOS verifies through
`getpeereid`. The code (`rust/chooshd/src/socket.rs:132`, `:141-145`) implements
Linux `SO_PEERCRED` and macOS `getpeereid`; only other Unix platforms fail closed.
The bullet now says that.

### F4 — M0-R13 cannot be claimed: no Kotlin, no Compose — **recorded** (`6392a32`, `7355af3`)

`docs/specs/android-toolchain.md` fixed Kotlin 2.4.10 and Compose BOM 2026.06.01
as the production baseline, and M0-R13 requires proving that Kotlin, AGP, the
Compose BOM, Sora Editor, and the Kotlin/Rust bridge are mutually compatible. In
the actual build:

- `gradle/libs.versions.toml` declares three versions: `agp`, `junit4`, `sora`.
  No Kotlin plugin, no Compose or AndroidX entry.
- `android/app/build.gradle.kts:5` applies only the Android application plugin;
  `:82-85` declares only `soraEditor` and `junit4`.
- There is not a single `.kt` file in the repository. All Android source is Java.
- `android/app/gradle.lockfile` shows `kotlin-build-tools-api:2.2.10`, which is
  AGP's internal Kotlin, not a declared 2.4.10 baseline.

AGENTS.md's "Kotlin code uses constructor injection" currently has no subject.

The specification now marks both rows resolved-but-unadopted and states that
M0-R13 must not be recorded as met until they are applied; `PLAN.md` carries the
matching unchecked item.

### F5 — The pinned Android baseline has never been built here — **environmental, not fixed**

`libs.versions.toml` pins `agp = "9.3.0"`, but the local Gradle cache holds only
`9.2.1`, so `./gradlew --offline` fails at plugin resolution. Sora Editor is not
in the cache at all. The last local build artifacts date from 2026-07-24.

CI resolves 9.3.0 from the network, so this is not a repository defect. Its
consequence is that the 77 JVM unit tests were not verified during this review,
and that this is exactly why F12 went undetected for twelve commits. The new
source-compile gate closes most of that hole; the rest needs a machine that can
resolve the pinned AGP.

### F6 — The packaged APK declared zero permissions — **fixed** (`abc4753`)

The merged manifests from the last local build contained **no** `<uses-permission>`
elements. Verified against the built artifact, not inferred.

`android.permission.INTERNET` is now declared. Without it the app cannot open a
TCP socket, so no amount of JNI/Russh work could produce a device connection. No
runtime symptom existed only because `MainActivity:69-77` injects a fail-closed
coordinator, so the transport was never reached.

`POST_NOTIFICATIONS` and `FOREGROUND_SERVICE` are deliberately **not** added: no
Android component needs them yet (F7), and an unused permission is a real cost.
`scripts/check-android.sh` now pins the declared set to exactly one permission, so
any addition is a reviewed change rather than a drift.

### F7 — The M2 notification stack is unreachable at runtime — **recorded** (`7355af3`)

`AndroidNotificationSink`, `NotificationServiceLifecycle`, `NotificationProjector`
and `NotificationIntent` exist with headless JVM coverage, but the manifest
declares no `<service>` and no `<receiver>`, and nothing constructs them outside
tests. The M2 row's "Device acceptance remains the final gate" understated that
there is no Android component to accept. The row now says so.

### F8 — Host update envelope encoded artifact bytes as a decimal array — **fixed** (`f40b4ca`)

Java and Rust agreed and both were bounded, so this was not a correctness bug. It
was a scaling problem to resolve before the envelope carries a real binary: a
decimal-array encoding expands payloads roughly 4× (versus 1.33× for base64) and
costs one `serde_json::Value` allocation per byte on decode.

`artifact_b64` now carries canonical unpadded base64url, matching `new_path_b64`
and `payload_b64` elsewhere in the protocol. The host decoder rejects padding,
whitespace, the standard `+/` alphabet, and non-zero unused trailing bits, so
exactly one encoding maps to any artifact, and it rejects an over-cap artifact
from the encoded length before allocating. A golden envelope emitted verbatim by
the Java encoder is pinned in the Rust tests, so a one-sided change to either half
fails a test rather than a connection.

### F9 — Evidence gates that ran nowhere continuously — **fixed** (`009ceae`)

CI now also runs `test-host-deployment.sh`, `check-m6-release-readiness.sh` (which
subsumes `check-release-reproducibility.sh`), and `test-host-acceptance-local-openssh.sh`.
The OpenSSH lane needs a root-owned privilege-separation directory, created in the
step, and keeps its explicit `69` "tool absent" code; CI reports that skip rather
than letting a missing runner package read as a pass.

`test-zellij-smoke.sh` remains local-only. It hard-requires a `zellij` binary, and
installing an unpinned third-party release would contradict the repository's
pinning policy. Doing it properly needs a pinned version and checksum, which is
now increment 9 in `PLAN.md`.

### F10 — Documentation drift — **fixed** (`6392a32`)

- `README.md` described the diff as "a bounded native LCS reference diff"; it is
  `bounded-myers-v1` (`rust/choosh-core/src/diff.rs:284-340`).
- `rust/choosh-android-transport/src/lib.rs:19-23` documented `COMPOSITION_BOUNDARY`
  as waiting on generated-key acceptance tests that now exist in the same file.

### F11 — Working-tree hygiene — **needs your action**

Three root-owned empty directories sit in the repository root: `run/sshd`, `│`,
and `││`, created 2026-07-26 ~02:39. The names suggest a mis-quoted
`sudo mkdir -p /run/sshd` (or a paste of a box-drawing-prefixed transcript) while
setting up the loopback OpenSSH lane. Git does not track empty directories, so
`git status` shows a clean tree and these are invisible to the normal workflow.

They could not be removed here — they are root-owned and `sudo` requires a
password in this session:

```
sudo rmdir run/sshd run '│' '││'
```

Separately, the root filesystem is 100% full (40 GiB). A first `cargo test` failed
with linker `No space left on device`. The repo root holds a 2.5 GiB `target/`
that duplicates the Justfile's `/tmp/choosh-target` (3.1 GiB). Nothing was
deleted during this review.

## Requirement-level coverage

Legend: **impl** = reachable from a binary or Activity; **domain** = deterministic
headless decision function with tests but no outer composition root; **none** =
not present.

### M0 — Foundation

| Req | State | Note |
|---|---|---|
| M0-R1 | impl (partial) | Package/namespace/catalog and all nine Rust crates present; Kotlin absent (F4) |
| M0-R2 | impl | CI builds both host targets and both Android ABIs |
| M0-R3 | impl | `SoraContentChangeTranslator` + device evidence |
| M0-R4 | impl | Bridge cancellation, typed errors, callbacks, `choosh_bridge_recreate` |
| M0-R5 | domain | Host-key-before-auth proven; concurrent PTY/exec/SFTP/direct-tcpip only on a generated-key harness |
| M0-R6 | impl | Framed hello/welcome over SSH stdio and a `0600` socket, incl. real OpenSSH lane |
| M0-R7 | none | No wgpu, glyphon, or libghostty-vt dependency; `vt.rs`/`renderer_binding.rs` are pure models |
| M0-R8 | domain | `normalize_codex` / `normalize_claude` / `normalize_opencode` + fixtures |
| M0-R9 | domain | `diff.rs` consumes already-fetched bytes; no live blob adapter |
| M0-R10 | none | `choosh-web` is 54 lines with no listener; `http_gateway.rs` is policy only |
| M0-R11 | impl | Signed APK, checksum, SBOM, notices, Obtainium discovery all gated |
| M0-R12 | impl | `minSdk = 26`, ADR 0006 |
| M0-R13 | **not met** | See F4 |
| M0-R14 | impl | Non-blocking preview lane |
| M0-R15 | partial | Zelland grant recorded; fonts, libghostty-vt, wgpu, glyphon open |

### M1–M7

| Milestone | Reachable from a binary | Domain-complete | Absent |
|---|---|---|---|
| M1 | none of R1–R8 | R1, R3, R5, R7, R8; host half of R2 | R4 (terminal), live SFTP write |
| M2 | none | R3, R4, R5, R7, R8; R1/R2 as install plans | Android service component (F7) |
| M3 | none | R3, R5, R9 | R6 (`choosh service run` CLI), R8 (gateway listener) |
| M4 | none | R7/R8 partial (`DiffRequest` seam, bounded Myers) | R1–R6 live Sora/SFTP adapters |
| M5 | none | R1, R3, R4, R5, R6, R8 | R2 (no Maud/Datastar), R7 live streaming |
| M6 | R6 | R5 (host half), R7 partial | R1–R4, R8–R11 |
| M7 | none | none | Spec only; acceptance matrix has no gate |

The `Absent` column is the read from the dependency graph: `choosh-core` and
`choosh-web` have no dependencies, and there is no wgpu, glyphon, libghostty,
Maud, Datastar, or HTTP-server crate anywhere in `Cargo.lock`. Those requirements
have not been started.

## What remains

1. `chooshd`'s outer composition root (F2) — now `PLAN.md` increment 3.
2. Kotlin and Compose adoption, closing M0-R13 (F4).
3. An Android service component for M2, and only then its permissions (F6, F7).
4. Remove the stray root-owned directories and reclaim disk (F11) — needs your
   password.
5. A pinned Zellij for CI (F9) — now `PLAN.md` increment 9.

The systemic risk is unchanged and worth restating: 19,934 lines of
dependency-free policy in `choosh-core` sit behind a `chooshd` binary that answers
one method and an Activity that fails closed by construction. Every additional
domain module raises the cost and the uncertainty of the eventual wiring pass. The
`git.status` thread proves the wiring pass is tractable; the next increments
should widen that thread rather than deepen the core.
