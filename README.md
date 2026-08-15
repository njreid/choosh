# Choosh

Choosh is a personal control plane for a fleet of development hosts, driven
from an Android phone. Register a devhost once — a cloud instance in any AWS
account, a Linux box, a Mac laptop — and Choosh keeps its coding agents and
dev services running in Zellij, keeps a jj workspace per agent so concurrent
edits are never a problem, and pushes a notification the instant an agent
needs a decision, wherever the phone happens to be. Sit down at a keyboard
and the same fleet opens in Zed and a plain terminal with no extra setup:
`ssh <devhost>` just works, no keys to copy and no fingerprint to confirm.

Full architecture: **[DESIGN.md](DESIGN.md)**.

## The experience

- A devhost's agent needs input. Your phone buzzes — an FCM push, not a
  fragile background connection — with exactly what's blocked and nothing
  else (no command text, no file contents). Tap it: the agent's terminal is
  already pinned and focused.
- Open the app cold. No password, no PIN — Android's passkey prompt (or
  nothing at all, if the device credential is already unlocked) and you're
  looking at your fleet: every devhost across every cloud account, which
  ones are up, what's running on each.
- Pin a workspace's `jj` change graph, tap a commit to see the diff, tap
  `undo` if an agent went sideways. Pin the changed-files list and open one
  in the built-in editor to make a quick fix from the couch.
- Later, at a laptop: `zed mbp-home` or plain `ssh build-box-large` just
  works. No VPN, no manually trusted host key, no password — the trust was
  already established when that devhost was enrolled.

## Key deliverables

- **`choosh-relayd`** — one small Rust binary, deployed once in the cloud.
  The fleet's rendezvous point: presence, tunnel brokering, FCM dispatch,
  and the only place a passkey is ever checked.
- **`choosh-hostd`** — one small Rust binary installed on every devhost
  (daemon mode: workspace/jj/Zellij ownership, agent event bridge, SSH
  bridge for Zed) and on any laptop that wants a zero-setup SSH/Zed path
  into the fleet (proxy mode).
- **The Choosh Android app** (`ai.choosh`) — installable via Obtainium. The
  primary way you drive the fleet: fixed explorer, swipeable pinned
  terminals, jj change graph and diffs, an in-app editor, Markdown preview,
  and tunneled web previews.

## Core decisions

- Trust boundary: `choosh-relayd` brokers every byte between phone, laptop,
  and devhost; no devhost ever accepts an inbound connection.
- Auth: passkeys for humans, device credentials minted from a
  passkey-authenticated session for machines — no password, no manual SSH
  fingerprint confirmation, ever.
- VCS: jj only, via `jj-lib` embedded directly in `choosh-hostd` — no Git
  support, no on-device diff engine.
- Persistence: one Zellij session per Workspace (= one `jj workspace`),
  agents and services each in their own managed tab.
- Editing: Sora in-app for quick/no-desktop edits; a real Zed remote session
  tunneled through `relayd` when a laptop is available.
- Terminal: Zelland-derived native `wgpu`/`glyphon` renderer with an
  Android IME extra-keys bar. `libghostty-vt` was the original target VT
  parser but remains blocked on a stable, independently versioned pin (see
  `docs/licenses/terminal-provenance.md`); the engine ships a pure-Rust
  `vte`-backed parser behind the same interface instead, a deliberate
  scope decision, not a dead end.
- Notifications: FCM-driven, redacted to workspace/agent/coarse reason.
- Toolchains: per-project `mise.toml`, provisioned by `choosh-hostd` on
  workspace registration.
- Android application ID: `ai.choosh`.

## Status

All nine milestones (M0–M8) in [docs/milestones/](docs/milestones/) have
real implementation and verification evidence against the architecture in
[DESIGN.md](DESIGN.md); see [PLAN.md](PLAN.md) for the current status
ledger and named follow-ups. The legacy pre-reset crates (an SSH-only
transport, Git-based diffing, a two-binary host daemon) that predated this
architecture were removed during M6 once confirmed unreferenced. This does
not mean the product is finished or bug-free — most notably, no live
`choosh-relayd` deployment exists yet, so the Android app still defaults to
an in-memory fake engine and no genuine phone-to-relay-to-devhost round
trip has been exercised end-to-end (see PLAN.md's "Known follow-ups").
There is no shipped user base and no backwards compatibility constraint.

## Repository layout

See [DESIGN.md §13](DESIGN.md#13-target-repository-layout) for the full
target layout and what's being repurposed vs. built new.

## Licence

Choosh is intended to be distributed under Apache-2.0. Sora Editor remains an LGPL-2.1+ dependency and will be distributed with its required notices and replacement/relinking information.
