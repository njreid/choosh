# Milestone 3 — jj diff and change graph

Proves the host-computed-diff and change-graph story ([DESIGN.md](../../DESIGN.md)
§8) that replaces the old client-side Myers diff engine entirely.

## Scope

- RPC: `workspace.diff` (structured hunks, `jj-lib`-computed, default
  `@-` → `@`), `workspace.log` (change graph nodes/edges), `workspace.op.log`,
  `workspace.op.undo`, `workspace.op.restore`.
- Android `JjDiff` item: native unified diff rendering from structured
  hunks — no on-device diff computation.
- Android `JjChangeGraph` item: interactive DAG view, tap-to-inspect a
  change, one-tap `undo`/`op restore`.
- Changed-files section of the explorer wired to `workspace.status`.

## Exit criteria

- A diff between any two revisions (not just `@-`→`@`) renders correctly,
  including a rename and a binary file (shown as metadata, not a garbled
  diff).
- The change graph reflects concurrent edits from two `jj workspace`s
  against the same repo correctly, including a conflicted merge.
- `jj undo` from the phone reverses the most recent operation and the
  change graph updates to reflect it within one refresh cycle.
- No diff or log computation happens on the Android side — verified by the
  absence of a diff-computation dependency in the Android build, not by
  policy alone.
