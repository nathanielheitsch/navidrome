//! BOOT-01 increment C: bootstrap acceptance tests over a real migrated SQLite
//! database (`docs/rust-port/subplans/boot-api.md` §BOOT-01 steps 4-6).
//!
//! Every input is synthetic: a temp directory as the music root, a temp
//! database, a made-up dev password. Nothing reads a real config, a real
//! credential, or a production database.
//!
//! The fixture mirrors `nd-auth/tests/login_http.rs:37-51`: a `file:` DSN with
//! `mode=rwc&_foreign_keys=on`, a pool whose first handle is migrated by the
//! production `apply_fresh_schema`, then returned to the pool.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use nd_cli::bootstrap::{
    bootstrap, BootstrapError, BootstrapInput, FailureKind, FailureStage, DEFAULT_LIBRARY_NAME,
};
use nd_db::execution::query_optional;
use nd_db::hydrate::row_to_library;
use nd_db::library_repo::{self, LibraryRecord};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::property_repo::get_sql;
use nd_db::store::SQLStore;
use nd_dbschema::schema::apply_fresh_schema;

/// A fresh signing secret is 32 random bytes rendered as 64 hex characters.
const SECRET_LEN: usize = 64;

/// Isolated music root: a real directory, because [`bootstrap`] only ever
/// compares a canonical root that preflight already validated.
fn music_root(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nd-cli-bootstrap-{label}-{}", nd_unique()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("cannot create root {dir:?}: {e}"));
    std::fs::canonicalize(&dir).unwrap_or_else(|e| panic!("cannot canonicalize {dir:?}: {e}"))
}

/// Unique suffix, so two tests in the same process never share a file.
fn nd_unique() -> String {
    nd_id::new_random()
}

/// A migrated temporary database, pool size 1 (the BOOT-01 caller-transaction
/// shape: one connection holds the whole transaction).
fn migrated_store(label: &str) -> (SQLStore, PathBuf) {
    let path = std::env::temp_dir().join(format!("nd-cli-bootstrap-{label}-{}.db", nd_unique()));
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn, 1)).unwrap();
    apply_fresh_schema(&first).unwrap();
    pool.put_back(first).expect("return configured handle");
    (SQLStore::new(pool), path)
}

fn input<'a>(root: &'a Path, password: Option<&'a str>, now: DateTime<Utc>) -> BootstrapInput<'a> {
    BootstrapInput {
        music_root: root,
        // Synthetic key material; the empty-string arm is exercised elsewhere.
        password_encryption_key: "synthetic-test-key",
        dev_password: password,
        now,
    }
}

/// Everything bootstrap is allowed to have written, read straight from the
/// tables. Used to prove a second call changes nothing. `PartialEq` only: the
/// library record holds a `total_duration` float, so it is not `Eq`.
#[derive(Debug, PartialEq)]
struct Snapshot {
    library: LibraryRecord,
    session_ciphertext: String,
    public_ciphertext: String,
    marker: String,
    /// `(user_id, library_id)` grant rows.
    grants: Vec<(String, i64)>,
    /// `user.password` for the dev admin, empty when there is no user.
    password: String,
}

fn snapshot(store: &SQLStore) -> Snapshot {
    let library = store
        .with_connection_sync(|conn| {
            query_optional(
                conn,
                &library_repo::get_sql(library_repo::DEFAULT_LIBRARY_ID),
                row_to_library,
            )
        })
        .expect("read library")
        .expect("library 1 exists after bootstrap");
    let read = |key: &str| -> String {
        store
            .with_connection_sync(|conn| {
                query_optional(conn, &get_sql(key), |row| row.get::<_, String>(0))
            })
            .expect("read property")
            .unwrap_or_else(|| panic!("{key} missing after bootstrap"))
    };
    let (grants, password) = store
        .with_connection_sync(|conn| {
            let mut statement = conn
                .prepare(
                    "SELECT user_id, library_id FROM user_library ORDER BY user_id, library_id",
                )
                .expect("prepare grants");
            let grants = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut users = conn
                .prepare("SELECT id, password FROM user WHERE user_name = 'admin'")
                .expect("prepare users");
            let password = users
                .query_map([], |row| row.get::<_, String>(1))?
                .next()
                .transpose()?
                .unwrap_or_default();
            Ok((grants, password))
        })
        .expect("read grants and user");
    Snapshot {
        library,
        session_ciphertext: read(nd_cli::signing_keys::JWT_SECRET_PROPERTY),
        public_ciphertext: read(nd_cli::signing_keys::JWT_PUBLIC_SECRET_PROPERTY),
        marker: read(nd_auth::initial_setup::INITIAL_SETUP_FLAG_KEY),
        grants,
        password,
    }
}

