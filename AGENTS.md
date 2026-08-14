# Repository guidance

See [DESIGN.md](DESIGN.md) for the full architecture. This file states the
constraints that follow from it.

## Priorities

1. Preserve the relay-brokered trust boundary: no devhost ever accepts an
   inbound connection; the phone only ever talks to `choosh-relayd`.
2. Keep durable state ownership explicit.
3. Treat all host paths, agent events, jj output, and service metadata as untrusted input.
4. Prefer versioned protocols and vertical acceptance tests over speculative features.

## Architecture constraints

- Android package and namespace are `ai.choosh`.
- No Node, Svelte, Tauri, or agent-specific chat renderer.
- No devhost ever listens on a public port; `choosh-hostd` always dials outbound to `choosh-relayd`.
- `choosh-hostd`'s RPC socket and SSH server bind to loopback only; the only way in is a `relayd`-brokered tunnel.
- No password-based or manually-confirmed-fingerprint authentication anywhere: passkeys for humans, device credentials minted from a passkey session for machines.
- Agent hooks are observational and never approve, deny, or rewrite operations.
- Zellij owns PTYs/process persistence; `choosh-hostd` owns workspace and item metadata.
- jj only, via `jj-lib` embedded in `choosh-hostd`; no Git support, no on-device diff engine — diffs are computed host-side and shipped as structured hunks.
- This repository's own version control is jj too, git-colocated for GitHub interop. Use `jj` commands (`jj new`, `jj describe`, `jj diff`, `jj log`, `jj git push`), never `git`, for local history operations here.
- Android and Kotlin dependencies are pinned stable releases; preview SDKs run only in a separate compatibility lane.
- Terminals use the native Rust GPU renderer; terminal modes and input encoding stay out of Compose and WebViews.

## Visual language

- Use Geomini as the default typeface for general application UI.
- Use Iosevka Charon Mono for terminal surfaces and headings.
- Bundle or ship fonts only after recording their exact source, version, licence, redistribution obligations, and required notices. Define deterministic fallbacks for missing glyphs and verify layout without visual-only acceptance steps.

## Dependency injection and composition

- Use explicit constructor injection and narrow capability interfaces to keep components independently implementable and headlessly testable.
- Define injectable boundaries for time, ID generation, durable storage, SSH/SFTP transport, host RPC, Zellij control, Git data, notifications, loopback gateways, and process launch.
- Rust code uses traits with generics or `Arc<dyn Trait>` as appropriate; assemble concrete implementations only in binary, JNI, or other outer composition roots.
- Kotlin code uses constructor injection; keep application and Android component wiring in composition roots at the `android/app` boundary.
- Prefer deterministic fakes over mocks that assert call order. Tests must be able to inject clocks, IDs, transports, storage, and fault behavior without wall-clock sleeps or external services.
- Do not use service locators, mutable global singletons, or hidden ambient dependencies.
- Do not introduce a DI framework during M0. If object-graph construction later justifies one, confine framework annotations and modules to `android/app`; shared and domain modules remain framework-agnostic.

## Documentation

- Update DESIGN.md before changing a protocol or trust boundary.
- Use RFC 2119 terms only for normative requirements.
- Examples must not contain real credentials, hostnames, or user paths.

## Verification

- JSON files must parse with `jq`.
- JSON Schemas use draft 2020-12.
- Markdown links should be relative for repository-owned documents.
- Protocol examples must conform to their schemas once fixture validation is available.

## Increment workflow

- Treat each independently working, verified slice as its own `jj` change; use `jj new` to cut the boundary rather than accumulating unrelated increments into one change.
- Before finalizing a change (`jj describe`), run the checks relevant to the changed scope and review `jj diff` for unrelated edits or sensitive data.
- Use a focused change description that states the working increment delivered.
- Push each completed increment (`jj git push`) after describing it, then verify the local and remote bookmarks match and the working copy is clean.
- Do not finalize or push a broken increment merely to checkpoint it. If external access prevents a push, preserve the verified local change and report the exact blocker.
