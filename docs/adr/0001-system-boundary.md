# ADR 0001: SSH-only system boundary

Status: Accepted

## Decision

All host communication uses one host-key-verified SSH connection. PTYs, stdio RPC, SFTP, binary streams, and service forwarding use separate SSH channels. `chooshd` exposes only a per-user Unix socket and no TCP listener.

Android Rust owns connection and document state. Host Rust owns workspace/item metadata. Neither WebView receives SSH credentials or direct remote paths.

## Consequences

- No VPN, public host port, TLS certificate, or web authentication service is required.
- SSH host-key UX, reconnect behavior, channel limits, and command construction become critical infrastructure.
- Features requiring remote access must fit an SSH channel or be rejected.

