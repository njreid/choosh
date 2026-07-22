# Android native runtime callbacks

Status: Draft

## Purpose

This contract defines the Android/JNI outer composition root for one admitted
Choosh SSH attempt. It joins Android-owned socket and Keystore operations to
Rust's verified SSH transport without exposing a private key, a Java object
reference, a host path, or a shell command to shared/domain code.

## Ownership

For each connection generation, Android creates one constructor-injected
runtime registration and returns a non-zero opaque lease ID. The lease owns:

- one bounded socket connection;
- one payload-only Keystore signing callback; and
- separately typed opaque endpoint, username, known-host, credential, and
  public-key references.

Rust MAY retain only the opaque lease and its typed IDs. Android MUST release
the registration exactly once when the native plan is rejected, cancelled,
completed, or superseded. A late callback for a released lease MUST fail with
a stable content-free status and MUST NOT recreate the registration.

## Callback surface

The native callback ABI is versioned and accepts only:

- a non-zero lease ID;
- a byte array plus offset and length for one socket read or write; or
- a byte array plus offset and length for one SSH signing challenge.

Socket read/write and signing input lengths MUST be positive and bounded by
the registered limits. A callback MUST copy returned bytes before releasing a
JNI local reference. Socket callbacks MUST NOT permit the caller to select a
host, port, file descriptor, command, or channel type. The signing callback
MUST NOT accept a credential reference or public-key selector: its identity is
fixed when Android creates the lease.

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
injection. No mutable global registry or service locator is permitted.

Headless acceptance MUST prove all of the following with deterministic fakes:

1. each callback rejects zero, oversized, stale, and released lease inputs;
2. one lease releases its socket and signer exactly once on every terminal path;
3. a changed generated host key invokes no signer callback;
4. an exact generated host key invokes the signer only after admission; and
5. one admitted `git.status` request reaches the fixed SSH stdio relay and a
   negotiated private `chooshd` socket without exposing a host path to Android.
