# M4: Source editing and Git review

## Outcome
Sora safely edits remote text and native client-computed Git diffs deep-link into it.

## Requirements
- **M4-R1:** Open with document ID/revision/encoding/line ending/remote identity/read-only state.
- **M4-R2:** Apply incremental revisioned edits; stale edits resync or conflict, never overwrite.
- **M4-R3:** Expose clean, dirty, saving, conflicted, offline, and read-only states.
- **M4-R4:** Atomically save over SFTP where supported after remote identity verification.
- **M4-R5:** Preserve encoding/line endings and bound/reject binary or large content.
- **M4-R6:** Provide undo/redo, search, go-to-line, basic highlighting, and changed-file open.
- **M4-R7:** Return immutable staged/unstaged Git status snapshots and bounded HEAD/index/worktree versions.
- **M4-R8:** Compute working/staged/combined diffs in Android Rust and render unified hunks with exact line maps.
- **M4-R9:** Diff selections open the correct current or historical Sora location.
- **M4-R10:** Define metadata/error fallbacks for binary, submodule, symlink, stale, and oversized changes.

## Exit gate
Encoding survives edit/save; concurrent host changes conflict safely; staged/untracked/deleted/renamed fixtures render correctly; pathological diffs hit limits without blocking UI.

## Excluded
Git mutations, LSP, completion, debugger, and binary editing.

