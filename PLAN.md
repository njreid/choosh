# Choosh delivery plan

Status date: 2026-07-20

This is the operational status ledger.  The [delivery milestones](docs/milestones/README.md)
remain the source of scope and exit gates; the [detailed designs](docs/design/README.md)
remain the source of verification requirements.  A checked box here means the
named slice has evidence, **not** that its enclosing milestone is complete.

## Current position

- [x] Repository, pinned Android/Rust toolchain, dependency locking and verification,
  deterministic Rust domain/testkit foundations, and the Sora revisioned-edit seam.
- [x] Typed bounded host framing, first-frame daemon/socket lifecycle, and local
  RPC-process evidence.
- [x] Signed universal `v0.0.1` APK release plumbing, checksum, SBOM, notices, and
  Obtainium-compatible GitHub Release discovery.
- [x] Android production baseline is API 36; `minSdk = 26` is recorded in
  [ADR 0006](docs/adr/0006-android-min-sdk.md).
- [x] Font assets and terminal provenance candidates are recorded, including the
  requested Geomini and Iosevka Charon Mono UI/terminal direction.
- [x] Sora is an accepted LGPL-2.1-or-later dependency for the editor spike; its
  exact packaging/source/notice evidence remains an explicit release gate rather
  than an implied consequence of dependency acceptance.
- [x] User-approved SSH-key import is modeled as an opaque, redacted credential
  reference plus public-key metadata; it neither reads Termux private storage nor
  exposes private-key material to the Rust domain.
- [x] SSH transport dependency admission has a deterministic contract for lock,
  licence, Android ABI, host-key-before-auth, channel, and fairness evidence.
- [x] The isolated Russh graph is selected, lockfile-pinned, and compiles for Android
  arm64-v8a/x86_64 under its [time-bounded exception](docs/adr/0007-russh-crypto-exception.md).
- [x] Exact host-key admission, injected credential signing, bounded fixed-command RPC,
  root-confined SFTP request boundaries, bounded Russh SFTP subsystem admission, and
  loopback-only forwarding have deterministic local evidence. SFTP reads require a
  canonical server-root attestation; writes fail closed until atomicity is proven.
- [x] The fixed host dispatcher and `chooshd rpc --stdio` now share an explicit bounded
  state/socket plan. Their actual platform process adapter remains an injected outer
  capability, so no ambient path lookup or shell execution has been introduced.
- [x] The SSH stdio-to-private-socket relay now completes and consumes the required
  daemon hello/welcome exchange before forwarding a bounded RPC frame; a subprocess
  acceptance test proves `chooshd rpc --stdio` reaches `host.describe` without exposing
  the handshake on the SSH-facing stdio stream.
- [x] Native authenticated-plan tokens are type-separated from generic requests, and the
  host has an injected direct-process adapter that emits only the fixed daemon argv. Both
  remain fail-closed until their outer platform compositions provide verified transport.
- [x] Android profile/known-host metadata, authenticated-operation composition, an opaque
  JNI plan boundary, and a Keystore signer boundary have headless evidence. The signer
  refuses to construct a request before exact host admission and exposes no key material;
  its admitted per-connection challenge callback binds the opaque credential and public
  metadata once, accepting only bounded SSH payloads thereafter;
  the JNI plan deliberately does not claim a live connection until native transport-side
  exact-host admission and Keystore signing are composed.
- [x] The native bridge now makes exact host-key admission a typed capability required
  before its Keystore public-key-authentication boundary; this is a headless ordering seam,
  not an implementation of a live JNI/Russh connection.
- [x] The Android candidate flow now has deterministic profile connection and authenticated
  workspace-status controllers. Native plans cannot advance without bridge ownership and fail
  closed to a typed transport-unavailable result.
- [x] Documentation now distinguishes the Java/View M0 connection-status screen and bounded
  `bounded-myers-v1` deterministic diff from the future Compose/navigation and production-diff
  targets. Agent integration is described as per-agent pluggable adapters, not neutrality.
- [x] The bounded reference diff preserves per-line `LF`/`CRLF` identity, including
  line-ending-only changes, with byte-exact headless reconstruction coverage.
- [x] Text diff construction now uses bounded Myers frontiers rather than a quadratic
  old-lines by new-lines LCS matrix; large unchanged inputs have headless coverage and
  retained-frontier exhaustion still returns metadata only.
