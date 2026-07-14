# ADR 0004: Client-side Git diff

Status: Accepted

## Decision

The host supplies machine-readable Git status plus bounded `HEAD`, index, and worktree versions. Android Rust computes textual hunks and Compose renders a unified diff. Android does not clone the repository or embed JGit/libgit2.

V1 Git functionality is read-only: no stage, discard, commit, branch, pull, or push.

## Consequences

- Diff UX is native and can deep-link precisely into Sora.
- Blob transfer and local computation require strict size/time limits.
- Binary, submodule, and oversized changes fall back to metadata.

