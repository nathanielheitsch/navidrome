//! #05 slice 1 — the SQLite connection pool (`db/db.go:61-62`).
//!
//! Go bounds the shared `sql.DB` with `db.SetMaxOpenConns(conf.MaxOpenConns())`
//! where `MaxOpenConns()` is `max(4, runtime.NumCPU())` (`conf/configuration.go:1213-1216`):
//! more connections buy reader parallelism while blocked on disk I/O, not write
//! parallelism (SQLite serializes writers on a single database-wide write lock,
//! `configuration.go:1211` comment). The port keeps that shape as a fixed pool of
//! owned [`rusqlite::Connection`]s behind one [`std::sync::Mutex`]: there is no
//! pool crate and no async connection surface — rusqlite stays synchronous inside
//! the crate and the async boundary is [`tokio::task::spawn_blocking`] at the
//! store verb boundary ([`crate::store`]).
//!
//! Every pooled connection is opened through
//! [`nd_dbschema::schema::open_with_options`], so the `SEEDEDRAND` scalar function
//! and the `NATSORT` collation are registered per connection (`db/db.go:44-51`
//! ConnectHook; schema-contract.md §4 requires the same per-connection
//! registration here) AND the [`DbOpenOptions`] the DSN asks for are applied as
//! explicit `PRAGMA` statements whose effective state is read back — rusqlite
//! applies none of go-sqlite3's DSN pragmas (D-OPEN decision, option a,
//! `docs/rust-port/subplans/db-runtime.md`), so a handle that does not hold the
//! requested settings never enters the pool.
//!
//! What a handle starts with, pinned honestly:
//!   * `PRAGMA foreign_keys` reads **1** on a plain `rusqlite::Connection::open`.
//!     That is not a rusqlite setting and not go-sqlite3's behavior — Go leaves FK
//!     alone unless the DSN sets it (`sqlite3.go:1234`, measured `fk=0` on a bare
//!     path). It is a compile-time default of this build: `libsqlite3-sys` 0.30.1
//!     compiles the SQLite amalgamation with `-DSQLITE_DEFAULT_FOREIGN_KEYS=1`
//!     (build.rs:123). [`DbOpenOptions::foreign_keys`] is therefore `Option`: when
//!     the DSN names nothing, the port emits no pragma and that compiled default
//!     stays in charge, exactly as it does today.
//!   * rusqlite installs `busy_timeout = 5000` on open
//!     (`rusqlite/src/inner_connection.rs:119`), which happens to equal go-sqlite3's
//!     driver default (`sqlite3.go:1231`); Navidrome's 15000 from
//!     `consts.DefaultDbPath` needs an explicit pragma.

use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(test)]
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};

use nd_dbschema::schema::{effective_state, DbOpenOptions, EffectiveState, SEEDEDRAND_SQL};
use rusqlite::Connection;

use crate::store::StoreError;

#[derive(Debug)]
struct PoolShared {
    live: AtomicUsize,
    idle: Mutex<Vec<Connection>>,
    available: Condvar,
    #[cfg(test)]
    wait_observer: Mutex<Option<Sender<()>>>,
}

/// Pool size: `max(4, available_parallelism())` — `conf/configuration.go:1217-1219`
/// `MaxOpenConns()`. NOT 1: the `SetMaxOpenConns(1)` in the Go tree is the e2e
/// harness's test setting (`server/subsonic/e2e/e2e_suite_test.go:243`), not the
/// production value.
pub fn pool_size() -> usize {
    match std::thread::available_parallelism() {
        Ok(n) => std::cmp::max(MIN_POOL_SIZE, n.get()),
        Err(_) => MIN_POOL_SIZE,
    }
}

/// Floor of [`pool_size`] (`conf/configuration.go:1217-1218`).
pub const MIN_POOL_SIZE: usize = 4;

