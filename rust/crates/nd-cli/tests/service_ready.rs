//! BOOT-01 composition slice: `ServiceReady::prepare` tests.
//!
//! Every case runs the real chain — `preflight` → `ServiceReady::prepare` — on an
//! isolated temp tree with an injected environment and cwd. `prepare` performs
//! exactly one `initialize`, then composes the shared cache and auth state.
//!
//! Acceptance coverage:
//! * a fresh prepare commits one library, two signing ciphertexts, the marker and
//!   the dev admin; a **restart** preserves library, ciphertexts, marker, user
//!   count, the admin's `token_epoch`, and non-default library stats — nothing is
//!   rewritten;
//! * an auth token minted through the composed state verifies under the committed
//!   session winner, read back from the committing store;
//! * state clones share one cache, one auth failure budget, and one transcode
//!   budget (real permits, no subprocess, no sleep);
//! * an invalid stream budget/cap is refused **before** any write — no data, cache,
//!   or database artifact is created;
//! * a no-dev-password prepare yields a ready state with zero users and a persisted
//!   marker;
//! * failure diagnostics are static, stage-named, and canary-free (no path, DSN,
//!   password, or signing winner).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use nd_auth::claims::{TokenSubject, AUDIENCE_NATIVE};
use nd_cli::preflight::{preflight, PreflightOptions};
use nd_cli::service_ready::{ReadyStage, ServiceReady};
use nd_cli::signing_keys::{
    decrypt_signing_secret, EncryptionKey, JWT_PUBLIC_SECRET_PROPERTY, JWT_SECRET_PROPERTY,
};
use nd_db::execution::query_optional;
use nd_db::hydrate::row_to_library;
use nd_db::library_repo::{self, DEFAULT_LIBRARY_ID};
use nd_db::property_repo::get_sql;
use nd_db::store::SQLStore;

const SESSION_TIMEOUT_SECS: i64 = 3600;
const AUTH_WINDOW_SECS: i64 = 20;
const AUTH_REQUEST_LIMIT: i64 = 5;
const BUDGET_RAW: &str = "10kB";

/// An isolated per-test tree.
struct Sandbox {
    inner: tempfile::TempDir,
    cwd: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let inner = tempfile::Builder::new()
            .prefix(&format!("nd-cli-service-ready-{tag}-"))
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

    fn entries(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![self.root().to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read dir") {
                let path = entry.expect("entry").path();
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
}

/// A boot-shaped config exercising the auth numbers, typed budget and caps.
///
/// Library stats are set by the caller after a fresh startup to prove the restart
/// preserves non-default stats; the config itself only carries launch values.
fn boot_toml(password: Option<&str>) -> String {
    // All top-level keys must precede the `[Transcoding]` section header, or a
    // later bare key would be parsed as part of that section (UnknownKey).
    let mut body = format!(
        "MusicFolder = \"music\"\nDataFolder = \"data\"\nCacheFolder = \"cache\"\nPort = 4611\n\
         SessionTimeout = \"{SESSION_TIMEOUT_SECS}s\"\nAuthWindowLength = \"{AUTH_WINDOW_SECS}s\"\n\
         AuthRequestLimit = {AUTH_REQUEST_LIMIT}\nTranscodingCacheSize = \"{BUDGET_RAW}\"\n"
    );
    if let Some(pw) = password {
        body.push_str(&format!("DevAutoCreateAdminPassword = \"{pw}\"\n"));
    }
    body.push_str("[Transcoding]\nMaxConcurrent = 2\nMaxConcurrentPerUser = 1\n");
    body
}

fn stamp(offset: u32) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&format!("2024-06-01T{:02}:00:00Z", 10 + offset))
        .expect("synthetic stamp")
        .with_timezone(&Utc)
}

