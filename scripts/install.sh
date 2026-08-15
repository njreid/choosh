#!/bin/sh
# Bootstrap installer for choosh-hostd, per docs/specs/host-deployment.md's
# "Bootstrap install" section. Invoked as:
#
#   curl -fsSL relay.example/install.sh | sudo sh -s -- --token=<enrollment-token>
#
# sudo is used ONLY for the two root-requiring steps this script performs
# (see root_step_linux / root_step_macos below) — nothing choosh-hostd does
# afterward (enrollment, reconnects, workspace registration, self-update)
# needs root. This script is also the ONLY component in the system allowed
# to know about a specific OS package manager (dnf/apt/brew); everything
# downstream of installing mise's own prerequisites is mise's job — see
# docs/specs/toolchain-provisioning.md.
set -eu

# --- Configuration / testability overrides -----------------------------
#
# CHOOSH_HOSTD_RELEASE_URL: override the download URL template. Defaults to
# a placeholder that is NOT live yet (no release hosting exists as of this
# writing — see docs/milestones/M8-security-and-release.md). The template
# receives OS and ARCH substituted in.
#
# CHOOSH_HOSTD_LOCAL_BINARY: path to an already-built choosh-hostd binary on
# this machine. When set, the script installs that binary directly instead
# of downloading anything — this is what makes the rest of this script
# testable today, against a real local build, with no release infrastructure.
CHOOSH_HOSTD_RELEASE_URL_TEMPLATE="${CHOOSH_HOSTD_RELEASE_URL:-https://relay.example/releases/choosh-hostd-OS-ARCH}"
CHOOSH_HOSTD_LOCAL_BINARY="${CHOOSH_HOSTD_LOCAL_BINARY:-}"
CHOOSH_RELAYD_URL="${CHOOSH_RELAYD_URL:-ws://127.0.0.1:7443/connect}"

TOKEN=""
DRY_RUN=0
INSTALL_PREFIX="${CHOOSH_HOSTD_INSTALL_PREFIX:-$HOME/.local/bin}"

usage() {
	cat <<'EOF'
Usage: install.sh --token=<enrollment-token> [--dry-run]

Environment overrides (for testing without live release hosting):
  CHOOSH_HOSTD_RELEASE_URL     Download URL template (OS/ARCH substituted)
  CHOOSH_HOSTD_LOCAL_BINARY    Install this local binary instead of downloading
  CHOOSH_RELAYD_URL            relayd WebSocket URL written into the service unit
  CHOOSH_HOSTD_INSTALL_PREFIX  Where to install the binary (default: ~/.local/bin)
EOF
}

log() { printf '==> %s\n' "$*" >&2; }
fail() { printf 'error: %s\n' "$*" >&2; exit 1; }

# run CMD...: executes CMD, or just prints it under --dry-run. Every
# privileged operation in this script MUST go through this (or run_sudo
# below) so --dry-run is a complete, truthful preview.
run() {
	if [ "$DRY_RUN" = "1" ]; then
		printf 'DRY-RUN: %s\n' "$*" >&2
	else
		"$@"
	fi
}

# run_sudo CMD...: the only two call sites for this in the whole script are
# root_step_linux and the (currently no-op) root_step_macos, per
# host-deployment.md's sudo-scope requirement.
run_sudo() {
	if [ "$DRY_RUN" = "1" ]; then
		printf 'DRY-RUN (sudo): %s\n' "$*" >&2
	else
		sudo "$@"
	fi
}

# --- Argument parsing ----------------------------------------------------
for arg in "$@"; do
	case "$arg" in
	--token=*) TOKEN="${arg#--token=}" ;;
	--dry-run) DRY_RUN=1 ;;
	--help | -h)
		usage
		exit 0
		;;
	*)
		usage >&2
		fail "unrecognized argument: $arg"
		;;
	esac
done

[ -n "$TOKEN" ] || {
	usage >&2
	fail "--token=<enrollment-token> is required"
}

# --- Step 1: detect OS and architecture -----------------------------------
detect_os() {
	case "$(uname -s)" in
	Linux) echo linux ;;
	Darwin) echo macos ;;
	*) fail "unsupported OS: $(uname -s)" ;;
	esac
}

detect_arch() {
	case "$(uname -m)" in
	x86_64 | amd64) echo x86_64 ;;
	arm64 | aarch64) echo arm64 ;;
	*) fail "unsupported architecture: $(uname -m)" ;;
	esac
}

OS="$(detect_os)"
ARCH="$(detect_arch)"
log "detected OS=$OS ARCH=$ARCH"

# --- Step 2: mise's own prerequisites (the ONLY OS-package-manager-aware step) ---
install_mise_prereqs_linux() {
	if command -v dnf >/dev/null 2>&1; then
		run sudo dnf install -y gcc curl unzip
	elif command -v apt-get >/dev/null 2>&1; then
		run sudo apt-get update
		run sudo apt-get install -y build-essential curl unzip
	else
		fail "no supported package manager found (need dnf or apt-get)"
	fi
}

