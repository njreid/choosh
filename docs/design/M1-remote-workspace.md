# M1: Remote workspace vertical slice

Status: Detailed design

## Outcome

From a clean Android application state, a client connects through a verified SSH transport, lists and registers only explicit workspaces, creates or adopts the same-named Zellij session, starts one managed agent, detaches and resumes it after client process recreation, browses a root-confined file tree, and renders one Markdown document. SSH remains the only host network listener.

M1 is accepted primarily through a black-box scenario runner driving Rust and host interfaces without Compose. Android instrumentation verifies only platform surfaces that cannot be meaningfully exercised off-device. No acceptance step relies on visual inspection or a human typing into an agent.

## Scope and dependencies

M1 consumes the passing M0 build, bridge, SSH, RPC and terminal gates from [M0](M0-foundation.md). Its normative boundaries are:

- [SSH-only system boundary](../adr/0001-system-boundary.md);
- [host daemon and Zellij ownership](../adr/0002-host-daemon-and-zellij.md);
- [host protocol](../specs/host-protocol.md);
- [workspace and item model](../specs/workspace-items.md);
- [terminal behavior](../specs/terminal-experience.md).

Notifications, services, multiple pins, editing/saving, Git diff pages, annotations and public releases remain excluded. Machine-readable Git status may be exposed as diagnostic evidence but is not an M1 UI gate.

## Actors and durable ownership

| State | Authority | M1 persistence |
| --- | --- | --- |
| host profile ID, hostname, port, username, trusted host key reference, credential reference | Android Rust | encrypted local store |
| private key/passphrase material | Android Keystore-backed platform adapter | non-exportable or encrypted by a non-exportable key |
| connection/session generations | Android Rust | transient; reconstructed |
| workspace identity, canonical root, revision | `chooshd` | atomic daemon state store |
| item identity/type/Zellij target/status | `chooshd` | atomic daemon state store |
| PTY, agent process, scrollback, session/tab | Zellij | Zellij-managed |
| current pin/focus | Android Rust | local saved state |
| file-tree page/cache | Android Rust | bounded cache; non-authoritative |
| Markdown rendered document/assets | Android Rust loopback server | transient bounded cache |

Kotlin and Compose are projections. They do not own workspace, item, path, terminal, or document truth.

## Stable scenario-runner interface

M1 provides a headless executable or test entry point with structured input/output. The logical command surface is:

```text
profile add-test
connect
workspace list
workspace register
workspace open
agent start
terminal attach | terminal input | terminal await
tree list
markdown render | markdown fetch-asset
disconnect
engine checkpoint | engine recreate
workspace unregister
agent stop
workspace terminate
inspect
```

Production code implements every operation; the runner supplies fake Keystore consent and deterministic confirmation tokens. Output is newline-delimited JSON with stable event/result codes. Human-facing strings are not assertions. The runner never accepts a raw remote command: agent executables and arguments come from an allowlisted test adapter fixture.

## Connection and profile design

### Profile model

A profile contains a stable ID, display label, SSH endpoint, username, host-key algorithm and fingerprint, credential reference, last successful daemon compatibility, and timestamps. It MUST NOT contain private key bytes in serializable snapshots or logs.

Endpoint validation rejects invalid ports, control characters and ambiguous host syntax. SSH uses separately encoded connection parameters, not a shell command.

### Connection state machine

```text
Idle
 -> Resolving
 -> HostKeyCheck
 -> Authenticating
 -> StartingBridge
 -> Negotiating
 -> Ready(generation, capabilities, limits)
 -> Reconnecting(attempt, deadline)

HostKeyCheck -> TrustRequired(fingerprint) -> HostKeyCheck
HostKeyCheck -> Failed(host_key_mismatch)
Negotiating -> Failed(protocol_incompatible)
any active state -> Idle via explicit disconnect
```

Unknown keys require a consent result bound to profile ID, endpoint and exact SHA-256 fingerprint. A mismatch is never presented as first trust and never auto-replaced. Credential failures and compatibility failures remain distinguishable. Backoff uses an injected clock and deterministic jitter seed in tests.

### Host installation/upgrade

With explicit consent, Android uploads a platform-matched, checksum-verified `chooshd`/`choosh-host` bundle to a per-user staging path, atomically activates it, and health-checks it through SSH stdio. Installation requires no root access and creates no TCP listener.

The transaction is:

```text
inspect -> stage -> verify checksum/version -> activate -> start/reload -> hello/health
```

