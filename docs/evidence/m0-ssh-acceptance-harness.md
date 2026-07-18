# M0 dependency-free SSH acceptance harness contract

Status: design-ready; transport adapter and harness runner are not implemented.

This contract turns Spike C into deterministic inputs and assertions without selecting an SSH
dependency. The same runner drives an in-memory scripted transport first and the selected local SSH
adapter later. Adapter-specific tests cannot replace these shared assertions.

## Boundary

The runner sees one `VerifiedConnection` composition root:

```text
verify(endpoint, presented_key) -> trusted | consent_required | mismatch
authenticate(credential_ref) -> connection_generation
open_pty(generation, target, limits) -> channel_id
open_exec(generation, argv, limits) -> channel_id
open_sftp(generation, root, limits) -> channel_id
open_direct_tcpip(generation, 127.0.0.1, registered_port, limits) -> channel_id
advance(logical_millis)
inject(channel_or_transport, fault)
drain_events() -> canonical events
```

No operation accepts shell text, an absolute SFTP path, a non-loopback direct-tcpip host, ambient
time, or a host-key bypass. The scripted transport uses no sockets, threads, sleeps, cryptography,
or external services. The real-adapter lane supplies the same events from a disposable local server.

## Deterministic scheduler

Fixtures contain bounded `Limits` and `Step { at_ms, ordinal, action }` records. The runner sorts by
`(at_ms, ordinal)` and rejects duplicate ordinals, decreasing time, zero limits, unknown channel
IDs, oversized packets, and arithmetic overflow before executing. Output events contain only
logical time, generation, channel ID/kind, stable outcome, and byte counters.

The scripted oracle uses deficit round robin with an explicit byte quantum per ready channel. One
step transfers at most one quantum per channel before revisiting another ready channel. PTY/control
traffic has reserved bounded capacity but bulk data is never silently discarded. The eventual
adapter need not use this internal algorithm; its observable progress must meet the same limits.

## Required fixtures

### Trust and authentication

- First presentation requires consent and opens no channels. Only consent naming the exact pending
  fingerprint permits authentication; stale/different consent fails.
- Stored-key match reaches ready. A changed key fails before authentication and opens no channels.
- Rejected trust and invalid credentials produce stable non-retryable codes. Evidence contains no
  key bytes, credential references, payloads, remote paths, or capabilities.

### Multiplex and fairness

One generation concurrently opens PTY, exec/RPC, SFTP, and direct-tcpip. The fixture starts an
8 MiB throttled SFTP transfer, injects 100 ms logical round-trip delay, submits 100 distinct 32-byte
PTY echoes, performs typed RPC, and exchanges bounded HTTP, WebSocket, and SSE records.

It asserts every channel progresses before bulk completion; RPC/service records are byte-exact;
every PTY sample is matched; sorted p99 PTY latency is at most 750 logical milliseconds; queue
high-water marks remain bounded; only exact `127.0.0.1:<registered-port>` can open direct-tcpip; and
SFTP rejects absolute, empty, dot, parent, and separator-ambiguous paths.

### Fault isolation and reconnect

Separate fixtures disconnect each channel kind during work. Channel-local failure closes only that
channel. Transport failure closes all channels and uses injected bounded reconnect decisions. After
exact-key reverification/authentication, generation advances; completion/input/resize/stream/close
events carrying the old generation fail `stale_generation` without writes or state changes.

Additional faults cover queue saturation, short writes, packet EOF, deadline, auth failure,
changed-key reconnect, retry exhaustion, and generation overflow. Each fixture ends with zero live
channels and queued bytes. Disconnect never implicitly stops a remote PTY/process.

## Canonical evidence

The future shared command is reserved as:

```sh
cargo test -p choosh-ssh-harness --test scripted_acceptance
```

That package does not exist yet. Failure output is bounded to fixture name, seed, step ordinal,
stable code, and canonical counters. CI retains canonical JSON with fixture version/seed, limits,
logical duration, progress counters, queue high-water marks, PTY sample count/p50/p99/max, and final
resource counts. It excludes payloads and machine paths. Re-running a seed must reproduce identical
JSON.

## Real-adapter promotion

After dependency approval, a loopback-only local-server lane reuses these fixtures and records real
monotonic durations. It must prove exact known-host verification, one TCP connection for all four
channel kinds, process-scoped server/filesystem/service fixtures, and no Android-side non-loopback
listener. A candidate is not selected until it supports independent cancellation and passes the
fairness gate. This contract does not approve any dependency or its licence/security obligations.
