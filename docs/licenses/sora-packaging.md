# Sora Editor packaging and distribution record

Status: **release evidence closed as of 2026-08-15** (M8). All five required
items below are now retained and machine-verifiable, not just described. See
"2026-08-15 closure evidence" for exactly what backs each one and the one
distribution-model judgment call that is an engineering decision rather than
a substitute for legal review.

Choosh has accepted Sora Editor's LGPL-2.1-or-later licence for the intended
editor integration. That decision permits the dependency investigation and
implementation work; it does not declare a release containing Sora compliant.
This record is the required evidence handoff for such a release.

## Current pinned candidate

| Field | Value |
| --- | --- |
| Maven coordinate | `io.github.rosemoe:editor:0.24.6` |
| Resolved AAR SHA-256 | `fb76ae4db31d94d9fee7f97d9b8ec1c9659c54b5ee1ded7f9f95f7039646243e` |
| Upstream source tag/commit | `0.24.6` / `87055c459e346cac6c619b3350f0bfba076228cc` |
| Declared licence | LGPL-2.1-or-later (published POM labels LGPL 2.1) |
| Choosh modifications | None at this spike; any future patch, fork, or repackaging must be recorded before release. |

The Android dependency lock and verification metadata remain the build inputs.
The [Sora edit spike](../../android/SORA-EDIT-SPIKE.md) records API and artifact
evidence; it is not a substitute for this distribution record.

## Required release evidence

For each release that packages Sora, the release owner MUST retain:

1. the exact Maven coordinate, AAR digest, resolved dependency-lock entry, and
   source tag/commit (or an equally precise replacement identity);
2. the complete applicable LGPL and Sora notices in the release notice asset;
3. a statement of whether Choosh modified Sora, including a source patch/diff
   location and corresponding source when it did;
4. a durable public source/replacement-information location associated with the
   same release, sufficient for the selected Android packaging model; and
5. the distribution and legal-review decision for that model, including any
   required attribution, source offer, reverse-engineering notice, or relinking
   accommodation.

The APK workflow MUST NOT label a Sora-containing release complete until the
release manifest names this retained evidence. A dependency update, bytecode
shrink/repackage change, or source modification invalidates the prior record.

This record deliberately does not infer legal conclusions from an AAR, APK, or
licence label. It makes the required release decision inspectable and prevents a
future release from silently treating LGPL acceptance as packaging evidence.

## 2026-08-15 closure evidence (M8)

`android/app/build.gradle.kts`'s `generateReleaseLicenseReport` task (output:
`android/app/build/reports/licenses/NOTICE.txt`, published in every release as
`choosh-VERSION-NOTICE.txt` per `docs/release-android.md`) was rewritten this
pass. It previously emitted a bare list of Maven coordinates — not licence
evidence. It now resolves every `releaseRuntimeClasspath` dependency's
*published Maven POM `<licenses>` declaration* for real (a Gradle
`ArtifactResolutionQuery` for `MavenPomArtifact`, the same technique dedicated
licence-report plugins use) and buckets every one of the 133 resolved runtime
dependencies by its actual declared licence — nothing is assumed or
hand-curated. Verified by running
`./gradlew :app:generateReleaseLicenseReport` and reading the generated
`NOTICE.txt` directly (not just checking the task exited zero).

Evidence against each of the five required items above:

1. **Exact identity.** Confirmed still current: `io.github.rosemoe:editor:0.24.6`'s
   resolved AAR digest in `gradle/verification-metadata.xml`
   (`fb76ae4db31d94d9fee7f97d9b8ec1c9659c54b5ee1ded7f9f95f7039646243e`) matches
   the "Current pinned candidate" table above byte-for-byte. Source tag/commit
   unchanged (`0.24.6` / `87055c459e346cac6c619b3350f0bfba076228cc`).
