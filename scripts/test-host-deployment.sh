#!/usr/bin/env bash
set -euo pipefail

# Deterministic host-owned deployment transaction gate.  This deliberately
# exercises only injected fakes: no SSH connection, service manager, host path,
# or daemon is touched by the test.
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
target_dir=${CARGO_TARGET_DIR:-/tmp/choosh-target}

cargo test --offline --manifest-path "$root/Cargo.toml" \
  -p choosh-host deployment --target-dir "$target_dir"
printf '%s\n' host_deployment_headless_ok
