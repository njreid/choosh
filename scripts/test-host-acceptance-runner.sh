#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture="$(mktemp -d -t choosh-host-acceptance-test.XXXXXX)"
cleanup() { rm -rf -- "$fixture"; }
trap cleanup EXIT INT TERM

identity="$fixture/id_ed25519"
known_hosts="$fixture/known_hosts"
printf 'fixture-private-key-not-a-real-key\n' >"$identity"
printf 'fixture.test ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFixtureOnlyNoPrivateKey\n' >"$known_hosts"

cat >"$fixture/config.json" <<EOF
{"schema_version":1,"ssh":{"host":"fixture.test","port":2222,"username":"fixture_user","identity_file":"$identity","known_hosts_file":"$known_hosts"},"remote":{"command":"/usr/local/lib/choosh/choosh-host","socket_path":"/run/user/1000/chooshd.sock"}}
EOF

cat >"$fixture/fake-ssh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\0' "$@" >"$CHOOSH_TEST_SSH_ARGS"
python3 - <<'PY'
import json, os, struct, sys
for request_id in ("00000000-0000-0000-0000-000000000101", "00000000-0000-0000-0000-000000000102"):
    body = json.dumps({"kind":"response","id":request_id,"result":{"protocol":{"major":1,"minor":0},"daemon":{"name":"chooshd","version":"fixture"},"host":{"name":"fixture","version":"fixture"},"capabilities":[],"limits":{"max_control_frame_bytes":1048576,"max_in_flight_requests":64}}}, separators=(",", ":")).encode()
    sys.stdout.buffer.write(struct.pack(">I", len(body)) + body)
PY
EOF
chmod +x "$fixture/fake-ssh"

actual=$(SSH_BIN="$fixture/fake-ssh" CHOOSH_TEST_SSH_ARGS="$fixture/args" "$root/scripts/run-host-acceptance.sh" --config "$fixture/config.json")
test "$actual" = '{"schema_version":1,"ssh_host_key":"verified","stdio_rpc":"passed","private_socket_relay":"passed","requests":2}'

mapfile -d '' -t args <"$fixture/args"
expected=(
  -o BatchMode=yes -o StrictHostKeyChecking=yes -o "UserKnownHostsFile=$known_hosts"
  -o GlobalKnownHostsFile=/dev/null -o IdentitiesOnly=yes -i "$identity" -p 2222 -- fixture_user@fixture.test
  /usr/local/lib/choosh/choosh-host rpc --stdio --socket /run/user/1000/chooshd.sock
)
[[ "${args[*]}" == "${expected[*]}" ]]

bad=$(jq '.remote.socket_path = "/run/user/1000/socket;whoami"' "$fixture/config.json")
printf '%s\n' "$bad" >"$fixture/bad.json"
if SSH_BIN="$fixture/fake-ssh" CHOOSH_TEST_SSH_ARGS="$fixture/bad-args" "$root/scripts/run-host-acceptance.sh" --config "$fixture/bad.json" >/dev/null 2>&1; then
  echo 'unsafe remote socket path was accepted' >&2
  exit 1
fi
test ! -e "$fixture/bad-args"

echo 'host_acceptance_runner_fixture_passed'
