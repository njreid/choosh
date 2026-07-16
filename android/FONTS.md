# Font resource plan

Choosh intends to use Geomini for UI text and Iosevka for headings and terminal cells. No font
binary is currently committed because this repository contains no licensed source asset or
provenance record for either family. Add assets only with the upstream source URL, exact version,
license text, checksum, and redistribution review. Until then the skeleton uses the platform sans
font and the native terminal renderer remains responsible for its eventual monospace font asset.

Font substitution is visual-only and must not move terminal shaping into Compose or a WebView.
