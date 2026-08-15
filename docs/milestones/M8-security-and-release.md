# Milestone 8 — Security and release

Proves the system is safe to depend on and ship, not just architecturally
sound. `relayd` becoming a trust-bearing intermediary (DESIGN.md §11) is a
real posture change from the old "no server at all" boundary and needs its
own review — the loopback-only, root-confined, observational-hook
properties inherited from earlier milestones are necessary but not
sufficient.

## Scope

- **`relayd` threat model.** A dedicated review of the new intermediary:
  devhost/laptop identity impersonation, enrollment-token replay or theft,
  a compromised laptop-proxy or devhost device credential, tunnel
  cross-wiring (Identity A reaching a tunnel meant for Identity B), and
  `relayd` availability/DoS — each with a documented mitigation or an
  explicitly accepted risk. This is additive to, not a replacement for, the
  existing path/redaction/command-construction threat model. Result:
  [`docs/security/relayd-threat-model.md`](../security/relayd-threat-model.md).
- **`choosh-hostd` self-update.** `relayd`-pushed `UPDATE_BINARY` control
  frame; atomic download-verify-`chmod`-`rename()` swap; re-exec or
  service-manager restart; rollback if the new binary fails its socket
  health check.
- **`choosh-relayd` release readiness.** `just deploy <ec2-instance-name>`
  (established in M0) gated on the same checks as an Android release:
  passing test suite, no unreviewed dependency changes, a rollback path if
  the health check fails post-deploy.
- **Signed Obtainium release pipeline.** One signed, reproducible universal
  APK named `choosh-VERSION.apk` per release, plus SHA-256 checksums, a
  CycloneDX SBOM, and dependency-licence notices, published so Obtainium's
  GitHub Releases discovery finds it without special-casing.
- **Licence closure.** Sora Editor's LGPL-2.1+ packaging/relinking
  obligations inside an Apache-2.0 APK, and the Zelland-derived terminal
  renderer's provenance and licence grant — both resolved as release gates,
  not left as open research questions (see `docs/licenses/sora-packaging.md`,
  `docs/licenses/terminal-provenance.md`, `docs/licenses/zelland-grant.md`).
- **Device and accessibility testing.** Screen-reader/TalkBack pass over
  the Explorer and pinned-item navigation, hardware-keyboard and DeX/
  external-display behavior, tablet layout, and a low-memory device
  profile under sustained terminal output. Result:
  [`docs/accessibility-device-report.md`](../accessibility-device-report.md).

## Exit criteria

- A threat-model review names every `relayd`-specific abuse case —
  impersonating a devhost, replaying an enrollment token, a compromised
  laptop-proxy credential, cross-tunnel leakage, relay downtime — with a
  documented mitigation or an accepted risk for each; none are left as
  unstated assumptions. Written up in
  [docs/security/relayd-threat-model.md](../security/relayd-threat-model.md).
- A `choosh-hostd` self-update on a live devhost with an attached agent
  session completes with no dropped Zellij session and a clean rollback
  when the pushed binary is deliberately broken in a test.
- `just deploy` against a `relayd` build with a failing health check does
  not leave the fleet disconnected — it rolls back automatically.
- A release build produces byte-identical output across two independent
  build runs (reproducibility), passes SBOM/checksum verification, and
  Obtainium detects and offers the update from a clean install.
- Sora and Zelland-terminal licence notices are present, accurate, and
  reviewed as a release gate — not deferred past the release they gate.
- The accessibility and device-profile pass has a written result (pass,
  or a named gap with a tracked follow-up) rather than an implied "looks
  fine." Written up in
  [docs/accessibility-device-report.md](../accessibility-device-report.md).
