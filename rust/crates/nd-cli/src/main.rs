// nd-cli: owned by #33 (docs/rust-port/issues.md Part B)

//! Service entrypoint: the typed service structs built by the other crates get wired here.
//! (Rust replaces Go's DI codegen — cmd/wire_gen.go, cmd/wire_injectors.go — with explicit
//! construction, decided once in #S1.)
//!
//! ## BOOT-00 context (construction/dependency/launch GATE only)
//!
//! This crate is the D1 wiring point (decisions.md:521): it constructs the
//! services and *injects* `nd-api`'s router into `nd-http` through
//! [`nd_http::RouterInjection`], which is what keeps the graph free of an
//! `nd-http → nd-api` edge. D3 has repaired the workspace manifest pins
//! (`chrono 0.4`, `tracing 0.1`, `tracing-subscriber 0.3`, `base64 0.22`);
//! D4 is ratified as ordinary multi-thread Send-Axum (decisions.md:502-516).
//! The injection type itself lives in `nd_http::RouterInjection` (nd-cli does
//! not depend on nd-http yet, so it is named, not linked).
//!
//! ## Config resolution gap this crate must close (BOOT-01-owned, NOT here)
//!
//! * `nd_conf::Config::load(None)` does **not** consult `ND_CONFIGFILE` — it
//!   only applies the path it is given plus `ND_*` env pairs
//!   (`nd-conf/src/config.rs:946-986`). Go resolves flag → `ND_CONFIGFILE`
//!   before loading (`conf/configuration.go:1198-1206`; the Rust gap is noted
//!   at `nd-conf/src/config.rs:390`). The CLI must resolve flag →
//!   `ND_CONFIGFILE` → default itself and pass the resolved `Some(path)`.
//! * `nd_conf::Dir::path()` calls `create_dir_all` and sets permissions
//!   (`nd-conf/src/dir.rs:56-70`). It is correct for data/cache/backup dirs
//!   and **must never** be applied to the configured music root, which has to
//!   exist and is never auto-created (boot-api.md §1 Config row, §3 BOOT-01
//!   step 2).
//!
//! No behavior is claimed: BOOT-00 leaves `main` empty and BOOT-01 owns the
//! resolution/validation/bootstrap sequence (boot-api.md §3 BOOT-01).

/// Scaffold stage: nothing is wired yet, so the binary exits 0.
fn main() {}
