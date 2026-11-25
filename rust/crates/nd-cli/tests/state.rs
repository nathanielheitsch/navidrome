//! BOOT-01 step 7: shared auth-state construction tests.
//!
//! Every case runs the **real** chain — `preflight` → `startup::initialize` →
//! `AuthState::new` — against an isolated temp tree with an injected environment
//! and cwd. The committed secret winners are captured from the actual
//! `InitializedState` (as synthetic strings) *before* it is consumed by
//! `AuthState::new`, then re-derived from the same committed store so the
//! verification assertions are checked against the values the transaction
//! committed, not against anything the test invented.
//!
//! No new crypto dependency and no nd-auth hook: verification goes through the
//! existing public methods (`validate`, `validate_public`,
//! `create_native_session_token`, `create_public_token`, `GuardedLimiter::acquire`
//! / `AuthSlot::release`).
//!
//! Coverage:
//! * a native session token minted by the constructed authenticator verifies, and
//!   its `exp − iat` is exactly the configured checked timeout;
//! * a public-link token verifies under the public winner and is rejected by the
//!   session path, and a session token is rejected by the public path — the two
//!   winners are distinct keys;
//! * a token minted under an unrelated secret is rejected. This proves the
//!   committed authenticator accepts only its own current winner; it deliberately
//!   does **not** claim to introspect `previous`;
//! * state clones share the exact failure budget (`limit` failed
//!   acquire/release cycles through one clone denies the same key through
//!   another; an unrelated client is unaffected), with no sleeps;
//! * invalid launch values produce static, numeric, source-free errors carrying
//!   no canary;
//! * a no-dev-password startup constructs a ready state with zero users and the
//!   marker persisted.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use nd_auth::claims::{TokenSubject, AUDIENCE_NATIVE};
use nd_auth::credentials::{Authenticator, SigningSecrets};
use nd_auth::limiter::LimiterError;
use nd_cli::preflight::{preflight, PreflightOptions};
use nd_cli::startup::initialize;
use nd_cli::state::AuthState;
use nd_db::store::SQLStore;

/// The configured session timeout, in whole seconds, written into the config.
const SESSION_TIMEOUT_SECS: i64 = 3600;
/// The configured auth window, in whole seconds.
const AUTH_WINDOW_SECS: i64 = 20;
/// The configured per-client failed-login limit.
const AUTH_REQUEST_LIMIT: i64 = 5;

/// An isolated per-test tree.
struct Sandbox {
    inner: tempfile::TempDir,
    cwd: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let inner = tempfile::Builder::new()
            .prefix(&format!("nd-cli-auth-state-{tag}-"))
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
}

/// A boot-shaped config with the three launch values written explicitly, so the
/// assertions below observe configured numbers rather than loader defaults.
fn boot_toml(password: Option<&str>) -> String {
    let mut body = format!(
        "MusicFolder = \"music\"\nDataFolder = \"data\"\nCacheFolder = \"cache\"\nPort = 4611\n\
         SessionTimeout = \"{SESSION_TIMEOUT_SECS}s\"\nAuthWindowLength = \"{AUTH_WINDOW_SECS}s\"\n\
         AuthRequestLimit = {AUTH_REQUEST_LIMIT}\n"
    );
    if let Some(pw) = password {
        body.push_str(&format!("DevAutoCreateAdminPassword = \"{pw}\"\n"));
    }
    body
}

fn stamp(offset: u32) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&format!("2024-06-01T{:02}:00:00Z", 10 + offset))
        .expect("synthetic stamp")
        .with_timezone(&Utc)
}

/// Run the real preflight with an injected cwd, empty env and a non-root uid.
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

/// Full sandbox: music dir plus the config file. `password` is the synthetic dev
/// admin password, or `None` for the marker-only boot.
fn sandbox_with_config(tag: &str, password: Option<&str>) -> Sandbox {
    let sandbox = Sandbox::new(tag);
    sandbox.dir("music");
    sandbox.file("conf.toml", &boot_toml(password));
    sandbox
}

/// Run `preflight` + `initialize` on a fresh tree.
async fn startup(sandbox: &Sandbox, offset: u32) -> nd_cli::startup::InitializedState {
    let outcome = boot_preflight(sandbox);
    initialize(&outcome, stamp(offset))
        .await
        .expect("startup of a boot-shaped config")
}