- [x] The first bounded `git.status` vertical host slice accepts only registered opaque
  workspace UUIDs, executes a fixed environment-cleared Git plan, reconciles paths under
  the registered root, and proves byte-preserving paths through a private Unix socket.
- [x] Android has a headless `git.status` request/response codec and controller with
  injected request IDs, strict envelope/base64 validation, and typed failure projection;
  a constructor-injected composition now reaches it through the planned native connector
  only after a connected result, while host-key rejection cannot construct a controller or
  issue an RPC. The underlying JNI runtime remains unavailable until it can invoke its
  registered socket and Keystore callback.
- [x] The Android transport composition root now joins opaque runtime capabilities to the
  pinned Russh admission path in the required order: exact host session, username, Keystore
  signer, injected stream, then Russh host-key callback before signing. Generated Ed25519
  acceptance proves a changed host key reaches no signer and an exact host key reaches the
  injected signer and authenticates; the custom-signer buffer contract is explicit. Concrete
  JNI runtime adapters and device evidence remain required before it can report connected.
- [x] The Android JNI plan ABI now carries a non-zero opaque, payload-only Keystore challenge
  callback handle. The Java callback retains key material on Android, Rust retains only the
  opaque handle after exact-host admission, and focused JVM/Rust tests prove the ABI rejects
  missing handles without exposing signing inputs or outputs through plan metadata.
- [x] Android now has a constructor-injected bounded socket adapter and an explicit per-attempt
  native-runtime lease. The lease releases opaque socket and signer registrations exactly once
  after plan rejection, cancellation, completion, or a late callback; headless JVM fakes cover
  the lifecycle. The JNI bridge does not yet invoke those registrations during a verified
  transport operation.
- [x] The JNI runtime overload now retains its bounded callback object only in the owning plan
  allocation and releases it before token reuse or on generation recreation. The bridge remains
  fail-closed until that allocation drives the verified stream and signer transport.
- [x] The Android transport crate no longer depends on the JNI bridge, making the bridge the
  explicit outer composition root for the remaining JNI-to-Russh adapter. The runtime contract
  now records that opaque IDs alone cannot establish a session: Android-owned registrations must
  resolve validated metadata into injected stream, exact-host-session, and signer capabilities.
- [x] The JNI runtime callback now has a versioned, bounded non-secret metadata capsule. Rust
  validates canonical username, exact host fingerprint, and public-key identity metadata before
  a future session can be composed; endpoint, credential selection, key material, signatures,
  paths, and commands remain outside the capsule.
- [x] The JNI runtime callback now also supplies a bounded canonical public SSH key. Rust parses
  it and requires its SHA-256 fingerprint to match the fixed lease metadata before a future
  signer can be created, with deterministic mismatch coverage.
- [x] Android now has a concrete constructor-injected bounded runtime lease adapter. It owns one
  validated socket, fixed identity capsule, public key, and payload-only signer callback without
  a static registry; headless tests cover stale-lease rejection and exactly-once socket release.
- [x] Runtime callback ownership is now thread-safe rather than mutable-borrow serialized. This
  permits independent bounded read and write workers without allowing a blocked socket read to
  prevent SSH progress; close remains one-way and exactly once.
- [x] The Android transport now has a headlessly tested asynchronous adapter for a blocking
  runtime lease. It runs each bounded read/write on Tokio's blocking pool, keeps the two socket
  directions independent, enforces the configured chunk limits, and maps failures to opaque I/O.
- [x] The JNI outer root now composes a validated runtime lease into a bounded asynchronous
  stream, exact-host Russh session, canonical public key, and payload-only signer. Construction
  has deterministic coverage proving it cannot invoke the signer; only the verified SSH path may.
- [x] Java JNI plans now support an explicit one-way transfer to a `SessionLease`; headless
  coverage proves the transferred session, not the connection-completion plan, owns exactly one
  native cancellation and Android runtime release.
- [x] The Rust bridge now has a constructor-owned bounded session registry with deterministic
  plan-ownership and clear-before-release coverage, ready to retain the verified fixed-RPC
  session without another ambient callback lookup service.
- [x] The bridge now has a bounded per-session fixed-RPC actor foundation: a one-slot command
  queue, one-shot replies, and explicit close. It keeps session I/O out of registry locks; JNI
  export wiring and the actor acceptance fixture remain the next increment.
- [x] Plan-owned session lookup now clones the fixed-RPC actor before awaiting its reply, so no
  registry borrow or lock spans I/O. Unknown plan capabilities are rejected deterministically.
