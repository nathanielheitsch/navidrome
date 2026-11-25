//! BOOT-01 increment E: startup assembly acceptance tests.
//!
//! Every case starts from a **real** [`nd_cli::preflight::preflight`] run against
//! a config file inside an injected temp cwd, with an injected environment and an
//! injected effective uid. No test mutates the process environment or the process
//! cwd, and every tree is a `tempfile::TempDir` removed on drop.
//!
//! Coverage against `boot-api.md:193-197` and this lane's decision record:
//! * absent data/cache directories are created by `initialize`, and only those;
//! * the configured music root is unchanged by startup (contents, mode, and no
//!   database file inside it) — `Dir::path` is never called on it;
//! * a fresh startup commits one library, two signing keys, the marker, the dev
//!   admin and its grant; a restart changes nothing (same ciphertexts, same
//!   password, same stats, no second admin, no second library);
//! * the no-password case sets the marker and leaves zero users;
//! * an invalid music root fails in **preflight** and creates no directory and no
//!   database file anywhere;
//! * an invalid DB input is refused by the pre-classification, before any
//!   directory exists;
//! * a real bootstrap refusal (changed library root) and a corrupt stored secret
//!   both fail closed and never yield a ready state;
//! * custom data/cache permissions carried by a callable preflight's
//!   `Dir::with_perm` are preserved when the directory is created, and an existing
//!   directory's mode is left alone;
//! * the heavy chain demonstrably runs on a blocking worker thread, proved by the
//!   thread id the closure reports — on a single-threaded runtime, with no sleep
//!   and no timing threshold.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use nd_auth::initial_setup::INITIAL_SETUP_FLAG_KEY;
use nd_cli::preflight::{preflight, PreflightOptions};
use nd_cli::startup::{initialize, StartupError, StartupStage};
use nd_conf::Dir;
use nd_db::execution::query_optional;
use nd_db::hydrate::row_to_library;
use nd_db::library_repo::{self, DEFAULT_LIBRARY_ID};
use nd_db::property_repo::get_sql;
use nd_db::store::SQLStore;

/// Everything startup is allowed to have written, read straight from the tables.
#[derive(Debug, PartialEq)]
struct Snapshot {
    library: library_repo::LibraryRecord,
    session_ciphertext: String,
    public_ciphertext: String,
    marker: Option<String>,
    grants: Vec<(String, i64)>,
    admin_passwords: Vec<String>,
    user_count: i64,
}

fn snapshot(store: &SQLStore) -> Snapshot {
    let library = store
        .with_connection_sync(|conn| {
            query_optional(
                conn,
                &library_repo::get_sql(DEFAULT_LIBRARY_ID),
                row_to_library,
            )
        })
        .expect("read library")
        .expect("library 1 exists after a committed startup");
    let read = |key: &str| {
        store
            .with_connection_sync(|conn| {
                query_optional(conn, &get_sql(key), |row| row.get::<_, String>(0))
            })
            .expect("read property")
    };
    let (grants, admin_passwords, user_count) = store
        .with_connection_sync(|conn| {
            let mut statement = conn.prepare(
                "SELECT user_id, library_id FROM user_library ORDER BY user_id, library_id",
            )?;
            let grants = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut admins = conn.prepare("SELECT password FROM user WHERE user_name = 'admin'")?;
            let admin_passwords = admins
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            let user_count =
                conn.query_row("SELECT count(*) FROM user", [], |row| row.get::<_, i64>(0))?;
            Ok((grants, admin_passwords, user_count))
        })
        .expect("read grants and users");
    Snapshot {
        library,
        session_ciphertext: read(nd_cli::signing_keys::JWT_SECRET_PROPERTY)
            .expect("JWTSecret committed"),
        public_ciphertext: read(nd_cli::signing_keys::JWT_PUBLIC_SECRET_PROPERTY)
            .expect("JWTPublicSecret committed"),
        marker: read(INITIAL_SETUP_FLAG_KEY),
        grants,
        admin_passwords,
        user_count,
    }
}

