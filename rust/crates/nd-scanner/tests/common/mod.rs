//! Shared fixture harness for the SC-05 phase-1 vertical tests.
//!
//! One shape for both test files: a real on-disk tree under `$TMPDIR` built by
//! **copying the already-committed nd-tagreader fixtures**
//! (`nd-tagreader/tests/fixtures/real/known.{mp3,flac,m4a,ogg}`) into per-test layouts,
//! plus a real pooled SQLite DB with the fresh schema and a seeded `library` row. No
//! new binary fixtures are committed and no absolute path is baked in — the fixture
//! source resolves from `CARGO_MANIFEST_DIR`.
//!
//! Both test binaries share this module, and each uses only part of it, so unused
//! helpers in the other binary are expected: `dead_code` is allowed for the module as a
//! whole rather than per item. No other lint is silenced here.
#![allow(dead_code)]
//!
//! Each DB lives at its own `$TMPDIR` path (mirroring `nd-db`'s
//! `tests/runtime_folder_writes.rs:124-135`), so a `SQLStore` stays `Send`: a shared
//! `tempfile`-style guard would make the pool non-`Send` and break scoped threads.

use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use std::collections::BTreeMap;

use nd_db::folder_repo::{FolderRepo, FolderUpdateInfo};
use nd_db::library_repo::{self, LibraryRecord, LibraryWriteError};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::scanner_queries::{self, Control, VisitOutcome};
use nd_db::store::SQLStore;
use nd_mapping::config::{MappingContext, PidOptions};
use nd_scanner::phase1::{
    self, FolderJob, LibraryScan, LibraryScanOutcome, Phase1FolderOutcome, ScanScope,
};
use nd_scanner::storage::LogicalPath;

/// The committed fixture files, resolved relative to this crate's manifest so the
/// tests work from any working directory. The returned path is canonicalized, so an
/// identity assertion against [`manifest_root`] compares real absolute paths (see there).
pub fn fixture_source(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../nd-tagreader/tests/fixtures/real")
        .join(name);
    std::fs::canonicalize(&path).unwrap_or(path)
}

/// Canonicalized absolute paths, for identity assertions. `CARGO_MANIFEST_DIR` joined
/// with `../nd-tagreader/...` still carries a `..` component — and a manifest reached
/// through a symlink keeps that difference — so a raw `starts_with` against the raw
/// manifest text is satisfied by a path that is not actually inside the manifest.
/// [`fixture_source`] canonicalizes, and these helpers give the canonicalized sides of the
/// comparison. A missing file falls back to the raw join, so a failing assertion still
/// reports the path it actually tried.
#[must_use]
pub fn manifest_root() -> PathBuf {
    std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")))
        .unwrap_or_else(|e| panic!("cannot canonicalize the manifest dir: {e}"))
}

/// The canonicalized directory the committed fixtures live in (the sibling
/// `nd-tagreader/tests/fixtures/real`), i.e. the manifest's parent joined with the
/// relative fragment. Comparing against this — rather than the nd-scanner manifest, which
/// canonicalized fixtures are NOT inside — is what makes the "resolves from the committed
/// tree" claim checkable.
#[must_use]
pub fn fixture_real_dir() -> PathBuf {
    let dir = manifest_root()
        .parent()
        .expect("the manifest dir sits inside the workspace crates directory")
        .join("nd-tagreader/tests/fixtures/real");
    std::fs::canonicalize(&dir).unwrap_or(dir)
}

/// Copy `name` into `dir` under `stored` (the on-disk file name the DB will record).
pub fn copy_fixture(dir: &Path, name: &str, stored: &str) -> PathBuf {
    let source = fixture_source(name);
    let target = dir.join(stored);
    std::fs::copy(&source, &target)
        .unwrap_or_else(|e| panic!("cannot copy {} to {target:?}: {e}", source.display()));
    target
}

/// The four committed fixtures' format names.
pub const ALL_FORMATS: [&str; 4] = ["known.mp3", "known.flac", "known.m4a", "known.ogg"];

/// A per-test scratch tree. Returned root is created empty; the caller fills it.
pub struct Tree {
    pub root: PathBuf,
}

impl Tree {
    /// Create `$TMPDIR/nd-scanner-phase1-<label>-<rand>`, canonicalized.
    ///
    /// Canonicalization matters here: a scratch tree under `$TMPDIR` can be reached through
    /// a symlinked temp dir (macOS `/var` → `/private/var`), and the ingest, the SC-02 walk
    /// and the DB all take this ONE path, so they must agree on its identity.
    #[must_use]
    pub fn new(label: &str) -> Tree {
        let root = PathBuf::from(nd_utils::files::temp_file_name(
            &format!("nd-scanner-phase1-{label}-"),
            ".tree",
        ));
        std::fs::create_dir_all(&root)
            .unwrap_or_else(|e| panic!("cannot create tree root {root:?}: {e}"));
        let root = std::fs::canonicalize(&root)
            .unwrap_or_else(|e| panic!("cannot canonicalize tree root {root:?}: {e}"));
        Tree { root }
    }