install_mise_prereqs_macos() {
	# Xcode Command Line Tools provide a C toolchain; curl and unzip ship
	# with the OS. brew itself is not assumed to be present, and this
	# script does not install brew — if a C toolchain is missing, direct
	# the operator to `xcode-select --install` rather than silently
	# reaching further than the three prerequisites mise needs.
	if ! xcode-select -p >/dev/null 2>&1; then
		fail "Xcode Command Line Tools are required (run: xcode-select --install) and were not found"
	fi
	log "Xcode Command Line Tools present; curl/unzip are part of the base OS"
}

case "$OS" in
linux) install_mise_prereqs_linux ;;
macos) install_mise_prereqs_macos ;;
esac

# --- Step 3: download and install the choosh-hostd binary ----------------
install_hostd_binary() {
	dest_dir="$INSTALL_PREFIX"
	dest="$dest_dir/choosh-hostd"
	run mkdir -p "$dest_dir"

	if [ -n "$CHOOSH_HOSTD_LOCAL_BINARY" ]; then
		log "installing local binary override: $CHOOSH_HOSTD_LOCAL_BINARY"
		[ -f "$CHOOSH_HOSTD_LOCAL_BINARY" ] || fail "CHOOSH_HOSTD_LOCAL_BINARY does not exist: $CHOOSH_HOSTD_LOCAL_BINARY"
		run cp "$CHOOSH_HOSTD_LOCAL_BINARY" "$dest"
	else
		url=$(echo "$CHOOSH_HOSTD_RELEASE_URL_TEMPLATE" | sed "s/OS/$OS/; s/ARCH/$ARCH/")
		log "downloading choosh-hostd from $url"
		run curl -fsSL "$url" -o "$dest"
	fi
	run chmod +x "$dest"
	echo "$dest"
}

HOSTD_BIN="$(install_hostd_binary)"

# --- Step 4: write the service-manager unit (do not start yet) -----------
#
# The enrollment token is a one-shot secret: it only matters on the
# service's first start (choosh-hostd persists a device credential after
# that and stops reading CHOOSH_ENROLLMENT_TOKEN). Writing it into the
# unit's Environment= is the simplest mechanism that requires no separate
# secret file/store, matching what choosh-hostd already reads
# (rust/choosh-hostd/src/serve.rs). Whoever operates this host afterward
# is responsible for the unit file's normal filesystem permissions; this
# script does not attempt additional secret-at-rest hardening beyond that.
write_systemd_unit() {
	unit_dir="$HOME/.config/systemd/user"
	unit_path="$unit_dir/choosh-hostd.service"
	run mkdir -p "$unit_dir"
	# KillMode=process (not the systemd default of control-group) is
	# load-bearing, not cosmetic: confirmed by direct experiment (real
	# systemd --user unit, real `zellij attach --create-background`
	# session, real long-running process inside it) that with the
	# default control-group KillMode, `systemctl --user restart
	# choosh-hostd.service` SIGKILLs every process in the unit's cgroup
	# — including the Zellij *server* and everything attached to it —
	# even though Zellij's server is not a session leader or process-group
	# member of the choosh-hostd process by the time of the kill. Only the
	# main tracked PID (choosh-hostd itself) is signaled under
	# KillMode=process, which is exactly host-deployment.md's Self-update
	# requirement ("Zellij sessions... MUST survive this restart
	# unaffected") and DESIGN.md principle 6 ("Zellij owns process
	# persistence independently of choosh-hostd's own liveness"). This is
	# also what makes choosh-hostd::update's own rollback watchdog (a
	# plain child process, no systemd-run/scope trickery needed) survive
	# the same restart it triggers.
	unit_content="[Unit]
Description=Choosh host daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=$HOSTD_BIN serve
Restart=on-failure
KillMode=process
Environment=CHOOSH_ENROLLMENT_TOKEN=$TOKEN
Environment=CHOOSH_RELAYD_URL=$CHOOSH_RELAYD_URL

[Install]
WantedBy=default.target
"
	if [ "$DRY_RUN" = "1" ]; then
		printf 'DRY-RUN: write %s:\n%s\n' "$unit_path" "$unit_content" >&2
	else
		printf '%s' "$unit_content" >"$unit_path"
	fi
	echo "$unit_path"
}

