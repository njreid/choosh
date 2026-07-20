# ADR 0004: Client-side Git diff

Status: Accepted

## Decision

The host supplies machine-readable Git status plus bounded `HEAD`, index, and worktree
versions. Android Rust computes textual hunks; a future native UI renders a unified diff.
Android does not clone the repository or embed JGit/libgit2.

The current M0 implementation is a bounded Myers shortest-edit-script algorithm
(`bounded-myers-v1`), not `imara-diff` or histogram. It retains bounded frontiers for
deterministic backtracking rather than allocating an old-lines by new-lines matrix, and
returns metadata rather than partial output on work or memory-budget exhaustion. Choosh
does not claim byte-for-byte Git presentation parity.

V1 Git functionality is read-only: no stage, discard, commit, branch, pull, or push.

## Consequences

- Diff UX is native and can deep-link precisely into Sora.
- Blob transfer and local computation require strict size/time limits.
- Binary, submodule, and oversized changes fall back to metadata.