Failure before activation removes staging. Failure after activation attempts one rollback to the previously verified version and reports `install_failed` or `rollback_failed`. Commands use fixed executables and encoded arguments. M1 fixtures cover compatible/no-op, fresh install, upgrade, corrupt upload, unsupported platform, activation failure and rollback.

## Workspace registration and persistence

### Registration request

The request contains a user-chosen name, candidate absolute root and an adoption choice. `chooshd` canonicalizes the root itself, checks it is a directory accessible to the SSH user, validates the name against the supported Zellij subset, and enforces uniqueness by name and canonical root.

The daemon MUST NOT scan for projects. It returns an explicit list from its state store only. Host paths in responses are untrusted client input for display and MUST be bounded and escaped.

### Registration transaction

```text
ValidateRequest
 -> CanonicalizeRoot
 -> CheckRegistryCollision
 -> InspectSameNamedZellijSession
 -> CreateSession | AdoptionRequired | AdoptConfirmed
 -> PersistWorkspace
 -> Registered(snapshot)
```

An existing same-named session cannot be adopted without a short-lived confirmation challenge tied to its observed identity. If session creation succeeds but persistence fails, the daemon terminates only the newly created empty session. It never terminates a pre-existing/adopted session during rollback. Atomic state replacement plus fsync policy is documented by the implementation and fault-injection tested.

### Workspace lifecycle

`open` reads metadata and reconciles the named Zellij session. Missing sessions yield a visible `session_missing`/unknown item state; opening does not silently recreate processes. These operations are distinct and idempotent:

| Operation | Registry record | Agent item/process | Zellij session |
| --- | --- | --- | --- |
| detach/disconnect | retained | retained/running | retained |
| unregister | removed after confirmation | not explicitly stopped | retained |
| agent stop | retained | stopped | retained |
| session terminate | retained but unavailable, or explicitly unregister separately | stopped by Zellij | terminated |

No pin, close-page, back-navigation or network-loss operation maps to a destructive lifecycle method.

## Agent start, attachment and resume

### Start transaction

M1 supports one configured test/production agent kind at a time through the typed item interface. `agent.start` validates workspace revision and adapter kind, allocates an item ID, creates a dedicated managed Zellij tab, launches the fixed adapter executable with separately encoded arguments/environment, persists the target, and returns the item snapshot.

```text
Absent -> Starting -> Running -> Stopped
                    -> Failed
Running -> Unknown     (session/target cannot be observed)
```

Start uses an idempotency key. Retrying after a lost response returns the original item rather than launching a duplicate. If tab creation succeeds but launch or persistence fails, only that newly created tab is cleaned up. Agent stdout/stderr is PTY content and never used to infer lifecycle success beyond a bounded launch probe.

### Terminal attachment

The client attaches through the Zellij-owned PTY target recorded by `chooshd`. Each binding has `(item_id, connection_generation, target_generation)`. Input carrying any stale generation is rejected locally. Resize is clamped and acknowledged before the renderer treats new dimensions as authoritative.

The headless terminal oracle sends a fixture program in the managed tab. It emits deterministic VT sequences, reads typed input, prints a nonce-derived acknowledgement and remains running. Assertions inspect normalized terminal-engine state, not terminal screenshots and not parsed agent chat.

### Process recreation

Before recreation, the client checkpoint contains profile/workspace/item IDs, local pin/focus and last acknowledged revisions—not sockets, PTY handles, credentials or terminal content. On reconstruction:

1. load profile and IDs;
2. verify host key and reconnect;
3. negotiate RPC;
4. open a fresh workspace/item snapshot;
5. mark missing or changed targets unavailable;
6. allocate new connection and target generations;
7. attach to the current Zellij target;
8. restore focus only after binding succeeds.

Zellij and the agent MUST continue throughout. The old client's late callbacks and input are rejected. Scrollback continuity is asserted using a marker written before recreation and observed after reattachment.

## Root-confined file browser

### Path identity

Client requests use `(workspace_id, root_relative_components)`, never a client-supplied absolute path. Components are bytes represented by a reversible protocol encoding; display decoding is separate and lossy decoding cannot become authority. Empty, `.`, `..`, slash-containing, NUL-containing, overlong and excessive-depth components are rejected.

For every operation the host resolves from the registered canonical root and verifies the result remains beneath that root. Symlinks that resolve outside are returned as non-traversable entries with a stable `path_escape` outcome. A check is repeated at open/read time to limit time-of-check/time-of-use substitution.

