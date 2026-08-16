#!/usr/bin/env bash
# Fake stand-in for a static-secret-paste CLI -- pattern C ("static secret
# paste, no browser flow at all") per docs/specs/resources-and-reauth.md's
# provider survey (the `aws configure` / Twilio CLI category). Scope-trimmed
# for this pass to a SINGLE prompt, deliberately -- a real `aws configure`
# asks four sequential questions (Access Key ID, Secret Access Key, region,
# output format); this fake only needs to exercise the "no URL, no code,
# just a value the human already has" shape choosh-hostd's pattern-C
# managed-subprocess path needs to smoke-test against, not clone the real
# CLI's full prompt sequence. A multi-field variant is a reasonable,
# separate follow-up if the real detector/runner ever needs one.
#
# Prints "Enter your fake API key: " with no trailing newline, reads one
# line from stdin (plain `read`, correct whether stdin is a real terminal
# or piped -- pattern C, like B, is always run by choosh-hostd as a managed
# subprocess with piped stdin, never PTY-scanned).
#
# Exit 0 and a success message on a non-empty line; exit 1 on EOF/empty.
#
# Usage:
#   ./fake-pattern-c.sh                       # interactive
#   echo "fake-api-key-abc123" | ./fake-pattern-c.sh
#   ./fake-pattern-c.sh </dev/null            # immediate EOF -> exit 1
set -uo pipefail

printf 'Enter your fake API key: '

if IFS= read -r key; then
  if [[ -n "$key" ]]; then
    printf '\nAPI key accepted (simulated).\n'
    exit 0
  fi
fi

printf '\nfake-pattern-c: no API key received (empty input or EOF)\n' >&2
exit 1