/// Real current wall-clock seconds. `validate` enforces `exp` against the actual
/// clock with no leeway, so a mint timestamp must be genuinely current for an
/// expiry assertion to mean anything.
fn real_now() -> i64 {
    Utc::now().timestamp()
}

fn subject() -> TokenSubject {
    TokenSubject {
        id: "syntheticuserid0000000000".to_string(),
        token_epoch: 0,
    }
}

fn count_users(store: &SQLStore) -> i64 {
    store
        .with_connection_sync(|conn| conn.query_row("SELECT count(*) FROM user", [], |r| r.get(0)))
        .expect("count users")
}

fn property(store: &SQLStore, key: &str) -> Option<String> {
    store
        .with_connection_sync(|conn| {
            Ok::<Option<String>, rusqlite::Error>(
                conn.query_row(
                    &format!("SELECT value FROM property WHERE id = '{key}'"),
                    [],
                    |r| r.get::<_, String>(0),
                )
                .ok(),
            )
        })
        .expect("read property")
}

#[tokio::test]
async fn session_token_verifies_under_committed_winner_with_exact_checked_expiry() {
    let sandbox = sandbox_with_config("session", Some("synthetic-dev-password"));
    let state = startup(&sandbox, 1).await;
    // Capture the committed winner before the InitializedState is consumed, and
    // keep a handle to the same pool (SQLStore clone shares the Arc<Pool>) so the
    // provenance check can read the row back after construction.
    let session_winner = state.bootstrap.session_secret.expose().to_string();
    let store = state.store.clone();
    let auth_state = AuthState::new(
        state,
        SESSION_TIMEOUT_SECS,
        AUTH_WINDOW_SECS,
        AUTH_REQUEST_LIMIT,
    )
    .expect("constructed auth state");

    let now = real_now();
    let token = auth_state
        .auth
        .create_native_session_token(&subject(), "admin", true, now)
        .expect("mint a native session token");

    let claims = auth_state
        .auth
        .validate(&token)
        .expect("verifies under the winner");
    assert_eq!(claims.epoch, 0, "epoch claim carried from the subject");
    assert!(claims.aud.iter().any(|a| a == AUDIENCE_NATIVE));
    assert_eq!(
        claims.exp - claims.iat,
        SESSION_TIMEOUT_SECS,
        "expiry is exactly the configured checked timeout"
    );

    // Provenance: the value handed to the authenticator is exactly what the
    // committing transaction stored — the row's ciphertext, decrypted with the
    // documented default key. Nothing here invents a secret.
    let stored = property(&store, nd_cli::signing_keys::JWT_SECRET_PROPERTY)
        .expect("JWTSecret row committed");
    let key = nd_cli::signing_keys::EncryptionKey::from_password_encryption_key("");
    let decrypted = nd_cli::signing_keys::decrypt_signing_secret(&key, &stored)
        .expect("stored secret decrypts")
        .expose()
        .to_string();
    assert_eq!(
        decrypted, session_winner,
        "the authenticator is keyed by the committed winner, not a fresh value"
    );
    assert_eq!(auth_state.auth.session_timeout_secs(), SESSION_TIMEOUT_SECS);
    assert!(!session_winner.is_empty(), "the winner is nonempty");
}

#[tokio::test]
async fn public_and_session_winners_are_distinct_keys() {
    let sandbox = sandbox_with_config("public", Some("synthetic-dev-password"));
    let state = startup(&sandbox, 2).await;
    let session_winner = state.bootstrap.session_secret.expose().to_string();
    let public_winner = state.bootstrap.public_secret.expose().to_string();
    assert_ne!(session_winner, public_winner, "two independent winners");
    let auth_state = AuthState::new(
        state,
        SESSION_TIMEOUT_SECS,
        AUTH_WINDOW_SECS,
        AUTH_REQUEST_LIMIT,
    )
    .expect("constructed auth state");

    // A public-link token verifies under the public winner…
    let public_token = auth_state
        .auth
        .create_public_token(&nd_auth::claims::Claims::for_user(
            &subject(),
            "share",
            false,
            None,
            real_now(),
        ))
        .expect("mint a public token");
    auth_state
        .auth
        .validate_public(&public_token)
        .expect("public token verifies under the public winner");

    // …and is rejected by the session path, so the two keys are genuinely
    // different, not aliases of one secret.
    assert!(
        auth_state.auth.validate(&public_token).is_err(),
        "a public token must not verify under the session winner"
    );

    // Symmetrically, a session token is rejected by the public path.
    let session_token = auth_state
        .auth
        .create_native_session_token(&subject(), "admin", true, real_now())
        .expect("mint a session token");
    assert!(
        auth_state.auth.validate_public(&session_token).is_err(),
        "a session token must not verify under the public winner"
    );
}

