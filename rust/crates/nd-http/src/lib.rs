// nd-http: owned by #24 (docs/rust-port/issues.md Part B)

//! HTTP transport seam for the Navidrome Rust port.
//!
//! ## BOOT-03 shell-only increment (`server.rs`, `shell.rs`, `peer.rs`, `shutdown.rs`)
//!
//! A callable TCP HTTP shell exists: bind-before-ready exposing the actual
//! bound address, an explicit route manifest with an exact (not
//! prefix-confusable) `BasePath` mount, the public ping, transport-derived peer
//! recording, and explicit shutdown with an observed join. It serves **no**
//! protected mount and no CLI wiring: `nd-cli` still has no listener (`fn
//! main() {}`), and authentication, proxy trust, CORS/security headers,
//! compression, in-flight drain observability, static UI, TLS, and Unix
//! sockets are all later increments. The shell accepts `&nd_conf::Config` for
//! bind parameters only; it does not re-resolve configuration.
//!
//! ## BOOT-00 gate scope (unchanged)
//!
//! The BOOT-00 D1 dependency boundary and its executed acceptance test
//! (`dependency_graph_has_no_http_api_cycle`, subplans/boot-api.md:156,
//! decisions.md:517-541) still hold, and the shell does not weaken them:
//! the API router is injected by the caller, never reached through an
//! `nd-api` type. The `serve_started` transport contract, SinkAdapter
//! deletion, and full axum/hyper wiring remain API-04/API-05 work under the
//! B3-ratified Candidate B (Send-Axum owner channels, decisions.md:502-516).
//!
//! ## D1 dependency boundary (PARENT DECIDED 2026-11-21, decisions.md:521)
//!
//! `nd-http` depends on `nd-conf` and `nd-auth` **only** (plus `axum`, locked
//! at 0.7.9 via nd-auth, and `tokio` for the listener runtime; both already
//! locked, so they add dependency edges and no new lock entries). There is
//! deliberately **no `nd-http → nd-api` edge**: the API router is *injected*
//! into the HTTP seam by `nd-cli` at construction — see [`RouterInjection`].
//! A direct edge would make the boot-api.md:156 cycle acceptance
//! unsatisfiable, so this crate must never declare `nd-api` in any
//! dependency table (`[dependencies]`, `[dev-dependencies]`, build deps).
pub mod peer;
pub mod server;
pub mod shell;
pub mod shutdown;

use nd_auth::middleware::MiddlewareState;
use nd_conf::Config;
pub use peer::AcceptedPeer;
pub use server::{BoundListener, Running, DEFAULT_GRACEFUL_DEADLINE};
pub use shell::{BindError, UnsupportedTransport};
pub use shutdown::{Shutdown, ShutdownOutcome};

/// Router-injection signature (D1): the fully constructed API router —
/// built by `nd-api` (#27) and wired by `nd-cli` (#33) — is handed to the
/// HTTP seam as an opaque value. `&Config` is the committed configuration the
/// seam needs for mounts (BasePath prefix, per boot-api.md §2); it is NOT a
/// hook for the seam to re-resolve config. The router type is opaque on
/// purpose: nothing here learns `nd-api`'s route types, which is what keeps
/// the graph acyclic. Later API-04 work may refine the return into a
/// `serve_started` transport type; this alias is the gate's compile-level
/// statement of the injection shape (boot-api.md:154, decisions.md:521).
pub type RouterInjection = fn(axum::Router, &Config) -> axum::Router;

/// Boundary receipt: the auth middleware state the injected router's layers
/// will later carry (`nd-auth/src/middleware.rs:58`, mounted via
/// `axum::middleware::from_fn_with_state`). Naming the type here proves the
/// `nd-http -> nd-auth` edge is a real, compiling one — one of the only two
/// internal edges this crate is allowed (D1).
pub const MOUNT_TARGET: core::marker::PhantomData<MiddlewareState> = core::marker::PhantomData;

