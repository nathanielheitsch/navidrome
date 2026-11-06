// nd-auth: owned by #13 (docs/rust-port/issues.md Part B)

//! Credential + claim model (model B), the jwt/apiKey/internal credential
//! switch arms, epoch revocation (#13's consumer of user_repo), the fail-closed
//! auth limiter, trusted sources, and publicurl. Vetted crates only.

/// Scaffold marker kept until #13's public surface replaces it.
pub const OWNED_BY_ISSUE: u8 = 13;

pub mod claims;
pub mod credentials;
pub mod initial_setup;
pub mod limiter;
pub mod publicurl;
pub mod trusted_sources;
