# Threat model

Status: Initial draft

## Assets

- SSH private keys and passphrases;
- remote source code, Git objects, terminal content, prompts, and agent output;
- ability to execute commands and approve agent actions;
- host workspace and service metadata;
- annotations and unsaved editor buffers.

## Trust boundaries

1. Android UI ↔ Android Rust engine.
2. Android ↔ host over verified SSH.
3. SSH stdio bridge ↔ `chooshd` Unix socket.
4. `chooshd` ↔ Zellij, Git, agent hooks, and project filesystem.
5. Android loopback gateway ↔ untrusted remote development web app.
6. Internal Markdown WebView ↔ Rust rendering server.

## Adversaries

- network attacker attempting host impersonation;
- malicious or compromised remote project;
- hostile filenames, symlinks, Git configuration, hooks, and diff drivers;
- compromised development web app;
- malicious Android app probing loopback ports;
- agent/tool output containing control sequences or deceptive paths;
- accidental destructive user action.

## Required controls

- Pin and verify SSH host keys; fail closed on mismatch.
- Store client credentials using Android Keystore-backed encryption.
- Never expose SSH credentials, RPC, or SFTP to WebViews.
- Run `chooshd` as the SSH user; use a `0600` Unix socket and state directory.
- Canonicalize every host path beneath an explicitly registered root.
- Use fixed Git arguments; disable external diffs, text conversion, pagers, and interactive prompts.
- Treat agent events and reported file paths as untrusted hints.
- Require explicit registration for workspaces and services.
- Require authenticated loopback gateways; random ports are insufficient.
- Separate internal and development-service WebViews and cookie stores where platform support permits.
- Bound frames, event spools, directory results, blobs, diffs, headers, tunnels, and logs.
- Keep destructive stop/terminate operations separate from pin/unpin and require confirmation.

## Initial abuse cases

| Case | Control |
| --- | --- |
| Project path escapes through symlink | Canonical root check for every operation |
| Git config executes diff helper | Disable external diff/textconv and environment-controlled helpers |
| Agent emits fake changed path | Canonicalize and reconcile with Git status |
| Another Android app probes service port | HttpOnly random gateway cookie; reject before SSH forwarding |
| Development app attacks internal API | Separate WebView/origin; no bridge or shared internal token |
| Hook automatically approves command | Adapters are observational; ignore decision output |
| Replayed event creates stale notification | Per-workspace sequence, ack, item/status reconciliation |
| Oversized diff exhausts memory | Negotiated byte/line/hunk/time limits |

## Open questions before release

- Host binary update signing and rollback.
- Unix socket peer credential verification on both host platforms.
- WebView data-directory/profile isolation across supported Android versions.
- Terminal escape-sequence filtering and clipboard policy.
- Annotation export confidentiality and repository inclusion policy.

