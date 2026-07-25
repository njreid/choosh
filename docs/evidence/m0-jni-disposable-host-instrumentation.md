# M0 JNI disposable-host instrumentation gate

Status: required before M0-R5/M0-R6 can be called complete.

## Why this is an instrumentation gate

The current bridge has a real plan-owned JNI callback allocation, bounded
Android socket adapter, exact-host Russh admission path, payload-only signer,
and fixed-RPC actor. Separately,
[`test-host-acceptance-local-openssh.sh`](../../scripts/test-host-acceptance-local-openssh.sh)
proves a disposable loopback OpenSSH host reaches a real private `chooshd`
socket through the fixed `choosh-host rpc --stdio` command.

Those proofs cannot be joined by an Android JVM unit test without replacing the
thing being proven. The unit-test runtime does not load the packaged Android
ABI library, does not provide Android Keystore, and cannot establish that a
`GlobalRef` callback survives an Android JNI/native worker-thread transition.
A host-built JNI library or a Java fake callback would test a different
binary/runtime and is not M0-R5/R6 evidence.

## Required setup

The instrumentation runner MUST receive a pre-started disposable SSH endpoint
and its exact generated host key through test-only runner configuration. The
endpoint is reachable from the device or emulator; it MUST NOT be selected from
an Android RPC, shell command, URI, or path. The disposable host has:

- one generated host key and one generated client public key;
- a loopback/private `chooshd` Unix socket and fixed deployed `choosh-host`;
- no remote shell setup, uploads, account creation, or TCP listener from
  `chooshd`; and
- a bounded lifetime with cleanup owned by the runner.

The test-only `DisposableHostInstrumentationComposition` accepts exactly four
runner arguments: `choosh.fixture.host`, `.port`, `.username`, and
`.host_fingerprint`. It parses them into a fixed test profile and builds the
real native connector only from constructor-injected Android runtime and
Keystore capabilities. This avoids production profile UI, static test state,
and any endpoint/path/command selection from the app protocol.

The Android test creates a test-only Keystore credential through the Android
platform API, imports only its public OpenSSH identity into the runtime lease,
and injects the resulting `BoundedAndroidNativeRuntime` at the composition
root. Private-key bytes, aliases, endpoint text, and host paths do not enter
the bridge assertion output.

The test-only `DisposableHostKeystoreIdentity` makes this a two-phase runner:
the first instrumentation invocation creates/reuses a non-exportable Android
Keystore Ed25519 alias and reports only its public `authorized_keys` line to
the disposable-host provisioner. The provisioner then starts OpenSSH with that
generated public identity. The connection invocation reports only redacted
outcome categories; neither invocation emits private-key bytes or the alias.
The existing `SmokeInstrumentation` exposes this as a strict
`choosh.mode=key-bootstrap` invocation; it emits the public line under
`fixture_authorized_key` and otherwise reports only a success/unavailable
category.

The headless command
`scripts/bootstrap-disposable-host-android.sh` installs both debug APKs,
invokes that mode, validates the `ssh-ed25519` public-key grammar, and prints
only the public line for the fixture provisioner. Missing ADB/device state is
reported as `android_bootstrap_device_unavailable` with exit 69.

## Required assertions

One test run MUST prove all of the following:

1. the installed APK loads its packaged ABI and opens a real
   `JniPlanBridge` session through `BoundedAndroidNativeRuntime`;
2. exact generated host-key admission succeeds and the Keystore callback signs
   only after admission;
3. one bounded `git.status` envelope reaches the fixed SSH stdio command and a
   real private `chooshd` socket, then returns a terminal bounded response;
4. replacing the generated known-host entry causes connection failure before
   the signing callback; and
5. cancellation closes the Android socket/lease once while the host daemon
   remains alive for a new admitted connection.

The runner records only outcome categories, protocol/version identities,
bounded byte counts, and cleanup status. It MUST redact endpoint, paths,
credential aliases, request bodies, signatures, and Java/SSH exception text.

## Deterministic prerequisites

The gate is blocked until a runner provides all of: an Android API 26+ device
or emulator with a working ABI, device-reachable disposable OpenSSH endpoint,
Android Keystore availability, and a configured `chooshd` fixture. Missing
prerequisites produce a stable skipped/preflight result, never a pass. The
existing local OpenSSH script remains the reproducible host preflight; it is
not a substitute for this instrumentation result.
