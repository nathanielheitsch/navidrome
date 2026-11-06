//! The typed JWT claim model — the port of `core/auth/claims.go` (#13).
//!
//! Go source: `core/auth/claims.go:11-30` (the `Claims` struct and its
//! non-zero `ToMap` discipline at `:31-70`). Wire names are the Go camel /
//! short names byte-frozen by the settled credential contract
//! (`docs/rust-port/credentials.md` §4): `uid`, `adm`, `id`, `f`, `b`,
//! `sid`, `ep`. Token mint/verify live in [`super::credentials`], which
//! wraps this shape in the dual-secret signer.
//!
//! The Go bug is fixed, not copied: `ToMap` is a serialization helper and
//! never an acceptance path — every token consumer goes through the
//! signature-verifying decode in `credentials.rs` (`server/public/handle_images.go:76-93`
//! decoded public tokens without verifying; per `credentials.md` §6 that
//! decode-only surface does not exist here).

use serde::{Deserialize, Serialize};

/// The single audience a Rust-native session or API token may be minted for
/// (`core/auth/auth.go:33-36` had `jellyfin|subsonic|native`; per Part C #10
/// only `native` survives — `jellyfin` left with #30, `subsonic` was never
/// minted on this surface).
pub const AUDIENCE_NATIVE: &str = "native";

/// The token issuer (`consts.JWTIssuer`, `consts/consts.go:35`).
pub const JWT_ISSUER: &str = "ND";

/// The typed JWT claims used throughout the port, replacing the untyped
/// map approach — the `Claims` struct of `core/auth/claims.go:11-30`.
/// `ToMap` emits only non-zero fields (`claims.go:31-70`); serde keeps that
/// contract: every custom claim is `Option`/`skip_serializing_if`-guarded
/// and the standard claims use the same zero-means-absent rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Claims {
    /// Standard claims (`claims.go:12-17`). The issuer is always
    /// [`JWT_ISSUER`] on any minted token (`auth.go:70-84`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub iss: String,
    /// Username for session tokens (`claims.go:14`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sub: String,
    /// Which API may accept this token; empty means any (`claims.go:15`).
    /// In the fresh service this is only ever empty (session tokens) or
    /// [`AUDIENCE_NATIVE`] (API tokens) — no other audience is minted, and
    /// the decoded claim is subject to the consumer's audience discipline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aud: Vec<String>,
    /// Seconds since the Unix epoch, matching Go's `UTC().Unix()` stamp
    /// (`claims.go:47,52`).
    #[serde(default, skip_serializing_if = "is_zero_ts")]
    pub iat: i64,
    /// Ditto — `exp` is zero only on non-expiring API tokens
    /// (`auth.go:85-88`).
    #[serde(default, skip_serializing_if = "is_zero_ts")]
    pub exp: i64,

    // Custom claims (`claims.go:19-29`).
    /// `uid` — the user id; the mismatch sentinel for [`super::credentials::check_session_claims`]
    /// (`auth.go:145-146`: usernames can be reused; the id cannot).
    #[serde(rename = "uid", default, skip_serializing_if = "String::is_empty")]
    pub user_id: String,
    /// `adm` — admin flag. Never serialized when false (`claims.go:57-59`).
    #[serde(rename = "adm", default, skip_serializing_if = "std::ops::Not::not")]
    pub is_admin: bool,
    /// `id` — artwork/mediafile id, on public-link tokens.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// `f` — requested audio format (public stream tokens).
    #[serde(rename = "f", default, skip_serializing_if = "String::is_empty")]
    pub format: String,
    /// `b` — requested audio bitrate. 0 means unset (`claims.go:68-69`).
    #[serde(rename = "b", default, skip_serializing_if = "is_zero")]
    pub bit_rate: i64,
    /// `sid` — share id, on share stream tokens.
    #[serde(rename = "sid", default, skip_serializing_if = "String::is_empty")]
    pub share_id: String,
    /// `ep` — the user's `token_epoch` captured at mint time
    /// (`user_repository.go:129-144` bumps it on every password change).
    #[serde(rename = "ep", default, skip_serializing_if = "is_zero")]
    pub epoch: i64,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

fn is_zero_ts(v: &i64) -> bool {
    *v == 0
}

/// The snapshot of a user a token names — the `model.User`/`model.Player`
/// fields the epoch/ownership checks read. Sibling slices wire the real
/// entities; `credentials.rs` consumes only this projection.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenSubject {
    /// The user's stable id (`user.id`, 22-char base62 per `model/id`).
    pub id: String,
    /// The user's `token_epoch` (`user.token_epoch`, bumped by
    /// `put_password_update_sql` in `nd-db`'s user_repo — #13 is its
    /// consumer, not its owner).
    pub token_epoch: i64,
}

impl Claims {
    /// `userClaims` (`auth.go:68-77`): the session/API token shape for one
    /// user. Callers own `iat`; the audience is `None` for session tokens
    /// (empty `aud`, valid anywhere) or [`AUDIENCE_NATIVE`] for scoped API
    /// tokens (`auth.go:79-88`) — no other value is mintable here.
    #[must_use]
    pub fn for_user(
        subject: &TokenSubject,
        username: &str,
        is_admin: bool,
        audience: Option<&str>,
        iat: i64,
    ) -> Self {
        Self {
            iss: JWT_ISSUER.to_string(),
            sub: username.to_string(),
            aud: audience.map(|a| vec![a.to_string()]).unwrap_or_default(),
            iat,
            user_id: subject.id.clone(),
            is_admin,
            epoch: subject.token_epoch,
            ..Self::default()
        }
    }

    /// `WithExpiresAt` (`claims.go:72-75` / `auth.go:110-114` `TouchClaims`):
    /// stamp a fresh expiry and re-sign. Zero is Go's zero time and leaves
    /// the token non-expiring.
    #[must_use]
    pub fn with_expires_at(mut self, exp: i64) -> Self {
        self.exp = exp;
        self
    }
}
