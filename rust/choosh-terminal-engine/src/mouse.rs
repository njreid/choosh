//! SGR mouse-event encoding (`\x1b[<Cb;Cx;Cy(M|m)`), gated on the terminal's
//! active tracking mode — porting the *behavior* of Zelland's
//! `TerminalSession::process_mouse`/`encode_mouse_event`
//! (`njreid/zelland@8bf9cf5:src-tauri/src/terminal.rs`), which called
//! libghostty's `ghostty_mouse_encoder_*` C FFI to do the same encoding.
//! This is a small enough wire format (SGR is the only encoding this
//! engine emits — see `crate::modes::MouseEncoding`) that it is
//! reimplemented directly in Rust rather than requiring the FFI.

use crate::modes::{MouseTracking, Modes};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    ScrollUp,
    ScrollDown,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

#[derive(Clone, Copy, Debug)]
pub struct MouseEvent {
    /// Zero-based cell column/row (already translated from pixel
    /// coordinates using the renderer's live cell metrics, per
    /// `terminal-experience.md`'s "live font/cell metrics ... used for
    /// ... pointer encoding").
    pub col: u16,
    pub row: u16,
    pub button: MouseButton,
    pub action: MouseAction,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// Encodes `event` as an SGR mouse sequence, or `None` if the terminal's
/// current mouse-tracking mode means this event must not be forwarded —
/// callers MUST check this before sending anything to the PTY, per
/// `terminal-experience.md`'s "never forward pointer input unless the
/// active terminal mode permits it".
#[must_use]
pub fn encode(modes: &Modes, event: MouseEvent) -> Option<Vec<u8>> {
    let motion = event.action == MouseAction::Motion;
    let allowed = match modes.mouse_tracking {
        MouseTracking::None => false,
        MouseTracking::Normal => !motion,
        MouseTracking::ButtonEvent => !motion || event.button != MouseButton::None,
        MouseTracking::AnyEvent => true,
    };
    if !allowed {
        return None;
    }

    let mut code: u16 = match event.button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::ScrollUp => 64,
        MouseButton::ScrollDown => 65,
        MouseButton::None => 35, // no button, motion-only report
    };
    if event.shift {
        code |= 4;
    }
    if event.alt {
        code |= 8;
    }
    if event.ctrl {
        code |= 16;
    }
    if motion {
        code |= 32;
    }

    let final_byte = if event.action == MouseAction::Release { 'm' } else { 'M' };
    Some(format!("\x1b[<{code};{};{}{final_byte}", event.col + 1, event.row + 1).into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(col: u16, row: u16, button: MouseButton) -> MouseEvent {
        MouseEvent { col, row, button, action: MouseAction::Press, shift: false, alt: false, ctrl: false }
    }

    #[test]
    fn click_is_dropped_without_mouse_tracking_enabled() {
        let modes = Modes::default();
        assert!(encode(&modes, press(0, 0, MouseButton::Left)).is_none());
    }

    #[test]
    fn click_is_encoded_as_sgr_when_normal_tracking_enabled() {
        let modes = Modes { mouse_tracking: MouseTracking::Normal, ..Modes::default() };
        let bytes = encode(&modes, press(3, 4, MouseButton::Left)).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text, "\x1b[<0;4;5M");
    }

    #[test]
    fn release_uses_lowercase_m_terminator() {
        let modes = Modes { mouse_tracking: MouseTracking::Normal, ..Modes::default() };
        let event = MouseEvent { col: 0, row: 0, button: MouseButton::Left, action: MouseAction::Release, shift: false, alt: false, ctrl: false };
        let text = String::from_utf8(encode(&modes, event).unwrap()).unwrap();
        assert!(text.ends_with('m'));
    }

    #[test]
    fn motion_without_button_event_mode_is_dropped() {
        let modes = Modes { mouse_tracking: MouseTracking::Normal, ..Modes::default() };
        let event = MouseEvent { col: 0, row: 0, button: MouseButton::None, action: MouseAction::Motion, shift: false, alt: false, ctrl: false };
        assert!(encode(&modes, event).is_none());
    }

    #[test]
    fn any_event_mode_forwards_bare_motion() {
        let modes = Modes { mouse_tracking: MouseTracking::AnyEvent, ..Modes::default() };
        let event = MouseEvent { col: 1, row: 1, button: MouseButton::None, action: MouseAction::Motion, shift: false, alt: false, ctrl: false };
        assert!(encode(&modes, event).is_some());
    }
}
