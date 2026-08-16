#!/usr/bin/env bash
# `just deploy <ec2-instance-name-or-id> [region]` — builds choosh-relayd in
# release mode, ships it to a named EC2 instance, restarts the systemd unit,
# health-checks it, and rolls back automatically if the health check fails.
#
# Design (DESIGN.md §5 "Deployment"; mirrors the *shape* of choosh-hostd's
# self-update in docs/specs/host-deployment.md, not its transport):
#   1. cargo build --release the choosh-relayd binary locally.
#   2. Upload it to a scratch S3 bucket and mint a short-lived presigned GET
#      URL — this avoids granting the instance's IAM role any S3
#      permissions (checked: the devhost-ssm instance profile has none) and
#      avoids needing an SSH-reachable path (no open inbound port, no EC2
#      Instance Connect Endpoint required).
#   3. Run scripts/deploy-relayd-remote.sh on the instance via
#      `aws ssm send-command` (AWS Systems Manager Run Command — the same
#      "reach an instance with no open port" path referenced by
#      DESIGN.md/M8, using the SSM agent directly rather than the
#      njreid/devhost `ssm` Go tool's EC2-Instance-Connect-tunnel approach,
#      since Run Command needs no extra VPC endpoint and this account's SSM
#      Agent is already registered and online for the tested instance).
#      That script does the download-verify-chmod-rename swap, restarts the
#      systemd unit, health-checks GET /healthz, and rolls back to the
#      previous binary automatically if the health check fails.
#   4. This script's exit code reflects whether the *new* version is live:
#      0 only on CHOOSH_DEPLOY_RESULT=ok. Any rollback (successful or not)
#      is a non-zero exit — deploy failed — but a successful rollback means
#      the fleet is left on a working previous version, not disconnected.
#
# Bootstrap secret: choosh-relayd refuses all WebAuthn device registration
# (503) unless CHOOSH_RELAYD_BOOTSTRAP_SECRET is set in its environment, so
# every deploy must carry one through. This script resolves it (before the
# slow cargo build) from the CHOOSH_RELAYD_BOOTSTRAP_SECRET env var if set,
# else from SSM Parameter Store (/choosh/relayd/bootstrap-secret,
# SecureString) — and fails fast, with no build/upload attempted, if neither
# source produces a value. Unlike RP_ID/RP_ORIGIN this has no silent-omit
# fallback: a missing bootstrap secret is always an error, never an empty
# Environment= line.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
instance_arg="${1:?usage: deploy-relayd.sh <instance-id-or-name> [region] [s3-bucket]}"
region="${2:-us-east-1}"
bucket="${3:-${CHOOSH_DEPLOY_BUCKET:-choosh-relayd-deploy-323509301014}}"
bind_addr="${CHOOSH_RELAYD_BIND:-127.0.0.1:7443}"
health_timeout_s="${CHOOSH_DEPLOY_HEALTH_TIMEOUT_S:-20}"
# Both optional and empty by default: `choosh-relayd` itself already falls
# back to `DEFAULT_RP_ID`/`https://<rp_id>` (rust/choosh-relayd/src/lib.rs)
# when its own env vars are unset, so an empty substitution here (no
# `Environment=` line written into the unit at all) preserves that
# behavior exactly rather than this script needing its own second default.
rp_id="${CHOOSH_RELAYD_RP_ID:-}"
rp_origin="${CHOOSH_RELAYD_RP_ORIGIN:-}"
cargo_target_dir="${CARGO_TARGET_DIR:?CARGO_TARGET_DIR must be set (see repo build instructions)}"

log() { printf '[deploy] %s\n' "$*" >&2; }
fail() { printf '[deploy] FAILED: %s\n' "$*" >&2; exit 1; }

command -v aws >/dev/null || fail "aws CLI is required"

# Resolve the WebAuthn bootstrap secret before doing anything slow (build,
# upload) so a missing secret fails fast. choosh-relayd hard-requires this
# (503s all device registration without it) so — unlike RP_ID/RP_ORIGIN —
# there is no silent-omit fallback here: an unset env var AND a failed/empty
# SSM lookup is always a fatal error, not "write no Environment= line".
bootstrap_secret="${CHOOSH_RELAYD_BOOTSTRAP_SECRET:-}"
if [[ -z "$bootstrap_secret" ]]; then
  log "CHOOSH_RELAYD_BOOTSTRAP_SECRET not set; looking up /choosh/relayd/bootstrap-secret in SSM Parameter Store ($region)"
  # Check the command's actual exit code, not just its printed output: `aws`
  # can emit "None" or an error message on stdout/stderr depending on the
  # failure mode, and neither should be mistaken for a real secret value.
  if bootstrap_secret=$(aws ssm get-parameter --name /choosh/relayd/bootstrap-secret \
      --with-decryption --region "$region" --query Parameter.Value --output text 2>/dev/null); then
    [[ "$bootstrap_secret" != "None" ]] || bootstrap_secret=""
  else
    bootstrap_secret=""
  fi
fi
[[ -n "$bootstrap_secret" ]] || fail "no WebAuthn bootstrap secret available (choosh-relayd refuses all device registration without one): set CHOOSH_RELAYD_BOOTSTRAP_SECRET explicitly, or store one first with 'aws ssm put-parameter --name /choosh/relayd/bootstrap-secret --type SecureString --value <secret> --region $region'"
log "bootstrap secret resolved (value not logged)"

# Resolve instance-id-or-name to an instance ID.
if [[ "$instance_arg" =~ ^i-[0-9a-f]+$ ]]; then
  instance_id="$instance_arg"
