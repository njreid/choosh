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
- [ ] A real Android SSH transport is selected and admitted under dependency and
  licence policy.  Current evidence deliberately leaves this blocked; see
  [SSH transport choice](docs/evidence/m0-ssh-transport-choice.md).
- [ ] An SSH interoperability harness proves exact host-key verification before
  authentication and concurrent PTY, exec, SFTP, and loopback-only `direct-tcpip`
  on one connection.
- [ ] The native terminal has a redistributable source/licence decision and an
  Android implementation/device conformance result.  Do not copy Zelland-derived
  source while [terminal provenance](docs/licenses/terminal-provenance.md) is blocked.

`v0.0.1` is an early signed distribution slice, not an assertion that M0 or a
public-1.0 milestone has passed.

## Milestone ledger

| Milestone | State | Evidence / remaining gate |
|---|---|---|
| M0 — Foundation | **In progress** | Build, editor, bridge/RPC, release and several headless domain spikes exist. M0-R5/R6 remain blocked on a real SSH transport and multiplex harness; M0-R7/R15 remain blocked on terminal provenance and device implementation. Device matrix and complete cross-target evidence remain required. |
| M1 — Remote workspace | **Not started** | Depends on M0 SSH and terminal gates. Profile, credentials, Zellij, root-confined SFTP, Markdown, reconnect, and lifecycle acceptance remain future work. |
| M2 — Agents and notifications | **Not started** | Adapter/event and Android notification slices await the M1 remote foundation. |
| M3 — Pinning and services | **Not started** | Depends on M2 snapshots and verified SSH `direct-tcpip`. |
| M4 — Editing and Git diff | **Not started** | Pure bounded diff groundwork exists, but the real daemon/SFTP/Git adapter and complete acceptance gate do not. |
| M5 — Markdown review | **Not started** | Depends on the M1/M4 workspace and document identity surfaces. |
| M6 — Public 1.0 release | **Not started** | 0.0.1 proves a release lane only; reproducibility, device/accessibility, migration, hardened host updates, and all prior gates remain. |

## Next increments

Each increment must have a deterministic headless command, negative-path test,
bounded resources, and a commit/push after verification.

1. **SSH credential-import boundary.** Model a user-approved Android document import
   as an opaque credential reference with public-key metadata.  The platform adapter
   must never silently read Termux private storage, expose key bytes to Rust snapshots,
   logs, or WebViews, or request broad storage access.  This is preparatory work only;
   it does not make SSH login functional.
2. **SSH transport admission spike.** Resolve a production-acceptable transport or
   record a time-bounded dependency exception.  Lock the graph, inventory licences,
   compile Android arm64-v8a/x86_64, and implement the generated-key local harness.
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
