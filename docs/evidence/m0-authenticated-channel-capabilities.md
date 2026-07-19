# Authenticated SFTP and RPC channel capability contract

Status: design-ready; the selected SSH adapter does not expose this contract yet.

## Boundary

This contract is the sole future constructor for Android-side RPC and SFTP channel
capabilities. It exists only after the exact host key is verified, the selected user
credential authenticates, and the stdio RPC handshake reaches `Ready` for one connection
generation. It is not an SSH client escape hatch and does not accept a hostname, username,
credential reference, private-key material, arbitrary command, absolute path, or ambient
timeout.

```text
VerifiedConnection::ready(generation, negotiated_limits)
  -> AuthenticatedChannelCapabilities

AuthenticatedChannelCapabilities::open_rpc(RpcLimits)
  -> RpcChannel(generation, channel_id)

AuthenticatedChannelCapabilities::open_sftp(RegisteredRootCapability, SftpLimits)
  -> RootConfinedSftpChannel(generation, channel_id)
```

The concrete composition root retains the verified connection and credential provider. The
capability objects expose only their generation, opaque channel ID, negotiated limits, typed
operations, and cancellation. They are invalidated together when the transport generation
closes. Construction is constructor-injected and scripted fakes implement the same boundary;
there is no service locator or global current connection.

## RPC capability

`open_rpc` opens exactly one SSH session/exec channel with the fixed argument vector:

```text
choosh-host rpc --stdio
```

It sends and receives the framed protocol in [host protocol](../specs/host-protocol.md). The
caller cannot select an executable, shell string, working directory, environment, or stdin
outside the framed stream. The first outbound frame is `hello`; no request/event frame is
accepted before `welcome` and negotiated limits. The channel exposes request IDs and bounded
frame bytes, not a raw writable process pipe.

An RPC channel has a cancellation token owned by its caller. Cancellation closes only that
channel and rejects late callbacks as `stale_channel`; it does not terminate the SSH transport,
SFTP work, Zellij processes, or `chooshd`. Framing violation, EOF, daemon incompatibility, and
channel close return distinct typed outcomes and never trigger an implicit reconnect command.

## SFTP capability

`open_sftp` opens one SFTP subsystem channel on the existing authenticated connection. Its
`RegisteredRootCapability` is issued only for an already registered canonical host root. It is
opaque to Android UI and WebViews and is invalid after its connection generation ends.

Every operation takes a bounded root-relative component sequence and a negotiated byte limit.
It rejects empty paths where an object is required, `.`/`..`, slash or NUL in a component,
absolute paths, excessive depth, oversized components, and byte limits above the negotiated
maximum before an SFTP request is emitted. The adapter resolves and rechecks the registered root
on the host side; an SFTP channel cannot create an alternate root or browse an unregistered host
path. Read and write APIs carry explicit cancellation and exact byte-count limits; writes use the
M4 identity/atomic-replacement protocol rather than an unguarded overwrite.

## Limits, ownership, and outcomes

The authenticated connection allocates channel IDs and enforces all negotiated limits before
calling the SSH library:

| Limit | Enforcement |
| --- | --- |
| total live channels | reject open with `channel_limit` |
| per-channel queued bytes | apply backpressure, then fail `channel_queue_limit` at the declared bound |
| aggregate queued bytes | apply cross-channel backpressure, then fail `aggregate_queue_limit` |
| RPC frame/request bytes | reject before allocation with `rpc_frame_limit` / `rpc_request_limit` |
| SFTP operation/read/write bytes | reject before the request with `sftp_limit` |
| logical deadline | injected scheduler returns `deadline_exceeded`; no wall-clock sleeps |

Only the transport owns socket and SSH session lifetime. The RPC capability owns its stdio
channel; the SFTP capability owns its subsystem channel; the caller owns cancellation. A failed
channel is local unless the SSH transport itself reports failure. On transport failure all live
capabilities close, queued bytes are released, and a subsequent verified/authenticated
connection receives a strictly newer generation. Calls carrying an old generation fail
`stale_generation` before any write or SSH operation.

Evidence and typed errors contain stable codes, generation/channel identifiers, byte counters,
and limit names only. They MUST NOT contain credential references, key material, capability
tokens, remote paths, request payloads, or arbitrary host diagnostics.

## Headless acceptance cases

The future `choosh-ssh-harness` scripted and real-adapter lanes extend the shared [SSH
acceptance harness](m0-ssh-acceptance-harness.md) with these deterministic cases:

1. **Authentication gate.** Unknown/rejected/changed host keys and invalid credentials open zero
   RPC and SFTP channels. Exact trusted key plus valid credential is the only setup that may
   construct `AuthenticatedChannelCapabilities`.
2. **Fixed RPC exec.** The fake transport records exactly `choosh-host`, `rpc`, and `--stdio` as
   separately encoded arguments. Attempts to pass a shell string, alternate executable, or raw
   stream endpoint are unrepresentable at the capability interface.
3. **Root confinement.** Absolute, empty, dot, parent, separator-ambiguous, NUL-containing,
   overlong, excessive-depth, expired-root, and symlink-escape fixtures emit their typed outcome
   and record zero SFTP operation bytes.
4. **Pre-allocation bounds.** Oversized RPC frames, requests, SFTP reads, writes, queues, and
   channel counts fail before the fake adapter allocates a channel or records a request. Exact
   boundary values succeed without an aggregate-limit bypass.
5. **Cancellation isolation.** Cancel a stalled SFTP read and a stalled RPC request separately.
   Each closes only its own channel, leaves the other making progress, releases its queue bytes,
   and preserves the remote Zellij process fixture.
6. **Fair concurrent progress.** With the specified 8 MiB throttled SFTP transfer and 100 ms
   logical RTT, issue 100 32-byte RPC requests plus the existing PTY samples. Assert all channel
   kinds progress before bulk completion, bounded queues, byte-exact RPC responses, and the
   documented p99 terminal budget without sleeps.
7. **Generation invalidation.** Drop the transport during RPC and SFTP work, reconnect with exact
   host verification and authentication, then inject old-generation completions, cancellation,
   and write attempts. Each is `stale_generation`; no old request reaches the fake host.
8. **Sanitized evidence.** Seed credentials, root capability canaries, absolute-path canaries,
   payload canaries, and host-error canaries. Canonical fixture output contains none while still
   reporting channel counts, byte counters, stable outcomes, and logical time.

The real-adapter promotion additionally proves that the RPC and SFTP capabilities share one TCP
connection with the PTY and allowed loopback-forwarding channels. It uses a disposable local
server and generated fixture identity; it does not require Android UI, an emulator, a network
service, a developer home directory, or human input.
