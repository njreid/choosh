# M0: Foundation and independently falsifiable risk spikes

Status: Detailed design

## Outcome

A clean checkout can reproduce the project skeleton and independently prove or reject every load-bearing technical boundary. M0 is not complete merely because an APK opens: each spike below produces machine-readable evidence and has its own pass/fail gate.

This milestone implements test surfaces, not polished product navigation or production persistence. It preserves the [SSH-only boundary](../adr/0001-system-boundary.md), the [host/Zellij ownership split](../adr/0002-host-daemon-and-zellij.md), and the presentation choices in [ADR 0003](../adr/0003-android-surfaces.md) and [ADR 0005](../adr/0005-native-terminal.md).

## Verification philosophy

The default verification path MUST be non-interactive. A test may launch local processes, an SSH test server, a virtual display, an emulator, or a fake Android bridge, but it MUST NOT require taps, visual judgment, credentials, a developer's home directory, or an Internet service after dependencies are cached.

Every spike MUST provide:

- one stable command usable by CI;
- deterministic fixtures with fixed clocks, UUIDs, ports or port discovery, and random seeds;
- a JUnit XML, JSON, or TAP result artifact;
- explicit timeout and resource bounds;
- at least one negative-path assertion;
- diagnostics that redact keys, terminal content, clipboard data, opaque capabilities, and project contents.

Device-only behavior is isolated in a separate instrumentation lane. A device test MUST expose assertions through instrumentation results, screenshots or GPU hashes only as supplemental diagnostics; a human looking at a screen is never the oracle.

## Repository and component boundary

The skeleton contains these ownership units:

```text
android/app        packaging and Android lifecycle
android/ui         Compose hosts and platform adapters
rust/choosh-core   state actor and client domain model
rust/choosh-android typed Kotlin/Rust bridge
rust/choosh-web    loopback rendering/gateway primitives
rust/chooshd       authoritative workspace/item metadata
rust/choosh-host   stdio RPC, stream and hook CLI
protocol/v1        schemas and deterministic fixtures
testkit            fake SSH, fake SFTP, RPC and terminal fixtures
```

Android package and namespace are `ai.choosh`. Dependency and SDK policy follows the [Android toolchain specification](../specs/android-toolchain.md). Rust crates MUST deny unintentional platform coupling: host crates compile for Linux x86_64 and macOS arm64, while Android-facing crates compile for Android arm64-v8a and x86_64.

The required host lanes run in the `host-rust` CI matrix on the explicit
`ubuntu-24.04` (x86_64) and `macos-15` (arm64) GitHub-hosted runner labels.
Each lane asserts `uname` identity and the exact Rust 1.96.1 compiler before
running `cargo fmt --all -- --check`, strict workspace Clippy, and the complete
workspace test suite. Specification syntax runs once in the Linux lane because
it is platform-independent; it is not used as a substitute for either host
compile/test lane.

## Common headless testkit

M0 establishes a repository-owned testkit used by later milestones.

### Hermetic host sandbox

The sandbox creates a temporary Unix user-equivalent directory layout, a project root, a state directory, a `0600` daemon socket, and fake or real Zellij behind the same session interface. Tests inject all paths; production defaults are never read. The sandbox records child PIDs and fails teardown if a process or listening TCP socket remains.

### Deterministic transport

The transport harness supports:

- an in-process byte stream for framing and actor tests;
- a local SSH server with a generated test host key for multiplexing tests;
- scripted latency, truncation, disconnect, short-read, channel-close, and backpressure faults;
- counters for open channels, queued bytes, allocations, and leaked tasks.

The generated key is test-only, lives under the temporary directory, and never appears in documentation fixtures.

### Golden fixtures

Fixtures are immutable inputs under versioned directories. Golden-output updates require an explicit command and reviewable diff; ordinary tests MUST NOT rewrite goldens. JSON fixtures MUST parse with `jq`, schemas MUST declare draft 2020-12, and examples MUST validate against their declared schemas.

## Spike A: reproducible build and dependency/legal entry gates

### Interface and decisions

