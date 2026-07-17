# M0 SSH and RPC implementation audit

Audit date: 2026-07-17. This is an implementation evidence inventory, not an M0-R5/R6 pass.

## Present headless evidence

- `choosh-core::connection` models fail-closed first trust, exact fingerprint consent, changed-key
  rejection, authentication outcomes, reconnect generations, and stale-channel rejection.
- Injectable SSH, SFTP, RPC, notification, and loopback gateway capabilities have deterministic
  fakes. They do not establish interoperability with an SSH implementation.
- `choosh-protocol` bounds four-byte framing, hello/welcome negotiation, request/response/event
  lifecycles, confirmation challenges, and EOF session invalidation.
- `choosh-host::bridge` bounds raw stdio reads, frame batches, response writes, errors, and
  diagnostics. Its local full-duplex EOF gate proves a completed frame is handled and echoed before
  clean EOF terminates the bridge, while daemon-owned process state remains unchanged.
- `chooshd::socket` rejects unsafe layouts and existing path types, creates a private `0700` state
  directory, binds only an injected Unix socket with mode `0600`, and performs identity-checked
  cleanup.
- Shell-free `choosh-host rpc --stdio` parsing exists.

## Missing M0-R5/R6 evidence

- There is no concrete SSH transport adapter, local SSH server harness, or black-box multiplex test
  for concurrent PTY, exec/RPC, SFTP, and direct-tcpip channels on one verified connection.
- Host-key policy is a tested state machine, not yet connected to a cryptographic SSH library or a
  persisted known-host store.
- `chooshd` now has a minimal composition-root binary with mandatory injected
  `--state-dir` and `--socket` paths. It binds the existing private Unix socket
  lifecycle and exposes only bounded raw `health`/echo frames. This is a
  black-box process seam, not the versioned JSON hello/welcome protocol.
- `choosh-host rpc --stdio --socket <absolute-path>` now composes bounded stdio
  framing with the injected daemon socket. The exact shell-free argument grammar
  rejects relative, non-normal, and oversized paths without echoing them.
- `scripts/test-rpc-socket.sh` starts both process roots, verifies the `0700`
  state directory and `0600` socket, and round-trips framed raw
  `health`/`healthy` evidence through `choosh-host` stdio. It does not claim
  JSON hello/welcome, parallel requests, or TCP-listener enumeration.
- The stdio bridge currently validates frame boundaries only. UTF-8, JSON, envelope kind, and
  negotiated-session enforcement live in separate protocol modules and are not composed behind the
  bridge handler.
- PTY latency/fairness under throttled SFTP, disconnect injection for every channel type, and
  direct-tcpip HTTP/WebSocket/SSE fidelity remain unimplemented.

Therefore M0-R5 and M0-R6 remain blocked. The new local process harness proves
the daemon's injected private-socket plus `choosh-host` stdio raw-frame seam.
The next vertical gate must add the versioned JSON protocol and verified SSH
transport; only after that should the repository claim end-to-end SSH
stdio-to-`0600`-socket behavior.
