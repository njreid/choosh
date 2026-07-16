# Repository guidance

## Priorities

1. Preserve the SSH-only trust boundary.
2. Keep durable state ownership explicit.
3. Treat all host paths, agent events, Git output, and service metadata as untrusted input.
4. Prefer versioned protocols and vertical acceptance tests over speculative features.

## Architecture constraints

- Android package and namespace are `ai.choosh`.
- No Node, Svelte, Tauri, public host listener, or agent-specific chat renderer.
- `chooshd` listens only on a per-user Unix socket.
- Android reaches `chooshd` through an SSH stdio bridge.
- Agent hooks are observational and never approve, deny, or rewrite operations.
- Zellij owns PTYs/process persistence; `chooshd` owns workspace and item metadata.
- Android computes textual Git diffs; the host supplies bounded metadata and blob versions.
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

- Update the relevant specification before changing a protocol or trust boundary.
- Add an ADR for decisions that are expensive to reverse.
- Use RFC 2119 terms only for normative requirements.
- Examples must not contain real credentials, hostnames, or user paths.

## Verification

- JSON files must parse with `jq`.
- JSON Schemas use draft 2020-12.
- Markdown links should be relative for repository-owned documents.
- Protocol examples must conform to their schemas once fixture validation is available.

## Increment workflow

- Treat each independently working, verified slice as a commit boundary; do not accumulate unrelated implementation increments in one commit.
- Before committing, run the checks relevant to the changed scope plus `git diff --check`, and review the staged file set for unrelated changes or sensitive data.
- Use a focused commit message that states the working increment delivered.
- Push each completed increment to the current tracked remote branch after committing, then verify the local and remote refs match and the worktree is clean.
- Do not commit or push a broken increment merely to checkpoint it. If external access prevents a push, preserve the verified local commit and report the exact blocker.
