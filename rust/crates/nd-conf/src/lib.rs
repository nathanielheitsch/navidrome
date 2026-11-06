//! `nd-conf` — Rust port of `conf/**` and the `consts/consts.go` defaults
//! (issue #03, docs/rust-port/issues.md Part B), honoring the frozen
//! fresh-service config contract (docs/rust-port/config-contract.md).
//!
//! Layout:
//! * [`Config`] — the typed config struct (`configOptions`) plus its nested
//!   sections, loaded from TOML with the `ND_*` env precedence ladder;
//! * [`keys`] — the fresh key set registry (types + defaults) used for the
//!   case-insensitive acceptance check and unknown-key fatality;
//! * [`consts`] — the migrated defaults table;
//! * [`Dir`], [`ByteSizeField`], [`DurationField`] — the value types the
//!   contract pins (lazy MkdirAll, byte sizes, Go durations);
//! * crontab validation compatible with `scheduler.ParseCrontab`.

pub mod bytes;
pub mod config;
pub mod consts;
pub mod crontab;
pub mod dir;
pub mod duration;
pub mod keys;

pub use config::{
    parse_bytes, BackupConfig, Config, ConfigError, DeezerConfig, ExtAuthConfig, HttpHeadersConfig,
    InspectConfig, JellyfinConfig, LastFmConfig, ListenBrainzConfig, MatcherConfig, PidConfig,
    PrometheusConfig, ScannerConfig, SearchConfig, TagConf, TranscodingConfig,
};
pub use dir::Dir;
