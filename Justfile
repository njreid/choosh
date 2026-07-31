# Deterministic entry points for build, verification, and release preparation.

set shell := ["bash", "-euo", "pipefail", "-c"]

default: check

# Mirrors the pre-device CI gates so a local pass predicts a CI pass.
check: specs rust clippy android-check release-check

specs:
	./scripts/check-specs.sh

rust:
	CARGO_TARGET_DIR={{env_var_or_default("CARGO_TARGET_DIR", "/tmp/choosh-target")}} cargo test --workspace --offline

clippy:
	CARGO_TARGET_DIR={{env_var_or_default("CARGO_TARGET_DIR", "/tmp/choosh-target")}} cargo clippy --workspace --all-targets --offline -- -D warnings

android-check:
	./scripts/check-android.sh

release-check:
	./scripts/check-m6-release-readiness.sh

host-smoke:
	./scripts/test-rpc-socket.sh
	./scripts/test-zellij-smoke.sh

android-build:
	./gradlew :app:assembleDebug :app:assembleDebugAndroidTest

android-instrument: android-build
	./scripts/run-android-instrumentation.sh

release: check
	@echo 'release-readiness-verified'
