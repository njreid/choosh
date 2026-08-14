# Registered development services

Status: Draft

See [DESIGN.md](../../DESIGN.md) §7 (`WebService` item) and §2.3 (the relay
as a blind broker) for the surrounding architecture.

## Explicit launch

Choosh does not infer services or ports from process tables or terminal
output. A service is launched explicitly:

```sh
choosh-hostd service run \
  --workspace app \
  --name web \
  --port 3000 \
  --protocol http \
  -- npm run dev
```

`choosh-hostd` validates the workspace, name, protocol, port, and command;
creates a dedicated managed Zellij tab; starts the command in the workspace
root; and records a typed service item: `{item_id, name, tab_target, port,
protocol, status}`, matching [host-rpc.md](host-rpc.md)'s item-registration
discipline — no filesystem discovery or port scanning anywhere in this
path.

V1 supports `http` with WebSockets and SSE. HTTPS-to-host support is
deferred because development certificates and hostname verification
require an explicit policy.

## Lifecycle

Statuses: `starting`, `running`, `stopped`, `failed`, `unknown`.

Pinning and running are independent:

- pin: request a relay tunnel and display the `WebService` WebView;
- unpin: close the WebView/tunnel only;
- stop: explicitly terminate the managed service process/tab;
- disconnect: close the client tunnel; leave the service running.

## Tunnel

For a pinned, running service, Android opens an ephemeral loopback HTTP
gateway and requests `choosh-relayd` broker a tunnel from it to
`127.0.0.1:<declared-port>` on the devhost — a `relayd`-brokered byte
stream, not an SSH `direct-tcpip` channel, but functionally identical:
`relayd` MUST NOT parse or transform the bytes it carries (DESIGN.md §2.3),
so HTTP semantics, WebSocket upgrade, and SSE all pass through unmodified
exactly as they would over a direct connection.

The gateway:

- binds only to Android loopback;
- requires a random per-pin token held in an HttpOnly WebView cookie;
- strips the gateway cookie before forwarding;
- forwards HTTP bodies with backpressure;
- supports WebSocket upgrade and long-lived SSE;
- caps headers, connection count, idle time, and buffered bytes;
- closes immediately on unpin, tunnel loss, or item removal.

A random loopback port alone is not authentication. Requests without the
gateway cookie receive `403` and never cause a tunnel-open request to
`relayd`.

## WebView isolation

Service content uses a WebView separate from Choosh's Markdown/Datastar
WebView. It has no JavaScript bridge, file/content access, Choosh cookies,
internal bearer token, or direct access to the relay connection or RPC.
External navigation requires an explicit user action and opens outside the
trusted internal surface.

## Readiness

`choosh-hostd` MAY probe the declared host loopback port after launch.
Readiness affects status only; it MUST NOT discover or substitute another
port. A service can be pinned while starting and shows a retrying
interstitial until ready or failed.

## Zellij web-client break-glass path

The same tunnel mechanism — a `relayd`-brokered stream to a devhost
loopback port, rendered in the `WebService` WebView — doubles as the
phone-only break-glass path when a workspace's native terminal path is
unreachable another way: `choosh-hostd` can point the tunnel at Zellij's
own web-client port instead of a registered dev server, with no new
transport code. This is the phone-only equivalent of the laptop-side
break-glass path (a plain `ssh <devhost>` through the enrolled SSH bridge),
covered in [ssh-bridge-and-zed.md](ssh-bridge-and-zed.md).
