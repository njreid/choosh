# Toolchain provisioning

Status: Draft

## Scope

How `choosh-hostd` keeps a workspace's language/tool versions correct and
its own operational tooling current, entirely via `mise`, with no manual
installation step on any devhost beyond the one-time bootstrap in
[`host-deployment.md`](host-deployment.md). See
[`../../DESIGN.md`](../../DESIGN.md) §10 for the narrative version.

## Two tiers

### Project-pinned toolchains

Each Project MAY declare a `mise.toml` (language runtimes, cloud CLIs, any
tool `mise` can manage) at its repository root. On `workspace.create`
(see [`jj-integration.md`](jj-integration.md)), `choosh-hostd` MUST run
`mise install` against that workspace's directory before reporting the
workspace ready.

Every agent, service, and shell process `choosh-hostd` spawns inside that
workspace MUST have `mise env` for that workspace's directory injected
into its environment. This environment MUST NOT leak into `choosh-hostd`'s
own process environment or into any other concurrently open workspace on
the same host — each spawned process gets its own resolved environment,
scoped to the directory `mise` was invoked against.

Project-pinned versions MUST NOT move on their own. A `mise.toml` pinning
`node@20` keeps resolving to `node@20` until a change to that file changes
it; `choosh-hostd` MUST NOT silently upgrade a project's pinned toolchain.

### Host-managed tools

`jj`, `zellij`, `zed-remote-server`, and `mise` itself are not project
state — they are what `choosh-hostd` depends on to function, and are kept
current by `choosh-hostd` against its own global `mise` config, at
tool-specific triggers rather than a single fixed schedule:

- `jj`, `zellij`: checked on daemon start, and periodically thereafter
  (a background check on a multi-hour interval is sufficient — these
  change infrequently).
- `zed-remote-server`: checked on each incoming Zed connection attempt,
  against the version Zed's exec request declares it expects, per
  [`ssh-bridge-and-zed.md`](ssh-bridge-and-zed.md). This is a per-connection
  check, not a periodic one, because the required version is a property of
  the connecting Zed client, not of the host.

Host-managed tool resolution MUST be isolated from project-pinned
resolution: a project's `mise.toml` MUST NOT affect which `jj`/`zellij`/
`zed-remote-server` version `choosh-hostd` itself runs, and vice versa.

## The `ubi` backend

Tools that ship as GitHub release binaries with no dedicated `mise` plugin
— `zed-remote-server` today — are installed via `mise`'s generic `ubi`
backend, which wraps the [`ubi`](https://github.com/houseabsolute/ubi)
universal binary installer:

```sh
mise use ubi:zed-industries/zed[exe=zed-remote-server]@<version>
```

**Open item, not yet verified:** the exact asset-matching syntax needed for
Zed's actual release naming (assets are per-platform and gzip-wrapped,
e.g. `zed-remote-server-linux-x86_64.gz`) has not been confirmed against
`ubi`'s matching/unpack behavior. This needs a live check before relying
on it; do not assume the syntax above is exact without verifying it
against a real `mise use ubi:...` invocation for this asset.

## Failure handling

If `mise install` fails for a workspace's `mise.toml` during
`workspace.create`, `choosh-hostd` MUST surface that as a distinct
workspace state (e.g. `provisioning_failed`, carrying the failing tool and
a bounded error summary) rather than reporting the workspace ready with a
partially provisioned environment. The Android app MUST render this state
distinctly from `ready` and MUST NOT allow attaching an agent or terminal
to a workspace in this state until it is re-provisioned successfully.

Failure to update a host-managed tool (e.g. `zed-remote-server` version
mismatch that `mise`/`ubi` cannot resolve) MUST fail that specific
connection attempt with a clear reason, and MUST NOT be silently ignored
by falling back to a stale cached binary without reporting the mismatch.