#[tokio::test]
async fn a_token_from_an_unrelated_secret_is_rejected() {
    let sandbox = sandbox_with_config("unrelated", Some("synthetic-dev-password"));
    let state = startup(&sandbox, 3).await;
    let auth_state = AuthState::new(
        state,
        SESSION_TIMEOUT_SECS,
        AUTH_WINDOW_SECS,
        AUTH_REQUEST_LIMIT,
    )
    .expect("constructed auth state");

    // A different secret entirely — this is a token that could only have come
    // from some other service or an older, unrelated key.
    let other = Authenticator::new(
        SigningSecrets {
            current: b"unrelated-synthetic-secret".to_vec(),
            previous: None,
            public_current: b"unrelated-synthetic-public".to_vec(),
        },
        SESSION_TIMEOUT_SECS,
    );
    let foreign = other
        .create_native_session_token(&subject(), "admin", true, real_now())
        .expect("mint under the unrelated secret");

    assert!(
        auth_state.auth.validate(&foreign).is_err(),
        "the committed authenticator accepts only its own committed winner"
    );
    // Note: this asserts rejection of a foreign key. It deliberately does not
    // claim to observe the `previous` field, which has no public accessor.
}

#[tokio::test]
async fn clones_share_the_exact_failure_budget() {
    let sandbox = sandbox_with_config("budget", Some("synthetic-dev-password"));
    let state = startup(&sandbox, 4).await;
    let auth_state = AuthState::new(
        state,
        SESSION_TIMEOUT_SECS,
        AUTH_WINDOW_SECS,
        AUTH_REQUEST_LIMIT,
    )
    .expect("constructed auth state");

    // Clones share one Arc: identity proof at the source level (Arc::ptr_eq),
    // then the behavioral consequence through a second clone.
    let second = auth_state.clone();
    assert!(
        auth_state.shares_limiter(&second),
        "one Arc<GuardedLimiter>"
    );

    // Exactly `limit` failures through the first clone exhaust the key…
    for attempt in 0..AUTH_REQUEST_LIMIT {
        let slot = auth_state
            .limiter
            .acquire("client-a")
            .unwrap_or_else(|e| panic!("attempt {attempt} admitted: {e}"));
        slot.release(true);
    }
    // …and the denial is observed through the other clone, i.e. the budget is
    // the shared one, not a per-handler window. No sleep: the configured window
    // (20s) is far longer than this test.
    assert!(
        matches!(
            second.limiter.acquire("client-a"),
            Err(LimiterError::LimitReached)
        ),
        "the failure budget is shared across clones"
    );

    // An unrelated client is unaffected, and the state's stored limit is exact.
    assert!(
        second.limiter.acquire("client-b").is_ok(),
        "another client keeps its own budget"
    );
    assert_eq!(auth_state.auth_request_limit, AUTH_REQUEST_LIMIT);
    assert_eq!(auth_state.auth_window_secs, AUTH_WINDOW_SECS);
}

