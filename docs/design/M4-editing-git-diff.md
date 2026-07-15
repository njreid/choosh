# M4 detailed design: source editing and Git review

Status: Proposed

This design refines [M4](../milestones/M4-editing-git-diff.md), the
[client-side Git diff specification](../specs/git-diff.md), and
[ADR 0004](../adr/0004-client-side-git-diff.md). Normative terms describe the
M4 implementation and its headless acceptance harness.

## Outcome and boundary

M4 provides revisioned text buffers, conflict-safe SFTP saves, immutable Git
status/blob inputs, bounded client-computed diffs, and exact navigation between
diffs and documents. Sora and Compose are projections of Rust-owned state. The
same commands and events used by those projections MUST be operable by a
headless harness.

M4 does not provide Git mutations, binary editing, LSP, completion, a local
checkout, or repository-configured diff behavior. All host paths and Git output
are untrusted. Host communication remains inside host-key-verified SSH as
required by [ADR 0001](../adr/0001-system-boundary.md).

## Components and authority

| Component | Authority | Must not own |
| --- | --- | --- |
| Android Rust document actor | buffers, revisions, save state, undo transaction IDs | remote file identity |
| Sora adapter | visible text, selection, composition | durable content or revision |
| Android Rust Git actor | snapshot cache, decoded versions, hunk and line maps | repository state |
| `chooshd` Git service | immutable status descriptors and reproducible blob capabilities | rendered hunks |
| SFTP peer | current remote bytes and metadata | client buffer state |
| Headless harness | scripted commands, fault injection, event assertions | alternate production logic |

The Kotlin boundary exposes immutable snapshots and commands containing stable
IDs plus expected revisions. A view rebind never creates a second mutation
authority.

## Document model

```text
DocumentId       opaque client-local stable ID
DocumentPath     workspace ID + canonical root-relative byte path
RemoteIdentity   canonical path + kind + size + mtime precision/value + optional hash
TextFormat       encoding + BOM policy + line-ending policy
DocumentRevision monotonically increasing u64, never reused for one DocumentId
BufferSnapshot   document ID + revision + UTF-8 text + format + remote identity
```

Supported writable encodings in V1 are UTF-8 (with or without BOM), UTF-16LE,
and UTF-16BE. Other decodable text opens read-only with `unsupported_encoding`.
Mixed line endings open read-only with `mixed_line_endings`; M4 never silently
normalizes them. A file containing NUL bytes in the binary probe window is
`binary`. Limits come from the negotiated connection limits; the initial
defaults are 2 MiB encoded bytes and 100,000 decoded lines. Limit classification
uses checked arithmetic before full allocation.

`RemoteIdentity` is evidence, not a globally unique filesystem identity. When
metadata cannot distinguish the open version from the current version, the save
preflight MUST compare a bounded cryptographic digest. Symlinks are not followed
for writable documents. A regular file reached through a path component that
changes after canonicalization fails with `path_changed`.

### Edit command

```json
{
  "document_id": "doc-1",
  "base_revision": 7,
  "client_change_id": "change-18",
  "range_utf8": { "start": 12, "end": 15 },
  "replacement_utf8": "new"
}
```

Offsets address UTF-8 byte boundaries in the canonical Rust buffer. The Sora
adapter converts UTF-16 editor positions against the exact projected revision.
Rust rejects split code points, out-of-range offsets, duplicate change IDs with
different content, and stale revisions. A successful edit increments the
revision exactly once and emits an immutable replacement snapshot or delta. A
stale edit never applies partially; it emits `document.resync_required` with the
current revision. Undo and redo submit ordinary revisioned edit transactions, so
they obey the same stale and bounds checks.

## Document state machine

The externally visible state is one of:

```text
clean(remote_identity)
dirty(base_remote_identity)
saving(save_id, base_remote_identity)
offline_dirty(base_remote_identity)
conflicted(base_remote_identity, observed_remote_identity)
read_only(reason)
```

Transitions are deterministic:

