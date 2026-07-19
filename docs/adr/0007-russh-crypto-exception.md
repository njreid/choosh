# ADR 0007: Time-bounded Russh cryptography exception

## Status

Accepted on 2026-07-19; expires on 2026-10-17 unless renewed by a new ADR.

## Context

Choosh needs one in-process Android SSH connection with exact host-key verification
before authentication, concurrent session channels, SFTP, and loopback-only
`direct-tcpip`. The dependency admission audit identifies `russh = 0.62.2` plus
`russh-sftp = 2.3.0` as the best fit, but its resolved graph includes required
release-candidate cryptography crates. Normal policy rejects prerelease production
dependencies.

The project owner explicitly approved Russh for the route to a usable remote
workspace on 2026-07-19. This exception is intentionally narrow: it authorizes only
the exact graph admitted by the SSH dependency fixture and does not waive any host-key,
resource-limit, Android ABI, licence, or transport-harness requirement.

## Decision

Create the isolated `choosh-ssh` outer adapter crate and pin:

- `russh = 0.62.2`, with default features disabled and the `ring` backend selected;
- `russh-sftp = 2.3.0`; and
- only the exact transitive prerelease packages recorded in the resulting lockfile.

The adapter MUST disable unused legacy algorithms and RSA unless a later, separately
reviewed interoperability requirement demands them. It MUST keep the domain crates
free of Russh types and compose the adapter only at an Android/JNI or binary boundary.

Before this graph may authenticate a non-fixture connection, the admission command
must lock and inventory the graph, build both Android ABIs, and pass the generated-key
host-key-before-auth, multiplexing, cancellation, and fairness harness.

## Consequences

- The exception unblocks implementation and test admission; it does not make M0-R5
  complete or permit a release claim before the harness passes.
- The dependency graph must be reviewed on every Russh, feature, crypto-backend, or
  lockfile change.
- By 2026-10-17, Choosh must remove prerelease cryptography from the production graph,
  replace Russh, or record a fresh decision with evidence of why removal is not yet
  possible. The release lane fails after expiry.
