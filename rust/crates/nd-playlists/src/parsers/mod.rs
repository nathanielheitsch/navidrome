//! #10 — Playlist parsers (port of `core/playlists/{parse_m3u,parse_nsp,import}.go`,
//! the parser half; repo wiring lands in #23).
//!
//! * [`m3u`] — M3U line parsing, path resolution (`pathResolver`), `#PLAYLIST`
//!   and `#EXTALBUMARTURL` extended directives.
//! * [`nsp`] — SmartPlaylist JSON parsing with comment stripping and
//!   line/column syntax-error reporting.
//! * [`import`] — import semantics: extension dispatch, synced-playlist
//!   creation, content fingerprints.

pub mod import;
pub mod m3u;
pub mod nsp;
