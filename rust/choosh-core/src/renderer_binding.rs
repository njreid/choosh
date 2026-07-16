//! Generation-safe binding plans for one retained native terminal renderer.

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalTarget(pub String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindAction {
    QuiesceInput,
    Detach,
    ClearTerminalState,
    PresentNeutralFrame,
    Attach {
        target: TerminalTarget,
        generation: u64,
    },
    ApplySnapshot {
        target: TerminalTarget,
        generation: u64,
    },
    EnableInput {
        generation: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngressKind {
    Frame,
    Input,
    Title,
    Clipboard,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingError {
    InvalidTarget,
    GenerationExhausted,
    NoPendingAttach,
    StaleGeneration,
    AttachFailed,
}

#[derive(Debug, Default)]
pub struct RendererBinding {
    generation: u64,
    target: Option<TerminalTarget>,
    pending: Option<TerminalTarget>,
    enabled: bool,
}

impl RendererBinding {
    /// Starts an ordered rebind plan. No action stops or mutates the remote PTY.
    /// # Errors
    /// Returns an invalid-target or generation exhaustion error without changing binding.
    pub fn begin_rebind(
        &mut self,
        target: TerminalTarget,
    ) -> Result<Vec<BindAction>, BindingError> {
        if target.0.is_empty() || target.0.len() > 256 || target.0.chars().any(char::is_control) {
            return Err(BindingError::InvalidTarget);
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(BindingError::GenerationExhausted)?;
        self.generation = generation;
        self.target = None;
        self.pending = Some(target.clone());
        self.enabled = false;
        Ok(vec![
            BindAction::QuiesceInput,
            BindAction::Detach,
            BindAction::ClearTerminalState,
            BindAction::PresentNeutralFrame,
            BindAction::Attach { target, generation },
        ])
    }

    /// Completes attachment, yielding snapshot-before-enable actions, or settles neutral disabled.
    /// # Errors
    /// Returns stale/no-pending/attach failure errors. Attach failure still clears pending state.
    pub fn complete_attach(
        &mut self,
        generation: u64,
        attached: bool,
    ) -> Result<Vec<BindAction>, BindingError> {
        if generation != self.generation {
            return Err(BindingError::StaleGeneration);
        }
        let target = self.pending.take().ok_or(BindingError::NoPendingAttach)?;
        if !attached {
            self.target = None;
            self.enabled = false;
            return Err(BindingError::AttachFailed);
        }
        self.target = Some(target.clone());
        self.enabled = true;
        Ok(vec![
            BindAction::ApplySnapshot { target, generation },
            BindAction::EnableInput { generation },
        ])
    }

    /// Accepts frame/input/title/clipboard ingress only for the active enabled generation.
    /// # Errors
    /// Returns stale generation when the surface is pending, neutral, disabled, or superseded.
    pub fn accept_ingress(
        &self,
        generation: u64,
        _kind: IngressKind,
    ) -> Result<&TerminalTarget, BindingError> {
        if generation != self.generation || !self.enabled {
            return Err(BindingError::StaleGeneration);
        }
        self.target.as_ref().ok_or(BindingError::StaleGeneration)
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rebind_order_is_quiesce_detach_clear_neutral_attach_snapshot_enable() {
        let mut b = RendererBinding::default();
        let first = b.begin_rebind(TerminalTarget("a".into())).unwrap();
        assert_eq!(
            first[..4],
            [
                BindAction::QuiesceInput,
                BindAction::Detach,
                BindAction::ClearTerminalState,
                BindAction::PresentNeutralFrame
            ]
        );
        let finish = b.complete_attach(1, true).unwrap();
        assert!(matches!(finish[0], BindAction::ApplySnapshot { .. }));
        assert_eq!(finish[1], BindAction::EnableInput { generation: 1 });
    }
    #[test]
    fn stale_ingress_all_kinds_is_rejected_after_rebind() {
        let mut b = RendererBinding::default();
        b.begin_rebind(TerminalTarget("a".into())).unwrap();
        b.complete_attach(1, true).unwrap();
        b.begin_rebind(TerminalTarget("b".into())).unwrap();
        for kind in [
            IngressKind::Frame,
            IngressKind::Input,
            IngressKind::Title,
            IngressKind::Clipboard,
        ] {
            assert_eq!(
                b.accept_ingress(1, kind),
                Err(BindingError::StaleGeneration)
            );
        }
    }
    #[test]
    fn attach_failure_leaves_neutral_disabled_surface() {
        let mut b = RendererBinding::default();
        b.begin_rebind(TerminalTarget("a".into())).unwrap();
        assert_eq!(b.complete_attach(1, false), Err(BindingError::AttachFailed));
        assert!(!b.enabled());
        assert_eq!(
            b.accept_ingress(1, IngressKind::Frame),
            Err(BindingError::StaleGeneration)
        );
    }
    #[test]
    fn superseded_attach_cannot_capture_renderer() {
        let mut b = RendererBinding::default();
        b.begin_rebind(TerminalTarget("a".into())).unwrap();
        b.begin_rebind(TerminalTarget("b".into())).unwrap();
        assert_eq!(
            b.complete_attach(1, true),
            Err(BindingError::StaleGeneration)
        );
        b.complete_attach(2, true).unwrap();
        assert_eq!(
            b.accept_ingress(2, IngressKind::Input).unwrap(),
            &TerminalTarget("b".into())
        );
    }
    #[test]
    fn plans_never_contain_remote_stop_action() {
        let mut b = RendererBinding::default();
        let actions = b
            .begin_rebind(TerminalTarget("pty-retained".into()))
            .unwrap();
        assert_eq!(actions.len(), 5);
        assert_eq!(b.generation(), 1);
    }
}