    /// Create a subdirectory and return its path.
    #[must_use]
    pub fn dir(&self, relative: &str) -> PathBuf {
        let path = self.root.join(relative);
        std::fs::create_dir_all(&path).unwrap_or_else(|e| panic!("cannot create {path:?}: {e}"));
        path
    }

    /// Copy one fixture into `relative` under its own base name, with its mtime pinned
    /// to [`file_time`].
    pub fn put(&self, relative: &str, fixture: &str) -> PathBuf {
        copy_pinned(&self.dir(relative), fixture, fixture)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// An `OsString` from raw bytes — the only way to ask for a name the platform may or may not
/// accept. Used by the non-UTF-8 name coverage in `tests/phase1_slice.rs`.
#[cfg(unix)]
#[must_use]
pub fn os_name(bytes: &[u8]) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    std::ffi::OsString::from_vec(bytes.to_vec())
}

/// Go's DSN shape translated by `nd_dbschema::schema::open` (`db/db.go:54-55`).
#[must_use]
pub fn dsn(path: &Path) -> String {
    format!("file:{}?mode=rwc&_foreign_keys=on", path.display())
}

/// A fresh, migrated, pooled store at its own `$TMPDIR` path.
#[must_use]
pub fn fresh_store(label: &str, size: usize) -> (SQLStore, PathBuf) {
    let path = PathBuf::from(nd_utils::files::temp_file_name(
        &format!("nd-scanner-phase1-{label}-"),
        ".db",
    ));
    if path.exists() {
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("cannot clear {path:?}: {e}"));
    }
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn(&path), size))
        .unwrap_or_else(|e| panic!("cannot open pool at {path:?}: {e}"));
    nd_dbschema::schema::apply_fresh_schema(&first)
        .unwrap_or_else(|e| panic!("cannot apply schema at {path:?}: {e}"));
    pool.put_back(first).expect("return configured handle");
    (SQLStore::new(pool), path)
}
/// A pool whose EVERY handle has `busy_timeout = 0`, so a contended write reports
/// `SQLITE_BUSY` immediately instead of waiting out the 5s driver timeout — the same
/// seam `nd-db`'s own retry test uses (`nd-db/tests/runtime_folder_persistence.rs:115-134`).
/// The retry budget itself is untouched: `persist_folder_delta` still uses the DB's
/// `try_with_tx_retry_sync` with the production 5s/10s/15s backoff, so a test that
/// measures elapsed time really observes one backoff gap.
pub fn busy_store(label: &str, size: usize) -> (SQLStore, PathBuf) {
    let path = PathBuf::from(nd_utils::files::temp_file_name(
        &format!("nd-scanner-phase1-{label}-"),
        ".db",
    ));
    if path.exists() {
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("cannot clear {path:?}: {e}"));
    }
    let config = PoolConfig {
        path: dsn(&path),
        size,
        options: Some(nd_dbschema::schema::DbOpenOptions {
            journal_mode: Some(nd_dbschema::schema::JournalMode::Delete),
            busy_timeout: 0,
            synchronous: nd_dbschema::schema::Synchronous::Normal,
            foreign_keys: Some(true),
            cache_size: None,
        }),
    };
    let (pool, first) =
        Pool::new(&config).unwrap_or_else(|e| panic!("cannot open pool at {path:?}: {e}"));
    nd_dbschema::schema::apply_fresh_schema(&first)
        .unwrap_or_else(|e| panic!("cannot apply schema at {path:?}: {e}"));
    pool.put_back(first).expect("return configured handle");
    (SQLStore::new(pool), path)
}

/// Drop a test's DB file after the store is gone (the pool holds the handles open).
pub fn remove_db(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Open a SECOND pool over the same file, with no schema pass: the reopen leg of
/// `reopens_with_fresh_pool_reads_back_through_executed_repos`. Call it only after the
/// original store is dropped, so the only writer on the file is the new pool.
///
/// Same DSN shape as [`fresh_store`] (`mode=rwc&_foreign_keys=on`), so the reopened
/// handles carry the same per-connection pragmas. The held-out initializer token is
/// returned to the pool, which is what makes a size-2 reopened store able to hold a
/// children read and a transaction at once.
#[must_use]
pub fn reopen_store(path: &Path, size: usize) -> SQLStore {
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn(path), size))
        .unwrap_or_else(|e| panic!("cannot reopen pool at {path:?}: {e}"));
    pool.put_back(first).expect("return configured handle");
    SQLStore::new(pool)
}

