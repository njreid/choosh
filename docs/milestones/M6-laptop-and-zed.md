# Milestone 6 — Laptop proxy and Zed bridge

Proves [DESIGN.md](../../DESIGN.md) §9's core claim: `ssh <devhost>` and
Zed's remote-project picker work with zero manual trust steps, because the
trust decision already happened once at enrollment.

## Scope

- `choosh-hostd`'s loopback SSH server: accepts only relay-tunneled
  connections, trusts the tunnel's identity claim, execs a shell or a
  version-checked/`mise`-updated `zed-remote-server` against a workspace
  path.
- `choosh-hostd proxy enroll`, `proxy connect` (the literal `ProxyCommand`),
  and `proxy sync` (writes/updates `~/.ssh/known_hosts` and `~/.ssh/config`
  from `relayd`'s relay-attested fleet list).
- `EditorPresence` item on Android: read-only "editing in Zed on `<host>`"
  indicator, driven by a `hostd`-emitted presence event.

## Exit criteria

- On a laptop that has only ever run `choosh-hostd proxy enroll` once,
  `ssh <devhost-alias>` succeeds with no fingerprint prompt and no
  password.
- Opening that same alias as a Zed remote project succeeds unmodified —
  Zed needs no Choosh-specific configuration.
- Adding a new devhost to the fleet makes it `ssh`-reachable from an
  already-enrolled laptop after the next `proxy sync`, with no re-run of
  `proxy enroll`.
- A file edited in Zed and a file written by an agent in the same
  workspace at overlapping times both land correctly in the next `jj`
  snapshot — no lock contention, no corrupted write.
