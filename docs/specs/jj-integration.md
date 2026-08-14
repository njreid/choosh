# jj integration

Status: Draft

## Purpose

`choosh-hostd` embeds [`jj-lib`](https://docs.rs/jj-lib) directly rather
than shelling out to the `jj` CLI, for the same anti-string-parsing reason
the pre-relay design gave for avoiding parsed `git` CLI output: CLI text
formats are not a stable wire contract. This document defines the
resulting file-browsing, diff, and change-graph RPC surface referenced
from [host-rpc.md](host-rpc.md) and [DESIGN.md §8](../../DESIGN.md#8-deep-dive-browsing-a-jj-workspace-from-the-phone).

**Constraint carried over from [DESIGN.md §14](../../DESIGN.md#14-open-questions):**
`jj-lib` does not carry the same cross-release API-stability guarantee as,
say, `git2`. `choosh-hostd` MUST pin an exact `jj-lib` release and bump it
as a deliberate, reviewed increment — never track a moving branch or a
loose semver range.

## Revision resolution

Every method below takes an optional `revision` (or `from`/`to`) parameter
resolved as follows:

- **`@` (the default, and the only revision with live filesystem
  semantics):** the current working copy. jj's working copy has no index —
  `@` *is* a commit, snapshotted automatically as it changes — so there is
  exactly one "current state," never a staged/unstaged split. Reads of `@`
  go to the real files on disk, subject to the same root-confinement and
  range bounds as [host-rpc.md](host-rpc.md) defines for any other
  path-bearing RPC.
- **Any other revision** (a change id, commit id, or revset expression):
  resolved and read entirely through `jj-lib`'s content-addressed store.
  No filesystem access outside the store is involved, so historical reads
  cannot be affected by concurrent writes to `@`.

## RPC methods

Method names below are fixed and used verbatim elsewhere in this document
set (`host-rpc.md`, the milestone docs) — do not rename them independently
in an implementation.

### `workspace.tree.list { workspace_id, path_prefix, revision? }`

Returns one directory level's entries under `path_prefix` at `revision`
(default `@`): `{ name, entry_type: file | dir | conflicted, tracked }`.
Recursion is client-driven — see the page-size bound in
[host-rpc.md](host-rpc.md). `conflicted` is a `jj-lib`-native flag (see
"Conflicts" below), not something the client infers from markers.

### `workspace.file.read { workspace_id, path, revision?, range? }`

Returns bounded file content at `revision` (default `@`). `range` is a
byte offset/length pair for large-file streaming, per the bound in
[host-rpc.md](host-rpc.md).

### `workspace.file.write { workspace_id, path, base_revision, content_or_edits }`

The only mutating file RPC, backing Sora's revisioned edit protocol
(M4). `base_revision` MUST be the revision the client last read `path` at.
`hostd` MUST compare it against the file's current revision before
applying the write:

- If unchanged, the write applies to `@` (jj snapshots the new working-copy
  state automatically — there is no separate "commit" step the caller
  needs to perform).
- If `base_revision` is stale — the file changed on disk (another edit,
  an agent, a Zed save) since the client's last read — `hostd` MUST reject
  the write with a `revision_stale` error (per the error model in
  [host-rpc.md](host-rpc.md)) carrying the current revision and content,
  and MUST NOT silently overwrite the intervening change. This is the same
  posture the pre-relay Sora document protocol took toward stale edits;
  jj's lack of an index makes the check simpler (one current state to
  compare against) but the safety requirement is identical.

`content_or_edits` MAY be either a full replacement body or an incremental
edit list (UTF-8 range edits), mirroring the old Sora protocol's
`ContentChangeEvent` translation — the exact wire shape is an
implementation choice deferred to the editor-protocol spec, not fixed
here.

### `workspace.diff { workspace_id, from = "@-", to = "@" }`

Returns structured hunks computed by `jj-lib`'s own diff — never by
Android. Each hunk: `{ old_path, new_path, old_start, old_lines,
new_start, new_lines, segments: [{ kind: context | added | removed, text
}] }`, with `old_path`/`new_path` differing only when `jj-lib` has already
resolved a rename pairing. Binary and oversized files return `{ path,
status, byte_size }` metadata instead of hunks, matching the pre-relay
design's policy for binaries.

### `workspace.log { workspace_id, revset?, limit }`

Returns change-graph nodes and edges for the `JjChangeGraph` item:
`{ change_id, commit_id, description, author, parent_change_ids,
is_working_copy, bookmarks: [string] }`. Default `revset` mirrors `jj log`'s
default (the working copy and a bounded set of ancestors/related changes);
`limit` bounds the response size the same way `workspace.tree.list`'s page
size does.

### `workspace.op.log { workspace_id, limit }`

Returns the operation log: `{ op_id, description, start_time, end_time,
tags }`, most recent first, bounded by `limit`.

### `workspace.op.undo { workspace_id, op_id }` / `workspace.op.restore { workspace_id, op_id }`

`op.undo` reverses the effect of a single named operation; `op.restore`
resets the repo to the state as of a named operation. Both are safe to
expose without the staged, review-only rollout the pre-relay design
required for Git mutation, because jj's operation log is itself always
reversible — undoing an undo is just another operation in the same log.
Both MUST themselves produce a new operation-log entry (never a
destructive rewrite of history that isn't itself undoable).

## Conflicts

A conflicted tree entry is a structural property `jj-lib` exposes
directly, not text markers a client has to parse out of file content.
`workspace.tree.list` and `workspace.status` (defined in
[host-rpc.md](host-rpc.md)) both surface it as an explicit
`entry_type: conflicted` / per-path conflict flag. `workspace.file.read`
on a conflicted path returns the materialized conflict markers as file
content (so it's still viewable), but callers MUST treat the structural
flag, not the presence of marker text, as the source of truth for whether
a path is conflicted — a file can legitimately contain marker-like text
without being a jj conflict. A native resolution UI is out of scope for
this spec (see the milestone plan); V1 surfaces conflicts read-only.

## One workspace per agent

`workspace.create` (in [host-rpc.md](host-rpc.md)) accepts an optional
`parent_workspace_id`. When present, `hostd` runs the `jj-lib` equivalent
of `jj workspace add` against the parent's repo store rather than cloning
or initializing a new one: the new Workspace gets its own independent
working copy (its own `@`) sharing the parent's commit/operation store.
The resulting jj workspace's internal name MUST equal the Choosh
`workspace_name` — the same "Workspace = Zellij session = jj workspace,
all one name" identity [DESIGN.md §4](../../DESIGN.md#4-domain-model)
establishes for the plain case extends unchanged to the multi-workspace
case. This is the concrete mechanism behind assigning each concurrent
agent its own workspace: two agents on the same Project each get a
`workspace.create` call with the same `parent_workspace_id` (or chained —
B's parent can be A's workspace) and never contend for the same working
copy.
