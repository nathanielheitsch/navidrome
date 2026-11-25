//! Explicit shutdown signal for the HTTP shell (BOOT-03 shell scope).
//!
//! The lifecycle contract (`subplans/boot-api.md:255`) requires shutdown to be
//! *signalled from outside* rather than assumed: stop accepts, wake owners,
//! await cleanup, then report an honest outcome. This module provides only the
//! signal itself plus the drain/deadline outcome types. It does **not** claim
//! body-bridge, cache-producer, stream, or DB-close ordering — those owners do
//! not exist yet, and `subplans/boot-api.md:255` explicitly forbids claiming
//! drain observability before API-04 adds it.

use tokio::sync::watch;

/// Cancellation signal shared by the acceptor and any later owner.
///
/// Cancellation signal shared by the acceptor and any later owner.
///
/// The signalled state is **persistent storage**, independent of how many
/// receivers exist: `signal()` uses `send_replace`, which writes unconditionally,
/// whereas `send` would return `Err` and store nothing when the receiver count
/// is zero. That distinction is exactly the lost-early-shutdown bug: a signal
/// fired before any owner subscribed (e.g. shutdown racing startup, before the
/// accept loop has built its `with_graceful_shutdown` future) must still stop
/// the acceptor. Waiters read the current state before awaiting a change, so an
/// already-persisted signal is observed without a notification.
#[derive(Clone, Debug)]
pub struct Shutdown {
    tx: watch::Sender<bool>,
}

impl Shutdown {
    /// A fresh, unsignalled shutdown channel.
    #[must_use]
    pub fn new() -> Self {
        // The initial receiver is deliberately dropped: `signal` must work with
        // zero receivers, which is why it uses `send_replace` rather than `send`.
        let (tx, _rx) = watch::channel(false);
        Self { tx }
    }

    /// Fire the signal. Idempotent, works with zero receivers, and safe to call
    /// from a sync context.
    pub fn signal(&self) {
        // `send_replace` writes unconditionally regardless of receiver count and
        // notifies any existing receivers; only `false` could ever be swapped
        // out, and no owner ever sends, so this cannot fail meaningfully.
        let _ = self.tx.send_replace(true);
    }

    /// Whether the signal has already been fired.
    #[must_use]
    pub fn signalled(&self) -> bool {
        *self.tx.borrow()
    }

    /// Resolve once the signal has fired.
    ///
    /// The current state is inspected **before** awaiting a change, so a signal
    /// that was persisted before this subscriber existed is observed
    /// immediately; a subscriber created after the notification still sees it,
    /// because the value (not just the notification) carries the state.
    pub async fn wait(&self) {
        let mut rx = self.tx.subscribe();
        loop {
            if *rx.borrow_and_update() {
                return;
            }
            if rx.changed().await.is_err() {
                // Sender dropped without signalling: treat the channel as gone,
                // which is a shutdown.
                return;
            }
        }
    }
}

impl Default for Shutdown {
    fn default() -> Self {
        Self::new()
    }
}

/// Honest result of stopping and joining the acceptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShutdownOutcome {
    /// Accepts stopped and the serve future completed normally: exit zero.
    Drained,
    /// The graceful deadline elapsed while the acceptor was still running.
    /// Reported as **incomplete**, never as success (`subplans/boot-api.md:255`:
    /// "timeout/pending cleanup -> report nonzero/incomplete"). axum exposes no
    /// count of in-flight connections, so this says nothing about how many
    /// remain; drain observability is API-04 work.
    DeadlineElapsed {
        /// Deadline that was applied, in milliseconds.
        deadline_ms: u64,
    },
    /// The serve future failed. The listener error text is carried so the
    /// supervisor can report nonzero with a cause
    /// (`subplans/boot-api.md:255`: "listener serve error -> cancel supervisor,
    /// stop admission, return nonzero").
    ServeFailed {
        /// Displayed I/O error from the accept loop.
        reason: String,
    },
    /// The acceptor task was cancelled before it could be joined, so its
    /// completion was never observed. Joining must be observed before success;
    /// this is never a success.
    NotJoined,
}

impl ShutdownOutcome {
    /// Whether this outcome may be reported as a normal zero exit.
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, ShutdownOutcome::Drained)
    }
}