/// A fixed scan stamp: no test depends on the wall clock.
#[must_use]
pub fn stamp() -> DateTime<Utc> {
    TimeZone::timestamp_opt(&Utc, 1_751_000_000, 0)
        .single()
        .expect("scan stamp is in chrono's range")
}

/// A fixed file mtime, an hour before the scan stamp: every copied fixture gets it, so
/// `updated_at`/`album.updated_at` are deterministic and distinguishable from the scan
/// stamp (Go's `updated_at` is the file's own mtime, never a scan timestamp).
#[must_use]
pub fn file_stamp() -> DateTime<Utc> {
    TimeZone::timestamp_opt(&Utc, 1_750_996_400, 0)
        .single()
        .expect("file stamp is in chrono's range")
}

/// `SystemTime` for [`file_stamp`].
#[must_use]
pub fn file_time() -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_750_996_400)
}

/// Pin a copied fixture's mtime so the storage-derived columns are deterministic.
pub fn set_file_time(path: &std::path::Path, at: std::time::SystemTime) {
    let file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap_or_else(|e| panic!("cannot open {path:?} to set mtime: {e}"));
    file.set_modified(at)
        .unwrap_or_else(|e| panic!("cannot set mtime on {path:?}: {e}"));
}

/// Pin a **directory**'s mtime. [`set_file_time`] cannot do this: its `append` open is
/// rejected with `EISDIR`. A plain read open IS accepted for a directory, and
/// `set_modified` on that handle sets the directory's own timestamp (verified on
/// macOS/APFS and Linux: a later `metadata().modified()` reports the instant set).
pub fn set_dir_time(path: &std::path::Path, at: std::time::SystemTime) {
    let file = std::fs::File::open(path)
        .unwrap_or_else(|e| panic!("cannot open directory {path:?} to set mtime: {e}"));
    file.set_modified(at)
        .unwrap_or_else(|e| panic!("cannot set mtime on directory {path:?}: {e}"));
}

/// A file/directory mtime strictly later than [`file_time`] (an hour after it). A test that
/// wants to prove an entry's mtime RAISED `folder.modTime` needs an instant above the
/// pinned fixture stamp, not one that merely ties with it.
#[must_use]
pub fn later_file_time() -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_750_996_400 + 3_600)
}

/// Copy a fixture and pin its mtime to [`file_time`]. Use this everywhere the test
/// asserts a storage-derived timestamp.
pub fn copy_pinned(dir: &Path, name: &str, stored: &str) -> PathBuf {
    let path = copy_fixture(dir, name, stored);
    set_file_time(&path, file_time());
    path
}

/// A stamp strictly later than [`stamp`], for the second pass of an idempotence run. The
/// second pass of a two-scan test must genuinely differ from the first: reusing `stamp()`
/// makes any "this scan's stamp" assertion vacuous, because it already equals the value
/// the FIRST scan wrote.
#[must_use]
pub fn later_stamp() -> DateTime<Utc> {
    TimeZone::timestamp_opt(&Utc, 1_751_000_600, 0)
        .single()
        .expect("second scan stamp is in chrono's range")
}

/// A third, strictly later stamp (`stamp` < [`later_stamp`] < [`laterest_stamp`]), for a
/// three-pass scan where the LAST pass must be distinguishable from both earlier ones.
#[must_use]
pub fn laterest_stamp() -> DateTime<Utc> {
    TimeZone::timestamp_opt(&Utc, 1_751_001_200, 0)
        .single()
        .expect("third scan stamp is in chrono's range")
}

/// Seed the `library` row the ingest is scoped to, through the production write seam
/// (`library_repo::put_in_tx`). `path` is the tree root, matching Go's `library.path`
/// being the configured root.
pub fn seed_library(store: &SQLStore, id: i64, name: &str, path: &Path) {
    let record = LibraryRecord {
        id,
        name: name.to_string(),
        path: path.display().to_string(),
        remote_path: String::new(),
        last_scan_at: stamp(),
        last_scan_started_at: stamp(),
        full_scan_in_progress: true,
        updated_at: stamp(),
        created_at: stamp(),
        total_songs: 0,
        total_albums: 0,
        total_artists: 0,
        total_folders: 0,
        total_files: 0,
        total_missing_files: 0,
        total_size: 0,
        total_duration: 0.0,
        default_new_users: false,
        pid_album: String::new(),
        pid_track: String::new(),
        scanned_pid_album: String::new(),
        scanned_pid_track: String::new(),
    };
    store
        .try_with_tx_sync::<library_repo::LibraryWriteResult, LibraryWriteError>(|tx| {
            library_repo::put_in_tx(tx, &record, &stamp(), &[])
        })
        .unwrap_or_else(|e| panic!("seed_library({id}) failed: {e}"));
}

