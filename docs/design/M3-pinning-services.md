# M3 detailed design: explorer, pinning, and services

Status: Draft

This design refines [M3](../milestones/M3-pinning-services.md) and depends on the [navigation](../specs/android-navigation.md), [terminal](../specs/terminal-experience.md), [service tunnel](../specs/service-tunnels.md), [workspace item](../specs/workspace-items.md), and [host protocol](../specs/host-protocol.md) specifications.

## Outcome and boundary

Explorer is always page zero. Agents, registered services, changed files, and project paths resolve to stable descriptors in an ordered, Android-owned pin set. Pinning controls presentation and client resources only; it never starts, stops, or terminates a remote process.

Services are created only by an explicit `choosh service run` request and listen on host loopback. Android previews them through SSH `direct-tcpip` and an authenticated Android-loopback gateway. There is no host TCP listener, port/process inference, arbitrary browser, JavaScript bridge, or shared trusted WebView state.

## Models and ownership

A pin descriptor is a versioned local record:

```text
PinV1 { host_id, workspace_id, kind, target_id, options, ordinal }
kind = agent | service | markdown | source | git_diff
```

`target_id` is an item UUID for agents/services and a canonical root-relative identity for files. Diff options include comparison mode. The compound identity excludes display names and Zellij target IDs. Ordinals are unique, dense, and transactional per workspace. Unknown future versions remain stored but are not rendered.

Android owns pin order, focused page, per-page presentation state, and gateway lifetime. `chooshd` owns workspace/item metadata and service lifecycle. Zellij owns service and agent processes. Missing targets remain unavailable placeholders with their original ordinal; the client never substitutes an item with the same name. Explicit unpin deletes the descriptor. Workspace termination may offer a separate confirmed cleanup.

Explorer projection order is fixed: active agents, registered services, changed files, project tree. Rows use canonical IDs, not list positions. A row toggle transaction inserts at the tail if absent or removes the exact descriptor if present. Page interaction and back navigation cannot call the toggle operation.

## Restore and reconciliation

On rotation, the retained state holder preserves descriptors and focus. After process death or reconnect the client:

1. loads PinV1 records without resolving them;
2. opens a workspace snapshot and subscribes as specified by M2;
3. resolves each descriptor by exact stable identity;
4. marks missing or unsupported targets unavailable in place;
5. restores the prior focused identity, or explorer if it is unavailable.

Reconnect does not start stopped services, attach to similarly named tabs, reorder pins, or delete placeholders. File identities that now violate root confinement become unavailable and are never opened.

## Terminal rebinding

One retained native Rust renderer binds to at most one logical terminal target. Every bind increments a 64-bit client generation and executes this ordered transition:

```text
quiesce input -> detach old stream -> clear composition/modifiers/selection/title
-> clear frame to neutral -> attach new target -> apply snapshot -> enable input
```

Frames, clipboard results, title changes, input, IME edits, and pointer events carry the generation and target ID. Rust drops mismatches. Remote PTYs continue in Zellij while detached. Failure to attach leaves a neutral unavailable surface and disabled input; it never restores the old target under the new page identity.

Gesture arbitration is a pure state machine driven by pointer traces. Edge-origin or an explicit horizontal threshold may claim page navigation before terminal/Sora/WebView content claims the gesture. Once claimed, ownership cannot change until all pointers lift or cancellation occurs. Terminal mouse-reporting mode, selection, two-finger scroll, Sora selection, and WebView scroll traces have deterministic precedence tables tested independently of Compose rendering.

## Service registration and lifecycle

The CLI grammar is:

```sh
choosh service run --workspace <workspace> --name <name> \
  --protocol http --port <1..65535> -- <command> [args...]
```

All flags and a non-empty command are required. The CLI sends an argv array, never a reconstructed shell string. The daemon validates the exact registered workspace, a normalized unique display name, supported protocol, declared port, command/argument count and byte bounds, then atomically reserves an item ID and creates a dedicated managed Zellij tab rooted at the canonical workspace root. It does not inspect output or process tables to infer readiness or another port.

Service state transitions are:

```text
starting -> running | failed | stopped | unknown
running  -> failed | stopped | unknown
unknown  -> running | failed | stopped
failed   -> starting | stopped
stopped  -> starting
```

An optional readiness probe connects only to `127.0.0.1:<declared-port>` with a bounded interval, attempt count, and timeout. Success changes `starting` to `running`; exhaustion changes it to `failed` with a stable diagnostic code. Pin/unpin does not affect this machine. Stop is a separate confirmed daemon command scoped to the exact item and Zellij target.

## Gateway and tunnel

Pinning a service creates a gateway instance with a fresh 256-bit random token, an ephemeral Android loopback listener, and no SSH channel yet. The listener must bind only loopback addresses. The isolated service WebView receives the token as a Secure, HttpOnly, SameSite=Strict cookie through native cookie APIs before navigation. The token is never placed in a URL, JavaScript, logs, saved state, or host request.

