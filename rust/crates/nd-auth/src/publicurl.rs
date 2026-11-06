//! Public-facing URL construction — the port of `core/publicurl/publicurl.go`.
//!
//! Go source: `publicurl.go:18-26` (`ImageURL`), `:29-53` (`PublicURL`),
//! `:56-85` (`AbsoluteURL`), pinned by `publicurl_test.go`. `ImageURL`'s
//! token mint lives in [`super::credentials`] (slice A), so this module
//! receives the already-signed token and only builds the URL.
//!
//! Port note: the Go module leans on `net/url`; this port is std-only and
//! builds the same URL shapes (scheme://host + joined path + encoded
//! query) that the golden tests pin.

use std::fmt::Write as _;

/// `/share/img` (`consts.URLPathPublicImages`, `consts/consts.go:61`).
pub const URL_PATH_PUBLIC_IMAGES: &str = "/share/img";

/// The address of the client-facing server endpoint, recorded by the
/// address middleware (`model/request.ServerAddressFrom`, consumed at
/// `publicurl.go:62,74`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerAddress {
    /// `https` or `http`.
    pub scheme: String,
    /// Host, with port if non-default.
    pub host: String,
}

/// The `conf.Server` slice `publicurl.go` reads: `ShareURL`, `BaseHost`,
/// `BaseScheme`, `BasePath`, `Port`, `TLSCert`/`TLSKey` and the request
/// address from the context.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PublicUrlConfig {
    /// `conf.Server.ShareURL` — preferred base for public resources.
    pub share_url: String,
    /// `conf.Server.BaseHost` — configured public host.
    pub base_host: String,
    /// `conf.Server.BaseScheme` — defaults to `http` when empty.
    pub base_scheme: String,
    /// `conf.Server.BasePath` — path prefix applied to absolute URLs.
    pub base_path: String,
    /// `conf.Server.Port` — used by the localhost fallback.
    pub port: u16,
    /// `conf.Server.TLSCert`.
    pub tls_cert: String,
    /// `conf.Server.TLSKey`.
    pub tls_key: String,
    /// The scheme/host of the triggering request, when available
    /// (`request.ServerAddressFrom`).
    pub server_address: Option<ServerAddress>,
}

impl PublicUrlConfig {
    /// `TLSEnabled()` — TLS counts only when both cert and key are set.
    fn tls_enabled(&self) -> bool {
        !self.tls_cert.is_empty() && !self.tls_key.is_empty()
    }
}

/// A parsed `scheme://host/path` base, the pieces `url.Parse` yields for
/// the shapes this module handles.
struct ParsedUrl {
    scheme: Option<String>,
    host: Option<String>,
    path: String,
    query: Option<String>,
}

/// Minimal stand-in for `url.Parse` on the inputs this module accepts:
/// either an absolute `scheme://host/path[?q]` or a bare
/// `/path[?q]`/relative path.
fn parse_url(u: &str) -> Option<ParsedUrl> {
    let (path_and_maybe_query, scheme, host) = if let Some(idx) = u.find("://") {
        // Absolute input: keep its scheme/host untouched.
        let scheme = u[..idx].to_string();
        let rest = &u[idx + 3..];
        let (rest, query) = split_query(rest);
        let (host, path) = match rest.find('/') {
            Some(slash) => (rest[..slash].to_string(), rest[slash..].to_string()),
            None => (rest.to_string(), String::new()),
        };
        (join_path_query(&path, query), Some(scheme), Some(host))
    } else {
        let (rest, query) = split_query(u);
        (join_path_query(rest, query), None, None)
    };
    let (path, query) = split_query(&path_and_maybe_query);
    Some(ParsedUrl {
        scheme,
        host,
        path: path.to_string(),
        query: query.map(str::to_string),
    })
}

fn split_query(s: &str) -> (&str, Option<&str>) {
    match s.find('?') {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    }
}

fn join_path_query(path: &str, query: Option<&str>) -> String {
    match query {
        Some(q) => format!("{path}?{q}"),
        None => path.to_string(),
    }
}

/// `path.Join(base, p)` for the clean, slash-prefixed inputs this module
/// is pinned on: join with exactly one slash, dropping nothing else.
fn path_join(base: &str, p: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.is_empty() {
        return p.to_string();
    }
    if p.is_empty() || p == "/" {
        return base.to_string();
    }
    if let Some(stripped) = p.strip_prefix('/') {
        format!("{base}/{stripped}")
    } else {
        format!("{base}/{p}")
    }
}

