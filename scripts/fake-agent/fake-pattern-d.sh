#!/usr/bin/env bash
# Fake stand-in for `firebase login --no-localhost` -- pattern D ("resume
# via a fresh command, not stdin") per docs/specs/resources-and-reauth.md's
# provider survey, which captured the real firebase-tools 15.27.0 shape
# live. Reproduces that captured shape: a session ID line, a fake (but
# correctly-shaped) https://auth.firebase.tools/login?... URL, and the
# "Complete the login by running: firebase login <authorizationCode>"
# fallback instruction.
#
# Two invocations, mirroring the real CLI's two-process shape:
#
#   ./fake-pattern-d.sh              Initial prompt (like `firebase login
#                                     --no-localhost`). The real CLI keeps
#                                     polling in the background as a
#                                     fallback after printing this; this
#                                     fake genuinely polls too, for a
#                                     bounded time, watching for a local
#                                     temp-file sentinel that the resume
#                                     invocation below writes -- a design
#                                     choice (documented here, not implied)
#                                     over the simpler "print and exit"
#                                     alternative, because it means running
#                                     both invocations against each other is
#                                     an actual end-to-end exercise of "the
#                                     resume command unblocks the original
#                                     waiting process" rather than two
#                                     independent, unconnected exits. Exits
#                                     non-zero if the timeout elapses with
#                                     no sentinel.
#
#   ./fake-pattern-d.sh <code>       Resume invocation (like `firebase
#                                     login <authorizationCode>`). Exits 0
#                                     with a success message if <code> is
#                                     non-empty (also writing the sentinel
#                                     file so any concurrently-waiting
#                                     no-arg invocation unblocks), non-zero
#                                     otherwise.
#
# Env vars:
#   FAKE_PATTERN_D_SENTINEL         Path to the completion sentinel file.
#                                   Defaults to a fixed path under TMPDIR so
#                                   the two invocations find each other
#                                   without extra plumbing; override this if
#                                   running more than one fake-pattern-d
#                                   session concurrently on the same host,
#                                   since the default path is shared/global.
#   FAKE_PATTERN_D_TIMEOUT_SECONDS  How long the no-arg invocation polls
#                                   before giving up. Default 15. Set low
#                                   (e.g. 1-2) for a fast smoke test of the
#                                   "nothing resolves it" timeout path.
#
# Usage:
#   ./fake-pattern-d.sh                         # initial prompt + poll
#   ./fake-pattern-d.sh some-fake-code           # resume invocation
#   FAKE_PATTERN_D_TIMEOUT_SECONDS=2 ./fake-pattern-d.sh   # fast timeout test
set -uo pipefail

sentinel="${FAKE_PATTERN_D_SENTINEL:-${TMPDIR:-/tmp}/choosh-fake-pattern-d.sentinel}"

if [[ $# -ge 1 ]]; then
  code="$1"
  if [[ -z "$code" ]]; then
    printf 'fake-pattern-d: no authorization code given\n' >&2
    exit 1
  fi
  printf 'Waiting for authentication...\n\n'
  printf '\xe2\x9c\x94 Success! Logged in as fake-user@example.com (simulated)\n'
  printf 'resolved:%s\n' "$code" > "$sentinel"
  exit 0
fi

session_id="$(printf '%05X' "$((RANDOM % 1048576))")"

printf 'To sign in to the Firebase CLI:\n\n'
printf '1. Take note of your session ID:\n\n'
printf '   %s\n\n' "$session_id"
printf '2. Visit the URL below on any device and follow the instructions to get your code:\n\n'
printf '   https://auth.firebase.tools/login?code_challenge=fake-challenge-%s&session=%s&attest=fake-attest-%s\n\n' "$$" "$session_id" "$$"
printf '3. Complete the login by running:\n\n'
printf '   firebase login <authorizationCode>\n\n'
printf '[fake-pattern-d] For this fake script specifically, that means: %s <authorizationCode>\n' "$0"

rm -f -- "$sentinel" 2>/dev/null || true

timeout="${FAKE_PATTERN_D_TIMEOUT_SECONDS:-15}"
elapsed=0
while (( elapsed < timeout )); do
  if [[ -s "$sentinel" ]]; then
    printf '\n%s\n' "$(cat -- "$sentinel")"
    printf '\xe2\x9c\x94 Success! Logged in as fake-user@example.com (simulated, via resume command)\n'
    exit 0
  fi
  sleep 1
  elapsed=$((elapsed + 1))
done

printf '\nfake-pattern-d: timed out after %ss with no resume command received\n' "$timeout" >&2
exit 1
