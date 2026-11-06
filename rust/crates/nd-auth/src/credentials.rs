//! Mint/verify of signed tokens, the credential-switch verdict arms, and
//! epoch/ownership/audience gate — the port of `core/auth/auth.go` (#13,
//! Model-B credential scheme per `docs/rust-port/credentials.md`).
//!
//! Go sources: dual signing secrets `core/auth/auth.go:23-29` (`TokenAuth`
//! signs UI/API session tokens and is rotated; `PublicTokenAuth` signs
//! public-link tokens on a separate secret that survives the rotation) and
//! `:50` (`Init`), `CreateToken` `:90-95`, `CreateAPIToken` `:85-88`,
//! `TouchClaims` `:110-114`, `Validate` `:117-119`, `CheckClaims` `:139-158`,
//! `validateCredentials` `server/subsonic/middlewares.go:269-285`,
//! `authenticateAPIKey`/`authenticate` `:100-188`. Epoch revocation's SQL
//! lives in `nd-db`'s `user_repo::put_password_update_sql`
//! (`persistence/user_repository.go:129-144`); this module is the consumer.
//!
//! NOT PORTED AT ALL (per `credentials.md` §1/§10; #25 is CANCELLED): the
//! `p` arm (`decodePassword(pass) == user.Password`, plaintext/`enc:`-hex
//! compare, `middlewares.go:243,269,281`) and the `t+s` arm
//! (`md5(password+salt)`, `middlewares.go:282-284`). Password verification
//! is argon2id-only, and no AES-GCM credential store exists in this port.
//! `subsonic`/`jellyfin` audiences are likewise gone: only
//! [`crate::claims::AUDIENCE_NATIVE`] is minted or accepted.
//!
//! SECURITY invariants: secrets are never logged, never compared with a
//! timing-leaky `==` — token-form verdicts are boolean digests and API keys
//! compare SHA-256 hex strings; audience is a *required* claim for API
//! tokens (jsonwebtoken only checks it against the accepted set, so an
//! aud-less API token is additionally rejected by
//! [`AuthError::MissingAudience`], the Part C #10 discipline), and epoch is
//! checked before an API token is accepted at all.

use argon2::password_hash::phc::PasswordHash as PhcHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::Algorithm as Argon2Algorithm;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, Header, Validation};
use sha2::{Digest, Sha256};

use crate::claims::{Claims, TokenSubject, AUDIENCE_NATIVE, JWT_ISSUER};

/// Default session timeout, 48h (`consts.DefaultSessionTimeout`,
/// `consts/consts.go:36`) — `TouchClaims` stamps `exp = now + this`
/// (`auth.go:110-114`).
pub const DEFAULT_SESSION_TIMEOUT_SECS: i64 = 48 * 60 * 60;

/// API key prefix (`consts.APIKeyPrefix`, `consts/consts.go:52`); the
/// `nds_` prefix of `^nds_[0-9A-Za-z]{22}$` (`player_repository.go:120`).
pub const API_KEY_PREFIX: &str = "nds_";

/// Errors — the Go sentinel trio of `auth.go:132-136` plus the transport
/// and Model-B failures. Never carries a secret or hash in its message.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum AuthError {
    /// `ErrTokenRevoked` (`auth.go:133`): epoch mismatch — a password
    /// change (`put_password_update_sql`) bumped `token_epoch` past the
    /// token's `ep` claim.
    #[error("token revoked")]
    TokenRevoked,
    /// `ErrWrongAudience` (`auth.go:134`): token not scoped for this API.
    #[error("token not valid for this API")]
    WrongAudience,
    /// `ErrWrongUser` (`auth.go:135`): `uid` names a different user id.
    #[error("token issued for a different user")]
    WrongUser,
    /// `ErrInvalidAuth` (`model.ErrInvalidAuth`): wrong password or a
    /// malformed API key.
    #[error("invalid auth")]
    InvalidAuth,
    /// A non-expiring API token has no `aud` claim — Part C #10 requires
    /// the audience discipline to hold before any signature is accepted.
    #[error("token missing audience claim")]
    MissingAudience,
    /// Signature/expiry/shape rejection — `jwtauth.VerifyToken`'s error
    /// (`auth.go:118`). Wrapped as a static, not a passthrough, so no
    /// crate detail or claim text can leak into logs.
    #[error("invalid token")]
    InvalidToken,
}