### Directory page contract

`tree.list` accepts path identity, page token, page-size limit and optional name filter. It returns a snapshot token, bounded entries, and an opaque continuation token. Entries include encoded identity, escaped display name, kind, size/mtime where available, and traversal capability. Ordering is bytewise and deterministic within a snapshot; directories precede files only if explicitly encoded in the ordering contract.

Default bounds are 500 entries/page, 4 KiB encoded component, 64 components, and 2 MiB aggregate response. The negotiated host limit may lower them. Invalid/stale page tokens produce a typed error and require refresh; they never restart at a misleading offset.

### Failure behavior

Permission denied, vanished file, symlink escape, unsupported file type, stale page, decoding issue, timeout and disconnect have distinct codes. Partial pages carry an explicit incomplete marker and are not cached as complete. Refresh swaps a complete snapshot atomically.

## Markdown rendering and asset access

### Pipeline

The selected root-confined file is read through bounded SFTP, decoded under the text-file policy, parsed and sanitized by Rust, and rendered into an internal document origin on `127.0.0.1` with an ephemeral port and unguessable per-process authentication token. The WebView receives neither SSH credentials nor raw remote paths.

Raw HTML is sanitized or disabled. The generated document uses a restrictive CSP, locally bundled resources, no JavaScript bridge, no file/content access, no mixed content and no arbitrary navigation. Internal Markdown and future development services use separate origins and credentials.

### Relative assets

Relative links resolve against the Markdown document's root-relative parent using component-aware normalization. Absolute filesystem paths, network URLs not explicitly allowed for navigation, `file:`, `content:`, traversal and escaping symlinks do not become asset reads. Asset URLs contain opaque capabilities bound to workspace, canonical file identity, byte limit and expiry.

M1 supports bounded static images needed by the README fixture. Range streaming may be added later; an unsupported/oversized asset returns a stable placeholder response. Redirects are not followed across the trust boundary.

### Headless oracle

`markdown render` returns sanitized HTML plus a route manifest in the test runner. Tests parse the HTML DOM, CSP and URLs, fetch declared assets through the loopback server, and attempt malicious links/assets. Browser rendering is not the semantic oracle. A small WebView instrumentation suite additionally asserts navigation interception, disabled platform access and authenticated loopback requests.

## Failure model and recovery matrix

| Failure | Durable effect | Client behavior | Retry |
| --- | --- | --- | --- |
| changed host key | none | fail closed, show mismatch | only after separate trust-management action |
| bad credential | none | typed authentication failure | explicit/user policy |
| incompatible daemon | none | offer consented compatible install | after install |
| RPC/SFTP channel loss | host state retained | mark projections stale, reconnect transport | deterministic backoff |
| daemon restart | Zellij retained | reconnect, refresh snapshot, reattach | automatic within bound |
| Android process death | host/Zellij retained | reconstruct from IDs and fresh truth | on reopen |
| registration persistence fault | no half-record; new empty session rolled back | report failure | safe retry with idempotency key |
| agent start response loss | one item/process | return same item for same key | safe retry |
| root escape | none | visible non-traversable/error result | no automatic retry |
| Markdown limit/sanitize failure | none | explicit placeholder/error | after input or limit changes |

All retryable responses state `retryable=true`; the client does not infer retryability from prose. Timeouts do not imply remote cancellation of a committed operation, so mutation requests use idempotency keys and subsequent snapshot reconciliation.

## Deterministic fixture topology

The M1 acceptance environment consists of:

```text
scenario runner
  -> local SSH test server (fixed test identity)
       -> choosh-host rpc/stream
            -> chooshd on temporary 0600 Unix socket
                 -> fake session backend or real pinned Zellij
                 -> hostile project fixture
```

The fake session backend is used for exhaustive state/fault tests; a real pinned Zellij is required for the vertical acceptance scenario. The hostile project contains normal Markdown, a relative image, Unicode and newline filenames, unreadable entries where supported, deep/wide trees, inside/outside symlinks, a symlink swapped during read, oversized content and malicious Markdown/HTML.

The clock, UUID source, random source, host capabilities, filesystem root and fault schedule are injected. Golden outputs replace temporary absolute paths with typed fixture IDs before comparison.

## Headless acceptance scenarios

Each scenario starts from a fresh client store and temporary host state and emits a machine-readable transcript.

