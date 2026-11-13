//! Fail-closed configuration boundary for authentication routes.
//!
//! `server/subsonic/auth_limiter.go:39-49` allows all requests for a
//! non-positive limit. The canonical trunk deliberately rejects that
//! configuration instead (`docs/rust-port/issues.md` Part C #9).

use std::time::Duration;

use crate::limiter::{AuthLimiter, AuthSlot, LimiterError};

/// Failures constructing the canonical authentication limiter
/// (`server/subsonic/auth_limiter.go:39-49`, with Part C #9 deviation).
#[derive(Debug, thiserror::Error)]
pub enum GuardedLimiterError {
    /// A non-positive limit would otherwise silently disable rate limiting.
    #[error("authentication request limit must be positive")]
    MisconfiguredLimit,
    /// The limiter could not obtain entropy for its key hash.
    #[error(transparent)]
    Limiter(#[from] LimiterError),
}

/// A limiter which cannot be constructed in the Go allow-all configuration
/// (`server/subsonic/auth_limiter.go:39-49`, Part C #9).
#[derive(Debug)]
pub struct GuardedLimiter(AuthLimiter);

impl GuardedLimiter {
    /// Constructs a fail-closed authentication limiter
    /// (`server/subsonic/auth_limiter.go:39-49`, Part C #9 deviation).
    ///
    /// # Errors
    /// Returns [`GuardedLimiterError::MisconfiguredLimit`] for a non-positive
    /// limit, or an entropy error if the primitive cannot be initialized.
    pub fn new(limit: i64, window: Duration) -> Result<Self, GuardedLimiterError> {
        if limit <= 0 {
            return Err(GuardedLimiterError::MisconfiguredLimit);
        }
        Ok(Self(AuthLimiter::new(limit, window)?))
    }

    /// Reserves an authentication attempt (`auth_limiter.go:51-67`).
    ///
    /// # Errors
    /// Returns [`LimiterError::LimitReached`] after the configured failures.
    pub fn acquire(&self, key: &str) -> Result<AuthSlot<'_>, LimiterError> {
        self.0.acquire(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_positive_limits_and_gates_positive_limits() {
        // Arrange / Act / Assert: an allow-all configuration is unrepresentable.
        for limit in [i64::MIN, -1, 0] {
            assert!(matches!(
                GuardedLimiter::new(limit, Duration::from_secs(60)),
                Err(GuardedLimiterError::MisconfiguredLimit)
            ));
        }
        let limiter = GuardedLimiter::new(1, Duration::from_secs(60)).unwrap();
        // Act: a failed first attempt consumes the configured failure budget.
        limiter.acquire("client").unwrap().release(true);
        // Assert
        assert!(matches!(
            limiter.acquire("client"),
            Err(LimiterError::LimitReached)
        ));
        assert!(limiter.acquire("other-client").is_ok());
    }
}
