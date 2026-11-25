// nd-cli: owned by #33 (docs/rust-port/issues.md Part B)

//! BOOT-01 step 7: construction of the shared authentication state.
//!
//! [`AuthState::new`] turns a committed [`crate::startup::InitializedState`] plus
//! three checked numeric launch values into the single object every later auth
//! route shares. It is a **library entry point**, not a binary and not a
//! listener: it wires nothing, binds nothing, and owns no cache, transcode,
//! route, or scanner concern.
//!
//! What it guarantees:
//!
//! * **Committed winners only.** The [`nd_auth::Authenticator`] is built from the
//!   session and public secrets the bootstrap transaction actually committed and
//!   read back ([`crate::bootstrap::BootstrapOutcome::session_secret`] and
//!   [`crate::bootstrap::BootstrapOutcome::public_secret`]). It never decrypts or
//!   reads the database again, and it never generates a replacement key: a
//!   committed value is passed through as-is.
//! * **`previous = None`.** There is no rotation at boot (`boot-api.md` step 4:
//!   "No previous session secret fabricated at boot"). A token minted with any
//!   other secret is therefore not accepted by the constructed authenticator;
//!   explicit rotation is a later, separate decision.
//! * **One limiter, shared by every clone.** Exactly one [`nd_auth::GuardedLimiter`]
//!   is created and held in an [`Arc`], so cloning an [`AuthState`] — the shape
//!   every axum route handler takes — shares the failed-login budget rather than
//!   handing each handler a fresh window. The [`nd_db::store::SQLStore`] clone
//!   likewise shares the **existing** pool: `SQLStore` is an `Arc<Pool>` clone, so
//!   no second pool, connection, or database file is opened here.
//! * **Positive launch values re-checked.** The preflight already rejects
//!   non-positive session timeout, auth window, and request limit, but this is the
//!   boundary where the numbers reach `nd-auth`, so they are validated again
//!   against the exact contract `Authenticator::new` and `GuardedLimiter::new`
//!   require. A non-positive value is refused here rather than producing a
//!   non-expiring token or an allow-all limiter.
//!
//! Secrets stay inside. [`AuthState`] has a **manual** `Debug` that reports only
//! non-secret counts and timeouts: neither signing secret, the [`Config`], any
//! password, any token, nor the SQLStore (whose derived `Debug` prints the pool
//! DSN) is ever printed. [`AuthStateError`] carries static text only.
//!
//! [`Config`]: nd_conf::Config

use std::sync::Arc;
use std::time::Duration;

use nd_auth::credentials::{Authenticator, SigningSecrets};
use nd_auth::guarded_limiter::GuardedLimiter;
use nd_db::store::SQLStore;

use crate::startup::InitializedState;

/// Why auth-state construction refused, as static text.
///
/// The variants name only the offending field and a numeric value — never a
/// secret, a `Config`, a DSN, a path, a password, or an upstream error payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthStateError {
    /// `Server.SessionTimeout` must be a positive whole number of seconds. A
    /// non-positive timeout makes every expiring mint fail with
    /// `AuthError::InvalidSessionExpiry`, so construction refuses it here instead.
    #[error("session timeout must be positive (got {value} seconds)")]
    NonPositiveSessionTimeout {
        /// The rejected value. Numeric only.
        value: i64,
    },
    /// `Server.AuthWindowLength` must be a positive whole number of seconds for
    /// the rolling failed-login window.
    #[error("auth window length must be positive (got {value} seconds)")]
    NonPositiveAuthWindow {
        /// The rejected value. Numeric only.
        value: i64,
    },
    /// `Server.AuthRequestLimit` must be positive. Go's `newAuthLimiter` returns
    /// a nil (allow-all) limiter for a non-positive limit; the canonical trunk
    /// rejects that configuration (issues.md Part C #9), so this boundary refuses
    /// it before `GuardedLimiter` is reached.
    #[error("auth request limit must be positive (got {value})")]
    NonPositiveAuthRequestLimit {
        /// The rejected value. Numeric only.
        value: i64,
    },
    /// The limiter could not be built — a non-positive limit reached
    /// `GuardedLimiter`, or its entropy seed was unavailable (fail-closed).
    ///
    /// Deliberately reduced to static text rather than forwarding
    /// [`GuardedLimiterError`], so the boundary's own `Debug` carries nothing
    /// beyond a fixed class.
    ///
    /// [`GuardedLimiterError`]: nd_auth::guarded_limiter::GuardedLimiterError
    #[error("authentication limiter could not be constructed (fail-closed)")]
    LimiterUnavailable,
}

