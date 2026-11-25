// nd-cli: owned by #33 (docs/rust-port/issues.md Part B)

//! BOOT-01 increment D: synchronous DB-open/migrate assembly
//! (`docs/rust-port/subplans/boot-api.md:194`, step 3).
//!
//! [`open_database`] is the callable boundary between the increment-A
//! preflight's resolved SQLite string and the increment-C caller-transaction
//! bootstrap: a Go-style DB input becomes an **open, migrated, fully pooled**
//! [`nd_db::store::SQLStore`]. Nothing else happens here — no config load, no
//! directory creation, no auth, no listener, no async wiring, no scanner, no
//! repair/ANALYZE/backup, and no `chdir`.
//!
//! Order is the pinned one (step 3: "open/migrate once off executor; retain
//! memory DB anchor as required; open SQLStore pool after migration") and is
//! enforced structurally rather than by convention:
//!
//! 1. [`nd_dbschema::schema::parse_open_input`] classifies the input **before**
//!    any handle exists, so an unsupported target never opens half-configured
//!    (D-OPEN option a: refuse rather than guess).
//! 2. [`nd_dbschema::schema::open`] opens the **migration anchor** and
//!    [`nd_dbschema::migrate::ensure_fresh_migration`] applies the single fresh
//!    migration on it. It is the only handle alive at this point.
//! 3. [`nd_db::pool::Pool::new`] opens the pool, and the fresh migration is
//!    re-run on the pool-bound [`nd_db::pool::Initializer`] token — never on a
//!    raw connection — because `apply_fresh_schema` toggles
//!    `PRAGMA foreign_keys` and a mis-configured handle must not enter the idle
//!    queue (DB-03 point 4). Re-running is an apply-once guard, so a database
//!    that already carries the marker runs no DDL statement a second time.
//! 4. The anchor is retained until [`nd_db::pool::Pool::put_back`] validates and
//!    admits the token, then dropped: a memory database is never left without a
//!    live handle while its pool still needs it.
//! 5. An [`nd_db::store::SQLStore`] is returned **only** after `put_back`
//!    succeeded. Every failure path returns `Err` and no store.
//!
//! Memory-DB policy is a **refusal before opening**, never a silent rewrite:
//! several independent handles are unsafe unless they address one database.
//! Supported targets are the exact bare `:memory:` (Go rewrites it to
//! `file::memory:?cache=shared`, `db/db.go:53-56`) and a `file:` URI naming a
//! memory database **and** asking for `cache=shared` as its last `cache=`
//! occurrence (SQLite applies URI `cache=` occurrences in order,
//! `sqlite3.c:182903-182965`; measured here: a named `?mode=memory&cache=shared`
//! URI shows a table created on one handle from another). Everything
//! memory-shaped else is refused: `:memory:?_fk=off` and `:memory:?cache=shared`
//! (their queries are stripped the way go-sqlite3 strips non-URI queries, so
//! SQLite receives plain private `:memory:`), `file::memory:` without
//! `cache=shared`, `file:x?mode=memory`, and `file:x?cache=shared&mode=memory&cache=private`
//! (the last occurrence wins). A plain file DB keeps whatever `cache=` it names —
//! `cache=private` there is legitimate and accepted.
//!
//! Startup-cwd anchoring (step 2, "capture startup cwd for relative paths")
//! applies to the DB input exactly as it does to the music root in
//! [`crate::preflight`]: only a **bare relative path** is joined onto the
//! supplied absolute `startup_cwd`, so a relative input creates its file under
//! the injected cwd and never under the process cwd. The query is retained
//! byte-for-byte (`<cwd>/<head>?<query>`), so SQLite canonicalizes the path
//! component while the URI parameters survive — measured: the default
//! `navidrome.db?cache=shared&_busy_timeout=15000&_journal_mode=WAL&…` shape
//! anchored to an injected cwd yields `journal=wal`, `busy=15000`, `fk=1` and a
//! `file:` argument carrying only `cache=shared`, with no `?` in the file name.
//! A query never prevents anchoring, and the memory path `:memory:` is never
//! anchored: it is SQLite's own special name, not a filename.
//!
//! Everything else reaches the settled parser unchanged:
//!
//! * an absolute plain path or an absolute `file:` URI — including percent-encoded
//!   separators (`file:/tmp/wei%2Frd/e.db`, measured: SQLite decodes it and opens
//!   `<tmp>/wei/rd/e.db`) and any query — is handed over verbatim;
//! * a relative `file:` URI that names a **file** is refused rather than
//!   rewritten (a memory URI needs no path at all, so it is accepted);
//! * percent encoding and URI-query grammars are deliberately **not**
//!   implemented here — `nd-dbschema` owns D-OPEN and owns that text.
//!
//! Errors carry a static stage, a static failure class and the numeric SQLite
//! codes only: no DSN, path, query, upstream message or `#[source]` crosses the
//! boundary. `nd_dbschema::Error::Failed` prints its path, `rusqlite::Error`
//! prints SQLite's message ("unable to open database file" names nothing but is
//! still upstream text), and `StoreError::Open` already stringifies schema
//! errors, so forwarding any of them would leak the input.

