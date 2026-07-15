# M6 detailed design: security hardening and public release

This document makes the [M6 release milestone](../milestones/M6-release.md)
independently verifiable. A release candidate is eligible for device testing only
after its headless gates pass from a clean checkout. Manual observations can add
evidence, but cannot replace a failed or missing automated assertion.

## Scope and invariants

M6 turns the already implemented vertical slices into a supportable public
release. It does not add product capabilities. The release pipeline MUST preserve
these invariants:

- Remote traffic crosses only a host-key-verified SSH connection.
- `chooshd` listens only on a user-owned `0600` Unix socket.
- Android listeners bind only to loopback, use ephemeral ports, and require an
  unguessable process-scoped token where the application protocol permits one.
- Every path, event, Git record, service record, terminal byte stream, and release
  manifest is treated as untrusted input and has an explicit resource bound.
- Durable profiles, workspaces, pins, annotations, and recovery markers survive
  supported upgrades or cause an atomic rollback with the old data intact.
- Published artifacts are traceable to one source revision and one immutable
  dependency lock set.

The public release gate consists of headless gates `H0` through `H8` followed by
device gates `D0` through `D3`. A gate emits a machine-readable result and fails
closed if required evidence is absent.

## Release inputs and outputs

The release job accepts only:

```text
ReleaseInput {
  source_revision: 40 lowercase hexadecimal characters,
  version_name: semantic version without build metadata,
  version_code: monotonically increasing positive integer,
  android_lock_digest: sha256,
  cargo_lock_digest: sha256,
  host_target: linux-x86_64 | macos-aarch64,
  signing_identity: CI secret reference, never key material
}
```

It produces an immutable directory containing:

- stable-name and versioned APKs;
- versioned `chooshd` and `choosh-host` archives for each supported host target;
- SHA-256 checksums, detached signatures, SBOMs, licence notices, provenance,
  migration compatibility metadata, and release notes;
- `release-manifest.json`, which names every artifact by relative path, size,
  digest, media type, target, version, source revision, and signature path;
- `gate-results.json`, which records every gate, command, result, duration, and
  evidence path without embedding credentials or absolute developer paths.

The JSON Schema for both manifests MUST be versioned before the release pipeline
is implemented. Consumers MUST reject duplicate artifact names, unknown digest
algorithms, traversal components, absolute paths, and manifest/artifact mismatches.

## Gate runner

A repository command named `release verify` (the implementation language is not
prescribed) is the single headless entry point. It MUST:

1. start in a clean, detached checkout of `source_revision`;
2. create all mutable state below a disposable test directory;
3. select gates explicitly, with `--headless` selecting `H0`–`H8`;
4. terminate child processes and remove sockets/listeners after every test;
5. emit JUnit XML plus the canonical `gate-results.json`;
6. return nonzero for a failed, skipped-required, timed-out, or missing gate; and
7. support a fixed random seed and print it for reproducibility.

Gate code MUST NOT depend on Android UI automation, a physical display, a human
confirmation dialog, developer home-directory state, public network listeners,
or pre-existing SSH/agent configuration. Network-dependent supply-chain checks
run from a pinned input mirror in CI; an offline replay verifies the captured
inputs.

## H0: source, dependency, and licence gate

H0 verifies a clean tree, lock-file fidelity, stable Android/Kotlin versions,
target SDK policy, Rust minimum-supported-version policy, and allowed licences.
It compares the resolved graph with committed snapshots and fails on unreviewed
dependency additions, preview SDKs in the release lane, yanked packages, known
critical/high vulnerabilities, or missing notices.

Fixtures include one dependency in each rejection class and a synthetic advisory
database snapshot. The negative fixtures MUST fail with stable error codes rather
than matching human-oriented log text.

## H1: protocol and persistence compatibility gate

Every supported prior public version contributes golden protocol frames and
durable-state fixtures. The candidate MUST read old state, migrate it exactly
once, preserve unknown additive fields where the owning format requires it, and
produce the documented canonical form. Re-running migration MUST be a no-op.

The harness injects truncation before and after every durable write boundary,
process termination during migration, full disk, read-only storage, stale
snapshots, and an unsupported future version. Each case MUST yield one of:

