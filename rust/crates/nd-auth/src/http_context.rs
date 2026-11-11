//! Request-scoped token epoch reporting for response refresh.
//!
//! This is the explicit request-owned equivalent of
//! `model/request/request.go:156-177` and the `Put` refresh condition in
//! `persistence/user_repository.go:180-185`. A handler reports the repository
//! outcome only after the transaction containing it has committed.

use std::sync::{Arc, Mutex};

use nd_db::user_repo::PutOutcome;

/// Shared per-request epoch slot (`model/request/request.go:156-177`).
/// Clones refer to the same slot; create a fresh value for each request.
#[derive(Clone, Debug, Default)]
pub struct TokenEpochHolder(Arc<Mutex<Option<i64>>>);

impl TokenEpochHolder {
    /// Records a committed user put's post-bump epoch only for the caller's
    /// own password change (`persistence/user_repository.go:180-185`).
    pub fn record_put(&self, outcome: &PutOutcome) {
        if let (true, Some(epoch)) = (outcome.refresh_inflight_token, outcome.token_epoch) {
            *self
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(epoch);
        }
    }

    /// Returns the latest reported epoch (`model/request/request.go:170-177`).
    #[must_use]
    pub fn epoch(&self) -> Option<i64> {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_only_refreshable_puts_and_shares_the_slot() {
        // Arrange
        let holder = TokenEpochHolder::default();
        let handler = holder.clone();
        let mut put = PutOutcome {
            is_new_user: false,
            token_epoch: Some(7),
            refresh_inflight_token: false,
        };
        // Act / Assert: another user's password change must not alter the token.
        handler.record_put(&put);
        assert_eq!(holder.epoch(), None);
        put.refresh_inflight_token = true;
        handler.record_put(&put);
        assert_eq!(holder.epoch(), Some(7));
        put.token_epoch = None;
        handler.record_put(&put);
        assert_eq!(holder.epoch(), Some(7));
    }
}
