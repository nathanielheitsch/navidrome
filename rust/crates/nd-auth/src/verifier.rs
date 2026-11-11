//! Request credential extraction and verified username resolution.
//!
//! The caller supplies the transport peer, never a client-controlled
//! forwarding header, for the ExtAuth trust decision.

use axum::extract::Query;
use axum::http::{header, HeaderMap, Uri};
use serde::Deserialize;

use crate::claims::Claims;
use crate::credentials::Authenticator;
use crate::trusted_sources::validate_ip_against_list;

/// UI bearer-token header (`consts/consts.go:31`, `server/auth.go:188-200`).
pub const UI_AUTHORIZATION_HEADER: &str = "x-nd-authorization";

/// Configuration for username resolution (`server/auth.go:203-247`).
#[derive(Debug, Clone, Default)]
pub struct ResolutionConfig {
    /// Development auto-login username (`server/auth.go:242-244`).
    pub dev_username: String,
    /// External authentication username header (`server/auth.go:220-223`).
    pub external_user_header: String,
    /// Comma-separated trusted proxy CIDRs or `@` (`server/auth.go:210-232`).
    pub trusted_sources: String,
    /// Listener address, needed for the unix-socket `@` case (`server/auth.go:418-452`).
    pub server_address: String,
}

/// Why a request was rejected as ambiguous rather than unauthenticated.
///
/// Go resolves the username by walking `UsernameFromConfig, UsernameFromToken,
/// UsernameFromExtAuthHeader` and stopping at the first nonempty result
/// (`server/auth.go:257-266`), so two credentials can arrive together and the
/// lower-priority one is silently ignored; `tokenAllowed` (`:249-255`) then only
/// inspects the token if that arm happened to win. This port treats that
/// situation as a conflict and fails closed — see [`Resolution`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthConflict {
    /// More than one auth source supplied a username and they disagree, or a
    /// supplied token failed validation while another source was present.
    #[error("conflicting authentication sources: {sources}")]
    Sources {
        /// Human-readable list of the disagreeing sources.
        sources: String,
    },
}

/// Username and signature-verified JWT, from exactly one auth source.
///
/// Deliberately STRICTER than Go's precedence order (`server/auth.go:257-266`).
/// Silently preferring one credential over another lets a stale cookie or query
/// token ride along with dev auto-login or a trusted-proxy header, and the
/// ignored token's claims are then never checked. Here a request presenting two
/// disagreeing sources is unauthenticated instead of decided on the client's
/// behalf. Sources naming the SAME username (case-insensitively, matching the
/// NOCASE lookup index) are not a conflict.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolution {
    /// The single username supplied by the auth sources that agreed.
    pub username: String,
    /// Verified claims, present only when a token supplied the username.
    pub claims: Option<Claims>,
}

#[derive(Deserialize)]
struct TokenQuery {
    jwt: Option<String>,
}

/// Extracts every supplied token, not only the highest-priority one
/// (`server/auth.go:184-200` collects header, cookie and query; the `jwtauth`
/// cookie and query names are `jwt`). Returning all of them is what lets
/// [`resolve_username`] notice a client presenting two token mechanisms at once
/// instead of silently picking one.
#[must_use]
pub fn supplied_tokens(headers: &HeaderMap, uri: &Uri) -> Vec<String> {
    let mut tokens = Vec::new();
    if let Some(value) = headers
        .get(UI_AUTHORIZATION_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        if let Some((scheme, token)) = value.split_once(' ') {
            if scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() {
                tokens.push(token.to_owned());
            }
        }
    }
    for value in headers.get_all(header::COOKIE) {
        if let Ok(value) = value.to_str() {
            for cookie in value.split(';') {
                if let Some((name, token)) = cookie.trim().split_once('=') {
                    if name == "jwt" && !token.is_empty() {
                        tokens.push(token.trim_matches('"').to_owned());
                    }
                }
            }
        }
    }
    if let Some(token) = Query::<TokenQuery>::try_from_uri(uri)
        .ok()
        .and_then(|query| query.0.jwt.filter(|token| !token.is_empty()))
    {
        tokens.push(token);
    }
    tokens
}

/// Highest-priority supplied token (header, cookie, query) for callers that only
/// need one. Prefer [`supplied_tokens`] where conflict detection matters.
#[must_use]
pub fn token_from_request(headers: &HeaderMap, uri: &Uri) -> Option<String> {
    supplied_tokens(headers, uri).into_iter().next()
}

