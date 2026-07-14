# M6: Security hardening and public release

## Outcome
A signed, accessible, upgradeable release ships with supported host binaries and verified trust boundaries.

## Requirements
- **M6-R1:** Close or explicitly accept every release-blocking threat-model finding.
- **M6-R2:** Fuzz framing, paths, events, Git parsing/diffs, and gateway HTTP/WebSockets.
- **M6-R3:** Test key mismatch, reconnect storms, spool loss, stale snapshots, daemon upgrades, and partial writes.
- **M6-R4:** Verify no public listener, sensitive log, WebView bridge/token leak, unbounded queue, or command interpolation.
- **M6-R5:** Sign host binaries with authenticated update, rollback, and compatibility checks.
- **M6-R6:** Publish signed APKs, monotonic versions, checksums, SBOM, notices, and reproducible instructions.
- **M6-R7:** Verify Obtainium install and two upgrades preserve hosts/workspaces/pins/annotations/recovery state.
- **M6-R8:** Pass screen-reader, touch-target, contrast, keyboard, reduced-motion, low-memory, rotation, background, phone, and tablet tests.
- **M6-R9:** Publish setup, adapter, service, security, backup, recovery, and troubleshooting docs.
- **M6-R10:** Re-resolve the stable Android/Kotlin baseline, meet the current target-SDK release requirement, and close or explicitly time-box every dependency compatibility exception.

## Release gate
All targets pass; a clean device reaches an agent and web preview from docs alone; no critical/high security issue remains; artifacts/checksums/SBOM/licences are public.

## Post-1.0
Git mutations, LSP, tmux, HTTPS service policy, pin sync, richer search, tablet split panes, and more agents require separate proposals.