/// An isolated per-test tree.
struct Sandbox {
    inner: tempfile::TempDir,
    cwd: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let inner = tempfile::Builder::new()
            .prefix(&format!("nd-cli-startup-{tag}-"))
            .tempdir()
            .expect("isolated temp root");
        let cwd = std::fs::canonicalize(inner.path()).expect("canonicalize temp root");
        Sandbox { inner, cwd }
    }

    fn root(&self) -> &Path {
        self.inner.path()
    }

    fn cwd(&self) -> &Path {
        self.cwd.as_path()
    }

    fn dir(&self, relative: &str) -> PathBuf {
        let p = self.root().join(relative);
        std::fs::create_dir_all(&p).expect("create sandbox dir");
        p
    }

    fn file(&self, relative: &str, contents: &str) -> PathBuf {
        let p = self.root().join(relative);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&p, contents).expect("write sandbox file");
        p
    }

    /// Write a config file and return its path relative to the injected cwd.
    fn config(&self, name: &str, body: &str) -> String {
        self.file(name, body);
        name.to_string()
    }

    /// List every path under the tree, relative to the root.
    fn entries(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![self.root().to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read dir") {
                let entry = entry.expect("entry");
                let path = entry.path();
                out.push(
                    path.strip_prefix(self.root())
                        .expect("inside root")
                        .to_string_lossy()
                        .into_owned(),
                );
                if path.is_dir() {
                    stack.push(path);
                }
            }
        }
        out.sort();
        out
    }

    /// Any file whose name ends in `.db` (or a SQLite sidecar), anywhere.
    fn db_files(&self) -> Vec<String> {
        self.entries()
            .into_iter()
            .filter(|p| p.ends_with(".db") || p.ends_with(".db-wal") || p.ends_with(".db-shm"))
            .collect()
    }
}

/// A boot-shaped config: a relative music root, relative data and cache folders,
/// and a real port, so the preflight takes the same path a boot would.
fn boot_toml(music: &str, data: &str, cache: &str, password: Option<&str>) -> String {
    let mut body = format!(
        "MusicFolder = \"{music}\"\nDataFolder = \"{data}\"\nCacheFolder = \"{cache}\"\nPort = 4611\n"
    );
    if let Some(pw) = password {
        body.push_str(&format!("DevAutoCreateAdminPassword = \"{pw}\"\n"));
    }
    body
}

/// A deterministic synthetic stamp. Restarts use a later one, so a rewrite of
/// `updated_at` would be visible rather than masked by an identical clock value.
fn stamp(offset: u32) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&format!("2024-06-01T{:02}:00:00Z", 10 + offset))
        .expect("synthetic stamp")
        .with_timezone(&Utc)
}

/// Run the real preflight with an injected cwd, empty env and a non-root uid.
fn boot_preflight(
    sandbox: &Sandbox,
    config_rel: &str,
) -> Result<nd_cli::preflight::PreflightOutcome, nd_cli::preflight::PreflightError> {
    preflight(PreflightOptions {
        config_file: Some(config_rel),
        env: Some(Box::new(|_key| None)),
        startup_cwd: Some(sandbox.cwd()),
        effective_uid: Some(1000),
        allow_zero_port: false,
    })
}

/// Renders an error and its whole source chain, for disclosure assertions.
fn full_text(err: &StartupError) -> String {
    let mut text = format!("{err} | debug {err:?}");
    let mut source = <StartupError as std::error::Error>::source(err);
    while let Some(s) = source {
        text.push_str(&format!(" | source {s} | source-debug {s:?}"));
        source = s.source();
    }
    text
}