/// `url.Values.Encode()`: keys sorted (stable for repeated keys), values
/// query-escaped, `k=v` pairs joined with `&`.
fn encode_params(params: &[(&str, &str)]) -> String {
    let mut pairs: Vec<(&str, &str)> = params.to_vec();
    pairs.sort_by(|a, b| a.0.cmp(b.0));
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", query_escape(k), query_escape(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// `url.QueryEscape`: unreserved bytes pass through, space becomes `+`,
/// everything else `%XX` (uppercase hex, like Go).
fn query_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(b));
            }
            b' ' => out.push('+'),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// Builds a full URL for public-facing resources — the port of
/// `publicurl.PublicURL` (`publicurl.go:29-53`). Uses `share_url` as the
/// base when set (scheme, host and path prefix), otherwise falls back to
/// [`absolute_url`].
pub fn public_url(config: &PublicUrlConfig, u: &str, params: &[(&str, &str)]) -> String {
    if config.share_url.is_empty() {
        return absolute_url(config, u, params);
    }
    let Some(share) = parse_url(&config.share_url) else {
        return absolute_url(config, u, params);
    };
    let (Some(share_scheme), Some(share_host)) = (share.scheme.clone(), share.host.clone()) else {
        return absolute_url(config, u, params);
    };
    let Some(mut build) = parse_url(u) else {
        return absolute_url(config, u, params);
    };
    build.scheme = Some(share_scheme);
    build.host = Some(share_host);
    let base_path = share.path.trim_end_matches('/');
    if !base_path.is_empty() {
        build.path = path_join(base_path, &build.path);
    }
    if !params.is_empty() {
        build.query = Some(encode_params(params));
    }
    render_url(&build)
}

/// Builds an absolute URL from a relative path — the port of
/// `publicurl.AbsoluteURL` (`publicurl.go:56-85`). Address precedence:
/// `base_host`/`base_scheme` from config, then the request address, then
/// `localhost:{port}` with `https` only when TLS is fully configured.
/// Absolute inputs pass through unchanged, as in Go.
pub fn absolute_url(config: &PublicUrlConfig, u: &str, params: &[(&str, &str)]) -> String {
    let Some(mut build) = parse_url(u) else {
        // Go logs and returns "" for unparseable paths.
        return String::new();
    };
    if u.starts_with('/') {
        build.path = path_join(&config.base_path, &build.path);
        if !config.base_host.is_empty() {
            build.scheme = Some(if config.base_scheme.is_empty() {
                "http".to_string()
            } else {
                config.base_scheme.clone()
            });
            build.host = Some(config.base_host.clone());
        } else if let Some(addr) = &config.server_address {
            build.scheme = Some(addr.scheme.clone());
            build.host = Some(addr.host.clone());
        } else {
            build.scheme = Some(if config.tls_enabled() {
                "https".to_string()
            } else {
                "http".to_string()
            });
            build.host = Some(format!("localhost:{}", config.port));
        }
    }
    if !params.is_empty() {
        build.query = Some(encode_params(params));
    }
    render_url(&build)
}

/// Generates a public URL for artwork images — the port of
/// `publicurl.ImageURL` (`publicurl.go:18-26`), receiving the signed token
/// minted by `auth.CreatePublicToken` (see [`super::credentials`]).
/// `size > 0` adds a `size` query parameter.
pub fn image_url(config: &PublicUrlConfig, token: &str, size: i64) -> String {
    let uri = path_join(URL_PATH_PUBLIC_IMAGES, token);
    if size > 0 {
        public_url(config, &uri, &[("size", &size.to_string())])
    } else {
        public_url(config, &uri, &[])
    }
}

