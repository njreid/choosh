# Android native runtime callbacks

Status: Draft

## Purpose

This contract defines the Android/JNI outer composition root for one admitted
Choosh SSH attempt. It joins Android-owned socket and Keystore operations to
Rust's verified SSH transport without exposing a private key, a host path, or
a shell command to shared/domain code. Java callback objects are confined to
the bridge allocation described in [ADR 0008](../adr/0008-jni-runtime-callback-ownership.md).

## Ownership

For each connection generation, Android creates one constructor-injected
runtime registration and returns a non-zero opaque lease ID. The lease owns:

- one bounded socket connection;
- one payload-only Keystore signing callback; and
- separately typed opaque endpoint, username, known-host, credential, and
  public-key references.

Rust transport MAY retain only a plan-owned capability lease and typed IDs. The
JNI bridge MAY retain the per-plan callback object only for that plan's native
allocation. Android MUST release the registration exactly once when the native
plan is rejected, cancelled, completed, or superseded. A late callback for a
released lease MUST fail with a stable content-free status and MUST NOT
recreate the registration.

Opaque IDs alone are insufficient to open SSH: the outer adapter also needs
validated endpoint, username, exact-host, public-key, and credential metadata.
That adapter MUST resolve those values from the Android-owned registration,
validate them at the JNI boundary, and pass only typed stream, exact-host
session, and signer capabilities to shared transport. It MUST NOT make shared
transport dereference an opaque ID or call back into a mutable global registry.

## Callback surface

The native callback ABI is versioned and accepts only:

- a non-zero lease ID;
- one bounded, versioned non-secret identity capsule containing canonical username, the exact
  persisted host fingerprint, and public-key algorithm/fingerprint metadata;
- one bounded canonical OpenSSH public key bound to that public-key fingerprint;
- a byte array plus offset and length for one socket read or write; or
- a byte array plus offset and length for one SSH signing challenge.

Socket read/write and signing input lengths MUST be positive and bounded by
the registered limits. A callback MUST copy returned bytes before releasing a
JNI local reference. Socket callbacks MUST NOT permit the caller to select a
host, port, file descriptor, command, or channel type. The signing callback
MUST NOT accept a credential reference or public-key selector: its identity is
fixed when Android creates the lease.

The metadata capsule MUST NOT contain an endpoint, path, command, credential
reference, private key, or signature. Rust MUST reject unknown versions, empty
fields, trailing bytes, invalid usernames, invalid fingerprints, unknown
algorithms, malformed public keys, or public-key fingerprint mismatches before
constructing the exact-host session or signer capability.

Rust MUST call the signing callback only from the credential signer that is
created after exact host-key admission. Unknown, changed, rejected, and
unverified host keys MUST cause no signing callback invocation.

## Failures and cancellation

The ABI returns only stable status classes: invalid argument, stale lease,
unavailable, bounded I/O failure, signing failure, and cancelled. It MUST NOT
return endpoint text, key aliases, provider detail, socket exceptions, path
text, or payload bytes.

Cancellation is idempotent. It invalidates the lease before closing its socket
and rejects future read, write, or signing callbacks. Rust converts callback
failure into the existing typed transport/authentication failure; it does not
retry a host-key or signing failure as a network retry.

## Composition and verification

Concrete JNI environment access is confined to `choosh-android-bridge`; shared
transport code receives narrow injected stream and signer capabilities. The
Android app assembles the runtime in its composition root using constructor
injection. No mutable global registry or service locator is permitted. The JNI
bridge is therefore the dependency-direction outer root: it may depend on the
transport composition crate, but the transport crate must not depend on JNI.
The v3 runtime overload retains its Java callback object in the plan-token-owned
bridge allocation and releases it on cancellation or generation recreation. It
remains fail-closed until that allocation is composed into the verified stream
and signer transport.

`BoundedAndroidNativeRuntime` is the Android-side constructor-injected owner for
this callback object. It opens the already validated endpoint, binds the fixed
identity capsule and canonical public key, and binds a payload-only signer
before returning one `NativeLease`. It has no static registry; rejecting lease
construction closes the socket, and lease close closes it once.

Headless acceptance MUST prove all of the following with deterministic fakes:

1. each callback rejects zero, oversized, stale, and released lease inputs;
2. one lease releases its socket and signer exactly once on every terminal path;
3. a changed generated host key invokes no signer callback;
4. an exact generated host key invokes the signer only after admission; and
5. one admitted `git.status` request reaches the fixed SSH stdio relay and a
   negotiated private `chooshd` socket without exposing a host path to Android.