#[tokio::test]
async fn fresh_startup_creates_data_and_cache_and_commits_setup() {
    let sandbox = Sandbox::new("fresh");
    sandbox.dir("music");
    let config = sandbox.config(
        "conf.toml",
        &boot_toml("music", "data", "cache", Some("synthetic-dev-password")),
    );
    let outcome = boot_preflight(&sandbox, &config).expect("preflight of a boot-shaped config");
    assert!(!outcome.data_folder.exists(), "preflight creates nothing");
    assert!(!sandbox.root().join("data").exists());
    assert!(!sandbox.root().join("cache").exists());

    let state = initialize(&outcome, stamp(1))
        .await
        .expect("startup of a fresh config");

    // Directories: exactly the two configured ones, plus the DB inside data.
    assert!(state.data_dir.is_dir(), "data directory created");
    assert!(state.cache_dir.is_dir(), "cache directory created");
    assert_eq!(
        state.data_dir,
        sandbox.cwd().join("data"),
        "the relative DataFolder was anchored to the injected startup cwd"
    );
    assert_eq!(
        state.cache_dir,
        sandbox.cwd().join("cache"),
        "the relative CacheFolder was anchored to the injected startup cwd"
    );
    assert!(state.database.migrated_on_open, "fresh database migrated");
    assert!(
        state.database.anchored_to_startup_cwd,
        "the loader's relative DbPath (<DataFolder>/navidrome.db?<query>) was anchored to the startup cwd, not the process cwd"
    );
    assert_eq!(
        state.database.pool_size,
        nd_db::pool::pool_size(),
        "the production pool size (max(4, NumCPU)) is used"
    );

    // Committed setup.
    assert!(state.bootstrap.library_created, "library 1 inserted");
    assert!(
        state.bootstrap.session_secret_created && state.bootstrap.public_secret_created,
        "both signing properties inserted on a fresh store"
    );
    assert!(state.bootstrap.setup_marker_written, "setup marker written");
    assert!(state.bootstrap.dev_admin_created, "dev admin created");
    assert_eq!(state.bootstrap.grants.len(), 1, "the admin got one grant");

    let snap = snapshot(&state.store);
    assert_eq!(snap.user_count, 1, "exactly one user");
    assert_eq!(snap.admin_passwords.len(), 1, "one admin row");
    assert_eq!(snap.grants.len(), 1);
    assert_eq!(
        snap.marker.as_deref(),
        Some(nd_db::time::to_text(&stamp(1)).as_str()),
        "the marker row carries the single bootstrap stamp"
    );

    // The music root is untouched.
    let music = sandbox.root().join("music");
    assert_eq!(
        std::fs::read_dir(&music).expect("music dir").count(),
        0,
        "nothing was created inside the configured music root"
    );
    assert!(
        sandbox.db_files().iter().all(|f| f.starts_with("data/")),
        "every database file lives under the data folder: {:?}",
        sandbox.db_files()
    );
}

#[tokio::test]
async fn restart_changes_no_committed_state() {
    let sandbox = Sandbox::new("restart");
    sandbox.dir("music");
    let config = sandbox.config(
        "conf.toml",
        &boot_toml("music", "data", "cache", Some("synthetic-dev-password")),
    );

    let first = boot_preflight(&sandbox, &config).expect("preflight");
    let state_a = initialize(&first, stamp(1)).await.expect("first startup");
    let snapshot_a = snapshot(&state_a.store);
    assert!(state_a.bootstrap.library_created);

    // Act: a second startup against the same config and cwd.
    let second = boot_preflight(&sandbox, &config).expect("preflight again");
    let state_b = initialize(&second, stamp(2))
        .await
        .expect("restart startup");
    assert!(
        !state_b.database.migrated_on_open,
        "restart must not migrate a second time"
    );
    assert_eq!(state_b.database.migration_count, 1, "still one migration");
    assert!(
        !state_b.bootstrap.library_created,
        "restart verifies the library rather than inserting it"
    );
    assert!(
        !state_b.bootstrap.session_secret_created && !state_b.bootstrap.public_secret_created,
        "existing signing secrets are reused, never rotated"
    );
    assert!(
        !state_b.bootstrap.setup_marker_written,
        "the marker is already present"
    );
    assert!(
        !state_b.bootstrap.dev_admin_created,
        "the existing admin is found, not re-created"
    );

    let snapshot_b = snapshot(&state_b.store);
    assert_eq!(
        snapshot_a.session_ciphertext, snapshot_b.session_ciphertext,
        "persisted session secret is byte-identical across restarts"
    );
    assert_eq!(
        snapshot_a.public_ciphertext, snapshot_b.public_ciphertext,
        "persisted public secret is byte-identical across restarts"
    );
    assert_eq!(
        snapshot_a.admin_passwords, snapshot_b.admin_passwords,
        "the admin password hash is unchanged"
    );
    assert_eq!(
        snapshot_a.user_count, snapshot_b.user_count,
        "no second user"
    );
    assert_eq!(snapshot_a.grants, snapshot_b.grants, "grants unchanged");
    assert_eq!(snapshot_a.marker, snapshot_b.marker, "marker unchanged");
    let stats = |r: &library_repo::LibraryRecord| {
        (
            r.total_songs,
            r.total_albums,
            r.total_artists,
            r.total_folders,
            r.total_files,
            r.total_missing_files,
            r.total_size,
            r.total_duration,
            r.full_scan_in_progress,
            r.last_scan_at,
            r.last_scan_started_at,
        )
    };
    assert_eq!(
        stats(&snapshot_a.library),
        stats(&snapshot_b.library),
        "library stats and never-scanned sentinels unchanged"
    );
    assert_eq!(
        snapshot_a.library.path, snapshot_b.library.path,
        "configured root unchanged"
    );
    assert_eq!(
        snapshot_a.library.created_at, snapshot_b.library.created_at,
        "library stamps unchanged"
    );
    assert_eq!(
        snapshot_a.library.updated_at, snapshot_b.library.updated_at,
        "library updated_at unchanged, so no rewrite happened"
    );
}

