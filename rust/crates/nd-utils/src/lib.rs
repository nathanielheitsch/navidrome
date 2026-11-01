//! #01 — Rust port of the Go `utils/**` primitives (see `docs/rust-port/issues.md` Part B).
//!
//! Pure modules: [`slice`], [`str`], [`hasher`], [`natural`], [`shellquote`],
//! [`nanoid`], [`number`], [`gg`], [`chrono`], [`jsoncommentstrip`],
//! [`index_group_parser`], [`time`].
//!
//! Effectful modules: [`files`], [`gravatar`], [`random`], [`ioutils`],
//! [`run`], [`cache`], [`netguard`], [`limiter`], [`context`], [`pl`].
//!
//! Deliberately **not** ported (documented decisions, not omissions):
//! - `utils/encrypt` — the AES-GCM reversible store is dead per
//!   `docs/rust-port/credentials.md`; passwords are argon2id from day one.
//! - `utils/singleton` — replaced by explicit typed construction per the #S1
//!   design freeze in `rust/Cargo.toml`.
//! - `utils/req`, `utils/httpclient` — HTTP-server/`http.Request` query-parameter
//!   helpers; the typed request layer is owned by the API issues (#18/#19), not
//!   by a utils crate. `utils/merge` — `fs.FS` overlaying is a Go-stdlib idiom
//!   without a stable Rust counterpart; re-own it at the consumer issue if
//!   needed. `utils/cache/spread_fs` + `file_caches` — depends on the
//!   `djherbis/fscache` layout and `conf` wiring; owned by the cache consumer.
//!
//! Byte-exactness contract (HIGH): [`hasher`] backs the `SEEDEDRAND` SQLite
//! function (`db/db.go:46`) and [`natural`] backs `NATSORT` (`db/db.go:49`).
//! Their seed/round structure and `CompareFold` semantics are ported
//! byte-for-byte, not approximately.

pub mod cache;
pub mod chrono;
pub mod context;
pub mod files;
pub mod gg;
pub mod gravatar;
pub mod hasher;
pub mod index_group_parser;
pub mod ioutils;
pub mod jsoncommentstrip;
pub mod limiter;
pub mod nanoid;
pub mod natural;
pub mod netguard;
pub mod number;
pub mod pl;
pub mod random;
pub mod run;
pub mod shellquote;
pub mod slice;
pub mod str;
pub mod time;
