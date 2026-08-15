# Terminal provenance and licence readiness

Status: **blocked** for importing or distributing the Zelland-derived terminal stack.

This record is the M0-R15 audit as of 2026-07-18. It distinguishes evidence present in this
repository from primary upstream evidence and assumptions. A local copy or sibling repository is
not authoritative upstream provenance.

## Readiness matrix

| Component | Repository evidence | Distribution status | Required evidence before use |
| --- | --- | --- | --- |
| Zelland renderer and Android bridge | Upstream commit [`8bf9cf5`](https://github.com/njreid/zelland/tree/8bf9cf55911588451804a39526f8ae869da021b6) is the `0.2.3` release commit. The project owner/copyright holder granted Choosh use of that exact source on 2026-07-19; see [the recorded grant](zelland-grant.md). | **Permission recorded; implementation still gated.** | Preserve the grant and exact source provenance when copying; complete the renderer graph and device conformance gates. |
| `libghostty-vt` | Zelland pins Ghostty submodule commit `bebca84668947bfc92b9a30ed58712e1c34eee1d`. Ghostty is MIT licensed, but the upstream [Ghostty 1.3 release notes](https://ghostty.org/docs/install/release-notes/1-3-0#libghostty) state that libghostty modules do not yet have a standalone versioned release. | **Blocked for a stable dependency pin.** A project licence exists, but no independently versioned libghostty-vt release/API or audited enabled-source boundary exists. | Wait for or explicitly approve a commit pin, then record its archive digest, MIT notice, build inputs, exported boundary, and complete enabled transitive/native inventory. |
| `wgpu` | Tagged source [`wgpu-v25.0.2`](https://github.com/gfx-rs/wgpu/tree/wgpu-v25.0.2) resolves to commit `f35cf942af1a3bb6f48aa9185f4d2bcee809f814`; its `wgpu` crate is `25.0.2`, authored by gfx-rs developers, and licensed `MIT OR Apache-2.0`. Root licence-file SHA-256 values are recorded below. | **Candidate provenance verified; import still blocked.** This is compatible with tagged glyphon `0.9.0`, but Choosh has not selected features/backends or audited the resulting Android graph. | Pin versions, features, and Android Vulkan/GLES backend policy; resolve a lockfile and audit every enabled Rust/native transitive and notice. |
| `glyphon` | Tagged source [`0.9.0`](https://github.com/grovesNL/glyphon/tree/0.9.0) resolves to commit `4ebd0f88a24d8a68f1dcccb94d457d25089b3b8b`; its manifest requires `wgpu = 25` and declares `MIT OR Apache-2.0 OR Zlib`. Licence-file SHA-256 values are recorded below. | **Candidate provenance verified; import still blocked.** The tagged pair is exact, but font/shaping transitives and target features are not audited. | Resolve the selected feature graph and preserve the chosen licence plus notices for glyphon, cosmic-text, etagere, lru, rustc-hash, and their enabled transitives. |
| Iosevka Charon Mono regular and bold | Three exact Android resource hashes are recorded below. An OFL 1.1 text naming the Iosevka Project authors is packaged in the app. | **Artifact integrity and packaged licence text verified; upstream provenance incomplete.** | Record an authoritative upstream URL, release/custom-build identifier and configuration, source archive digest, Reserved Font Name determination, and confirmation that the packaged binaries correspond to that source. |
| Geomini | One exact Android resource hash is recorded below. An OFL 1.1 text naming the Geomini Project authors is packaged in the app. | **Artifact integrity and packaged licence text verified; upstream provenance incomplete.** | Record an authoritative upstream URL, exact release/commit and source digest, Reserved Font Name determination, and confirmation that the binary corresponds to that source. |
| Terminal native transitives | No terminal renderer native libraries are present outside the font resources. | **Not yet auditable.** | Once target ABIs and renderer features are pinned, inventory every packaged `.so`, its origin, digest, licence, notice, and source-offer obligation. |

The blocked entries are entry gates, not documentation TODOs that may be waived during
implementation. `libghostty-vt`, `wgpu`, or `glyphon` source MUST NOT be copied into the
repository until its row has authoritative evidence. Font distribution MUST NOT be treated as
fully provenance-ready until the missing upstream identity evidence is recorded.

## Zelland adoption boundary

The permitted Zelland source was independently fetched at the recorded `v0.2.3` lightweight tag,
which resolves to `8bf9cf55911588451804a39526f8ae869da021b6`. Its Git tree and deterministic
`git archive --format=tar` digest are retained in
[`zelland-source-audit.json`](../evidence/zelland-source-audit.json). This establishes an exact,
re-auditable source candidate; it does **not** add the source as a submodule or a dependency.

The audit identifies four candidate terminal files only. They remain outside Choosh because the
pinned tree's `libghostty-source` gitlink has no `.gitmodules` mapping, its Cargo graph contains
Tauri plus the historical `wgpu 23.0.1`/`glyphon 0.7.0` pair, and its JNI layer is coupled to
package-specific global renderer/application state. Copying it would violate both the unresolved
dependency audit and Choosh's explicit composition rule. The next admissible implementation
increment is therefore a separately pinned, reproducible libghostty input and a newly resolved
Choosh-only renderer graph—not an import of this application tree.

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
packaged texts identify SIL Open Font License 1.1, and verifies that the checked font resources,
font families, heading/terminal styles, and retained OFL notices are the ones the Android package
actually selects. It confirms this record retains every required component, and fails if terminal
renderer crates appear in **any** Cargo manifest or the lockfile while the machine-readable decision
is blocked. It also validates
[`terminal-go-no-go.json`](../evidence/terminal-go-no-go.json), including the four prerequisite
gates, exact device classes, required conformance scenarios, evidence paths, and derived decision.
It validates the Zelland audit's exact source pin and the fact that its integration boundary remains
blocked until those separate dependency and composition prerequisites are resolved.

The JSON file is the authoritative machine-readable handoff state. A reviewer clears one prerequisite only by changing
its status to `passed`, retaining a non-empty repository-owned evidence path, and replacing the
next action with the completed verification result. Device conformance becomes `passed` only after
all four target entries contain retained evidence covering every listed scenario. The decision can
be `go` only when all prerequisite gates and device conformance pass; the checker rejects any
inconsistent optimistic decision.

This is an engineering provenance gate, not a legal opinion.

## 2026-08-14/15 addendum: the M2 renderer implementation pass

The rows above (and `docs/evidence/terminal-go-no-go.json`/
`zelland-source-audit.json`, which this file's checker script references) are
pre-`94b3553` ("Reset architecture to a relay-brokered, jj-backed,
passkey-authenticated fleet design") scaffolding: the referenced
`docs/evidence/` directory does not exist at the current `HEAD`, and
`scripts/check-specs.sh` (the only caller of `scripts/check-terminal-provenance.sh`)
itself references other files that no longer exist post-reset
(`CHOOSH_DESIGN_PLAN.md`, `docs/threat-model.md`, `protocol/v1/envelope.schema.json`).
Both scripts fail on a clean, untouched checkout of current `HEAD`, independent of
this pass — this addendum does not attempt to resurrect that JSON evidence-gate
apparatus (four device classes x nine scenarios) as part of this increment; that
remains a separate, real follow-up if that governance model is still wanted.

What *is* real, from this pass, implementing M2's terminal renderer per
`docs/specs/terminal-experience.md`:

- **Zelland source**: `njreid/zelland` at `8bf9cf55911588451804a39526f8ae869da021b6`
  (the same commit `zelland-grant.md` covers), cloned read-only outside this repo.
  Ported (adapted, not copied verbatim): `src-tauri/src/renderer/mod.rs`'s wgpu/glyphon
  pipeline (surface-format detection and atlas rebuild, per-row damage cache, cursor
  shader, deferred resize via a session-level pending-size — the same fix
  `WGPU_FIXES.md` records, independently rediscovered on-device during this pass'
  own verification) into `rust/choosh-android-bridge/src/terminal_renderer.rs`, and
  `src-tauri/src/renderer/android.rs`'s `Surface` -> `ANativeWindow` JNI conversion
  into `rust/choosh-android-bridge/src/terminal_jni.rs`. Not ported: Tauri, Svelte,
  the JS bridge, Zelland's package/class names, and `src-tauri/src/ghostty.rs`/
  `terminal.rs` (see next point).
- **`libghostty-vt`**: a real, reportable attempt, not skipped. `zig@0.16.0` was
  provisioned via `mise` and `ghostty-org/ghostty` (upstream, current `main`, not
  Zelland's vendored copy) was built with
  `zig build -Demit-lib-vt=true -Dapp-runtime=none -Dtarget=aarch64-linux-android`
  (and separately for `x86_64-linux-android`) — **both succeeded**, producing a real
  `libghostty-vt.a` linked against the Android NDK sysroot, with a C header layout
  (`ghostty/vt/render.h`, `terminal.h`, etc.) substantially compatible with the FFI
  surface Zelland's pinned commit used. The toolchain is proven viable. What was
  *not* done: wiring that static library into this crate's actual build (`bindgen`
  against the current, self-described "incomplete, work-in-progress" API; a
  `build.rs` invoking `zig build`; porting the render-state row/cell iteration and
  mouse/key encoders onto the current header shape). Given this task's explicit
  priority order (real GPU rendering verified on-device, and real PTY tunnel
  wiring, both rank above the choice of VT backend within item 1), the pure-Rust
  `vte` crate was used instead, behind the same engine interface libghostty-vt
  would have filled — see `rust/choosh-terminal-engine/src/terminal.rs`'s module
  doc for the full reasoning.
- **`wgpu`/`glyphon`**: `wgpu = "23.0"` (resolved `23.0.1`) / `glyphon = "0.7"`
  (resolved `0.7.0`) — the versions this task's own instructions specified (matching
  Zelland's pinned pair), not the newer `wgpu 25`/`glyphon 0.9` pair the table above
  names as a "candidate" — that newer pair was not evaluated in this pass.
- **Fonts**: Iosevka Charon Mono regular/bold (already-packaged resources, hashes
  above) embedded directly into the `.so` at compile time via `include_bytes!` and
  loaded into `glyphon`'s `FontSystem`, so cell metrics come from the real,
  measured face (confirmed on-device: `14.25x38px` at the loaded size) rather than
  a hard-coded constant.
- **On-device verification**: built and installed on the real Genymotion `cloud_arm`
  device already provisioned in this environment. A synthetic byte-injection JNI
  path (`native_terminal_test_inject`, bypassing the PTY tunnel) fed real ANSI
  sequences (a colored `ls -la` prompt/listing, a 24-row 256-colour full-screen
  redraw) into the parser and renderer. Screenshots (`adb exec-out screencap`)
  confirmed real glyphs, real SGR colors, and a real cursor rectangle rendering via
  wgpu/Vulkan (SwiftShader on this device) — not a stub. The app also survived a
  rotation cycle and a home/background/foreground cycle without a crash (confirmed
  via `adb logcat`, no `FATAL EXCEPTION`/`SIGSEGV`/`panicked at` across the whole
  session). Two real bugs were found and fixed *during* this on-device pass, not
  just in review: (1) `android/app/build.gradle.kts` didn't order
  `mergeDebugJniLibFolders`/`mergeDebugNativeLibs` after `buildRustAndroid`, so an
  incremental Rust-only rebuild could package a stale `.so`; (2) a race where
  Android's `surfaceChanged` (real pixel dimensions) could arrive before the async
  `Renderer::init()` GPU-device creation finished, silently dropping the resize and
  leaving the surface permanently configured at its `1x1` placeholder (frames
  rendered "successfully" into a single pixel, stretched over the whole view) — fixed
  with a session-level `pending_size`, the same shape as Zelland's own `PENDING_SIZE`
  static per `WGPU_FIXES.md`, independently re-encountered here.
- **PTY tunnel transport**: at the time this task began, `rust/choosh-android-transport`
  did not yet have the tunnel-multiplexing client (`open_pty_tunnel`/`PtyTunnelHandle`)
  the task's own instructions described as "already built and tested" — the file
  present at the actual working `HEAD` (confirmed via `git log`/`git reflog`) was an
  earlier, single-request-response-only `PhoneConnection` with no tunnel support at
  all. The wire-format primitives (`ControlRequest::OpenTunnel`, `FRAME_CLASS_TUNNEL`,
  `encode_tunnel_frame`, etc.) already existed in `choosh-protocol`, so the client-side
  multiplexing (a background I/O task demultiplexing control responses by
  `request_id` and tunnel bytes by tunnel ID) was implemented for real in this pass —
  see `rust/choosh-android-transport/src/lib.rs`'s module doc — rather than treated as
  a stub, and is covered by real tests including a genuinely concurrent tunnel-open +
  unrelated control call.
