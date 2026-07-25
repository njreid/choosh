#!/usr/bin/env bash
# Real, loopback-only OpenSSH/chooshd lane for the disposable-host runner.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
for command in cargo jq ssh sshd ssh-keygen python3 xxd; do
  command -v "$command" >/dev/null || { echo "host_acceptance_local_${command}_required" >&2; exit 69; }
done
# OpenSSH builds used by Linux distributions commonly require this root-owned
# privilege-separation directory even when the test server itself is unprivileged.
[[ -d /run/sshd && -x /run/sshd ]] || { echo 'host_acceptance_local_sshd_privsep_unavailable' >&2; exit 69; }

fixture="$(mktemp -d -t choosh-host-acceptance-local.XXXXXX)"
target_dir="${CARGO_TARGET_DIR:-$root/target}"
daemon_pid=''
sshd_pid=''
cleanup() {
  for pid in "$sshd_pid" "$daemon_pid"; do
    if [[ -n "$pid" ]]; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
  rm -rf -- "$fixture"
}
trap cleanup EXIT INT TERM

port=$(python3 - <<'PY'
import socket
sock = socket.socket()
sock.bind(("127.0.0.1", 0))
print(sock.getsockname()[1])
sock.close()
PY
)
username=$(id -un)
[[ "$username" =~ ^[A-Za-z_][A-Za-z0-9_-]{0,31}$ ]] || { echo 'host_acceptance_local_username_invalid' >&2; exit 65; }

cargo build --quiet -p chooshd -p choosh-host
chooshd="$target_dir/debug/chooshd"
choosh_host="$target_dir/debug/choosh-host"
[[ -x "$chooshd" && -x "$choosh_host" ]] || { echo 'host_acceptance_local_binaries_missing' >&2; exit 70; }

umask 077
ssh-keygen -q -t ed25519 -N '' -f "$fixture/host_key"
ssh-keygen -q -t ed25519 -N '' -f "$fixture/client_key"
cp "$fixture/client_key.pub" "$fixture/authorized_keys"
host_public=$(cut -d' ' -f1-2 "$fixture/host_key.pub")
printf '[127.0.0.1]:%s %s\n' "$port" "$host_public" >"$fixture/known_hosts"

cat >"$fixture/sshd_config" <<EOF
Port $port
ListenAddress 127.0.0.1
HostKey $fixture/host_key
PidFile $fixture/sshd.pid
AuthorizedKeysFile $fixture/authorized_keys
AllowUsers $username
PasswordAuthentication no
KbdInteractiveAuthentication no
ChallengeResponseAuthentication no
PermitRootLogin no
StrictModes no
PrintMotd no
LogLevel VERBOSE
EOF

state_dir="$fixture/state"
socket_path="$state_dir/chooshd.sock"
"$chooshd" --state-dir "$state_dir" --socket "$socket_path" &
daemon_pid=$!
for _ in $(seq 1 100); do
  [[ -S "$socket_path" ]] && break
  sleep 0.02
done
[[ -S "$socket_path" ]] || { echo 'host_acceptance_local_daemon_unavailable' >&2; exit 70; }

# Validate the generated server configuration before starting it, then keep its
# only listener on loopback. No account, key, known-host record, or daemon state
# escapes the fixture directory.
sshd -t -f "$fixture/sshd_config"
sshd -D -e -f "$fixture/sshd_config" >"$fixture/sshd.log" 2>&1 &
sshd_pid=$!
ready=false
for _ in $(seq 1 100); do
  ssh -o BatchMode=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile="$fixture/known_hosts" \
    -o GlobalKnownHostsFile=/dev/null -o ConnectTimeout=1 -i "$fixture/client_key" -p "$port" \
    "$username@127.0.0.1" true >/dev/null 2>&1 && { ready=true; break; }
  sleep 0.02
done
kill -0 "$sshd_pid" 2>/dev/null || { echo 'host_acceptance_local_sshd_unavailable' >&2; exit 70; }
"$ready" || { echo 'host_acceptance_local_ssh_login_unavailable' >&2; exit 70; }

cat >"$fixture/config.json" <<EOF
{"schema_version":1,"ssh":{"host":"127.0.0.1","port":$port,"username":"$username","identity_file":"$fixture/client_key","known_hosts_file":"$fixture/known_hosts"},"remote":{"command":"$choosh_host","socket_path":"$socket_path"}}
EOF
result="$($root/scripts/run-host-acceptance.sh --config "$fixture/config.json")"
test "$result" = '{"schema_version":1,"ssh_host_key":"verified","stdio_rpc":"passed","private_socket_relay":"passed","requests":2}'

# A different host key must make the runner fail before it can run the fixed
# remote relay. This is an OpenSSH check, not a generated-key simulation.
ssh-keygen -q -t ed25519 -N '' -f "$fixture/other_host_key"
other_public=$(cut -d' ' -f1-2 "$fixture/other_host_key.pub")
printf '[127.0.0.1]:%s %s\n' "$port" "$other_public" >"$fixture/changed_known_hosts"
cat >"$fixture/changed-config.json" <<EOF
{"schema_version":1,"ssh":{"host":"127.0.0.1","port":$port,"username":"$username","identity_file":"$fixture/client_key","known_hosts_file":"$fixture/changed_known_hosts"},"remote":{"command":"$choosh_host","socket_path":"$socket_path"}}
EOF
if "$root/scripts/run-host-acceptance.sh" --config "$fixture/changed-config.json" >"$fixture/changed.stdout" 2>"$fixture/changed.stderr"; then
  echo 'host_acceptance_local_changed_host_accepted' >&2
  exit 1
fi
test ! -s "$fixture/changed.stdout"
test "$(<"$fixture/changed.stderr")" = 'host_acceptance_ssh_or_rpc_failed'

echo 'host_acceptance_local_openssh_fixture_passed'