- Gradle wrapper, version catalog, dependency verification metadata, JDK, NDK and Rust toolchain are pinned.
- Production builds use stable dependencies. The latest available next-platform preview lane is non-blocking and cannot publish artifacts.
- `minSdk` is selected from supported-device and required-API evidence, recorded in an ADR, and tested by lint; it is not inferred from `compileSdk`.
- Before source is copied or an editor dependency becomes structural, ownership/licence findings for Zelland, Sora Editor, libghostty-vt, wgpu, glyphon, bundled fonts and native transitives are recorded. An unresolved redistribution obligation fails this spike.

### Headless evidence

The build verification command MUST, from a clean checkout:

1. build Android arm64-v8a and x86_64 variants;
2. build and test both host targets (native or through the documented cross-build container);
3. assert the APK manifest package is `ai.choosh`;
4. assert dependency locks and verification metadata are unchanged;
5. emit an SPDX or CycloneDX inventory and a licence-policy result;
6. reject dynamic versions, preview production dependencies, unapproved native libraries, and an absent `minSdk` ADR.

### Pass gate

The stable toolchain combination packages Sora and the Rust bridge in one APK on JDK 17. Any compatibility exception follows the four-part exception rule in the [toolchain specification](../specs/android-toolchain.md). Legal provenance is either approved with recorded obligations or the dependent spike is explicitly stopped; "research pending" is not a pass.

## Spike B: canonical actor and Kotlin/Rust bridge

### Command/event contract

Kotlin sends typed commands with a `command_id`, target generation and, for mutations, expected revision. Rust returns exactly one terminal outcome:

```text
completed(result) | failed(typed_error) | cancelled
```

Rust publishes immutable revisioned snapshots. Callbacks carry a monotonically increasing subscription generation; Kotlin MUST discard callbacks from a closed or superseded generation. Cancellation is idempotent and does not roll back an already committed state mutation.

### Document edit state machine

The Sora spike uses this minimal state machine:

```text
Unopened -> Clean(revision=1)
Clean(r) --valid local edit(base=r)--> Dirty(r+1)
Dirty(r) --valid local edit(base=r)--> Dirty(r+1)
any open state --stale edit--> unchanged + ResyncRequired(current_revision)
any open state --close--> Unopened
```

Programmatic Sora updates carry an adapter suppression token. They update the widget but MUST NOT return as local edits. Text ranges use an explicitly tested UTF-16-to-UTF-8 conversion and reject invalid scalar boundaries.

### Headless evidence

- Rust model tests generate edit sequences, Unicode boundaries, cancellation races and callback reorderings from fixed seeds.
- A JVM bridge contract test uses the packaged native library without Compose and asserts typed success, error, callback and cancellation behavior.
- A Robolectric or instrumentation adapter test feeds synthetic `ContentChangeEvent` values and asserts exactly one Rust mutation per user edit and zero mutations for projection updates.
- A saved-state fixture serializes only durable client identifiers/revisions, reconstructs the engine, and proves stale callbacks cannot mutate the new generation.

### Failure behavior and pass gate

Panics and Java exceptions cannot cross the FFI boundary. Invalid edits produce a typed validation or stale-revision result; callback failure closes that subscriber without stopping the actor. The spike passes only if race tests complete under a fixed timeout with no leaked tasks and process recreation yields the same snapshot as uninterrupted replay.

## Spike C: verified SSH multiplexing

### Connection state machine

```text
Disconnected
  -> VerifyingHostKey
  -> Authenticating
  -> Ready(connection_generation)
  -> Reconnecting(attempt, next_deadline)
  -> Ready(new_generation)

VerifyingHostKey --unknown--> TrustDecisionRequired
VerifyingHostKey --mismatch--> Failed(host_key_mismatch)
Authenticating --failure--> Failed(authentication_failed)
```

Only the exact stored host key can reach `Ready`. There is no test or production bypass flag in the runtime path. Commands are fixed executable plus separately encoded arguments; user-controlled strings are never interpolated into a shell command.

One ready SSH transport supports concurrent PTY, exec, SFTP and direct-tcpip channels. Channel failure is local unless the transport itself fails. Each channel and aggregate buffered bytes have bounds and deadlines.

### Headless evidence

The local SSH harness MUST prove:

- first-seen, accepted-key, rejected-key and changed-key behavior;
- authentication failure without secret-bearing logs;
- simultaneous progress on PTY, exec, SFTP and an HTTP/WebSocket/SSE direct-tcpip echo service;
- fairness while SFTP is throttled and PTY latency is measured;
- disconnect during every channel type, reconnect generation changes, and stale-channel rejection;
- no non-loopback listener created by Android-side test components.