/// Resolves a single unambiguous username, FAILING CLOSED on conflict.
///
/// See [`Resolution`] for why this deliberately drops Go's precedence order
/// (`server/auth.go:257-266`). `proxy_ip` is supplied by the transport; do not
/// derive it from an untrusted request header.
#[must_use]
pub fn resolve_username(
    auth: &Authenticator,
    config: &ResolutionConfig,
    headers: &HeaderMap,
    uri: &Uri,
    proxy_ip: Option<&str>,
) -> Option<Resolution> {
    resolve_inner(auth, config, headers, uri, proxy_ip).ok()
}

/// Fail-closed resolution that reports which sources disagreed.
pub fn resolve_username_checked(
    auth: &Authenticator,
    config: &ResolutionConfig,
    headers: &HeaderMap,
    uri: &Uri,
    proxy_ip: Option<&str>,
) -> Result<Resolution, AuthConflict> {
    resolve_inner(auth, config, headers, uri, proxy_ip)
        .map_err(|sources| AuthConflict::Sources { sources })
}

fn resolve_inner(
    auth: &Authenticator,
    config: &ResolutionConfig,
    headers: &HeaderMap,
    uri: &Uri,
    proxy_ip: Option<&str>,
) -> Result<Resolution, String> {
    let tokens = supplied_tokens(headers, uri);
    // Every supplied token must validate. A request carrying a stale or forged
    // token alongside another credential is ambiguous, not "ignore the bad one".
    let mut verified: Vec<Claims> = Vec::new();
    for token in &tokens {
        verified.push(auth.validate(token).map_err(|_| "JWT token".to_owned())?);
    }

    let mut sources: Vec<&str> = Vec::new();
    let mut usernames: Vec<String> = Vec::new();
    if !config.dev_username.is_empty() {
        sources.push("dev auto-login");
        usernames.push(config.dev_username.clone());
    }
    if !tokens.is_empty() {
        sources.push("JWT token");
        for claims in verified.iter().filter(|claims| !claims.sub.is_empty()) {
            usernames.push(claims.sub.clone());
        }
    }
    // Only count the ExtAuth arm when the peer is genuinely trusted, so an
    // untrusted proxy cannot manufacture a conflict.
    let ext = if tokens.is_empty() {
        header_username(config, headers, proxy_ip)
    } else {
        trusted_header_username(config, headers, proxy_ip)
    };
    if let Some(username) = ext {
        sources.push("ExtAuth header");
        usernames.push(username);
    }

    if usernames.is_empty() {
        return Err("no auth source".to_owned());
    }
    let distinct: std::collections::HashSet<String> =
        usernames.iter().map(|name| name.to_lowercase()).collect();
    if distinct.len() != 1 {
        return Err(sources.join(" + "));
    }
    let Some(username) = usernames.first() else {
        return Err("no auth source".to_owned());
    };
    let username = username.clone();
    if username.is_empty() {
        return Err("empty username".to_owned());
    }
    // Claims ride only when a token supplied the username, so a dev-config or
    // ExtAuth request never carries someone else's claims.
    let claims = if sources.contains(&"JWT token") {
        verified.into_iter().find(|claims| !claims.sub.is_empty())
    } else {
        None
    };
    Ok(Resolution { username, claims })
}

/// Trust check only: the ExtAuth username when the peer is trusted, with no
/// dev-config fallback. Lets conflict detection count a trusted proxy header as
/// a second auth source even when a token is also present.
#[must_use]
fn trusted_header_username(
    config: &ResolutionConfig,
    headers: &HeaderMap,
    proxy_ip: Option<&str>,
) -> Option<String> {
    let ip = proxy_ip?;
    if !validate_ip_against_list(ip, &config.trusted_sources, &config.server_address) {
        return None;
    }
    let username = headers
        .get(config.external_user_header.as_str())?
        .to_str()
        .ok()?;
    (!username.is_empty()).then(|| username.to_owned())
}