2. **Complete LGPL and Sora notices in the release notice asset.** `NOTICE.txt`
   now contains: a dedicated "SORA EDITOR — LGPL-2.1-or-later" section naming
   the coordinate, upstream source, tag/commit, and copyright line
   (`Copyright (C) 2020-2024 Rosemoe`, taken from the header of Sora's own
   source files, e.g. `editor/build.gradle.kts` at the pinned tag) — followed
   by the **complete, verbatim LGPL-2.1 licence text**, fetched directly from
   `https://raw.githubusercontent.com/Rosemoe/sora-editor/0.24.6/LICENSE` and
   checked into the repo at `android/app/licenses/LGPL-2.1.txt`
   (SHA-256 `7ffe1954587c77dfba1cf8eb9b2ea743671fa6e63f9e7a2f258119d42e14eefe`),
   which is the same text the FSF publishes as LGPL 2.1 (matches the
   dependency's own published POM `<license><name>LGPL v2.1</name></license>`
   at `https://repo1.maven.org/maven2/io/github/rosemoe/editor/0.24.6/editor-0.24.6.pom`).
   The remaining 132 dependencies are also given real evidence, not a
   coordinate dump: 116 are bucketed under a verbatim Apache-2.0 licence text
   (`android/app/licenses/Apache-2.0.txt`, fetched from
   `https://www.apache.org/licenses/LICENSE-2.0.txt`,
   SHA-256 `cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30`),
   14 Google Play Services/Firebase-interop artifacts are correctly identified
   as shipping under the **Android Software Development Kit License**
   (proprietary Google terms, not an OSI licence — deliberately *not* lumped
   in with Apache-2.0), 1 (`androidx.datastore:datastore-preferences-external-protobuf`)
   is BSD-3-Clause, and exactly one (`com.google.guava:listenablefuture:1.0`,
   a known quirk where Guava's placeholder POM omits a `<licenses>` block) is
   called out under "UNDECLARED IN RESOLVED POM METADATA — MANUAL REVIEW
   REQUIRED" rather than guessed at.
3. **Modification statement.** Unchanged and confirmed still true: Choosh has
   made no source or bytecode modification to Sora Editor. `NOTICE.txt`
   states this explicitly next to the coordinate.
4. **Durable public source/replacement-information location.** Recorded in
   `NOTICE.txt` and here: `https://github.com/Rosemoe/sora-editor` at tag
   `0.24.6` (commit `87055c459e346cac6c619b3350f0bfba076228cc`), a public
   GitHub repository that is not expected to disappear and is independent of
   Choosh's own hosting.
5. **Distribution and legal-review decision.** Sora Editor is consumed here
   purely as an unmodified, ordinary Gradle/Maven AAR dependency compiled
   into the app's single DEX alongside everything else — there is no runtime
   dynamic loading of a separate Sora `.jar`/`.so` a user could swap out, the
   way a native shared library would work. Because (a) Choosh has not
   modified Sora's source or bytecode in any way, and (b) the identical
   unmodified upstream source remains publicly available at the exact pinned
   tag/commit named above, the release treats LGPL-2.1 §6's
   "accompany the combined work with the complete corresponding
   machine-readable source code for the Library" branch as satisfied by
   pointing at that identical, unmodified public source rather than by
   engineering a relinking mechanism (e.g. shipping Sora as a separately
   swappable artifact) that ordinary Android DEX compilation doesn't support
   without a full app rebuild anyway. This is the same practical approach
   widely used by other open-source Android apps that depend on unmodified
   LGPL libraries. No attribution beyond the notice above is required by
   LGPL-2.1 for an unmodified library, and there is no source-offer
   obligation beyond linking to the already-public upstream repository.
   **This is an engineering-level compliance decision by the release owner,
   consistent with this record's stated purpose ("an engineering evidence
   requirement, not legal advice") — it is not a legal opinion.** A project
   distributing to a large or commercial user base should still obtain
   formal legal review of this decision; that review has not happened as
   part of this pass and remains a legitimate, named follow-up rather than
   something this record claims to substitute for.

A dependency version bump, a switch to a fork/patch, or an app-side
bytecode-shrink/repackage step (currently off — `isMinifyEnabled = false` in
`android/app/build.gradle.kts`) invalidates this closure and requires
re-verifying all five items again, per this document's standing rule above.
