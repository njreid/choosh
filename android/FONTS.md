# Bundled fonts

Choosh redistributes these unmodified font binaries under the SIL Open Font
License 1.1 texts packaged with the application. The sibling repository was used
as the local artifact source; it is not an upstream authority.

| Android resource | Local source artifact | SHA-256 | Intended mapping |
| --- | --- | --- | --- |
| `res/font/geomini.ttf` | `/home/njr/code/zync/app/src/main/res/font/geomini.ttf` | `baf3fa2b1078c6a5cac05196889c01d63536ed6233e705262c7e6d4fbefffa59` | General UI body text through `@font/choosh_ui` |
| `res/font/iosevka_charon_mono.ttf` | `/home/njr/code/zync/app/src/main/res/font/iosevka_charon_mono.ttf` | `ae87c9bc7baae0a18e78cbe498d967865c251cae20fffa0c34e5937ce118f845` | Terminal regular through `@font/choosh_terminal` |
| `res/font/iosevka_charon_mono_bold.ttf` | `/home/njr/code/zync/app/src/main/res/font/iosevka_charon_mono_bold.ttf` | `d5a0e6259a77a98b086897b3b86f120c1170b85ab5e82f527cf810e239f082cf` | Headings via `Choosh.Text.Heading` and terminal bold through `@font/choosh_terminal` |

The Geomini OFL text is packaged as `res/raw/geomini_ofl.txt` with normalized
line endings and trailing whitespace (SHA-256
`f540e48ef1971065cb9ec32f31a4dc83c1bef7be9e34ed6883a8284fa942aec0`), from
`/home/njr/code/zync/web/src/commonMain/resources/fonts/Geomini-OFL.txt`.
The Iosevka Charon Mono OFL text is packaged with the same text normalization as
`res/raw/iosevka_charon_mono_ofl.txt` (SHA-256
`58b40bf4152bcb93ecc20489aad21093b5b1e67d64e6814e7f1cb6615cf50784`), from
`/home/njr/code/zync/web/src/commonMain/resources/fonts/IosevkaCharonMono-OFL.txt`.

Font choice never carries semantic meaning. Android accessibility scaling remains
authoritative, and labels/order must remain usable independently of glyph shape.
The XML families give deterministic weight selection. If a bundled font cannot be
loaded, platform text may fall back to its default sans or monospace family; layout
and terminal cell metrics must be remeasured rather than assuming identical glyph
widths.
