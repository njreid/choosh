# Milestone 7 — Fleet, offload, and provisioning

Proves the multi-devhost, multi-cloud-account promise that motivated the
relay architecture in the first place, plus the provisioning story that
keeps a fleet's tooling current without hand-maintenance.

## Scope

- Fleet view: every enrolled devhost, across every AWS/GCP/Azure account,
  visible and status-tracked from `relayd`'s presence registry alone — the
  phone never holds cloud credentials.
- `dev-exec --host=<id> <cmd>`: cross-host task offload, brokered by
  `relayd`, running against a matching `jj` revision in an ephemeral
  workspace on the target host.
- SSO/cloud-CLI device-code bridge: `hostd` detects a headless device-code
  flow (`aws sso login` etc.), emits `auth_required` over the relay event
  bus, phone opens the verification URL; on a devhost with a local
  browser, the flow stays local and never touches the relay.
- Two-tier `mise` provisioning end-to-end (§10): project-pinned toolchains
  via `mise.toml` on workspace registration; host-managed tools (`jj`,
  `zellij`, `zed-remote-server`) kept current by `hostd` at their natural
  triggers, including the `ubi` backend path for GitHub-release binaries.

## Exit criteria

- The fleet view correctly lists and status-tracks devhosts in at least
  two different AWS accounts with no shared credentials between them.
- `dev-exec` against a heavier host completes a real build/test command
  and streams output back to the originating agent's terminal.
- A device-code SSO flow on a headless cloud devhost completes via the
  phone with no command, token, or prompt text leaking into the
  notification.
- A workspace registered against a Project with no `mise.toml` still gets
  a working `jj`/`zellij` environment; one with a `mise.toml` pinning an
  older runtime never gets silently upgraded.