#[tokio::test]
async fn no_dev_password_sets_marker_and_leaves_zero_users() {
    let sandbox = Sandbox::new("no-password");
    sandbox.dir("music");
    let config = sandbox.config("conf.toml", &boot_toml("music", "data", "cache", None));
    let outcome = boot_preflight(&sandbox, &config).expect("preflight");

    let state = initialize(&outcome, stamp(3)).await.expect("startup");
    assert!(state.bootstrap.setup_marker_written, "marker set");
    assert!(
        !state.bootstrap.dev_admin_created,
        "no dev admin is created without a password"
    );
    assert!(
        state.bootstrap.library_created,
        "the library is still initialized"
    );
    assert_eq!(state.bootstrap.grants.len(), 0, "no grants without a user");

    let snap = snapshot(&state.store);
    assert_eq!(snap.user_count, 0, "zero users");
    assert!(snap.admin_passwords.is_empty(), "no admin row");
    assert_eq!(
        snap.marker.as_deref(),
        Some(nd_db::time::to_text(&stamp(3)).as_str()),
        "the marker row carries the single bootstrap stamp"
    );
    // createAdmin remains available: the marker is present but no user exists.
    assert_eq!(snap.grants.len(), 0);
}

#[tokio::test]
async fn invalid_music_root_fails_in_preflight_and_creates_nothing() {
    let sandbox = Sandbox::new("bad-music");
    // No music directory: the configured root does not exist.
    let config = sandbox.config(
        "conf.toml",
        &boot_toml("does-not-exist", "data", "cache", Some("pw")),
    );

    let err = boot_preflight(&sandbox, &config).expect_err("a missing music root is fatal");
    let text = err.to_string();
    assert!(
        !text.contains("data/") && !text.contains("cache"),
        "preflight failure must not describe directories it did not touch: {text}"
    );

    // Nothing was created: no data, no cache, no database file anywhere.
    assert!(!sandbox.root().join("data").exists(), "no data directory");
    assert!(!sandbox.root().join("cache").exists(), "no cache directory");
    assert!(
        !sandbox.root().join("does-not-exist").exists(),
        "music stays absent"
    );
    assert!(sandbox.db_files().is_empty(), "no database file created");
    assert_eq!(
        sandbox.entries(),
        vec!["conf.toml".to_string()],
        "the config file is the only thing in the tree"
    );
}

