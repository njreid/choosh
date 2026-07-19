# Terminal provenance and licence readiness

Status: **blocked** for importing or distributing the Zelland-derived terminal stack.

This record is the M0-R15 audit as of 2026-07-18. It distinguishes evidence present in this
repository from primary upstream evidence and assumptions. A local copy or sibling repository is
not authoritative upstream provenance.

## Readiness matrix

| Component | Repository evidence | Distribution status | Required evidence before use |
| --- | --- | --- | --- |
| Zelland renderer and Android bridge | Upstream commit [`8bf9cf5`](https://github.com/njreid/zelland/tree/8bf9cf55911588451804a39526f8ae869da021b6) is the `0.2.3` release commit and its Git history identifies Nicholas Reid as author. Its tree contains no `LICENSE`, `COPYING`, or `NOTICE`; `src-tauri/Cargo.toml` says only `authors = ["you"]`. | **Blocked.** Repository authorship does not grant redistribution permission, and the pinned tree has no licence grant. | The copyright holder must add an explicit licence covering the exact source or provide a separately recorded grant and required notices. |
| `libghostty-vt` | Zelland pins Ghostty submodule commit `bebca84668947bfc92b9a30ed58712e1c34eee1d`. Ghostty is MIT licensed, but the upstream [Ghostty 1.3 release notes](https://ghostty.org/docs/install/release-notes/1-3-0#libghostty) state that libghostty modules do not yet have a standalone versioned release. | **Blocked for a stable dependency pin.** A project licence exists, but no independently versioned libghostty-vt release/API or audited enabled-source boundary exists. | Wait for or explicitly approve a commit pin, then record its archive digest, MIT notice, build inputs, exported boundary, and complete enabled transitive/native inventory. |
| `wgpu` | Tagged source [`wgpu-v25.0.2`](https://github.com/gfx-rs/wgpu/tree/wgpu-v25.0.2) resolves to commit `f35cf942af1a3bb6f48aa9185f4d2bcee809f814`; its `wgpu` crate is `25.0.2`, authored by gfx-rs developers, and licensed `MIT OR Apache-2.0`. Root licence-file SHA-256 values are recorded below. | **Candidate provenance verified; import still blocked.** This is compatible with tagged glyphon `0.9.0`, but Choosh has not selected features/backends or audited the resulting Android graph. | Pin versions, features, and Android Vulkan/GLES backend policy; resolve a lockfile and audit every enabled Rust/native transitive and notice. |
| `glyphon` | Tagged source [`0.9.0`](https://github.com/grovesNL/glyphon/tree/0.9.0) resolves to commit `4ebd0f88a24d8a68f1dcccb94d457d25089b3b8b`; its manifest requires `wgpu = 25` and declares `MIT OR Apache-2.0 OR Zlib`. Licence-file SHA-256 values are recorded below. | **Candidate provenance verified; import still blocked.** The tagged pair is exact, but font/shaping transitives and target features are not audited. | Resolve the selected feature graph and preserve the chosen licence plus notices for glyphon, cosmic-text, etagere, lru, rustc-hash, and their enabled transitives. |
| Iosevka Charon Mono regular and bold | Three exact Android resource hashes are recorded below. An OFL 1.1 text naming the Iosevka Project authors is packaged in the app. | **Artifact integrity and packaged licence text verified; upstream provenance incomplete.** | Record an authoritative upstream URL, release/custom-build identifier and configuration, source archive digest, Reserved Font Name determination, and confirmation that the packaged binaries correspond to that source. |
| Geomini | One exact Android resource hash is recorded below. An OFL 1.1 text naming the Geomini Project authors is packaged in the app. | **Artifact integrity and packaged licence text verified; upstream provenance incomplete.** | Record an authoritative upstream URL, exact release/commit and source digest, Reserved Font Name determination, and confirmation that the binary corresponds to that source. |
| Terminal native transitives | No terminal renderer native libraries are present outside the font resources. | **Not yet auditable.** | Once target ABIs and renderer features are pinned, inventory every packaged `.so`, its origin, digest, licence, notice, and source-offer obligation. |

The blocked entries are entry gates, not documentation TODOs that may be waived during
implementation. Zelland, `libghostty-vt`, `wgpu`, or `glyphon` source MUST NOT be copied into the
repository until its row has authoritative evidence. Font distribution MUST NOT be treated as
fully provenance-ready until the missing upstream identity evidence is recorded.

## Primary-source dependency findings

The pinned Zelland commit itself used registry `glyphon 0.7.0` (crate checksum
`36257cc8db90a3c90c500c283a0ca5a403da50fd2ae1db83bff06f7fecfbde7d`) and `wgpu 23.0.1`
(crate checksum `80f70000db37c469ea9d67defdc13024ddf9a5f1b89cb2941b812ad7cde1735a`). Those
values explain the source lineage; they are not a recommendation to adopt an older graph.

The newer, mutually compatible tagged audit candidate is:

| Source | Version and commit | Declared licence | Upstream licence-file SHA-256 |
| --- | --- | --- | --- |
| glyphon | `0.9.0`, `4ebd0f88a24d8a68f1dcccb94d457d25089b3b8b` | `MIT OR Apache-2.0 OR Zlib` | MIT `23f18e03dc49df91622fe2a76176497404e46ced8a715d9d2b67a7446571cca3`; Apache-2.0 `a60eea817514531668d7e00765731449fe14d059d3249e0bc93b36de45f759f2`; Zlib `b3ec01ad0869c5c50937bc68780cd0bb44e235ad858dfa54a7a381dced9b115a` |
| wgpu | `25.0.2`, `f35cf942af1a3bb6f48aa9185f4d2bcee809f814` | `MIT OR Apache-2.0` | MIT `c7fea58d1cfe49634cd92e54fc10a9d871f4b275321a4cd8c09e449122caaeb4`; Apache-2.0 `a6cba85bc92e0cff7a450b1d873c0eaa2e9fc96bf472df0247a26bec77bf3ff9` |

This table verifies source identity and top-level licence choices only. It does not close the
native-distribution audit or authorize copying unlicensed Zelland code.

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