/// The dual signing secrets — `core/auth/auth.go:23-29,50`. Current +
/// previous for each: rotation pushes the current key into `previous` so a
/// verification window accepts tokens minted under either, while minting
/// always uses `current` (`credentials.md` §3 rotation survival).
// NO Debug derive: it would dump raw signing keys into any log/error path
// (security-reviewer #13 finding 1).
#[derive(Clone)]
pub struct SigningSecrets {
    /// Session/API tokens (`TokenAuth`, rotated).
    pub current: Vec<u8>,
    /// The most-recently retired session secret, if a rotation happened.
    pub previous: Option<Vec<u8>>,
    /// Public-link tokens (`PublicTokenAuth`, survives rotation).
    pub public_current: Vec<u8>,
}

/// The token mints and verifies — `TokenAuth`/`PublicTokenAuth` +
/// `CheckClaims` bound to their secrets, the receiver for every operation.
/// The raw secrets are kept (not the `jsonwebtoken` key handles) because
/// `jsonwebtoken::EncodingKey` is `Clone` but not `Debug`, and `validate`
/// re-derives its `DecodingKey`s per call from these copies.
#[derive(Clone)]
pub struct Authenticator {
    secrets: SigningSecrets,
    session_timeout_secs: i64,
}

impl Authenticator {
    /// `Init` (`auth.go:50-54`): bind the loaded secrets. Owns no DB —
    /// the caller loads or creates the `property`-table values
    /// (`credentials.md` §3: fresh service stores them plaintext).
    pub fn new(secrets: SigningSecrets, session_timeout_secs: i64) -> Self {
        Self {
            session_timeout_secs,
            secrets,
        }
    }

    /// The session timeout in effect — the configured
    /// `conf.Server.SessionTimeout` (`auth.go:47`).
    #[must_use]
    pub fn session_timeout_secs(&self) -> i64 {
        self.session_timeout_secs
    }

    /// `CreateToken` (`auth.go:90-95`): a session token with no audience
    /// and the session timeout stamped (`TouchToken`'s `exp` is the only
    /// difference from `CreateAPIToken`).
    pub fn create_token(
        &self,
        subject: &TokenSubject,
        username: &str,
        is_admin: bool,
        now: i64,
    ) -> Result<String, AuthError> {
        let claims = Claims::for_user(subject, username, is_admin, None, now)
            .with_expires_at(now + self.session_timeout_secs);
        self.encode_session_claims(&claims)
    }

    /// `CreateAPIToken` (`auth.go:85-88`): a non-expiring token scoped to
    /// one API — revocation is by epoch, not expiry. Audience is always
    /// [`AUDIENCE_NATIVE`]; no other value is mintable in this port.
    pub fn create_api_token(
        &self,
        subject: &TokenSubject,
        username: &str,
        is_admin: bool,
        now: i64,
    ) -> Result<String, AuthError> {
        let claims = Claims::for_user(subject, username, is_admin, Some(AUDIENCE_NATIVE), now);
        self.encode_session_claims(&claims)
    }

    /// `TouchClaims` (`auth.go:110-114`): re-sign a verified token's
    /// claims with a fresh session timeout. The epoch claim is NOT
    /// re-stamped here — the refreshing-writer path (`server/auth.go:310-327`)
    /// owns that and supplies fresh claims.
    pub fn touch_claims(&self, claims: &Claims, now: i64) -> Result<String, AuthError> {
        let touched = claims
            .clone()
            .with_expires_at(now + self.session_timeout_secs);
        self.encode_session_claims(&touched)
    }

    fn encode_session_claims(&self, claims: &Claims) -> Result<String, AuthError> {
        encode(
            &Header::new(Algorithm::HS256),
            claims,
            &jsonwebtoken::EncodingKey::from_secret(&self.secrets.current),
        )
        .map_err(|_| AuthError::InvalidToken)
    }