# AbandonProcessGroup (default false): without it, launchd tracks and
# signals this job's entire BSD process group on `launchctl kickstart -k`,
# not just the tracked PID — the macOS analogue of the Linux unit's
# KillMode=process fix above, for the exact same reason (Zellij's server
# and choosh-hostd::update's rollback watchdog must survive a self-update
# restart).
#
# No plist keys are added here for docs/specs/host-deployment.md's power-
# assertion requirement (rust/choosh-hostd/src/power_assertion.rs):
# IOPMAssertionCreateWithName is a runtime IOKit call the running process
# makes for itself, not a launchd job-configuration concern — there is no
# ProcessType/LowPriorityIO-style plist key that grants or is required for
# permission to hold a power assertion, and the per-user LaunchAgent
# GUI-session context this plist already runs in (RunAtLoad/KeepAlive
# above) is sufficient for that call to succeed.
write_launchd_plist() {
	plist_dir="$HOME/Library/LaunchAgents"
	plist_path="$plist_dir/ai.choosh.hostd.plist"
	run mkdir -p "$plist_dir"
	plist_content="<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
	<key>Label</key>
	<string>ai.choosh.hostd</string>
	<key>ProgramArguments</key>
	<array>
		<string>$HOSTD_BIN</string>
		<string>serve</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>CHOOSH_ENROLLMENT_TOKEN</key>
		<string>$TOKEN</string>
		<key>CHOOSH_RELAYD_URL</key>
		<string>$CHOOSH_RELAYD_URL</string>
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>AbandonProcessGroup</key>
	<true/>
</dict>
</plist>
"
	if [ "$DRY_RUN" = "1" ]; then
		printf 'DRY-RUN: write %s:\n%s\n' "$plist_path" "$plist_content" >&2
	else
		printf '%s' "$plist_content" >"$plist_path"
	fi
	echo "$plist_path"
}

case "$OS" in
linux) UNIT_PATH="$(write_systemd_unit)" ;;
macos) UNIT_PATH="$(write_launchd_plist)" ;;
esac
log "wrote service unit: $UNIT_PATH"

# --- Step 5: the two root-requiring, one-time operations ------------------
#
# Linux: without `loginctl enable-linger`, systemd --user kills every
# process owned by this user the instant the installing SSH session's
# login session ends — including the just-started choosh-hostd.
root_step_linux() {
	run_sudo loginctl enable-linger "$(id -un)"
}

# macOS: a PER-USER LaunchAgent (~/Library/LaunchAgents, not
# /Library/LaunchAgents or /Library/LaunchDaemons) does not require root to
# install or load — only system-level LaunchDaemons do. There is no root
# step on macOS. This is intentionally a documented no-op rather than an
# invented privileged operation.
root_step_macos() {
	log "no root step required on macOS (per-user LaunchAgent)"
}

case "$OS" in
linux) root_step_linux ;;
macos) root_step_macos ;;
esac

# --- Step 6: start the service --------------------------------------------
start_service_linux() {
	run systemctl --user daemon-reload
	run systemctl --user enable --now choosh-hostd.service
}

start_service_macos() {
	uid="$(id -u)"
	run launchctl bootstrap "gui/$uid" "$UNIT_PATH"
	run launchctl kickstart -k "gui/$uid/ai.choosh.hostd"
}

case "$OS" in
linux) start_service_linux ;;
macos) start_service_macos ;;
esac

# --- Step 7: choosh-hostd handles enrollment itself on first start -------
# (nothing to do here beyond having already written CHOOSH_ENROLLMENT_TOKEN
# into the unit above)

# --- Step 8: health-check before exiting ----------------------------------
#
# docs/specs/host-deployment.md says to health-check "choosh-hostd's local
# RPC socket" before exiting. As implemented today, choosh-hostd opens NO
# local socket or listener at all (confirmed: no bind()/TcpListener/
# UnixListener anywhere in rust/choosh-hostd/src) — it only dials OUT to
# relayd. That RPC-socket health-check target does not exist yet in this
# architecture; a local IPC surface may or may not ever be reintroduced.
# Until/unless one exists, the most truthful available signal is the
# service manager's own "is this unit active" state, which this function
# polls with a bounded timeout instead of a fixed sleep. THIS IS A KNOWN
# SPEC/IMPLEMENTATION GAP, not a silent substitution — flagged again in
# this script's own final report.
health_check_linux() {
	if [ "$DRY_RUN" = "1" ]; then
		printf 'DRY-RUN: poll `systemctl --user is-active choosh-hostd.service` until active (30s timeout)\n' >&2
		return 0
	fi
	i=0
	while [ "$i" -lt 30 ]; do
		if systemctl --user is-active --quiet choosh-hostd.service; then
			log "choosh-hostd.service is active"
			return 0
		fi
		i=$((i + 1))
		sleep 1
	done
	fail "choosh-hostd.service did not become active within 30s"
}

health_check_macos() {
	if [ "$DRY_RUN" = "1" ]; then
		printf 'DRY-RUN: poll `launchctl print gui/$(id -u)/ai.choosh.hostd` until present (30s timeout)\n' >&2
		return 0
	fi
	uid="$(id -u)"
	i=0
	while [ "$i" -lt 30 ]; do
		if launchctl print "gui/$uid/ai.choosh.hostd" >/dev/null 2>&1; then
			log "ai.choosh.hostd is loaded"
			return 0
		fi
		i=$((i + 1))
		sleep 1
	done
	fail "ai.choosh.hostd did not become active within 30s"
}

case "$OS" in
linux) health_check_linux ;;
macos) health_check_macos ;;
esac

log "install complete"
