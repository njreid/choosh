#!/usr/bin/env bash
# Deterministic, headless M6 release-readiness gate.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
"$root/scripts/check-specs.sh"
"$root/scripts/check-release-reproducibility.sh"
"$root/scripts/test-release-discovery.sh"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/choosh-target}" \
  cargo test -p chooshd --test upgrade_acceptance --offline
printf '%s\n' 'm6_release_readiness_headless_ok'