#[tokio::test]
async fn invalid_db_input_is_refused_before_any_directory_exists() {
    let cases = [
        (
            "badparam.db?unknown_param=1",
            "an unknown parameter is refused, not dropped",
        ),
        (
            ":memory:?_fk=off",
            "a private memory target would give the pool independent databases",
        ),
        (
            "file:relative/place.db",
            "a relative file: URI naming a file cannot be anchored without rewriting it",
        ),
    ];
    for (db_path, why) in cases {
        let sandbox = Sandbox::new("bad-db");
        sandbox.dir("music");
        let body = format!(
            "MusicFolder = \"music\"\nDataFolder = \"data\"\nCacheFolder = \"cache\"\nPort = 4611\nDbPath = \"{db_path}\"\n"
        );
        let config = sandbox.config("conf.toml", &body);
        let outcome = boot_preflight(&sandbox, &config).expect("preflight does not resolve URIs");

        let err = initialize(&outcome, stamp(4))
            .await
            .expect_err(&format!("{db_path} must be refused: {why}"));
        assert_eq!(
            err.stage,
            StartupStage::DatabaseInput,
            "{db_path}: refused at the pre-classification stage: {err}"
        );
        assert!(
            err.errno.is_none(),
            "{db_path}: nothing touched the filesystem: {err}"
        );
        assert!(
            !sandbox.root().join("data").exists(),
            "{db_path}: no data directory"
        );
        assert!(
            !sandbox.root().join("cache").exists(),
            "{db_path}: no cache directory"
        );
        assert!(sandbox.db_files().is_empty(), "{db_path}: no database file");
        assert!(
            full_text(&err).find(db_path).is_none(),
            "{db_path}: the input is not echoed: {}",
            full_text(&err)
        );
    }
}

#[tokio::test]
async fn unopenable_database_never_returns_a_ready_state() {
    // A read-only parent: SQLite refuses the open, and startup reports the
    // database stage rather than creating anything or returning a store.
    let sandbox = Sandbox::new("unopenable");
    sandbox.dir("music");
    let locked = sandbox.dir("data");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500))
        .expect("make the data folder read-only");
    let config = sandbox.config(
        "conf.toml",
        &boot_toml("music", "data", "cache", Some("pw")),
    );
    let outcome = boot_preflight(&sandbox, &config).expect("preflight");

    let err = initialize(&outcome, stamp(5))
        .await
        .expect_err("the database cannot be opened in a read-only directory");
    assert_eq!(err.stage, StartupStage::Database, "{err}");
    assert!(
        err.sqlite.is_some(),
        "numeric SQLite codes are reported: {err}"
    );
    // Directories are created before the open, exactly as Go does them at config
    // load; what must not happen is a database file, a bootstrap row, or a ready
    // state.
    assert!(sandbox.db_files().is_empty(), "no database file");
    let goose = sandbox.root().join("data").join("navidrome.db").exists();
    assert!(
        !goose,
        "no database file was left behind by the failed open"
    );
    // Restore so the TempDir can clean up. Permission limitation: the mode bits
    // only deny access for an unprivileged user; under root they are bypassed, in
    // which case this case would succeed — the "no db file left behind" assertion
    // holds either way.
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700))
        .expect("restore permissions");
}

#[tokio::test]
async fn changed_library_root_refuses_startup_and_leaves_state_intact() {
    let sandbox = Sandbox::new("conflict");
    sandbox.dir("music");
    sandbox.dir("other");
    let config = sandbox.config(
        "conf.toml",
        &boot_toml("music", "data", "cache", Some("pw")),
    );
    let outcome = boot_preflight(&sandbox, &config).expect("preflight");
    let state = initialize(&outcome, stamp(6)).await.expect("first startup");
    let before = snapshot(&state.store);
    assert_eq!(
        before.library.path,
        sandbox.cwd().join("music").to_str().unwrap()
    );

    // Act: point the same database at a different root.
    let second_config = sandbox.config(
        "conf2.toml",
        &boot_toml("other", "data", "cache", Some("pw")),
    );
    let second = boot_preflight(&sandbox, &second_config).expect("preflight of the new root");
    let err = initialize(&second, stamp(7))
        .await
        .expect_err("repointing a library that owns rows is refused");
    assert_eq!(err.stage, StartupStage::Bootstrap, "{err}");
    assert_eq!(
        err.reason, "startup refused by the committed database state",
        "{err}"
    );
    assert!(
        err.sqlite.is_none(),
        "a domain refusal has no SQLite codes: {err}"
    );
    assert!(
        !full_text(&err).contains("other"),
        "the refused root is not echoed: {}",
        full_text(&err)
    );

    // Assert: the store is exactly as it was — no repointing, no partial write.
    let after = snapshot(&state.store);
    assert_eq!(before, after, "a refused startup changes nothing");
}