/// The mapping context for a scan run: the shipped YAML table plus Go's default PID
/// rules (empty overrides resolve through `PidOptions::effective`).
#[must_use]
pub fn mapping() -> MappingContext {
    MappingContext::new(nd_mapping::config::MappingOptions::default())
}

/// The album PID override used by the transition tests: a canonical tag-name spec whose
/// derived id genuinely differs from Go's default (`albumid` is illegal in an album
/// spec, so the spec is a plain tag list).
pub const OVERRIDE_ALBUM_PID: &str = "album,albumartist";

/// A job with an album PID override, the Go `PIDAlbum` config-change case.
#[must_use]
pub fn job_with_album_pid<'a>(
    library_id: i64,
    root: &'a Path,
    relative: &str,
    scan_stamp: DateTime<Utc>,
    mapping: &'a MappingContext,
    album_spec: &str,
    prev_album_pid: Option<&'a str>,
) -> FolderJob<'a> {
    let mut job = job(library_id, root, relative, scan_stamp, mapping);
    job.pid = PidOptions {
        album: album_spec.to_string(),
        track: String::new(),
    };
    job.prev_album_pid = prev_album_pid;
    job
}

/// A `FolderJob` for `relative` under `root`, full-scan mode with Go's default
/// booleans (`FollowSymlinks`/`IgnoreDotFolders`/`AppendSubtitle` all `true`).
#[must_use]
pub fn job<'a>(
    library_id: i64,
    root: &'a Path,
    relative: &str,
    scan_stamp: DateTime<Utc>,
    mapping: &'a MappingContext,
) -> FolderJob<'a> {
    FolderJob {
        library_id,
        library_root: root.as_os_str(),
        folder_path: LogicalPath::parse(relative)
            .unwrap_or_else(|e| panic!("path {relative}: {e}")),
        scan_stamp,
        full_scan_mode: true,
        append_subtitle: phase1::DEFAULT_APPEND_SUBTITLE,
        follow_symlinks: true,
        ignore_dot_folders: true,
        mapping,
        pid: PidOptions::default(),
        // Go sets this only when `lib.ScannedPIDAlbum != pid.Album` (`scanner.go:85-87`);
        // the vertical's tests scan with the default spec, so the re-derivation arm is off.
        prev_album_pid: None,
        // macOS/APFS reports a creation time, so the default job keeps the storage fact.
        no_birth_time: false,
    }
}

/// A job that declares the device as birth-time-less, the port of `localFS.noBirthTime`
/// (`core/storage/local/local.go:129-145`). This is how a test exercises the substitute
/// arm deterministically on a platform that *does* report birth times, instead of
/// depending on the host filesystem.
#[must_use]
pub fn job_without_birth<'a>(
    library_id: i64,
    root: &'a Path,
    relative: &str,
    scan_stamp: DateTime<Utc>,
    mapping: &'a MappingContext,
) -> FolderJob<'a> {
    let mut job = job(library_id, root, relative, scan_stamp, mapping);
    job.no_birth_time = true;
    job
}

/// Run one fixture statement (test-only seeding), bound values only.
pub fn execute(store: &SQLStore, sql: &str, values: &[nd_db::sqlrepo::Value]) {
    store
        .with_connection_sync(|conn| {
            nd_db::execution::execute(conn, &(sql.to_string(), values.to_vec()))
        })
        .unwrap_or_else(|e| panic!("execute {sql} failed: {e}"));
}

/// Read a scalar out of the DB. `None` for no row or a NULL value.
pub fn scalar(store: &SQLStore, sql: &str) -> Option<String> {
    store
        .with_connection_sync(|conn| {
            nd_db::execution::query_optional(conn, &(sql.to_string(), Vec::new()), |row| {
                row.get::<_, Option<String>>(0)
            })
            .map(|cell| cell.flatten())
        })
        .unwrap_or_else(|e| panic!("query {sql} failed: {e}"))
}

/// Count rows from the DB.
pub fn count(store: &SQLStore, sql: &str) -> i64 {
    store
        .with_connection_sync(|conn| {
            nd_db::execution::query_optional(conn, &(sql.to_string(), Vec::new()), |row| {
                row.get::<_, i64>(0)
            })
        })
        .unwrap_or_else(|e| panic!("count {sql} failed: {e}"))
        .unwrap_or_else(|| panic!("count {sql} returned no row"))
}