/// The shared authentication state that later auth routes consume.
///
/// `Clone` is the intended use: every handler gets a cheap clone that shares the
/// same pool and the same failed-login budget.
#[derive(Clone)]
pub struct AuthState {
    /// The open, migrated store. Cloning it clones an `Arc<Pool>`, so the pool is
    /// shared and no new connection or database is created.
    pub store: SQLStore,
    /// The token mints and verifies, bound to the committed secrets. No `Debug`:
    /// the raw signing keys must never be printed.
    pub auth: Authenticator,
    /// The single shared limiter. Every clone of this state shares this exact
    /// `Arc`, so the failed-login budget is global, not per-handler.
    pub limiter: Arc<GuardedLimiter>,
    /// Session timeout in seconds, retained as the checked value handed to
    /// [`Authenticator::new`]. Non-secret.
    pub session_timeout_secs: i64,
    /// Auth window length in seconds, retained as the checked value handed to the
    /// limiter. Non-secret.
    pub auth_window_secs: i64,
    /// Request limit, retained as the checked value handed to the limiter.
    /// Non-secret.
    pub auth_request_limit: i64,
}

impl AuthState {
    /// Construct the shared authentication state from committed startup output
    /// and the three checked launch values (BOOT-01 step 7).
    ///
    /// The secrets are the bootstrap transaction's committed winners, passed
    /// through as bytes exactly as Go's `jwtauth.New("HS256", []byte(secret), nil)`
    /// does (`core/auth/auth.go:45-46`); nothing is re-read or re-generated.
    /// `previous` is `None`: no secret is fabricated at boot.
    ///
    /// Consumes the [`InitializedState`], so the caller cannot accidentally keep
    /// and reuse a startup handle past this point.
    ///
    /// # Errors
    /// [`AuthStateError`] when a launch value is not positive, or the limiter
    /// cannot be built. Nothing is constructed and no state is returned on
    /// failure.
    pub fn new(
        state: InitializedState,
        session_timeout_secs: i64,
        auth_window_secs: i64,
        auth_request_limit: i64,
    ) -> Result<Self, AuthStateError> {
        // Re-check the exact positive contract the auth layer requires, before
        // the store clone or the limiter exists.
        if session_timeout_secs <= 0 {
            return Err(AuthStateError::NonPositiveSessionTimeout {
                value: session_timeout_secs,
            });
        }
        if auth_window_secs <= 0 {
            return Err(AuthStateError::NonPositiveAuthWindow {
                value: auth_window_secs,
            });
        }
        if auth_request_limit <= 0 {
            return Err(AuthStateError::NonPositiveAuthRequestLimit {
                value: auth_request_limit,
            });
        }

        // Committed winners, byte-for-byte, with previous = None.
        let secrets = SigningSecrets {
            current: state.bootstrap.session_secret.expose().as_bytes().to_vec(),
            previous: None,
            public_current: state.bootstrap.public_secret.expose().as_bytes().to_vec(),
        };
        let auth = Authenticator::new(secrets, session_timeout_secs);

        // Exactly one limiter. Positivity is proven above, so the i64 window fits
        // u64 (u64::MAX > i64::MAX) and the only remaining failure is the
        // fail-closed entropy error.
        let window =
            u64::try_from(auth_window_secs).map_err(|_| AuthStateError::NonPositiveAuthWindow {
                value: auth_window_secs,
            })?;
        let limiter = GuardedLimiter::new(auth_request_limit, Duration::from_secs(window))
            .map_err(|_| AuthStateError::LimiterUnavailable)?;

        Ok(AuthState {
            // SQLStore::clone clones the Arc<Pool>, sharing the existing pool.
            store: state.store.clone(),
            auth,
            limiter: Arc::new(limiter),
            session_timeout_secs,
            auth_window_secs,
            auth_request_limit,
        })
    }

    /// `true` when two states share the one limiter — the failed-login budget
    /// identity, asserted by the integration tests.
    #[must_use]
    pub fn shares_limiter(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.limiter, &other.limiter)
    }
}

/// Manual `Debug`: reports only non-secret launch facts and counts.
///
/// Withheld deliberately: the [`Authenticator`] holds the raw signing keys (and
/// has no `Debug` derive for exactly that reason), the [`SQLStore`] derived
/// `Debug` prints the pool DSN with its URI query, and no password, token, hash,
/// or `Config` value is stored here at all.
///
/// [`SQLStore`]: nd_db::store::SQLStore
impl std::fmt::Debug for AuthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthState")
            .field("session_timeout_secs", &self.session_timeout_secs)
            .field("auth_window_secs", &self.auth_window_secs)
            .field("auth_request_limit", &self.auth_request_limit)
            // Deliberately withheld: signing keys, DSN, passwords, tokens.
            .field("auth", &"<authenticator>")
            .field("store", &"<pooled store>")
            .field("limiter", &"<shared limiter>")
            .finish()
    }
}