- [x] The JNI native open now consumes its exact plan-owned callback allocation, composes the
  bounded Android stream through exact-host SSH admission and Keystore signing, then retains the
  authenticated `AndroidRpcSession` behind a one-slot fixed-RPC actor. Java transfers only a
  successful plan to `JniNativeSession`; its bounded RPC and exactly-once close retain the sole
  native token and Android lease. JVM and Rust headless tests cover the ownership transition and
  actor lifecycle. A full JVM callback/socket-to-real-`chooshd` acceptance remains required.
- [ ] Device evidence was attempted on 2026-07-25 using a one-hour, SSM-managed `m6i.metal`
  runner after `m7i.metal` was unavailable in-region. The runner accepted its bounded shutdown
  command and began Android toolchain setup, but AWS credentials expired during the system-image
  download, so no instrumentation result was retrieved and this is not acceptance evidence.
- [x] Android release selection now has a headless bounded planner that selects a canonical
  newer stable APK, verifies its SHA-256 and pinned signing certificate through injected
  boundaries, and returns data-only staging instructions. Download, app-private writing, and
  user-mediated package installation remain outer adapters.
- [x] Host upgrade orchestration now has a filesystem-backed deterministic acceptance fixture:
  an immutable staged artifact is SHA-256 verified before atomic activation, health is checked
  only for the candidate version, corrupt staging is discarded without touching the active
  artifact, and an unhealthy activation rolls back exactly once to the prior verified artifact.
- [x] Authenticated updater wiring is explicitly gated: the current workspace-confined SFTP
  surface cannot perform atomic writes and the fixed SSH dispatcher exposes only RPC, so neither
  can be repurposed for deployment. A versioned immutable-upload and host-owned activation
  protocol remains required before Android deployment wiring can begin.
- [x] The host now has an injected immutable deployment transaction that accepts only bounded
  release version/digest/bytes and keeps release paths, atomic selection, service activation,
  and private-socket health inside host adapters; digest failures discard stages and every
  post-activation service or health failure rolls back once.
- [x] The bridge now owns a bounded, one-close-only per-plan runtime callback allocation with
  deterministic bounds and released-lease tests. A JNI `GlobalRef` adapter remains required to
  supply its socket and signing callbacks on Android.
- [x] A generated-key Android-shaped `git.status` acceptance reaches the fixed
  `choosh-host rpc --stdio` SSH command through a bounded native-stream composition and returns
  a bounded terminal envelope. Its SSH fixture invokes the real host stdio relay, which completes
  hello/welcome and forwards the request to a registered real `chooshd` `git.status` handler on a
  private Unix socket. It rejects a non-request envelope before SSH. This proves the Rust
  composition seam and real daemon method only; it does not yet bind a JVM socket or invoke the
  Java callback from Android.
- [x] Blob capability completion consumes a bounded reader and stops an oversized source
  after the first byte above its declared limit; daemon fixture roots are unique under
  parallel headless test execution.
- [x] Handshake, request, and socket-relay readers admit two coalesced frames solely to
  classify duplicate replies explicitly, then fail closed before accepting either result.
- [x] Linux daemon accepts verify `SO_PEERCRED` against the daemon effective UID before
  protocol reads. Non-Linux Unix builds fail closed until an equivalent credential adapter
  is implemented.
- [x] Reconnect policy now rejects first-trust downgrade after an authenticated generation;
  a deterministic headless acceptance test proves logical retry timing, stale-channel
  invalidation, terminal changed-key handling, and replay-versus-snapshot recovery actions.
- [x] A generated-key protocol harness proves fixed exec, SFTP subsystem, and loopback-only
  forwarding can coexist on one authenticated Russh transport. It is not yet the full
  Android-to-host vertical acceptance harness.
- [x] The macOS host-Rust lane canonicalizes fixture roots before comparing reconciled paths,
  preserving the same containment assertion on `/var` and `/private/var` systems; its private
  socket admission now verifies the peer's effective user through `getpeereid` before protocol
  bytes are read.
- [ ] A real Android SSH transport passes dependency admission and interoperability
  under the selected graph; see [SSH transport choice](docs/evidence/m0-ssh-transport-choice.md).
- [ ] An SSH interoperability harness proves exact host-key verification before
  authentication and concurrent PTY, exec, SFTP, and loopback-only `direct-tcpip`
  on one connection.
- [x] The project owner granted Choosh permission to use the exact Zelland source;
  its [recorded grant](docs/licenses/zelland-grant.md) clears that provenance item.