    /// `Validate` (`auth.go:117-119`): signature + expiry verify. The
    /// rotation window (`credentials.md` §3): tries the current secret,
    /// then the previous one. Callers must still run [`Self::check_session_claims`]
    /// before trusting the claims.
    pub fn validate(&self, token_str: &str) -> Result<Claims, AuthError> {
        if let Ok(data) = decode::<Claims>(
            token_str,
            &DecodingKey::from_secret(&self.secrets.current),
            &Self::validation(),
        ) {
            return Ok(data.claims);
        }
        // Only the previous-secret fallback can revive the token here.
        match &self.secrets.previous {
            Some(prev) => decode::<Claims>(
                token_str,
                &DecodingKey::from_secret(prev),
                &Self::validation(),
            )
            .map(|d| d.claims)
            .map_err(|_| AuthError::InvalidToken),
            None => Err(AuthError::InvalidToken),
        }
    }

    /// `Validation` with the `ND` issuer required. The audience set is
    /// [`AUDIENCE_NATIVE`] only (Part C #10) — a token carrying any other
    /// audience is rejected at the signature gate, and an aud-less token
    /// additionally fails the API-acceptance check in
    /// [`Self::check_api_claims`].
    fn validation() -> Validation {
        let mut v = Validation::new(Algorithm::HS256);
        // `exp` must not be a *required* claim: API tokens are minted
        // non-expiring (`CreateAPIToken`, `auth.go:85-88` — revocation is
        // by epoch, not expiry). Expiry is still enforced whenever the
        // claim IS present, which keeps expired session tokens dead.
        v.required_spec_claims.clear();
        // No leeway: `jwtauth.VerifyToken` accepts no clock skew
        // (`auth.go:117-119`); the crate default of 60s would keep a
        // freshly-expired session token alive.
        v.leeway = 0;
        v.set_issuer(&[JWT_ISSUER]);
        v.set_audience(&[AUDIENCE_NATIVE]);
        v
    }

    /// `ValidatePublic` (`auth.go:121-130`): the public-link secret. Not
    /// an audience-checking path (`credentials.md` §6): public tokens
    /// carry `id`/`sid`/`f`/`b`, not user audiences, so `aud` is not
    /// required. Users of this method MUST treat the returned claims as
    /// trusted only after this verifying call — the decode-only Go helper
    /// is deliberately absent here.
    pub fn validate_public(&self, token_str: &str) -> Result<Claims, AuthError> {
        let mut v = Validation::new(Algorithm::HS256);
        v.required_spec_claims.clear();
        v.leeway = 0;
        v.set_issuer(&[JWT_ISSUER]);
        v.validate_aud = false;
        decode::<Claims>(
            token_str,
            &DecodingKey::from_secret(&self.secrets.public_current),
            &v,
        )
        .map(|d| d.claims)
        .map_err(|_| AuthError::InvalidToken)
    }

    /// `CreatePublicToken` (`auth.go:70-84`): mint on the public secret;
    /// `exp` is zero (non-expiring) unless the caller set one
    /// (`CreateExpiringPublicToken`).
    pub fn create_public_token(&self, claims: &Claims) -> Result<String, AuthError> {
        let mut minted = claims.clone();
        minted.iss = JWT_ISSUER.to_string();
        encode(
            &Header::new(Algorithm::HS256),
            &minted,
            &jsonwebtoken::EncodingKey::from_secret(&self.secrets.public_current),
        )
        .map_err(|_| AuthError::InvalidToken)
    }

    /// `CheckClaims` (`auth.go:139-158`): gate a *verified* session token
    /// against the user it names. Same order as Go — wrong user id →
    /// revoked epoch → audience. Named `check_session_claims` (not the
    /// Go-faithful `check_claims`) so no future slice mistakes it for the
    /// API gate: its empty-`aud` = "any API" Go-parity arm is unsound for
    /// API acceptance (Part C #10); API paths MUST use `check_api_claims`
    /// which rejects aud-less tokens (security-reviewer #13 finding 3).
    pub fn check_session_claims(
        &self,
        claims: &Claims,
        subject: &TokenSubject,
        username: &str,
        audience: &str,
    ) -> Result<(), AuthError> {
        // Usernames can be reused: deleting a user and recreating the name
        // yields a new random id at epoch 0, which an old token must not
        // match (`auth.go:143-145`).
        if !claims.user_id.is_empty() && claims.user_id != subject.id {
            return Err(AuthError::WrongUser);
        }
        if claims.epoch != subject.token_epoch {
            return Err(AuthError::TokenRevoked);
        }
        if !claims.aud.is_empty() && !claims.aud.iter().any(|a| a == audience) {
            return Err(AuthError::WrongAudience);
        }
        let _ = username; // sub match is the caller's; Go logs it, not the gate
        Ok(())
    }