- successful atomic migration;
- intact prior state and a retryable recovery marker; or
- intact prior state and a typed incompatible-version refusal.

No case may leave a partially upgraded authoritative store.

## H2: parser, path, and resource fuzz gate

Coverage-guided targets include RPC framing, JSON schemas, normalized agent
events, root-confined paths, Git machine output, diff construction, terminal
escape parsing, Markdown/annotation anchors, service metadata, HTTP ranges,
WebSocket frames, and release manifests.

Each target defines maximum input bytes, decoded records, allocation, nesting,
processing time, and output bytes. CI runs a bounded deterministic corpus on every
change and a longer scheduled campaign. A crash, panic across an FFI boundary,
out-of-root access, unbounded growth, sanitizer finding, or timeout is a failure.
Every discovered input becomes a minimized, committed regression fixture unless
it contains sensitive material.

## H3: trust-boundary integration gate

The headless harness starts disposable SSH, fake SFTP, `chooshd`, Zellij-adapter,
Git-repository, HTTP, WebSocket, and SSE peers inside an isolated network
namespace or equivalent host sandbox. It asserts:

- unknown host keys require an explicit trust result; changed keys never fall
  back to password or permissive verification;
- malformed, replayed, out-of-order, and oversized RPC/event traffic fails with
  typed errors and bounded reconnect behavior;
- symlink swaps and traversal encodings cannot escape the registered root;
- Git helpers, text-conversion drivers, hooks, and shell interpolation cannot be
  triggered through protocol input;
- the daemon socket mode and peer identity are checked before requests execute;
- tunnels can target only a currently registered loopback service; and
- disconnect closes Android-side listeners while leaving declared persistent host
  processes in their specified state.

Packet/socket inspection MUST prove that no process opens a wildcard or public
listener. Tests enumerate listening sockets rather than infer safety from config.

## H4: WebView-origin server gate

This gate tests the loopback HTTP surface without launching a WebView. An HTTP
client and adversarial origin server verify token rejection, CSP and security
headers, navigation policy inputs, range semantics, cache bounds, Markdown HTML
sanitization, MIME handling, redirect refusal, WebSocket/SSE closure, and asset
root confinement.

The test corpus contains script URLs, active SVG, malformed ranges, encoded path
separators, redirect chains, oversized headers, decompression bombs, mixed-content
targets, and cross-origin requests. Secret scanners inspect responses and logs for
SSH material, tokens, remote absolute paths, document contents, and terminal data.

## H5: reconnect and fault-injection gate

A deterministic virtual clock drives reconnect storms, delayed acknowledgements,
duplicate events, spool loss, sequence gaps, stale snapshots, partial writes, SSH
channel exhaustion, daemon restarts, and client process death. Assertions cover:

- capped exponential backoff with seeded jitter and no busy loop;
- bounded queues with documented coalescing or loss indicators;
- idempotent command retry only where the protocol declares it safe;
- deterministic resynchronization after gaps;
- no duplicate durable item, notification, pin, or annotation; and
- eventual resource cleanup after the terminal state.

Every state machine MUST expose a test-only snapshot containing logical state,
queue sizes, retry deadline, and last accepted sequence. It MUST NOT expose
credentials or production-only mutation controls.

## H6: terminal correctness and performance gate

The terminal conformance runner feeds recorded PTY byte fixtures and input events
directly to the native Rust terminal stack, without Compose, a WebView, or a GPU
display. It compares semantic screen grids, cursor/mode state, scrollback hashes,
damage regions, and encoded outbound bytes with golden results.

A software or headless GPU backend runs renderer smoke tests and device-independent
frame captures. The gate covers UTF-8 boundaries, wide/combining glyphs, bidi
policy, alternate screen, bracketed paste, mouse modes, resize/reflow, IME
composition, extra keys, malformed escapes, device loss, and bounded scrollback.
Performance budgets are evaluated on pinned CI hardware and reported separately
from correctness; regressions beyond the committed tolerance fail the gate.

## H7: reproducibility, provenance, and signing gate

