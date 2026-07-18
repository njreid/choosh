# M0 SSH transport implementation choice

The dependency-independent executable contract is specified in the
[M0 SSH acceptance harness](m0-ssh-acceptance-harness.md). Candidate evaluation MUST use that
shared fixture surface rather than adapter-specific happy-path tests.

Status: Blocked; do not add an SSH dependency yet.

## Required fit

M0-R5 needs one Android-side Rust SSH client connection with mandatory exact-key
verification before authentication, concurrent session/PTY/exec, SFTP, and
`direct-tcpip` channels, bounded per-channel and aggregate queues, cancellation and
deadlines, and a disposable in-process/local-server harness. The existing
`choosh-core` connection policy and transport fakes do not prove interoperability.

## Candidate audit (2026-07-17)

`russh 0.62.2` is the best architectural candidate, but is not currently admissible:

- Its client handler requires `check_server_key`; the default rejects all keys. A
  Choosh adapter can compare the received public key with injected trusted-key bytes
  and return true only for an exact match before authentication.
- Its client handle exposes independent session and `direct-tcpip` channels, and
  Russh describes channels as parallel requests on one connection. Session channels
  support PTY and exec requests.
- It is asynchronous over Tokio and has an Apache-2.0 package declaration, matching
  Choosh's language/runtime and top-level licence direction.
- However, the 0.62.2 manifest defaults to `aws-lc-rs` and RSA and directly pins
  multiple release-candidate cryptography crates (`curve25519-dalek`,
  `ed25519-dalek`, `p256`, `p384`, `p521`, `pkcs1`, and `rsa`). Choosh policy forbids
  pre-release production dependencies without an ADR and expiry condition.
- SFTP is a separate integration (`russh-sftp`, currently documented as 2.3.0).
  Its exact compatible Russh version, complete transitive licences, packet/resource
  limits, cancellation behavior, and Android build have not been locked or tested in
  this repository.
- No pinned dependency, Cargo lock evidence, Android arm64/x86_64 build, local-server
  multiplex fixture, or fairness measurement exists here.

Primary evidence:

- [Russh 0.62.2 package manifest](https://docs.rs/crate/russh/0.62.2/source/Cargo.toml)
- [Russh client handler and host-key callback](https://docs.rs/russh/0.62.2/russh/client/trait.Handler.html)
- [Russh client handle channel API](https://docs.rs/russh/0.62.2/russh/client/struct.Handle.html)
- [Russh protocol/channel overview](https://docs.rs/russh/0.62.2/russh/)
- [russh-sftp 2.3.0 documentation](https://docs.rs/russh-sftp/2.3.0/russh_sftp/)

`ssh2 0.9.6` is not the preferred fallback. It has stable dual MIT/Apache licensing
and exposes host-key bytes/known-host checks, session channels, SFTP, and
`direct-tcpip`. But it binds native libssh2/OpenSSL. Its session documentation warns
that a blocking channel operation blocks all other objects on the same internally
locked session; concurrency needs a carefully driven nonblocking loop or multiple
connections. That conflicts with the M0 single-connection fairness gate and adds
Android native packaging, OpenSSL/libssh2 licence inventory, and cancellation work.

Primary evidence:

- [ssh2 0.9.6 package and licence](https://docs.rs/crate/ssh2/0.9.6)
- [ssh2 Session host-key, channel, SFTP, and concurrency behavior](https://docs.rs/ssh2/0.9.6/ssh2/struct.Session.html)

## Unblock gate

Proceed with Russh only after one reviewed dependency-only spike can:

1. select a Russh/russh-sftp pair whose entire locked graph contains no prerelease
   production crates, or approve a time-bounded crypto ADR;
2. inventory every transitive licence and crypto backend, disabling insecure legacy
   algorithms and unused default features explicitly;
3. compile and test on Android arm64-v8a and x86_64 with the pinned Rust toolchain;
4. run a generated-key local Russh or OpenSSH fixture proving unknown/rejected/exact/
   changed host-key outcomes before authentication;
5. concurrently exercise PTY, fixed-argv exec, SFTP, and loopback-only
   `direct-tcpip`, including independent cancellation and disconnects;
6. enforce injected time, channel-count, packet, per-channel queue, and aggregate-byte
   limits and pass the documented throttled-SFTP/PTY latency budget without sleeps or
   external services.

Until those checks pass, `choosh-core` traits and deterministic fakes remain the only
SSH implementation surface; M0-R5 is not complete.
