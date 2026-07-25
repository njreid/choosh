#!/usr/bin/env bash
# Runs the disposable-host half of the M0-R6 vertical acceptance gate.
#
# This is intentionally a harness, not a deployment mechanism.  The target must
# already run chooshd under its per-user service manager; this script never
# starts a daemon through an SSH shell or accepts arbitrary remote shell text.
set -euo pipefail

usage() {
  cat <<'USAGE'
usage: scripts/run-host-acceptance.sh --config FILE

The versioned JSON config contains the local SSH identity/known-host files and
the fixed, already-deployed choosh-host executable plus its private Unix socket.
Set SSH_BIN to an absolute ssh-compatible executable only for a hermetic test.
USAGE
}

config=''
while (($#)); do
  case "$1" in
    --config)
      (($# >= 2)) || { usage >&2; exit 64; }
      config=$2
      shift 2
      ;;
    --help)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 64
      ;;
  esac
done

[[ -n "$config" && -f "$config" ]] || { echo 'host_acceptance_config_missing' >&2; exit 64; }
command -v jq >/dev/null || { echo 'host_acceptance_jq_required' >&2; exit 69; }

jq -e '
  type == "object" and (keys | sort) == ["remote", "schema_version", "ssh"] and
  .schema_version == 1 and
  (.ssh | type == "object" and (keys | sort) == ["host", "identity_file", "known_hosts_file", "port", "username"]) and
  (.remote | type == "object" and (keys | sort) == ["command", "socket_path"])
' "$config" >/dev/null || { echo 'host_acceptance_config_invalid' >&2; exit 65; }

host=$(jq -r '.ssh.host' "$config")
port=$(jq -r '.ssh.port' "$config")
username=$(jq -r '.ssh.username' "$config")
identity_file=$(jq -r '.ssh.identity_file' "$config")
known_hosts_file=$(jq -r '.ssh.known_hosts_file' "$config")
remote_command=$(jq -r '.remote.command' "$config")
socket_path=$(jq -r '.remote.socket_path' "$config")

[[ "$host" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]{0,251})?$ ]] || { echo 'host_acceptance_host_invalid' >&2; exit 65; }
[[ "$port" =~ ^[0-9]+$ ]] && ((port >= 1 && port <= 65535)) || { echo 'host_acceptance_port_invalid' >&2; exit 65; }
[[ "$username" =~ ^[A-Za-z_][A-Za-z0-9_-]{0,31}$ ]] || { echo 'host_acceptance_username_invalid' >&2; exit 65; }