/// Resolves the dev-config or trusted proxy header without considering a JWT
/// (`server/auth.go:210-232,242-244,371-379`). The peer must be supplied by the
/// connection acceptor, not a client-controlled request header.
///
/// This is the login-endpoint arm (`login_from_headers`), where a JWT is not
/// part of the decision. Request authentication goes through
/// [`resolve_username`], which fails closed on conflicts.
#[must_use]
pub fn header_username(
    config: &ResolutionConfig,
    headers: &HeaderMap,
    proxy_ip: Option<&str>,
) -> Option<String> {
    if !config.dev_username.is_empty() {
        return Some(config.dev_username.clone());
    }
    trusted_header_username(config, headers, proxy_ip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn supplied_tokens_collects_every_mechanism_in_priority_order() {
        let uri: Uri = "/?jwt=query".parse().unwrap();
        let mut headers = HeaderMap::new();
        assert_eq!(supplied_tokens(&headers, &uri), ["query"]);
        headers.insert(header::COOKIE, HeaderValue::from_static("x=1; jwt=cookie"));
        assert_eq!(supplied_tokens(&headers, &uri), ["cookie", "query"]);
        headers.insert(
            UI_AUTHORIZATION_HEADER,
            HeaderValue::from_static("bearer header"),
        );
        assert_eq!(
            supplied_tokens(&headers, &uri),
            ["header", "cookie", "query"]
        );
        // Highest-priority extraction is still available for single-token callers.
        assert_eq!(
            token_from_request(&headers, &uri).as_deref(),
            Some("header")
        );
    }

    #[test]
    fn a_single_source_still_resolves() {
        let (auth, token, now) = fixture();
        let uri: Uri = "/".parse().unwrap();
        let config = ResolutionConfig {
            external_user_header: "x-user".into(),
            trusted_sources: "10.0.0.0/8,@".into(),
            server_address: "unix:/run/server.sock".into(),
            ..ResolutionConfig::default()
        };

        // Dev auto-login alone.
        let dev = ResolutionConfig {
            dev_username: "dev".into(),
            ..config.clone()
        };
        let headers = HeaderMap::new();
        assert_eq!(
            resolve_username(&auth, &dev, &headers, &uri, Some("10.1.2.3"))
                .unwrap()
                .username,
            "dev"
        );

        // A token alone carries its claims.
        let mut headers = HeaderMap::new();
        headers.insert(
            UI_AUTHORIZATION_HEADER,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        let resolved = resolve_username(&auth, &config, &headers, &uri, Some("10.1.2.3")).unwrap();
        assert_eq!(resolved.username, "jwt-user");
        assert!(resolved.claims.is_some());

        // A trusted proxy header alone.
        let mut headers = HeaderMap::new();
        headers.insert("x-user", HeaderValue::from_static("proxy-user"));
        let resolved = resolve_username(&auth, &config, &headers, &uri, Some("10.1.2.3")).unwrap();
        assert_eq!(resolved.username, "proxy-user");
        assert!(resolved.claims.is_none(), "no token means no claims");
        assert_eq!(
            resolve_username(&auth, &config, &headers, &uri, Some("@"))
                .unwrap()
                .username,
            "proxy-user"
        );

        // Untrusted peer and no other source -> unauthenticated, as in Go.
        assert!(resolve_username(&auth, &config, &headers, &uri, Some("11.1.2.3")).is_none());
        let mut unconfigured = config.clone();
        unconfigured.trusted_sources.clear();
        assert!(resolve_username(&auth, &unconfigured, &headers, &uri, Some("@")).is_none());
        let _ = now;
    }

    #[test]
    fn conflicting_sources_fail_closed_instead_of_preferring_one() {
        let (auth, token, _) = fixture();
        let uri: Uri = "/".parse().unwrap();
        let config = ResolutionConfig {
            dev_username: "dev".into(),
            external_user_header: "x-user".into(),
            trusted_sources: "10.0.0.0/8,@".into(),
            server_address: "unix:/run/server.sock".into(),
        };
        let mut headers = HeaderMap::new();
        headers.insert("x-user", HeaderValue::from_static("proxy-user"));
        headers.insert(
            UI_AUTHORIZATION_HEADER,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );

        // Go would take dev auto-login and silently ignore both the token and the
        // proxy header. Here the request is rejected outright.
        assert!(resolve_username(&auth, &config, &headers, &uri, Some("10.1.2.3")).is_none());
        assert_eq!(
            resolve_username_checked(&auth, &config, &headers, &uri, Some("10.1.2.3")),
            Err(AuthConflict::Sources {
                sources: "dev auto-login + JWT token + ExtAuth header".to_owned()
            })
        );

        // Token vs trusted proxy header, with dev login disabled.
        let no_dev = ResolutionConfig {
            dev_username: String::new(),
            ..config.clone()
        };
        assert!(resolve_username(&auth, &no_dev, &headers, &uri, Some("10.1.2.3")).is_none());
        assert_eq!(
            resolve_username_checked(&auth, &no_dev, &headers, &uri, Some("10.1.2.3")),
            Err(AuthConflict::Sources {
                sources: "JWT token + ExtAuth header".to_owned()
            })
        );

        // A trusted header alone still works once the token is gone.
        headers.remove(UI_AUTHORIZATION_HEADER);
        assert_eq!(
            resolve_username(&auth, &no_dev, &headers, &uri, Some("10.1.2.3"))
                .unwrap()
                .username,
            "proxy-user"
        );
    }

    #[test]
    fn sources_naming_the_same_user_agree() {
        let (auth, token, _) = fixture();
        let uri: Uri = "/".parse().unwrap();
        // Same user from dev config and token: not a conflict, and the claims ride.
        let config = ResolutionConfig {
            dev_username: "jwt-user".into(),
            ..ResolutionConfig::default()
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            UI_AUTHORIZATION_HEADER,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        let resolved = resolve_username(&auth, &config, &headers, &uri, None).unwrap();
        assert_eq!(resolved.username, "jwt-user");
        assert!(resolved.claims.is_some());

        // Case-insensitive agreement, matching the NOCASE lookup index.
        // Agreement is case-insensitive, but the reported username comes from the
        // highest-priority source (Go's arm order, `server/auth.go:257-266`). The
        // lookup itself is NOCASE, so both spellings resolve the same user.
        let mixed = ResolutionConfig {
            dev_username: "JWT-User".into(),
            ..ResolutionConfig::default()
        };
        let resolved = resolve_username(&auth, &mixed, &headers, &uri, None).unwrap();
        assert_eq!(resolved.username, "JWT-User");
        assert!(resolved.claims.is_some());

        // A trusted proxy header naming the same user also agrees.
        let trusted = ResolutionConfig {
            external_user_header: "x-user".into(),
            trusted_sources: "10.0.0.0/8".into(),
            ..ResolutionConfig::default()
        };
        headers.insert("x-user", HeaderValue::from_static("Jwt-User"));
        let resolved = resolve_username(&auth, &trusted, &headers, &uri, Some("10.1.2.3")).unwrap();
        assert_eq!(resolved.username, "jwt-user");
        assert!(resolved.claims.is_some());
    }

    #[test]
    fn invalid_token_fails_closed_when_another_source_is_present() {
        let (auth, _, _) = fixture();
        let uri: Uri = "/".parse().unwrap();
        let config = ResolutionConfig {
            dev_username: "dev".into(),
            ..ResolutionConfig::default()
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            UI_AUTHORIZATION_HEADER,
            HeaderValue::from_static("Bearer not-a-valid-token"),
        );
        // Go ignores the unusable token and authenticates from dev config; here the
        // unverifiable token makes the request ambiguous and it is rejected.
        assert!(resolve_username(&auth, &config, &headers, &uri, None).is_none());
        // An invalid token with no other source stays unauthenticated (as in Go).
        let none = ResolutionConfig::default();
        assert!(resolve_username(&auth, &none, &headers, &uri, None).is_none());
    }

    #[test]
    fn two_tokens_naming_different_users_conflict() {
        let (auth, token, now) = fixture();
        let other = auth
            .create_token(
                &crate::claims::TokenSubject {
                    id: "other".into(),
                    token_epoch: 0,
                },
                "other-user",
                false,
                now,
            )
            .unwrap();
        let uri: Uri = format!("/?jwt={other}").parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            UI_AUTHORIZATION_HEADER,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        assert!(
            resolve_username(&auth, &ResolutionConfig::default(), &headers, &uri, None).is_none()
        );
    }

    fn fixture() -> (Authenticator, String, i64) {
        use crate::claims::TokenSubject;
        use crate::credentials::SigningSecrets;
        use std::time::{SystemTime, UNIX_EPOCH};

        let auth = Authenticator::new(
            SigningSecrets {
                current: b"session secret".to_vec(),
                previous: None,
                public_current: b"public secret".to_vec(),
            },
            3600,
        );
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let subject = TokenSubject {
            id: "id".into(),
            token_epoch: 0,
        };
        let token = auth.create_token(&subject, "jwt-user", false, now).unwrap();
        (auth, token, now)
    }
}
