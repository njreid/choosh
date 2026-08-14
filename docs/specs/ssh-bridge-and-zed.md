# SSH bridge and Zed remote editing

Status: Draft

## Scope

`choosh-hostd` exposes a loopback-bound SSH server so that ordinary SSH
clients — a plain `ssh` invocation, or Zed's built-in remote-development
transport — can reach a devhost through `choosh-relayd` with no
Choosh-specific client. This document specifies that server, the laptop-side
`choosh-hostd proxy` mode that makes `~/.ssh/config` "just work" against it,
and the presence signal Zed attachment produces for the Android app. See
[`../../DESIGN.md`](../../DESIGN.md) §9 for the narrative version.

## Loopback SSH server

`choosh-hostd serve` MUST bind its SSH server to a loopback address only. It
MUST NOT accept a network connection from any interface other than
loopback.

The server MUST NOT perform its own key-based client authentication
challenge (no host-side `authorized_keys` check, no per-connection
handshake to verify a client key). A connection is admitted solely because
it arrived as a `relayd`-brokered tunnel on `hostd`'s existing outbound
control connection to `relayd` (the same multiplexed connection RPC and
agent events use — see [`relay-protocol.md`](relay-protocol.md)). `hostd`
has no other listening surface an attacker could target directly: the
loopback bind means the only path in is through that one already-verified
outbound connection, so there is nothing to spoof.

Each opened SSH-bridge tunnel MUST carry the Identity that `relayd`
authenticated as its requester (a laptop-proxy or, for the break-glass
shell path, a phone). `hostd` MUST treat that Identity as the
authenticated party for the resulting SSH session and MUST NOT proceed if
the tunnel-open control frame is missing or malformed — see
[`relay-protocol.md`](relay-protocol.md) for the exact frame shape and
`relayd`'s own authentication of the requesting Identity.

## Session handling

Once a session is established, `hostd`'s SSH server behaves like an
ordinary login/exec server scoped to that host:

- A plain interactive shell or an explicit exec (e.g.
  `zellij attach <workspace>`) MUST work exactly as it would over a normal
  SSH connection — no Choosh-specific negotiation is required from the
  client.
- An exec request matching Zed's remote-server invocation pattern (argv
  naming `zed-remote-server`, or an equivalent recognizable invocation Zed
  issues) MUST be detected and handled specially: `hostd` compares the
  version Zed's request declares against its `mise`-managed
  `zed-remote-server` install and installs/updates it before exec'ing, per
  [`toolchain-provisioning.md`](toolchain-provisioning.md). This document
  does not redefine that mechanism.
- Command construction for any exec path MUST use fixed argv vectors; no
  shell interpolation of client-supplied text.

## Laptop proxy mode

`choosh-hostd proxy` is a client-only mode with no daemon, no Zellij
control, and no workspace registry — it exists purely to bridge a laptop's
standard SSH tooling through `relayd`.

### `choosh-hostd proxy connect <host-id>`

This is the literal `ProxyCommand` target. Given the host alias/id SSH
supplies via its `ProxyCommand` expansion (`%h`, and `%p` if present), it
MUST:

1. Authenticate to `relayd` using the laptop's stored device credential
   (issued during `proxy enroll`; credential issuance is specified in
   [`auth-and-enrollment.md`](auth-and-enrollment.md), not here).
2. Request a tunnel to that devhost's SSH-bridge endpoint.
3. Pipe raw bytes bidirectionally between its own stdio and the opened
   tunnel, with no protocol translation, buffering beyond what's needed for
   the underlying transport, or inspection of the SSH bytes it carries.

It MUST exit non-zero, with no partial byte-piping, if authentication or
tunnel setup fails, so the wrapping `ssh`/Zed client reports a clean
connection failure rather than hanging.

### `choosh-hostd proxy sync`

Keeps `~/.ssh/known_hosts` and `~/.ssh/config` current against `relayd`'s
fleet list without ever prompting the user. Algorithm:

1. Query `relayd` for the current devhost fleet: each entry's stable alias
   and its relay-attested SSH host public key, captured once at that
   host's enrollment (see [`auth-and-enrollment.md`](auth-and-enrollment.md))
   — never learned via a manual TOFU fingerprint prompt.
2. For `~/.ssh/known_hosts`: write one line per devhost keyed by its stable
   alias. Existing Choosh-managed lines (identified by alias) MUST be
   updated in place, not duplicated. Lines for devhosts no longer present
   in the fleet list MUST be removed.
3. For `~/.ssh/config`: write one `Host <alias>` block per devhost with
   `ProxyCommand choosh-hostd proxy connect %h`. Blocks MUST be written
   inside markers Choosh owns (e.g. a `# BEGIN choosh` / `# END choosh`
   region) so `sync` never touches config the user wrote by hand outside
   that region, and so the whole region can be regenerated idempotently.
4. Retired devhosts' `Host` blocks MUST be removed on the same pass.

This procedure MUST be idempotent: running it twice with no fleet change
MUST produce no diff to either file.

`proxy sync` MUST run once at the end of `proxy enroll`, and thereafter on
a periodic schedule — every 15 minutes via a user-level timer (`systemd
--user` timer on Linux, a `launchd` `StartInterval` agent on macOS) — so a
newly enrolled devhost becomes reachable from an already-enrolled laptop
without the user re-running `proxy enroll`.

## Editor presence

When a Zed session attaches to a workspace through this bridge, `hostd`
MUST emit an `editor_attached` event on the workspace's event stream:

```json
{"type": "editor_attached", "workspace_id": "<id>", "editor": "zed"}
```

and a corresponding `editor_detached` event when that SSH session closes.
The event carries no file paths, command text, or session content — it
exists solely to drive the Android app's read-only `EditorPresence` item
(named in [`../../DESIGN.md`](../../DESIGN.md) §7; this document defines
only the event that feeds it).

## Concurrency

Zed, an agent, and Sora writing to the same workspace concurrently is
expected and safe: jj's working-copy-is-a-commit model means every write
becomes part of the next `@` snapshot with no lock to contend for. See
[`jj-integration.md`](jj-integration.md) for the underlying mechanism.
