# Choosh

Choosh is an Android-first remote development cockpit for persistent, agent-pluggable workspaces.

A workspace is an explicitly registered project root plus a Zellij session with the same name. Each coding agent and development service runs in its own managed Zellij tab. The Android client presents a fixed explorer followed by swipeable pinned terminals, Markdown previews, source editors, Git diffs, and tunneled web services.

## Status

Choosh is an early engineering preview. Signed preview APK releases, a private-socket
host daemon, and deterministic boundary tests exist. The native Android connection path now
has a bounded callback stream, exact-host SSH admission, payload-only signing, fixed RPC, and
one-close-only session ownership. The shipped M0 screen deliberately remains a connection-status
surface: it validates a profile ID and reports an unavailable saved profile; it does not yet
configure a host, establish a user session, or provide a usable terminal/workspace flow.

## What currently works

- The installable `ai.choosh` app launches a non-visual-testable Java/View screen with a labelled
  profile field and Connect action. A valid profile string enables Connect; the default composition
  fails closed as an unavailable saved profile.
- The Android/Rust bridge accepts only a plan-owned, bounded runtime callback capability. It checks
  the exact SSH host key before requesting a Keystore signature, exposes only framed fixed RPC, and
  releases the Android socket/runtime lease exactly once when a session closes.
- Headless generated-key acceptance proves `git.status` crosses authenticated SSH, the fixed
  `choosh-host rpc --stdio` command, and a real private `chooshd` Unix socket. This is not yet a
  claim of a configured, end-user Android host connection.

## Core decisions

- Android application ID: `ai.choosh`
- Android UI: a programmatic Java/View M0 connection-status screen today; Compose navigation
  and explorer surfaces remain a future target on the [stable Android/Kotlin toolchain](docs/specs/android-toolchain.md)
- Terminal: [Zelland-derived native wgpu/glyphon renderer](docs/specs/terminal-experience.md) with libghostty-vt and an Android IME extra-keys bar
- Source editor: [Sora Editor](https://github.com/Rosemoe/sora-editor)
- Durable engines: Rust on Android and a small Rust `chooshd` on the host
- Remote boundary: host-key-verified SSH only
- Persistence: Zellij sessions and tabs
- Documents: SFTP, with revision-aware saves
- Markdown: Maud/Datastar fragments in a locked-down WebView
- Git review: host-supplied metadata/blobs with a bounded native LCS reference diff today;
  production diff algorithm/fidelity remains pending
- Agent adapters: fixture-normalized Codex, OpenCode, and Claude Code lifecycle events;
  each adapter is independently versioned and maintained, and absent/incompatible adapters
  leave the terminal usable without notification integration
- Initial host targets: macOS/arm64 and Linux/x86_64

## Documents

- [System design and delivery plan](CHOOSH_DESIGN_PLAN.md)
- [Specification index](docs/specs/README.md)
- [Current delivery status](PLAN.md)
- [Milestone plan](docs/milestones/README.md)
- [Architecture decisions](docs/adr/README.md)
- [Threat model](docs/threat-model.md)
- [Android release and Obtainium distribution](docs/release-android.md)
- [Android runtime callback contract](docs/specs/android-native-runtime.md)

## Planned repository layout

```text
android/app/          Packaging, Java/View M0 screen, and Android composition roots
rust/choosh-core/     Android-side state engine
rust/choosh-android-bridge/  Android/Rust bridge
rust/choosh-web/      Maud, Datastar, and loopback gateways
rust/chooshd/         Host workspace/item daemon
rust/choosh-host/     Host CLI, hooks, and SSH stdio bridge
protocol/             Schemas and protocol fixtures
docs/                 Specifications, ADRs, and threat model
```

## Licence

Choosh is intended to be distributed under Apache-2.0. Sora Editor remains an LGPL-2.1+ dependency and will be distributed with its required notices and replacement/relinking information.
