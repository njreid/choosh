# Milestone 0 — Enrollment skeleton

Proves the trust boundary in [DESIGN.md](../../DESIGN.md) §5–§6 works
end-to-end before anything else is built on top of it: nothing dials in,
nobody types a password, and a devhost showing up on the phone is a real
cryptographic fact, not a UI mock.

## Scope

- `choosh-relayd`: WebAuthn RP (`webauthn-rs`) for passkey registration and
  assertion; presence registry; enrollment-token issuance to an
  already-authenticated session; a persistent-connection endpoint for
  Identities to dial into.
- `choosh-relayd` deployment: `just deploy <ec2-instance-name>` builds and
  ships the binary to the single owned EC2 instance and health-checks it.
- `choosh-hostd serve`: exchanges a one-shot enrollment token for a
  long-lived device credential, dials `relayd` outbound, reconnects with
  backoff, survives the install session closing (`loginctl enable-linger`
  on Linux, a `launchd` LaunchAgent on macOS).
- Platform bootstrap script (`curl -fsSL relay.example/install.sh | sudo sh
  -s -- --token=<token>`) installs `choosh-hostd` and its service-manager
  unit. `sudo` is used here only.
- Android app: passkey registration/login via Credential Manager, a
  persistent connection to `relayd`, and a fleet list showing every
  connected devhost's alias/platform/last-seen — nothing else yet.

## Explicit non-goals for M0

- No workspace, jj, Zellij, or agent integration.
- No FCM (the persistent connection is enough to prove presence while the
  app is open).
- No laptop proxy mode.

## Exit criteria

- From a clean phone, passkey registration completes with no password at
  any point, and reopening the app later reuses the stored credential
  silently.
- A freshly booted, unenrolled Linux and macOS instance each go from the
  one bootstrap command to appearing in the Android fleet list within the
  command's runtime, with no further manual step.
- Killing and restarting a devhost's network reconnects it without
  re-enrollment.
- `just deploy <ec2-instance-name>` updates a running `relayd` with no
  devhost or phone needing to reconnect through anything but the normal
  backoff/retry path.