else
  instance_id=$(aws ec2 describe-instances --region "$region" \
    --filters "Name=tag:Name,Values=$instance_arg" "Name=instance-state-name,Values=running" \
    --query 'Reservations[0].Instances[0].InstanceId' --output text 2>/dev/null || true)
  [[ -n "$instance_id" && "$instance_id" != "None" ]] || fail "no running instance named '$instance_arg' in $region"
fi
log "target instance: $instance_id ($region)"

ping_status=$(aws ssm describe-instance-information --region "$region" \
  --filters "Key=InstanceIds,Values=$instance_id" \
  --query 'InstanceInformationList[0].PingStatus' --output text 2>/dev/null || true)
[[ "$ping_status" == "Online" ]] || fail "instance $instance_id is not a registered/online SSM managed instance (got '$ping_status')"

log "building choosh-relayd --release (--locked)"
CARGO_TARGET_DIR="$cargo_target_dir" cargo build --release --locked -p choosh-relayd --manifest-path "$root/Cargo.toml"
binary="$cargo_target_dir/release/choosh-relayd"
[[ -f "$binary" ]] || fail "expected release binary not found at $binary"

digest=$(sha256sum "$binary" | cut -d' ' -f1)
log "built binary sha256=$digest size=$(stat -c%s "$binary") bytes"

key="deploy/$instance_id/choosh-relayd-$(date -u +%Y%m%dT%H%M%SZ)-${digest:0:12}"
log "uploading to s3://$bucket/$key"
aws s3 cp --only-show-errors "$binary" "s3://$bucket/$key"
presigned_url=$(aws s3 presign "s3://$bucket/$key" --expires-in 900 --region "$region")

remote_script=$(mktemp "${TMPDIR:-/tmp}/choosh-deploy-remote.XXXXXX.sh")
trap 'rm -f "$remote_script"' EXIT
# sed's replacement text treats unescaped `&`/`\` specially (`&` re-inserts the
# whole match), and a presigned S3 URL is full of `&` query separators — escape
# both before substituting so the URL survives intact.
sed_escape() { printf '%s' "$1" | sed -e 's/[\&|]/\\&/g'; }
sed \
  -e "s|CHOOSH_DEPLOY_PRESIGNED_URL|$(sed_escape "$presigned_url")|g" \
  -e "s|CHOOSH_DEPLOY_EXPECTED_SHA256|$(sed_escape "$digest")|g" \
  -e "s|CHOOSH_DEPLOY_BIND_ADDR|$(sed_escape "$bind_addr")|g" \
  -e "s|CHOOSH_DEPLOY_HEALTH_TIMEOUT_S|$(sed_escape "$health_timeout_s")|g" \
  -e "s|CHOOSH_DEPLOY_RP_ID|$(sed_escape "$rp_id")|g" \
  -e "s|CHOOSH_DEPLOY_RP_ORIGIN|$(sed_escape "$rp_origin")|g" \
  -e "s|CHOOSH_DEPLOY_BOOTSTRAP_SECRET|$(sed_escape "$bootstrap_secret")|g" \
  "$root/scripts/deploy-relayd-remote.sh" > "$remote_script"

b64=$(base64 -w0 "$remote_script")
command_doc='AWS-RunShellScript'
remote_command="echo $b64 | base64 -d > /tmp/choosh-deploy-remote.sh && bash /tmp/choosh-deploy-remote.sh; rc=\$?; rm -f /tmp/choosh-deploy-remote.sh; exit \$rc"

log "sending deploy command over SSM Run Command"
command_id=$(aws ssm send-command --region "$region" \
  --instance-ids "$instance_id" \
  --document-name "$command_doc" \
  --comment "choosh-relayd deploy sha256:${digest:0:12}" \
  --parameters "commands=[\"$remote_command\"]" \
  --timeout-seconds 300 \
  --query 'Command.CommandId' --output text)
log "SSM command id: $command_id"

status="Pending"
for _ in $(seq 1 90); do
  status=$(aws ssm get-command-invocation --region "$region" \
    --command-id "$command_id" --instance-id "$instance_id" \
    --query 'Status' --output text 2>/dev/null || echo Pending)
  case "$status" in
    Success|Failed|Cancelled|TimedOut) break ;;
  esac
  sleep 2
done

invocation=$(aws ssm get-command-invocation --region "$region" --command-id "$command_id" --instance-id "$instance_id")
stdout=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["StandardOutputContent"])' <<<"$invocation")
stderr=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["StandardErrorContent"])' <<<"$invocation")

printf '%s\n' "$stdout"
[[ -n "$stderr" ]] && printf '%s\n' "$stderr" >&2

result=$(grep -o 'CHOOSH_DEPLOY_RESULT=[a-z_]*' <<<"$stdout" | tail -1 | cut -d= -f2)

log "SSM command status=$status deploy_result=${result:-unknown}"

case "$result" in
  ok)
    log "deploy succeeded: choosh-relayd $digest is live and healthy on $instance_id"
    exit 0
    ;;
  rolled_back)
    fail "health check failed after deploy; automatic rollback restored the previous binary and it is healthy again — fleet is NOT disconnected, but this deploy did not ship"
    ;;
  rollback_impossible_no_previous)
    fail "health check failed on a first-ever deploy with no previous binary to roll back to — service was stopped rather than left in a broken state"
    ;;
  rollback_failed)
    fail "CRITICAL: health check failed AND rollback also failed to restore health — manual intervention required on $instance_id"
    ;;
  digest_mismatch)
    fail "downloaded binary digest did not match the uploaded artifact's digest — aborted before touching the running service"
    ;;
  *)
    fail "deploy did not report a recognized result (SSM status=$status); see output above"
    ;;
esac
