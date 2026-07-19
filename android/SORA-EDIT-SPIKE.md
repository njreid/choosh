# Sora-to-Rust revisioned edit spike

Status: Blocked on a locally resolved Sora dependency/API fixture and licence approval.

## Evidence in this checkout

- `gradle/libs.versions.toml` has no Sora coordinate or version.
- `android/app/gradle.lockfile` and `gradle/verification-metadata.xml` contain no
  Sora artifact.
- The local Gradle cache contains no Rosemoe/Sora artifact from which the exact
  `ContentChangeEvent` API can be compiled or inspected.
- `rust/choosh-android-bridge` has no dependency on `choosh-core`. Its current C
  ABI intentionally accepts fixed-width integers only and documents that no
  pointer crosses the ABI.
- `choosh-core::text` already provides bounded UTF-16-to-UTF-8 conversion and
  generation-scoped projection suppression. `choosh-core::document` already
  provides revisioned, idempotent edits and stale-revision resync outcomes. No
  production composition currently connects those modules to Android.

Adding an unverified Sora coordinate or inventing event accessors would not be a
headlessly verifiable increment. Extending the current C ABI with borrowed Java
string pointers would also contradict its established safety boundary.

## Candidate audit (2026-07-18)

The current candidate is `io.github.rosemoe:editor:0.24.6`. Maven Central marks it
as the current `latest`/`release` version and its published Gradle metadata marks
the artifact as a release. The AAR declares `minSdk 21` and `minCompileSdk 36`, so
those dimensions are compatible with Choosh's `minSdk 26` and `compileSdk 37`.
The published AAR SHA-256 is
`fb76ae4db31d94d9fee7f97d9b8ec1c9659c54b5ee1ded7f9f95f7039646243e`.

This is a candidate, not an approval to ship. Upstream and the published POM
identify Sora as LGPL-2.1-or-later (the POM labels it LGPL 2.1), so its source,
notice, relinking, and distribution obligations require the provenance review
already required for M0. Its runtime graph includes AndroidX Collection 1.5.0,
Kotlin stdlib 2.3.10, and AndroidX Annotation 1.10.0; that graph must be locked
and verified on Choosh's Kotlin 2.4.10/AGP 9.3.0 baseline before it becomes a
production dependency.

The documented widget is `io.github.rosemoe.sora.widget.CodeEditor`, and its
lifecycle requires `release()` when the view is no longer used. The exact
`ContentChangeEvent` callback/API remains an evidence requirement: it must be
compiled from the resolved artifact rather than inferred from documentation.

Primary evidence:

- [Maven Central artifact metadata](https://central.sonatype.com/artifact/io.github.rosemoe/editor/0.24.6)
- [Sora Editor repository and LGPL notice](https://github.com/Rosemoe/sora-editor)
- [Sora Editor getting-started documentation](https://sora-editor.github.io/sora-editor-docs/guide/getting-started)

## Smallest supportable implementation after unblocking

The Android adapter should translate one verified `ContentChangeEvent` into this
dependency-neutral value before crossing the bridge:

```text
EditorEdit(
  change_id,
  generation,
  base_revision,
  start_utf16,
  end_utf16,
  replacement_utf8,
  optional_projection_token
)
```

The adapter must derive offsets from the exact pre-change projection associated
with `base_revision`; it must not infer deleted text from the post-change widget.
One event produces at most one command. A projection token produces zero local
commands and is consumed exactly once.

The Rust composition boundary should own a bounded document handle table. For one
command it must, in order:

1. reject a stale generation without mutation;
2. consume or reject a projection-suppression token;
3. translate the UTF-16 range against the current Rust snapshot;
4. submit the resulting byte edit with `change_id` and `base_revision`;
5. return one typed outcome: `applied(new_revision)`, `duplicate(revision)`,
   `resync_required(current_revision)`, or a stable validation error.

Strings should cross through a length-delimited JNI method or an owned bounded
byte-buffer API whose copy, UTF-8 validation, panic containment, and maximum sizes
are tested. The existing fixed-width lifecycle ABI should remain unchanged.

## Required headless fixture

Before production wiring, pin one stable Sora release in the version catalog,
lock it, and add dependency-verification metadata. Compile a JVM/Robolectric test
against that exact artifact which constructs or captures its real
`ContentChangeEvent` values and proves:

- insert, delete, replace, and non-BMP Unicode ranges map to expected UTF-8 edits;
- a valid event increments the Rust document revision exactly once;
- duplicate delivery is idempotent and changed reuse of an ID is rejected;
- stale revision/generation returns resync without mutation;
- programmatic projection callbacks cause zero Rust mutations;
- oversized replacement/range and split-surrogate input fail closed;
- event order and output are deterministic without wall-clock sleeps.

The spike is unblocked only when the selected Sora artifact, its licence/provenance,
and the actual event API are locally available and pass dependency verification.