use std::path::{Path, PathBuf};

use nd_db::pool::{Pool, PoolConfig};
use nd_db::store::SQLStore;
use nd_dbschema::migrate::already_migrated;
use nd_dbschema::schema::{parse_open_input, MEMORY_PATH};
use rusqlite::Error as RsError;

/// True for a DB input whose path component is a relative path — the only form
/// [`resolve_input`] anchors to `startup_cwd`.
///
/// `:memory:` is excluded: it is SQLite's special database name, not a
/// filename, and anchoring it would open a file literally named
/// `<cwd>/:memory:`. A `file:` URI is excluded even when its path component is
/// relative, because anchoring it means rewriting a URI, which belongs to the
/// settled parser. The head is taken before any `?`, so a query never prevents
/// anchoring.
fn is_relative_head(db_path: &str) -> bool {
    if db_path.is_empty() || db_path.starts_with("file:") || db_path == MEMORY_PATH {
        return false;
    }
    let head = match db_path.find('?') {
        Some(at) => &db_path[..at],
        None => db_path,
    };
    !head.is_empty() && head != MEMORY_PATH && !Path::new(head).is_absolute()
}

/// Raw query text, using go-sqlite3's rule that a query exists only when the
/// first `?` is at index >= 1 (`sqlite3.go:1247`). A leading `?` is part of the
/// filename.
fn raw_query_of(db_path: &str) -> Option<&str> {
    match db_path.find('?') {
        Some(at) if at >= 1 => Some(&db_path[at + 1..]),
        _ => None,
    }
}

/// True when the query's **last** literal `cache=` occurrence is `shared`.
///
/// Only the literal form is matched: the raw query is never percent-decoded or
/// rebuilt, because decoding here would duplicate D-OPEN's grammar and a decoded
/// `cache=shared` is not what SQLite sees. SQLite applies all URI `cache=`
/// occurrences in order (`sqlite3.c:182903-182965`), so the last one decides; a
/// value may carry a NUL, after which SQLite truncates it
/// (`sqlite3.c:182849-182863`), so the comparison uses the part before the first
/// NUL.
fn query_ends_with_shared_cache(raw_query: &str) -> bool {
    raw_query
        .split('&')
        .rev()
        .filter_map(|item| item.split_once('='))
        // `find` after `rev()` is the LAST `cache=` occurrence, which is what
        // SQLite's ordered option loop leaves in effect.
        .find(|(name, _)| *name == "cache")
        .is_some_and(|(_, value)| value.split('\0').next() == Some("shared"))
}

/// True when a memory-shaped input addresses one shared database.
///
/// The bare `:memory:` is shared because the parser rewrites it to
/// `file::memory:?cache=shared`; a `file:` memory URI is shared only if its own
/// URI says so, since that URI reaches SQLite untouched.
fn memory_input_is_shared(db_path: &str) -> bool {
    db_path == MEMORY_PATH
        || (db_path.starts_with("file:")
            && raw_query_of(db_path).is_some_and(query_ends_with_shared_cache))
}

