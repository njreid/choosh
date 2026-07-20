# Sora Editor packaging and distribution record

Status: **accepted dependency; release evidence blocked**.

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
