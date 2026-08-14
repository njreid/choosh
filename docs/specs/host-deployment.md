# Host deployment and service lifecycle

Status: Draft

## Scope

How `choosh-hostd` gets onto a devhost, how it stays running as a
service-manager-owned process rather than a shell background job, and how
it updates itself afterward. See [`../../DESIGN.md`](../../DESIGN.md) §6
for the narrative version.

## Bootstrap install

The install command is:

```sh
curl -fsSL relay.example/install.sh | sudo sh -s -- --token=<enrollment-token>
```

`<enrollment-token>` is a single-use, short-lived token minted by
`relayd` from an already passkey-authenticated session — see
[`auth-and-enrollment.md`](auth-and-enrollment.md). The script MUST, in
order:

1. Detect OS and architecture.
2. Install the OS-level prerequisites `mise` itself needs (a C toolchain,
   `curl`, `unzip`) via the platform package manager (`dnf`/`apt`/
   `brew`-equivalent). This script is the only component in the system
   permitted to know about a specific OS package manager; everything
   downstream is `mise`'s job (see
   [`toolchain-provisioning.md`](toolchain-provisioning.md)).
3. Download and install the `choosh-hostd` binary.
4. Write the platform service-manager unit (below) but MUST NOT start it
   yet.
5. Perform the one-time platform lifecycle setup that requires root (below).
6. Start the service via the service manager.
7. `choosh-hostd`, on that first start, exchanges `<enrollment-token>` for
   a long-lived per-host device credential and persists it, then dials
   `relayd` outbound.
8. Health-check `choosh-hostd`'s local RPC socket before the script exits
   successfully.

### `sudo` scope

`sudo` MUST be used only for this install and only for the two operations
that require root:

- Linux: `loginctl enable-linger $USER`, without which `systemd --user`
  kills all of that user's processes the instant the install session's
  login session ends.
- Both platforms: writing the service-manager unit file into a
  root-owned or otherwise privileged location, if the target platform
  requires it.

Nothing `choosh-hostd` does after this install step — enrollment,
reconnects, workspace registration, self-update — requires root.

## Linux service lifecycle

- Unit: `~/.config/systemd/user/choosh-hostd.service`, `Type=simple`,
  `Restart=on-failure`, `WantedBy=default.target`.
- `loginctl enable-linger $USER` MUST be set during install so the unit
  survives the installing SSH session closing.
- Activation: `systemctl --user daemon-reload && systemctl --user enable
  --now choosh-hostd.service`.

## macOS service lifecycle

- LaunchAgent plist at `~/Library/LaunchAgents/ai.choosh.hostd.plist` with
  `RunAtLoad` and `KeepAlive` set.
- Activation: bootstrap into the `gui/<uid>` domain and kickstart the
  `ai.choosh.hostd` label.
- Power assertions: `choosh-hostd` MUST claim an
  `IOPMAssertionCreateWithName` assertion
  (`kIOPMAssertionTypePreventUserIdleSystemSleep`) whenever the host has at
  least one attached PTY (an interactive terminal or agent session) or a
  running registered service/build process, and MUST release it as soon as
  none of those conditions hold. This prevents macOS sleep from severing
  the outbound `relayd` connection mid-task without holding the assertion
  needlessly while genuinely idle.

## Self-update

`relayd` can push an `UPDATE_BINARY` control frame (specified in
[`relay-protocol.md`](relay-protocol.md)) naming a target version and
digest. On receipt, `choosh-hostd` MUST:

1. Download the new binary to a sibling path (e.g.
   `choosh-hostd.new`) and verify its digest before proceeding.
2. `chmod +x` the new binary.
3. Atomically `rename()` it over the running binary's path.
4. Ask the service manager to restart the unit (`systemctl --user restart
   choosh-hostd.service` / `launchctl kickstart -k`) rather than
   self-exec'ing, so the service manager's own supervision (`Restart=
   on-failure`, `KeepAlive`) stays authoritative for the process lifecycle.

Zellij sessions and any processes running inside them MUST survive this
restart unaffected: `choosh-hostd` restarting MUST NOT signal or otherwise
disturb the Zellij server or the processes attached to it, since Zellij
owns process persistence independently of `choosh-hostd`'s own liveness
(design principle 6 in [`../../DESIGN.md`](../../DESIGN.md) §2).

### Rollback

If the restarted binary fails its local RPC socket health-check within a
bounded window, `choosh-hostd`'s supervising wrapper (or the service
manager's failure-restart path, if the health-check is itself what
`Restart=on-failure` observes) MUST roll back to the previous binary
automatically and restart with that version. A binary that repeatedly
fails health-check after rollback MUST stop retrying and report an
`agent_status`-shaped failure event upstream rather than crash-looping
indefinitely.