/// The stage of the assembly that failed. Static: the variant name is the whole
/// message, so no input text travels with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbStage {
    /// The DB input was rejected before any handle was opened.
    Input,
    /// The migration connection could not be opened or configured.
    MigrationOpen,
    /// The fresh migration could not be applied.
    Migration,
    /// Pool handles could not be opened or did not hold the requested settings.
    Pool,
    /// The migration token was refused admission to the pool.
    Admission,
}

impl std::fmt::Display for DbStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            DbStage::Input => "input",
            DbStage::MigrationOpen => "migration-open",
            DbStage::Migration => "migration",
            DbStage::Pool => "pool",
            DbStage::Admission => "admission",
        };
        f.write_str(name)
    }
}

/// Why the assembly failed, as static text only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbFailureClass {
    /// The settled parser refused the input (unknown parameter, malformed query,
    /// truncated URI, …). Its reason stays inside `nd-dbschema`.
    UnsupportedInput,
    /// A memory database whose handles would be independent: no shared cache.
    PrivateMemory,
    /// A relative `file:` URI naming a file: only a bare relative path is
    /// anchored, and a URI must not be rewritten here.
    RelativeUri,
    /// A `file:` URI whose authority component SQLite rejects
    /// (`file://tmp/x.db`, `sqlite3.c:182815-182824`).
    InvalidAuthority,
    /// The anchored path is not valid UTF-8, so it cannot be handed to the parser
    /// without lossy rewriting.
    NotUtf8,
    /// A relative input would need a `startup_cwd` that is not absolute, so the
    /// resulting path would depend on the process cwd.
    StartupCwdNotAbsolute,
    /// A relative input would need a `startup_cwd` that does not exist. This
    /// stage creates nothing, so it fails instead of inventing a directory.
    StartupCwdMissing,
    /// SQLite refused the open of the migration connection.
    OpenFailed,
    /// The fresh migration failed on a live handle.
    MigrationFailed,
    /// Pool handles could not be opened.
    PoolFailed,
    /// A handle's effective state does not match what the input asked for.
    ConfigurationFailed,
    /// The migration token was refused admission to the pool.
    AdmissionFailed,
}

impl std::fmt::Display for DbFailureClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            DbFailureClass::UnsupportedInput => "input rejected by the settled DSN parser",
            DbFailureClass::PrivateMemory => {
                "memory database without a shared cache: pool handles would be independent databases"
            }
            DbFailureClass::RelativeUri => {
                "relative file: URI naming a file: only a bare relative path is anchored"
            }
            DbFailureClass::InvalidAuthority => "file: URI with an invalid authority",
            DbFailureClass::NotUtf8 => "anchored path is not valid UTF-8",
            DbFailureClass::StartupCwdNotAbsolute => "startup cwd is not absolute",
            DbFailureClass::StartupCwdMissing => "startup cwd does not exist",
            DbFailureClass::OpenFailed => "SQLite refused the database open",
            DbFailureClass::MigrationFailed => "the fresh migration failed",
            DbFailureClass::PoolFailed => "pool handles could not be opened",
            DbFailureClass::ConfigurationFailed => "handle does not hold the requested settings",
            DbFailureClass::AdmissionFailed => "migration handle was refused pool admission",
        };
        f.write_str(name)
    }
}

/// Numeric SQLite result codes of a failure.
///
/// This is the only failure detail that crosses the boundary: codes are stable,
/// enumerable and carry no path, query or credential. SQLite's own message is
/// upstream text and is deliberately dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqliteCode {
    /// The primary result code (`libsqlite3-sys` derives its `ErrorCode` from
    /// the low byte of the extended code, so this is that same low byte).
    pub primary: i32,
    /// The extended result code, which carries the subtype (e.g. 262 =
    /// `SQLITE_LOCKED_SHAREDCACHE`, 787 = `SQLITE_CONSTRAINT_FOREIGNKEY`).
    pub extended: i32,
}

