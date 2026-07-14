# ADR 0003: Android presentation surfaces

Status: Accepted

## Decision

Jetpack Compose owns navigation and the fixed explorer. Sora provides source editing through `AndroidView`. A native `SurfaceView` hosts the Rust GPU terminal. Internal Markdown uses a locked-down WebView driven by Rust/Maud/Datastar. Development services use a separate isolated WebView and authenticated loopback gateway. Agent pages are complete interactive terminal TUIs, not parsed native chats.

## Consequences

- The explorer is always page zero; all other pages are an ordered local pin set.
- Heavy views are retained and rebound rather than instantiated per logical page.
- Gesture arbitration and WebView isolation require dedicated device tests.