#[tokio::test]
async fn corrupt_stored_secret_fails_closed_and_returns_no_store() {
    let sandbox = Sandbox::new("corrupt-secret");
    sandbox.dir("music");
    let config = sandbox.config(
        "conf.toml",
        &boot_toml("music", "data", "cache", Some("pw")),
    );
    let outcome = boot_preflight(&sandbox, &config).expect("preflight");
    let state = initialize(&outcome, stamp(8)).await.expect("first startup");
    let original = snapshot(&state.store);
    drop(state);

    // Corrupt the stored session secret the way a truncated/malformed property
    // would look, then restart.
    let db = sandbox.root().join("data").join("navidrome.db");
    let raw = nd_dbschema::schema::open(db.to_str().unwrap()).expect("open the database");
    raw.execute(
        "UPDATE property SET value = 'not-valid-ciphertext' WHERE id = 'JWTSecret'",
        [],
    )
    .expect("corrupt the stored secret");
    drop(raw);

    let again = boot_preflight(&sandbox, &config).expect("preflight");
    let err = initialize(&again, stamp(9))
        .await
        .expect_err("a corrupt stored secret is corruption, not a missing value");
    assert_eq!(err.stage, StartupStage::Bootstrap, "{err}");
    assert!(
        !full_text(&err).contains("not-valid-ciphertext"),
        "the corrupt value is never echoed: {}",
        full_text(&err)
    );

    // Assert: the corruption is still there — startup refused rather than
    // silently rotating a replacement.
    let reopened = nd_dbschema::schema::open(db.to_str().unwrap()).expect("reopen");
    let value: String = reopened
        .query_row(
            "SELECT value FROM property WHERE id = 'JWTSecret'",
            [],
            |row| row.get(0),
        )
        .expect("read the secret");
    assert_eq!(
        value, "not-valid-ciphertext",
        "no silent rotation at restart"
    );
    let user_count: i64 = reopened
        .query_row("SELECT count(*) FROM user", [], |row| row.get(0))
        .expect("count users");
    assert_eq!(user_count, 1, "the committed first startup is untouched");
    drop(reopened);
    assert_eq!(original.user_count, 1);
}

