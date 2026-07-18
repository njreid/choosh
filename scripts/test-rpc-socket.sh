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

hello='{"kind":"hello","protocol":{"major":1,"minor":0},"client":{"name":"rpc-process-gate","version":"1"},"capabilities":[]}'
hello_length=${#hello}
response_frame="$fixture/response.frame"
host_stderr="$fixture/host.stderr"
{
  printf '%08x' "$hello_length" | xxd -r -p
  printf '%s' "$hello"
} | "$root/target/debug/choosh-host" rpc --stdio --socket "$socket_path" \
  >"$response_frame" 2>"$host_stderr"

test ! -s "$host_stderr"
test "$(wc -c <"$response_frame")" -ge 5
response_hex_length=$(xxd -p -l 4 "$response_frame")
response_length=$((16#$response_hex_length))
test "$response_length" -gt 0
test "$response_length" -le 1048576
test "$(wc -c <"$response_frame")" -eq "$((response_length + 4))"
tail -c +5 "$response_frame" >"$fixture/welcome.json"

jq -e '
  type == "object" and
  (keys | sort) == (["capabilities", "daemon", "host", "kind", "limits", "protocol"] | sort) and
  .kind == "welcome" and
  .protocol == {"major": 1, "minor": 0} and
  .daemon.name == "chooshd" and
  (.daemon.version | type == "string" and length > 0) and
  .host == {"name": "local-host", "version": "unknown"} and
  .capabilities == [] and
  .limits == {"max_control_frame_bytes": 1048576, "max_in_flight_requests": 64}
' "$fixture/welcome.json" >/dev/null

kill "$daemon_pid"
wait "$daemon_pid" 2>/dev/null || true
daemon_pid=''

echo 'rpc_typed_hello_welcome_process_test_passed'
