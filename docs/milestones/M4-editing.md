# Milestone 4 — Safe source editing

Proves in-app editing survives the realities of a mobile connection
(rotation, backgrounding, reconnect) without silently overwriting a
concurrently changed file — including files an agent or a laptop Zed
session touched while the phone was away.

## Scope

- Sora embedded in Compose; revisioned document protocol against
  `workspace.file.read`/a new `workspace.file.write` RPC.
- Dirty/saving/conflicted/offline state surfaced explicitly, not inferred.
- Because jj's working copy has no index, "conflicting write" means the
  file changed on disk since the document was opened — detected by
  comparing the captured open-time revision, not by any Git-style
  stage/merge machinery.
- Large-file and binary thresholds: read-only/rejected rather than a
  silent partial load.

## Exit criteria

- A save from the phone, an agent's write, and a Zed save (M6, if
  sequenced after) landing on the same file at different times each
  produce the correct next `@` snapshot with no silent data loss.
- Editing offline, then reconnecting to a file that changed remotely in
  the meantime, surfaces a conflict state the user must resolve — never a
  silent overwrite in either direction.
- Encoding and line endings are byte-identical on round-trip for a file
  with mixed line endings.
