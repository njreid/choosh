# Termux SSH-key import boundary

Status: domain-model and headless Android coordinator evidence; no SSH transport is
selected or implemented.

## Purpose

A user may use a dedicated SSH key generated with Termux to authenticate Choosh to a
host in a future transport adapter.  The private key is not a cross-application service
credential.  Choosh models only the result of an explicit, user-approved import:

```text
User-selected document
  -> Android credential adapter
  -> private Keystore-backed storage
  -> opaque credential reference + public algorithm + SHA-256 fingerprint
  -> Rust profile domain
```

No private-key document bytes, passphrase, document URI, Termux path, or Android account
identifier crosses into the Rust domain, serializable profile snapshot, logs, WebView, or
headless evidence.

## Required import behavior

1. The user creates or chooses a key deliberately and installs only its public half on
   the intended host.  A dedicated `ed25519` key is preferred over reusing a general
   purpose identity.
2. Choosh starts a user-mediated document-selection operation.  It does not enumerate or
   silently open Termux private storage and does not request broad filesystem access.
3. The Android adapter reads the chosen document, validates it locally, and immediately
   imports it into app-private storage protected by a non-exportable Keystore key.  It
   removes any temporary copy before reporting success.
4. The adapter derives and shows the public-key algorithm and canonical unpadded
   `SHA256:` fingerprint for confirmation.  It emits to Rust only a newly allocated
   opaque credential reference plus that non-secret metadata.
5. A replacement import is an explicit, atomic profile-binding change. Failed validation,
   cancelled consent, keystore failure, and profile-store failure leave the prior binding
   unchanged and use stable typed outcomes.

The importer must never accept a path or URI as a credential reference.  It must also
avoid reporting reference values in diagnostics.  The raw key is supplied only to a
future approved SSH adapter at authentication time, after exact known-host verification;
it is never passed to `chooshd`, a terminal, SFTP, Git, a loopback gateway, or a WebView.

## Headless domain evidence

`choosh_core::ssh_identity` supplies a dependency-free model for the non-secret result:

- `CredentialRef` is a bounded store-local handle and redacts its `Debug` output;
- `PublicKeyFingerprint` accepts only the canonical unpadded SHA-256 form;
- `PublicKeyMetadata` carries only algorithm and fingerprint; and
- `UserApprovedSshKeyImport` has no fields or constructors for raw private material,
  paths, URIs, or passphrases.

The Android application now has the corresponding constructor-injected
`SshKeyImportCoordinator` seam.  Its `ActivityResultDocumentPicker` abstraction is the
only entry point and its document reader and Keystore store receive opaque handles, not
paths or byte arrays. Its profile-store interface requires atomic replacement: failure
must retain the prior binding. A profile-binding failure discards the newly stored credential;
if cleanup fails it returns `CLEANUP_FAILED` rather than claiming success.  The concrete
`ACTION_OPEN_DOCUMENT` and Android Keystore adapters remain outer-composition work and
must not add storage permissions or Termux-path access.

`AndroidOpenDocumentPicker` is now the concrete outer picker adapter. It builds an
`ACTION_OPEN_DOCUMENT`, `CATEGORY_OPENABLE`, read-grant-only request and delegates
registered Activity Result wiring through constructor injection. A successful Android
result is reduced to a package-private opaque document handle; only a future document
reader can obtain its URI. It neither persists URI access nor reads a document itself.

Run the deterministic proof with:

```sh
cargo test -p choosh-core ssh_identity
GRADLE_USER_HOME=/tmp/choosh-gradle ANDROID_HOME=/opt/android-sdk ./gradlew :app:testDebugUnitTest
```

The negative cases reject filesystem paths, document URIs, private-key marker text,
control characters, malformed fingerprints, accidental debug disclosure of the opaque
handle, cancelled picks, invalid documents, and profile-binding rollback.

## Deferred integration gate

This evidence does not make SSH login functional.  The transport selection and the
shared SSH acceptance harness remain required before any credential reference is used to
authenticate a connection.  That future harness must prove host-key verification occurs
before authentication and must retain no raw credential material or opaque reference in
its evidence.  See [SSH acceptance harness](m0-ssh-acceptance-harness.md) and [SSH
transport choice](m0-ssh-transport-choice.md).