The initial measurable budget is: under an injected 100 ms round-trip delay and 8 MiB throttled SFTP transfer, a 32-byte PTY echo completes within 750 ms at the 99th percentile in 100 deterministic samples. Failure to meet it triggers profiling or an ADR for a second bulk SSH connection under the same verified key; it may not be silently waived.

## Spike D: daemon socket and versioned RPC

### Boundary

The only daemon listener is its per-user Unix socket. Startup creates the parent state directory without group/other access and socket mode `0600`; an unsafe existing path fails closed. Android reaches it only through `choosh-host rpc --stdio` over SSH exec, as specified by the [host protocol](../specs/host-protocol.md).

### Framing and lifecycle

The bridge reads a four-byte big-endian length followed by UTF-8 JSON. Before negotiation it accepts only `hello`; afterward it accepts only negotiated version-1 envelopes. Requests may complete out of order. EOF cancels outstanding bridge work without stopping `chooshd` or Zellij.

Malformed length, invalid UTF-8/JSON, unknown envelope kind, or a frame above 1 MiB terminates the bridge with a stable nonzero exit classification. Diagnostic stderr is bounded and contains neither frame content nor capabilities.

### Headless evidence

- Byte-level golden tests cover fragmentation at every boundary, coalesced frames, zero length, maximum length, maximum-plus-one, truncation, and slow readers/writers.
- Schema tests validate every `protocol/v1/examples` file and reject curated invalid fixtures.
- A black-box test starts `chooshd`, asserts filesystem modes, invokes `choosh-host rpc --stdio`, performs hello/welcome and parallel requests, then proves no TCP listener exists.
- Fuzz targets exercise the frame decoder and envelope deserializer with allocation limits; a bounded CI corpus runs on every change and a longer job runs separately.

## Spike E: native terminal engine, renderer and input

The normative behavior is in the [terminal specification](../specs/terminal-experience.md). M0 separates terminal correctness from GPU/device integration so most failures are reproducible headlessly.

### Internal interfaces

```text
TerminalEngine.feed(bytes) -> Damage + TerminalSnapshot
TerminalEngine.input(TypedInput, target_generation) -> encoded bytes | stale_target
TerminalRenderer.bind(surface, snapshot_source, target_generation)
TerminalRenderer.render(Damage) -> FrameStats
```

The engine owns VT modes, grids, scrollback, selection and input encoding. The renderer consumes immutable snapshots/damage. This boundary is also the concrete fallback seam: if the wgpu/glyphon port fails its go/no-go gate, a CPU cell-grid renderer using the same snapshots is the named plan B. Selecting plan B or accepting degraded budgets requires an ADR before M1.

### Headless engine suite

- Recorded, licensed VT byte streams for Zellij and each supported agent produce normalized grid goldens, never screenshot-only expectations.
- Property tests split identical input at every byte boundary and require identical terminal state.
- Fixtures cover alternate screen, SGR, wide/combining text, invalid UTF-8, hyperlinks, cursor shapes, resize, mouse modes and bracketed paste.
- Typed input vectors assert mode-dependent encoded bytes for IME commit, hardware keys, extra keys, paste and accessibility actions.
- Stale target generations, oversized paste and multiline warning decisions have negative fixtures.

### Automated device lane

Instrumentation recreates surfaces, rotates, locks/unlocks, backgrounds, forces low-memory recreation where supported, and injects IME composing/commit events. Assertions inspect engine state, active generation, frame counters and bounded perceptual hashes. GPU loss is injected behind the renderer interface. A blank frame, duplicate input or input delivered to a previous target is a test failure.

The initial falsifiable budgets are 60 fps when continuously damaged on a representative mid-tier device, p99 committed-input-to-engine latency below 50 ms excluding network time, no unbounded memory growth during a 10-minute 10 MiB/minute stream, and recovery to a rendered frame within 2 seconds of surface recreation. Hardware-specific exceptions require recorded device identifiers and an ADR.

## Spike F: observational agent normalization

