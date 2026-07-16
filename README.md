# Choosh

Choosh is an Android-first remote development cockpit for persistent, agent-neutral workspaces.

A workspace is an explicitly registered project root plus a Zellij session with the same name. Each coding agent and development service runs in its own managed Zellij tab. The Android client presents a fixed explorer followed by swipeable pinned terminals, Markdown previews, source editors, Git diffs, and tunneled web services.

## Status

Choosh is in specification and risk-spike stage. No production APK or host daemon exists yet.

## Core decisions

- Android application ID: `ai.choosh`
- Android UI: Jetpack Compose on the [latest mutually compatible stable Android/Kotlin toolchain](docs/specs/android-toolchain.md)
- Terminal: [Zelland-derived native wgpu/glyphon renderer](docs/specs/terminal-experience.md) with libghostty-vt and an Android IME extra-keys bar
- Source editor: [Sora Editor](https://github.com/Rosemoe/sora-editor)
- Durable engines: Rust on Android and a small Rust `chooshd` on the host
- Remote boundary: host-key-verified SSH only
- Persistence: Zellij sessions and tabs
- Documents: SFTP, with revision-aware saves
- Markdown: Maud/Datastar fragments in a locked-down WebView
- Git review: host-supplied metadata/blobs, client-computed native diffs
- Supported agents: Codex, OpenCode, and Claude Code interactive TUIs
- Initial host targets: macOS/arm64 and Linux/x86_64

## Documents

- [System design and delivery plan](CHOOSH_DESIGN_PLAN.md)
- [Specification index](docs/specs/README.md)
- [Milestone plan](docs/milestones/README.md)
- [Architecture decisions](docs/adr/README.md)
- [Threat model](docs/threat-model.md)
- [Android release and Obtainium distribution](docs/release-android.md)

## Planned repository layout

```text
android/app/          Android packaging and entry point
android/ui/           Compose shell and platform adapters
rust/choosh-core/     Android-side state engine
rust/choosh-android/  Kotlin/Rust bridge
rust/choosh-web/      Maud, Datastar, and loopback gateways
rust/chooshd/         Host workspace/item daemon
rust/choosh-host/     Host CLI, hooks, and SSH stdio bridge
protocol/             Schemas and protocol fixtures
docs/                 Specifications, ADRs, and threat model
```

## Licence

Choosh is intended to be distributed under Apache-2.0. Sora Editor remains an LGPL-2.1+ dependency and will be distributed with its required notices and replacement/relinking information.
