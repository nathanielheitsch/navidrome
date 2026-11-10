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

/// Configuration for ordered username resolution (`server/auth.go:203-247`).
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

/// Username and signature-verified JWT, if any (`server/auth.go:203-208,258-278`).
/// A JWT must still be checked against the resolved database user before use.
#[derive(Clone, Debug)]
pub struct Resolution {
    /// First nonempty username in config, JWT, ExtAuth order.
    pub username: String,
    /// Verified claims, even when a higher-priority arm selected the user.
    pub claims: Option<Claims>,
}

#[derive(Deserialize)]
struct TokenQuery {
    jwt: Option<String>,
}

/// Extracts the first supplied token in header, cookie, query order
/// (`server/auth.go:184-200`; `jwtauth` cookie and query names are `jwt`).
#[must_use]
pub fn token_from_request(headers: &HeaderMap, uri: &Uri) -> Option<String> {
    let header_token = headers
        .get(UI_AUTHORIZATION_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            let (scheme, token) = value.split_once(' ')?;
            (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
        });
    if let Some(token) = header_token {
        return Some(token.to_owned());
    }
    for value in headers.get_all(header::COOKIE) {
        if let Ok(value) = value.to_str() {
            for cookie in value.split(';') {
                if let Some((name, token)) = cookie.trim().split_once('=') {
                    if name == "jwt" && !token.is_empty() {
                        return Some(token.trim_matches('"').to_owned());
                    }
                }
            }
        }
    }
    Query::<TokenQuery>::try_from_uri(uri)
        .ok()
        .and_then(|query| query.0.jwt.filter(|token| !token.is_empty()))
}

/// Verifies the token and chooses a username in the Go arm order
/// (`server/auth.go:184-232,242-244,258-278`). `proxy_ip` is supplied by
/// the transport; do not derive it from an untrusted request header.
#[must_use]
pub fn resolve_username(
    auth: &Authenticator,
    config: &ResolutionConfig,
    headers: &HeaderMap,
    uri: &Uri,
    proxy_ip: Option<&str>,
) -> Option<Resolution> {
    let claims = token_from_request(headers, uri).and_then(|token| auth.validate(&token).ok());
    let username = if !config.dev_username.is_empty() {
        config.dev_username.clone()
    } else if let Some(claims) = claims.as_ref().filter(|claims| !claims.sub.is_empty()) {
        claims.sub.clone()
    } else {
        let ip = proxy_ip?;
        if !validate_ip_against_list(ip, &config.trusted_sources, &config.server_address) {
            return None;
        }
        headers
            .get(config.external_user_header.as_str())?
            .to_str()
            .ok()?
            .to_owned()
    };
    if username.is_empty() {
        None
    } else {
        Some(Resolution { username, claims })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn token_sources_obey_precedence() {
        let uri: Uri = "/?jwt=query".parse().unwrap();
        let mut headers = HeaderMap::new();
        assert_eq!(token_from_request(&headers, &uri).as_deref(), Some("query"));
        headers.insert(header::COOKIE, HeaderValue::from_static("x=1; jwt=cookie"));
        assert_eq!(
            token_from_request(&headers, &uri).as_deref(),
            Some("cookie")
        );
        headers.insert(
            UI_AUTHORIZATION_HEADER,
            HeaderValue::from_static("bearer header"),
        );
        assert_eq!(
            token_from_request(&headers, &uri).as_deref(),
            Some("header")
        );
    }

    #[test]
    fn resolution_uses_verified_tokens_and_trusted_proxies_in_order() {
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
        let mut headers = HeaderMap::new();
        headers.insert("x-user", HeaderValue::from_static("proxy-user"));
        headers.insert(
            UI_AUTHORIZATION_HEADER,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        let uri: Uri = "/".parse().unwrap();
        let mut config = ResolutionConfig {
            dev_username: "dev".into(),
            external_user_header: "x-user".into(),
            trusted_sources: "10.0.0.0/8,@".into(),
            server_address: "unix:/run/server.sock".into(),
        };
        assert_eq!(
            resolve_username(&auth, &config, &headers, &uri, Some("10.1.2.3"))
                .unwrap()
                .username,
            "dev"
        );
        config.dev_username.clear();
        assert_eq!(
            resolve_username(&auth, &config, &headers, &uri, Some("10.1.2.3"))
                .unwrap()
                .username,
            "jwt-user"
        );
        headers.remove(UI_AUTHORIZATION_HEADER);
        assert_eq!(
            resolve_username(&auth, &config, &headers, &uri, Some("10.1.2.3"))
                .unwrap()
                .username,
            "proxy-user"
        );
        assert!(resolve_username(&auth, &config, &headers, &uri, Some("11.1.2.3")).is_none());
        assert_eq!(
            resolve_username(&auth, &config, &headers, &uri, Some("@"))
                .unwrap()
                .username,
            "proxy-user"
        );
        config.trusted_sources.clear();
        assert!(resolve_username(&auth, &config, &headers, &uri, Some("@")).is_none());
    }
}