For every request the gateway validates method syntax, request-target form, header count/bytes, and an exact constant-time cookie match before opening any SSH channel. Failure returns `403` locally. It strips the gateway cookie and hop-by-hop headers before forwarding. An authenticated accepted connection opens `direct-tcpip` only to host `127.0.0.1` and the immutable registered port for that item.

HTTP bodies stream with backpressure. WebSocket upgrade becomes bounded bidirectional byte streams after validating the handshake. SSE is ordinary streaming HTTP and is not subject to a short response timeout. Negotiated limits cover concurrent connections, headers, body buffering, per-direction queued bytes, idle timeout, and total gateway instances; exhaustion rejects or closes locally with a stable reason. Redirects remain within the gateway origin; external navigation requires explicit user action and opens outside Choosh.

Unpin, disconnect, item removal, workspace close, or token rotation first stops acceptance, then cancels all associated SSH channels, clears the cookie, and destroys the service WebView. None sends `service.stop`. Re-pin creates a new origin and token.

The service WebView uses a data/cache/cookie boundary distinct from internal Markdown, with no JavaScript interface, file/content access, internal bearer token, SFTP/RPC capability, mixed-content exception, or persisted gateway credential. Service content is untrusted even though its transport is SSH-protected.

## Failure behavior

| Failure | Required behavior |
| --- | --- |
| Duplicate service name or invalid port/argv | Reject before creating an item or Zellij tab |
| Zellij creation/start failure | Record failed item or roll back reservation deterministically; never report running |
| Readiness timeout | Show failed/retry state; never scan or substitute ports |
| Missing pin target | Keep unavailable placeholder and ordinal |
| Renderer attach failure | Neutral cleared frame, disabled input, generation remains advanced |
| Missing/bad gateway cookie | Local `403`; zero SSH channel attempts |
| Header/buffer/connection limit | Local bounded rejection/closure; gateway remains usable for other connections |
| SSH disconnect | Close gateway channels/WebView; leave Zellij process and pin descriptor intact |
| WebView crash | Destroy that gateway and token; remote service remains running |

## Headless verification

Tests use an in-memory pin store, fixed workspace snapshots, a fake renderer/PTY transport, synthetic pointer traces, a scripted Zellij façade, deterministic readiness clock, fake SSH channel factory, loopback HTTP client, WebSocket echo peer, and SSE fixture server. No emulator, WebView pixels, external network, real shell command, wall-clock sleep, or human gesture is required for the milestone gate.

Security fixtures include absent/duplicated/malformed cookies, token canaries, oversized and ambiguous headers, upgrade attempts, slow bodies, disconnects, redirects, and concurrent-limit exhaustion. The SSH factory records every requested destination so tests can prove unauthenticated traffic and substituted ports never cross the boundary.

The minimum commands are:

```sh
cargo test -p chooshd service_registry
cargo test -p choosh-host service_cli
cargo test -p choosh-core gateway
./gradlew :app:testDebugUnitTest --tests 'ai.choosh.navigation.*'
./gradlew :app:testDebugUnitTest --tests 'ai.choosh.terminal.BindingStateMachineTest'
```

Until those packages exist, CI must provide one equivalently named aggregate target and document the mapping.

## Acceptance criteria

- Given the same shuffled snapshot and stored PinV1 fixture, 100 restore runs produce byte-identical ordered projections, including unavailable placeholders.
- Rotation, process-death, and disconnect simulations preserve pin identity/order; no simulation starts or stops a remote item.
- A randomized stale-event test sends frames and input from old generations during 1,000 alternating binds; none reaches or appears on the new target, and every bind begins with a neutral frame.
- Golden pointer traces produce one stable owner for terminal mouse mode, terminal selection/scroll, Sora, WebView, and edge swipe, with no ownership transfer mid-gesture.
- CLI table tests reject every missing flag, port outside `1..65535`, empty argv, duplicate name, and oversized field without invoking Zellij; valid argv reaches the scripted tab without shell reconstruction.
- Unpin and disconnect close all fake gateway channels but leave the scripted service process running; only confirmed stop targets it.
- HTTP request/response bodies, WebSocket frames, and SSE events pass through bounded fake SSH streams with backpressure and deterministic cancellation.
- Every unauthenticated or malformed-cookie request receives local `403`, and the fake SSH factory records zero channel opens.
- Every authenticated channel destination is exactly `127.0.0.1:<registered-port>` even under hostile Host headers, redirects, and request targets.
- Cookie/token canaries never appear in forwarded headers, URLs, logs, saved state, or the internal Markdown cookie fixture.
- A readiness probe contacts only the declared loopback port and reaches deterministic running, failed, and reconnect states under the fake clock.
