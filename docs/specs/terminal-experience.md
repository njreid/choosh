# Terminal rendering and input

## Scope

Choosh terminal pages MUST provide a complete interactive TUI for agents, shells, and development services. The terminal is a native Android surface backed by Rust; it is not rendered in a WebView and terminal output is never converted into a native chat transcript.

## Zelland renderer lineage

The first implementation MUST port as much of Zelland's proven Android terminal path as remains compatible and maintainable:

| Zelland capability | Choosh treatment |
| --- | --- |
| `SurfaceView` to `ANativeWindow` lifecycle | Port into an `AndroidView`-hosted native terminal surface. |
| wgpu rendering on Android/Vulkan | Port, while retaining wgpu's supported backend fallback where useful. |
| glyphon font system, shaping, and glyph atlas | Port and update to mutually compatible stable crates. |
| libghostty-vt parser and render state | Port behind a terminal-engine interface after licence and build verification. |
| Styled cell runs and per-row damage cache | Port; avoid reshaping unchanged rows. |
| Cell backgrounds, cursor and selection shaders | Port with theme and accessibility support. |
| Live font/cell metrics | Port and use for PTY sizing, selection, and pointer encoding. |
| SGR mouse encoding and tracking-mode guard | Port; never forward pointer input unless the active terminal mode permits it. |
| Pinch zoom, scroll, tap, selection, copy, and paste | Port into Choosh gesture arbitration and command dispatch. |
| Surface loss, deferred resize, atlas-format rebuild, and GPU-limit handling | Port as mandatory lifecycle behavior. |
| Tauri, Svelte, JavaScript bridges, and Zelland package names | Do not port; replace with Compose, Kotlin, and the Choosh Rust bridge. |

The port MUST preserve upstream provenance and applicable notices. Before copying source, M0 MUST establish ownership or an explicit licence for Zelland code and audit the licences and distribution requirements of libghostty-vt, wgpu, glyphon, fonts, and transitive native libraries.

The terminal typeface is Iosevka Charon Mono. Headings use the same family, while general application UI uses Geomini. The implementation MUST pin the distributed font artifacts, retain their licence and provenance, define deterministic missing-glyph fallbacks, and derive terminal cell metrics from the exact loaded face rather than hard-coded dimensions.

## Rendering architecture

```text
Zellij PTY bytes over SSH
  -> Rust terminal engine
  -> revisioned grid, modes, cursor and damage
  -> wgpu/glyphon renderer
  -> Android SurfaceView / ANativeWindow

Android IME, hardware keys, extra-keys bar and touch
  -> one Kotlin input dispatcher
  -> typed Rust terminal input command
  -> mode-aware byte encoding
  -> Zellij PTY over SSH
```

There MUST be one retained renderer per visible terminal surface, rebound to the selected pinned terminal without stopping or recreating remote processes. Switching pages MUST not leak a frame, selection, title, clipboard payload, or input into the previously bound terminal.

The terminal engine owns parsing, terminal modes, scrollback, cursor state, selection coordinates, and key/mouse encoding. Kotlin owns Android focus, IME integration, accessibility semantics, insets, gestures, and haptics. UI buttons MUST send typed key commands rather than hard-coded escape strings; the Rust engine encodes application-cursor, modifier, bracketed-paste, and other mode-dependent sequences.

## Android IME

The native terminal host MUST expose a real Android `InputConnection`. It MUST handle committed and composing text, deletion, Enter, Tab, Escape, directional keys, editor actions, Unicode, dead keys, and hardware keyboards. It SHOULD disable suggestions, personalized learning, extract/fullscreen editing, and rich-content insertion for terminal input.

IME composition MUST remain local until committed. Terminal output MUST NOT be exposed as surrounding text to the keyboard. Focus, keyboard visibility, and composition MUST recover after page changes, rotation, backgrounding, and surface recreation without duplicating input.

## Extra-keys bar

A native Compose extra-keys bar MUST appear on terminal pages immediately above the regular Android keyboard. It MUST follow `WindowInsets.ime` and navigation-bar insets rather than estimating keyboard height. When the IME is hidden, the user MAY keep the bar docked at the bottom or hide it.

The default layout MUST provide:

- one-shot Ctrl, Alt, and Meta modifiers with clear active state;
- Escape, Tab, Enter, Backspace, and forward Delete;
- Left, Up, Down, and Right;
- Home, End, Page Up, and Page Down;
- a compact configurable row for common terminal characters such as `/`, `-`, `|`, `~`, and backtick;
- a keyboard show/hide action and an overflow/customization action.

The bar MUST support key repeat where appropriate, haptic feedback, minimum 48dp touch targets, screen-reader labels, large fonts, and landscape/narrow-screen overflow. Modifier state MUST be cleared on focus loss and terminal rebind. A future locked-modifier gesture MAY be added, but one-shot behavior is the V1 default.

IME, hardware keyboard, extra-key, paste, and accessibility actions MUST all use the same command dispatcher and tests. No input path may bypass terminal-mode handling or write to an inactive terminal.

## Gestures and clipboard

Single-finger terminal selection and pointer input, two-finger scroll, pinch zoom, and horizontal page swipes MUST have explicit arbitration. Ordinary terminal interaction wins inside the surface; navigation requires an edge or deliberate horizontal threshold. Mouse-reporting mode changes whether a gesture selects locally or is encoded for the remote TUI, and the active behavior MUST be discoverable.

Copy is local and explicit. Paste MUST require a user action, obey bracketed-paste mode, enforce a size limit, and warn before sending multiline content unless the user has disabled that warning. Clipboard contents and terminal text MUST NOT be logged, persisted as analytics, or included in crash reports.

## Performance and recovery gates

The implementation MUST be tested on representative low-, mid-, and high-tier Vulkan devices and an x86_64 emulator. M0 sets numeric budgets from measurements; at minimum the renderer must:

- maintain responsive input during sustained agent output;
- render only after damage, cursor blink, selection, resize, or page activation;
- bound glyph-atlas, scrollback, frame-queue, and paste memory;
- recover from surface loss, GPU device loss, screen lock, rotation, and process recreation;
- clamp dimensions to GPU limits without corrupting PTY rows/columns;
- fall back visibly when GPU initialization fails rather than presenting a blank terminal.

Correctness fixtures MUST cover ANSI/VT behavior used by Zellij, Codex, OpenCode, and Claude Code, including alternate screen, color and attributes, wide and combining characters, hyperlinks, cursor shapes, mouse modes, bracketed paste, resize, and high-volume output.

## Zelland references

- [Renderer](https://github.com/njreid/zelland/blob/8bf9cf55911588451804a39526f8ae869da021b6/src-tauri/src/renderer/mod.rs)
- [Android renderer bridge](https://github.com/njreid/zelland/blob/8bf9cf55911588451804a39526f8ae869da021b6/src-tauri/src/renderer/android.rs)
- [Terminal engine integration](https://github.com/njreid/zelland/blob/8bf9cf55911588451804a39526f8ae869da021b6/src-tauri/src/terminal.rs)
- [Android wgpu findings](https://github.com/njreid/zelland/blob/8bf9cf55911588451804a39526f8ae869da021b6/WGPU_FIXES.md)
- [Native key-bar design](https://github.com/njreid/zelland/blob/8bf9cf55911588451804a39526f8ae869da021b6/docs/features/NATIVE_UX.md)
