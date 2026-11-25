//! API-01a: the non-HTTP native API-key principal and current library access.
//!
//! This module resolves a presented API key into an immutable, server-owned
//! principal by REUSING the existing production paths, and re-reads the owner's
//! library grants from the DB on every operation:
//!
//! * key format is [`nd_db::player_repo::validate_api_key`] (`^nds_[0-9A-Za-z]{22}$`),
//! * the key lookup is [`nd_db::player_repo::PlayerRepo::find_by_api_key`] (the
//!   indexed SHA-256 hash column; the plaintext never reaches the statement),
//! * the owner and its current grants come from [`nd_db::user_repo::UserRepo::get`]
//!   (which hydrates `libraries_json`),
//! * library-scoped reads stay in [`nd_db::scanner_queries`] (its opaque
//!   `UserScope` and `get_visible_media`), so this crate renders no scope SQL of
//!   its own.
//!
//! Deliberate bounds (docs/rust-port/subplans/boot-api.md §API-01):
//! * No HTTP. No listener/router and no status codes: failures stay typed
//!   (denial vs `Operational`) so the later transport layer maps them.
//! * No credential material in `Debug`/`Display`. The plaintext key is a
//!   redacted newtype; the principal carries no key, password or hash.
//! * No token minting, no key generation, no new auth/hash code, and no cached
//!   grants — every operation reloads the current user.
//!
//! The DB reads run through [`nd_db::store::SQLStore::with_connection`], which
//! moves checkout and all SQLite work off the async executor, so an async caller
//! never blocks a worker thread.

use std::fmt;

use nd_db::player_repo::{validate_api_key, PlayerError, PlayerRepo};
use nd_db::store::SQLStore;
use nd_db::user_repo::{UserError, UserRepo};

/// The exact transport prefix of a native API key (`^nds_…`, `player_repo.rs`).
pub const API_KEY_PREFIX: &str = nd_db::player_repo::API_KEY_PREFIX;

/// A presented-but-not-yet-resolved API key, redacted in every diagnostic.
///
/// The plaintext lives only here and is consumed by
/// [`resolve_api_principal`]; it is never printed, logged, or copied into the
/// resolved principal.
pub struct ApiKeyCredential(String);

impl ApiKeyCredential {
    /// Wrap a presented key WITHOUT validating its format. Validation is
    /// [`Self::validate`], which keeps the existing format rule as the source of
    /// truth.
    #[must_use]
    pub fn new(raw: &str) -> Self {
        Self(raw.to_string())
    }

    /// Whether the key matches the canonical `^nds_[0-9A-Za-z]{22}$` shape, via
    /// the existing [`validate_api_key`]. A failure is a credential problem,
    /// never an operational one, and never echoes the key.
    ///
    /// # Errors
    /// [`nd_db::player_repo::PlayerError::ApiKeyValidation`] when the shape is
    /// wrong (bad prefix, wrong length, or a non-alphanumeric body character).
    pub fn validate(&self) -> Result<(), PlayerError> {
        validate_api_key(&self.0)
    }

    /// The key's shape, for tests only — it reports validity, never the bytes.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        nd_db::player_repo::api_key_format_matches(&self.0)
    }
}

/// `Debug` and `Display` redact the key: a credential must never reach a log or
/// an error string (boot-api.md: "no password/hash in DTO/log").
impl fmt::Debug for ApiKeyCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKeyCredential(<redacted>)")
    }
}

impl fmt::Display for ApiKeyCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted api key>")
    }
}

/// Which kind of credential authenticated the request. Only the API-key kind is
/// produced here; the JWT/session kind belongs to the transport layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    /// A native API key resolved through the player repository.
    ApiKey,
}

/// An immutable, server-owned principal produced from a valid API key.
///
/// It carries the owner's identity and the authenticated player id, plus the
/// admin flag as read from the owner's current row. It deliberately carries NO
/// library grant list: grants are re-read from the DB per operation by
/// [`current_library_access`], so a stale principal cannot widen access after a
/// grant is revoked between requests.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiPrincipal {
    /// The key's owner user id (`player.user_id`).
    owner_id: String,
    /// The owner's user name, read from the owner's row — NOT the player's
    /// joined column, so a stale join cannot drive identity.
    owner_name: String,
    /// The authenticated player id (`player.id`).
    player_id: String,
    /// The owner's current admin flag.
    is_admin: bool,
    /// The credential kind that produced this principal.
    kind: CredentialKind,
}