/// Every row of one column, ordered by the caller's `ORDER BY`.
pub fn strings(store: &SQLStore, sql: &str) -> Vec<String> {
    store
        .with_connection_sync(|conn| {
            nd_db::execution::query_all(conn, &(sql.to_string(), Vec::new()), |row| {
                row.get::<_, String>(0)
            })
        })
        .unwrap_or_else(|e| panic!("read {sql} failed: {e}"))
}

/// `true` when the pool is still usable, checked by USING it rather than by asking
/// the queue's mutex: this leases a connection, runs a statement through it, and
/// returns the lease. A poisoned idle queue fails the first half; a connection that
/// SQLite has left unusable (an abandoned transaction, a poisoned handle) fails the
/// second. `idle_count().is_ok()` alone only proves the mutex was not poisoned.
///
/// The statement is `SELECT 1` — read-only, so calling this from an assertion does not
/// change the state a later assertion inspects.
#[must_use]
pub fn pool_healthy(store: &SQLStore) -> bool {
    if store.idle_count().is_err() {
        return false;
    }
    store
        .with_connection_sync(|conn| conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0)))
        .is_ok_and(|value| value == 1)
}

/// Read a folder's media children back through the SAME frozen DB-11 visitor the
/// ingest uses — the read-back leg of every vertical assertion.
pub fn children(store: &SQLStore, folder_id: &str) -> Vec<nd_db::mediafile_repo::MediaFileRecord> {
    let cancel = nd_utils::context::Cancellation::new();
    let mut collected = Vec::new();
    let (outcome, cursor_error) = store
        .with_connection_sync(|conn| {
            let result =
                scanner_queries::visit_media_in_folder(conn, folder_id, None, &cancel, |record| {
                    collected.push(record);
                    Control::Continue
                });
            Ok(match result {
                Ok(visit) => (visit, None),
                Err(source) => (VisitOutcome::Completed { visited: 0 }, Some(source)),
            })
        })
        .unwrap_or_else(|e| panic!("children read failed: {e}"));
    if let Some(source) = cursor_error {
        panic!("children read failed: {source}");
    }
    match outcome {
        VisitOutcome::Completed { .. } => collected,
        VisitOutcome::Stopped { visited } => panic!("children read stopped after {visited} rows"),
    }
}

/// Read one scalar as TEXT through a lease of its own, for build-level evidence
/// (`sqlite_version`, compile options, `pragma_module_list`). Statements must spell
/// `CAST(... AS TEXT)`: a pragma row holding an INTEGER does not scan as `String`.
#[must_use]
pub fn pragma(store: &SQLStore, sql: &str) -> String {
    store
        .with_connection_sync(|conn| {
            nd_db::execution::query_optional(conn, &(sql.to_string(), Vec::new()), |row| {
                row.get::<_, String>(0)
            })
        })
        .unwrap_or_else(|e| panic!("pragma {sql} failed: {e}"))
        .unwrap_or_else(|| panic!("pragma {sql} returned no row"))
}

/// Read one scalar as TEXT **inside an already-held lease**, so a test can report the
/// pragmas of the specific connection it is using (the every-handle policy evidence).
pub fn pragma_in_lease<T>(
    store: &SQLStore,
    run: impl FnOnce(&mut dyn FnMut(&str) -> String) -> T,
) -> T {
    store
        .with_connection_sync(|conn| {
            let mut read = |sql: &str| {
                nd_db::execution::query_optional(conn, &(sql.to_string(), Vec::new()), |row| {
                    row.get::<_, String>(0)
                })
                .unwrap_or_else(|e| panic!("pragma {sql} failed: {e}"))
                .unwrap_or_else(|| panic!("pragma {sql} returned no row"))
            };
            Ok(run(&mut read))
        })
        .unwrap_or_else(|e| panic!("lease for pragma evidence failed: {e}"))
}

/// Assert one scan succeeded, with a readable failure message.
#[must_use]
pub fn scan_ok(store: &SQLStore, job: &FolderJob<'_>) -> Phase1FolderOutcome {
    let token = nd_scanner::contract::CancelToken::new();
    phase1::scan_folder(store, job, &token).unwrap_or_else(|e| panic!("scan_folder failed: {e}"))
}

/// A whole-library `LibraryScan` — the multi-folder entry the SC-06 controller takes.
///
/// `full` is Go's `lib.FullScanInProgress` after job creation (`scanner.go:97`), and
/// `last_scan_started_at` is the instant `isOutdated`'s full-scan arm compares the stored
/// `folder.updated_at` against (`folder_entry.go:73`). Pass a stamp strictly later than the
/// previous scan's stored `updated_at` when a full pass must re-import through that arm
/// rather than through an accidental hash change.
#[must_use]
pub fn library_scan<'a>(
    library_id: i64,
    root: &'a Path,
    mapping: &'a MappingContext,
    scan_stamp: DateTime<Utc>,
    last_scan_started_at: DateTime<Utc>,
    full: bool,
) -> LibraryScan<'a> {
    let mut scan = LibraryScan::quick(
        library_id,
        root.as_os_str().to_os_string(),
        scan_stamp,
        last_scan_started_at,
        mapping,
    );
    scan.full_scan_in_progress = full;
    scan
}

