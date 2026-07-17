#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture="$(mktemp -d -t choosh-rpc-socket.XXXXXX)"
state_dir="$fixture/state"
socket_path="$state_dir/chooshd.sock"
daemon_pid=''

cleanup() {
  if [[ -n "$daemon_pid" ]]; then
    kill "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
  fi
  rm -rf -- "$fixture"
}
trap cleanup EXIT INT TERM

cd "$root"
cargo build --quiet -p chooshd -p choosh-host
"$root/target/debug/chooshd" --state-dir "$state_dir" --socket "$socket_path" &
daemon_pid=$!

for _ in $(seq 1 200); do
  if [[ -S "$socket_path" ]]; then
    break
  fi
done
test -S "$socket_path"
test "$(stat -c '%a' "$state_dir")" = '700'
test "$(stat -c '%a' "$socket_path")" = '600'

response="$({ printf '\0\0\0\6health'; } | "$root/target/debug/choosh-host" rpc --stdio --socket "$socket_path" | od -An -tx1 | tr -d ' \n')"
test "$response" = '000000076865616c746879'

echo 'rpc_socket_process_test_passed'