impl ApiPrincipal {
    /// The key's owner user id.
    #[must_use]
    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    /// The owner's user name.
    #[must_use]
    pub fn owner_name(&self) -> &str {
        &self.owner_name
    }

    /// The authenticated player id.
    #[must_use]
    pub fn player_id(&self) -> &str {
        &self.player_id
    }

    /// Whether the owner is an admin, as read during resolution. This is the
    /// admin flag only; it is not a grant list.
    #[must_use]
    pub fn is_admin(&self) -> bool {
        self.is_admin
    }

    /// The credential kind.
    #[must_use]
    pub fn kind(&self) -> CredentialKind {
        self.kind
    }
}

/// Manual `Debug`: it names identity fields and never prints a key, hash, or
/// password. (`PlayerRecord` derives `Debug` and carries `api_key`, so the
/// principal is built from selected fields, never from the record itself.)
impl fmt::Debug for ApiPrincipal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiPrincipal")
            .field("owner_id", &self.owner_id)
            .field("owner_name", &self.owner_name)
            .field("player_id", &self.player_id)
            .field("is_admin", &self.is_admin)
            .field("kind", &self.kind)
            .finish()
    }
}

/// Why a principal could not be produced, keeping the classes the transport
/// layer must map apart (boot-api.md: malformed/unknown/revoked key → 401;
/// store/decode failure → sanitized 500). No HTTP status is produced here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrincipalError {
    /// The presented key is not in canonical form (`^nds_[0-9A-Za-z]{22}$`).
    /// A credential problem: it never touches the database.
    MalformedKey,
    /// No player row matches the key's hash, OR the key was revoked (its hash
    /// column set to NULL). Go's `FindByAPIKey` cannot tell these apart, so they
    /// share one denial — boot-api.md requires the same 401 for both.
    CredentialNotFound,
    /// The player row exists but its owner user row does not. A key cannot
    /// survive owner deletion.
    OwnerNotFound,
    /// A database read failed. Operational, NOT a denial: the transport layer
    /// must map it to a sanitized server error, not to 401 or 404.
    Operational,
}

impl PrincipalError {
    /// Whether this failure denies the credential (as opposed to an operational
    /// database failure). Callers use this — never a status code here — to keep
    /// "not authenticated" distinct from "server error".
    #[must_use]
    pub fn is_denial(&self) -> bool {
        matches!(
            self,
            Self::MalformedKey | Self::CredentialNotFound | Self::OwnerNotFound
        )
    }
}

/// Public, credential-free description of the failure. It never carries the key,
/// a hash, SQL, or a path.
impl fmt::Display for PrincipalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedKey => f.write_str("api key is not in the required format"),
            Self::CredentialNotFound => f.write_str("api key is unknown or revoked"),
            Self::OwnerNotFound => f.write_str("api key owner no longer exists"),
            Self::Operational => f.write_str("database lookup failed"),
        }
    }
}

/// The typed outcome of the single database step, so a store callback returns
/// `Ok` for every credential-level result and reserves `Err` for a genuine
/// checkout/store failure. Operational failures raised BY the repositories fold
/// onto [`LookupOutcome::Operational`] rather than constructing a SQLite error —
/// nd-api declares no `rusqlite` dependency and builds no database error.
enum LookupOutcome {
    /// A principal was resolved.
    Principal(ApiPrincipal),
    /// The current library access was read.
    Access(LibraryAccess),
    /// Unknown or revoked key.
    CredentialNotFound,
    /// Owner row is gone.
    OwnerNotFound,
    /// A repository read failed for a non-denial reason.
    Operational,
}