/// A database-assembly failure: stage, static class and numeric codes.
///
/// No `#[source]`: nesting the underlying error would re-expose its `Display`,
/// which embeds the DSN (`nd_dbschema::Error::Failed` prints the path).
#[derive(Debug, thiserror::Error)]
pub struct DatabaseError {
    /// Which step failed.
    pub stage: DbStage,
    /// Static reason class.
    pub class: DbFailureClass,
    /// Numeric SQLite codes, when the failure came from SQLite.
    pub sqlite: Option<SqliteCode>,
}

impl std::fmt::Display for DatabaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let stage = self.stage;
        let class = self.class;
        match self.sqlite {
            Some(c) => write!(
                f,
                "database setup failed at {stage}: {class} (sqlite code {}, extended {})",
                c.primary, c.extended
            ),
            None => write!(f, "database setup failed at {stage}: {class}"),
        }
    }
}

impl DatabaseError {
    /// A refusal with no SQLite involvement.
    fn refusal(stage: DbStage, class: DbFailureClass) -> Self {
        DatabaseError {
            stage,
            class,
            sqlite: None,
        }
    }

    /// A failure whose only extra detail is the numeric SQLite code.
    fn sqlite(stage: DbStage, class: DbFailureClass, err: &RsError) -> Self {
        DatabaseError {
            stage,
            class,
            sqlite: code_of(err),
        }
    }
}

/// Numeric codes of a rusqlite failure; `None` for a non-SQLite error.
fn code_of(err: &RsError) -> Option<SqliteCode> {
    match err {
        RsError::SqliteFailure(code, _) => Some(SqliteCode {
            primary: code.extended_code & 0xff,
            extended: code.extended_code,
        }),
        _ => None,
    }
}

/// Map a schema-layer error to a stage and numeric codes, without forwarding its
/// text (which names the path).
fn schema_error(err: &nd_dbschema::schema::Error, open_stage: DbStage) -> DatabaseError {
    match err {
        nd_dbschema::schema::Error::Failed { cause, .. } => {
            DatabaseError::sqlite(open_stage, DbFailureClass::OpenFailed, cause)
        }
        nd_dbschema::schema::Error::InvalidDsn { .. } => {
            DatabaseError::refusal(DbStage::Input, DbFailureClass::UnsupportedInput)
        }
        nd_dbschema::schema::Error::OpenConfiguration { .. } => {
            DatabaseError::refusal(open_stage, DbFailureClass::ConfigurationFailed)
        }
        // `Filesystem` belongs to backup/restore, which this stage never calls.
        nd_dbschema::schema::Error::Filesystem { .. } => {
            DatabaseError::refusal(DbStage::Migration, DbFailureClass::MigrationFailed)
        }
    }
}

/// What the input resolved to, reported for startup logging and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseTarget {
    /// True for a memory database.
    pub is_memory: bool,
    /// True when this call applied the fresh DDL; false when the marker was
    /// already present, i.e. the database was reopened.
    pub migrated_on_open: bool,
    /// Migration ids recorded after this call. The port has exactly one fresh
    /// migration, so this is 1 both fresh and reopened — and a reopen that added
    /// a second row would show up here.
    pub migration_count: usize,
    /// Handles the pool owns after admission.
    pub pool_size: usize,
    /// True when the input's path component was anchored to `startup_cwd`.
    pub anchored_to_startup_cwd: bool,
}

impl std::fmt::Display for DatabaseTarget {
    /// Deliberately does not print the input: this is logged at startup and the
    /// DSN can carry a query.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "database target (memory: {}, migrated: {}, migrations: {}, pool size: {}, anchored: {})",
            self.is_memory,
            self.migrated_on_open,
            self.migration_count,
            self.pool_size,
            self.anchored_to_startup_cwd,
        )
    }
}

