// nd-api: owned by #27 (docs/rust-port/issues.md Part B)

//! Scaffold stage for nd-api: this crate instantiates empty so later issues
//! (#27) can fill it without pulling the world in (#S1 test contract).
//!
//! ## BOOT-00 D1 boundary (decisions.md:521; no behavior claimed here)
//!
//! * The router this crate will eventually build is **injected** into the HTTP
//!   seam by `nd-cli` — the signature is `nd_http::RouterInjection`
//!   (`fn(axum::Router, &nd_conf::Config) -> axum::Router`). There is
//!   deliberately **no `nd-api → nd-http` and no `nd-http → nd-api` edge**: that
//!   is the only way the `dependency_graph_has_no_http_api_cycle` acceptance
//!   (boot-api.md:156) can hold, and it is executed as a test in nd-http.
//! * Route contracts (BasePath, canonical paths, error classes, credential
//!   transports) are frozen by API-00 **before** any handler exists
//!   (boot-api.md §2/§3); axum/hyper wiring is API-04/API-05 under the
//!   B3-ratified Candidate B transport (decisions.md:502-516).
//! * This crate keeps zero dependencies at BOOT-00. API-00+ may add workspace
//!   path deps (`nd-db`, `nd-auth`, `nd-stream`, `nd-conf`, `nd-id`) and the
//!   already-locked `axum`/`hyper-util`/`http-body-util`/`bytes` — each still
//!   needs its own D2 check that it adds no new `Cargo.lock` entry.

/// Scaffold marker: the public surface authored by #27 replaces this constant.
pub const OWNED_BY_ISSUE: u8 = 27;

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn nd_api_instantiates() {
        // Arrange: the owning-issue number this crate must expose.
        let expected: u8 = 27;
        // Act: read the crate's own scaffold marker.
        let actual: u8 = OWNED_BY_ISSUE;
        // Assert
        assert_eq!(
            actual, expected,
            "nd-api instantiates without pulling in the world"
        );
    }
}