| State + input | Result |
| --- | --- |
| `clean` + valid edit | `dirty` |
| `dirty` + disconnect | `offline_dirty`; retain the complete buffer locally |
| `offline_dirty` + more valid edits | `offline_dirty`; revisions continue |
| dirty state + save | `saving`; freeze a save snapshot while later edits may create a newer dirty revision |
| `saving` + verified atomic write | `clean` only if the saved revision is still current; otherwise `dirty` with the new remote identity |
| `saving` + identity mismatch | `conflicted`; write no project-path bytes |
| `saving` + transport failure before rename | dirty/offline dirty; outcome records whether a temp may remain |
| any writable state + unsupported remote kind/path change | `conflicted` or `read_only`; never overwrite |
| `conflicted` + reload | discard local buffer only after explicit confirmation, then open a new clean revision lineage |
| `conflicted` + save copy | write only to an explicitly selected, newly validated path |

M4 has no automatic offline edit replay or three-way merge. Reconnection performs
one preflight per dirty document. If identity still matches, the local buffer
remains dirty and may be saved. If it differs, that document alone becomes
conflicted. Other documents continue independently; no multi-file transaction or
reject-all behavior is implied. Conflict resolution exposes local bytes, the
open-time base bytes when retained within cache limits, and freshly read remote
bytes as read-only versions. "Overwrite anyway" is not an M4 action.

## Save protocol

For a save snapshot `(revision, base identity, encoded bytes)`:

1. Re-resolve the root-relative path without following a final symlink and verify
   every component remains beneath the registered workspace root.
2. Stat and, when metadata is inconclusive, hash the current regular file. Compare
   it with the base identity.
3. Encode the snapshot using its original `TextFormat`. Reject unrepresentable
   text before any remote write.
4. Create a sibling temporary file with an unpredictable name and exclusive-create
   semantics. Apply the original permission bits where supported.
5. Write, close, and verify the byte count. Repeat the target identity check after
   the upload and immediately before replacement; an observed change removes the
   temporary and reports conflict.
6. Rename over the target only when the SFTP server advertises a replacement
   operation with atomic semantics. Stat/hash the resulting target and publish the
   new identity.

If atomic replacement is unsupported, the writable save action is disabled with
`atomic_replace_unsupported`; M4 does not fall back to truncate-in-place. Temporary
cleanup is best effort and cannot turn an uncertain save into success. Cancellation
before rename is safe; cancellation after rename completes reconciliation before
reporting an outcome. Logs contain IDs, sizes, state transitions, and error codes,
never paths or document content by default.

SFTP has no portable compare-and-swap rename. The two identity checks close the
ordinary upload window but cannot eliminate a change in the final check-to-rename
interval. A server that cannot provide a conditional replacement capability reports
that limitation in diagnostics. Tests make this interval explicit; a future
host-mediated conditional replace would require a protocol specification update.

## Git snapshot and content protocol

`git.status` returns the immutable descriptor defined in
[the Git diff specification](../specs/git-diff.md). Every changed entry also has a
stable `entry_id`, raw root-relative old/new paths, modes, object identities when
available, staged/unstaged classification, and a fallback reason if text retrieval
is unsupported. Paths are length-delimited data and never shell-interpolated or
decoded from a human display format.

The host invokes Git with fixed arguments, `--no-ext-diff`, disabled text
conversion, no pager, a fixed locale, disabled prompting, and `-z` machine output.
V1 fidelity intentionally excludes custom diff drivers, word diff, move coloring,
whitespace-ignore modes, and automatic rename discovery beyond rename pairs
reported by the bounded status command. `.gitattributes` may inform text/binary
classification but MUST NOT execute a filter, textconv, or external driver.

Blob preparation addresses `(snapshot_id, entry_id, comparison, side)`, not an
arbitrary path. HEAD and index sides are pinned by object identity. A worktree side
is stat/hash checked before and after streaming. Any mismatch produces
`stale_snapshot` and discards received bytes. Capabilities are single-use, expire,
and are bound as described by [the host protocol](../specs/host-protocol.md).

The client cache key is the immutable side identity plus decoding policy. It has
explicit byte and entry ceilings, least-recently-used eviction, and no dirty source
buffers. Concurrent requests for one key coalesce. Status may be batched, but blob
streams remain independently bounded and cancellable so bulk transfer cannot block
terminal/control channels indefinitely.

## Diff model and navigation

The diff actor consumes two complete, validated versions and returns either:

```text
TextDiff(snapshot ID, entry ID, comparison, hunks, exact line map, algorithm ID)
MetadataOnly(snapshot ID, entry ID, reason, available metadata)
```

It never returns a truncated textual diff. Default ceilings are 2 MiB and 100,000
lines per side, 10,000 hunks, and a one-second computation budget. Cancellation,
timeout, arithmetic overflow, invalid encoding, binary input, submodule, unsafe
symlink, or any exceeded ceiling yields `MetadataOnly` with a stable reason.

