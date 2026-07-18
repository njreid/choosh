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

## Candidate audit (rechecked 2026-07-18)

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
- SFTP is a separate integration (`russh-sftp 2.3.0`, Apache-2.0). Its client accepts
  a raw asynchronous subsystem stream rather than depending on a matching Russh
  release, so the earlier concern about an exact Russh version pair is not the main
  blocker. Its complete resolved graph, packet/resource limits, cancellation behavior,
  and Android build still have not been locked or tested in this repository.
- No pinned dependency, Cargo lock evidence, Android arm64/x86_64 build, local-server
  multiplex fixture, or fairness measurement exists here.

Rechecking with default features disabled does not make `russh 0.62.2` admissible.
`curve25519-dalek =5.0.0-rc.1`, `digest 0.11.0-rc.5`,
`ed25519-dalek =3.0.0-rc.1`, and the release-candidate P-curve crates are mandatory
direct dependencies. Disabling the optional `rsa` feature removes its exact
release-candidate `rsa` and `pkcs1` dependencies, and selecting `ring` instead of
`aws-lc-rs` changes the native crypto backend, but neither choice removes the mandatory
pre-release graph. Therefore Russh remains the closest technical fit but does not pass
Choosh's production dependency policy without the already-described time-bounded ADR.

Primary evidence:

- [Russh 0.62.2 package manifest](https://docs.rs/crate/russh/0.62.2/source/Cargo.toml)
- [Russh client handler and host-key callback](https://docs.rs/russh/0.62.2/russh/client/trait.Handler.html)
- [Russh client handle channel API](https://docs.rs/russh/0.62.2/russh/client/struct.Handle.html)
- [Russh protocol/channel overview](https://docs.rs/russh/0.62.2/russh/)
- [russh-sftp 2.3.0 documentation](https://docs.rs/russh-sftp/2.3.0/russh_sftp/)
- [russh-sftp 2.3.0 package source and licence](https://docs.rs/crate/russh-sftp/2.3.0/source/)

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

`libssh-rs 0.3.8` is a technically broader native alternative, not an immediate
unblock. Its API exposes multiple session channels, PTY/exec, SFTP, nonblocking poll
state, and a `direct-tcpip`-style forwarding channel. The Rust wrapper is MIT licensed,
but it binds and may vendor libssh plus OpenSSL and zlib. Choosh has no locked Android
build, native source digests, ABI packaging result, or complete licence/notice and
source-offer analysis for that native graph. It also requires the same explicit
single-session fairness, cancellation, host-key-before-authentication, and resource
limit harness work as `ssh2`. It is therefore a valid dependency-only spike candidate,
but not more admissible than Russh today.

Primary evidence:

- [libssh-rs 0.3.8 package, dependencies, features, and wrapper licence](https://docs.rs/crate/libssh-rs/0.3.8)
- [libssh-rs session SFTP and nonblocking poll API](https://docs.rs/libssh-rs/0.3.8/libssh_rs/struct.Session.html)
- [libssh-rs channel PTY, exec, and forwarding API](https://docs.rs/libssh-rs/0.3.8/libssh_rs/struct.Channel.html)

Two other stable Rust-facing options do not satisfy the M0 harness surface:

- `openssh 0.11.6` shells out to a Unix OpenSSH client or talks to its control-master
  socket. Android does not provide that process/runtime contract, and the crate is not
  an in-process SSH transport implementation.
- Pure-Rust `ssh-rs 0.5.0` exposes shell and exec session APIs, but its published API
  has no SFTP or `direct-tcpip` channel surface. Its stable dependency versions do not
  compensate for the missing required capabilities.

Primary evidence:

- [openssh 0.11.6 process/control-master design and manifest](https://docs.rs/crate/openssh/0.11.6)
- [ssh-rs 0.5.0 package and dependency list](https://docs.rs/crate/ssh-rs/0.5.0)
- [ssh-rs 0.5.0 published API surface](https://docs.rs/ssh-rs/0.5.0/ssh/all.html)

## Unblock gate

Proceed with Russh only after one reviewed dependency-only spike can:

1. select a Russh/russh-sftp graph whose entire lock contains no prerelease production
   crates, or approve a time-bounded crypto ADR; alternatively, select `libssh-rs` only
   after its complete native Android graph and distribution obligations are approved;
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