### A1: complete vertical slice

1. Add a profile using a Keystore test double and pin the fixture host key.
2. Connect and negotiate protocol v1.
3. Assert `workspace.list` is empty even though unrelated directories and Zellij sessions exist.
4. Register fixture root as `choosh-test`; create the same-named Zellij session.
5. Open the workspace and start the deterministic agent fixture with an idempotency key.
6. Attach, wait for its ready grid, send Unicode and an extra-key command, and assert its acknowledgement.
7. List the root, filter for `README.md`, and assert pagination identity/order.
8. Render README, validate its DOM/CSP, and fetch its relative image using the opaque route.
9. Checkpoint and destroy the client engine while the agent emits a scrollback marker.
10. Recreate, reconnect, refresh and reattach; assert the original item ID/process nonce and marker remain.
11. Detach and assert daemon, session and agent remain running.

### A2: trust and protocol failures

Changed host key, rejected first trust, invalid credential, incompatible major version, malformed welcome and oversized frame each fail with their specific code. No workspace/SFTP/PTY channel opens before host verification and negotiation succeed.

### A3: registration crash consistency

Fault injection at every registration transaction boundary proves there is no half-visible record, accidental adopted-session termination or duplicate session after an idempotent retry. Corrupt state files fail visibly and preserve the last valid snapshot where the atomic-store design permits.

### A4: path confinement

Attempt `..`, absolute paths, separator injection, outside symlinks, symlink swap, stale page tokens and hostile byte names. No response returns bytes or metadata from outside the fixture root. Every denial is visible and the subsequent valid request still succeeds.

### A5: Markdown isolation

Render script/raw HTML, external image, traversal asset, escaping symlink, oversized data URI, malformed image and hostile link fixtures. Assert sanitization, CSP, route capability binding/expiry, authentication rejection, and absence of credentials/absolute paths in HTML and logs.

### A6: lifecycle separation

Pin/unpin, focus change, disconnect and unregister do not stop the agent or terminate the session. `agent.stop` stops only its managed item. `workspace.terminate` requires a distinct confirmation challenge and does not silently unregister. Repeating each operation produces its documented idempotent result.

### A7: bounded degradation

Inject slow SFTP, stalled PTY, daemon restart, SSH loss, Android-engine recreation and response loss. Assert deadlines, bounded queues/tasks, deterministic retry schedule, fresh generations, no duplicate agent and no stale input delivery.

## Android platform instrumentation

The following remains automated but requires an emulator/device:

- Keystore-backed profile round trip and secret non-exportability/log redaction;
- lifecycle/process-recreation restoration through saved IDs;
- native terminal SurfaceView, IME composition, hardware/extra-key dispatch and resize;
- Markdown WebView CSP/navigation/file/content access settings;
- traversal and symlink failures presented as non-success UI states;
- accessibility labels for trust, failure and destructive confirmation surfaces.

Instrumentation uses fixture servers and accessibility/test APIs. Screenshots are diagnostic artifacts, not pass criteria.

## Security assertions

During every acceptance scenario an observer enumerates listening sockets, process arguments, environment capture allowed by the test sandbox, logs and generated HTML. It MUST establish:

- no host TCP listener is created by Choosh components;
- Android loopback listeners bind only `127.0.0.1` and require authentication;
- secrets, opaque capabilities, terminal bytes and absolute fixture paths are absent from logs;
- no shell receives user-controlled workspace names, roots, filenames or agent arguments as interpolated source;
- all child processes run as the SSH user/test equivalent and are reaped on teardown.

## M1 exit gate

M1 exits when A1–A7 pass against both the fake backend and the applicable real-Zellij subset on Linux x86_64, host protocol/schema checks pass, and Android instrumentation passes on arm64-v8a hardware plus an x86_64 emulator. The supported macOS arm64 host lane runs the same black-box contract before release artifacts are called compatible.

The final evidence manifest includes commit, pinned tool versions, host platform, fixture hashes, test commands, durations and result artifacts. From a new client state, the deterministic vertical slice registers a root, runs and resumes one agent across process recreation, browses and renders README, rejects traversal and escaping symlinks visibly, and observes no public listener.

## Excluded

Background notifications, multiple pinned pages, development services, source editing/saving, Git diff presentation, Markdown annotations, arbitrary browsing, automatic workspace discovery, agent-output parsing, and any lifecycle operation inferred from navigation are outside M1.