# These values become remote-shell tokens in OpenSSH.  Restrict them to a
# single absolute, canonical-looking path so configuration cannot add argv or
# shell syntax.  Product protocol code never receives either path.
safe_remote_path() {
  [[ "$1" =~ ^/[A-Za-z0-9._/-]+$ ]] && [[ "$1" != *//* ]] && [[ "$1" != */../* ]] && [[ "$1" != */.. ]] && [[ "$1" != */./* ]] && [[ "$1" != */. ]]
}
safe_remote_path "$remote_command" || { echo 'host_acceptance_remote_command_invalid' >&2; exit 65; }
safe_remote_path "$socket_path" || { echo 'host_acceptance_socket_invalid' >&2; exit 65; }
[[ -f "$identity_file" && -r "$identity_file" ]] || { echo 'host_acceptance_identity_unavailable' >&2; exit 66; }
[[ -s "$known_hosts_file" && -r "$known_hosts_file" ]] || { echo 'host_acceptance_known_hosts_unavailable' >&2; exit 66; }

ssh_bin=${SSH_BIN:-ssh}
if [[ "$ssh_bin" == */* ]]; then
  [[ "$ssh_bin" = /* && -x "$ssh_bin" ]] || { echo 'host_acceptance_ssh_invalid' >&2; exit 66; }
else
  command -v "$ssh_bin" >/dev/null || { echo 'host_acceptance_ssh_unavailable' >&2; exit 69; }
fi

fixture=$(mktemp -d -t choosh-host-acceptance.XXXXXX)
cleanup() { rm -rf -- "$fixture"; }
trap cleanup EXIT INT TERM

request_one='00000000-0000-0000-0000-000000000101'
request_two='00000000-0000-0000-0000-000000000102'
describe_one="{\"kind\":\"request\",\"id\":\"$request_one\",\"method\":\"host.describe\",\"params\":{}}"
describe_two="{\"kind\":\"request\",\"id\":\"$request_two\",\"method\":\"host.describe\",\"params\":{}}"

{
  printf '%08x' "${#describe_one}" | xxd -r -p
  printf '%s' "$describe_one"
  printf '%08x' "${#describe_two}" | xxd -r -p
  printf '%s' "$describe_two"
} | "$ssh_bin" \
  -o BatchMode=yes \
  -o StrictHostKeyChecking=yes \
  -o UserKnownHostsFile="$known_hosts_file" \
  -o GlobalKnownHostsFile=/dev/null \
  -o IdentitiesOnly=yes \
  -i "$identity_file" \
  -p "$port" \
  -- "$username@$host" \
  "$remote_command" rpc --stdio --socket "$socket_path" \
  >"$fixture/response.frame" 2>"$fixture/ssh.stderr" || {
    echo 'host_acceptance_ssh_or_rpc_failed' >&2
    exit 70
  }

test ! -s "$fixture/ssh.stderr" || { echo 'host_acceptance_remote_stderr' >&2; exit 70; }

offset=0
extract_frame() {
  local output=$1 frame_hex_length frame_length
  frame_hex_length=$(dd if="$fixture/response.frame" bs=1 skip="$offset" count=4 status=none | xxd -p)
  [[ ${#frame_hex_length} -eq 8 ]] || return 1
  frame_length=$((16#$frame_hex_length))
  ((frame_length > 0 && frame_length <= 1048576)) || return 1
  dd if="$fixture/response.frame" bs=1 skip="$((offset + 4))" count="$frame_length" status=none >"$output"
  [[ $(wc -c <"$output") -eq $frame_length ]] || return 1
  offset=$((offset + 4 + frame_length))
}

extract_frame "$fixture/first.json" || { echo 'host_acceptance_frame_invalid' >&2; exit 70; }
extract_frame "$fixture/second.json" || { echo 'host_acceptance_frame_invalid' >&2; exit 70; }
[[ $offset -eq $(wc -c <"$fixture/response.frame") ]] || { echo 'host_acceptance_extra_frame_data' >&2; exit 70; }

assert_describe() {
  jq -e --arg id "$1" '
    type == "object" and (keys | sort) == ["id", "kind", "result"] and
    .kind == "response" and .id == $id and
    (.result | type == "object" and (keys | sort) == ["capabilities", "daemon", "host", "limits", "protocol"]) and
    .result.protocol == {"major": 1, "minor": 0} and
    .result.daemon.name == "chooshd" and
    (.result.daemon.version | type == "string" and length > 0) and
    (.result.host.name | type == "string" and length > 0) and
    (.result.host.version | type == "string" and length > 0) and
    .result.capabilities == [] and
    .result.limits == {"max_control_frame_bytes": 1048576, "max_in_flight_requests": 64}
  ' "$2" >/dev/null
}
assert_describe "$request_one" "$fixture/first.json" || { echo 'host_acceptance_first_response_invalid' >&2; exit 70; }
assert_describe "$request_two" "$fixture/second.json" || { echo 'host_acceptance_second_response_invalid' >&2; exit 70; }

# Do not expose target identity, paths, RPC contents, or SSH diagnostics in CI.
printf '%s\n' '{"schema_version":1,"ssh_host_key":"verified","stdio_rpc":"passed","private_socket_relay":"passed","requests":2}'
