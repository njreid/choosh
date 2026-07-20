# ADR 0005: Native GPU terminal and Android input accessory

Status: Accepted

## Decision

Choosh will start from Zelland's Rust terminal lineage: libghostty-vt state feeding a wgpu/glyphon renderer on an Android native surface. The port will retain its rendering, damage, cursor, selection, pointer, and lifecycle work while removing Tauri, Svelte, JavaScript bridges, and package-specific coupling.

Compose will provide a terminal-only extra-keys accessory immediately above the Android IME. IME, hardware, accessory, touch, and paste input share a typed dispatcher into the Rust terminal engine.

## Consequences

- M0 changes from an open renderer comparison to a port, licence, compatibility, and performance validation.
- The renderer remains behind the `TerminalRenderer` interface defined by the
  [M0 terminal spike](../design/M0-foundation.md#internal-interfaces). If the
  wgpu/glyphon port does not pass its go/no-go gate, the concrete fallback is a
  CPU cell-grid renderer consuming the same immutable snapshots. Selecting it,
  or accepting lower budgets, requires a new ADR before M1; a blank or
  non-interactive surface is not an acceptable fallback.
- Native surface lifecycle, gesture arbitration, IME behavior, and GPU/device compatibility become release-critical Android work.
- Terminal modes and escape-sequence encoding remain in Rust rather than being duplicated in Compose.