fn render_url(u: &ParsedUrl) -> String {
    let mut out = String::new();
    if let (Some(scheme), Some(host)) = (&u.scheme, &u.host) {
        out.push_str(scheme);
        out.push_str("://");
        out.push_str(host);
    }
    if !u.path.is_empty() {
        out.push_str(&u.path);
    } else if u.host.is_some() {
        out.push('/');
    }
    if let Some(query) = &u.query {
        out.push('?');
        out.push_str(query);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from publicurl_test.go — golden pins, same Arrange/Act/Assert.
    fn config() -> PublicUrlConfig {
        PublicUrlConfig::default()
    }

    #[test]
    fn public_url_uses_share_url_as_base() {
        // Arrange
        let mut cfg = config();
        cfg.share_url = "https://share.example.com".to_string();
        // Act
        let result = public_url(&cfg, "/path/to/resource", &[]);
        // Assert
        assert_eq!(result, "https://share.example.com/path/to/resource");
    }

    #[test]
    fn public_url_includes_query_parameters() {
        // Arrange
        let mut cfg = config();
        cfg.share_url = "https://share.example.com".to_string();
        let params = [("size", "300"), ("format", "png")];
        // Act
        let result = public_url(&cfg, "/image/123", &params);
        // Assert
        assert!(result.starts_with("https://share.example.com/image/123"));
        assert!(result.contains("size=300"));
        assert!(result.contains("format=png"));
    }

    #[test]
    fn public_url_prepends_share_url_path() {
        // Arrange
        let mut cfg = config();
        cfg.share_url = "https://example.com/navi".to_string();
        // Act
        let result = public_url(&cfg, "/share/img/hash", &[]);
        // Assert
        assert_eq!(result, "https://example.com/navi/share/img/hash");
    }

    #[test]
    fn public_url_prepends_share_url_path_with_params() {
        // Arrange
        let mut cfg = config();
        cfg.share_url = "https://example.com/navi".to_string();
        // Act
        let result = public_url(&cfg, "/share/img/hash", &[("size", "600")]);
        // Assert
        assert_eq!(result, "https://example.com/navi/share/img/hash?size=600");
    }

    #[test]
    fn public_url_handles_trailing_slash_in_share_url_path() {
        // Arrange
        let mut cfg = config();
        cfg.share_url = "https://example.com/navi/".to_string();
        // Act
        let result = public_url(&cfg, "/share/img/hash", &[]);
        // Assert
        assert_eq!(result, "https://example.com/navi/share/img/hash");
    }

    #[test]
    fn public_url_falls_back_to_absolute_url_with_request() {
        // Arrange
        let mut cfg = config();
        cfg.server_address = Some(ServerAddress {
            scheme: "https".to_string(),
            host: "myserver.com".to_string(),
        });
        // Act
        let result = public_url(&cfg, "/path/to/resource", &[]);
        // Assert
        assert_eq!(result, "https://myserver.com/path/to/resource");
    }

    #[test]
    fn public_url_falls_back_to_localhost_without_request() {
        // Arrange
        let mut cfg = config();
        cfg.port = 4533;
        // Act
        let result = public_url(&cfg, "/path/to/resource", &[]);
        // Assert
        assert_eq!(result, "http://localhost:4533/path/to/resource");
    }

    #[test]
    fn absolute_url_uses_base_host_and_scheme() {
        // Arrange
        let mut cfg = config();
        cfg.base_host = "configured.example.com".to_string();
        cfg.base_scheme = "https".to_string();
        // Act
        let result = absolute_url(&cfg, "/path/to/resource", &[]);
        // Assert
        assert_eq!(result, "https://configured.example.com/path/to/resource");
    }

    #[test]
    fn absolute_url_defaults_to_http_when_base_scheme_empty() {
        // Arrange
        let mut cfg = config();
        cfg.base_host = "configured.example.com".to_string();
        // Act
        let result = absolute_url(&cfg, "/path/to/resource", &[]);
        // Assert
        assert_eq!(result, "http://configured.example.com/path/to/resource");
    }

    #[test]
    fn absolute_url_extracts_host_from_request() {
        // Arrange
        let mut cfg = config();
        cfg.server_address = Some(ServerAddress {
            scheme: "https".to_string(),
            host: "request.example.com".to_string(),
        });
        // Act
        let result = absolute_url(&cfg, "/path/to/resource", &[]);
        // Assert
        assert_eq!(result, "https://request.example.com/path/to/resource");
    }

    #[test]
    fn absolute_url_falls_back_to_localhost_on_port_without_request() {
        // Arrange
        let mut cfg = config();
        cfg.port = 8080;
        // Act
        let result = absolute_url(&cfg, "/path/to/resource", &[]);
        // Assert
        assert_eq!(result, "http://localhost:8080/path/to/resource");
    }

    #[test]
    fn absolute_url_uses_https_in_fallback_when_tls_configured() {
        // Arrange
        let mut cfg = config();
        cfg.port = 4533;
        cfg.tls_cert = "cert.pem".to_string();
        cfg.tls_key = "key.pem".to_string();
        // Act
        let result = absolute_url(&cfg, "/path/to/resource", &[]);
        // Assert
        assert_eq!(result, "https://localhost:4533/path/to/resource");
    }

    #[test]
    fn absolute_url_stays_on_http_when_only_cert_configured() {
        // Arrange
        let mut cfg = config();
        cfg.port = 4533;
        cfg.tls_cert = "cert.pem".to_string();
        // Act
        let result = absolute_url(&cfg, "/path/to/resource", &[]);
        // Assert
        assert_eq!(result, "http://localhost:4533/path/to/resource");
    }

    #[test]
    fn absolute_url_prepends_base_path() {
        // Arrange
        let mut cfg = config();
        cfg.base_path = "/navidrome".to_string();
        cfg.base_host = "example.com".to_string();
        cfg.base_scheme = "https".to_string();
        // Act
        let result = absolute_url(&cfg, "/path/to/resource", &[]);
        // Assert
        assert_eq!(result, "https://example.com/navidrome/path/to/resource");
    }

    #[test]
    fn absolute_url_passes_through_absolute_urls() {
        // Arrange
        let cfg = config();
        // Act
        let result = absolute_url(&cfg, "https://other.example.com/path", &[]);
        // Assert
        assert_eq!(result, "https://other.example.com/path");
    }

    #[test]
    fn absolute_url_includes_query_parameters() {
        // Arrange
        let mut cfg = config();
        cfg.base_host = "example.com".to_string();
        cfg.base_scheme = "https".to_string();
        // Act
        let result = absolute_url(&cfg, "/path", &[("key", "value")]);
        // Assert
        assert_eq!(result, "https://example.com/path?key=value");
    }

    #[test]
    fn image_url_generates_url_with_artwork_token() {
        // Arrange
        let mut cfg = config();
        cfg.share_url = "https://share.example.com".to_string();
        // Act
        let result = image_url(&cfg, "sometoken", 0);
        // Assert
        assert!(result.starts_with("https://share.example.com/share/img/"));
    }

    #[test]
    fn image_url_includes_size_parameter_when_provided() {
        // Arrange
        let mut cfg = config();
        cfg.share_url = "https://share.example.com".to_string();
        // Act
        let result = image_url(&cfg, "sometoken", 300);
        // Assert
        assert!(result.contains("size=300"));
    }

    #[test]
    fn image_url_omits_size_parameter_when_zero() {
        // Arrange
        let mut cfg = config();
        cfg.share_url = "https://share.example.com".to_string();
        // Act
        let result = image_url(&cfg, "sometoken", 0);
        // Assert
        assert!(!result.contains("size="));
    }

    #[test]
    fn image_url_uses_address_of_triggering_request() {
        // Arrange
        let mut cfg = config();
        cfg.server_address = Some(ServerAddress {
            scheme: "https".to_string(),
            host: "music.example.com".to_string(),
        });
        // Act
        let result = image_url(&cfg, "sometoken", 300);
        // Assert
        assert!(result.starts_with("https://music.example.com/share/img/"));
        assert!(result.contains("size=300"));
    }

    #[test]
    fn image_url_prefers_share_url_over_context_address() {
        // Arrange
        let mut cfg = config();
        cfg.share_url = "https://share.example.com".to_string();
        cfg.server_address = Some(ServerAddress {
            scheme: "https".to_string(),
            host: "music.example.com".to_string(),
        });
        // Act
        let result = image_url(&cfg, "sometoken", 0);
        // Assert
        assert!(result.starts_with("https://share.example.com/share/img/"));
    }

    #[test]
    fn image_url_falls_back_to_localhost_when_no_address() {
        // Arrange
        let mut cfg = config();
        cfg.port = 4533;
        // Act
        let result = image_url(&cfg, "sometoken", 0);
        // Assert
        assert!(result.starts_with("http://localhost:4533/share/img/"));
    }
}
