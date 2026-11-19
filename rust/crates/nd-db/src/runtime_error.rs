//! Typed failures for the additive transaction-runtime seam.

use std::error::Error;

use crate::store::StoreError;

/// The transaction phase that produced a SQLite failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionStage {
    /// SQLite could not start the transaction.
    Begin,
    /// The transaction callback returned a SQLite failure.
    Body,
    /// SQLite could not commit the transaction.
    Commit,
}

impl std::fmt::Display for TransactionStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Begin => "begin",
            Self::Body => "body",
            Self::Commit => "commit",
        })
    }
}

/// Callback failure accepted by the typed transaction API.
///
/// This deliberately has no transaction stage: the executor assigns `Body` to
/// callback SQLite failures and cannot receive callback-forged cleanup/pool errors.
#[derive(Debug, thiserror::Error)]
pub enum DbOperationError<E: Error + 'static> {
    /// A domain failure from the operation; the concrete value remains available.
    #[error("{0}")]
    Domain(#[source] E),

    /// A concrete SQLite failure produced by the callback.
    #[error("{0}")]
    Sqlite(#[source] rusqlite::Error),
}

impl<E: Error + 'static> DbOperationError<E> {
    /// Wrap a SQLite error produced by the callback body.
    #[must_use]
    pub fn sqlite(source: rusqlite::Error) -> Self {
        Self::Sqlite(source)
    }
}

/// Typed error boundary for new transaction operations.
///
/// SQLite variants retain the concrete `rusqlite::Error` as their source,
/// including extended result codes. A rollback failure retains both the first
/// operation error and the concrete rollback error; affected pooled connections
/// are discarded rather than returned dirty.
#[derive(Debug, thiserror::Error)]
pub enum DbRuntimeError<E: Error + 'static> {
    /// A domain failure from the operation.
    #[error("{0}")]
    Domain(#[source] E),

    /// A SQLite failure during transaction setup, the callback, or commit.
    #[error("SQLite transaction {stage} failed: {source}")]
    Sqlite {
        /// Failure phase.
        stage: TransactionStage,
        /// Original SQLite error, retained for typed inspection.
        #[source]
        source: rusqlite::Error,
    },

    /// Retry attempts were exhausted by SQLite busy failures.
    #[error("SQLite transaction {stage} remained busy after {attempts} attempts: {source}")]
    BusyExhausted {
        /// Total transactions attempted.
        attempts: u32,
        /// Phase of the final busy failure.
        stage: TransactionStage,
        /// Original final SQLite error.
        #[source]
        source: rusqlite::Error,
    },

    /// Rollback itself failed after the operation had already failed.
    #[error("transaction cleanup failed after {original}: {rollback}")]
    RollbackFailed {
        /// The original callback or commit failure that triggered cleanup.
        #[source]
        original: Box<Self>,
        /// Concrete error returned by SQLite's rollback attempt.
        rollback: rusqlite::Error,
    },

    /// Pool checkout or blocking-task failure, retained without stringification.
    #[error("{0}")]
    Pool(#[source] StoreError),
}

impl<E: Error + 'static> DbRuntimeError<E> {
    /// Return the SQLite error from a failed rollback, if cleanup failed.
    #[must_use]
    pub fn rollback_error(&self) -> Option<&rusqlite::Error> {
        match self {
            Self::RollbackFailed { rollback, .. } => Some(rollback),
            _ => None,
        }
    }
}