    /// The audience discipline for API acceptance (Part C #10): a token
    /// must actually carry `aud: [native]` — an aud-less token is NOT
    /// accepted for API use even though `check_claims` would pass it
    /// (`auth.go:152-154` treated empty as "any API"). Checked before
    /// the caller accepts the signature as authorization.
    pub fn check_api_claims(
        &self,
        claims: &Claims,
        subject: &TokenSubject,
        username: &str,
    ) -> Result<(), AuthError> {
        if claims.aud.iter().any(|a| a == AUDIENCE_NATIVE) {
            self.check_session_claims(claims, subject, username, AUDIENCE_NATIVE)
        } else {
            Err(AuthError::MissingAudience)
        }
    }

    /// The `jwt` arm of `validateCredentials`
    /// (`server/subsonic/middlewares.go:273-278`): verify the signature,
    /// the subject, and the native-audience claims. Only arm 1 of the
    /// Go switch survives (see the module doc for the cancelled `p`
    /// and `t+s` arms).
    pub fn validate_jwt_credential(
        &self,
        token_str: &str,
        subject: &TokenSubject,
        username: &str,
    ) -> Result<Claims, AuthError> {
        let claims = self.validate(token_str)?;
        if claims.sub != username {
            return Err(AuthError::WrongUser);
        }
        self.check_api_claims(&claims, subject, username)?;
        Ok(claims)
    }

    /// The `apiKey` arm of the credential switch
    /// (`server/subsonic/middlewares.go:127-128` → `authenticateAPIKey`
    /// `:190-231`): look up a presented API key by its SHA-256 hex hash.
    /// A keyed SHA-256 hex digest is constant-time to compare here
    /// (base64 encoding is not byte-ordered): compare the full digests
    /// with a fixed-traversal fold rather than `==`, so the comparison
    /// time cannot depend on where the strings differ.
    pub fn verify_api_key_hash(presented_key: &str, stored_hex_hash: &str) -> bool {
        if !presented_key.starts_with(API_KEY_PREFIX) {
            return false;
        }
        let digest = Sha256::digest(presented_key.as_bytes());
        let presented_hex = hex_of(&digest);
        // Constant-time compare (`credentials.md`: never a timing-leaky
        // string `==` on key material); `subtle` is not in the vetted set,
        // so the fold is hand-rolled with the same semantics.
        let stored = stored_hex_hash.as_bytes();
        let presented = presented_hex.as_bytes();
        stored.len() == presented.len()
            && stored
                .iter()
                .zip(presented)
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }

    /// The `internal` arm of the credential switch
    /// (`server/subsonic/middlewares.go:112-126`): a trusted internal or
    /// reverse-proxy username (`ExtAuth`/`UsernameFromExtAuthHeader`,
    /// `server/auth.go:212,418-427`) — the CIDR/`@` trust decision is
    /// `trusted_sources.rs`'s; this arm only confirms the named user
    /// exists and matches the caller's expectation.
    pub fn validate_internal_auth(
        subject: &TokenSubject,
        username: &str,
        claimed_username: &str,
    ) -> Result<(), AuthError> {
        if username.eq_ignore_ascii_case(claimed_username) && !subject.id.is_empty() {
            Ok(())
        } else {
            Err(AuthError::InvalidAuth)
        }
    }
}

/// The password side of the credential verdicts — argon2id only
/// (`credentials.md` §1). The Go `passwordEncoder` and the plaintext/`enc:`
/// arms it served are NOT ported (#25 CANCELLED).
pub mod password {
    use super::*;
    use argon2::{Argon2, Params, Version};

    /// Errors minting or verifying an argon2id hash. Text is static —
    /// never includes the hash, salt, or password.
    #[derive(Debug, thiserror::Error, PartialEq)]
    pub enum PasswordError {
        /// `user.password` is not a parseable argon2id PHC string.
        #[error("stored password hash invalid")]
        InvalidHash,
        /// PHC encoding failed (memory/param pressure).
        #[error("password hash failed")]
        HashFailed,
    }

