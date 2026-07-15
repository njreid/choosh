//! Headless terminal input state and mode-aware byte encoding.
//!
//! This module deliberately has no Android or renderer dependencies. All input
//! sources enter through [`TerminalInput`], making generation checks, paste
//! policy, and terminal-mode encoding independently testable.

const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TerminalModes {
    pub application_cursor: ModeState,
    pub application_keypad: ModeState,
    pub bracketed_paste: ModeState,
    pub alternate_screen: ModeState,
    pub mouse_reporting: MouseReporting,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ModeState {
    #[default]
    Disabled,
    Enabled,
}

impl ModeState {
    #[must_use]
    fn is_enabled(self) -> bool {
        self == Self::Enabled
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MouseReporting {
    #[default]
    Off,
    Press,
    ButtonMotion,
    AnyMotion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Modifier {
    Ctrl,
    Alt,
    Meta,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const CTRL: Self = Self(1);
    pub const ALT: Self = Self(2);
    pub const META: Self = Self(4);

    #[must_use]
    pub fn contains(self, modifier: Modifier) -> bool {
        self.0 & Self::bit(modifier) != 0
    }

    fn toggle(&mut self, modifier: Modifier) {
        self.0 ^= Self::bit(modifier);
    }

    fn bit(modifier: Modifier) -> u8 {
        match modifier {
            Modifier::Ctrl => Self::CTRL.0,
            Modifier::Alt => Self::ALT.0,
            Modifier::Meta => Self::META.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Left,
    Up,
    Down,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    KeypadDigit(u8),
    KeypadEnter,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalInput {
    CommitText(String),
    SetComposition(String),
    ClearComposition,
    ToggleModifier(Modifier),
    Key(Key),
    ExtraKey(Key),
    Paste(String),
    ResolvePaste { decision_id: u64, send: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PastePolicy {
    pub max_bytes: usize,
    pub confirm_multiline: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputOutcome {
    Bytes(Vec<u8>),
    LocalStateChanged,
    PasteConfirmationRequired { decision_id: u64, byte_len: usize },
    PasteDiscarded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalInputError {
    StaleGeneration,
    PasteTooLarge,
    NoPendingPaste,
    WrongPasteDecision,
    InvalidKey,
    GenerationExhausted,
}

#[derive(Debug)]
struct PendingPaste {
    decision_id: u64,
    text: String,
}

#[derive(Debug)]
pub struct TerminalInputState {
    generation: u64,
    modes: TerminalModes,
    modifiers: Modifiers,
    composition: String,
    paste_policy: PastePolicy,
    pending_paste: Option<PendingPaste>,
    next_decision_id: u64,
}

impl TerminalInputState {
    #[must_use]
    pub fn new(generation: u64, paste_policy: PastePolicy) -> Self {
        Self {
            generation,
            modes: TerminalModes::default(),
            modifiers: Modifiers::default(),
            composition: String::new(),
            paste_policy,
            pending_paste: None,
            next_decision_id: 1,
        }
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }
    #[must_use]
    pub fn modes(&self) -> TerminalModes {
        self.modes
    }
    #[must_use]
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }
    #[must_use]
    pub fn composition(&self) -> &str {
        &self.composition
    }
    pub fn set_modes(&mut self, modes: TerminalModes) {
        self.modes = modes;
    }

    /// Atomically moves the dispatcher to a new target generation. Pending
    /// composition, one-shot modifiers, and paste approval cannot cross it.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalInputError::StaleGeneration`] unless `generation` is
    /// newer than the active generation.
    pub fn rebind(&mut self, generation: u64) -> Result<(), TerminalInputError> {
        if generation <= self.generation {
            return Err(TerminalInputError::StaleGeneration);
        }
        self.generation = generation;
        self.modes = TerminalModes::default();
        self.modifiers = Modifiers::default();
        self.composition.clear();
        self.pending_paste = None;
        Ok(())
    }

    /// Applies one typed input action to the active terminal binding.
    ///
    /// # Errors
    ///
    /// Returns a stable [`TerminalInputError`] for a stale generation, an
    /// invalid key, a paste policy violation, or an invalid paste decision.
    pub fn input(
        &mut self,
        generation: u64,
        input: TerminalInput,
    ) -> Result<InputOutcome, TerminalInputError> {
        if generation != self.generation {
            return Err(TerminalInputError::StaleGeneration);
        }
        match input {
            TerminalInput::SetComposition(text) => {
                self.composition = text;
                Ok(InputOutcome::LocalStateChanged)
            }
            TerminalInput::ClearComposition => {
                self.composition.clear();
                Ok(InputOutcome::LocalStateChanged)
            }
            TerminalInput::ToggleModifier(modifier) => {
                self.modifiers.toggle(modifier);
                Ok(InputOutcome::LocalStateChanged)
            }
            TerminalInput::CommitText(text) => {
                self.composition.clear();
                Ok(InputOutcome::Bytes(self.encode_text(text.as_bytes())))
            }
            TerminalInput::Key(key) | TerminalInput::ExtraKey(key) => {
                self.encode_key(key).map(InputOutcome::Bytes)
            }
            TerminalInput::Paste(text) => self.begin_paste(text),
            TerminalInput::ResolvePaste { decision_id, send } => {
                self.resolve_paste(decision_id, send)
            }
        }
    }

    fn begin_paste(&mut self, text: String) -> Result<InputOutcome, TerminalInputError> {
        if text.len() > self.paste_policy.max_bytes {
            return Err(TerminalInputError::PasteTooLarge);
        }
        if self.paste_policy.confirm_multiline && (text.contains('\n') || text.contains('\r')) {
            let decision_id = self.next_decision_id;
            self.next_decision_id = self
                .next_decision_id
                .checked_add(1)
                .ok_or(TerminalInputError::GenerationExhausted)?;
            let byte_len = text.len();
            self.pending_paste = Some(PendingPaste { decision_id, text });
            return Ok(InputOutcome::PasteConfirmationRequired {
                decision_id,
                byte_len,
            });
        }
        Ok(InputOutcome::Bytes(self.encode_paste(text.as_bytes())))
    }

    fn resolve_paste(
        &mut self,
        decision_id: u64,
        send: bool,
    ) -> Result<InputOutcome, TerminalInputError> {
        let pending = self
            .pending_paste
            .take()
            .ok_or(TerminalInputError::NoPendingPaste)?;
        if pending.decision_id != decision_id {
            self.pending_paste = Some(pending);
            return Err(TerminalInputError::WrongPasteDecision);
        }
        if send {
            Ok(InputOutcome::Bytes(
                self.encode_paste(pending.text.as_bytes()),
            ))
        } else {
            Ok(InputOutcome::PasteDiscarded)
        }
    }

    fn encode_paste(&self, text: &[u8]) -> Vec<u8> {
        if !self.modes.bracketed_paste.is_enabled() {
            return text.to_vec();
        }
        let mut bytes = Vec::with_capacity(
            BRACKETED_PASTE_START.len() + text.len() + BRACKETED_PASTE_END.len(),
        );
        bytes.extend_from_slice(BRACKETED_PASTE_START);
        bytes.extend_from_slice(text);
        bytes.extend_from_slice(BRACKETED_PASTE_END);
        bytes
    }

    fn encode_text(&mut self, text: &[u8]) -> Vec<u8> {
        let modifiers = core::mem::take(&mut self.modifiers);
        let mut bytes = Vec::with_capacity(text.len() + 2);
        if modifiers.contains(Modifier::Alt) {
            bytes.push(0x1b);
        }
        if modifiers.contains(Modifier::Meta) {
            bytes.push(0x1b);
        }
        if modifiers.contains(Modifier::Ctrl) && text.len() == 1 {
            let byte = text[0];
            if byte.is_ascii_alphabetic() {
                bytes.push(byte.to_ascii_uppercase() & 0x1f);
                return bytes;
            }
        }
        bytes.extend_from_slice(text);
        bytes
    }

    fn encode_key(&mut self, key: Key) -> Result<Vec<u8>, TerminalInputError> {
        let base: &[u8] = match key {
            Key::Enter => b"\r",
            Key::Tab => b"\t",
            Key::Escape => b"\x1b",
            Key::Backspace => b"\x7f",
            Key::Delete => b"\x1b[3~",
            Key::Home => b"\x1b[H",
            Key::End => b"\x1b[F",
            Key::PageUp => b"\x1b[5~",
            Key::PageDown => b"\x1b[6~",
            Key::Left => {
                if self.modes.application_cursor.is_enabled() {
                    b"\x1bOD"
                } else {
                    b"\x1b[D"
                }
            }
            Key::Up => {
                if self.modes.application_cursor.is_enabled() {
                    b"\x1bOA"
                } else {
                    b"\x1b[A"
                }
            }
            Key::Down => {
                if self.modes.application_cursor.is_enabled() {
                    b"\x1bOB"
                } else {
                    b"\x1b[B"
                }
            }
            Key::Right => {
                if self.modes.application_cursor.is_enabled() {
                    b"\x1bOC"
                } else {
                    b"\x1b[C"
                }
            }
            Key::KeypadEnter => {
                if self.modes.application_keypad.is_enabled() {
                    b"\x1bOM"
                } else {
                    b"\r"
                }
            }
            Key::KeypadDigit(digit) if digit < 10 => {
                if self.modes.application_keypad.is_enabled() {
                    return Ok(self.finish_key(vec![0x1b, b'O', b'p' + digit]));
                }
                return Ok(self.finish_key(vec![b'0' + digit]));
            }
            Key::KeypadDigit(_) => return Err(TerminalInputError::InvalidKey),
        };
        Ok(self.finish_key(base.to_vec()))
    }

    fn finish_key(&mut self, mut bytes: Vec<u8>) -> Vec<u8> {
        let modifiers = core::mem::take(&mut self.modifiers);
        if modifiers.contains(Modifier::Ctrl) && bytes.len() == 1 && bytes[0].is_ascii_alphabetic()
        {
            bytes[0] = bytes[0].to_ascii_uppercase() & 0x1f;
        }
        if modifiers.contains(Modifier::Alt) {
            bytes.insert(0, 0x1b);
        }
        if modifiers.contains(Modifier::Meta) {
            bytes.insert(0, 0x1b);
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> TerminalInputState {
        TerminalInputState::new(
            7,
            PastePolicy {
                max_bytes: 16,
                confirm_multiline: true,
            },
        )
    }
    fn bytes(outcome: InputOutcome) -> Vec<u8> {
        match outcome {
            InputOutcome::Bytes(value) => value,
            other => panic!("expected bytes, got {other:?}"),
        }
    }

    #[test]
    fn utf8_ime_commit_is_exact_and_composition_is_local() {
        let mut input = state();
        assert_eq!(
            input.input(7, TerminalInput::SetComposition("e\u{301}".into())),
            Ok(InputOutcome::LocalStateChanged)
        );
        assert_eq!(
            bytes(
                input
                    .input(7, TerminalInput::CommitText("é猫".into()))
                    .unwrap()
            ),
            "é猫".as_bytes()
        );
        assert_eq!(input.composition(), "");
    }

    #[test]
    fn cursor_and_keypad_goldens_follow_modes() {
        let mut input = state();
        assert_eq!(
            bytes(input.input(7, TerminalInput::Key(Key::Up)).unwrap()),
            b"\x1b[A"
        );
        input.set_modes(TerminalModes {
            application_cursor: ModeState::Enabled,
            application_keypad: ModeState::Enabled,
            ..TerminalModes::default()
        });
        assert_eq!(
            bytes(input.input(7, TerminalInput::ExtraKey(Key::Up)).unwrap()),
            b"\x1bOA"
        );
        assert_eq!(
            bytes(
                input
                    .input(7, TerminalInput::Key(Key::KeypadDigit(2)))
                    .unwrap()
            ),
            b"\x1bOr"
        );
    }

    #[test]
    fn one_shot_modifiers_encode_and_clear() {
        let mut input = state();
        input
            .input(7, TerminalInput::ToggleModifier(Modifier::Ctrl))
            .unwrap();
        input
            .input(7, TerminalInput::ToggleModifier(Modifier::Alt))
            .unwrap();
        assert_eq!(
            bytes(
                input
                    .input(7, TerminalInput::CommitText("c".into()))
                    .unwrap()
            ),
            b"\x1b\x03"
        );
        assert_eq!(input.modifiers(), Modifiers::default());
        assert_eq!(
            bytes(
                input
                    .input(7, TerminalInput::CommitText("c".into()))
                    .unwrap()
            ),
            b"c"
        );
    }

    #[test]
    fn bracketed_paste_has_exact_markers() {
        let mut input = state();
        input.set_modes(TerminalModes {
            bracketed_paste: ModeState::Enabled,
            ..TerminalModes::default()
        });
        assert_eq!(
            bytes(
                input
                    .input(7, TerminalInput::Paste("hello".into()))
                    .unwrap()
            ),
            b"\x1b[200~hello\x1b[201~"
        );
    }

    #[test]
    fn multiline_paste_never_sends_before_matching_confirmation() {
        let mut input = state();
        assert_eq!(
            input.input(7, TerminalInput::Paste("a\nb".into())),
            Ok(InputOutcome::PasteConfirmationRequired {
                decision_id: 1,
                byte_len: 3
            })
        );
        assert_eq!(
            input.input(
                7,
                TerminalInput::ResolvePaste {
                    decision_id: 2,
                    send: true
                }
            ),
            Err(TerminalInputError::WrongPasteDecision)
        );
        assert_eq!(
            bytes(
                input
                    .input(
                        7,
                        TerminalInput::ResolvePaste {
                            decision_id: 1,
                            send: true
                        }
                    )
                    .unwrap()
            ),
            b"a\nb"
        );
        assert_eq!(
            input.input(
                7,
                TerminalInput::ResolvePaste {
                    decision_id: 1,
                    send: true
                }
            ),
            Err(TerminalInputError::NoPendingPaste)
        );
    }

    #[test]
    fn paste_limit_is_utf8_byte_based() {
        let mut input = TerminalInputState::new(
            1,
            PastePolicy {
                max_bytes: 3,
                confirm_multiline: false,
            },
        );
        assert_eq!(
            input.input(1, TerminalInput::Paste("猫".into())).map(bytes),
            Ok("猫".as_bytes().to_vec())
        );
        assert_eq!(
            input.input(1, TerminalInput::Paste("猫a".into())),
            Err(TerminalInputError::PasteTooLarge)
        );
    }

    #[test]
    fn stale_input_cannot_mutate_and_rebind_clears_ephemeral_state() {
        let mut input = state();
        input
            .input(7, TerminalInput::ToggleModifier(Modifier::Meta))
            .unwrap();
        input
            .input(7, TerminalInput::SetComposition("pending".into()))
            .unwrap();
        input.input(7, TerminalInput::Paste("a\nb".into())).unwrap();
        assert_eq!(
            input.input(6, TerminalInput::CommitText("lost".into())),
            Err(TerminalInputError::StaleGeneration)
        );
        input.rebind(8).unwrap();
        assert_eq!(input.modifiers(), Modifiers::default());
        assert_eq!(input.composition(), "");
        assert_eq!(
            input.input(
                8,
                TerminalInput::ResolvePaste {
                    decision_id: 1,
                    send: true
                }
            ),
            Err(TerminalInputError::NoPendingPaste)
        );
        assert_eq!(input.rebind(8), Err(TerminalInputError::StaleGeneration));
    }

    #[test]
    fn modes_preserve_non_input_state_for_headless_observation() {
        let mut input = state();
        let modes = TerminalModes {
            alternate_screen: ModeState::Enabled,
            mouse_reporting: MouseReporting::AnyMotion,
            ..TerminalModes::default()
        };
        input.set_modes(modes);
        assert_eq!(input.modes(), modes);
    }
}