/// Resolve a presented API key into an [`ApiPrincipal`] against a real store.
///
/// Order (boot-api.md: credential parsing → single read-only lookup → owner
/// reload): validate the format WITHOUT touching the DB, then inside one leased
/// connection off the executor run [`PlayerRepo::find_by_api_key`] and
/// [`UserRepo::get`] on the owner. The admin flag is read from the owner's row,
/// so a claimed username never selects a different user and a key cannot survive
/// owner deletion.
///
/// Runs entirely off the async executor via [`SQLStore::with_connection`].
///
/// # Errors
/// * [`PrincipalError::MalformedKey`] for a bad shape (no DB read attempted).
/// * [`PrincipalError::CredentialNotFound`] for an unknown or revoked key.
/// * [`PrincipalError::OwnerNotFound`] when the owner row is gone.
/// * [`PrincipalError::Operational`] when a checkout or SQLite read fails — never
///   a denial.
pub async fn resolve_api_principal(
    store: &SQLStore,
    credential: &ApiKeyCredential,
) -> Result<ApiPrincipal, PrincipalError> {
    if credential.validate().is_err() {
        return Err(PrincipalError::MalformedKey);
    }

    // Copy the key bytes into the closure; the redacted newtype is not `Clone`,
    // and only this owned copy ever holds the plaintext.
    let key = credential.0.clone();
    let outcome = store
        .with_connection(move |conn| {
            Ok(match PlayerRepo::new(conn).find_by_api_key(&key) {
                Ok(player) => match UserRepo::new(conn).get(&player.user_id) {
                    Ok(owner) => LookupOutcome::Principal(ApiPrincipal {
                        owner_id: owner.id.clone(),
                        owner_name: owner.user_name.clone(),
                        player_id: player.id.clone(),
                        is_admin: owner.is_admin,
                        kind: CredentialKind::ApiKey,
                    }),
                    Err(UserError::NotFound) => LookupOutcome::OwnerNotFound,
                    Err(UserError::Sqlite(_)) => LookupOutcome::Operational,
                },
                // A read path yields NotFound or Sqlite only; both fold onto
                // denial-vs-operational without constructing a SQLite error.
                Err(PlayerError::NotFound) => LookupOutcome::CredentialNotFound,
                Err(_) => LookupOutcome::Operational,
            })
        })
        .await
        .map_err(|_| PrincipalError::Operational)?;

    match outcome {
        LookupOutcome::Principal(principal) => Ok(principal),
        LookupOutcome::CredentialNotFound => Err(PrincipalError::CredentialNotFound),
        LookupOutcome::OwnerNotFound => Err(PrincipalError::OwnerNotFound),
        LookupOutcome::Access(_) | LookupOutcome::Operational => Err(PrincipalError::Operational),
    }
}

/// The owner's CURRENT library access, read fresh from the DB.
///
/// This is the per-operation grant read the subplan requires (boot-api.md:
/// "User library grants are consulted per operation/current DB, not cached
/// indefinitely in token/principal"). It reuses [`UserRepo::get`], which hydrates
/// the current `libraries_json`; an admin's effective access is unrestricted
/// regardless of the grant list, and an owner with zero grants is represented as
/// an EMPTY grant set — never "all".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryAccess {
    /// The principal's owner id.
    pub owner_id: String,
    /// Whether the owner is an admin (as read now).
    pub is_admin: bool,
    /// The owner's granted library ids as read now. Empty means no grants, which
    /// the DB scope layer turns into a deny, never into unrestricted access.
    pub granted_library_ids: Vec<i64>,
}

impl LibraryAccess {
    /// Whether this access grants nothing. Zero grants is denial, not "all".
    #[must_use]
    pub fn denies_all(&self) -> bool {
        !self.is_admin && self.granted_library_ids.is_empty()
    }
}

/// Read the owner's library grants from the CURRENT database state.
///
/// Runs off the executor via [`SQLStore::with_connection`]. The scoped read in
/// [`nd_db::scanner_queries`] decides the final clause from these facts plus the
/// `library` table count; this function neither renders nor caches a clause.
///
/// # Errors
/// [`PrincipalError::OwnerNotFound`] if the owner row is gone (e.g. deleted
/// between requests); [`PrincipalError::Operational`] on a store/SQLite failure.
pub async fn current_library_access(
    store: &SQLStore,
    principal: &ApiPrincipal,
) -> Result<LibraryAccess, PrincipalError> {
    let owner_id = principal.owner_id.clone();
    let outcome = store
        .with_connection(move |conn| {
            Ok(match UserRepo::new(conn).get(&owner_id) {
                Ok(owner) => LookupOutcome::Access(LibraryAccess {
                    owner_id: owner.id.clone(),
                    is_admin: owner.is_admin,
                    granted_library_ids: owner.libraries.iter().map(|library| library.id).collect(),
                }),
                Err(UserError::NotFound) => LookupOutcome::OwnerNotFound,
                Err(UserError::Sqlite(_)) => LookupOutcome::Operational,
            })
        })
        .await
        .map_err(|_| PrincipalError::Operational)?;

    match outcome {
        LookupOutcome::Access(access) => Ok(access),
        LookupOutcome::OwnerNotFound => Err(PrincipalError::OwnerNotFound),
        LookupOutcome::Principal(_)
        | LookupOutcome::CredentialNotFound
        | LookupOutcome::Operational => Err(PrincipalError::Operational),
    }
}
