# Relay protocol

Status: Draft

## Purpose

`choosh-relayd` is the single rendezvous point between every Identity in the
system (phone, laptop-proxy, devhost). This spec defines the wire protocol
each Identity speaks to `relayd` — framing, control frames, and tunnel
frames — per [DESIGN.md](../../DESIGN.md) §5. It does not define
authentication (see [auth-and-enrollment.md](auth-and-enrollment.md)) or the
contents of any tunnel's payload (see the spec for whatever that tunnel
carries — [host-rpc.md](host-rpc.md), [agent-events.md](agent-events.md),
[ssh-bridge-and-zed.md](ssh-bridge-and-zed.md), etc.).

## Transport

Each Identity holds exactly one persistent connection to `relayd`: a
WebSocket over TLS. Machine Identities (devhost, laptop-proxy) MUST
reconnect on any close with exponential backoff and jitter, starting at 1s
and capped at 60s. The phone SHOULD reconnect the same way while its
process is alive, and otherwise relies on FCM (see
[notifications.md](notifications.md)) rather than trying to stay connected
indefinitely in the background.

A connection is authenticated before any frame other than the initial
credential presentation is accepted; see auth-and-enrollment.md. An
unauthenticated or failed-authentication connection MUST be closed by
`relayd` with no frames processed.

## Framing

Every frame, in both directions, is:

```text
4-byte unsigned big-endian payload length
payload
```

- Maximum control-frame payload: 1 MiB.
- Maximum tunnel-frame payload: 256 KiB. A larger logical payload MUST be
  split across multiple tunnel frames by the sender; `relayd` MUST NOT
  reassemble or inspect them to do so — reassembly, if needed, is the
  tunnel endpoints' concern, not `relayd`'s.
- A zero-length frame is invalid.
- On a malformed frame (invalid length prefix, non-UTF-8 JSON where JSON is
  expected, unknown control-frame `type`, or a frame exceeding its class's
  size cap), `relayd` MUST terminate the connection. There is no partial
  recovery — the Identity reconnects and re-authenticates.

## Frame classes

Every frame carries a one-byte class discriminant immediately after the
length prefix:

- `0x01` **Control frame** — typed JSON, addressed to `relayd` itself.
- `0x02` **Tunnel frame** — an opaque payload, prefixed with an 8-byte
  tunnel ID, addressed through `relayd` to another Identity. `relayd` MUST
  NOT parse, log, or otherwise interpret bytes after the tunnel ID.

## Control frames

All control-frame JSON bodies include a `request_id` (client-generated,
echoed in the response) except server-pushed frames (`update_binary`),
which include a `push_id` for idempotent handling on redelivery after
reconnect.

### `enroll`

Devhost/laptop-proxy only. Exchanges a one-shot enrollment token for a
long-lived device credential. See auth-and-enrollment.md for the full
exchange; this frame just carries `{ token, identity_class, public_key }`
and returns `{ device_id, certificate }` or a typed failure.

### `request-enrollment-token`

Phone/web only, on an already-authenticated connection. Request:
`{ identity_class: "devhost" | "laptop-proxy" }`. Response:
`{ token, expires_at }`. See auth-and-enrollment.md for token properties.

### `list-devhosts`

Phone only. Request: `{}`. Response: `{ devhosts: [DevHostPresence] }`
where each `DevHostPresence` is
`{ device_id, alias, platform, account_label, connection_state, last_seen }`.
`connection_state` is `"online"` or `"offline"`; there is no partial state.

### `open-tunnel`

Any authenticated Identity. Request:
`{ target_device_id, purpose }` where `purpose` is an opaque tag the two
tunnel endpoints agree on out of band (e.g. `"rpc"`, `"pty:<item_id>"`,
`"ssh"`, `"web-preview:<item_id>"`) — `relayd` does not validate `purpose`
beyond passing it to the target's tunnel-offer notification. Response:
`{ tunnel_id }` on success, or a typed failure if `target_device_id` is
offline or the requesting Identity lacks a capability scope permitting a
tunnel to that target (see auth-and-enrollment.md's capability scopes).
The target Identity receives an unsolicited `tunnel-offered` push frame
`{ tunnel_id, from_device_id, purpose }` and begins accepting `0x02` frames
for that `tunnel_id` immediately — there is no separate accept/reject
handshake at the relay layer; a target that doesn't want the tunnel simply
never sends data and lets it idle-timeout (below).

### `agent-event`

Devhost only. Carries one normalized event as defined in
[agent-events.md](agent-events.md). `relayd` forwards it to the owning
phone Identity if connected, and/or triggers FCM dispatch per
[notifications.md](notifications.md); it does not interpret the event body
beyond routing fields (`workspace_id`, `severity`).

### `register-fcm-token`

Phone only. Request: `{ fcm_token }`. Response: `{}`. Replaces any
previously registered token for that phone Identity — `relayd` holds at
most one FCM token per phone Identity.

### `update_binary` (server-pushed)

`relayd` → devhost only, triggered by an operator action (from the Android
app or `just deploy`-adjacent tooling, not from this spec's Identity
protocol). Push: `{ push_id, download_url, sha256, version }`. The devhost
does not reply over this channel; see
[host-deployment.md](host-deployment.md) for the update procedure itself.

## Presence

`relayd` tracks, per devhost Identity, exactly the fields in
`DevHostPresence` above, updated on connect/disconnect and never inferred
from tunnel activity. `alias`, `platform`, and `account_label` are set once
at enrollment (see auth-and-enrollment.md) and are otherwise immutable
through this protocol — changing them is an out-of-band administrative
action, not a control frame in this spec. `last_seen` updates on every
frame received from that Identity, control or tunnel.

## Tunnel lifecycle

1. **Open** — via `open-tunnel` as above. A tunnel ID is unique for the
   lifetime of the tunnel and MUST NOT be reused after close.
2. **Data** — `0x02` frames flow in both directions, routed purely by
   tunnel ID. `relayd` enforces per-tunnel backpressure (a slow reader on
   one side MUST NOT cause `relayd` to buffer unboundedly for the other;
   it applies WebSocket-level flow control and, if a tunnel's outbound
   queue exceeds a bounded threshold, closes the tunnel rather than
   growing memory without limit).
3. **Close** — either endpoint sends a zero-payload `0x02` frame for that
   tunnel ID as a close signal, or `relayd` closes it after 300s with no
   data frames in either direction (idle timeout). Both endpoints MUST
   treat a tunnel's closure, from any cause, as final — there is no resume.
4. **Reconnect discontinuity.** If a devhost's underlying WebSocket
   connection drops and it reconnects, every tunnel that terminated at
   that devhost is implicitly closed; `relayd` MUST NOT attempt to splice
   a new physical connection onto an old tunnel ID. Callers observe this
   as an ordinary tunnel close and re-open if they still need the stream
   (e.g. a PTY attachment re-opens and the terminal renderer redraws from
   the Zellij session's current state, per
   [terminal-experience.md](terminal-experience.md)).

## Errors and backpressure

- `open-tunnel` against an offline `target_device_id` fails immediately
  with a typed `target_offline` error; it does not queue or retry.
- A tunnel whose destination Identity goes offline mid-stream is closed by
  `relayd` (not left half-open); the surviving endpoint sees an ordinary
  close.
- Control-frame requests against an Identity's own connection that arrive
  faster than `relayd` can process (e.g. `list-devhosts` polling) are rate
  limited per-Identity; exceeding the limit closes the connection rather
  than silently dropping requests, so a client can detect and back off
  rather than getting inconsistent responses.