Two isolated builders produce unsigned inputs from the same release input. After
normalizing only documented nondeterministic signature fields, their digests MUST
match. Signing occurs after reproducibility comparison in a restricted job that
receives digests, not a mutable source checkout.

Verification starts from public keys and the manifest, confirms every artifact
digest/signature/SBOM/provenance link, rejects substitution and downgrade
manifests, and proves the update client checks version monotonicity and target
compatibility before installation. Logs and artifacts are scanned to ensure no
private key, passphrase, CI token, or signing-session material escaped.

## H8: installation and upgrade model gate

Host installers run in disposable Linux and macOS environments without root. The
harness verifies install, start, stop, upgrade, rollback, socket ownership/mode,
version negotiation, concurrent old/new client behavior, and complete uninstall
without deleting user data unless explicitly requested.

Android persistence migrations run on filesystem/database snapshots from the last
two supported versions through `old -> intermediate -> candidate`, `old ->
candidate`, candidate reinstall, and simulated rollback. Canonical exports of
profiles, trusted-host records, workspace selection, ordered pins, annotations,
and recovery state MUST match expected fixtures. Keystore behavior is represented
by an interface-compatible fake that can model locked, invalidated, and missing
keys; physical-keystore behavior remains a device gate.

## Device-only gates

Headless success is necessary but not sufficient:

- `D0` verifies APK signatures, clean install, Obtainium discovery, and two real
  upgrades on the supported Android API matrix.
- `D1` verifies TalkBack semantics, touch targets, contrast, keyboard traversal,
  IME/accessory behavior, reduced motion, rotation, and phone/tablet layouts.
- `D2` verifies native GPU correctness, device loss, background/foreground,
  thermal/performance budgets, and representative vendor devices.
- `D3` follows only published documentation from a clean Android device and clean
  supported host to reach an agent terminal and registered web preview.

Each device run uploads structured assertions, device/build identity, sanitized
logs, and screenshots only where the assertion is inherently visual. A human
checklist without per-assertion evidence is not a passing result.

## Threat-model closure

Every threat-model entry receives a stable ID, owner, severity, affected gate,
test identifier, and status: `mitigated`, `accepted`, or `deferred`. `accepted`
requires written rationale and an expiry/review release. Critical/high findings
cannot be accepted for public 1.0. The release verifier fails when an entry lacks
a test, an acceptance has expired, or evidence refers to a test absent from the
current result set.

## Failure handling and publication

Artifacts from a failed gate MUST remain quarantined and MUST NOT be attached to
a public release. Retrying a gate creates a new result record linked to the same
immutable input; it does not overwrite prior evidence. Publication is a promotion
of already verified digests, never a rebuild.

A post-publication verification job downloads artifacts through the public release
path and repeats manifest, checksum, signature, installability, and update-feed
checks. Failure marks the release feed unavailable for automatic update and opens
a release incident; it never silently republishes a different artifact under the
same version.

## Requirement traceability

| Milestone requirement | Primary evidence |
| --- | --- |
| M6-R1 | Threat closure table plus H0–H8/D0–D3 links |
| M6-R2 | H2 corpus, crash-free reports, regression fixtures |
| M6-R3 | H1 and H5 fault matrices |
| M6-R4 | H3/H4 socket, boundary, and secret-scan assertions |
| M6-R5 | H7 signatures/provenance and H8 host upgrade results |
| M6-R6 | H0/H7 manifest, SBOM, notices, reproducibility evidence |
| M6-R7 | H8 migration fixtures and D0 Obtainium upgrades |
| M6-R8 | D1 structured accessibility/device assertions |
| M6-R9 | D3 clean-room documentation run |
| M6-R10 | H0 resolved toolchain report |
| M6-R11 | H6 conformance/performance and D2 device matrix |

## Exit criteria

M6 is complete only when:

1. `release verify --headless` passes twice from clean checkouts with identical
   release inputs and reproducible unsigned outputs;
2. all required device gates pass for the release candidate digest;
3. no critical/high threat or vulnerability remains open;
4. the public-path post-publication verifier passes;
5. recovery from the previous two supported versions is demonstrated; and
6. a reviewer can map every M6 requirement to current, retained evidence without
   relying on prose-only claims.
