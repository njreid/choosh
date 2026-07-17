# Terminal provenance and licence readiness

Status: **blocked** for importing or distributing the Zelland-derived terminal stack.

This record is the offline M0-R15 audit as of 2026-07-17. It distinguishes evidence present in
this repository from assumptions about upstream projects. A local copy or sibling repository is
not authoritative upstream provenance.

## Readiness matrix

| Component | Repository evidence | Distribution status | Required evidence before use |
| --- | --- | --- | --- |
| Zelland renderer and Android bridge | The terminal specification pins reference links to Zelland commit `8bf9cf55911588451804a39526f8ae869da021b6`; no Zelland source is present in Choosh. | **Blocked.** No ownership statement or licence text is recorded here. | Establish copyright ownership or obtain an explicit licence covering the exact referenced source and preserve its notice obligations. |
| `libghostty-vt` | Named by the terminal specification; absent from `Cargo.lock` and workspace manifests. | **Blocked for import.** No pinned release, source digest, licence file, or transitive inventory exists. | Pin the exact source/release and digest; record copyright, licence, notices, and all enabled-feature transitives before copying or resolving it. |
| `wgpu` | Named by the terminal specification; absent from `Cargo.lock` and workspace manifests. | **Blocked for import.** Version and native backend set are undecided, so the relevant transitive obligations cannot yet be enumerated. | Pin mutually compatible versions/features/targets and audit their resolved Cargo licence and native backend inventory. |
| `glyphon` | Named by the terminal specification; absent from `Cargo.lock` and workspace manifests. | **Blocked for import.** Version, source digest, licence, and font/shaping transitives are not recorded. | Pin the release/source and audit the complete resolved dependency graph and notices. |
| Iosevka Charon Mono regular and bold | Three exact Android resource hashes are recorded below. An OFL 1.1 text naming the Iosevka Project authors is packaged in the app. | **Artifact integrity and packaged licence text verified; upstream provenance incomplete.** | Record an authoritative upstream URL, release/custom-build identifier and configuration, source archive digest, Reserved Font Name determination, and confirmation that the packaged binaries correspond to that source. |
| Geomini | One exact Android resource hash is recorded below. An OFL 1.1 text naming the Geomini Project authors is packaged in the app. | **Artifact integrity and packaged licence text verified; upstream provenance incomplete.** | Record an authoritative upstream URL, exact release/commit and source digest, Reserved Font Name determination, and confirmation that the binary corresponds to that source. |
| Terminal native transitives | No terminal renderer native libraries are present outside the font resources. | **Not yet auditable.** | Once target ABIs and renderer features are pinned, inventory every packaged `.so`, its origin, digest, licence, notice, and source-offer obligation. |

The blocked entries are entry gates, not documentation TODOs that may be waived during
implementation. Zelland, `libghostty-vt`, `wgpu`, or `glyphon` source MUST NOT be copied into the
repository until its row has authoritative evidence. Font distribution MUST NOT be treated as
fully provenance-ready until the missing upstream identity evidence is recorded.

## Locally verified font artifacts

| Repository path | SHA-256 |
| --- | --- |
| `android/app/src/main/res/font/geomini.ttf` | `baf3fa2b1078c6a5cac05196889c01d63536ed6233e705262c7e6d4fbefffa59` |
| `android/app/src/main/res/font/iosevka_charon_mono.ttf` | `ae87c9bc7baae0a18e78cbe498d967865c251cae20fffa0c34e5937ce118f845` |
| `android/app/src/main/res/font/iosevka_charon_mono_bold.ttf` | `d5a0e6259a77a98b086897b3b86f120c1170b85ab5e82f527cf810e239f082cf` |
| `android/app/src/main/res/raw/geomini_ofl.txt` | `f540e48ef1971065cb9ec32f31a4dc83c1bef7be9e34ed6883a8284fa942aec0` |
| `android/app/src/main/res/raw/iosevka_charon_mono_ofl.txt` | `58b40bf4152bcb93ecc20489aad21093b5b1e67d64e6814e7f1cb6615cf50784` |

These hashes identify the files committed to Choosh; they do not prove upstream origin. The
application font-family XML selects Geomini for UI text and Iosevka Charon Mono regular/bold for
terminal and heading styles. Missing-glyph fallback and cell metrics still require automated tests
against the exact loaded faces.

## Deterministic evidence

Run `./scripts/check-terminal-provenance.sh`. It verifies the five recorded hashes, checks that the
packaged texts identify SIL Open Font License 1.1, confirms this record retains every required
component and blocked gate, and fails if terminal renderer crates appear in Cargo manifests or the
lockfile before the audit is updated.

This is an engineering provenance gate, not a legal opinion.
