# M0 disposable-host acceptance runner

Status: implemented harness; it records the host half of the remaining Android-to-host
vertical gate and is not itself evidence that a device callback reached that host.

`scripts/run-host-acceptance.sh` is a reproducible, non-provisioning runner for a disposable
test account. The host must already have a per-user `chooshd` service and an already-deployed
fixed `choosh-host` executable. The runner opens one strict-known-host SSH exec channel, sends
two framed `host.describe` requests to `choosh-host rpc --stdio`, and verifies the two bounded
responses from the private daemon socket. Two requests make accidental one-shot relay behavior
observable.

It deliberately does **not** start `chooshd` through SSH, upload artifacts, add known hosts,
disable host-key checks, or use a remote shell snippet. Daemon/process persistence stays with the
host service manager rather than the client SSH lifetime.

## Configuration

Create a mode-`0600` local JSON file outside the repository. It has no private-key bytes;
`identity_file` and `known_hosts_file` name local files.

```json
{
  "schema_version": 1,
  "ssh": {
    "host": "acceptance.example.test",
    "port": 22,
    "username": "choosh_acceptance",
    "identity_file": "/secure/acceptance/id_ed25519",
    "known_hosts_file": "/secure/acceptance/known_hosts"
  },
  "remote": {
    "command": "/usr/local/lib/choosh/choosh-host",
    "socket_path": "/run/user/1001/chooshd.sock"
  }
}
```

The runner requires exactly this schema. It permits DNS/IPv4-style host labels only; use a
temporary DNS name rather than an IPv6 literal. The remote executable and socket are restricted
to canonical-looking absolute paths consisting only of safe path characters. They are harness
configuration, never Android protocol inputs.

Run it with:

```sh
scripts/run-host-acceptance.sh --config /secure/acceptance/host.json
```

The only success output is canonical, redacted JSON:

```json
{"schema_version":1,"ssh_host_key":"verified","stdio_rpc":"passed","private_socket_relay":"passed","requests":2}
```

This output intentionally contains no endpoint, local paths, payloads, or diagnostic stderr.
Failure output is a stable category only.

## Reproducible headless verification

The fixture does not contact a network or require a daemon:

```sh
scripts/test-host-acceptance-runner.sh
```

It uses an SSH-compatible fake to assert the exact argv: `BatchMode`, strict host-key checking,
the supplied user-known-hosts file, disabled global known-hosts lookup, explicit identity, and
the fixed `choosh-host rpc --stdio --socket` argv. A metacharacter-bearing socket path is rejected
before the SSH executable is invoked.

Where OpenSSH server tooling is available, a second headless lane starts an
ephemeral loopback-only `sshd` and a real private-socket `chooshd`, generates
both host and client keys in a temporary directory, and invokes the same
runner:

```sh
CARGO_TARGET_DIR=/tmp/choosh-target scripts/test-host-acceptance-local-openssh.sh
```

It verifies two real SSH-stdio-to-`chooshd` requests and then replaces the
known-host entry with a different generated key. The runner must fail without
stdout. The lane is optional in generic CI because it requires an `sshd`
binary, its root-managed `/run/sshd` privilege-separation directory, and
permission to bind a loopback port; it creates no users, system services,
firewall rules, or persistent keys. Missing prerequisites fail with a stable
preflight category before any build, key, listener, or daemon is created.

## Promotion to the complete vertical test

The final M0-R5/R6 instrumentation must run this disposable host while the installed Android app
supplies its actual Keystore-backed callback and socket lease. It must also retain the following
evidence separately:

1. exact known-host success and changed-host failure before any signing callback;
2. an authenticated fixed `git.status` request through the real Android JNI callback/socket,
   not a generated-key Rust fixture;
3. cancellation and reconnect after an SSH interruption while the per-user daemon remains alive;
4. bounded redacted logs plus runner cleanup of the disposable account/host.

The existing generated-key tests prove the Rust composition and private-socket relay. This runner
makes the disposable-host portion repeatable without overstating that remaining Android evidence.
