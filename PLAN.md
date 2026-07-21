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
- [x] Native authenticated-plan tokens are type-separated from generic requests, and the
  host has an injected direct-process adapter that emits only the fixed daemon argv. Both
  remain fail-closed until their outer platform compositions provide verified transport.
- [x] Android profile/known-host metadata, authenticated-operation composition, an opaque
  JNI plan boundary, and a Keystore signer boundary have headless evidence. The signer
  refuses to construct a request before exact host admission and exposes no key material;
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
  it is deliberately not wired to the unavailable live SSH connector yet.
- [x] The Android transport composition root now joins opaque runtime capabilities to the
  pinned Russh admission path in the required order: exact host session, username, Keystore
  signer, injected stream, then Russh host-key callback before signing. Generated Ed25519
  acceptance proves a changed host key reaches no signer and an exact host key reaches the
  injected signer and authenticates; the custom-signer buffer contract is explicit. Concrete
  JNI runtime adapters and device evidence remain required before it can report connected.
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
| M0 — Foundation | **In progress** | Build, editor seams, bridge/RPC, release, SSH channel evidence, and a protocol multiplex harness exist. The shipped M0 UI is a Java/View connection-status screen, not the future Compose shell. M0-R5/R6 still need one composed Android-to-daemon proof; M0-R7/R15 remain blocked on terminal provenance and device implementation. |
| M1 — Remote workspace | **Not started** | Depends on M0 SSH and terminal gates. Profile/known-host and root-confined SFTP read seams exist, but a concrete live transport, safe atomic writes, Zellij, Markdown, reconnect, and lifecycle acceptance remain future work. |
| M2 — Agents and notifications | **Not started** | Adapter/event and Android notification slices await the M1 remote foundation. |
| M3 — Pinning and services | **Not started** | Depends on M2 snapshots and verified SSH `direct-tcpip`. |
| M4 — Editing and Git diff | **Not started** | Pure bounded diff groundwork exists, but the real daemon/SFTP/Git adapter and complete acceptance gate do not. |
| M5 — Markdown review | **Not started** | Depends on the M1/M4 workspace and document identity surfaces. |
| M6 — Public 1.0 release | **Not started** | 0.0.1 proves a release lane only; reproducibility, device/accessibility, migration, hardened host updates, and all prior gates remain. |

## Next increments

Each increment must have a deterministic headless command, negative-path test,
bounded resources, and a commit/push after verification.

1. **Join the first vertical composition.** Exercise the existing bounded `git.status`
   daemon/private-socket slice through the Android connector boundary; do not claim it is
   live until exact host-key admission and Keystore signing are present.
2. **Complete the native Android connector.** Turn the opaque JNI plan into a real
   exact-host-key-before-signing connection using a reviewed Keystore callback and a
   bounded stream adapter; add deterministic reconnect/recovery while continuing to expose
   no private key material.
3. **M0-R5/M0-R6 vertical proof.** Exercise the real Android/native connector,
   credential use, the fixed daemon method, bounded cancellation, and negotiated
   stdio-to-private-socket RPC in one harness—not only protocol fakes.
4. **Then widen host/SFTP operations.** Compose the injected host process adapter and add
   only server-proven root-confined/atomic SFTP operations after the first vertical thread
   is reachable.
5. **Terminal go/no-go.** Preserve the recorded Zelland grant, close native and
   font provenance gates, then implement the wgpu renderer behind the existing
   interface (or approve the specified CPU cell-grid fallback by ADR) and run
   headless conformance plus isolated device instrumentation.
6. **M1 profile/workspace slice.** Persist profiles securely, pin known hosts, list
   explicit registrations, create/adopt Zellij sessions, and exercise reconnect and
   separated destructive lifecycle actions in one black-box scenario.

## Completion rule

Do not promote a milestone based on implementation volume or a successful APK build.
Promote it only when every requirement and both its mandatory headless/device scenarios
in the acceptance matrix pass twice deterministically, with all prerequisite
milestones complete.
