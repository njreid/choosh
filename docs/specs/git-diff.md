# Client-side Git diff

Status: Draft

## Goals

- Display changed files without a local Android checkout.
- Compute textual hunks in Android Rust.
- Open a diff location in Sora using stable old/new line mapping.
- Never run repository-configured external diff or text-conversion commands.

## Status snapshot

`git.status` returns an immutable snapshot descriptor:

```text
snapshot_id
workspace_id
HEAD object ID or unborn state
index identity
worktree observation time
changed entries
limits
```

Each entry contains old/new root-relative paths, worktree and index status, mode/type information, and binary/submodule hints. Status is obtained with fixed Git arguments and machine-readable `-z` output.

## Comparisons

| Mode | Left | Right |
| --- | --- | --- |
| `working` | index | worktree |
| `staged` | HEAD | index |
| `combined` | HEAD | worktree |

Untracked files use empty content on the left. Deleted files use empty content on the right. Renames retain both paths. An unborn repository uses empty HEAD content.

## Version retrieval

`git.blob.prepare` accepts `snapshot_id`, path, side, and comparison mode. It returns metadata plus a short-lived stream capability. The daemon MUST reject stale snapshots when the requested identity can no longer be reproduced.

Worktree reads MUST be root-confined and checked against the snapshot metadata before and after streaming. HEAD and index content MUST be addressed by resolved object identity rather than mutable names.
The daemon reads every blob stream with the capability's byte bound: it retains no more
than that bound and probes one additional byte to classify an oversized source.

## Android computation

Android decodes text using the document encoding policy, normalizes only for comparison,
and preserves original line-ending metadata. Each emitted line records `none`, `lf`, or
`crlf`; a line-ending-only change is therefore a visible deletion/addition pair rather
than unchanged text. The current M0 implementation uses the
bounded quadratic `bounded-lcs-v1` reference algorithm. It is not a claim of Git,
histogram, Myers, or `imara-diff` fidelity. A production algorithm replacement MUST
preserve this bounded result contract and add deterministic performance/fidelity evidence.

Default V1 limits:

```text
maximum side size: 2 MiB
maximum lines per side: 100,000
maximum generated hunks: 10,000
computation budget: 1 second on a background dispatcher
```

Limits are negotiated in the host handshake and MAY be reduced by the client. Exceeding a limit produces a metadata-only page, never a partial misleading diff.

## View model

The native page contains file status, comparison selector, hunks, old/new line numbers, and line kind (`context`, `addition`, `deletion`). Hunk context defaults to three lines and can be expanded within limits.

Selecting a context/addition line opens the new path at its new line. Selecting a deletion opens the new path at the nearest surviving line; for a deleted file it opens a read-only historical buffer.

## Non-goals

V1 does not stage, unstage, discard, commit, branch, merge, pull, or push. It does not render textual diffs for binary files, submodules, symlinks with unsafe targets, or files above limits.