    /// `encryptPassword`'s replacement: mint an argon2id PHC string. The
    /// random 16-byte salt comes from the `password-hash` crate's own
    /// `getrandom`-backed `SaltString::generate` (argon2's own docs use
    /// this path), so the vetted-crate rule holds — argon2id v19 with
    /// the crate defaults, per `credentials.md` §1.
    pub fn hash_password(password: &str) -> Result<String, PasswordError> {
        let argon2 = Argon2::new(Argon2Algorithm::Argon2id, Version::V0x13, Params::default());
        argon2
            .hash_password(password.as_bytes())
            .map(|h| h.to_string())
            .map_err(|_| PasswordError::HashFailed)
    }

    /// Verification is always argon2id verify (`credentials.md` §1: no
    /// plaintext compare, no `enc:` decode, no `md5(t+s)` — #25 CANCELLED,
    /// see the module doc in `credentials.rs`).
    pub fn verify_password(password: &str, stored_hash: &str) -> Result<bool, PasswordError> {
        let parsed = PhcHash::new(stored_hash).map_err(|_| PasswordError::InvalidHash)?;
        Ok(Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok())
    }
}

fn hex_of(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[usize::from(b >> 4)] as char);
        s.push(HEX[usize::from(b & 0x0f)] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claims::{Claims, TokenSubject, AUDIENCE_NATIVE, JWT_ISSUER};

    fn subject(id: &str, epoch: i64) -> TokenSubject {
        TokenSubject {
            id: id.to_string(),
            token_epoch: epoch,
        }
    }

    fn secrets() -> SigningSecrets {
        SigningSecrets {
            current: b"session-secret-current".to_vec(),
            previous: Some(b"session-secret-old".to_vec()),
            public_current: b"public-secret".to_vec(),
        }
    }

    /// Wall-clock seconds — jsonwebtoken's validation compares `exp`
    /// against the current timestamp, so tests use real time.
    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default()
    }

    fn test_auth() -> Authenticator {
        Authenticator::new(secrets(), DEFAULT_SESSION_TIMEOUT_SECS)
    }

    fn valid_password_hash() -> String {
        password::hash_password("swordfish").unwrap()
    }

    // ---- mint/verify round-trip: session token ----

    #[test]
    fn create_token_round_trips_with_native_gates() {
        // Arrange: a user at epoch 3.
        let auth = test_auth();
        let subj = subject("user-abc", 3);
        let token = auth.create_token(&subj, "alice", true, now()).unwrap();

        // Act: verify + gate against the same subject.
        let claims = auth.validate(&token).unwrap();
        let gate = auth.check_api_claims(&claims, &subj, "alice");

        // Assert: session tokens carry no audience, so the Part C #10
        // aud-less rule rejects them for API use.
        assert!(claims.aud.is_empty());
        assert!(matches!(gate, Err(AuthError::MissingAudience)));
        assert_eq!(claims.sub, "alice");
        assert_eq!(claims.user_id, "user-abc");
        assert_eq!(claims.epoch, 3);
        assert!(claims.is_admin);
        assert_eq!(claims.iss, JWT_ISSUER);
    }

    #[test]
    fn create_api_token_round_trips_and_passes_gates() {
        // Arrange.
        let auth = test_auth();
        let subj = subject("user-abc", 3);
        let token = auth.create_api_token(&subj, "alice", false, now()).unwrap();

        // Act.
        let claims = auth.validate(&token).unwrap();
        let gate = auth.check_api_claims(&claims, &subj, "alice");

        // Assert: minted with aud=[native], non-expiring, gates pass.
        assert_eq!(claims.aud, vec![AUDIENCE_NATIVE]);
        assert_eq!(claims.exp, 0);
        assert!(gate.is_ok());
    }

    #[test]
    fn validate_rejects_garbage_and_wrong_secret() {
        // Arrange.
        let auth = test_auth();
        let forged = Authenticator::new(
            SigningSecrets {
                current: b"attacker".to_vec(),
                previous: None,
                public_current: b"p".to_vec(),
            },
            DEFAULT_SESSION_TIMEOUT_SECS,
        );

        // Act: garbage, expired, and signature-forged tokens.
        let garbage = auth.validate("not-a-token");
        let expired = {
            let subj = subject("u", 0);
            let old = Claims::for_user(&subj, "a", false, Some(AUDIENCE_NATIVE), now() - 10)
                .with_expires_at(now() - 5);
            encode(
                &Header::new(Algorithm::HS256),
                &old,
                &jsonwebtoken::EncodingKey::from_secret(b"session-secret-current"),
            )
            .unwrap()
        };
        let subj = subject("u", 0);
        let forged_token = forged.create_api_token(&subj, "a", false, now()).unwrap();

        // Assert.
        assert!(matches!(garbage, Err(AuthError::InvalidToken)));
        assert!(matches!(
            auth.validate(&expired),
            Err(AuthError::InvalidToken)
        ));
        assert!(matches!(
            auth.validate(&forged_token),
            Err(AuthError::InvalidToken)
        ));
    }

    // ---- rotation window: previous secret still verifies ----

    #[test]
    fn rotation_window_verifies_previous_secret_tokens() {
        // Arrange: a token minted under the previous (pre-rotation) secret.
        let pre = Authenticator::new(
            SigningSecrets {
                current: b"session-secret-old".to_vec(),
                previous: None,
                public_current: b"public-secret".to_vec(),
            },
            DEFAULT_SESSION_TIMEOUT_SECS,
        );
        let subj = subject("user-abc", 1);
        let old_token = pre.create_api_token(&subj, "alice", false, now()).unwrap();

        // Act: verify with the post-rotation Authenticator (current =
        // new secret, previous = old secret).
        let claims = test_auth().validate(&old_token);

        // Assert: accepted inside the rotation window, and a minted-new
        // token also still verifies.
        assert!(claims.is_ok());
        let fresh = test_auth();
        let t2 = fresh
            .create_api_token(&subj, "alice", false, now())
            .unwrap();
        assert!(fresh.validate(&t2).is_ok());
    }

    #[test]
    fn without_previous_secret_old_tokens_die_at_rotation() {
        // Arrange: same pre-rotation token, but the rotation dropped the
        // previous secret (None).
        let pre = Authenticator::new(
            SigningSecrets {
                current: b"session-secret-old".to_vec(),
                previous: None,
                public_current: b"public-secret".to_vec(),
            },
            DEFAULT_SESSION_TIMEOUT_SECS,
        );
        let subj = subject("u", 0);
        let old_token = pre.create_api_token(&subj, "a", false, now()).unwrap();

        // Act.
        let rotated = Authenticator::new(
            SigningSecrets {
                current: b"session-secret-current".to_vec(),
                previous: None,
                public_current: b"public-secret".to_vec(),
            },
            DEFAULT_SESSION_TIMEOUT_SECS,
        );
        let verdict = rotated.validate(&old_token);

        // Assert: stale sessions die (`auth.go:24` rotation intent).
        assert!(matches!(verdict, Err(AuthError::InvalidToken)));
    }

    // ---- CheckClaims: epoch / wrong-user / audience ----

    #[test]
    fn check_session_claims_epoch_bump_invalidates_old_token() {
        // Arrange: token minted at epoch 3, user's epoch bumped to 4 by a
        // password change (`put_password_update_sql`).
        let auth = test_auth();
        let before = subject("user-abc", 3);
        let token = auth
            .create_api_token(&before, "alice", false, now())
            .unwrap();
        let claims = auth.validate(&token).unwrap();
        let after = subject("user-abc", 4);

        // Act: gate against the post-bump epoch.
        let verdict = auth.check_api_claims(&claims, &after, "alice");

        // Assert: `ErrTokenRevoked` (`auth.go:149-150`).
        assert!(matches!(verdict, Err(AuthError::TokenRevoked)));
    }

    #[test]
    fn check_session_claims_rejects_reused_username_with_wrong_id() {
        // Arrange: a deleted-then-recreated user — same name, new id,
        // epoch 0 (`auth.go:143-145`).
        let auth = test_auth();
        let old_subject = subject("old-random-id", 7);
        let token = auth
            .create_api_token(&old_subject, "alice", false, now())
            .unwrap();
        let claims = auth.validate(&token).unwrap();
        let recreated = subject("new-random-id", 0);

        // Act: sub matches but uid does not.
        let verdict = auth.check_api_claims(&claims, &recreated, "alice");

        // Assert: `ErrWrongUser`, not `ErrTokenRevoked` (Go precedence:
        // the uid check runs first).
        assert!(matches!(verdict, Err(AuthError::WrongUser)));
    }

    #[test]
    fn check_session_claims_epoch_zero_is_a_real_epoch() {
        // Arrange: a brand-new user at epoch 0 minting a token.
        let auth = test_auth();
        let subj = subject("fresh-id", 0);
        let token = auth.create_api_token(&subj, "bob", false, now()).unwrap();
        let claims = auth.validate(&token).unwrap();

        // Act + Assert: epoch 0 matches epoch 0.
        assert!(auth.check_api_claims(&claims, &subj, "bob").is_ok());
    }

    // ---- audience discipline ----

    #[test]
    fn cross_audience_token_is_rejected_at_validation() {
        // Arrange: a token minted with a foreign audience — Go's set had
        // `subsonic`/`jellyfin`, but those constants are gone; forge one.
        let auth = test_auth();
        let subj = subject("u", 0);
        let mut claims = Claims::for_user(&subj, "alice", false, None, now());
        claims.aud = vec!["subsonic".to_string()];
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(b"session-secret-current"),
        )
        .unwrap();

        // Act.
        let verdict = auth.validate(&token);

        // Assert: rejected before any claim is trusted — the validation
        // audience set is `native` ONLY (Part C #10).
        assert!(matches!(verdict, Err(AuthError::InvalidToken)));
    }

    #[test]
    fn aud_less_token_rejected_for_api_use() {
        // Arrange: a session-shaped token (no aud) with a valid signature.
        let auth = test_auth();
        let subj = subject("user-abc", 3);
        let token = auth.create_token(&subj, "alice", false, now()).unwrap();
        let claims = auth.validate(&token).unwrap();

        // Act: ask whether it authorizes API use.
        let verdict = auth.check_api_claims(&claims, &subj, "alice");

        // Assert: `MissingAudience` — the #10 discipline; an aud-less
        // token must not reach `check_claims`'s "empty means any" arm.
        assert!(matches!(verdict, Err(AuthError::MissingAudience)));
    }

    #[test]
    fn jwt_arm_rejects_subject_mismatch() {
        // Arrange: token for "alice" presented as "mallory".
        let auth = test_auth();
        let subj = subject("user-abc", 3);
        let token = auth.create_api_token(&subj, "alice", false, now()).unwrap();

        // Act: the `jwt` arm of the credential switch
        // (`middlewares.go:273-278`) checks sub.
        let verdict = auth.validate_jwt_credential(&token, &subj, "mallory");

        // Assert.
        assert!(matches!(verdict, Err(AuthError::WrongUser)));
        assert!(auth.validate_jwt_credential(&token, &subj, "alice").is_ok());
    }

    #[test]
    fn jwt_arm_rejects_expired_session_token() {
        // Arrange: a session token whose 48h expiry has passed.
        let auth = test_auth();
        let subj = subject("u", 0);
        let expired = auth
            .create_token(&subj, "a", false, now() - 10 * DEFAULT_SESSION_TIMEOUT_SECS)
            .unwrap();

        // Act.
        let verdict = auth.validate_jwt_credential(&expired, &subj, "a");

        // Assert.
        assert!(matches!(verdict, Err(AuthError::InvalidToken)));
    }

    // ---- apiKey arm ----

    #[test]
    fn api_key_arm_verdict_table() {
        // Arrange: a real `nds_` key's stored hash.
        let key = "nds_0123456789abcdefghijklmnopqr";
        let stored = hex_of(&Sha256::digest(key.as_bytes()));

        // Act + Assert: the presented key matches, a wrong key and a
        // non-prefixed key do not.
        assert!(Authenticator::verify_api_key_hash(key, &stored));
        assert!(!Authenticator::verify_api_key_hash(
            "nds_0123456789abcdefghijklmnopqs",
            &stored
        ));
        assert!(!Authenticator::verify_api_key_hash("deadbeef", &stored));
        // The digest itself never matches a raw stored value.
        assert!(!Authenticator::verify_api_key_hash(key, key));
    }

    // ---- internal arm ----

    #[test]
    fn internal_arm_verdict_table() {
        // Arrange.
        let subj = subject("user-abc", 3);

        // Act + Assert: matching username passes, mismatch and empty id
        // fail (`middlewares.go:112-126`).
        assert!(Authenticator::validate_internal_auth(&subj, "alice", "alice").is_ok());
        assert!(matches!(
            Authenticator::validate_internal_auth(&subj, "alice", "bob"),
            Err(AuthError::InvalidAuth)
        ));
        let empty = subject("", 3);
        assert!(matches!(
            Authenticator::validate_internal_auth(&empty, "alice", "alice"),
            Err(AuthError::InvalidAuth)
        ));
    }

    // ---- password: argon2id only ----

    #[test]
    fn password_arm_verdict_table() {
        // Arrange: argon2id hash of "swordfish".
        let stored = valid_password_hash();

        // Act + Assert: verify matches, wrong password does not, and a
        // malformed stored value is a hard error (`InvalidHash`), not a
        // false verdict.
        assert!(password::verify_password("swordfish", &stored).unwrap());
        assert!(!password::verify_password("wrong", &stored).unwrap());
        assert_eq!(
            password::verify_password("swordfish", "not-a-phc-hash"),
            Err(password::PasswordError::InvalidHash)
        );
    }

    #[test]
    fn hash_password_round_trips_and_uses_random_salts() {
        // Act: two independent hashes of the same password.
        let h1 = password::hash_password("swordfish").unwrap();
        let h2 = password::hash_password("swordfish").unwrap();

        // Assert: both verify, and the random salts made them distinct.
        assert!(password::verify_password("swordfish", &h1).unwrap());
        assert!(password::verify_password("swordfish", &h2).unwrap());
        assert_ne!(h1, h2);
    }

    // ---- public tokens ----

    #[test]
    fn public_token_mint_verifies_and_survives_rotation() {
        // Arrange: a public token (artwork) on the public secret.
        let pre = Authenticator::new(
            SigningSecrets {
                current: b"session-secret-old".to_vec(),
                previous: None,
                public_current: b"public-secret".to_vec(),
            },
            DEFAULT_SESSION_TIMEOUT_SECS,
        );
        let claims = Claims {
            id: "art-123".to_string(),
            ..Claims::default()
        };
        let token = pre.create_public_token(&claims).unwrap();

        // Act: verify after rotating the SESSION secrets — the public
        // secret is untouched (`auth.go:25-27`).
        let verified = test_auth().validate_public(&token);

        // Assert: issuer forced, id carried, still valid.
        assert!(verified.is_ok());
        let back = verified.unwrap();
        assert_eq!(back.iss, JWT_ISSUER);
        assert_eq!(back.id, "art-123");
    }

    #[test]
    fn public_token_rejects_session_secret_and_wrong_key() {
        // Arrange.
        let auth = test_auth();
        let claims = Claims {
            id: "art-123".to_string(),
            ..Claims::default()
        };
        let session_token = auth.create_public_token(&claims);

        // Act: forge a public-token claim with the SESSION secret —
        // only the public secret must be able to sign public tokens.
        let forged = Claims {
            id: "art-999".to_string(),
            ..Claims::default()
        };
        let forged_token = encode(
            &Header::new(Algorithm::HS256),
            &forged,
            &jsonwebtoken::EncodingKey::from_secret(b"session-secret-current"),
        )
        .unwrap();

        // Assert: the forged one fails, and so does garbage.
        assert!(session_token.is_ok());
        assert!(matches!(
            auth.validate_public(&forged_token),
            Err(AuthError::InvalidToken)
        ));
        assert!(matches!(
            auth.validate_public("junk"),
            Err(AuthError::InvalidToken)
        ));
    }

    // ---- touch ----

    #[test]
    fn touch_claims_restamps_expiry() {
        // Arrange: a near-expiry session token.
        let auth = test_auth();
        let subj = subject("u", 0);
        let token = auth.create_token(&subj, "a", false, now()).unwrap();
        let claims = auth.validate(&token).unwrap();

        // Act: re-sign at now+10.
        let touched = auth.touch_claims(&claims, now() + 10).unwrap();
        let back = auth.validate(&touched).unwrap();

        // Assert: exp = (now+10) + timeout, epoch preserved.
        assert_eq!(back.exp, now() + 10 + DEFAULT_SESSION_TIMEOUT_SECS);
        assert_eq!(back.epoch, claims.epoch);
    }
}
