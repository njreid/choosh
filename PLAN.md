# Choosh delivery plan

Status date: 2026-07-19

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
  and root-confined SFTP request boundaries have deterministic local evidence. The
  host's fixed dispatcher parser is also fail-closed until a direct-exec allowlist is
  composed.
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
| M0 — Foundation | **In progress** | Build, editor, bridge/RPC, release and SSH admission/channel boundary evidence exist. M0-R5/R6 still need Android composition plus one multiplexed interoperability harness; M0-R7/R15 remain blocked on terminal provenance and device implementation. Device matrix and complete cross-target evidence remain required. |
| M1 — Remote workspace | **Not started** | Depends on M0 SSH and terminal gates. Root-confined SFTP request scaffolding exists, but profile, concrete transport, Zellij, Markdown, reconnect, and lifecycle acceptance remain future work. |
| M2 — Agents and notifications | **Not started** | Adapter/event and Android notification slices await the M1 remote foundation. |
| M3 — Pinning and services | **Not started** | Depends on M2 snapshots and verified SSH `direct-tcpip`. |
| M4 — Editing and Git diff | **Not started** | Pure bounded diff groundwork exists, but the real daemon/SFTP/Git adapter and complete acceptance gate do not. |
| M5 — Markdown review | **Not started** | Depends on the M1/M4 workspace and document identity surfaces. |
| M6 — Public 1.0 release | **Not started** | 0.0.1 proves a release lane only; reproducibility, device/accessibility, migration, hardened host updates, and all prior gates remain. |

## Next increments

Each increment must have a deterministic headless command, negative-path test,
bounded resources, and a commit/push after verification.

1. **Android SSH composition.** Bind the already user-selected, opaque credential and
   known-host records into the admitted Russh adapter through explicit DI. It must not
   expose private-key material or add broad filesystem access.
2. **Concrete SFTP and host direct-exec adapters.** Bind the root-confined request
   boundary to the selected SFTP subsystem and bind the host decoder only to an
   explicit allowlist. Preserve bounded reads/writes and no-shell execution.
3. **M0-R5/M0-R6 vertical proof.** Compose known-host persistence, credential use,
   multiplexed channels, bounded cancellation, and negotiated stdio-to-private-socket
   RPC.  Make the harness the release claim, not unit fakes.
4. **Terminal go/no-go.** Obtain terminal source permission/licence or choose a
   permitted replacement; then implement the renderer behind the existing interface
   and run headless conformance plus isolated device instrumentation.
5. **M1 profile/workspace slice.** Persist profiles securely, pin known hosts, list
   explicit registrations, create/adopt Zellij sessions, and exercise reconnect and
   separated destructive lifecycle actions in one black-box scenario.

## Completion rule

Do not promote a milestone based on implementation volume or a successful APK build.
Promote it only when every requirement and both its mandatory headless/device scenarios
in the acceptance matrix pass twice deterministically, with all prerequisite
milestones complete.