Each Codex, OpenCode and Claude Code adapter is tested as a pure transformation from captured vendor fixture to the [normalized event schema](../../protocol/v1/agent-event.schema.json). Adapters ignore incomplete Choosh environments and MUST NOT emit approval, denial, rewritten input or model context.

The harness invokes `choosh-host emit` with stdin/environment fixtures and a fake daemon socket. It asserts bounded input, path treatment as untrusted text, redaction, exit status, timeout, and identical normalized JSON across runs. Each agent requires at least one `input_required`, `turn_completed`, `files_changed`, malformed, oversized and no-Choosh-context fixture. No test launches or contacts the vendor agent.

## Spike G: bounded client-side textual diff

The spike follows [ADR 0004](../adr/0004-client-side-git-diff.md) and the [Git diff specification](../specs/git-diff.md).

A hostile repository fixture includes spaces/newlines/non-UTF-8 paths, symlinks, renames, deletion, untracked and binary files, submodules, attributes, external diff/textconv configuration, an unborn branch, and files at every size limit. The host returns machine-readable status and identity-bound HEAD/index/worktree byte versions. Android Rust alone creates hunks.

Tests MUST prove external helpers and hooks are not executed, paths cannot escape the registered root, snapshot mutation is detected before/after worktree streaming, and binary/oversized inputs yield metadata-only results. Golden hunks define Choosh V1 behavior; exact parity with every Git display policy is not claimed. Fuzz/property tests bound bytes, lines, hunks, execution time and allocations.

## Spike H: declared service tunnel and authenticated gateway

A fake registered service exposes deterministic HTTP, WebSocket and SSE endpoints on host loopback. A direct-tcpip channel maps it to an ephemeral Android loopback listener protected by a per-process token/cookie. Tests assert protocol fidelity, origin/navigation policy, token rejection before forwarding, port-registration enforcement, disconnect cleanup, and that neither side binds a public interface.

The test launches a hostile HTTP fixture attempting external navigation, oversized headers, mixed content, internal-route access and cookie reuse. All fail with stable outcomes and no SSH credential, remote path, or internal Markdown token exposure.

## Spike I: release discovery

A release fixture builds a monotonically versioned development APK, signs it with an ephemeral CI test key, emits checksums and release metadata, and serves the metadata from a local HTTP fixture implementing the subset consumed by the Obtainium discovery check. Headless parsing MUST select the newer stable filename and reject a bad checksum/signature association.

Publishing to GitHub Releases and verifying against the real Obtainium client is a protected release-lane integration test. It may require credentials and network access, but its assertions and logs remain automated. Production signing material is never used in pull-request CI.

## Gate matrix

| Gate | Headless required | Device/integration required | Blocks M1 |
| --- | --- | --- | --- |
| A build/legal | clean build, locks, SBOM, policy | packaged APK install smoke | yes |
| B bridge | Rust/JVM contracts, race replay | process recreation instrumentation | yes |
| C SSH | local server multiplex/fault suite | representative network smoke | yes |
| D RPC | black-box socket/stdio/fuzz suite | none | yes |
| E terminal | VT/input goldens and bounds | GPU, surface and IME suite | yes; plan B allowed only by ADR |
| F adapters | pure fixture normalization | optional vendor smoke | no for M1, required before M2 |
| G Git | hostile repository and diff goldens | Android performance smoke | no for M1 core, required before Git UI |
| H services | local protocol/security fixture | WebView isolation instrumentation | no for M1, required before M3 |
| I release | local signed-artifact discovery | protected GitHub/Obtainium lane | required for public test distribution |

## M0 exit gate

M0 exits when all M1-blocking gates pass from a clean checkout with pinned dependencies, every remaining gate is either passing or explicitly deferred to its consuming milestone, and no legal entry gate is unresolved. The evidence manifest records tool versions, fixture versions, commands, durations, hashes and artifact paths without embedding machine-specific absolute paths.

Changed SSH keys fail closed; the daemon exposes only a `0600` Unix socket; a single verified connection carries independent channels; the bridge survives cancellation and recreation; terminal state/input correctness is proven headlessly and GPU/IME behavior by automated instrumentation.

## Excluded

Polished navigation, production credential/profile storage, production workspace persistence, background notification delivery, editing saves, arbitrary service browsing, public-release support, and human-only acceptance procedures are outside M0.
