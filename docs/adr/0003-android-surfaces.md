# ADR 0003: Android presentation surfaces

Status: Accepted

## Decision

This ADR records the target presentation architecture, not the current M0 implementation.
The current Android application is a programmatic Java/View connection-status screen;
it has no Compose navigation/explorer, Sora editor, terminal, or WebView surface.

When those surfaces are implemented, Jetpack Compose will own navigation and the fixed
explorer. Sora will provide source editing through `AndroidView`. A native `SurfaceView`
will host the Rust GPU terminal. Internal Markdown will use a locked-down WebView driven
by Rust/Maud/Datastar. Development services will use a separate isolated WebView and
authenticated loopback gateway. Agent pages will remain complete interactive terminal
TUIs, not parsed native chats.

## Consequences

- The explorer is always page zero; all other pages are an ordered local pin set.
- Heavy views are retained and rebound rather than instantiated per logical page.
- Gesture arbitration and WebView isolation require dedicated device tests.
