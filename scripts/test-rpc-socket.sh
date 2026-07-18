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
request_id='00000000-0000-0000-0000-000000000001'
describe="{\"kind\":\"request\",\"id\":\"$request_id\",\"method\":\"host.describe\",\"params\":{}}"
response_frame="$fixture/response.frame"
host_stderr="$fixture/host.stderr"
{
  printf '%08x' "${#hello}" | xxd -r -p
  printf '%s' "$hello"
  printf '%08x' "${#describe}" | xxd -r -p
  printf '%s' "$describe"
} | "$root/target/debug/choosh-host" rpc --stdio --socket "$socket_path" \
  >"$response_frame" 2>"$host_stderr"

test ! -s "$host_stderr"
offset=0
extract_frame() {
  output=$1
  frame_hex_length=$(dd if="$response_frame" bs=1 skip="$offset" count=4 status=none | xxd -p)
  test "${#frame_hex_length}" -eq 8
  frame_length=$((16#$frame_hex_length))
  test "$frame_length" -gt 0
  test "$frame_length" -le 1048576
  dd if="$response_frame" bs=1 skip="$((offset + 4))" count="$frame_length" status=none >"$output"
  test "$(wc -c <"$output")" -eq "$frame_length"
  offset=$((offset + 4 + frame_length))
}

extract_frame "$fixture/welcome.json"
extract_frame "$fixture/describe.json"
test "$offset" -eq "$(wc -c <"$response_frame")"

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

jq -e --arg id "$request_id" '
  type == "object" and
  (keys | sort) == (["id", "kind", "result"] | sort) and
  .kind == "response" and
  .id == $id and
  (.result | keys | sort) == (["capabilities", "daemon", "host", "limits", "protocol"] | sort) and
  .result.protocol == {"major": 1, "minor": 0} and
  .result.daemon.name == "chooshd" and
  (.result.daemon.version | type == "string" and length > 0) and
  .result.host == {"name": "local-host", "version": "unknown"} and
  .result.capabilities == [] and
  .result.limits == {"max_control_frame_bytes": 1048576, "max_in_flight_requests": 64}
' "$fixture/describe.json" >/dev/null

kill "$daemon_pid"
wait "$daemon_pid" 2>/dev/null || true
daemon_pid=''

echo 'rpc_typed_hello_welcome_host_describe_process_test_passed'
