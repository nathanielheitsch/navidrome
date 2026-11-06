// nd-sched: owned by #12 (docs/rust-port/issues.md Part B)

//! Port of `scheduler/**`, `log/**` and `core/maintenance.go` semantics.

/// Scaffold marker kept until #12's public surface replaces it.
pub const OWNED_BY_ISSUE: u8 = 12;

pub mod crontab;
pub mod log_adapter;
pub mod schedule;
