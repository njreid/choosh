# ADR 0004: Client-side Git diff

Status: Accepted

## Decision

The host supplies machine-readable Git status plus bounded `HEAD`, index, and worktree
versions. Android Rust computes textual hunks; a future native UI renders a unified diff.
Android does not clone the repository or embed JGit/libgit2.

The current M0 implementation is a bounded quadratic LCS reference algorithm
(`bounded-lcs-v1`), not `imara-diff`, histogram, or Myers. It is deliberately constrained
for deterministic fixtures and returns metadata rather than partial output on budget
exhaustion. Selecting a production algorithm and its fidelity/performance evidence is
future work; Choosh does not claim byte-for-byte Git presentation parity.

V1 Git functionality is read-only: no stage, discard, commit, branch, pull, or push.

## Consequences

- Diff UX is native and can deep-link precisely into Sora.
- Blob transfer and local computation require strict size/time limits.
- Binary, submodule, and oversized changes fall back to metadata.
