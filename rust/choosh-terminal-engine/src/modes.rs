//! Terminal modes toggled by CSI `?<n>h`/`?<n>l` (DEC private modes) and a
//! few ECMA-48 modes, plus the handful of terminal-wide bits (mouse
//! encoding flavor, application keypad) that the renderer/input dispatcher
//! need to read. Named individually (not a bare bitset) so callers get
//! compile-time-checked field access, matching how Zelland's
//! `TerminalSession::get_mouse_mode`/`get_cursor_pos` exposed single named
//! queries rather than a raw mode word.

/// Which mouse events are reported, per the DEC private mode that enabled
/// tracking (`?1000`/`?1002`/`?1003`). `None` means mouse/touch input is
/// never forwarded — matching `terminal-experience.md`'s "never forward
/// pointer input unless the active terminal mode permits it".
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum MouseTracking {
    #[default]
    None,
    /// `?1000`: button press/release only.
    Normal,
    /// `?1002`: press/release plus motion while a button is held.
    ButtonEvent,
    /// `?1003`: press/release plus all motion.
    AnyEvent,
}

/// How a reported mouse event is encoded on the wire. SGR (`?1006`) is the
/// only encoding this engine emits — legacy X10/UTF-8 encodings are not
/// implemented since every terminal multiplexer/TUI this client targets
/// (Zellij, Codex, `OpenCode`, Claude Code) supports SGR.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum MouseEncoding {
    #[default]
    Sgr,
}

// Each field is an independently-toggled DEC private mode; a real terminal
// carries all of these simultaneously, so there is no smaller enum/state
// machine that doesn't just re-encode the same independent bits.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug)]
pub struct Modes {
    pub application_cursor_keys: bool,
    pub application_keypad: bool,
    pub bracketed_paste: bool,
    pub mouse_tracking: MouseTracking,
    pub mouse_encoding: MouseEncoding,
    pub origin_mode: bool,
    pub autowrap: bool,
    pub cursor_visible: bool,
    pub alt_screen_active: bool,
    /// Insert (`true`) vs. replace (`false`) mode, IRM (`\x1b[4h`/`\x1b[4l`).
    pub insert_mode: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            application_cursor_keys: false,
            application_keypad: false,
            bracketed_paste: false,
            mouse_tracking: MouseTracking::None,
            mouse_encoding: MouseEncoding::Sgr,
            origin_mode: false,
            autowrap: true,
            cursor_visible: true,
            alt_screen_active: false,
            insert_mode: false,
        }
    }
}
