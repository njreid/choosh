# Host protocol

Status: Draft

## Purpose

The host protocol connects Android's Rust engine to `chooshd` without opening a host network port.

```text
Android Rust → SSH exec → choosh-host rpc → 0600 Unix socket → chooshd
```

The normal RPC channel uses `choosh-host rpc --stdio`. A deployment
composition root MAY instead invoke `chooshd rpc --stdio --state-dir
<absolute-state-dir> --socket <absolute-socket>` directly. Both paths are
explicitly injected configuration: neither helper discovers a socket through
`HOME`, the current directory, environment variables, or a default path. The
socket MUST be the immediate child of the supplied state directory, and the
relay verifies that the directory is private and real and that the endpoint is
a mode-`0600` Unix socket before connecting. Android does not send either path
on the SSH channel.
On Linux, the daemon also verifies `SO_PEERCRED` after accept and before reading
protocol bytes; the peer effective UID MUST equal the daemon effective UID.
Platforms without an equivalent adapter fail closed rather than silently skipping
peer-credential verification.

## Transport

The client executes `choosh-host rpc --stdio`. Standard input and output carry frames. Standard error is diagnostic only and MUST NOT contain protocol data or secrets.

Each frame is:

```text
4-byte unsigned big-endian payload length
UTF-8 JSON payload
```

- Maximum control-frame payload: 1 MiB.
- A zero length is invalid.
- Invalid UTF-8, malformed JSON, oversized frames, or unknown envelope kinds terminate the bridge.
- The bridge MUST apply backpressure rather than buffering without bound.

## Handshake

The first client frame MUST be `hello`; the first daemon response MUST be `welcome` or `incompatible`.
The handshake permits exactly one reply: a coalesced second reply is rejected before
either reply establishes a session. Likewise, one request permits exactly one coalesced
terminal response; additional responses close the client session.

```json
{
  "kind": "hello",
  "protocol": { "major": 1, "minor": 0 },
  "client": { "name": "choosh-android", "version": "0.1.0" },
  "capabilities": ["events", "git-blobs", "services"]
}
```

`welcome` includes the selected version, daemon version, host identity, supported capabilities, and limits. Major versions MUST match. The selected minor version MUST be no greater than either peer's advertised minor version.

## Envelopes

After negotiation, every frame conforms to [the envelope schema](../../protocol/v1/envelope.schema.json).

- `request`: client-generated UUID, method, and params.
- `response`: matching UUID plus exactly one of `result` or `error`.
- `event`: daemon-generated workspace sequence and payload.

Requests MAY complete out of order. Events are ordered only within a workspace. Clients MUST deduplicate events by `(workspace_id, sequence)`.

## Initial methods

```text
host.describe
workspace.list
workspace.register
workspace.open
workspace.terminate
item.list
agent.start
agent.focus
service.start
service.stop
git.status
git.blob.prepare
events.subscribe
events.ack
```

Destructive methods require an explicit `confirmation` object tied to a short-lived daemon challenge.

### `git.status`

`git.status` accepts exactly one parameter, an already registered opaque workspace identity:

```json
{"workspace_id":"00000000-0000-0000-0000-000000000000"}
```

The client MUST NOT provide a host path. The result contains a bounded snapshot and entries
whose `new_path_b64` and optional `old_path_b64` fields use unpadded URL-safe base64 of the
original Git path bytes. This preserves valid non-UTF-8 Git paths without making an implicit
display-decoding policy. Unknown workspaces return `not_found`; malformed parameters return
`invalid_request`; exceeded status bounds return `limit_exceeded`.

## Binary streams

Large file versions are not base64-encoded into control frames. `git.blob.prepare` returns a single-use, short-lived capability and exact byte limit. Android opens a second SSH exec channel:

```text
choosh-host stream --capability <opaque-token>
```

That channel emits raw bytes and then exits. Capabilities are bound to the authenticated local user, workspace, object identity, and expiry; they MUST NOT appear in logs.

## Errors

Errors have stable machine codes and non-sensitive messages. Initial codes:

```text
invalid_request
not_found
already_exists
conflict
permission_denied
stale_revision
limit_exceeded
unsupported
host_unavailable
internal
```

Unknown error codes are treated as `internal` while preserving the display message.
