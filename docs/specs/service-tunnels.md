# Registered development services

Status: Draft

## Explicit launch

Choosh does not infer services or ports from process tables or terminal output. A service is launched explicitly:

```sh
choosh service run \
  --workspace app \
  --name web \
  --port 3000 \
  --protocol http \
  -- npm run dev
```

The CLI sends a request to `chooshd`. The daemon validates the workspace, name, protocol, port, and command; creates a dedicated managed Zellij tab; starts the command in the workspace root; and records a typed service item.

V1 supports `http` with WebSockets and SSE. HTTPS-to-host support is deferred because development certificates and hostname verification require an explicit policy.

## Lifecycle

Statuses: `starting`, `running`, `stopped`, `failed`, `unknown`.

Pinning and running are independent:

- pin: create client gateway/tunnel and display WebView;
- unpin: close WebView/tunnel only;
- stop: explicitly terminate the managed service process/tab;
- disconnect: close client tunnel; leave the service running.

## Tunnel

For a pinned running service, Android creates an ephemeral loopback HTTP gateway. Each upstream connection opens SSH `direct-tcpip` forwarding to `127.0.0.1:<declared-port>` on the host.

The gateway:

- binds only to Android loopback;
- requires a random per-pin token held in an HttpOnly WebView cookie;
- strips the gateway cookie before forwarding;
- forwards HTTP bodies with backpressure;
- supports WebSocket upgrade and long-lived SSE;
- caps headers, connection count, idle time, and buffered bytes;
- closes immediately on unpin, host disconnect, or item removal.

A random loopback port alone is not authentication. Requests without the gateway cookie receive `403` and never open an SSH channel.

## WebView isolation

Service content uses a WebView separate from Choosh's Markdown/Datastar WebView. It has no JavaScript bridge, file/content access, Choosh cookies, internal bearer token, or direct access to SFTP/RPC. External navigation requires an explicit user action and opens outside the trusted internal surface.

## Readiness

The daemon MAY probe the declared host loopback port after launch. Readiness affects status only; it MUST NOT discover or substitute another port. A service can be pinned while starting and shows a retrying interstitial until ready or failed.

