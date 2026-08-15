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
	./scripts/check-android-sources.sh

release-check:
	./gradlew --no-configuration-cache :app:cyclonedxBom :app:generateReleaseLicenseReport

host-smoke:
	./scripts/test-zellij-smoke.sh

android-build:
	./gradlew :app:assembleDebug :app:assembleDebugAndroidTest

android-instrument: android-build
	./scripts/run-android-instrumentation.sh

release: check
	@echo 'release-readiness-verified'

# Builds the signed universal release APK, SBOM, and licence notices, then
# bundles them into build/release/ with the same names and evidence
# (checksum, signer.json) that CI publishes to a GitHub Release — for local
# verification without waiting on a CI run. Requires CHOOSH_KEYSTORE_FILE/
# CHOOSH_KEYSTORE_PASSWORD/CHOOSH_KEY_ALIAS/CHOOSH_KEY_PASSWORD and
# CHOOSH_VERSION_NAME/CHOOSH_VERSION_CODE to already be set in the environment.
release-bundle:
	./gradlew --no-configuration-cache :app:buildRustAndroid :app:assembleRelease :app:checkNativeAbiPackaging :app:cyclonedxBom :app:generateReleaseLicenseReport --stacktrace
	./scripts/collect-release-evidence.sh "$CHOOSH_VERSION_NAME"

# Ships choosh-relayd --release to an EC2 instance over SSM, restarts it,
# health-checks it, and rolls back automatically on a failed health check.
deploy INSTANCE REGION="us-east-1":
	CARGO_TARGET_DIR={{env_var_or_default("CARGO_TARGET_DIR", "/tmp/choosh-target")}} ./scripts/deploy-relayd.sh "{{INSTANCE}}" "{{REGION}}"