#[tokio::test]
async fn invalid_launch_values_are_static_numeric_errors_with_no_canary() {
    let canary_password = "canary-dev-password";
    let cases: [(i64, i64, i64, &str, i64); 3] = [
        (
            0,
            AUTH_WINDOW_SECS,
            AUTH_REQUEST_LIMIT,
            "session timeout",
            0,
        ),
        (
            SESSION_TIMEOUT_SECS,
            0,
            AUTH_REQUEST_LIMIT,
            "auth window",
            0,
        ),
        (
            SESSION_TIMEOUT_SECS,
            AUTH_WINDOW_SECS,
            0,
            "request limit",
            0,
        ),
    ];
    for (timeout, window, limit, expected, rejected_value) in cases {
        let sandbox = sandbox_with_config("invalid", Some(canary_password));
        let state = startup(&sandbox, 5).await;
        // Capture the committed winners so they can be used as canaries too.
        let session_winner = state.bootstrap.session_secret.expose().to_string();
        let public_winner = state.bootstrap.public_secret.expose().to_string();
        let err = AuthState::new(state, timeout, window, limit)
            .expect_err("a non-positive launch value is refused at this boundary");

        // Render Display, Debug and the whole source chain.
        let mut chain = format!("{err} | {err:?}");
        let mut source = std::error::Error::source(&err);
        let mut source_count = 0;
        while let Some(s) = source {
            chain.push_str(&format!(" | source {s} | source-debug {s:?}"));
            source_count += 1;
            source = s.source();
        }

        assert!(
            chain.contains(expected),
            "static class names the field: {chain}"
        );
        assert!(chain.contains("must be positive"), "static reason: {chain}");
        assert!(
            chain.contains(&rejected_value.to_string()),
            "the rejected numeric value is reported: {chain}"
        );
        assert_eq!(
            source_count, 0,
            "the error carries no source chain: {chain}"
        );
        for canary in [
            canary_password,
            session_winner.as_str(),
            public_winner.as_str(),
            sandbox.root().to_str().unwrap(),
            "navidrome.db",
            ".db?",
        ] {
            assert!(
                !chain.contains(canary),
                "no winner, password, path or DSN crosses this boundary: {chain}"
            );
        }
    }
}

#[tokio::test]
async fn no_password_startup_constructs_ready_state_with_marker_and_zero_users() {
    let sandbox = sandbox_with_config("no-password", None);
    let state = startup(&sandbox, 6).await;
    assert_eq!(count_users(&state.store), 0, "zero users");
    assert!(state.bootstrap.setup_marker_written, "marker written");
    drop(state);

    // Re-start against the committed database, then construct auth state.
    let again = startup(&sandbox, 7).await;
    let marker_value = property(&again.store, nd_auth::initial_setup::INITIAL_SETUP_FLAG_KEY);
    assert!(marker_value.is_some(), "marker row persisted");
    assert_eq!(count_users(&again.store), 0, "still zero users");

    let auth_state = AuthState::new(
        again,
        SESSION_TIMEOUT_SECS,
        AUTH_WINDOW_SECS,
        AUTH_REQUEST_LIMIT,
    )
    .expect("auth state constructs without any user");

    // A ready auth state: it mints and verifies without a user existing, which is
    // the precondition for the later createAdmin path.
    let token = auth_state
        .auth
        .create_native_session_token(&subject(), "placeholder", true, real_now())
        .expect("mint without a user");
    auth_state
        .auth
        .validate(&token)
        .expect("verify without a user");
    assert_eq!(
        count_users(&auth_state.store),
        0,
        "no user was created by construction"
    );
    assert_eq!(auth_state.auth.session_timeout_secs(), SESSION_TIMEOUT_SECS);
}

#[tokio::test]
async fn auth_state_debug_withholds_winners_store_and_password() {
    let password = "canary-dev-password";
    let sandbox = sandbox_with_config("debug", Some(password));
    let state = startup(&sandbox, 8).await;
    let session_winner = state.bootstrap.session_secret.expose().to_string();
    let public_winner = state.bootstrap.public_secret.expose().to_string();
    let auth_state = AuthState::new(
        state,
        SESSION_TIMEOUT_SECS,
        AUTH_WINDOW_SECS,
        AUTH_REQUEST_LIMIT,
    )
    .expect("constructed auth state");

    let shown = format!("{auth_state:?}");
    for secret in [password, session_winner.as_str(), public_winner.as_str()] {
        assert!(
            !shown.contains(secret),
            "secret never appears in Debug: {shown}"
        );
    }
    assert!(
        !shown.contains("navidrome.db"),
        "no database filename: {shown}"
    );
    assert!(!shown.contains(".db?"), "no DSN with a query: {shown}");
    assert!(
        !shown.contains(sandbox.root().to_str().unwrap()),
        "no path: {shown}"
    );
    assert!(shown.contains("<authenticator>"), "auth withheld: {shown}");
    assert!(shown.contains("<pooled store>"), "store withheld: {shown}");
    assert!(
        shown.contains("<shared limiter>"),
        "limiter withheld: {shown}"
    );
    assert!(
        shown.contains(&SESSION_TIMEOUT_SECS.to_string()),
        "the non-secret launch values are reported: {shown}"
    );
}