- [ ] The native terminal still needs a pinned native graph and Android implementation/device
  conformance result; see [terminal provenance](docs/licenses/terminal-provenance.md).
- [ ] Bundled-font authoritative upstream identity, deterministic fallback layout
  evidence, and Sora distribution evidence remain release blockers; see
  [terminal provenance](docs/licenses/terminal-provenance.md) and [Sora packaging](docs/licenses/sora-packaging.md).
- [ ] No preview currently supplies the opt-in redacted diagnostic bundle required
  for supportable public distribution; its headless-first contract is in
  [diagnostics](docs/specs/diagnostics.md).

`v0.0.1` is an early signed distribution slice, not an assertion that M0 or a
public-1.0 milestone has passed.

## Milestone ledger

| Milestone | State | Evidence / remaining gate |
|---|---|---|
| M0 — Foundation | **In progress** | Build, editor seams, bridge/RPC, release, SSH channel evidence, and a generated-key Android-shaped fixed-RPC proof exist. The shipped M0 UI is a Java/View connection-status screen, not the future Compose shell. M0-R5/R6 still need the real JVM callback/socket-to-`chooshd` proof; M0-R7/R15 remain blocked on terminal provenance and device implementation. |
| M1 — Remote workspace | **Not started** | Depends on M0 SSH and terminal gates. Profile/known-host and root-confined SFTP read seams exist, but a concrete live transport, safe atomic writes, Zellij, Markdown, reconnect, and lifecycle acceptance remain future work. |
| M2 — Agents and notifications | **Not started** | Adapter/event and Android notification slices await the M1 remote foundation. |
| M3 — Pinning and services | **Not started** | Depends on M2 snapshots and verified SSH `direct-tcpip`. |
| M4 — Editing and Git diff | **Not started** | Pure bounded diff groundwork exists, but the real daemon/SFTP/Git adapter and complete acceptance gate do not. |
| M5 — Markdown review | **Not started** | Depends on the M1/M4 workspace and document identity surfaces. |
| M6 — Public 1.0 release | **Not started** | 0.0.1 proves a release lane only; reproducibility, device/accessibility, migration, hardened host updates, and all prior gates remain. |

## Next increments

Each increment must have a deterministic headless command, negative-path test,
bounded resources, and a commit/push after verification.

1. **Finish M0-R5/M0-R6.** Restore AWS authentication, retrieve/terminate the bounded metal
   runner if it is still present, then run `scripts/run-android-instrumentation.sh` on an accelerated
   x86_64 emulator or arm64 device and exercise the real Android/native connector, credential use,
   bounded cancellation, the fixed `git.status` daemon method, and negotiated
   stdio-to-real-`chooshd` private-socket RPC in one harness—not only generated-key fixtures.
2. **Deploy and update `chooshd` without SSH session ownership.** Specify and implement a
   fixed-command, SSH/SFTP-only release installer: Android verifies GitHub-release metadata and
   artifacts before upload; the host stages immutable version directories, verifies the digest,
   atomically activates a per-user service-manager unit, health-checks through the private
   socket, and rolls back to the previous version on failure. Support `systemd --user` and
   `launchd` explicitly; unsupported hosts fail closed rather than using shell backgrounding.
   The service and Zellij processes must survive Android transport loss.
3. **Then widen host/SFTP operations.** Compose the injected host process adapter and add
   only server-proven root-confined/atomic SFTP operations after the first vertical thread
   is reachable.
4. **Terminal go/no-go.** Preserve the recorded Zelland grant, close native and
   font provenance gates, then implement the wgpu renderer behind the existing
   interface (or approve the specified CPU cell-grid fallback by ADR) and run
   headless conformance plus isolated device instrumentation.
6. **M1 profile/workspace slice.** Persist profiles securely, pin known hosts, list
   explicit registrations, create/adopt Zellij sessions, and exercise reconnect and
   separated destructive lifecycle actions in one black-box scenario.
7. **M0 exit review and release candidate.** Run every M0 headless gate from a clean checkout,
   then the required emulator/device gates for the native terminal and connection lifecycle.
   Publish a release candidate only after the evidence manifest, SBOM, notices, updater
   verification, and Obtainium discovery checks are current.

## Completion rule

Do not promote a milestone based on implementation volume or a successful APK build.
Promote it only when every requirement and both its mandatory headless/device scenarios
in the acceptance matrix pass twice deterministically, with all prerequisite
milestones complete.
