//! Key -> byte-sequence encoding. Per `docs/specs/terminal-experience.md`,
//! "UI buttons MUST send typed key commands rather than hard-coded escape
//! strings; the Rust engine encodes application-cursor, modifier,
//! bracketed-paste, and other mode-dependent sequences" — this module is
//! that single encoder, shared by the IME path, hardware keyboard path,
//! and the extra-keys bar.

use crate::modes::Modes;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Delete,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// Encodes one key press into the bytes to send to the PTY, honoring
/// application-cursor-keys mode (DECCKM) for the arrow/Home/End family.
// `mods`/`modes` read as similar to clippy's name-similarity heuristic but
// name genuinely distinct concepts (terminal mode state vs. key modifiers)
// that every call site needs both of — renaming either makes call sites
// less clear, not more.
#[allow(clippy::similar_names)]
#[must_use]
pub fn encode(modes: &Modes, key: Key, mods: Modifiers) -> Vec<u8> {
    if let Key::Char(ch) = key {
        if mods.ctrl
            && let Some(byte) = ctrl_byte(ch) {
                return vec![byte];
            }
        if mods.alt {
            let mut out = vec![0x1b];
            out.extend(ch.to_string().into_bytes());
            return out;
        }
        return ch.to_string().into_bytes();
    }

    let app = modes.application_cursor_keys;
    match key {
        Key::Enter => b"\r".to_vec(),
        Key::Tab => b"\t".to_vec(),
        Key::Backspace => vec![0x7f],
        Key::Delete => b"\x1b[3~".to_vec(),
        Key::Escape => vec![0x1b],
        Key::Up => arrow(app, 'A'),
        Key::Down => arrow(app, 'B'),
        Key::Right => arrow(app, 'C'),
        Key::Left => arrow(app, 'D'),
        Key::Home => if app { b"\x1bOH".to_vec() } else { b"\x1b[H".to_vec() },
        Key::End => if app { b"\x1bOF".to_vec() } else { b"\x1b[F".to_vec() },
        Key::PageUp => b"\x1b[5~".to_vec(),
        Key::PageDown => b"\x1b[6~".to_vec(),
        Key::F(n) => f_key(n),
        Key::Char(_) => unreachable!("handled above"),
    }
}

fn arrow(application_mode: bool, letter: char) -> Vec<u8> {
    let prefix = if application_mode { 'O' } else { '[' };
    format!("\x1b{prefix}{letter}").into_bytes()
}

fn f_key(n: u8) -> Vec<u8> {
    match n {
        1..=4 => format!("\x1bO{}", (b'P' + (n - 1)) as char).into_bytes(),
        5 => b"\x1b[15~".to_vec(),
        6 => b"\x1b[17~".to_vec(),
        7 => b"\x1b[18~".to_vec(),
        8 => b"\x1b[19~".to_vec(),
        9 => b"\x1b[20~".to_vec(),
        10 => b"\x1b[21~".to_vec(),
        11 => b"\x1b[23~".to_vec(),
        12 => b"\x1b[24~".to_vec(),
        _ => Vec::new(),
    }
}

/// Maps a Ctrl-modified printable ASCII letter/punctuation to its C0
/// control byte (`Ctrl-A` -> 0x01, ... `Ctrl-_` -> 0x1f).
fn ctrl_byte(ch: char) -> Option<u8> {
    let upper = ch.to_ascii_uppercase();
    if upper.is_ascii_uppercase() {
        return Some(upper as u8 - b'A' + 1);
    }
    match ch {
        '@' => Some(0x00),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        _ => None,
    }
}

/// Wraps `paste` in bracketed-paste markers when the terminal has enabled
/// that mode (`\x1b[200~ ... \x1b[201~`), or passes it through unwrapped
/// otherwise. Callers are responsible for the size limit and multiline
/// warning `terminal-experience.md` requires before calling this.
#[must_use]
pub fn encode_paste(modes: &Modes, paste: &[u8]) -> Vec<u8> {
    if modes.bracketed_paste {
        let mut out = Vec::with_capacity(paste.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(paste);
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        paste.to_vec()
    }
}

#[cfg(test)]
#[allow(clippy::similar_names)] // see encode()'s identical rationale for mods/modes
mod tests {
    use super::*;

    #[test]
    fn plain_char_passes_through() {
        let modes = Modes::default();
        assert_eq!(encode(&modes, Key::Char('a'), Modifiers::default()), b"a");
    }

    #[test]
    fn ctrl_c_is_end_of_text() {
        let modes = Modes::default();
        let mods = Modifiers { ctrl: true, ..Modifiers::default() };
        assert_eq!(encode(&modes, Key::Char('c'), mods), vec![0x03]);
    }

    #[test]
    fn arrow_keys_switch_on_application_cursor_mode() {
        let mut modes = Modes::default();
        assert_eq!(encode(&modes, Key::Up, Modifiers::default()), b"\x1b[A");
        modes.application_cursor_keys = true;
        assert_eq!(encode(&modes, Key::Up, Modifiers::default()), b"\x1bOA");
    }

    #[test]
    fn bracketed_paste_wraps_when_enabled() {
        let modes = Modes { bracketed_paste: true, ..Modes::default() };
        let wrapped = encode_paste(&modes, b"hello");
        assert!(wrapped.starts_with(b"\x1b[200~"));
        assert!(wrapped.ends_with(b"\x1b[201~"));
        assert!(!encode_paste(&Modes::default(), b"hello").starts_with(b"\x1b["));
    }

    #[test]
    fn alt_modifier_prefixes_escape() {
        let modes = Modes::default();
        let mods = Modifiers { alt: true, ..Modifiers::default() };
        assert_eq!(encode(&modes, Key::Char('x'), mods), b"\x1bx");
    }
}
