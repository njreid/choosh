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