/// Seed the library row with EXPLICIT scan fields — `full_scan_in_progress` and
/// `last_scan_started_at` — so a test driving the scan through
/// [`nd_scanner::phase1::LibraryScan::from_library_row`] exercises the real repository read
/// rather than an injected guess. [`seed_library`] pins both to [`stamp`] and `true`, which
/// is what the single-folder verticals need but not what a change-detection test does.
pub fn seed_library_scan_row(
    store: &SQLStore,
    id: i64,
    name: &str,
    path: &Path,
    last_scan_started_at: DateTime<Utc>,
    full_scan_in_progress: bool,
) {
    let record = LibraryRecord {
        id,
        name: name.to_string(),
        path: path.display().to_string(),
        remote_path: String::new(),
        last_scan_at: last_scan_started_at,
        last_scan_started_at,
        full_scan_in_progress,
        updated_at: stamp(),
        created_at: stamp(),
        total_songs: 0,
        total_albums: 0,
        total_artists: 0,
        total_folders: 0,
        total_files: 0,
        total_missing_files: 0,
        total_size: 0,
        total_duration: 0.0,
        default_new_users: false,
        pid_album: String::new(),
        pid_track: String::new(),
        scanned_pid_album: String::new(),
        scanned_pid_track: String::new(),
    };
    store
        .try_with_tx_sync::<library_repo::LibraryWriteResult, LibraryWriteError>(|tx| {
            library_repo::put_in_tx(tx, &record, &stamp(), &[])
        })
        .unwrap_or_else(|e| panic!("seed_library_scan_row({id}) failed: {e}"));
}

/// Run a whole-library scan through the public controller and assert it started and
/// finished without a pre-walk error. A [`LibraryScanOutcome::stopped`] or a non-empty
/// [`LibraryScanOutcome::failures`] is NOT an error here — those are the per-folder signals
/// a test asserts on directly, the way Go's `phaseFolders` returns them.
pub async fn scan_all_ok(
    store: &SQLStore,
    scan: &LibraryScan<'_>,
    scope: &ScanScope,
) -> LibraryScanOutcome {
    let token = nd_scanner::contract::CancelToken::new();
    phase1::scan_library(store, scan, scope, &token)
        .await
        .unwrap_or_else(|e| panic!("scan_library failed: {e}"))
}

/// The stored update-info map the controller would have read, via the same public
/// [`nd_db::folder_repo::FolderRepo::get_folder_update_info`] over the whole library.
/// Reading it back is what turns "the folder row carried Go's hash" into an executed claim
/// instead of a trust in an outcome field.
#[must_use]
pub fn update_info(store: &SQLStore, library_id: i64) -> BTreeMap<String, FolderUpdateInfo> {
    store
        .with_connection_sync(|conn| {
            Ok(
                match FolderRepo::new(conn).get_folder_update_info(library_id, &[]) {
                    Ok(map) => map,
                    Err(e) => panic!("update-info read failed: {e}"),
                },
            )
        })
        .unwrap_or_else(|e| panic!("cannot lease a connection for the update-info read: {e}"))
}

/// Flag one stored media row missing by PATH — the state Go's `missing` re-import arm
/// (`phase_1_folders.go:262`) starts from, when the test needs it without a preceding scan
/// that produced it.
pub fn set_media_missing(store: &SQLStore, path: &str, missing: bool) {
    execute(
        store,
        "UPDATE media_file SET missing = ?1 WHERE path = ?2",
        &[
            nd_db::sqlrepo::Value::Bool(missing),
            nd_db::sqlrepo::Value::Text(path.to_string()),
        ],
    );
}

/// Portable guard against collapsed test timestamps: the scan stamps and the file times
/// every fixture pins must stay **strictly** ordered. If two of them ever became equal —
/// by an edit, or by a helper accidentally returning the same instant — the assertions
/// that depend on "this entry's mtime RAISED the folder mtime" and on "the second/third
/// scan's stamp differs from the first" would stop proving anything while still passing.
/// Cheap and platform-independent, so the fault tests call it first and every existing
/// timestamp assertion inherits the guarantee.
pub fn assert_stamp_ordering() {
    assert!(
        stamp() < later_stamp(),
        "stamp() must precede later_stamp(): {} vs {}",
        stamp().to_rfc3339(),
        later_stamp().to_rfc3339()
    );
    assert!(
        later_stamp() < laterest_stamp(),
        "later_stamp() must precede laterest_stamp(): {} vs {}",
        later_stamp().to_rfc3339(),
        laterest_stamp().to_rfc3339()
    );
    assert!(
        file_stamp() < stamp(),
        "file_stamp() must precede the scan stamp: {} vs {}",
        file_stamp().to_rfc3339(),
        stamp().to_rfc3339()
    );
    assert!(
        file_time() < later_file_time(),
        "file_time() must precede later_file_time(), or a later-mtime assertion is vacuous"
    );
}

