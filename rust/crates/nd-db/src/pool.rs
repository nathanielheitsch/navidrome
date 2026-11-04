//! #05 slice 1 — the SQLite connection pool (`db/db.go:62`).
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
//! Every pooled connection is opened through [`nd_dbschema::schema::open`] so the
//! `SEEDEDRAND` scalar function and the `NATSORT` collation are registered
//! per connection (`db/db.go:44-51` ConnectHook; schema-contract.md §4 requires
//! the same per-connection registration here).

use std::sync::{Condvar, Mutex};

use rusqlite::Connection;

use crate::store::StoreError;

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

/// The path the pool opens every connection against.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// DB path or DSN; `:memory:` is rewritten by `nd_dbschema::schema::open`
    /// (`db/db.go:54-59`).
    pub path: String,
    /// Fixed pool size; callers use [`pool_size`] unless a test pins it.
    pub size: usize,
}

impl PoolConfig {
    /// Config with the production pool size (`conf/configuration.go:1217-1219`).
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            size: pool_size(),
        }
    }

    /// Config with an explicit size (tests, harnesses).
    #[must_use]
    pub fn with_size(path: impl Into<String>, size: usize) -> Self {
        Self {
            path: path.into(),
            size: size.max(1),
        }
    }
}

/// The lease handed out by [`Pool::checkout`]: on drop it returns its connection
/// to the idle queue, mirroring Go's `database/sql` implicit release.
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
    pub fn conn_mut(&mut self) -> &mut Connection {
        self.conn
            .as_mut()
            .expect("lease connection is only None while being returned")
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
    idle: Mutex<Vec<Connection>>,
    /// Signalled whenever a connection is returned to [`Pool::idle`].
    available: Condvar,
}

impl Pool {
    /// Opens `config.size` connections through `nd_dbschema::schema::open`
    /// (per-connection extension registration, schema-contract.md §4). Fails on
    /// the first connection that cannot be opened; already-opened connections
    /// are dropped (each owns its own sqlite handle, nothing to unwind).
    ///
    /// # Errors
    /// [`StoreError::Open`] when a connection cannot be opened or its
    /// per-connection extensions fail to register.
    pub fn new(config: &PoolConfig) -> Result<Self, StoreError> {
        let mut idle = Vec::with_capacity(config.size);
        for _ in 0..config.size {
            match nd_dbschema::schema::open(&config.path) {
                Ok(conn) => idle.push(conn),
                Err(e) => return Err(StoreError::Open(e.to_string())),
            }
        }
        Ok(Self {
            config: config.clone(),
            idle: Mutex::new(idle),
            available: Condvar::new(),
        })
    }

    /// Borrows one idle connection, blocking until one is returned. The
    /// [`Lease`] guard releases it on drop.
    ///
    /// # Errors
    /// [`StoreError::Poisoned`] when the idle queue mutex was poisoned by a
    /// panic in a previous lease holder.
    pub fn checkout(&self) -> Result<Lease<'_>, StoreError> {
        let mut idle = self.lock_idle()?;
        while idle.is_empty() {
            match self.available.wait(idle) {
                Ok(guard) => idle = guard,
                Err(_poisoned) => return Err(StoreError::Poisoned),
            };
        }
        let conn = idle
            .pop()
            .expect("idle non-empty is guaranteed by the loop above");
        Ok(Lease {
            pool: self,
            conn: Some(conn),
        })
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
        match self.idle.lock() {
            Ok(guard) => Ok(guard),
            Err(_poisoned) => Err(StoreError::Poisoned),
        }
    }

    fn checkin(&self, conn: Connection) {
        match self.idle.lock() {
            Ok(mut idle) => {
                idle.push(conn);
                drop(idle);
                self.available.notify_one();
            }
            Err(_poisoned) => {
                // The pool is unusable after a panic in a lease holder: dropping
                // the connection here closes it, and the next `checkout` reports
                // the poison instead of silently lending from a broken queue.
                drop(conn);
            }
        }
    }
}