/// The path and per-handle settings the pool opens every connection against.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// DB path or DSN. `nd_dbschema::schema::open` translates it: `:memory:` is
    /// rewritten (`db/db.go:54-55`), and the go-sqlite3 DSN parameters
    /// (`_journal_mode`, `_busy_timeout`, `_foreign_keys`, `synchronous`) become the
    /// open options below instead of part of the filename.
    pub path: String,
    /// Fixed pool size; callers use [`pool_size`] unless a test pins it.
    pub size: usize,
    /// Explicit per-handle settings. `None` (the default) uses what the DSN itself
    /// resolves to — Navidrome's preset for its default DSN, go-sqlite3's driver
    /// state for anything else — which is how Go's `sql.Open(Driver, Path)` behaves
    /// (`db/db.go:61`).
    pub options: Option<DbOpenOptions>,
}

impl PoolConfig {
    /// Config with the production pool size (`conf/configuration.go:1217-1219`) and
    /// no explicit open options: each handle takes the settings implied by `path`.
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            size: pool_size(),
            options: None,
        }
    }

    /// Config with an explicit size (tests, harnesses) and no explicit open options.
    #[must_use]
    pub fn with_size(path: impl Into<String>, size: usize) -> Self {
        Self {
            path: path.into(),
            size: size.max(1),
            options: None,
        }
    }

    /// Override the per-handle settings, whatever the DSN asks for. Fields left as
    /// `None` emit no pragma, matching go-sqlite3's conditional execs.
    #[must_use]
    pub fn with_open_options(mut self, options: DbOpenOptions) -> Self {
        self.options = Some(options);
        self
    }
}

/// Pool-bound held-out handle. Only [`Pool::new`] constructs one. It cannot be
/// detached or replaced: immutable Deref only, no DerefMut or extraction method.
/// `apply_fresh_schema` takes `&Connection`, so immutable access suffices for DDL.
/// Foreign raw connections (including read-only or private-memory handles) are
/// structurally impossible to admit; state checks additionally defend issued tokens.
///
/// A raw read-only handle is not an admission token:
/// ```compile_fail
/// use nd_db::pool::{Pool, PoolConfig};
/// let (pool, _first) = Pool::new(&PoolConfig::with_size("file::memory:", 1)).unwrap();
/// let raw = nd_dbschema::schema::open("file::memory:?mode=ro").unwrap();
/// pool.put_back(raw).unwrap();
/// ```
/// A raw private-memory handle is not an admission token either:
/// ```compile_fail
/// use nd_db::pool::{Pool, PoolConfig};
/// let (pool, _first) = Pool::new(&PoolConfig::with_size(":memory:", 1)).unwrap();
/// let raw = nd_dbschema::schema::open("file::memory:").unwrap();
/// pool.put_back(raw).unwrap();
/// ```
/// Tokens cannot be forged or their connection replaced:
/// ```compile_fail
/// let token = nd_db::pool::Initializer { conn: None, live: Default::default() };
/// ```
/// ```compile_fail
/// use nd_db::pool::{Pool, PoolConfig};
/// let (_pool, mut token) = Pool::new(&PoolConfig::with_size(":memory:", 1)).unwrap();
/// let detached = std::mem::replace(&mut *token, rusqlite::Connection::open_in_memory().unwrap());
/// ```
#[derive(Debug)]
pub struct Initializer {
    conn: Option<Connection>,
    shared: Arc<PoolShared>,
}

impl std::ops::Deref for Initializer {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        self.conn
            .as_ref()
            .expect("initializer is only empty during admission")
    }
}

impl Drop for Initializer {
    fn drop(&mut self) {
        if self.conn.take().is_some() {
            match self.shared.idle.lock() {
                Ok(_idle) => {
                    self.shared.live.fetch_sub(1, Ordering::AcqRel);
                    drop(_idle);
                }
                Err(_poisoned) => {
                    self.shared.live.fetch_sub(1, Ordering::AcqRel);
                }
            }
            self.shared.available.notify_all();
        }
    }
}

