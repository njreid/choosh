# Milestone 5 — Web preview and Markdown

Proves tunneled dev-server preview and Markdown rendering work over the
relay the same way they did over SSH `direct-tcpip` in the old design —
same UX, different transport underneath.

## Scope

- Explicit service launch (`choosh-hostd service run --workspace <name>
  --name <svc> --port <port> -- <cmd>`) in a dedicated Zellij tab; no port
  or process inference.
- `WebService` item: `relayd`-brokered tunnel from an ephemeral Android
  loopback port to the devhost's declared loopback port, preserving HTTP,
  WebSockets, and SSE.
- `choosh-web` (Maud/Datastar) renders Markdown; remote images/large assets
  served as root-confined, range-capable loopback URLs.
- Zellij's own web client, tunneled the same way, as the phone-only
  break-glass path when the native terminal or a workspace's Zellij
  session itself is unreachable another way.

## Exit criteria

- A registered dev server with a WebSocket connection (e.g. Vite HMR)
  stays connected through backgrounding and reattachment.
- A Markdown file with a remote image renders correctly via ranged loopback
  fetches, without the WebView ever holding a devhost address or
  credential.
- Unpinning a `WebService` closes its tunnel but leaves the remote process
  running; stopping the service is a separate, explicit action.
