# ADR 0008: Per-plan JNI runtime callback ownership

## Status

Accepted for the M0 Android native connector.

## Context

The JNI plan ABI identifies Android-owned socket and Keystore callback
registrations with an opaque runtime-lease handle. The next connector step must
invoke bounded read, write, and signing callbacks from Rust while the SSH task
may outlive the Java call that created the plan. Storing callback objects in a
process-global mutable registry would obscure ownership, survive a plan's
cancellation incorrectly, and violate the explicit composition requirement.

## Decision

The Android/JNI composition root will pass one runtime callback object only to
`choosh-android-bridge` when it creates an authenticated plan. The bridge will
retain a JNI global reference inside that plan's native allocation, not in a
global callback registry. The plan allocation owns the reference until exactly
one terminal transition: rejection, cancellation, completion, or process
generation invalidation.

The callback object has only bounded `read`, `write`, `sign`, and `close`
operations. Each operation includes the opaque runtime-lease ID and a byte
array slice. It has no method for selecting an endpoint, host path, command,
credential, public key, or channel type. The bridge validates every lease,
offset, length, and status before constructing a shared transport result.

Worker threads attach to the Java VM only for the duration of a callback and
delete local references before returning. `JNIEnv` and JNI object references
remain confined to `choosh-android-bridge`; `choosh-android-transport`,
`choosh-ssh`, and domain crates receive only injected stream and signer
capabilities. Signing is reachable only from the signer created after exact
host-key admission.

## Consequences

- The current v3 ABI carries the opaque lease ID but does not yet retain or
  invoke the callback object. It MUST remain fail-closed until the per-plan
  native allocation and bounded JNI adapter are implemented.
- The implementation requires deterministic fake-JNI tests for released,
  stale, oversized, and cross-lease callbacks before a device connection may
  report `CONNECTED`.
- A Java object crosses only the JNI outer composition boundary; no Java object
  or credential material enters shared/domain code.
