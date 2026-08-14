# Milestone 1 — Workspace and jj foundation

Proves the Workspace = `jj workspace` + Zellij session model
([DESIGN.md](../../DESIGN.md) §4, §8) end-to-end, without yet needing a
live terminal or an agent attached.

## Scope

- `choosh-hostd`: Project/Workspace registry (explicit registration only,
  canonical root confinement — no filesystem discovery); `jj-lib` embedded
  and pinned; `jj workspace add` on registration; a same-named Zellij
  session created alongside it.
- RPC: `workspace.create`, `workspace.list`, `workspace.status`,
  `workspace.tree.list`, `workspace.file.read` (bounded, root-confined,
  ranged reads for large files).
- Android: register a workspace against a chosen devhost (fresh clone or
  an existing local repo path), list registered workspaces, browse the
  file tree read-only, open a file read-only.

## Exit criteria

- Registering a workspace against a fresh Git remote produces a
  git-colocated `jj` repo, a named `jj workspace`, and a same-named Zellij
  session, all visible from the phone.
- Registering a second workspace against the same Project on the same host
  produces an independent `jj workspace` sharing the one repo store.
- Browsing and reading files works for both the live working copy and at
  least one historical revision, with paths canonicalized under the
  workspace root before any read is served.
- A conflicted path (constructed via two `jj workspace`s editing the same
  file) is flagged as conflicted in `workspace.status`, not silently
  resolved or hidden.
