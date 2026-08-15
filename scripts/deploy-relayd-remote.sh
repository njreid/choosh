#!/bin/bash
# Runs *on* the target instance (via `aws ssm send-command`), not locally.
# Template variables (CHOOSH_DEPLOY_*) are substituted by scripts/deploy-relayd.sh
# before this script is base64-shipped over SSM Run Command. See that script's
# header comment for the overall deploy/rollback design this implements.
#
# Mirrors the shape of choosh-hostd's self-update (docs/specs/host-deployment.md
# "Self-update"/"Rollback"): download-verify-chmod-rename, service-manager
# restart (not self-exec), health-check, and an automatic rollback to the
# previously-installed binary if the health check fails within a bounded
# window. `choosh-relayd` deploys are operator-triggered (`just deploy`), not
# self-pushed over the relay (DESIGN.md §5 "Deployment"), so this script is
# invoked by a human/CI job via SSM Run Command rather than by relayd itself.
set -euo pipefail

INSTALL_DIR="/opt/choosh-relayd"
BIN="$INSTALL_DIR/choosh-relayd"
BIN_NEW="$INSTALL_DIR/choosh-relayd.new"
BIN_PREV="$INSTALL_DIR/choosh-relayd.prev"
BIN_FAILED="$INSTALL_DIR/choosh-relayd.failed"
STATE_DIR="$INSTALL_DIR/state"
UNIT_PATH="/etc/systemd/system/choosh-relayd.service"
BIND_ADDR="CHOOSH_DEPLOY_BIND_ADDR"
HEALTH_URL="http://CHOOSH_DEPLOY_BIND_ADDR/healthz"
PRESIGNED_URL="CHOOSH_DEPLOY_PRESIGNED_URL"
EXPECTED_SHA256="CHOOSH_DEPLOY_EXPECTED_SHA256"
HEALTH_TIMEOUT_S=CHOOSH_DEPLOY_HEALTH_TIMEOUT_S
# Both empty by default (deploy-relayd.sh substitutes an empty string when
# its own CHOOSH_RELAYD_RP_ID/CHOOSH_RELAYD_RP_ORIGIN env vars are unset) —
# an empty value here means "write no Environment= line for it", which
# preserves choosh-relayd's own DEFAULT_RP_ID/derived-origin fallback
# (rust/choosh-relayd/src/lib.rs) rather than this script needing to know
# what that default is.
RP_ID="CHOOSH_DEPLOY_RP_ID"
RP_ORIGIN="CHOOSH_DEPLOY_RP_ORIGIN"

log() { printf '[deploy-relayd] %s\n' "$*"; }

mkdir -p "$INSTALL_DIR" "$STATE_DIR"

log "downloading candidate binary"
curl -fsSL --retry 3 -o "$BIN_NEW" "$PRESIGNED_URL"

actual_sha256=$(sha256sum "$BIN_NEW" | cut -d' ' -f1)
if [[ "$actual_sha256" != "$EXPECTED_SHA256" ]]; then
  log "DIGEST MISMATCH: expected $EXPECTED_SHA256 got $actual_sha256"
  rm -f "$BIN_NEW"
  echo "CHOOSH_DEPLOY_RESULT=digest_mismatch"
  exit 1
fi
chmod +x "$BIN_NEW"
log "digest verified: $actual_sha256"

# Built up separately (not inline in the heredoc below) so an empty
# RP_ID/RP_ORIGIN contributes no Environment= line at all, rather than one
# with an empty value — an explicit `Environment=CHOOSH_RELAYD_RP_ID=`
# would override choosh-relayd's own DEFAULT_RP_ID fallback with an empty
# string instead of leaving that fallback in effect.
extra_env=""
if [[ -n "$RP_ID" ]]; then
  extra_env+=$'\n'"Environment=CHOOSH_RELAYD_RP_ID=$RP_ID"
fi
if [[ -n "$RP_ORIGIN" ]]; then
  extra_env+=$'\n'"Environment=CHOOSH_RELAYD_RP_ORIGIN=$RP_ORIGIN"
fi

# Write/refresh the systemd unit idempotently. Root-run system-level unit:
# SSM Run Command executes as root on this box by default and choosh-relayd
# is the sole workload on this instance, so a dedicated unprivileged service
# user is a reasonable follow-up hardening step, not a correctness
# requirement for proving the deploy/rollback mechanism itself.
cat > "$UNIT_PATH" <<UNIT
[Unit]
Description=choosh-relayd
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=$BIN
WorkingDirectory=$INSTALL_DIR
Environment=CHOOSH_RELAYD_BIND=$BIND_ADDR
Environment=CHOOSH_RELAYD_STATE_DIR=$STATE_DIR$extra_env
Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload

had_previous=0
if [[ -f "$BIN" ]]; then
  had_previous=1
  cp -f "$BIN" "$BIN_PREV"
  log "backed up running binary to $BIN_PREV"
fi

# Atomic swap: same filesystem, so mv is rename(2).
mv -f "$BIN_NEW" "$BIN"
log "installed candidate binary at $BIN"

systemctl enable --now choosh-relayd.service >/dev/null 2>&1 || true
systemctl restart choosh-relayd.service
log "restarted choosh-relayd.service"

health_ok=0
deadline=$((SECONDS + HEALTH_TIMEOUT_S))
while (( SECONDS < deadline )); do
  if curl -fsS -m 2 "$HEALTH_URL" 2>/dev/null | grep -qx 'ok'; then
    health_ok=1
    break
  fi
  sleep 1
done

if (( health_ok == 1 )); then
  log "health check passed: $HEALTH_URL"
  rm -f "$BIN_PREV"
  echo "CHOOSH_DEPLOY_RESULT=ok"
  exit 0
fi

log "HEALTH CHECK FAILED after ${HEALTH_TIMEOUT_S}s — rolling back"
systemctl status choosh-relayd.service --no-pager -l || true
journalctl -u choosh-relayd.service --no-pager -n 40 || true
mv -f "$BIN" "$BIN_FAILED"

if (( had_previous == 0 )); then
  log "no previous binary to roll back to (this was the first-ever deploy) — stopping the unit"
  systemctl stop choosh-relayd.service || true
  echo "CHOOSH_DEPLOY_RESULT=rollback_impossible_no_previous"
  exit 1
fi

mv -f "$BIN_PREV" "$BIN"
systemctl restart choosh-relayd.service
rollback_health_ok=0
deadline=$((SECONDS + HEALTH_TIMEOUT_S))
while (( SECONDS < deadline )); do
  if curl -fsS -m 2 "$HEALTH_URL" 2>/dev/null | grep -qx 'ok'; then
    rollback_health_ok=1
    break
  fi
  sleep 1
done

if (( rollback_health_ok == 1 )); then
  log "rollback succeeded: previous binary is healthy again at $HEALTH_URL"
  echo "CHOOSH_DEPLOY_RESULT=rolled_back"
  exit 1
else
  log "ROLLBACK FAILED TOO — previous binary did not become healthy either"
  systemctl status choosh-relayd.service --no-pager -l || true
  echo "CHOOSH_DEPLOY_RESULT=rollback_failed"
  exit 1
fi