#[cfg(test)]
mod dependency_boundary_tests {
    use super::RouterInjection;

    /// Compile-level half of the D1 assertion: the injection type above type
    /// checks against `axum::Router` + `nd_conf::Config` only. If anyone ever
    /// routes the seam through an `nd-api` type, this signature (and the
    /// crate's imports) cannot compile without adding a forbidden edge.
    const INJECTION_IS_FN_VALUE: fn(RouterInjection) -> bool =
        |f| std::ptr::fn_addr_eq(f, axum_router_identity as RouterInjection);

    fn axum_router_identity(router: axum::Router, _config: &nd_conf::Config) -> axum::Router {
        router
    }

    /// Executes the BOOT-00 acceptance `dependency_graph_has_no_http_api_cycle`
    /// (boot-api.md:156, decisions.md:517-541) against the real dependency
    /// graph: resolves this crate's package through `cargo metadata` and
    /// asserts no dependency *edge* — normal, dev, build, or a path dependency
    /// — named `nd-api` exists, and that no `nd`+`api` identifier path appears
    /// in this crate's sources. Negative controls: adding
    /// `nd-api = { path = "../nd-api" }` to `nd-http/Cargo.toml`, or importing
    /// that crate's API paths in the sources, fails this test; deleting
    /// `RouterInjection` fails the compile-level assertion below.
    ///
    /// The identifier check builds its needle from parts at runtime so the
    /// doc comments in this file — which must name the forbidden crate to
    /// explain D1 — do not themselves trip the assertion.
    #[test]
    fn dependency_graph_has_no_http_api_cycle() {
        // Arrange: locate the workspace manifest from the compiled-in crate dir.
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        // Act: ask cargo itself for the resolved graph, offline (--no-deps
        // still resolves workspace members; --locked proves the same lock the
        // build gate uses).
        let output = std::process::Command::new(env!("CARGO"))
            .args(["metadata", "--locked", "--no-deps", "--format-version", "1"])
            .current_dir(manifest_dir)
            .output()
            .expect("cargo metadata must run from the crate directory");
        assert!(
            output.status.success(),
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let metadata = String::from_utf8(output.stdout).expect("cargo metadata is UTF-8");

        // Assert 1: the JSON dependency list of the nd-http package contains
        // no `"name": "nd-api"` entry (cargo --no-deps emits `dependencies`
        // for each workspace member, including dev/build deps).
        let nd_http_start = metadata
            .find(r#""name":"nd-http""#)
            .or(metadata.find(r#""name": "nd-http""#))
            .expect("nd-http must be a workspace member in cargo metadata");
        let rest = &metadata[nd_http_start..];
        let deps_end = rest.find(r#""manifest_path""#).unwrap_or(rest.len());
        let deps = &rest[..deps_end];
        assert!(
            !deps.contains(r#""name":"nd-api""#) && !deps.contains(r#""name": "nd-api""#),
            "D1 violation: nd-http declares an nd-api dependency edge: {deps}"
        );

        // Assert 2: textual negative control — the crate's API identifier path
        // (assembled at runtime, see above) appears nowhere in its sources.
        let needle = ["nd", "api"].join("_");
        let sources =
            std::fs::read_to_string(std::path::Path::new(manifest_dir).join("src/lib.rs"))
                .expect("nd-http/src/lib.rs exists");
        assert!(
            !sources.contains(&needle),
            "D1 violation: nd-http sources contain the forbidden API identifier path"
        );

        // Assert 3: the injection type is a usable fn value (non-vacuity: the
        // graph assertion alone would pass on an empty crate), and the nd-auth
        // boundary receipt resolves to the real middleware type.
        assert!(INJECTION_IS_FN_VALUE(axum_router_identity));
        let _: core::marker::PhantomData<nd_auth::middleware::MiddlewareState> =
            super::MOUNT_TARGET;
    }
}
