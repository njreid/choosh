# Host deployment and service ownership

Status: Draft

## Scope

The Android client deploys immutable `chooshd` releases through the existing SSH/SFTP trust
boundary. A host service manager, rather than an SSH session or shell background job, owns the
running daemon. The service and Zellij-owned processes therefore survive Android transport loss.

## Service-manager boundary

The deployment composition root MUST inject a host service-manager adapter. It MUST select only
one of these explicit per-user targets:

- `systemd --user`: reload the manager, then enable and start the fixed `chooshd.service` unit.
- `launchd`: bootstrap a validated absolute plist into the numeric `gui/<uid>` domain, then
  kickstart the fixed `ai.choosh.chooshd` label.

Unsupported manager types MUST fail closed. The implementation MUST NOT use `sh`, shell
backgrounding, `nohup`, process-table discovery, or an ambient `$HOME`/current-directory lookup.
All manager invocations are fixed argv vectors; runner errors and non-zero statuses stop the
operation before later commands run.

The adapter receives no release archive, SSH credential, or private key. A launchd plist path is
validated as an absolute normalized path before it can be placed in an argv vector. Diagnostics
MUST redact deployment paths and manager arguments.

## Activation ordering

1. The installer has already staged and verified an immutable release and atomically selected it.
2. The injected manager adapter activates the fixed per-user daemon unit.
3. The installer health-checks `chooshd` through its private Unix socket.
4. On failure, the installer rolls back the selected release and invokes the manager's matching
   activation/stop procedure through the same adapter.

This document defines the manager boundary only. Upload, digest verification, socket health, and
rollback orchestration are separate increments.

## Authenticated updater wiring gate

The current authenticated [`VerifiedConnection`](../../rust/choosh-ssh/src/connection.rs)
capabilities are deliberately insufficient to perform this deployment transaction:

- Its admitted SFTP surface is bound to a server-attested workspace root. It MUST NOT be used
  for release directories, service units, or daemon state.
- That SFTP surface returns `atomic_write_not_proven` for every write. An updater MUST NOT
  emulate atomic installation with overwrite, rename-by-assumption, or a shell command.
- Its fixed SSH exec surface currently admits the existing host RPC dispatcher only. An updater
  MUST NOT invent a `choosh-host` subcommand or pass deployment paths, release bytes, or service
  manager arguments through the generic fixed-command encoder.

Before Android can wire an authenticated updater, a versioned host deployment protocol MUST add
both of these separately admitted capabilities:

1. an immutable, digest-addressed upload/stage operation with server-proven atomic publication;
2. a fixed deployment activation/health operation that owns the host paths and service-manager
   argv entirely on the host.

The Android transport then supplies only verified release bytes and metadata to those capability
ports. It MUST retain no host deployment path, service-manager argument vector, or shell text.

The first host-owned capability is `choosh_host::deployment::HostDeployment`. It accepts a
bounded `DeploymentUpload` containing only release version, SHA-256 digest, and artifact bytes;
its `ImmutableDeploymentStore`, `DeploymentService`, and `DeploymentHealth` adapters retain all
paths, atomic publication, manager argv, and private-socket health authority. Its transaction is
stage → digest → activate → service activation → version health, with one rollback after any
post-activation failure. A future versioned SSH stdin envelope MUST decode to this upload type
and invoke no broader capability.

The schema-1 envelope contains only the release version, lowercase hexadecimal SHA-256 digest,
and bounded artifact bytes. GitHub release discovery remains an Android-side authority: it selects
the newest stable release, verifies checksum and signer evidence, then serializes this envelope over
the authenticated SSH capability. The host never contacts GitHub and never accepts caller-supplied
paths, executables, or service-manager arguments.
