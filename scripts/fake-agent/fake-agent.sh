#!/usr/bin/env bash
# Fake, checked-in stand-in for a real coding-agent CLI (Claude Code, Codex,
# OpenCode, ...) running inside a devhost AgentTerminal/Shell item. It is a
# normal foreground program -- no special integration beyond being
# executable -- that prints a realistic sequence of "agent doing work"
# status lines, sleeping between them, and along the way prints two
# different kinds of "I need a human" moment so choosh-hostd's re-auth
# detection and input_required/hooks plumbing can be smoke-tested
# end-to-end without a real agent or real cloud credentials:
#
#   1. A `gh auth login`-shaped pattern-A device-code prompt, byte-for-byte
#      the same text as rust/choosh-hostd/src/auth_detect.rs's real,
#      captured `GH_PIPED_REAL` test fixture -- this is what exercises the
#      real passive PTY scanner (auth_detect.rs::detect_github), not an
#      approximation of it.
#   2. A plain "waiting for approval" moment. See the NOTE below the STEPS
#      array: this is NOT reproducing any real, discovered stdout-marker
#      convention, because no such convention exists to find -- see the
#      note for why, and treat this half as scaffolding, not a verified
#      fixture the way the gh block above is.
#
# Usage: just run it.
#   ./fake-agent.sh
#
# Env vars:
#   FAKE_AGENT_STEP_SECONDS   Fixed sleep (seconds) between every step, e.g.
#                             `FAKE_AGENT_STEP_SECONDS=0` for a fast smoke
#                             test. If unset, each step sleeps a random
#                             3-8s, which is what "realistic" pacing means
#                             here -- set this when you don't want to wait.
set -euo pipefail

log() {
  printf '[fake-agent] %s\n' "$1"
}

step_sleep() {
  local seconds
  if [[ -n "${FAKE_AGENT_STEP_SECONDS:-}" ]]; then
    seconds="$FAKE_AGENT_STEP_SECONDS"
  else
    seconds=$(( (RANDOM % 6) + 3 )) # 3-8 inclusive
  fi
  if [[ "$seconds" != "0" ]]; then
    sleep "$seconds"
  fi
}

log "Starting task: \"Add retry handling to the SSO device-code poller\""
step_sleep

log "Reading repository layout..."
step_sleep

log "Scanning rust/choosh-hostd/src for related modules..."
step_sleep

log "Drafting implementation plan..."
step_sleep

# --- Pattern A: gh auth login --web, piped/no-controlling-terminal shape.
# Byte-for-byte rust/choosh-hostd/src/auth_detect.rs's GH_PIPED_REAL test
# fixture (reread there before touching this block) -- this is the exact
# real capture the real detect_github() matcher fires on, not a lookalike.
log "Need to push a branch -- checking GitHub auth..."
printf '\n! First copy your one-time code: F1FE-DF1C\nOpen this URL to continue in your web browser: https://github.com/login/device\n'
log "(waiting for the device-code login to complete in a browser elsewhere...)"
step_sleep
log "GitHub authentication complete."
step_sleep

log "Applying changes to auth_detect.rs..."
step_sleep

log "Running local tests..."
step_sleep

# --- "Ask a human a question" moment.
#
# NOTE on verification status (read before trusting this block against
# anything real): docs/specs/agent-events.md's input_required event and
# rust/choosh-hostd/src/hooks.rs's emission path are NOT a stdout-marker
# convention an arbitrary CLI can print to signal "I need approval" --
# hooks.rs's normalize()/to_wire_event() are only ever reached by a *real*
# Claude Code/Codex/OpenCode process invoking its own agent-specific hook
# surface (Claude Code's PermissionRequest/Notification/... surfaces, per
# hooks.rs's CLAUDE_SURFACES and agent-events.md's adapter table), which in
# turn shells out to `choosh-hostd emit --surface <Surface>` with a JSON
# payload on stdin -- see hooks.rs's EmitInput/normalize(). There is no
# generic "print this exact string to stdout" trigger to reproduce, because
# the real mechanism isn't a stdout scan at all for this event (unlike
# pattern-A auth detection, which genuinely is one). This fake script has no
# hook wiring and is not one of the three named adapters, so it cannot
# faithfully trigger a real input_required event by printing anything.
# Below is therefore clearly-labeled plain text only, standing in for that
# moment -- flagged here explicitly per this task's instructions, rather
# than guessed at as if it were a verified shape. If a real stdout-marker
# convention for this ever gets added, this block should be updated to
# reproduce it exactly, the same way the gh block above does for pattern A.
log "STATUS: waiting_for_approval (SIMULATED -- see NOTE in this script's source for why this is plain text, not a real input_required trigger)"
log "Requesting permission to run: rm -rf build/stale-artifacts/"
step_sleep
log "Approval received (simulated). Continuing."
step_sleep

log "Finalizing changes..."
step_sleep

log "Task complete."
exit 0
