#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
fixtures="$root/protocol/ssh-admission/v1/fixtures"

fail() {
  echo "ssh admission fixture check failed: $1" >&2
  exit 1
}

test -d "$fixtures" || fail "fixture directory is missing"

seen_ids=''
for fixture in "$fixtures"/*.json; do
  test -f "$fixture" || fail "no fixtures found"
  jq empty "$fixture" || fail "invalid JSON: $fixture"

  jq -e '
    type == "object" and
    keys == ["credential", "endpoint_id", "expected", "fixture_version", "id", "limits", "presented_fingerprint", "profile_id", "seed", "steps", "stored_fingerprint"] and
    .fixture_version == 1 and
    (.id | test("^[a-z][a-z0-9_-]*$") and length <= 64) and
    (.seed | test("^[a-z][a-z0-9-]*$") and length <= 64) and
    (.endpoint_id | test("^[a-z][a-z0-9-]*$") and length <= 64) and
    (.profile_id | test("^[a-z][a-z0-9-]*$") and length <= 64) and
    (.credential | test("^[a-z][a-z0-9-]*$") and length <= 64) and
    (.presented_fingerprint | test("^SHA256:fixture-[a-z0-9-]{1,40}$")) and
    (.stored_fingerprint == null or (.stored_fingerprint | test("^SHA256:fixture-[a-z0-9-]{1,40}$"))) and
    (.limits | type == "object" and keys == ["aggregate_bytes", "channels", "per_channel_bytes"] and
      (.channels | type == "number" and floor == . and . >= 1 and . <= 64) and
      (.per_channel_bytes | type == "number" and floor == . and . >= 1 and . <= 1048576) and
      (.aggregate_bytes | type == "number" and floor == . and . >= 1 and . <= 16777216) and
      (.aggregate_bytes >= .per_channel_bytes)) and
    (.expected | type == "object" and keys == ["ordered_events", "outcome", "prohibited_before_outcome"] and
      (.outcome | IN("ready", "trust_consent_required", "trust_rejected", "host_key_mismatch", "authentication_failed")) and
      (.ordered_events | type == "array" and length >= 2 and (length == (unique | length))) and
      (.prohibited_before_outcome | type == "array" and (length == (unique | length))) and
      ([.ordered_events[], .prohibited_before_outcome[]] | all(IN("transport_connected", "host_key_presented", "trust_consent_required", "trust_rejected", "trust_accepted_exact", "host_key_matched", "host_key_mismatch", "authentication_started", "authentication_succeeded", "authentication_failed", "ready", "channel_opened", "transport_closed"))) and
      ([.ordered_events[]] as $ordered | [.prohibited_before_outcome[]] | all(. as $event | ($ordered | index($event) | not)))) and
    (.steps | type == "array" and length <= 128 and
      all(.[]; type == "object" and keys == ["action", "at_ms", "ordinal"] and
        (.action | test("^[a-z][a-z0-9-]*$") and length <= 64) and
        (.at_ms | type == "number" and floor == . and . >= 0 and . <= 600000) and
        (.ordinal | type == "number" and floor == . and . >= 0 and . <= 1024)) and
      ([.[].ordinal] | length == (unique | length)) and
      (reduce .[] as $step ({time: -1, ordinal: -1, valid: true};
        .valid = (.valid and ($step.at_ms > .time or ($step.at_ms == .time and $step.ordinal > .ordinal))) |
        .time = $step.at_ms | .ordinal = $step.ordinal) | .valid))
  ' "$fixture" >/dev/null || fail "invalid fixture shape: $fixture"

  id=$(jq -r '.id' "$fixture")
  case " $seen_ids " in *" $id "*) fail "duplicate fixture ID: $id" ;; esac
  seen_ids="$seen_ids $id"

  case "$id" in
    unknown_key)
      outcome=trust_consent_required
      ordered='["transport_connected","host_key_presented","trust_consent_required","transport_closed"]'
      prohibited='["authentication_started","channel_opened"]'
      stored=null
      ;;
    unknown_key_rejected)
      outcome=trust_rejected
      ordered='["transport_connected","host_key_presented","trust_consent_required","trust_rejected","transport_closed"]'
      prohibited='["authentication_started","channel_opened"]'
      stored=null
      ;;
    unknown_key_accepted_exact)
      outcome=ready
      ordered='["transport_connected","host_key_presented","trust_consent_required","trust_accepted_exact","authentication_started","authentication_succeeded","ready"]'
      prohibited='["channel_opened"]'
      stored=null
      ;;
    stored_key_exact)
      outcome=ready
      ordered='["transport_connected","host_key_presented","host_key_matched","authentication_started","authentication_succeeded","ready"]'
      prohibited='["trust_consent_required","channel_opened"]'
      stored=equal
      ;;
    stored_key_changed)
      outcome=host_key_mismatch
      ordered='["transport_connected","host_key_presented","host_key_mismatch","transport_closed"]'
      prohibited='["trust_consent_required","authentication_started","channel_opened"]'
      stored=different
      ;;
    stored_key_exact_bad_credential)
      outcome=authentication_failed
      ordered='["transport_connected","host_key_presented","host_key_matched","authentication_started","authentication_failed","transport_closed"]'
      prohibited='["ready","channel_opened"]'
      stored=equal
      ;;
    *) fail "unrecognized fixture ID: $id" ;;
  esac

  jq -e --arg outcome "$outcome" --argjson ordered "$ordered" --argjson prohibited "$prohibited" --arg stored "$stored" '
    .expected.outcome == $outcome and
    .expected.ordered_events == $ordered and
    .expected.prohibited_before_outcome == $prohibited and
    (if $stored == "null" then .stored_fingerprint == null
     elif $stored == "equal" then .stored_fingerprint == .presented_fingerprint
     else .stored_fingerprint != .presented_fingerprint end)
  ' "$fixture" >/dev/null || fail "contract mismatch: $fixture"
done

test "$(printf '%s\n' "$seen_ids" | wc -w | tr -d ' ')" = 6 || fail "expected six trust fixtures"
echo "SSH admission fixture checks passed."
