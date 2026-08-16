#!/usr/bin/env bash
# Fake stand-in for `gcloud auth login --no-launch-browser` -- pattern B
# ("manual code paste-back") per docs/specs/resources-and-reauth.md's
# provider survey. Prints a realistic (fake, but correctly-shaped)
# accounts.google.com OAuth URL, then blocks on a real interactive prompt
# in the *same* process, exactly like the real CLI does: "Enter
# authorization code: " with no trailing newline. Per this project's design
# decision, pattern B is always run by choosh-hostd itself as a managed
# child process with piped stdin/stdout/stderr, so this script only needs
# to behave correctly as an ordinary blocking interactive CLI -- it reads
# one line from its real stdin (fd 0), which is correct whether stdin is a
# real terminal or a pipe (`echo "code" | ./fake-pattern-b.sh`); no
# /dev/tty special-casing needed or wanted, since a managed subprocess with
# piped stdin has no /dev/tty of its own to read from anyway.
#
# Exit 0 and a success message on a non-empty line; exit 1 on EOF/empty.
#
# Usage:
#   ./fake-pattern-b.sh                    # interactive
#   echo "fake-code-123" | ./fake-pattern-b.sh
#   ./fake-pattern-b.sh </dev/null         # immediate EOF -> exit 1
set -uo pipefail

printf 'Go to the following link in your browser:\n\n'
printf '    https://accounts.google.com/o/oauth2/auth?response_type=code&client_id=FAKE-CLIENT-ID.apps.googleusercontent.com&redirect_uri=urn%%3Aietf%%3Awg%%3Aoauth%%3A2.0%%3Aoob&scope=openid+https%%3A%%2F%%2Fwww.googleapis.com%%2Fauth%%2Fuserinfo.email&state=fake-state-%s\n\n' "$$"
printf 'Enter authorization code: '

if IFS= read -r code; then
  if [[ -n "$code" ]]; then
    printf '\nYou are now authenticated (simulated).\n'
    exit 0
  fi
fi

printf '\nfake-pattern-b: no code received (empty input or EOF)\n' >&2
exit 1