/// Inputs to [`open_database`].
#[derive(Debug, Clone)]
pub struct DatabaseOptions<'a> {
    /// The DB input from the preflight (`Config::db_path`), verbatim. A bare
    /// relative path is anchored to `startup_cwd`; anything else reaches the
    /// settled parser unchanged.
    pub db_path: &'a str,
    /// Absolute startup working directory — the same anchor the preflight used
    /// for the music root. Required for a relative input, never read from the
    /// process cwd, never created.
    pub startup_cwd: &'a Path,
    /// Pool size: `nd_db::pool::pool_size()` for production
    /// (`max(4, NumCPU)`, `conf/configuration.go:1217-1219`), pinned to 1 in
    /// tests. `0` is clamped to 1 the way [`PoolConfig`] clamps it.
    pub pool_size: usize,
}

/// Result of [`open_database`]: an initialized store plus resolution facts.
pub struct OpenedDatabase {
    /// Open, migrated, fully pooled store. Handles reach callers only through
    /// its own checkout/transaction verbs.
    pub store: SQLStore,
    /// Resolution facts, safe to log.
    pub target: DatabaseTarget,
}

impl std::fmt::Debug for OpenedDatabase {
    /// `SQLStore`'s derived `Debug` prints its pool config, i.e. the DSN with its
    /// query, so the field is withheld and only the non-secret facts print.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenedDatabase")
            .field("target", &self.target)
            .field("store", &"<pooled store>")
            .finish()
    }
}

/// Classify a DB input and settle the anchoring question, **before** opening.
///
/// Public because later bootstrap and tests need to know what will be opened
/// without opening it; it performs no SQLite work and creates nothing.
///
/// # Errors
/// [`DbStage::Input`] for an unsupported DSN, private memory, a relative file
/// URI, a malformed authority or a non-UTF-8 anchored path;
/// [`DbStage::MigrationOpen`] when a relative input needs a `startup_cwd` that is
/// not absolute or does not exist.
pub fn resolve_input(db_path: &str, startup_cwd: &Path) -> Result<(String, bool), DatabaseError> {
    // Anchoring needs a usable cwd, and this stage owns no directory creation.
    let relative = is_relative_head(db_path);
    if relative {
        if !startup_cwd.is_absolute() {
            return Err(DatabaseError::refusal(
                DbStage::MigrationOpen,
                DbFailureClass::StartupCwdNotAbsolute,
            ));
        }
        if !startup_cwd.exists() {
            return Err(DatabaseError::refusal(
                DbStage::MigrationOpen,
                DbFailureClass::StartupCwdMissing,
            ));
        }
    }

    // Classify as written. For a relative path the path component alone decides
    // memory-ness and authority, so this is independent of the anchoring result.
    let target = parse_open_input(db_path)
        .map_err(|_| DatabaseError::refusal(DbStage::Input, DbFailureClass::UnsupportedInput))?;

    if db_path.starts_with("file:") {
        // SQLite rejects an authority-bearing URI and no anchoring can repair it.
        if target.path.starts_with("//") {
            return Err(DatabaseError::refusal(
                DbStage::Input,
                DbFailureClass::InvalidAuthority,
            ));
        }
        // A relative URI names a file whose location is only settled by SQLite at
        // open time; rewriting the URI to anchor it is not this stage's job. A
        // memory URI has no filesystem location to anchor, so it is accepted.
        if !target.is_memory && !Path::new(target.path.as_str()).is_absolute() {
            return Err(DatabaseError::refusal(
                DbStage::Input,
                DbFailureClass::RelativeUri,
            ));
        }
    }

    // Independent handles on a memory DB are separate databases: refuse.
    if target.is_memory && !memory_input_is_shared(db_path) {
        return Err(DatabaseError::refusal(
            DbStage::Input,
            DbFailureClass::PrivateMemory,
        ));
    }

    if !relative {
        return Ok((db_path.to_string(), false));
    }

    // `<cwd>/<head>?<query>`: the head is joined, the query appended verbatim.
    let (head, query) = match db_path.find('?') {
        Some(at) => (&db_path[..at], Some(&db_path[at + 1..])),
        None => (db_path, None),
    };
    let joined: PathBuf = startup_cwd.join(head);
    let text = joined
        .to_str()
        .ok_or_else(|| DatabaseError::refusal(DbStage::Input, DbFailureClass::NotUtf8))?;
    let resolved = match query {
        Some(query) => format!("{text}?{query}"),
        None => text.to_string(),
    };

    // Re-classify the string that will actually be opened, so the guarantees
    // above hold for the real argument and the memory decision cannot disagree.
    parse_open_input(resolved.as_str())
        .map_err(|_| DatabaseError::refusal(DbStage::Input, DbFailureClass::UnsupportedInput))?;
    Ok((resolved, true))
}

