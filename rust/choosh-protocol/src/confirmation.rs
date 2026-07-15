//! Wire-independent lifecycle for destructive-operation confirmations.
//!
//! Callers supply both logical time and opaque tokens. This module never reads
//! a clock, generates randomness, or formats token material.

use std::collections::BTreeMap;
use std::fmt;

pub const MAX_CONFIRMATION_FIELD_BYTES: usize = 256;
pub const MAX_CONFIRMATION_TOKEN_BYTES: usize = 256;

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct ConfirmationToken(Vec<u8>);

impl ConfirmationToken {
    /// Retains a non-empty, bounded opaque token.
    ///
    /// # Errors
    ///
    /// Returns `invalid_token` when the token is empty or exceeds its bound.
    pub fn new(value: impl Into<Vec<u8>>) -> Result<Self, ConfirmationError> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_CONFIRMATION_TOKEN_BYTES {
            return Err(ConfirmationError::InvalidToken);
        }
        Ok(Self(value))
    }

    /// Exposes token bytes only for a trusted wire adapter.
    #[must_use]
    pub fn expose_for_transport(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for ConfirmationToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ConfirmationToken(<redacted>)")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfirmationScope {
    method: String,
    target_id: String,
    connection_id: String,
    user_id: String,
}

impl ConfirmationScope {
    /// Creates the exact operation and authenticated scope a challenge permits.
    ///
    /// # Errors
    ///
    /// Returns `invalid_scope` for empty, oversized, or control-bearing fields.
    pub fn new(
        method: impl Into<String>,
        target_id: impl Into<String>,
        connection_id: impl Into<String>,
        user_id: impl Into<String>,
    ) -> Result<Self, ConfirmationError> {
        let scope = Self {
            method: method.into(),
            target_id: target_id.into(),
            connection_id: connection_id.into(),
            user_id: user_id.into(),
        };
        if [
            &scope.method,
            &scope.target_id,
            &scope.connection_id,
            &scope.user_id,
        ]
        .into_iter()
        .any(|value| {
            value.is_empty()
                || value.len() > MAX_CONFIRMATION_FIELD_BYTES
                || value.chars().any(char::is_control)
        }) {
            return Err(ConfirmationError::InvalidScope);
        }
        Ok(scope)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfirmationError {
    InvalidLimit,
    InvalidToken,
    InvalidScope,
    InvalidTtl,
    ExpiryOverflow,
    OutstandingLimitExceeded,
    DuplicateToken,
    UnknownChallenge,
    ScopeMismatch,
    Expired,
}

impl ConfirmationError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimit => "invalid_limit",
            Self::InvalidToken => "invalid_token",
            Self::InvalidScope => "invalid_scope",
            Self::InvalidTtl => "invalid_ttl",
            Self::ExpiryOverflow => "expiry_overflow",
            Self::OutstandingLimitExceeded => "outstanding_limit_exceeded",
            Self::DuplicateToken => "duplicate_token",
            Self::UnknownChallenge => "unknown_challenge",
            Self::ScopeMismatch => "scope_mismatch",
            Self::Expired => "expired",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Challenge {
    scope: ConfirmationScope,
    expires_at: u64,
}

/// A bounded, connection-independent set of one-shot confirmation challenges.
#[derive(Debug)]
pub struct ConfirmationChallenges {
    max_outstanding: usize,
    challenges: BTreeMap<ConfirmationToken, Challenge>,
}

impl ConfirmationChallenges {
    /// Creates a challenge set with a positive bound.
    ///
    /// # Errors
    ///
    /// Returns `invalid_limit` when `max_outstanding` is zero.
    pub fn new(max_outstanding: usize) -> Result<Self, ConfirmationError> {
        if max_outstanding == 0 {
            return Err(ConfirmationError::InvalidLimit);
        }
        Ok(Self {
            max_outstanding,
            challenges: BTreeMap::new(),
        })
    }

    /// Registers caller-generated token material for exactly one operation.
    ///
    /// # Errors
    ///
    /// Returns a stable error when the TTL is zero, expiry arithmetic
    /// overflows, the token is already registered, or the outstanding bound
    /// has been reached. Failed registration does not mutate the set.
    pub fn issue(
        &mut self,
        token: ConfirmationToken,
        scope: ConfirmationScope,
        now: u64,
        ttl: u64,
    ) -> Result<u64, ConfirmationError> {
        if ttl == 0 {
            return Err(ConfirmationError::InvalidTtl);
        }
        let expires_at = now
            .checked_add(ttl)
            .ok_or(ConfirmationError::ExpiryOverflow)?;
        if self.challenges.contains_key(&token) {
            return Err(ConfirmationError::DuplicateToken);
        }
        if self.challenges.len() == self.max_outstanding {
            return Err(ConfirmationError::OutstandingLimitExceeded);
        }
        self.challenges
            .insert(token, Challenge { scope, expires_at });
        Ok(expires_at)
    }

    /// Consumes a live challenge if every bound scope field matches exactly.
    ///
    /// # Errors
    ///
    /// Returns `unknown_challenge` for an absent or replayed token,
    /// `expired` at or beyond its expiry, or `scope_mismatch` when any scope
    /// field differs. Expired challenges are removed; mismatches are retained.
    pub fn consume(
        &mut self,
        token: &ConfirmationToken,
        scope: &ConfirmationScope,
        now: u64,
    ) -> Result<(), ConfirmationError> {
        let challenge = self
            .challenges
            .get(token)
            .ok_or(ConfirmationError::UnknownChallenge)?;
        if now >= challenge.expires_at {
            self.challenges.remove(token);
            return Err(ConfirmationError::Expired);
        }
        if challenge.scope != *scope {
            return Err(ConfirmationError::ScopeMismatch);
        }
        self.challenges.remove(token);
        Ok(())
    }

    /// Removes all challenges whose expiry has been reached.
    pub fn remove_expired(&mut self, now: u64) -> usize {
        let before = self.challenges.len();
        self.challenges
            .retain(|_, challenge| now < challenge.expires_at);
        before - self.challenges.len()
    }

    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.challenges.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(value: u8) -> ConfirmationToken {
        ConfirmationToken::new(vec![value]).unwrap()
    }

    fn scope(method: &str, target: &str, connection: &str, user: &str) -> ConfirmationScope {
        ConfirmationScope::new(method, target, connection, user).unwrap()
    }

    #[test]
    fn exact_scope_is_required_and_success_is_one_shot() {
        let expected = scope(
            "workspace.terminate",
            "workspace-1",
            "connection-1",
            "user-1",
        );
        let mut challenges = ConfirmationChallenges::new(2).unwrap();
        challenges
            .issue(token(7), expected.clone(), 100, 10)
            .unwrap();

        let mismatches = [
            scope("service.stop", "workspace-1", "connection-1", "user-1"),
            scope(
                "workspace.terminate",
                "workspace-2",
                "connection-1",
                "user-1",
            ),
            scope(
                "workspace.terminate",
                "workspace-1",
                "connection-2",
                "user-1",
            ),
            scope(
                "workspace.terminate",
                "workspace-1",
                "connection-1",
                "user-2",
            ),
        ];
        for mismatch in mismatches {
            assert_eq!(
                challenges.consume(&token(7), &mismatch, 105),
                Err(ConfirmationError::ScopeMismatch)
            );
        }
        assert_eq!(challenges.outstanding(), 1);
        assert_eq!(challenges.consume(&token(7), &expected, 109), Ok(()));
        assert_eq!(
            challenges.consume(&token(7), &expected, 109),
            Err(ConfirmationError::UnknownChallenge)
        );
    }

    #[test]
    fn expiry_boundary_fails_and_cleans_up() {
        let expected = scope("service.stop", "service-1", "connection-1", "user-1");
        let mut challenges = ConfirmationChallenges::new(2).unwrap();
        challenges.issue(token(1), expected.clone(), 5, 3).unwrap();
        assert_eq!(
            challenges.consume(&token(1), &expected, 8),
            Err(ConfirmationError::Expired)
        );
        assert_eq!(challenges.outstanding(), 0);

        challenges.issue(token(2), expected.clone(), 8, 1).unwrap();
        challenges.issue(token(3), expected, 8, 2).unwrap();
        assert_eq!(challenges.remove_expired(9), 1);
        assert_eq!(challenges.outstanding(), 1);
    }

    #[test]
    fn issue_rejects_invalid_or_unbounded_state_without_mutation() {
        let expected = scope("service.stop", "service-1", "connection-1", "user-1");
        let mut challenges = ConfirmationChallenges::new(1).unwrap();
        assert_eq!(
            challenges.issue(token(1), expected.clone(), 0, 0),
            Err(ConfirmationError::InvalidTtl)
        );
        assert_eq!(
            challenges.issue(token(1), expected.clone(), u64::MAX, 1),
            Err(ConfirmationError::ExpiryOverflow)
        );
        challenges.issue(token(1), expected.clone(), 0, 1).unwrap();
        assert_eq!(
            challenges.issue(token(1), expected.clone(), 0, 1),
            Err(ConfirmationError::DuplicateToken)
        );
        assert_eq!(
            challenges.issue(token(2), expected, 0, 1),
            Err(ConfirmationError::OutstandingLimitExceeded)
        );
        assert_eq!(challenges.outstanding(), 1);
    }

    #[test]
    fn validation_and_errors_are_stable_and_tokens_are_redacted() {
        assert_eq!(
            ConfirmationChallenges::new(0).unwrap_err(),
            ConfirmationError::InvalidLimit
        );
        assert_eq!(
            ConfirmationToken::new(Vec::new()).unwrap_err().code(),
            "invalid_token"
        );
        assert_eq!(
            ConfirmationScope::new("", "target", "connection", "user").unwrap_err(),
            ConfirmationError::InvalidScope
        );
        assert_eq!(format!("{:?}", token(42)), "ConfirmationToken(<redacted>)");
        assert!(!format!("{:?}", token(42)).contains("42"));
    }
}