/// The lease handed out by [`Pool::checkout`]: on drop it returns its connection
/// to the idle queue, mirroring Go's `database/sql` implicit release.
/// No public API yields `&mut Connection`, so a lease's handle cannot be substituted.
///
/// ```compile_fail
/// use nd_db::pool::{Pool, PoolConfig};
/// let (pool, first) = Pool::new(&PoolConfig::with_size(":memory:", 1)).unwrap();
/// pool.put_back(first).unwrap();
/// let mut lease = pool.checkout().unwrap();
/// let detached = std::mem::replace(lease.conn_mut(), rusqlite::Connection::open_in_memory().unwrap());
/// ```
#[derive(Debug)]
pub struct Lease<'a> {
    pool: &'a Pool,
    conn: Option<Connection>,
}

impl Lease<'_> {
    /// The connection for the duration of the lease.
    #[must_use]
    pub fn conn(&self) -> &Connection {
        self.conn
            .as_ref()
            .expect("lease connection is only None while being returned")
    }

    /// Mutable access (rusqlite transactions take `&mut Connection`,
    /// rusqlite transaction.rs:416).
    pub(crate) fn conn_mut(&mut self) -> &mut Connection {
        self.conn
            .as_mut()
            .expect("lease connection is only None while being returned")
    }

    /// Start a transaction without exposing a replaceable `&mut Connection`.
    /// The transaction borrows this lease. A nested checkout on the same thread
    /// can deadlock when all slots are held. Transaction drop resolves its configured
    /// commit/rollback behavior (rollback by default) before the lease can return.
    ///
    /// # Errors
    /// Returns SQLite's transaction-start error.
    pub fn transaction(&mut self) -> Result<rusqlite::Transaction<'_>, rusqlite::Error> {
        self.conn_mut().transaction()
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.pool.checkin(conn);
        }
    }
}

/// Fixed pool of owned connections (`db/db.go:62 SetMaxOpenConns`).
#[derive(Debug)]
pub struct Pool {
    config: PoolConfig,
    /// State of a freshly configured handle, before the migration temporarily
    /// disables FK. Returned external handles must match this snapshot.
    initial_state: EffectiveState,
    shared: Arc<PoolShared>,
}

impl Pool {
    /// Opens `config.size` connections through
    /// `nd_dbschema::schema::open_with_options` — per-connection extension
    /// registration (schema-contract.md §4) plus the open options applied as
    /// explicit pragmas and read back (D-OPEN decision, option a) — and returns the
    /// pool together with its **first** handle, which is deliberately not put in
    /// the idle queue.
    ///
    /// That handle is where schema initialization belongs:
    /// `nd_dbschema::schema::apply_fresh_schema` toggles `PRAGMA foreign_keys` for
    /// its DDL pass, so running it on a connection the pool does not own means a
    /// failed restore cannot put a mis-configured handle into the idle queue — the
    /// handle stays outside the pool and the error names the leaked state
    /// (`schema.rs`, DB-03 point 4). Once initialized, hand it back with
    /// [`Pool::put_back`] to reach `config.size` idle connections. Dropping it
    /// instead closes that handle and leaves the pool with `config.size - 1`
    /// connections.
    ///
    /// # Errors
    /// [`StoreError::Open`] when the input is rejected, a connection cannot be
    /// opened, its per-connection extensions fail to register, or its effective
    /// state does not match the requested options.
    pub fn new(config: &PoolConfig) -> Result<(Self, Initializer), StoreError> {
        let mut idle = Vec::with_capacity(config.size.max(1) - 1);
        let mut first: Option<Connection> = None;
        for _ in 0..config.size.max(1) {
            match nd_dbschema::schema::open_with_options(&config.path, config.options) {
                Ok(conn) => {
                    if first.is_none() {
                        first = Some(conn);
                    } else {
                        idle.push(conn);
                    }
                }
                Err(e) => return Err(StoreError::Open(e.to_string())),
            }
        }
        let first = first.expect("at least one connection is opened");
        let initial_state = effective_state(&first).map_err(|e| StoreError::Open(e.to_string()))?;
        let shared = Arc::new(PoolShared {
            live: AtomicUsize::new(config.size.max(1)),
            idle: Mutex::new(idle),
            available: Condvar::new(),
            #[cfg(test)]
            wait_observer: Mutex::new(None),
        });
        let first = Initializer {
            conn: Some(first),
            shared: Arc::clone(&shared),
        };
        let pool = Self {
            config: PoolConfig {
                size: config.size.max(1),
                ..config.clone()
            },
            initial_state,
            shared,
        };
        Ok((pool, first))
    }