/// Open and migrate `db_path`, then assemble the pooled store.
///
/// Runs entirely on the calling thread; an async caller must wrap it in
/// `spawn_blocking`, exactly as [`SQLStore::open`] does. Nothing is created: a
/// relative input requires an existing `startup_cwd`, and a missing parent for an
/// absolute path is reported as an open failure rather than repaired.
///
/// # Errors
/// See [`DatabaseError`]. No store is returned on any failure path.
pub fn open_database(options: DatabaseOptions<'_>) -> Result<OpenedDatabase, DatabaseError> {
    let (resolved, anchored) = resolve_input(options.db_path, options.startup_cwd)?;

    // Step 3a: the migration anchor — the only handle that exists yet.
    let anchor = nd_dbschema::schema::open(resolved.as_str()).map_err(|err| match &err {
        nd_dbschema::schema::Error::Failed { cause, .. } => {
            DatabaseError::sqlite(DbStage::MigrationOpen, DbFailureClass::OpenFailed, cause)
        }
        _ => schema_error(&err, DbStage::MigrationOpen),
    })?;

    // Freshness is read BEFORE the DDL runs: `ensure_fresh_migration` is an
    // apply-once guard, so the returned ids alone cannot say whether this call
    // did the work.
    let fresh = !already_migrated(&anchor);
    nd_dbschema::migrate::ensure_fresh_migration(&anchor).map_err(|err| migration_error(&err))?;

    // Step 3b: open the pool. Every handle goes through
    // `schema::open_with_options` (extensions + pragmas + effective read-back),
    // and the first is handed out as the pool-bound token.
    let config = PoolConfig::with_size(resolved.clone(), options.pool_size);
    let (pool, token) = Pool::new(&config)
        .map_err(|_| DatabaseError::refusal(DbStage::Pool, DbFailureClass::PoolFailed))?;

    // Step 3c: migrate on the pool-bound token. On an already-migrated database
    // this re-probes the marker and runs no DDL statement, which is also what
    // proves a reopen does not migrate twice.
    let versions = nd_dbschema::migrate::ensure_fresh_migration(&token)
        .map_err(|err| migration_error(&err))?;

    // Step 3d: validate and admit the token. Until this returns Ok the pool holds
    // one handle out; after it, all handles are idle and the pool alone keeps a
    // memory database alive — so the anchor is dropped only now.
    pool.put_back(token)
        .map_err(|_| DatabaseError::refusal(DbStage::Admission, DbFailureClass::AdmissionFailed))?;
    drop(anchor);

    Ok(OpenedDatabase {
        store: SQLStore::new(pool),
        target: DatabaseTarget {
            is_memory: parse_open_input(resolved.as_str()).is_ok_and(|t| t.is_memory),
            migrated_on_open: fresh,
            migration_count: versions.len(),
            pool_size: options.pool_size.max(1),
            anchored_to_startup_cwd: anchored,
        },
    })
}

/// Migration failures keep their numeric codes but never their text.
fn migration_error(err: &nd_dbschema::schema::Error) -> DatabaseError {
    match err {
        nd_dbschema::schema::Error::Failed { cause, .. } => {
            DatabaseError::sqlite(DbStage::Migration, DbFailureClass::MigrationFailed, cause)
        }
        _ => schema_error(err, DbStage::Migration),
    }
}
