# Sora-to-Rust revisioned edit spike

Status: Event API and project licence acceptance verified; blocked on the headless
adapter fixture and release distribution evidence.

## Evidence in this checkout

- `gradle/libs.versions.toml` pins `io.github.rosemoe:editor:0.24.6`.
- The resolved `editor-0.24.6.aar` has SHA-256
  `fb76ae4db31d94d9fee7f97d9b8ec1c9659c54b5ee1ded7f9f95f7039646243e`; its
  `ContentChangeEvent.class` API matches upstream tag `0.24.6`, commit
  `87055c459e346cac6c619b3350f0bfba076228cc`.
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
those dimensions are compatible with Choosh's `minSdk 26` and `compileSdk 36`.
The published AAR SHA-256 is
`fb76ae4db31d94d9fee7f97d9b8ec1c9659c54b5ee1ded7f9f95f7039646243e`.

Upstream and the published POM identify Sora as LGPL-2.1-or-later (the POM labels
it LGPL 2.1). The project has accepted that licence for this integration; its
source, notice, and distribution obligations remain release gates described in
[the Sora packaging record](../docs/licenses/sora-packaging.md). Its runtime
graph includes AndroidX Collection 1.5.0, Kotlin stdlib 2.3.10, and AndroidX
Annotation 1.10.0; that graph must be locked and verified on Choosh's Kotlin
2.4.10/AGP 9.3.0 baseline before it becomes a production dependency.

The documented widget is `io.github.rosemoe.sora.widget.CodeEditor`, and its
lifecycle requires `release()` when the view is no longer used.

### Verified `ContentChangeEvent` contract

The resolved class exposes action values `ACTION_SET_NEW_TEXT = 1`,
`ACTION_INSERT = 2`, and `ACTION_DELETE = 3`, plus `changeStart`, `changeEnd`,
`changedText`, and `causedByUndoManager`. The exact tagged source documents the
[event fields and changed-text meanings](https://github.com/Rosemoe/sora-editor/blob/0.24.6/editor/src/main/java/io/github/rosemoe/sora/event/ContentChangeEvent.java):

- `ACTION_INSERT`: `changeStart.index` is the insertion point,
  `changeEnd.index` is immediately after the inserted text, and `changedText` is
  the inserted text. The editor computes both positions after insertion.
- `ACTION_DELETE`: start/end describe the half-open region in the pre-delete
  projection and `changedText` is the deleted text. After deletion, the editor
  recomputes `start` in the remaining content and reconstructs
  `end.index = start.index + deletedText.length()`; therefore adapters must not
  try to slice the post-delete widget using that end index. See the exact
  [`afterInsert`/`afterDelete` dispatch](https://github.com/Rosemoe/sora-editor/blob/0.24.6/editor/src/main/java/io/github/rosemoe/sora/widget/CodeEditor.java#L5253-L5339).
- `ACTION_SET_NEW_TEXT`: `CodeEditor.setText(...)` always dispatches one event
  with start at BOF, end at the end of the new content, `changedText` equal to
  the new `Content`, and `causedByUndoManager = false`; it is a full projection,
  not an incremental replacement. See the tagged
  [`setText` implementation](https://github.com/Rosemoe/sora-editor/blob/0.24.6/editor/src/main/java/io/github/rosemoe/sora/widget/CodeEditor.java#L3951-L4019).

`CharPosition.index` and `column` count Java `char`/`CharSequence` positions,
so they are UTF-16 code-unit offsets, not Unicode scalar-value or UTF-8 byte
offsets. Newline separators contribute their Java `CharSequence.length()` to
the global index. Non-BMP characters therefore occupy two units and the adapter
must reject a boundary that splits a surrogate pair.

Callbacks are not user-only. Programmatic `setText(...)` emits
`ACTION_SET_NEW_TEXT`, and programmatic mutation through
`editor.getText().insert/delete/replace` reaches the editor's content listener
and emits insert/delete events. A replace is explicitly implemented as delete
then insert and consequently produces two events; `beforeReplace` only announces
that pairing. The only exposed cause bit distinguishes undo/redo, not user input
from programmatic projection. See upstream [`Content.replace`](https://github.com/Rosemoe/sora-editor/blob/0.24.6/editor/src/main/java/io/github/rosemoe/sora/text/Content.java#L567-L600)
and [`ContentListener`](https://github.com/Rosemoe/sora-editor/blob/0.24.6/editor/src/main/java/io/github/rosemoe/sora/text/ContentListener.java#L35-L70).

Consequently, projection suppression cannot rely on the action or undo flag. It
must be an adapter-owned token/generation around programmatic mutations, and it
must account for replace producing a delete/insert pair.

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

The spike is unblocked when the verified event API passes the dependency and
adapter fixture.
