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
- There are library entry points but no composed `chooshd` and `choosh-host` binaries that connect
  SSH stdio framing to the daemon Unix socket.
- No single black-box test currently starts `chooshd`, inspects filesystem modes, invokes
  `choosh-host rpc --stdio`, performs hello/welcome, issues parallel requests, and proves absence of
  a TCP listener.
- The stdio bridge currently validates frame boundaries only. UTF-8, JSON, envelope kind, and
  negotiated-session enforcement live in separate protocol modules and are not composed behind the
  bridge handler.
- PTY latency/fairness under throttled SFTP, disconnect injection for every channel type, and
  direct-tcpip HTTP/WebSocket/SSE fidelity remain unimplemented.

Therefore M0-R5 and M0-R6 remain blocked. The next vertical gate should add composition-root
binaries and a local process harness; only after that should the repository claim end-to-end SSH
stdio-to-`0600`-socket behavior.
