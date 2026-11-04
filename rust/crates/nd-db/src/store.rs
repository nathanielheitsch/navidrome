//! #05 slice 1 — transactions with busy retry (`persistence/persistence.go`).
//!
//! Go models retry as an ambient `context.Context` value: `WithTxRetry`
//! (`persistence.go:207-228`) marks each attempt with `withBusyRetry`
//! (`persistence.go:234`) and statements consult the flag to downgrade logging.
//! The port classifies at the error site instead — same observable semantics
//! (`db/db.go:128-140` is the truth table, `db/db_test.go:26-36`), no ambient
//! context. [`WithTx`] models `persistence.go:165-185` (`Transactional` over a
//! fresh `dbx.DB`); [`with_tx_retry`] models `persistence.go:207-228` with
//! [`TX_MAX_RETRIES`] = `txMaxRetries` (`persistence.go:205`) and
//! [`TX_RETRY_DELAY`] = `txRetryDelay` (`persistence.go:203`).

use std::time::Duration;

use rusqlite::ffi;
use rusqlite::Error as RsError;

/// `txRetryDelay` (`persistence.go:203`): spacing between transaction reruns;
/// each attempt has already waited out the busy timeout.
pub const TX_RETRY_DELAY: Duration = Duration::from_secs(5);

/// `txMaxRetries` (`persistence.go:205`): give up after this many retries.
pub const TX_MAX_RETRIES: u32 = 3;

/// Failures surfaced by the nd-db store/pool layer.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A pooled connection (or its per-connection extensions) failed to open.
    #[error("cannot open pooled connection: {0}")]
    Open(String),

    /// A transaction was abandoned after [`TX_MAX_RETRIES`] busy retries.
    #[error("database busy, gave up after {attempts} retries: {cause}")]
    BusyRetriesExhausted {
        /// Attempts made (retries + 1).
        attempts: u32,
        /// Last busy error.
        cause: String,
    },

    /// The idle-queue mutex was poisoned by a panic in a lease holder.
    #[error("nd-db pool state poisoned by a panic in a previous lease holder")]
    Poisoned,

    /// Everything else, classified not-busy: fails immediately
    /// (`persistence.go:218` `!db.IsBusy(err)` branch).
    #[error(transparent)]
    Other(#[from] RsError),
}

/// `db/db.go:128-140 ErrorCodes`/`IsBusy`: `SQLITE_BUSY` (extended code 5,
/// including `SQLITE_BUSY_SNAPSHOT`) and `SQLITE_LOCKED` (extended code 17,
/// including `SQLITE_LOCKED_SHAREDCACHE`) are the lock-class failures the
/// transaction retry may clear; everything else fails immediately
/// (`persistence.go:218`).
#[must_use]
pub fn is_busy(err: &RsError) -> bool {
    match err {
        RsError::SqliteFailure(failure, _) => {
            // Extended codes: 5 = SQLITE_BUSY | SQLITE_BUSY_SNAPSHOT (+ RECOVERY,
            // + TIMEOUT), 17 = SQLITE_LOCKED | SQLITE_LOCKED_SHAREDCACHE (+ VTAB).
            // Match on the low byte, mirroring Go's `code == ErrBusy` comparison
            // of the base code (`db/db.go:139`) while keeping LOCKED retryable
            // per the slice-1 decision (next-work.md:71-76).
            failure.extended_code & 0xff == ffi::SQLITE_BUSY
                || failure.extended_code & 0xff == ffi::SQLITE_LOCKED
        }
        _ => false,
    }
}

/// Runs `block` inside a rusqlite transaction on `conn`, modelling
/// `persistence.go:165-185` `WithTx` (BEGIN/commit/rollback with the dbx
/// `Transactional` semantics: block error → rollback, block success → commit).
///
/// # Errors
/// [`StoreError`] as classified by [`classify`]; the transaction rolls back on
/// any `block` error (rusqlite default `DropBehavior::Rollback`).
pub fn with_tx<T>(
    conn: &mut rusqlite::Connection,
    block: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, RsError>,
) -> Result<T, StoreError> {
    let tx = match conn.transaction() {
        Ok(tx) => tx,
        Err(e) => return Err(classify(e)),
    };
    let outcome = block(&tx);
    match outcome {
        Ok(value) => match tx.commit() {
            Ok(()) => Ok(value),
            Err(e) => Err(classify(e)),
        },
        Err(e) => {
            // DropBehavior::Rollback runs on drop; make the rollback explicit so
            // a poisoned `conn` state can never leak past this call.
            let _ = tx.finish();
            Err(classify(e))
        }
    }
}

/// Runs `block` in a transaction, retrying busy-class failures up to
/// [`TX_MAX_RETRIES`] times, modelling `persistence.go:207-228` `WithTxRetry`.
/// The backoff between attempts is `(attempt + 1) * TX_RETRY_DELAY` — 5s, 10s,
/// 15s (`persistence.go:225`), not a flat 5s; tests inject [`Delay`] to avoid
/// real sleeps.
///
/// # Errors
/// [`StoreError::BusyRetriesExhausted`] when retries are spent on busy-class
/// failures, [`StoreError::Other`] for any non-busy failure (first occurrence
/// fails immediately, `persistence.go:218`).
pub fn with_tx_retry<T>(
    conn: &mut rusqlite::Connection,
    delay: &dyn Fn(u32) -> Duration,
    block: impl Fn(&rusqlite::Transaction<'_>) -> Result<T, RsError>,
) -> Result<T, StoreError> {
    let mut attempt: u32 = 0;
    loop {
        let result = with_tx(conn, &block);
        match result {
            Ok(value) => return Ok(value),
            Err(StoreError::Other(e)) if is_busy(&e) => {
                if attempt >= TX_MAX_RETRIES {
                    return Err(StoreError::BusyRetriesExhausted {
                        attempts: attempt + 1,
                        cause: e.to_string(),
                    });
                }
                std::thread::sleep(delay(attempt));
                attempt += 1;
            }
            Err(other) => return Err(other),
        }
    }
}

/// Maps a rusqlite failure into [`StoreError`]: busy-class failures stay in
/// [`StoreError::Other`] (the retry loop re-classifies via [`is_busy`],
/// mirroring `persistence.go:218` `!db.IsBusy(err)`), everything else fails
/// immediately.
///
/// # Errors
/// Always returns the classified [`StoreError`].
pub fn classify(err: RsError) -> StoreError {
    StoreError::Other(err)
}

/// Test seam for the retry backoff (`shortcut: tests inject a zero delay here
/// instead of sleeping 15s; upgrade when a tokio-time based store lands in
/// slices 2-4 and the real [`TX_RETRY_DELAY`] schedule can be exercised
/// end-to-end`).
pub type Delay = dyn Fn(u32) -> Duration;

/// The production backoff: `(attempt + 1) * TX_RETRY_DELAY`
/// (`persistence.go:225`, increasing 5s/10s/15s).
#[must_use]
pub fn production_delay(attempt: u32) -> Duration {
    TX_RETRY_DELAY * (attempt + 1)
}
