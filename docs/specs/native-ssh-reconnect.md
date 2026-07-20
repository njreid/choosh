# Native SSH reconnect

Status: Draft

## Purpose

This contract governs recovery after an Android network change or a lost SSH
transport. It preserves the SSH-only trust boundary: reconnection is a new SSH
admission, not an attempt to reuse a broken TCP connection.

## Required ordering

For every initial connection and reconnect attempt, the native composition
root MUST:

1. allocate a new logical connection generation;
2. open a bounded transport supplied by its Android outer adapter;
3. verify the presented host key against the exact persisted fingerprint;
4. request a Keystore-backed signature only after that verification succeeds;
5. authenticate with the injected public key;
6. expose bounded RPC/channel capabilities only after authentication succeeds.

It MUST NOT invoke the credential signer for an unknown, changed, rejected, or
unverified host key. It MUST NOT expose a session merely because a native plan
was created.

## Loss and retry

A transport loss MUST invalidate every channel belonging to its generation.
The native connector MUST use the deterministic bounded backoff policy from
`choosh-core`; Android supplies logical time and jitter samples through an
injected boundary. Implementations MUST NOT use wall-clock sleeps in domain or
headless tests.

After a retry deadline, the next attempt repeats the required ordering above.
Authentication and host-key failures are terminal for that attempt and MUST
NOT be silently downgraded to a transport retry. A user disconnect cancels all
pending retry work.

## Recovery

After a newly authenticated generation is ready, the connector MUST obtain
component checkpoints and use `RecoveryCoordinator` actions:

- replay when the retained event window covers the local revision;
- request a snapshot when it does not;
- reject remote revision regression;
- reject results for stale generations.

`chooshd` and Zellij remain host-owned and persistent across Android network
loss. Android reattaches; it does not attempt to recreate remote processes as
a side effect of reconnecting.

## Acceptance evidence

A vertical harness must prove all of the following without a public host
listener:

1. a changed generated host key prevents the signer callback;
2. a lost stream invalidates old RPC/channel generation handles;
3. a retry waits for its injected logical deadline;
4. a restored connection signs only after exact host-key admission;
5. replay and snapshot actions match the supplied checkpoints.