fn boot_preflight(sandbox: &Sandbox) -> nd_cli::preflight::PreflightOutcome {
    preflight(PreflightOptions {
        config_file: Some("conf.toml"),
        env: Some(Box::new(|_key| None)),
        startup_cwd: Some(sandbox.cwd()),
        effective_uid: Some(1000),
        allow_zero_port: false,
    })
    .expect("preflight of a boot-shaped config")
}

fn sandbox_with_config(tag: &str, password: Option<&str>) -> Sandbox {
    let sandbox = Sandbox::new(tag);
    sandbox.dir("music");
    sandbox.file("conf.toml", &boot_toml(password));
    sandbox
}

/// Committed facts read straight from the store, for the restart comparison.
#[derive(Debug, PartialEq)]
struct Snapshot {
    library: nd_db::library_repo::LibraryRecord,
    session_ciphertext: String,
    public_ciphertext: String,
    marker: Option<String>,
    grants: Vec<(String, i64)>,
    admin_passwords: Vec<String>,
    admin_epochs: Vec<i64>,
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
        .expect("library 1 committed");
    let read = |key: &str| {
        store
            .with_connection_sync(|conn| {
                query_optional(conn, &get_sql(key), |row| row.get::<_, String>(0))
            })
            .expect("read property")
    };
    let (grants, admin_passwords, admin_epochs, user_count) = store
        .with_connection_sync(|conn| {
            let mut stmt = conn.prepare(
                "SELECT user_id, library_id FROM user_library ORDER BY user_id, library_id",
            )?;
            let grants = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut admins =
                conn.prepare("SELECT password, token_epoch FROM user WHERE user_name = 'admin'")?;
            let rows = admins
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let user_count =
                conn.query_row("SELECT count(*) FROM user", [], |row| row.get::<_, i64>(0))?;
            Ok((
                grants,
                rows.iter().map(|r| r.0.clone()).collect::<Vec<_>>(),
                rows.iter().map(|r| r.1).collect::<Vec<_>>(),
                user_count,
            ))
        })
        .expect("read grants/users");
    Snapshot {
        library,
        session_ciphertext: read(JWT_SECRET_PROPERTY).expect("JWTSecret committed"),
        public_ciphertext: read(JWT_PUBLIC_SECRET_PROPERTY).expect("JWTPublicSecret committed"),
        marker: read(nd_auth::initial_setup::INITIAL_SETUP_FLAG_KEY),
        grants,
        admin_passwords,
        admin_epochs,
        user_count,
    }
}

/// Set non-default library stats so a restart rewrite would be observable.
fn set_nondefault_stats(store: &SQLStore) {
    store
        .with_connection_sync(|conn| {
            conn.execute(
                "UPDATE library SET total_songs = 42, total_albums = 7, total_artists = 3, \
                 total_folders = 11, total_files = 42, total_size = 123456 WHERE id = 1",
                [],
            )
        })
        .expect("set nondefault stats");
}

fn real_now() -> i64 {
    Utc::now().timestamp()
}

fn subject() -> TokenSubject {
    TokenSubject {
        id: "syntheticuserid0000000000".to_string(),
        token_epoch: 0,
    }
}

