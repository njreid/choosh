# M5: Markdown review and annotations

## Outcome
Project Markdown is a persistent review surface with resilient annotations and agent-readable export.

## Requirements
- **M5-R1:** Render sanitized CommonMark, code, tables, tasks, and confined relative assets.
- **M5-R2:** Stream Maud/Datastar fragments without WebView RPC/SFTP/filesystem access.
- **M5-R3:** CRUD annotations anchored by document/revision/range/context fingerprint.
- **M5-R4:** Re-anchor after non-overlapping edits; mark ambiguous/orphaned anchors.
- **M5-R5:** Persist locally by host/workspace/document across reconnects.
- **M5-R6:** Explicitly export bounded Markdown or `.choosh/annotations.json`; never silently modify the repo.
- **M5-R7:** Range-stream assets with cache limits, cancellation, and content validation.
- **M5-R8:** Annotation navigation/state works with touch, keyboard, and accessibility services.

## Exit gate
Unrelated edits retain anchors; overlapping rewrites orphan safely; exports are agent-readable; traversal/unsafe HTML/oversized assets fail closed.

## Excluded
Real-time collaboration, automatic annotation commits, and proprietary formats.

