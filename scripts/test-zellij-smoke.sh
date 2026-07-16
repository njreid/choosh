#!/usr/bin/env bash
set -euo pipefail

command -v zellij >/dev/null
command -v jq >/dev/null

root="$(mktemp -d -t choosh-zellij-smoke.XXXXXX)"
session="choosh-smoke-${$}-$(basename "$root")"
config="$root/config"
data="$root/data"
mkdir -p "$config" "$data"

export ZELLIJ_CONFIG_DIR="$config"
export ZELLIJ_DATA_DIR="$data"

cleanup() {
  zellij delete-session "$session" --force >/dev/null 2>&1 || true
  rm -rf -- "$root"
}
trap cleanup EXIT INT TERM

if zellij list-sessions --short --no-formatting 2>/dev/null | awk -v name="$session" '$0 == name { found = 1 } END { exit !found }'; then
  echo 'zellij_smoke_session_collision' >&2
  exit 1
fi

zellij attach --create-background "$session" options --default-cwd "$root"
zellij list-sessions --short --no-formatting | awk -v name="$session" '$0 == name { found = 1 } END { exit !found }'

tab_id="$(zellij --session "$session" action new-tab --name choosh-agent --cwd "$root" -- /bin/cat)"
case "$tab_id" in
  ''|*[!0-9]*) echo 'zellij_smoke_invalid_tab_id' >&2; exit 1 ;;
esac

panes="$(zellij --session "$session" action list-panes --json --all --command --state --tab)"
pane_number="$(jq -er --argjson tab "$tab_id" '.[] | select(.tab_id == $tab and .is_plugin == false) | .id' <<<"$panes")"
pane_id="terminal_${pane_number}"

marker="choosh-marker-${$}"
zellij --session "$session" action write-chars --pane-id "$pane_id" "$marker"
zellij --session "$session" action send-keys --pane-id "$pane_id" Enter
zellij --session "$session" action dump-screen --pane-id "$pane_id" --full | grep -F -- "$marker" >/dev/null

zellij --session "$session" action close-pane --pane-id "$pane_id"
panes="$(zellij --session "$session" action list-panes --json)"
if jq -e --argjson pane "$pane_number" '.[] | select(.id == $pane and .is_plugin == false)' <<<"$panes" >/dev/null; then
  echo 'zellij_smoke_pane_survived_close' >&2
  exit 1
fi

echo 'zellij_smoke_passed'