#[test]
fn fresh_bootstrap_then_restart_commits_one_library_and_two_keys_and_changes_nothing() {
    let label = "fresh";
    let root = music_root(label);
    let (store, db) = migrated_store(label);
    let now = DateTime::parse_from_rfc3339("2024-05-05T10:00:00Z")
        .expect("synthetic stamp")
        .with_timezone(&Utc);
    let password = "synthetic-dev-password";

    // Act: first call on an empty, migrated database.
    let first = bootstrap(&store, &input(&root, Some(password), now)).expect("fresh bootstrap");

    // Assert: the library row is the configured one, with explicit BOOT-01
    // policy values rather than DDL defaults.
    assert!(
        first.library_created,
        "the fresh call inserted the library row"
    );
    assert_eq!(first.library.id, library_repo::DEFAULT_LIBRARY_ID);
    assert_eq!(first.library.name, DEFAULT_LIBRARY_NAME);
    assert_eq!(
        first.library.path,
        root.to_string_lossy(),
        "stored root is canonical"
    );
    assert!(
        first.library.default_new_users,
        "configured library serves new users"
    );
    assert_eq!(first.library.remote_path, "");
    assert_eq!(first.library.total_songs, 0);
    assert_eq!(first.library.total_albums, 0);
    assert_eq!(first.library.total_artists, 0);
    assert_eq!(first.library.total_folders, 0);
    assert_eq!(first.library.total_files, 0);
    assert_eq!(first.library.total_missing_files, 0);
    assert_eq!(first.library.total_size, 0);
    assert_eq!(first.library.total_duration, 0.0);
    assert!(!first.library.full_scan_in_progress);
    assert_eq!(first.library.pid_album, "");
    assert_eq!(first.library.pid_track, "");
    assert_eq!(first.library.scanned_pid_album, "");
    assert_eq!(first.library.scanned_pid_track, "");
    assert_eq!(
        first.library.created_at, now,
        "created_at is the caller stamp"
    );
    assert_eq!(
        first.library.updated_at, now,
        "updated_at is the same stamp"
    );
    assert_eq!(
        first.library.last_scan_at,
        nd_db::time::zero(),
        "never scanned: last_scan_at stays the zero time"
    );
    assert_eq!(
        first.library.last_scan_started_at,
        nd_db::time::zero(),
        "never scanned: last_scan_started_at stays the zero time"
    );

    // Assert: both signing properties were written by this call, independently,
    // and differ from each other.
    assert!(first.session_secret_created);
    assert!(first.public_secret_created);
    assert_eq!(first.session_secret.expose().len(), SECRET_LEN);
    assert_eq!(first.public_secret.expose().len(), SECRET_LEN);
    assert_ne!(
        first.session_secret.expose(),
        first.public_secret.expose(),
        "the session and public keys are independent values"
    );

    // Assert: marker and dev admin, with the admin granted the library.
    assert!(first.setup_marker_written);
    assert!(first.dev_admin_created);
    assert_eq!(
        first.grants.len(),
        1,
        "the dev admin gets exactly one grant"
    );
    assert_eq!(first.grants[0].1, library_repo::DEFAULT_LIBRARY_ID);

    // Assert: the RAW persisted representation of the never-scanned stamps. The
    // repository's insert leg binds them through `nd_db::time::value`, so the
    // stored TEXT is Go's zero time in the port's canonical SQLite form — NOT
    // the DDL column DEFAULT `'0000-00-00 00:00:00'`, which is never reached
    // because the INSERT always supplies an explicit value.
    let raw_stamps: (String, String) = store
        .with_connection_sync(|conn| {
            conn.query_row(
                "SELECT last_scan_at, last_scan_started_at FROM library WHERE id = 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
        })
        .expect("read raw scan stamps");
    let canonical_zero = nd_db::time::to_text(&nd_db::time::zero());
    assert_eq!(
        canonical_zero, "0001-01-01 00:00:00+00:00",
        "canonical zero-time TEXT"
    );
    assert_eq!(
        raw_stamps.0, canonical_zero,
        "persisted last_scan_at representation"
    );
    assert_eq!(
        raw_stamps.1, canonical_zero,
        "persisted last_scan_started_at representation"
    );

    // Assert: the committed state, read from the tables.
    let mut before = snapshot(&store);
    assert_eq!(before.library, first.library);
    assert_eq!(
        before.marker,
        nd_db::time::to_text(&now),
        "marker is the caller stamp"
    );
    assert_eq!(before.grants, first.grants);
    assert_ne!(before.password, "", "the dev admin's hash is stored");
    assert_ne!(before.session_ciphertext, before.public_ciphertext);
    assert!(!before.session_ciphertext.contains(password));
    assert!(
        !before.password.contains(password),
        "the cleartext is never stored"
    );

    // Mutate the committed rows the way a scanner and a password change would:
    // non-zero stats, PID overrides, a scanned-PID, a moved stamp and a bumped
    // token epoch. A restart must leave every one of these alone.
    store
        .with_connection_sync(|conn| {
            conn.execute(
                "UPDATE library SET total_songs = 42, total_albums = 7, total_size = 99, \
                 pid_album = 'pid-album', pid_track = 'pid-track', \
                 scanned_pid_album = 'scanned-album', updated_at = '2020-01-02 03:04:05', \
                 last_scan_at = '2020-01-02 03:04:05' WHERE id = 1",
                [],
            )?;
            conn.execute(
                "UPDATE user SET token_epoch = 17 WHERE user_name = 'admin'",
                [],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
        .expect("seed non-default values");
    // Re-baseline: the comparison below must be against the seeded state.
    before = snapshot(&store);

    // Act: restart of the same database with the same root, with a LATER stamp.
    let later = DateTime::parse_from_rfc3339("2024-05-06T11:00:00Z")
        .expect("synthetic stamp")
        .with_timezone(&Utc);
    let second =
        bootstrap(&store, &input(&root, Some(password), later)).expect("restart bootstrap");

    // Assert: nothing was created a second time.
    assert!(
        !second.library_created,
        "restart verifies, never re-initializes"
    );
    assert!(
        !second.session_secret_created,
        "restart preserves the stored session key"
    );
    assert!(
        !second.public_secret_created,
        "restart preserves the stored public key"
    );
    assert!(
        !second.setup_marker_written,
        "restart leaves the marker alone"
    );
    assert!(
        !second.dev_admin_created,
        "restart never creates a second admin"
    );

    // Assert: the published winners are the committed ones, not new values.
    assert_eq!(
        second.session_secret.expose(),
        first.session_secret.expose(),
        "restart returns the committed session key"
    );
    assert_eq!(
        second.public_secret.expose(),
        first.public_secret.expose(),
        "restart returns the committed public key"
    );
    assert_eq!(second.grants, first.grants);

    // Assert: byte-identical persisted state, including the seeded stats, PIDs,
    // stamps and token epoch that an updating implementation would have moved.
    let after = snapshot(&store);
    assert_eq!(after, before, "restart writes nothing");
    let seeded = full_state(&store);
    assert_eq!(seeded.libraries[0].7, 42, "total_songs preserved");
    assert_eq!(seeded.libraries[0].8, "pid-album", "pid_album preserved");
    assert_eq!(
        seeded.libraries[0].9, "scanned-album",
        "scanned_pid_album preserved"
    );
    assert_eq!(
        seeded.libraries[0].3, "2020-01-02 03:04:05",
        "updated_at is not re-stamped with the later clock"
    );
    assert_eq!(
        seeded.libraries[0].5, "2020-01-02 03:04:05",
        "last_scan_at is not reset to the never-scanned zero time"
    );
    assert_eq!(seeded.users[0].4, 17, "token_epoch is not bumped");

    std::fs::remove_file(&db).ok();
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_relative_root_is_refused_before_any_write() {
    let label = "relative";
    let (store, db) = migrated_store(label);
    let now = DateTime::parse_from_rfc3339("2024-05-05T10:00:00Z")
        .expect("synthetic stamp")
        .with_timezone(&Utc);

    let error = bootstrap(&store, &input(Path::new("music"), Some("pw"), now))
        .expect_err("a relative root is a caller bug");
    assert!(matches!(error, BootstrapError::RelativeMusicRoot));
    let count: i64 = store
        .with_connection_sync(|conn| {
            conn.query_row("SELECT count(*) FROM library", [], |row| row.get(0))
        })
        .expect("count libraries");
    assert_eq!(count, 0, "a refused bootstrap writes no library row");
    let properties: i64 = store
        .with_connection_sync(|conn| {
            conn.query_row("SELECT count(*) FROM property", [], |row| row.get(0))
        })
        .expect("count properties");
    assert_eq!(properties, 0, "a refused bootstrap writes no property row");

    std::fs::remove_file(&db).ok();
}

/// A synthetic absolute path whose final component is not valid UTF-8. Built
/// from raw bytes, so no filesystem object is needed and none is created.
#[cfg(unix)]
fn invalid_utf8_root() -> PathBuf {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let mut bytes = std::env::temp_dir().as_os_str().as_bytes().to_vec();
    bytes.extend_from_slice(b"/nd-cli-bootstrap-");
    bytes.push(0xff);
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

#[test]
#[cfg(unix)]
fn a_non_utf8_root_is_rejected_before_preparation_or_writes() {
    let label = "non-utf8";
    let (store, db) = migrated_store(label);
    let now = DateTime::parse_from_rfc3339("2024-05-05T10:00:00Z")
        .expect("synthetic stamp")
        .with_timezone(&Utc);

    let root = invalid_utf8_root();
    assert!(root.is_absolute(), "the synthetic path is absolute");
    assert!(
        root.to_str().is_none(),
        "the synthetic path is not valid UTF-8"
    );

    let error = bootstrap(&store, &input(&root, Some("pw"), now))
        .expect_err("an unstorable root must be refused");
    assert!(matches!(error, BootstrapError::NonUtf8MusicRoot));
    // The rejected bytes are not echoed in either rendering.
    assert!(!format!("{error}").contains("nd-cli-bootstrap"));
    assert!(!format!("{error:?}").contains("nd-cli-bootstrap"));

    // Nothing was written: no library, no properties, no users or grants.
    assert_eq!(count(&store, "library"), 0);
    assert_eq!(count(&store, "property"), 0);
    assert_eq!(count(&store, "user"), 0);
    assert_eq!(count(&store, "user_library"), 0);

    std::fs::remove_file(&db).ok();
}
/// Count rows of one table, read outside any transaction.
fn count(store: &SQLStore, table: &str) -> i64 {
    let sql = format!("SELECT count(*) FROM {table}");
    store
        .with_connection_sync(|conn| conn.query_row(&sql, [], |row| row.get::<_, i64>(0)))
        .unwrap_or_else(|e| panic!("count {table}: {e}"))
}
/// Text the injected trigger makes SQLite report. It must never surface.
const CANARY: &str = "injected-canary-text-4f2a";

/// A trigger that fails **only** once every other row of the transaction exists:
/// it guards on the library row, both ciphertexts and the admin's grant, and
/// fires on the setup-marker INSERT — the last write of the sequence. So a
/// failure proves the earlier legs ran, and the whole transaction must unwind.
const LATE_FAILURE_TRIGGER: &str = r#"CREATE TRIGGER nd_cli_late_failure BEFORE INSERT ON property FOR EACH ROW
    WHEN NEW.id = 'InitialSetup'
      AND EXISTS (SELECT 1 FROM library WHERE id = 1)
      AND EXISTS (SELECT 1 FROM property WHERE id = 'JWTSecret')
      AND EXISTS (SELECT 1 FROM property WHERE id = 'JWTPublicSecret')
      AND EXISTS (SELECT 1 FROM user_library)
      AND EXISTS (SELECT 1 FROM user WHERE user_name = 'admin')
    BEGIN
      SELECT RAISE(ABORT, 'injected-canary-text-4f2a');
    END"#;

#[test]
fn a_late_failure_inside_the_transaction_rolls_back_every_row_kind_and_a_retry_succeeds() {
    let label = "rollback";
    let root = music_root(label);
    let (store, db) = migrated_store(label);
    let now = DateTime::parse_from_rfc3339("2024-05-05T10:00:00Z")
        .expect("synthetic stamp")
        .with_timezone(&Utc);
    let password = "synthetic-dev-password";

    store
        .with_connection_sync(|conn| conn.execute_batch(LATE_FAILURE_TRIGGER))
        .expect("install injected trigger");

    // Act: the same input a fresh bootstrap would get.
    let error = bootstrap(&store, &input(&root, Some(password), now))
        .expect_err("the injected trigger must fail the transaction");

    // Assert: a typed database failure at the body stage, not a refusal, and
    // NOT any text from SQLite/the trigger.
    let BootstrapError::Store(failure) = &error else {
        panic!("expected a store failure, got {error:?}");
    };
    assert!(error.domain().is_none(), "not reported as a domain refusal");
    assert_eq!(
        failure.stage,
        FailureStage::Body,
        "the failure is attributed to the transaction body"
    );
    // The injected `RAISE(ABORT, …)` is a constraint failure whose extended code
    // is `SQLITE_CONSTRAINT_TRIGGER`; primary is its low byte (`SQLITE_CONSTRAINT`).
    assert_eq!(
        failure.kind,
        FailureKind::Sqlite {
            primary: rusqlite::ffi::SQLITE_CONSTRAINT,
            extended: rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER,
        },
        "classified with SQLite result codes: {failure:?}"
    );

    // Canary: the trigger's message text must appear nowhere a caller can print.
    for rendered in [
        format!("{error}"),
        format!("{error:?}"),
        format!("{failure}"),
        format!("{failure:?}"),
    ] {
        assert!(
            !rendered.contains(CANARY),
            "the injected message leaked into a rendered error: {rendered}"
        );
    }
    // The source chain too: walk it to the bottom.
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
    while let Some(current) = source {
        assert!(
            !current.to_string().contains(CANARY),
            "the injected message leaked into the source chain: {current}"
        );
        assert!(
            !format!("{current:?}").contains(CANARY),
            "the injected message leaked into a Debug of the chain: {current:?}"
        );
        source = current.source();
    }

    // Assert: every row kind the transaction wrote is gone. `library` and
    // `user`/`user_library` are their own kinds; `property` covers the marker
    // and both ciphertexts together.
    assert_eq!(count(&store, "library"), 0, "no library row survives");
    assert_eq!(count(&store, "property"), 0, "no marker and no ciphertext");
    assert_eq!(count(&store, "user"), 0, "no dev admin survives");
    assert_eq!(count(&store, "user_library"), 0, "no grant survives");

    // Act: remove the fault and run the identical input again.
    store
        .with_connection_sync(|conn| conn.execute_batch("DROP TRIGGER nd_cli_late_failure"))
        .expect("remove injected trigger");
    let retried = bootstrap(&store, &input(&root, Some(password), now)).expect("retry succeeds");

    // Assert: the retry is a real fresh setup, so the failure left no partial
    // state the retry could have reused.
    assert!(retried.library_created);
    assert!(retried.session_secret_created);
    assert!(retried.public_secret_created);
    assert!(retried.setup_marker_written);
    assert!(retried.dev_admin_created);
    assert_eq!(retried.grants.len(), 1);
    assert_eq!(count(&store, "library"), 1);
    assert_eq!(count(&store, "property"), 3);
    assert_eq!(count(&store, "user"), 1);
    assert_eq!(count(&store, "user_library"), 1);

    std::fs::remove_file(&db).ok();
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_boot_without_a_dev_password_sets_the_marker_and_creates_no_user_or_grant() {
    let label = "no-password";
    let root = music_root(label);
    let (store, db) = migrated_store(label);
    let now = DateTime::parse_from_rfc3339("2024-05-05T10:00:00Z")
        .expect("synthetic stamp")
        .with_timezone(&Utc);

    // Act: no dev password means marker only (`server/initial_setup.go:26-27`).
    let outcome = bootstrap(&store, &input(&root, None, now)).expect("no-password bootstrap");

    // Assert: the library, marker and both independent keys are still set up.
    assert!(outcome.library_created);
    assert_eq!(outcome.library.path, root.to_string_lossy());
    assert!(outcome.setup_marker_written);
    assert!(outcome.session_secret_created);
    assert!(outcome.public_secret_created);
    assert_ne!(
        outcome.session_secret.expose(),
        outcome.public_secret.expose(),
        "both keys are generated independently even with no user"
    );

    // Assert: no user and therefore no grant.
    assert!(!outcome.dev_admin_created);
    assert!(outcome.grants.is_empty());
    assert_eq!(count(&store, "user"), 0);
    assert_eq!(count(&store, "user_library"), 0);
    assert_eq!(count(&store, "library"), 1);
    assert_eq!(count(&store, "property"), 3, "marker plus two ciphertexts");
    assert_eq!(
        snapshot(&store).marker,
        nd_db::time::to_text(&now),
        "the marker stamp is the caller's"
    );

    std::fs::remove_file(&db).ok();
    std::fs::remove_dir_all(&root).ok();
}

/// Result of one concurrent bootstrap caller, handed back over a channel.
struct CallerResult {
    index: usize,
    outcome: Result<nd_cli::bootstrap::BootstrapOutcome, BootstrapError>,
}

/// Two independently opened stores (pool size 1 each) over the same migrated
/// file database, migrated ONCE through the first pool's initializer — the
/// shape of two Navidrome processes racing to initialize the same DB.
fn migrated_pair(label: &str) -> (SQLStore, SQLStore, PathBuf) {
    let path = std::env::temp_dir().join(format!("nd-cli-bootstrap-{label}-{}.db", nd_unique()));
    let dsn = |p: &Path| format!("file:{}?mode=rwc&_foreign_keys=on", p.display());
    let (first_pool, first) = Pool::new(&PoolConfig::with_size(dsn(&path), 1)).unwrap();
    apply_fresh_schema(&first).unwrap();
    first_pool
        .put_back(first)
        .expect("return configured handle");
    // The second pool opens the already-migrated file, so migration runs once.
    let (second_pool, second) = Pool::new(&PoolConfig::with_size(dsn(&path), 1)).unwrap();
    second_pool
        .put_back(second)
        .expect("return configured handle");
    (SQLStore::new(first_pool), SQLStore::new(second_pool), path)
}

/// Number of the two callers that report `flag` as true.
fn created_count(
    outcomes: &[&nd_cli::bootstrap::BootstrapOutcome],
    flag: fn(&nd_cli::bootstrap::BootstrapOutcome) -> bool,
) -> usize {
    outcomes.iter().filter(|outcome| flag(outcome)).count()
}

#[test]
fn two_concurrent_callers_converge_on_one_committed_key_pair() {
    let label = "race";
    let root = music_root(label);
    let (store_a, store_b, db) = migrated_pair(label);
    let now = DateTime::parse_from_rfc3339("2024-05-05T10:00:00Z")
        .expect("synthetic stamp")
        .with_timezone(&Utc);
    let password = "synthetic-dev-password".to_owned();

    // Gate: both callers are released by ONE barrier, so neither start is
    // ordered by the harness. Completion is bounded by the channel timeout, so
    // a wedged caller fails the test instead of hanging it. No sleep is used
    // anywhere for correctness.
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let (sender, receiver) = std::sync::mpsc::channel::<CallerResult>();

    let run = |index: usize, store: SQLStore| {
        let gate = std::sync::Arc::clone(&gate);
        let sender = sender.clone();
        let root = root.clone();
        let password = password.clone();
        std::thread::spawn(move || {
            gate.wait();
            let outcome = bootstrap(&store, &input(&root, Some(&password), now));
            let _ = sender.send(CallerResult { index, outcome });
        })
    };
    let handle_a = run(0, store_a.clone());
    let handle_b = run(1, store_b.clone());

    let mut outcomes: Vec<nd_cli::bootstrap::BootstrapOutcome> = Vec::new();
    for _ in 0..2 {
        let CallerResult { index, outcome } = receiver
            .recv_timeout(std::time::Duration::from_secs(120))
            .expect("both callers report within the bound");
        outcomes.push(outcome.unwrap_or_else(|error| panic!("caller {index} failed: {error:?}")));
    }
    handle_a.join().expect("caller 0 finished");
    handle_b.join().expect("caller 1 finished");
    let first = &outcomes[0];
    let second = &outcomes[1];

    // Assert: both published the SAME committed key pair.
    assert_eq!(
        first.session_secret.expose(),
        second.session_secret.expose(),
        "both callers return one committed session key"
    );
    assert_eq!(
        first.public_secret.expose(),
        second.public_secret.expose(),
        "both callers return one committed public key"
    );
    assert_ne!(
        first.session_secret.expose(),
        first.public_secret.expose(),
        "the two committed keys remain independent"
    );
    assert_eq!(first.library, second.library, "one library, one read-back");
    assert_eq!(first.grants, second.grants);

    // Assert: exactly one caller created each kind. This is the invariant the
    // race must preserve, whichever caller won.
    let both: Vec<&nd_cli::bootstrap::BootstrapOutcome> = vec![first, second];
    assert_eq!(
        created_count(&both, |outcome| outcome.library_created),
        1,
        "exactly one caller inserted the library row"
    );
    assert_eq!(
        created_count(&both, |outcome| outcome.session_secret_created),
        1,
        "exactly one caller inserted JWTSecret"
    );
    assert_eq!(
        created_count(&both, |outcome| outcome.public_secret_created),
        1,
        "exactly one caller inserted JWTPublicSecret"
    );
    assert_eq!(
        created_count(&both, |outcome| outcome.setup_marker_written),
        1,
        "exactly one caller wrote the setup marker"
    );
    assert_eq!(
        created_count(&both, |outcome| outcome.dev_admin_created),
        1,
        "exactly one caller created the dev admin"
    );

    // Assert: no duplicate rows.
    assert_eq!(count(&store_a, "library"), 1, "one library row");
    assert_eq!(
        count(&store_a, "property"),
        3,
        "marker plus two ciphertexts"
    );
    assert_eq!(count(&store_a, "user"), 1, "one dev admin");
    assert_eq!(count(&store_a, "user_library"), 1, "one grant row");

    // Assert: the persisted ciphertexts decrypt to each returned winner, so no
    // caller published a candidate it did not commit.
    let key =
        nd_cli::signing_keys::EncryptionKey::from_password_encryption_key("synthetic-test-key");
    let committed = snapshot(&store_a);
    let committed_session =
        nd_cli::signing_keys::decrypt_signing_secret(&key, &committed.session_ciphertext)
            .expect("committed session ciphertext is usable");
    let committed_public =
        nd_cli::signing_keys::decrypt_signing_secret(&key, &committed.public_ciphertext)
            .expect("committed public ciphertext is usable");
    for (index, outcome) in outcomes.iter().enumerate() {
        assert_eq!(
            committed_session.expose(),
            outcome.session_secret.expose(),
            "caller {index} returned the committed session key"
        );
        assert_eq!(
            committed_public.expose(),
            outcome.public_secret.expose(),
            "caller {index} returned the committed public key"
        );
    }

    // What this proves, stated precisely: the barrier makes both starts
    // simultaneous, and SQLite serializes the two write transactions — the
    // loser's busy failure is retried by the store and its re-run sees the
    // winner's committed rows. Which caller wins is deliberately NOT asserted,
    // and nothing here observes interleaved writes inside one transaction.
    std::fs::remove_file(&db).ok();
    std::fs::remove_dir_all(&root).ok();
}

/// Every row bootstrap is allowed to touch, read straight from the tables.
/// Used to prove a refusal changes **nothing** — library rows, signing
/// properties, marker, users and grants alike.
/// One `library` row as persisted: `(id, name, path, updated_at, created_at,
/// last_scan_at, last_scan_started_at, total_songs, pid_album,
/// scanned_pid_album, default_new_users)`. The stats/PID/timestamp columns are
/// included so any rewrite of existing values is visible.
type LibraryRow = (
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    String,
    String,
    bool,
);

/// One `user` row as persisted: `(id, user_name, password, is_admin,
/// token_epoch)`.
type UserRow = (String, String, String, bool, i64);

#[derive(Debug, PartialEq)]
struct FullState {
    /// `library` rows, ordered by id.
    libraries: Vec<LibraryRow>,
    /// `(id, value)`, ordered by id: marker plus both ciphertexts.
    properties: Vec<(String, String)>,
    /// `user` rows, ordered by id.
    users: Vec<UserRow>,
    grants: Vec<(String, i64)>,
}

fn text_of(value: &rusqlite::types::Value) -> String {
    match value {
        rusqlite::types::Value::Text(s) => s.clone(),
        other => format!("{other:?}"),
    }
}

fn int_of(value: &rusqlite::types::Value) -> i64 {
    match value {
        rusqlite::types::Value::Integer(i) => *i,
        other => panic!("expected an integer column, got {other:?}"),
    }
}

/// Read all four tables. Columns are fetched as untyped `Value`s so one helper
/// serves every shape.
fn full_state(store: &SQLStore) -> FullState {
    let read = |conn: &rusqlite::Connection,
                sql: &str,
                width: usize|
     -> rusqlite::Result<Vec<Vec<rusqlite::types::Value>>> {
        let mut statement = conn.prepare(sql)?;
        let rows = statement
            .query_map([], |row| {
                (0..width)
                    .map(|i| row.get::<_, rusqlite::types::Value>(i))
                    .collect::<Result<Vec<_>, _>>()
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    };
    let (libraries, properties, users, grants) = store
        .with_connection_sync(|conn| {
            Ok::<_, rusqlite::Error>((
                read(
                    conn,
                    "SELECT id, name, path, updated_at, created_at, last_scan_at, \
                     last_scan_started_at, total_songs, pid_album, scanned_pid_album, \
                     default_new_users FROM library ORDER BY id",
                    11,
                )?
                .into_iter()
                .map(|c| {
                    (
                        int_of(&c[0]),
                        text_of(&c[1]),
                        text_of(&c[2]),
                        text_of(&c[3]),
                        text_of(&c[4]),
                        text_of(&c[5]),
                        text_of(&c[6]),
                        int_of(&c[7]),
                        text_of(&c[8]),
                        text_of(&c[9]),
                        matches!(c[10], rusqlite::types::Value::Integer(1)),
                    )
                })
                .collect::<Vec<_>>(),
                read(conn, "SELECT id, value FROM property ORDER BY id", 2)?
                    .into_iter()
                    .map(|c| (text_of(&c[0]), text_of(&c[1])))
                    .collect::<Vec<_>>(),
                read(
                    conn,
                    "SELECT id, user_name, password, is_admin, token_epoch FROM user ORDER BY id",
                    5,
                )?
                .into_iter()
                .map(|c| {
                    (
                        text_of(&c[0]),
                        text_of(&c[1]),
                        text_of(&c[2]),
                        matches!(c[3], rusqlite::types::Value::Integer(1)),
                        int_of(&c[4]),
                    )
                })
                .collect::<Vec<_>>(),
                read(
                    conn,
                    "SELECT user_id, library_id FROM user_library ORDER BY user_id, library_id",
                    2,
                )?
                .into_iter()
                .map(|c| (text_of(&c[0]), int_of(&c[1])))
                .collect::<Vec<_>>(),
            ))
        })
        .expect("read full state");
    FullState {
        libraries,
        properties,
        users,
        grants,
    }
}

/// A database already booted with a synthetic root, plus its committed snapshot.
struct Booted {
    store: SQLStore,
    db: PathBuf,
    root: PathBuf,
    now: DateTime<Utc>,
    state: FullState,
}

fn booted(label: &str, password: Option<&str>) -> Booted {
    let root = music_root(label);
    let (store, db) = migrated_store(label);
    let now = DateTime::parse_from_rfc3339("2024-05-05T10:00:00Z")
        .expect("synthetic stamp")
        .with_timezone(&Utc);
    let outcome = bootstrap(&store, &input(&root, password, now)).expect("fixture bootstrap");
    let state = full_state(&store);
    assert_eq!(state.properties.len(), 3, "marker plus two ciphertexts");
    assert!(outcome.library_created);
    Booted {
        store,
        db,
        root,
        now,
        state,
    }
}

impl Booted {
    fn cleanup(&self) {
        std::fs::remove_file(&self.db).ok();
        std::fs::remove_dir_all(&self.root).ok();
    }

    /// Overwrite one property row: simulates corrupted committed state.
    fn write_property(&self, key: &str, value: &str) {
        self.store
            .with_connection_sync(|conn| {
                conn.execute(
                    "UPDATE property SET value = ? WHERE id = ?",
                    rusqlite::params![value.to_owned(), key.to_owned()],
                )
            })
            .expect("write synthetic property");
    }

    /// Insert an extra library row owned by another id.
    fn insert_library(&self, id: i64, name: &str, path: &str) {
        self.store
            .with_connection_sync(|conn| {
                conn.execute(
                    "INSERT INTO library(id, name, path, updated_at, created_at) VALUES(?, ?, ?, ?, ?)",
                    rusqlite::params![
                        id,
                        name.to_owned(),
                        path.to_owned(),
                        nd_db::time::to_text(&self.now),
                        nd_db::time::to_text(&self.now),
                    ],
                )
            })
            .expect("insert synthetic library");
    }

    /// Delete one row: simulates an inconsistent store.
    fn delete_row(&self, table: &str, column: &str, value: &str) {
        let sql = format!("DELETE FROM {table} WHERE {column} = ?");
        self.store
            .with_connection_sync(|conn| conn.execute(&sql, rusqlite::params![value.to_owned()]))
            .unwrap_or_else(|e| panic!("delete from {table}: {e}"));
    }

    /// Re-snapshot the committed state after the fixture itself has been
    /// mutated, so [`Booted::expect_refusal`] compares against current state.
    fn rebase(&mut self) {
        self.state = full_state(&self.store);
    }

    /// Run bootstrap against `root` and require a typed refusal whose committed
    /// state is identical afterwards.
    fn expect_refusal(&self, root: &Path, expected: &nd_cli::bootstrap::BootstrapDomain) {
        let error = bootstrap(&self.store, &input(root, None, self.now))
            .expect_err("the configured state must be refused");
        assert_eq!(
            error.domain(),
            Some(expected),
            "typed refusal, got: {error:?}"
        );
        assert_eq!(
            full_state(&self.store),
            self.state,
            "a refusal writes nothing: library, properties, marker, users and grants"
        );
    }
}

/// The configured key, rebuilt to decrypt committed ciphertexts in assertions.
fn configured_key() -> nd_cli::signing_keys::EncryptionKey {
    nd_cli::signing_keys::EncryptionKey::from_password_encryption_key("synthetic-test-key")
}

#[test]
fn an_independently_missing_property_is_created_while_the_survivor_is_preserved() {
    // Each key is missing-only-inserted independently: deleting one row re-runs
    // setup for that key alone and never touches the other.
    for missing in [
        nd_cli::signing_keys::JWT_SECRET_PROPERTY,
        nd_cli::signing_keys::JWT_PUBLIC_SECRET_PROPERTY,
    ] {
        let fixture = booted(
            &format!("missing-{missing}"),
            Some("synthetic-dev-password"),
        );
        fixture.delete_row("property", "id", missing);
        let before = full_state(&fixture.store);
        assert_eq!(before.properties.len(), 2, "{missing} is gone");

        let outcome = bootstrap(&fixture.store, &input(&fixture.root, None, fixture.now))
            .expect("the missing key is refilled");

        assert_eq!(
            outcome.session_secret_created,
            missing == nd_cli::signing_keys::JWT_SECRET_PROPERTY,
            "only the missing session key reports creation"
        );
        assert_eq!(
            outcome.public_secret_created,
            missing == nd_cli::signing_keys::JWT_PUBLIC_SECRET_PROPERTY,
            "only the missing public key reports creation"
        );
        assert!(!outcome.library_created, "the library row is verified only");
        assert!(!outcome.setup_marker_written, "the marker is already set");

        let after = full_state(&fixture.store);
        assert_eq!(after.properties.len(), 3, "the missing row is back");
        let stored = |key: &str| {
            after
                .properties
                .iter()
                .find(|(id, _)| id == key)
                .unwrap_or_else(|| panic!("{key} missing"))
                .1
                .clone()
        };
        // The survivor and the marker are byte-identical to before.
        for key in [
            nd_cli::signing_keys::JWT_SECRET_PROPERTY,
            nd_cli::signing_keys::JWT_PUBLIC_SECRET_PROPERTY,
            nd_auth::initial_setup::INITIAL_SETUP_FLAG_KEY,
        ] {
            if key == missing {
                continue;
            }
            let before_value = before
                .properties
                .iter()
                .find(|(id, _)| id == key)
                .map(|(_, value)| value.clone())
                .expect("survivor stored before");
            assert_eq!(stored(key), before_value, "{key} is byte-identical");
        }

        // Each published winner is the value now committed for its key.
        let key = configured_key();
        let session = nd_cli::signing_keys::decrypt_signing_secret(
            &key,
            &stored(nd_cli::signing_keys::JWT_SECRET_PROPERTY),
        )
        .expect("committed session ciphertext");
        let public = nd_cli::signing_keys::decrypt_signing_secret(
            &key,
            &stored(nd_cli::signing_keys::JWT_PUBLIC_SECRET_PROPERTY),
        )
        .expect("committed public ciphertext");
        assert_eq!(session.expose(), outcome.session_secret.expose());
        assert_eq!(public.expose(), outcome.public_secret.expose());
        assert_ne!(session.expose(), public.expose());

        assert_eq!(after.libraries, before.libraries);
        assert_eq!(after.users, before.users);
        assert_eq!(after.grants, before.grants);
        fixture.cleanup();
    }
}

#[test]
fn corrupted_stored_secrets_fail_closed_without_replacement_or_partial_writes() {
    let foreign_key =
        nd_cli::signing_keys::EncryptionKey::from_password_encryption_key("a-different-key");
    // Ciphertext from the real codec under a DIFFERENT key: decrypting it with
    // the configured key is an authentication failure, not a missing row.
    let foreign_ciphertext = nd_cli::signing_keys::encrypt_signing_secret(
        &foreign_key,
        nd_cli::signing_keys::generate_signing_secret()
            .expect("synthetic secret")
            .expose(),
    )
    .expect("synthetic ciphertext");

    // Two values that ARE valid ciphertext under the configured key but whose
    // plaintext could not have come from Go's generator: a short secret, and one
    // carrying whitespace. Both are encrypted with the real codec and the
    // configured key, so they reach the shape checks rather than the AEAD.
    let configured = configured_key();
    let short_ciphertext = nd_cli::signing_keys::encrypt_signing_secret(&configured, "x")
        .expect("synthetic short-plaintext ciphertext");
    // At least GO_ID_LEN characters, so the length floor passes and the
    // character check is what rejects it.
    let spaced_ciphertext =
        nd_cli::signing_keys::encrypt_signing_secret(&configured, "has whitespace inside it ok")
            .expect("synthetic spaced-plaintext ciphertext");

    // (label, stored value, expected static corruption class).
    let cases: [(&str, &str, &str); 6] = [
        ("empty", "", "empty"),
        (
            "not-base64",
            "not-base64-at-all",
            "not decodable ciphertext",
        ),
        (
            "truncated-base64",
            "AAAAAAAAAAAAAAAA",
            "not decodable ciphertext",
        ),
        (
            "wrong-key",
            foreign_ciphertext.as_str(),
            "authentication tag mismatch",
        ),
        (
            "short-plaintext",
            short_ciphertext.as_str(),
            "shorter than a Go-generated id",
        ),
        (
            "spaced-plaintext",
            spaced_ciphertext.as_str(),
            "invalid characters",
        ),
    ];

    for target in [
        nd_cli::signing_keys::JWT_SECRET_PROPERTY,
        nd_cli::signing_keys::JWT_PUBLIC_SECRET_PROPERTY,
    ] {
        for (label, stored, reason) in cases {
            let fixture = booted(&format!("corrupt-{target}-{label}"), Some("pw"));
            fixture.write_property(target, stored);
            let before = full_state(&fixture.store);

            let error = bootstrap(&fixture.store, &input(&fixture.root, None, fixture.now))
                .expect_err("corruption is fatal and is never rotated");
            assert_eq!(
                error.domain(),
                Some(&nd_cli::bootstrap::BootstrapDomain::SecretCorrupt {
                    key: target,
                    reason
                }),
                "corruption of {target} ({label}) names the key and class: {error:?}"
            );

            // Fail closed: the corrupted row is still there, byte-identical, and
            // nothing else was written or removed.
            let after = full_state(&fixture.store);
            assert_eq!(after, before, "no replacement and no partial writes");
            assert_eq!(
                after
                    .properties
                    .iter()
                    .find(|(id, _)| id == target)
                    .map(|(_, value)| value.clone())
                    .expect("the corrupted row survives"),
                stored,
                "a corrupted secret is never overwritten"
            );
            fixture.cleanup();
        }
    }
}

#[test]
fn a_changed_library_path_is_refused_without_repointing_or_rewriting_stats() {
    let fixture = booted("conflict-changed-path", Some("synthetic-dev-password"));
    // Non-default policy values, so a repointing/updating implementation shows up.
    fixture
        .store
        .with_connection_sync(|conn| {
            conn.execute(
                "UPDATE library SET total_songs = 42, pid_album = 'pid-a', \
                 scanned_pid_album = 'scanned-a', updated_at = '2020-01-02 03:04:05'",
                [],
            )?;
            conn.execute(
                "UPDATE user SET token_epoch = 17 WHERE user_name = 'admin'",
                [],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
        .expect("seed non-default values");
    let mut fixture = fixture;
    fixture.rebase();
    assert_eq!(fixture.state.libraries.len(), 1);
    assert_eq!(
        fixture.state.libraries[0].3, "2020-01-02 03:04:05",
        "seeded stamp"
    );

    // Act: restart against a DIFFERENT existing root.
    let other_root = music_root("conflict-changed-path-other");
    fixture.expect_refusal(
        &other_root,
        &nd_cli::bootstrap::BootstrapDomain::LibraryPathConflict,
    );

    let after = full_state(&fixture.store);
    assert_eq!(
        after.libraries[0].2,
        fixture.root.to_string_lossy(),
        "still the original root"
    );
    assert_eq!(
        after.libraries[0].3, "2020-01-02 03:04:05",
        "updated_at is not moved forward"
    );
    assert_eq!(
        after.libraries[0].9, "scanned-a",
        "scanned_pid_album preserved"
    );
    assert_eq!(after.users[0].4, 17, "token_epoch preserved");
    assert_eq!(after.libraries.len(), 1, "no second library row");

    std::fs::remove_dir_all(&other_root).ok();
    fixture.cleanup();
}

#[test]
fn a_duplicate_root_at_another_library_id_is_refused_without_replacing_either_row() {
    let fixture = booted("conflict-duplicate", Some("synthetic-dev-password"));
    let other_root = music_root("conflict-duplicate-other");
    fixture.insert_library(2, "Other Library", &other_root.to_string_lossy());
    let mut fixture = fixture;
    fixture.rebase();
    assert_eq!(fixture.state.libraries.len(), 2);

    // Act: configure the root library 2 already owns. Library 1 exists with a
    // different path too, so the duplicate guard must win: `INSERT OR REPLACE`
    // would cascade-delete library 2's albums, media files and grants.
    fixture.expect_refusal(
        &other_root,
        &nd_cli::bootstrap::BootstrapDomain::DuplicateLibraryPath,
    );

    let after = full_state(&fixture.store);
    assert_eq!(after.libraries.len(), 2, "both library rows survive");
    assert_eq!(after.libraries[1].0, 2, "library 2 is not replaced");

    std::fs::remove_dir_all(&other_root).ok();
    fixture.cleanup();
}

#[test]
fn an_initialized_store_without_library_one_is_refused_without_reseeding() {
    // Two shapes of "initialized but library-less": a setup marker, and a user
    // with no marker. Neither may be silently re-initialized.
    for shape in ["marker", "user"] {
        let fixture = booted(
            &format!("conflict-no-library-{shape}"),
            Some("synthetic-dev-password"),
        );
        // Deleting library 1 cascades its grant rows (FK ON DELETE CASCADE) —
        // the fixture's doing, asserted below, not bootstrap's.
        fixture.delete_row("library", "id", "1");
        if shape == "user" {
            fixture.delete_row(
                "property",
                "id",
                nd_auth::initial_setup::INITIAL_SETUP_FLAG_KEY,
            );
        }
        let before = full_state(&fixture.store);
        assert!(before.libraries.is_empty(), "library 1 is absent");
        assert_eq!(before.users.len(), 1, "the store is initialized");
        assert_eq!(before.grants, vec![], "the cascade removed the grant");
        assert_eq!(
            before.properties.len(),
            if shape == "marker" { 3 } else { 2 },
            "marker present or absent per shape"
        );

        let error = bootstrap(&fixture.store, &input(&fixture.root, None, fixture.now))
            .expect_err("an initialized store is not re-initialized silently");
        assert_eq!(
            error.domain(),
            Some(&nd_cli::bootstrap::BootstrapDomain::InitializedStoreWithoutLibrary),
            "typed refusal, got: {error:?}"
        );

        let after = full_state(&fixture.store);
        assert_eq!(
            after, before,
            "no library, marker, user or grant was written"
        );
        assert!(after.libraries.is_empty(), "library 1 is not reseeded");
        assert!(after.grants.is_empty(), "no default grant was created");
        fixture.cleanup();
    }
}

#[test]
fn an_existing_admin_with_a_missing_marker_is_preserved_and_not_reported_as_created() {
    let mut fixture = booted("existing-admin", Some("synthetic-dev-password"));
    let epoch = fixture.state.users[0].4;
    assert_eq!(epoch, 0, "a fresh dev admin starts at epoch 0");
    // Simulate a partially migrated store: the admin and its library grant
    // exist, the setup marker does not. A later hash must not replace anything.
    fixture.delete_row(
        "property",
        "id",
        nd_auth::initial_setup::INITIAL_SETUP_FLAG_KEY,
    );
    fixture.rebase();
    assert_eq!(fixture.state.properties.len(), 2);
    assert_eq!(fixture.state.grants.len(), 1);

    let outcome = bootstrap(
        &fixture.store,
        &input(&fixture.root, Some("other-password"), fixture.now),
    )
    .expect("marker-only repair");

    // The marker was written; the existing admin was NOT created and NOT touched.
    assert!(
        outcome.setup_marker_written,
        "the missing marker is written"
    );
    assert!(
        !outcome.dev_admin_created,
        "an existing admin is never reported as created"
    );
    assert!(!outcome.library_created, "the library row is verified only");
    assert!(
        !outcome.session_secret_created && !outcome.public_secret_created,
        "keys preserved"
    );
    assert_eq!(
        outcome.grants, fixture.state.grants,
        "committed grants unchanged"
    );

    let after = full_state(&fixture.store);
    assert_eq!(
        after.users, fixture.state.users,
        "hash, epoch and flags unchanged"
    );
    assert_eq!(after.users[0].4, epoch, "token_epoch is not bumped");
    assert_eq!(
        after.libraries, fixture.state.libraries,
        "library row unchanged"
    );
    assert_eq!(after.grants, fixture.state.grants, "grants unchanged");
    assert_eq!(
        after.properties.len(),
        3,
        "the marker row is the only addition"
    );
    // The stored hash is still the original one: a later password did not replace it.
    assert_eq!(
        after.users[0].2, fixture.state.users[0].2,
        "the existing password hash is preserved"
    );
    fixture.cleanup();
}

#[test]
fn a_marker_only_store_without_library_one_is_refused_without_creating_a_user() {
    // The marker alone marks a store as initialized: no user rows are needed to
    // trigger the refusal, and the refusal must not create a user either.
    let fixture = booted("marker-only-no-library", None);
    fixture.delete_row("library", "id", "1");
    let before = full_state(&fixture.store);
    assert!(before.libraries.is_empty());
    assert!(before.users.is_empty(), "a password-less boot has no user");
    assert_eq!(before.properties.len(), 3, "marker plus two ciphertexts");

    let error = bootstrap(
        &fixture.store,
        &input(&fixture.root, Some("pw"), fixture.now),
    )
    .expect_err("the marker alone is enough to refuse silent initialization");
    assert_eq!(
        error.domain(),
        Some(&nd_cli::bootstrap::BootstrapDomain::InitializedStoreWithoutLibrary),
        "typed refusal, got: {error:?}"
    );

    let after = full_state(&fixture.store);
    assert_eq!(
        after, before,
        "no library, user, grant or property was written"
    );
    fixture.cleanup();
}