/// The canonicalized scratch directory a test needs **outside** the library root: the
/// out-of-root endpoint tests place a real media file here and link into it from inside
/// the tree. It carries its own unique suffix like [`Tree::new`], and
/// [`remove_external_root`] clears it. Canonicalized for the same reason `Tree::new` is:
/// `$TMPDIR` can be reached through a symlink (`/var` → `/private/var`), and the
/// outside-root relation compares real path identities.
#[must_use]
pub fn external_root(label: &str) -> PathBuf {
    let path = PathBuf::from(nd_utils::files::temp_file_name(
        &format!("nd-scanner-phase1-{label}-"),
        ".ext",
    ));
    std::fs::create_dir_all(&path).unwrap_or_else(|e| panic!("cannot create {path:?}: {e}"));
    std::fs::canonicalize(&path).unwrap_or_else(|e| panic!("cannot canonicalize {path:?}: {e}"))
}

/// Best-effort removal of an [`external_root`] scratch directory, called after the
/// assertions so a failing one still leaves [`Tree`]'s own `Drop` to clean the library.
pub fn remove_external_root(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

/// Every row the phase-1 vertical can write, **per table**. A rejected or aborted scan must
/// leave this exactly as the test observed before it ran — the "provably no DB writes" half of
/// the preflight contract, asserted over every writable table rather than one.
///
/// Per table rather than summed deliberately: a single global sum passes when one helper writes
/// a row and another deletes one (`folder` +1, `item_artwork` −1 nets to zero), so the equality
/// has to be per table for a stray junction, an FTS ghost, or a rolled-back art deletion to
/// fail. [`writable_row_count`] stays for the tests that only need the cheap guard.
///
/// The table list is the SINGLE list both phase-1 suites use for their snapshots:
/// `tests/phase1_faults.rs` re-exports this one instead of keeping its own copy, so a table
/// added here lands in the rollback snapshot immediately and order drift cannot hide a leg.
/// Alongside `folder`, the junctions, the FTS indexes, `artwork_queue`, and `item_artwork`, it
/// includes `annotation`: `persist_folder_delta` runs the warn-only ReassignAnnotation leg on an
/// album id transition (`nd-db/src/scanner_writes.rs:705-716`), which moves rows IN `annotation`
/// (`nd-db/src/sql_annotations.rs:393-398`) and then runs `UPDATE album SET average_rating`
/// (`nd-db/src/sql_annotations.rs:330-334`). A snapshot without `annotation` could not see that
/// leg at all, so an FTS ghost, a queued row, an art deletion, or an annotation leak all fail here.
#[must_use]
pub fn writable_rows(store: &SQLStore) -> Vec<(&'static str, i64)> {
    WRITABLE_TABLES
        .iter()
        .map(|table| {
            (
                *table,
                count(store, &format!("SELECT COUNT(*) FROM {table}")),
            )
        })
        .collect()
}

/// Assert one helper's abort/rejection path wrote nothing, **table by table**, against the
/// snapshot the test took before it ran. One implementation for both phase-1 suites so the
/// claim cannot be narrowed by a local variant: a stray junction row, an FTS ghost, a queue
/// item, or a deleted `item_artwork` row fails the per-table equality even when another leg
/// compensates it, which a single summed count would hide.
///
/// `annotation` is in the snapshot, and no case that calls this can legitimately see the
/// warn-only ReassignAnnotation leg change it: that leg runs only inside the folder transaction
/// after an album id transition (`nd-db/src/scanner_writes.rs:702-716`), and every caller here
/// either never reaches persistence (preflight rejection, cancellation before the DB read,
/// projection refusal), aborts before it (children-read failure), or rolls it back (injected
/// fatal leg). So a surviving annotation row is a real leak, not an expected warn-only write.
/// The accepted path is the opposite case and uses [`assert_wrote`], whose `>=` check tolerates
/// a reassign moving `item_id` without changing the count.
pub fn assert_no_writes(store: &SQLStore, before: &[(&'static str, i64)], case: &str) {
    let after = writable_rows(store);
    assert_eq!(
        after.len(),
        before.len(),
        "{case}: the writable snapshot covers the same table set before and after"
    );
    for ((table, after), (before_table, before_count)) in after.iter().zip(before.iter()) {
        assert_eq!(
            table, before_table,
            "{case}: snapshot table order drifted ({before_table} vs {table})"
        );
        assert_eq!(
            *after, *before_count,
            "{case}: a rejected or aborted scan changed {table} ({before_count} -> {after});              the claim is per table, so a compensating write in another table cannot hide this"
        );
    }
}

/// Assert one helper's accepted path wrote rows, table by table: nothing may be **deleted**
/// outside the artwork invalidation leg, and at least one table must gain rows. Called once per
/// vertical's happy path so the matching [`assert_no_writes`] claims are non-vacuous.
pub fn assert_wrote(store: &SQLStore, before: &[(&'static str, i64)], case: &str) {
    let after = writable_rows(store);
    let mut gained = 0usize;
    for ((table, after), (before_table, before_count)) in after.iter().zip(before.iter()) {
        assert_eq!(table, before_table, "{case}: snapshot table order drifted");
        assert!(
            *after >= *before_count,
            "{case}: an accepted scan deleted from {table} ({before_count} -> {after})"
        );
        if *after > *before_count {
            gained += 1;
        }
    }
    assert!(
        gained > 0,
        "{case}: an accepted scan wrote nothing, so the no-write assertions prove nothing:          {before:?} -> {after:?}"
    );
}

/// The summed row count over [`WRITABLE_TABLES`]. Kept for cheap "nothing was written" guards;
/// prefer [`writable_rows`] where a compensating write/delete must not cancel out.
#[must_use]
pub fn writable_row_count(store: &SQLStore) -> i64 {
    WRITABLE_TABLES
        .iter()
        .map(|table| count(store, &format!("SELECT COUNT(*) FROM {table}")))
        .sum()
}

/// The writable tables a phase-1 scan can touch, including the derived indexes, the queue,
/// and the artwork rows the queue legs DELETE (`nd-db`'s `scanner_writes` reports
/// `deleted_artwork` for exactly those: `artwork_queue_repo.rs` deletes `item_artwork` rows
/// for re-imported and missing media). A no-write or rollback claim that omitted
/// `item_artwork` would miss an art deletion, so it belongs in both snapshots.
pub const WRITABLE_TABLES: &[&str] = &[
    "folder",
    "media_file",
    "album",
    "artist",
    "tag",
    "library_artist",
    "library_tag",
    "album_artists",
    "album_tags",
    "media_file_artists",
    "media_file_tags",
    "artwork_queue",
    "item_artwork",
    "annotation",
    "media_file_fts",
    "album_fts",
    "artist_fts",
];

/// Walk ONE selected folder with the committed SC-02 walker, with the target as the only
/// configured target — i.e. exactly the scope a standalone `scan_folder` captures.
///
/// This is the oracle for the selected-scope parity claims: the walker enters the target as a
/// fresh walk root and pushes its identity, so its live identities after entry are the
/// configured root plus the target — the same context `phase1` seeds. Comparing hashes, child
/// subfolder counts, and cycle/depth warnings through this helper compares the two code paths
/// on one scope rather than comparing an ingest against itself.
pub async fn walk_selected(
    root: &Path,
    target: &str,
    follow_symlinks: bool,
) -> (
    Vec<nd_scanner::walk::FolderEvent>,
    nd_scanner::walk::WalkSummary,
) {
    use nd_scanner::contract::{CancelToken, LibraryRoot, WalkRequest, WalkTarget};
    use nd_scanner::storage::LogicalPath;
    use nd_scanner::walk::WalkHandle;

    let request = WalkRequest {
        libraries: vec![LibraryRoot {
            id: 1,
            name: "selected-scope".to_string(),
            root: root.as_os_str().to_os_string(),
        }],
        targets: vec![WalkTarget {
            library_id: 1,
            path: LogicalPath::parse(target).unwrap_or_else(|e| panic!("path {target}: {e}")),
        }],
        follow_symlinks,
        ignore_dot_folders: true,
    };
    let mut handle = WalkHandle::start(request, CancelToken::new());
    let mut events = Vec::new();
    while let Some(event) = handle.next().await {
        events.push(event);
    }
    let summary = handle.terminal().await;
    (events, summary)
}

/// A `FolderJob` with symlink following **disabled**, the `FollowSymlinks: false` config
/// arm. Kept separate from [`job`] so no existing test's defaults shift.
#[must_use]
pub fn job_without_symlinks<'a>(
    library_id: i64,
    root: &'a Path,
    relative: &str,
    scan_stamp: DateTime<Utc>,
    mapping: &'a MappingContext,
) -> FolderJob<'a> {
    let mut job = job(library_id, root, relative, scan_stamp, mapping);
    job.follow_symlinks = false;
    job
}
