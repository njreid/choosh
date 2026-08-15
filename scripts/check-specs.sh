#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

find "$root/protocol" -type f -name '*.json' -print0 |
  xargs -0 -r -n1 jq empty

for required in \
  "$root/README.md" \
  "$root/DESIGN.md" \
  "$root/PLAN.md" \
  "$root/docs/specs/README.md" \
  "$root/docs/security/relayd-threat-model.md" \
  "$root/protocol/v1/envelope.schema.json"
do
  test -s "$required"
done

"$root/scripts/check-terminal-provenance.sh"
"$root/scripts/check-ssh-admission-fixtures.sh"

echo "Specification syntax checks passed."
