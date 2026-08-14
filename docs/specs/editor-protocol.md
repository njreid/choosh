# Sora editor document protocol

Status: Draft

## Scope

This defines how the `SourceEditor` pinned item ([android-navigation.md](android-navigation.md))
persists edits against a Workspace's live working copy. It is unchanged in
spirit from the superseded Git-era Sora protocol; what changed is what's
underneath it: there is no SFTP channel and no staged/unstaged split,
because jj's working copy has no index (see [jj-integration.md](jj-integration.md)).

## Opening a document

Opening a file issues `workspace.file.read { workspace_id, path }` (no
`revision` — defaults to the live working copy `@`, see
[jj-integration.md](jj-integration.md)) and returns:

```text
{ document_id, revision, content, encoding, line_ending, read_only }
```

`revision` here is the content identity captured at open time (the file's
current on-disk state), not a `jj` change/commit id — it exists purely to
detect a conflicting concurrent write, the same role `base_revision` plays
below.

## Editing

Sora emits incremental `ContentChangeEvent`s. The Rust engine translates
each into a UTF-8 range edit carrying the `base_revision` it was composed
against, validates and applies it, and returns the new `revision`. An edit
whose `base_revision` no longer matches the document's current revision
(the file changed on disk since the last edit was applied or since open —
from an agent write, a `jj workspace` sync, or a Zed save) produces a
resync/conflict event rather than a silent overwrite.

## Save state

Saving is debounced but state is always explicit, one of: `clean`,
`dirty`, `saving`, `conflicted`, `offline`. There is no `staged` state —
jj has no index, so a save simply becomes the workspace's next `@`
snapshot.

## Persistence

A save issues `workspace.file.write { workspace_id, path, base_revision,
content_or_edits }` (defined in [jj-integration.md](jj-integration.md)).
`hostd` MUST reject a write whose `base_revision` doesn't match the file's
current on-disk revision rather than silently overwriting it — this is the
entire conflict model; there is no separate merge/rebase step to get
wrong, because there is nothing but the working copy to write to. Encoding
and line endings MUST round-trip byte-identical.

## Limits

V1 MUST reject binary files and open oversized text read-only, with
thresholds enforced host-side before content ever leaves `hostd`.

## Editor features

Sora provides text editing, undo/redo, search, and basic TextMate
highlighting in V1. LSP, completion, and tree-sitter are later features,
not prerequisites for the remote-control product.

## Concurrent writers

A file edited from Sora while an agent or a laptop Zed session
([ssh-bridge-and-zed.md](ssh-bridge-and-zed.md)) writes the same path is
the normal case, not an edge case — see [jj-integration.md](jj-integration.md)'s
concurrency note. Sora's `base_revision` check is what surfaces that as a
conflict state to the user instead of a lost write.
