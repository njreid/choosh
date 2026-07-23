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
