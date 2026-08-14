# Android native runtime callbacks

Status: Draft

## Purpose

This contract defines the Android/JNI outer composition root for one
admitted Choosh relay connection attempt. It joins Android-owned socket and
Keystore operations to Rust's relay client transport
([choosh-android-transport](../../DESIGN.md#13-target-repository-layout),
per DESIGN.md §13) without exposing a private key, a workspace path, or a
command to shared/domain code. Java callback objects are confined to a
single bridge allocation, the same discipline the superseded SSH-era
contract used — only the admission precondition changed: it used to be
"exact host-key match," it's now "the `phone` Identity's session credential
was accepted by `relayd`" (see [auth-and-enrollment.md](auth-and-enrollment.md)).

## Ownership

For each connection generation, Android creates one constructor-injected
runtime registration and returns a non-zero opaque lease ID. The lease
owns:

- one bounded WebSocket connection to `choosh-relayd`;
- one payload-only Keystore signing callback, used only during WebAuthn
  assertion/registration and session-credential renewal — never for
  per-message signing;
- separately typed opaque endpoint, session-credential, and passkey
  reference IDs.

Rust transport MAY retain only a plan-owned capability lease and typed
IDs. The JNI bridge MAY retain the per-plan callback object only for that
plan's native allocation. Android MUST release the registration exactly
once when the native plan is rejected, cancelled, completed, or
superseded. A late callback for a released lease MUST fail with a stable
content-free status and MUST NOT recreate the registration.

Opaque IDs alone are insufficient to open a relay connection: the outer
adapter also needs a validated `relayd` endpoint and a session-credential
(or, for first registration, a WebAuthn assertion) capability. That
adapter MUST resolve those values from the Android-owned registration,
validate them at the JNI boundary, and pass only typed stream and
credential capabilities to shared transport. It MUST NOT make shared
transport dereference an opaque ID or call back into a mutable global
registry.

## Callback surface

The native callback ABI is versioned and accepts only:

- a non-zero lease ID;
- one bounded, versioned non-secret identity capsule containing the
  `relayd` endpoint and the Identity class (`phone`) fixed for this app;
- a byte array plus offset and length for one WebSocket read or write; or
- a byte array plus offset and length for one WebAuthn/Keystore signing
  challenge.

Socket read/write and signing input lengths MUST be positive and bounded
by the registered limits. A callback MUST copy returned bytes before
releasing a JNI local reference. Socket callbacks MUST NOT permit the
caller to select an endpoint, tunnel target, or command. The signing
callback MUST NOT accept an arbitrary credential reference: its identity
is fixed when Android creates the lease and it MUST only be invoked for
the WebAuthn ceremony or session-credential renewal described in
[auth-and-enrollment.md](auth-and-enrollment.md).

The combined signing result MUST fit the callback's bounded frame limit;
implementations MUST size that limit from the actual WebAuthn assertion
signature format in use, not assume a fixed algorithm.

The metadata capsule MUST NOT contain a workspace path, command, private
key, or raw signature. Rust MUST reject unknown versions, empty fields,
trailing bytes, or a malformed identity capsule before constructing the
relay client session or signer capability.

Rust MUST call the signing callback only from the credential flow that is
active after the relay's TLS handshake completes — never before a live
connection to `relayd` exists.

## Failures and cancellation

The ABI returns only stable status classes: invalid argument, stale
lease, unavailable, bounded I/O failure, signing failure, and cancelled.
It MUST NOT return endpoint text, key aliases, provider detail, socket
exceptions, path text, or payload bytes.

Cancellation is idempotent. It invalidates the lease before closing its
socket and rejects future read, write, or signing callbacks. Rust
converts callback failure into the existing typed transport/authentication
failure; it does not retry a credential-rejection failure as a network
retry — a rejected session credential MUST force the re-auth path in
[auth-and-enrollment.md](auth-and-enrollment.md), not a reconnect loop.

## Composition and verification

Concrete JNI environment access is confined to `choosh-android-bridge`;
shared transport code receives narrow injected stream and signer
capabilities. The Android app assembles the runtime in its composition
root using constructor injection. No mutable global registry or service
locator is permitted. The JNI bridge is therefore the dependency-direction
outer root: it may depend on the transport composition crate, but the
transport crate must not depend on JNI.

The runtime retains its Java callback object in the plan-token-owned
bridge allocation and releases it on cancellation or generation
recreation. The JNI outer root consumes that allocation into the verified
stream and signer transport; it reports success only after the relay
WebSocket handshake and session-credential acceptance have completed and
the resulting connection is retained under that same plan token.

`BoundedAndroidNativeRuntime` is the Android-side constructor-injected
owner for this callback object. It opens the already-validated `relayd`
endpoint and binds a payload-only signer before returning one
`NativeLease`. It has no static registry; rejecting lease construction
closes the socket, and lease close closes it once. Read and write
callbacks are independent after lease validation; a blocked socket read
MUST NOT serialize or prevent a bounded write.

`choosh-android-transport` adapts the lease's blocking read and write
capabilities through separately scheduled bounded operations. It MUST NOT
run an Android socket callback on the async transport's own worker
threads. Each callback failure maps to a content-free I/O failure and
does not retry credential acceptance.

On successful connection, `Plan` MUST transfer its token and Android lease
to one `SessionLease`. The connection-completion path MUST NOT close the
plan after that transfer. The session owner is then responsible for
exactly one native cancellation and Android runtime release when the
relay connection is closed or fails.

Headless acceptance MUST prove all of the following with deterministic
fakes:

1. each callback rejects zero, oversized, stale, and released lease
   inputs;
2. one lease releases its socket and signer exactly once on every
   terminal path;
3. a rejected session credential invokes no further signing callback and
   surfaces the typed re-auth failure, not a retry;
4. an accepted session credential composes the relay client session only
   after that acceptance; and
5. one admitted request reaches `relayd` and a real `open-tunnel`
   round-trip completes without exposing a workspace path or credential
   material to Android application code.