#[tokio::test]
async fn custom_directory_permissions_are_preserved_when_created() {
    let sandbox = Sandbox::new("perm");
    sandbox.dir("music");
    let config = sandbox.config(
        "conf.toml",
        &boot_toml("music", "data", "cache", Some("pw")),
    );
    let mut outcome = boot_preflight(&sandbox, &config).expect("preflight");

    // A callable preflight may carry an explicit permission through `Dir`
    // (Go's `NewDirWithPerm(..., 0700)` shape). The loader cannot express it in
    // TOML, so the synthetic outcome does — the anchor and the permission must
    // both survive into `initialize`.
    outcome.config.data_folder = Dir::with_perm("data", 0o700);
    outcome.config.cache_folder = Dir::with_perm("cache", 0o750);

    let state = initialize(&outcome, stamp(10)).await.expect("startup");
    assert!(state.data_dir.is_dir() && state.cache_dir.is_dir());
    let data_mode = std::fs::metadata(&state.data_dir)
        .expect("data metadata")
        .permissions()
        .mode()
        & 0o777;
    let cache_mode = std::fs::metadata(&state.cache_dir)
        .expect("cache metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        data_mode, 0o700,
        "the configured data permission was applied"
    );
    assert_eq!(
        cache_mode, 0o750,
        "the configured cache permission was applied"
    );

    // A pre-existing directory keeps the mode it already had: `Dir::path` only
    // applies permissions when it creates.
    drop(state);
    let existing = boot_preflight(&sandbox, &config).expect("preflight again");
    let mut existing = existing;
    existing.config.data_folder = Dir::with_perm("data", 0o700);
    existing.config.cache_folder = Dir::with_perm("cache", 0o700);
    let again = initialize(&existing, stamp(11)).await.expect("restart");
    assert_eq!(
        std::fs::metadata(&again.data_dir)
            .expect("data metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "an existing directory keeps its mode"
    );
    assert_eq!(
        std::fs::metadata(&again.cache_dir)
            .expect("cache metadata")
            .permissions()
            .mode()
            & 0o777,
        0o750,
        "an existing cache directory is not re-permissioned"
    );
}

#[test]
fn heavy_chain_runs_off_the_calling_runtime_thread() {
    // A single-threaded runtime: every future in this test runs on exactly one
    // worker thread, so a different thread id from the blocking closure is a
    // deterministic proof — no sleep, no timing threshold.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("single-threaded runtime");
    let sandbox = Sandbox::new("worker");
    sandbox.dir("music");
    let config = sandbox.config(
        "conf.toml",
        &boot_toml("music", "data", "cache", Some("pw")),
    );
    let outcome = boot_preflight(&sandbox, &config).expect("preflight");

    let caller_thread = std::thread::current().id();
    let state = runtime.block_on(async {
        let in_task = std::thread::current().id();
        assert_eq!(
            in_task, caller_thread,
            "the current_thread runtime runs the task on the calling thread"
        );
        let state = initialize(&outcome, stamp(12)).await.expect("startup");
        assert_eq!(
            std::thread::current().id(),
            in_task,
            "the async continuation resumed on the runtime thread"
        );
        state
    });

    assert_ne!(
        state.worker_thread, caller_thread,
        "directory creation, open/migrate, hashing and the transaction ran on a blocking worker, not the runtime thread"
    );
    // The state was produced and the store works on the runtime thread.
    assert_eq!(snapshot(&state.store).user_count, 1);
    assert!(state.data_dir.is_dir() && state.cache_dir.is_dir());
}

#[tokio::test]
async fn initialized_debug_withholds_store_and_secrets() {
    let sandbox = Sandbox::new("debug");
    sandbox.dir("music");
    let password = "canary-dev-password";
    let config = sandbox.config(
        "conf.toml",
        &boot_toml("music", "data", "cache", Some(password)),
    );
    let outcome = boot_preflight(&sandbox, &config).expect("preflight");
    let state = initialize(&outcome, stamp(13)).await.expect("startup");

    let shown = format!("{state:?}");
    assert!(
        !shown.contains(password),
        "the dev password never appears in Debug"
    );
    assert!(
        !shown.contains(&state.bootstrap.session_secret.expose().to_string()),
        "the session secret never appears in Debug"
    );
    assert!(
        !shown.contains(&state.bootstrap.public_secret.expose().to_string()),
        "the public secret never appears in Debug"
    );
    assert!(
        !shown.contains(".db?"),
        "no DSN with a query appears in Debug"
    );
    assert!(
        !shown.contains("navidrome.db"),
        "no database filename in Debug"
    );
    assert!(
        shown.contains("<pooled store>"),
        "the store is withheld: {shown}"
    );
    assert!(
        shown.contains("worker_thread"),
        "the blocking-thread observation is reported: {shown}"
    );

    // Error Debug/Display carry no path, no secret, no upstream text.
    let err = StartupError {
        stage: StartupStage::Directories,
        reason: "data directory could not be created",
        errno: Some(13),
        sqlite: None,
    };
    let text = full_text(&err);
    assert!(
        text.contains("errno 13"),
        "the numeric errno survives: {text}"
    );
    assert!(
        !text.contains(sandbox.root().to_str().unwrap()),
        "no path: {text}"
    );
    assert!(
        !text.contains("Permission denied"),
        "no upstream OS message: {text}"
    );
}