    /// Return only this pool's held-out token, waking one borrower. Raw Connection
    /// admission is deliberately unavailable: equal pragmas/extensions do not prove
    /// identity (read-only and private-memory handles can have identical state).
    /// Rejected tokens are closed and decrement their issuing pool's live count.
    /// Capacity is structural: new opens exactly size handles, including ONE token,
    /// and no API mints a replacement or admits raw connections. Transfers cannot
    /// increase live count; a valid held-out token implies idle <= size - 1. Thus
    /// neither live > size nor a full idle queue is reachable at owned admission;
    /// redundant arithmetic guards would not enforce any additional invariant.
    /// The blocked-checkout test `r5_h3_live_capacity_includes_lease_and_transaction`
    /// proves real capacity; the foreign-owner test proves owner refusal/accounting.
    ///
    /// # Errors
    /// [`StoreError::Open`] for a foreign token, changed effective
    /// state (including cache_size), or missing SEEDEDRAND/NATSORT.
    /// [`StoreError::Poisoned`] if the idle mutex was poisoned.
    pub fn put_back(&self, mut first: Initializer) -> Result<(), StoreError> {
        self.check_owner(&first)?;
        let conn = &*first;
        let mut idle = self.lock_idle()?;
        let got = effective_state(conn).map_err(|e| StoreError::Open(e.to_string()))?;
        if got != self.initial_state {
            return Err(StoreError::Open(format!(
                "returned connection state differs: expected {:?}, got {got:?}",
                self.initial_state
            )));
        }
        conn.query_row(SEEDEDRAND_SQL, [], |row| row.get::<_, i64>(0))
            .map_err(|e| StoreError::Open(format!("returned connection SEEDEDRAND: {e}")))?;
        let natural: bool = conn
            .query_row("SELECT 'track2' COLLATE NATSORT < 'track10'", [], |row| {
                row.get(0)
            })
            .map_err(|e| StoreError::Open(format!("returned connection NATSORT: {e}")))?;
        if !natural {
            return Err(StoreError::Open(
                "returned connection NATSORT failed".to_string(),
            ));
        }
        idle.push(
            first
                .conn
                .take()
                .expect("validated initializer holds a connection"),
        );
        drop(idle);
        self.shared.available.notify_one();
        Ok(())
    }

    /// Initialize the issued token, then admit it. An error or panic closes the
    /// token without queuing it, and decrements live accounting.
    ///
    /// # Errors
    /// Propagates the initializer's error or [`Pool::put_back`]'s admission error.
    pub fn initialize(
        &self,
        first: Initializer,
        init: impl FnOnce(&Connection) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.check_owner(&first)?;
        init(&first)?;
        self.put_back(first)
    }

    fn check_owner(&self, first: &Initializer) -> Result<(), StoreError> {
        if !Arc::ptr_eq(&self.shared, &first.shared) {
            return Err(StoreError::Open(
                "initializer does not belong to this pool".to_string(),
            ));
        }
        Ok(())
    }

    /// Live owned handles, including leases and the issued initializer token.
    #[must_use]
    pub fn live_count(&self) -> usize {
        self.shared.live.load(Ordering::Acquire)
    }