fn full_text<E: std::error::Error>(err: &E) -> String {
    let mut text = format!("{err} | debug {err:?}");
    let mut source = <E as std::error::Error>::source(err);
    while let Some(s) = source {
        text.push_str(&format!(" | source {s} | source-debug {s:?}"));
        source = s.source();
    }
    text
}
/// Reopen the committed database independently (raw helper, no live store) and
/// confirm the full committed setup: library 1, both signing ciphertexts, the
/// setup marker, the dev admin and its grant. Used to prove the DB commit
/// survived a later stream-cache failure.
fn verify_committed(db_path: &Path) -> Snapshot {
    let raw = nd_dbschema::schema::open(db_path.to_str().expect("utf-8 db path"))
        .expect("reopen the committed database");
    let library = raw
        .query_row("SELECT * FROM library WHERE id = 1", [], row_to_library)
        .expect("library 1 committed and readable after reopen");
    let read = |key: &str| -> Option<String> {
        raw.query_row(
            &format!("SELECT value FROM property WHERE id = '{key}'"),
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
    };
    let marker = read(nd_auth::initial_setup::INITIAL_SETUP_FLAG_KEY);
    let session_ciphertext = read(JWT_SECRET_PROPERTY).expect("JWTSecret committed");
    let public_ciphertext = read(JWT_PUBLIC_SECRET_PROPERTY).expect("JWTPublicSecret committed");
    let grants: Vec<(String, i64)> = raw
        .prepare("SELECT user_id, library_id FROM user_library ORDER BY user_id, library_id")
        .expect("grants stmt")
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .expect("grants")
        .collect::<Result<Vec<_>, _>>()
        .expect("grant rows");
    let (admin_passwords, admin_epochs, user_count) = {
        let mut stmt = raw
            .prepare("SELECT password, token_epoch FROM user WHERE user_name = 'admin'")
            .expect("admin stmt");
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .expect("admins")
            .collect::<Result<Vec<_>, _>>()
            .expect("admin rows");
        let user_count = raw
            .query_row("SELECT count(*) FROM user", [], |row| row.get::<_, i64>(0))
            .expect("count users");
        (
            rows.iter().map(|r| r.0.clone()).collect::<Vec<_>>(),
            rows.iter().map(|r| r.1).collect::<Vec<_>>(),
            user_count,
        )
    };
    Snapshot {
        library,
        session_ciphertext,
        public_ciphertext,
        marker,
        grants,
        admin_passwords,
        admin_epochs,
        user_count,
    }
}

#[tokio::test]
async fn fresh_prepare_commits_and_restart_preserves_every_committed_fact() {
    let sandbox = sandbox_with_config("fresh-restart", Some("synthetic-dev-password"));

    // Fresh prepare.
    let outcome = boot_preflight(&sandbox);
    let ready = ServiceReady::prepare(&outcome, stamp(1))
        .await
        .expect("fresh prepare");
    assert_eq!(
        ready.database.migration_count, 1,
        "exactly one fresh migration"
    );
    assert!(ready.database.migrated_on_open, "fresh database migrated");
    assert!(
        ready.stream.cache.root().is_dir(),
        "the transcoding cache root exists after prepare"
    );
    assert_eq!(ready.stream.cache_size_bytes, 10_000);
    assert_eq!(ready.stream.global_limit, 2);
    assert_eq!(ready.stream.user_limit, 1);

    // Give the library non-default stats so a restart rewrite would be visible.
    set_nondefault_stats(&ready.auth.store);
    let snapshot_a = snapshot(&ready.auth.store);
    assert_eq!(snapshot_a.user_count, 1, "one dev admin");
    assert_eq!(snapshot_a.grants.len(), 1);
    assert!(snapshot_a.marker.is_some(), "marker committed");
    assert_eq!(
        snapshot_a.library.total_songs, 42,
        "nondefault stats committed"
    );
    assert_eq!(snapshot_a.library.total_size, 123456);

    // Act: a restart prepare against the same config/cwd.
    let second = boot_preflight(&sandbox);
    let ready_b = ServiceReady::prepare(&second, stamp(2))
        .await
        .expect("restart prepare");
    assert!(
        !ready_b.database.migrated_on_open,
        "restart does not migrate again"
    );
    assert_eq!(
        ready_b.database.migration_count, 1,
        "still one migration row"
    );

    // Assert: nothing committed changed.
    let snapshot_b = snapshot(&ready_b.auth.store);
    assert_eq!(
        snapshot_a.session_ciphertext, snapshot_b.session_ciphertext,
        "session secret ciphertext byte-identical across restarts"
    );
    assert_eq!(
        snapshot_a.public_ciphertext, snapshot_b.public_ciphertext,
        "public secret ciphertext byte-identical across restarts"
    );
    assert_eq!(snapshot_a.marker, snapshot_b.marker, "marker unchanged");
    assert_eq!(snapshot_a.user_count, snapshot_b.user_count, "no new user");
    assert_eq!(
        snapshot_a.admin_passwords, snapshot_b.admin_passwords,
        "hash unchanged"
    );
    assert_eq!(
        snapshot_a.admin_epochs, snapshot_b.admin_epochs,
        "admin token_epoch unchanged"
    );
    assert_eq!(snapshot_a.grants, snapshot_b.grants, "grants unchanged");
    assert_eq!(
        snapshot_a.library, snapshot_b.library,
        "the whole library row (including nondefault stats and stamps) is unchanged"
    );
}

#[tokio::test]
async fn auth_token_verifies_under_committed_winner_after_prepare() {
    let sandbox = sandbox_with_config("token", Some("synthetic-dev-password"));
    let outcome = boot_preflight(&sandbox);
    let ready = ServiceReady::prepare(&outcome, stamp(3))
        .await
        .expect("prepare");

    // Mint through the composed authenticator and verify.
    let token = ready
        .auth
        .auth
        .create_native_session_token(&subject(), "admin", true, real_now())
        .expect("mint");
    let claims = ready
        .auth
        .auth
        .validate(&token)
        .expect("verifies under committed winner");
    assert!(claims.aud.iter().any(|a| a == AUDIENCE_NATIVE));
    assert_eq!(claims.exp - claims.iat, SESSION_TIMEOUT_SECS);

    // Provenance: the authenticator is keyed by the committed winner, not a fresh
    // value — decrypt the stored ciphertext with the documented default key.
    let stored = ready
        .auth
        .store
        .with_connection_sync(|conn| {
            Ok::<Option<String>, rusqlite::Error>(
                conn.query_row(
                    &format!("SELECT value FROM property WHERE id = '{JWT_SECRET_PROPERTY}'"),
                    [],
                    |r| r.get::<_, String>(0),
                )
                .ok(),
            )
        })
        .expect("read JWTSecret")
        .expect("row present");
    let key = EncryptionKey::from_password_encryption_key("");
    let winner = decrypt_signing_secret(&key, &stored)
        .expect("decrypts")
        .expose()
        .to_string();
    // The token was minted under the state; verify a token from a foreign key fails.
    let foreign = nd_auth::credentials::Authenticator::new(
        nd_auth::credentials::SigningSecrets {
            current: winner.as_bytes().to_vec(),
            previous: None,
            public_current: winner.as_bytes().to_vec(),
        },
        SESSION_TIMEOUT_SECS,
    );
    let foreign_token = foreign
        .create_native_session_token(&subject(), "admin", true, real_now())
        .expect("mint foreign");
    // Same secret verifies (sanity); a genuinely different one does not.
    assert!(ready.auth.auth.validate(&foreign_token).is_ok());
    let unrelated = nd_auth::credentials::Authenticator::new(
        nd_auth::credentials::SigningSecrets {
            current: b"unrelated-synthetic-secret".to_vec(),
            previous: None,
            public_current: b"unrelated-synthetic-public".to_vec(),
        },
        SESSION_TIMEOUT_SECS,
    );
    let unrelated_token = unrelated
        .create_native_session_token(&subject(), "admin", true, real_now())
        .expect("mint unrelated");
    assert!(
        ready.auth.auth.validate(&unrelated_token).is_err(),
        "only the committed winner is accepted"
    );
}

#[tokio::test]
async fn clones_share_one_cache_auth_budget_and_stream_budget() {
    let sandbox = sandbox_with_config("clones", Some("synthetic-dev-password"));
    let outcome = boot_preflight(&sandbox);
    let ready = ServiceReady::prepare(&outcome, stamp(4))
        .await
        .expect("prepare");

    // One cache: Arc identity + shared root.
    let clone = ready.clone();
    assert!(
        ready.stream.shares_cache(&clone.stream),
        "clones share the one Arc<TranscodeCache>"
    );
    assert_eq!(ready.stream.cache.root(), clone.stream.cache.root());

    // One auth failure budget: exhaust limit failures through one clone, deny
    // through the other; an unrelated client unaffected. No sleep.
    for attempt in 0..AUTH_REQUEST_LIMIT {
        let slot = ready
            .auth
            .limiter
            .acquire("client-a")
            .unwrap_or_else(|e| panic!("attempt {attempt} admitted: {e}"));
        slot.release(true);
    }
    assert!(
        matches!(
            clone.auth.limiter.acquire("client-a"),
            Err(nd_auth::limiter::LimiterError::LimitReached)
        ),
        "the auth failure budget is shared across clones"
    );
    assert!(
        clone.auth.limiter.acquire("client-b").is_ok(),
        "another client keeps its budget"
    );

    // One transcode budget: per-user cap of 1 shared; global cap of 2 shared.
    let first = ready.stream.limiter.acquire("alice").expect("slot");
    assert!(
        matches!(
            clone.stream.limiter.acquire("alice"),
            Err(nd_stream::limiter::TooManyTranscodes)
        ),
        "the transcode per-user budget is shared across clones"
    );
    let bob = clone.stream.limiter.acquire("bob").expect("bob slot");
    assert!(
        matches!(
            clone.stream.limiter.acquire("carol"),
            Err(nd_stream::limiter::TooManyTranscodes)
        ),
        "the shared global transcode cap is reached at 2"
    );
    drop(first);
    drop(bob);
}

#[tokio::test]
async fn invalid_stream_config_precheck_creates_no_data_cache_or_db() {
    // An unparseable budget spelled in the config would be rejected by the loader,
    // so we drive the boundary through a mutated PreflightOutcome: the fields are
    // public and externally mutable, which is exactly why prepare re-validates.
    let sandbox = sandbox_with_config("invalid-stream", Some("pw"));
    let mut outcome = boot_preflight(&sandbox);
    // from_raw silently zeroes an unparseable spelling (the bypass prepare closes).
    outcome.config.transcoding_cache_size =
        nd_conf::bytes::ByteSizeField::from_raw("canary-typed-budget");

    let err = ServiceReady::prepare(&outcome, stamp(5))
        .await
        .expect_err("invalid budget must be refused before initialize");
    assert_eq!(err.stage(), ReadyStage::Validation);
    // No side effect at all: no data dir, no cache dir, no database file.
    assert!(!sandbox.root().join("data").exists(), "no data directory");
    assert!(!sandbox.root().join("cache").exists(), "no cache directory");
    assert!(
        sandbox
            .entries()
            .iter()
            .all(|p| p == "conf.toml" || p == "music"),
        "only the config and music dir exist: {:?}",
        sandbox.entries()
    );
    let text = full_text(&err);
    for canary in [
        "canary-typed-budget",
        sandbox.root().to_str().unwrap(),
        "navidrome.db",
        ".db?",
    ] {
        assert!(!text.contains(canary), "diagnostic is canary-free: {text}");
    }
}

#[tokio::test]
async fn invalid_caps_precheck_creates_no_data_cache_or_db() {
    let sandbox = sandbox_with_config("invalid-cap", Some("pw"));
    let mut outcome = boot_preflight(&sandbox);
    outcome.config.transcoding.max_concurrent = -1;
    let err = ServiceReady::prepare(&outcome, stamp(6))
        .await
        .expect_err("a negative cap must be refused before initialize");
    assert_eq!(err.stage(), ReadyStage::Validation);
    assert!(!sandbox.root().join("data").exists(), "no data directory");
    assert!(!sandbox.root().join("cache").exists(), "no cache directory");
}

#[tokio::test]
async fn non_positive_auth_number_precheck_creates_no_data_cache_or_db() {
    // These numbers reach `AuthState::new`, and `PreflightOutcome`'s fields are
    // public, so prepare re-checks them before initialize writes anything.
    for (timeout, window, limit, why) in [
        (0, AUTH_WINDOW_SECS, AUTH_REQUEST_LIMIT, "session timeout"),
        (SESSION_TIMEOUT_SECS, 0, AUTH_REQUEST_LIMIT, "auth window"),
        (
            SESSION_TIMEOUT_SECS,
            AUTH_WINDOW_SECS,
            0,
            "auth request limit",
        ),
    ] {
        let sandbox = sandbox_with_config("bad-auth", Some("pw"));
        let mut outcome = boot_preflight(&sandbox);
        outcome.session_timeout_secs = timeout;
        outcome.auth_window_secs = window;
        // config.auth_request_limit is validated by preflight, so mutate it too.
        outcome.config.auth_request_limit = limit;

        let err = ServiceReady::prepare(&outcome, stamp(9))
            .await
            .expect_err(&format!("{why} must be refused before initialize"));
        assert_eq!(err.stage(), ReadyStage::Validation, "{why}: {err}");
        let text = full_text(&err);
        assert!(
            text.contains(why),
            "{why}: static class names the field: {text}"
        );
        assert!(
            !sandbox.root().join("data").exists(),
            "{why}: no data directory"
        );
        assert!(
            !sandbox.root().join("cache").exists(),
            "{why}: no cache directory"
        );
        assert!(
            sandbox
                .entries()
                .iter()
                .all(|p| p == "conf.toml" || p == "music"),
            "{why}: nothing was written: {:?}",
            sandbox.entries()
        );
        for canary in [sandbox.root().to_str().unwrap(), "navidrome.db", ".db?"] {
            assert!(!text.contains(canary), "{why}: canary-free: {text}");
        }
    }
}

#[tokio::test]
async fn no_dev_password_prepare_is_ready_with_zero_users_and_marker() {
    let sandbox = sandbox_with_config("no-password", None);
    let outcome = boot_preflight(&sandbox);
    let ready = ServiceReady::prepare(&outcome, stamp(7))
        .await
        .expect("prepare without a password");

    let snap = snapshot(&ready.auth.store);
    assert_eq!(snap.user_count, 0, "zero users");
    assert!(snap.marker.is_some(), "marker persisted");
    assert!(snap.admin_passwords.is_empty(), "no admin row");
    // Still ready: the composed state mints and verifies with no user present.
    let token = ready
        .auth
        .auth
        .create_native_session_token(&subject(), "placeholder", true, real_now())
        .expect("mint");
    ready.auth.auth.validate(&token).expect("verify");
    assert_eq!(
        ready
            .auth
            .store
            .with_connection_sync(
                |conn| conn.query_row("SELECT count(*) FROM user", [], |r| r.get::<_, i64>(0))
            )
            .expect("count"),
        0,
        "no user created by construction"
    );
}

#[tokio::test]
async fn service_ready_debug_has_no_secret_or_dsn() {
    let password = "canary-dev-password";
    let sandbox = sandbox_with_config("debug", Some(password));
    let outcome = boot_preflight(&sandbox);
    let ready = ServiceReady::prepare(&outcome, stamp(8))
        .await
        .expect("prepare");

    let shown = format!("{ready:?}");
    // The store's DSN and any signing winner must not appear.
    assert!(!shown.contains(password), "no dev password: {shown}");
    assert!(!shown.contains(".db?"), "no DSN with query: {shown}");
    // The withheld placeholders prove the store/cache/keys are redacted, not omitted.
    assert!(
        shown.contains("<pooled store>"),
        "auth store withheld by AuthState Debug: {shown}"
    );
    assert!(
        shown.contains("<authenticator>"),
        "authenticator withheld: {shown}"
    );
    assert!(
        shown.contains("<transcode cache>"),
        "cache path withheld: {shown}"
    );
    // Directory facts are printed by design (operational, non-secret); confirm the
    // withheld components are actually withheld, not silently omitted.
    assert!(shown.contains("ServiceReady"));
}

#[tokio::test]
async fn postcommit_cache_failure_reports_stream_stage_and_recovers_idempotently() {
    // Deterministic, uid-independent post-commit failure: startup creates the
    // cache parent and commits setup, but `cache/transcoding` is a regular file,
    // so StreamState::new's create_dir_all fails regardless of privilege.
    let password = "canary-dev-password";
    let sandbox = sandbox_with_config("postcommit-fail", Some(password));
    let outcome = boot_preflight(&sandbox);

    // Force the failure target before prepare: cache parent exists, the transcoding
    // root is a regular file (not a directory).
    let cache_parent = sandbox.cwd().join("cache");
    std::fs::create_dir_all(&cache_parent).expect("cache parent");
    let fixture = cache_parent.join(nd_stream::transcode_cache::TRANSCODING_CACHE_DIR);
    std::fs::write(&fixture, b"not-a-directory").expect("regular-file fixture");

    let err = ServiceReady::prepare(&outcome, stamp(1))
        .await
        .expect_err("cache root cannot be created over a regular file");

    // Assert: Stream stage, no ready value.
    assert_eq!(err.stage(), ReadyStage::Stream, "{err}");
    let text = full_text(&err);
    // The diagnostic is a static class plus a numeric errno only. The word
    // "transcoding" appears inside the fixed error-class text (not as a path);
    // the real disclosure surface — path, password, DSN, db filename — is absent.
    for canary in [
        password,
        sandbox.root().to_str().unwrap(),
        "navidrome.db",
        ".db?",
    ] {
        assert!(
            !text.contains(canary),
            "diagnostic/source chain is safe: {text}"
        );
    }

    // Independently reopen the DB (raw helper) and confirm the commit survived:
    // library 1, both keys, marker, admin, grant all exist.
    let db_path = sandbox.root().join("data").join("navidrome.db");
    let committed = verify_committed(&db_path);
    assert_eq!(committed.library.id, 1, "library 1 committed");
    assert_eq!(committed.user_count, 1, "dev admin committed");
    assert_eq!(committed.grants.len(), 1, "admin grant committed");
    assert!(committed.marker.is_some(), "setup marker committed");
    assert!(
        !committed.session_ciphertext.is_empty(),
        "session key committed"
    );
    assert!(
        !committed.public_ciphertext.is_empty(),
        "public key committed"
    );

    // Remove ONLY the fixture file, then retry the same preflight.
    std::fs::remove_file(&fixture).expect("remove fixture");
    let retry_outcome = boot_preflight(&sandbox);
    let ready = ServiceReady::prepare(&retry_outcome, stamp(2))
        .await
        .expect("retry succeeds once the fixture is gone");
    assert!(ready.stream.cache.root().is_dir(), "cache root now created");

    // Idempotent recovery: ciphertexts, admin hash, epoch and library unchanged by
    // the retry (which must NOT re-commit or rotate anything).
    let after = verify_committed(&db_path);
    assert_eq!(
        committed.session_ciphertext, after.session_ciphertext,
        "session ciphertext unchanged by retry"
    );
    assert_eq!(
        committed.public_ciphertext, after.public_ciphertext,
        "public ciphertext unchanged by retry"
    );
    assert_eq!(
        committed.admin_passwords, after.admin_passwords,
        "hash unchanged"
    );
    assert_eq!(
        committed.admin_epochs, after.admin_epochs,
        "epoch unchanged"
    );
    assert_eq!(committed.library, after.library, "library row unchanged");
    assert_eq!(committed.marker, after.marker, "marker unchanged");
    assert_eq!(committed.grants, after.grants, "grants unchanged");
    assert_eq!(after.user_count, 1, "no second user created by retry");
}
