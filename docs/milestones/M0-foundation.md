# M0: Foundation and risk spikes

## Outcome
A reproducible skeleton proves every high-risk boundary before product work depends on it.

## Requirements
- **M0-R1:** Gradle app uses package/namespace `ai.choosh`, the pinned stable baseline in the [Android toolchain specification](../specs/android-toolchain.md), and a version catalog; Rust workspace contains Android core/bridge/web plus `chooshd` and `choosh-host`.
- **M0-R2:** CI builds Android arm64-v8a/x86_64 and both host targets.
- **M0-R3:** Sora `ContentChangeEvent` becomes a revisioned Rust edit without feedback loops.
- **M0-R4:** Kotlin/Rust bridge proves cancellation, typed errors, callbacks, and process recreation.
- **M0-R5:** SSH proves host-key verification plus concurrent PTY, exec, SFTP, and direct-tcpip channels.
- **M0-R6:** Framed hello/welcome RPC works through SSH stdio and a `0600` Unix socket.
- **M0-R7:** Terminal renderer benchmark covers VT/ANSI, IME, keyboard, resize, alternate screen, clipboard, and sustained output; record an ADR.
- **M0-R8:** Each agent produces normalized input-required and changed-file events without terminal parsing.
- **M0-R9:** Host versions produce a bounded Android-computed textual diff.
- **M0-R10:** Declared HTTP preview proves HTTP, WebSocket, SSE, and gateway authentication.
- **M0-R11:** GitHub Releases publishes a signed development APK detected by Obtainium.
- **M0-R12:** Decide and document `minSdk` from supported devices and required APIs; do not derive it from `compileSdk`.
- **M0-R13:** Prove Kotlin 2.4.10, AGP 9.2.1, Compose BOM 2026.06.01, Sora Editor, and the Kotlin/Rust bridge are mutually compatible. Any temporary stable-version exception follows the toolchain compatibility rule.
- **M0-R14:** CI builds the production API 36 target and runs a non-blocking Android 17/API 37 preview compatibility lane.

## Exit gate
A clean clone builds all targets from pinned dependencies on JDK 17; changed SSH keys fail closed; one connection carries PTY/SFTP/RPC/service traffic; all three agents emit fixtures; Obtainium detects an upgrade.

## Excluded
Polished navigation, production persistence, and public release support.