    /// Borrows one idle connection, blocking until one is returned. Depletion and
    /// mutex poisoning are reported distinctly rather than waiting forever.
    ///
    /// # Errors
    /// [`StoreError::Poisoned`] for poisoned state; [`StoreError::Depleted`] when
    /// no owned connections remain and none can be returned.
    pub fn checkout(&self) -> Result<Lease<'_>, StoreError> {
        let mut idle = self.lock_idle()?;
        loop {
            if let Some(conn) = idle.pop() {
                return Ok(Lease {
                    pool: self,
                    conn: Some(conn),
                });
            }
            if self.shared.live.load(Ordering::Acquire) == 0 {
                return Err(StoreError::Depleted);
            }
            #[cfg(test)]
            if let Ok(observer) = self.shared.wait_observer.lock() {
                if let Some(observer) = observer.as_ref() {
                    let _observed = observer.send(());
                }
            }
            idle = match self.shared.available.wait(idle) {
                Ok(guard) => guard,
                Err(_poisoned) => return Err(StoreError::Poisoned),
            };
        }
    }

    /// Number of currently idle connections (tests and metrics).
    ///
    /// # Errors
    /// [`StoreError::Poisoned`] when the idle queue mutex was poisoned.
    pub fn idle_count(&self) -> Result<usize, StoreError> {
        Ok(self.lock_idle()?.len())
    }

    /// Configured pool size.
    #[must_use]
    pub fn size(&self) -> usize {
        self.config.size
    }

    /// Configured DB path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.config.path
    }

    fn lock_idle(&self) -> Result<std::sync::MutexGuard<'_, Vec<Connection>>, StoreError> {
        match self.shared.idle.lock() {
            Ok(guard) => Ok(guard),
            Err(_poisoned) => Err(StoreError::Poisoned),
        }
    }

    fn checkin(&self, conn: Connection) {
        let mut idle = match self.shared.idle.lock() {
            Ok(idle) => idle,
            Err(_poisoned) => {
                drop(conn);
                self.shared.live.fetch_sub(1, Ordering::AcqRel);
                self.shared.available.notify_all();
                return;
            }
        };
        if !conn.is_autocommit() {
            drop(conn);
            self.shared.live.fetch_sub(1, Ordering::AcqRel);
            drop(idle);
            self.shared.available.notify_all();
            return;
        }
        idle.push(conn);
        drop(idle);
        self.shared.available.notify_one();
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        let mut idle = self
            .shared
            .idle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let removed = idle.len();
        idle.clear();
        self.shared.live.fetch_sub(removed, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod checkout_wait_tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn dropping_initializer_notifies_a_checkout_known_to_be_waiting() {
        let (pool, initializer) = Pool::new(&PoolConfig::with_size(":memory:", 1)).unwrap();
        let pool = Arc::new(pool);
        let (wait_started_tx, wait_started_rx) = mpsc::channel();
        let (completed_tx, completed_rx) = mpsc::channel();
        *pool.shared.wait_observer.lock().unwrap() = Some(wait_started_tx);

        let waiter_pool = Arc::clone(&pool);
        let waiter = std::thread::spawn(move || {
            let depleted = matches!(waiter_pool.checkout(), Err(StoreError::Depleted));
            completed_tx.send(depleted).unwrap();
            depleted
        });
        let wait_started = wait_started_rx.recv_timeout(Duration::from_secs(1)).is_ok();

        if wait_started {
            // This same mutex cannot be acquired until Condvar::wait has atomically
            // released it, proving the waiter is blocked before the initializer drops.
            let idle = pool.shared.idle.lock().unwrap();
            drop(idle);
        }
        drop(initializer);

        let completed = match completed_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(result) => Some(result),
            Err(_timeout) => {
                // A missing notification is the mutation under test. Wake the worker
                // only for bounded cleanup, then fail the assertion below.
                pool.shared.available.notify_all();
                let _cleanup = completed_rx.recv_timeout(Duration::from_secs(1));
                None
            }
        };
        let joined_result = waiter.join().unwrap();
        assert!(wait_started, "checkout reached its wait boundary");
        assert_eq!(
            completed,
            Some(true),
            "initializer drop notified the blocked waiter"
        );
        assert!(joined_result);
    }
}