Hunks carry zero-based half-open old/new line ranges and lines carry optional
one-based old/new display line numbers. Navigation is derived from this stored map:

- context or addition opens the new path at its new line;
- deletion opens the new path at the next surviving new line, otherwise the previous
  surviving line, otherwise line 1;
- deleted files open the pinned historical right-or-left version read-only;
- rename comparisons use the new path for current content and old path only for the
  historical side;
- navigation revalidates the current document identity; a changed worktree opens the
  newest document but reports `location_stale` rather than claiming exact mapping.

## Stable errors and failure behavior

M4 adds domain reasons beneath the host protocol's stable error envelope:

```text
binary, too_large, too_many_lines, unsupported_encoding, mixed_line_endings,
read_only, stale_revision, stale_snapshot, path_changed, remote_changed,
atomic_replace_unsupported, encode_failed, diff_budget_exceeded, cancelled
```

Unknown reasons display as unsupported/internal without retrying a write. Retrying a
read is safe; save retry requires a new identity preflight. No error response embeds
file content, a host absolute path, a capability, or a Git command line.

## Headless verification contract

The repository MUST provide a non-Android executable test driver that speaks
newline-delimited JSON commands to the production Rust actors. Commands include
`document.open`, `document.edit`, `document.save`, `connection.set`, `git.status`,
`git.diff`, and `diff.navigate`. Responses/events are canonical JSON with volatile
IDs and timestamps supplied by a seeded fixture clock/ID source.

The harness uses:

- an in-process fake SFTP filesystem supporting metadata precision, symlinks,
  short writes, disconnects, rename capability, and barriers before/after rename;
- disposable real Git repositories created from declarative fixtures, with fixed
  author identity, timestamps, locale, default branch, and file modes;
- golden JSON diff models, not screenshots;
- a fake monotonic clock for debounce, capability expiry, and diff budgets;
- fault schedules keyed to named protocol steps rather than wall-clock races.

Fixture names and content MUST be synthetic. Golden files normalize platform path
separators and do not contain host absolute paths.

## Acceptance criteria

M4 is complete only when one documented headless command runs all of these checks
without an emulator, network, Android UI, or human judgment:

1. UTF-8 BOM and UTF-16LE/BE fixtures survive edit/save byte-for-byte except for the
   intended edit; LF and CRLF are preserved.
2. Invalid UTF-16 positions, stale revisions, duplicated mismatched change IDs, NUL
   content, mixed endings, and oversized inputs fail with the expected stable reason
   and no buffer mutation.
3. Rotation/rebind simulation reconstructs the projection from the Rust snapshot;
   process-restart simulation restores a bounded dirty buffer and its base identity.
4. Remote mutation before preflight and at the pre-rename barrier produces conflict;
   the original remote bytes are never silently replaced.
5. Short write, disconnect, cancellation, unsupported atomic rename, and post-rename
   response loss produce deterministic state and leave either the old or complete new
   target, never a truncated target.
6. Multiple offline-dirty files reconcile independently; one changed remote file does
   not block saving an unchanged one, and no dirty file auto-saves on reconnect.
7. Git fixtures cover staged, unstaged, combined, untracked, deleted, renamed,
   conflicted, unborn HEAD, mode-only, binary, submodule, symlink, odd-byte path, and
   oversized entries without invoking hooks, filters, textconv, pager, or prompts.
8. Golden hunk models and navigation targets are stable for additions, deletions,
   empty files, no-final-newline markers, rename pairs, and boundary hunks.
9. Stale worktree streams and expired/reused capabilities fail closed and publish no
   diff. Cache eviction and request coalescing remain within configured byte limits.
10. Property tests generate Unicode edit sequences and line pairs; applying emitted
    hunks reconstructs the right side, every line map is monotonic/in-range, and no
    panic or allocation beyond negotiated bounds occurs.

The CI job records the seed on failure and reruns each fault fixture at least once to
prove deterministic output.

## Traceability

| Milestone requirement | Design sections |
| --- | --- |
| M4-R1–R6 | Document model, state machine, save protocol |
| M4-R7 | Git snapshot and content protocol |
| M4-R8–R9 | Diff model and navigation |
| M4-R10 | Stable errors, acceptance criteria |
