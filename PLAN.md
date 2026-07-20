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
- [x] Android profile/known-host metadata, authenticated-operation composition, and an
  opaque JNI plan boundary have headless evidence. The JNI plan deliberately does not
  claim a live connection until native exact-host admission and Keystore signing exist.
- [x] A generated-key protocol harness proves fixed exec, SFTP subsystem, and loopback-only
  forwarding can coexist on one authenticated Russh transport. It is not yet the full
  Android-to-host vertical acceptance harness.
- [ ] A real Android SSH transport passes dependency admission and interoperability
  under the selected graph; see [SSH transport choice](docs/evidence/m0-ssh-transport-choice.md).
- [ ] An SSH interoperability harness proves exact host-key verification before
  authentication and concurrent PTY, exec, SFTP, and loopback-only `direct-tcpip`
  on one connection.
- [x] The project owner granted Choosh permission to use the exact Zelland source;
  its [recorded grant](docs/licenses/zelland-grant.md) clears that provenance item.
- [ ] The native terminal still needs a pinned native graph and Android implementation/device
  conformance result; see [terminal provenance](docs/licenses/terminal-provenance.md).

`v0.0.1` is an early signed distribution slice, not an assertion that M0 or a
public-1.0 milestone has passed.

## Milestone ledger

| Milestone | State | Evidence / remaining gate |
|---|---|---|
| M0 — Foundation | **In progress** | Build, editor, bridge/RPC, release, SSH channel evidence, and a protocol multiplex harness exist. M0-R5/R6 still need the native Android connector, actual process adapter, and a full vertical harness; M0-R7/R15 remain blocked on terminal provenance and device implementation. Required Android CI is being stabilized. |
| M1 — Remote workspace | **Not started** | Depends on M0 SSH and terminal gates. Profile/known-host and root-confined SFTP read seams exist, but a concrete live transport, safe atomic writes, Zellij, Markdown, reconnect, and lifecycle acceptance remain future work. |
| M2 — Agents and notifications | **Not started** | Adapter/event and Android notification slices await the M1 remote foundation. |
| M3 — Pinning and services | **Not started** | Depends on M2 snapshots and verified SSH `direct-tcpip`. |
| M4 — Editing and Git diff | **Not started** | Pure bounded diff groundwork exists, but the real daemon/SFTP/Git adapter and complete acceptance gate do not. |
| M5 — Markdown review | **Not started** | Depends on the M1/M4 workspace and document identity surfaces. |
| M6 — Public 1.0 release | **Not started** | 0.0.1 proves a release lane only; reproducibility, device/accessibility, migration, hardened host updates, and all prior gates remain. |

## Next increments

Each increment must have a deterministic headless command, negative-path test,
bounded resources, and a commit/push after verification.

1. **Complete the native Android connector.** Turn the opaque JNI plan into a real
   exact-host-key-before-signing connection using a reviewed Keystore callback and a
   bounded stream adapter; continue to expose no private key material.
2. **Complete concrete host/SFTP adapters.** Compose the host launcher with an injected
   process adapter, and add only server-proven root-confined/atomic SFTP operations.
3. **M0-R5/M0-R6 vertical proof.** Exercise the real Android/native connector,
   credential use, multiplexed channels, bounded cancellation, and negotiated
   stdio-to-private-socket RPC in one release harness—not only protocol fakes.
4. **Android candidate flow and CI.** Make the profile → connect → workspace-status
   flow installable/headless-testable, and restore a green required pre-device CI lane.
5. **Terminal go/no-go.** Obtain terminal source permission/licence or choose a
   permitted replacement; then implement the renderer behind the existing interface
   and run headless conformance plus isolated device instrumentation.
6. **M1 profile/workspace slice.** Persist profiles securely, pin known hosts, list
   explicit registrations, create/adopt Zellij sessions, and exercise reconnect and
   separated destructive lifecycle actions in one black-box scenario.

## Completion rule

Do not promote a milestone based on implementation volume or a successful APK build.
Promote it only when every requirement and both its mandatory headless/device scenarios
in the acceptance matrix pass twice deterministically, with all prerequisite
milestones complete.
