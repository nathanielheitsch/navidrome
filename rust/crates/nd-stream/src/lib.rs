//! `nd-stream` — Rust port of `core/stream/**` (issue #14 decision tables,
//! with #15's serving half still deferred; docs/rust-port/issues.md Part B).
//!
//! This slice (#14) is the **decision layer only**: which transcode (format/
//! bitrate/maxRate) and whether to transcode at all. It is pure — no HTTP, no
//! process launching, no DB I/O. The serving half (`media_streamer.go`,
//! `limiter.go`, `archiver.go`) stays in #15.
//!
//! Divergences from Go frozen here:
//!   - Go's ambient `context.Context` (`request.PlayerFrom`/`request.TranscodingFrom`)
//!     becomes explicit parameters ([`DecisionOptions`] in `types.rs`).
//!   - Go's `ds.Transcoding().FindByFormat` DB lookup becomes an explicit read-only
//!     table ([`Transcoding`] in `decider.rs`) so the decision layer stays
//!     side-effect free.
//!   - Go's `time.Time` is flattened to epoch **seconds** (`i64`) everywhere the
//!     decision layer touches it; the token half truncates to seconds exactly as
//!     Go's `Truncate(time.Second)` does.
//!   - Go's probe verbs (`IsProbeAvailable`/`ensureProbed`/ffprobe launch,
//!     `decider.go:48-58,431-459`) live in nd-ffmpeg (#07). The decision layer
//!     keeps the same branch table but receives the probe verdict as
//!     [`ProbeInput`], so Go's probe-failure error arm cannot fire here and
//!     `make_decision` is infallible in the port.
//!
//! The serving half (#15) is [`range`] plus [`direct`]: the conditional/Range
//! matrix Go delegates to `http.ServeContent` (`media_streamer.go:160-162`) and
//! the bounded direct-file body behind it. [`range`] is pure — metadata plus
//! request in, headers plus byte window out — so the whole matrix is testable
//! without a server. [`direct`] is the only I/O in that path and takes an
//! **already-authorized** path; see the trust boundary in `direct.rs`.
//! Still open in #15: the transcoding-file cache eviction contract
//! (`utils/cache`), the archiver (`core/archiver.go`), and the HTTP handler that
//! ties them together. The transcode limiter (`limiter.go`) is now **enforced**
//! through [`transcode_body::open_transcode_gated`], and the process-backed
//! transcode body (`media_streamer.go:101-133`) is ported in [`transcode_body`].

pub mod aliases;
pub mod codec;
pub mod decider;
pub mod direct;
pub mod legacy_client;
pub mod limitations;
pub mod limiter;
pub mod probe_adapter;
pub mod probe_json;
pub mod range;
pub mod token;
pub mod transcode_body;
pub mod types;

/// Scaffold marker kept for the #S1 instantiate contract: the owning issue
/// number this crate exposes.
pub const OWNED_BY_ISSUE: u8 = 14;

pub use aliases::{
    codec_alias_groups, container_alias_groups, contains_ignore_case, is_aac_codec, matches_codec,
    matches_container, matches_with_aliases,
};
pub use codec::{
    codec_fixed_output_sample_rate, codec_max_channels, codec_max_sample_rate, is_lossless_format,
    normalize_probe_codec, normalize_source_sample_rate, target_bit_depth,
};
pub use decider::{build_source_stream, parse_probe_data, resolve_target_format};
pub use decider::{
    new_transcode_decider, ProbeInput, TranscodeDecider, Transcoding, FALLBACK_BITRATE,
};
pub use direct::{
    meta_from_metadata, normalize_etag, open_authorized, plan_and_open, RangeBody, MAX_CHUNK_BYTES,
};
pub use legacy_client::{
    apply_server_override, build_legacy_client_info, resolve_client_request, resolve_request,
};
pub use limitations::{apply_limitation, check_limitations, AdjustResult};
pub use probe_json::{probe_bytes, probe_json};
pub use range::{
    check_if_match, check_if_modified_since, check_if_none_match, check_if_range,
    check_if_unmodified_since, etag_strong_match, etag_weak_match, http_date, parse_http_time,
    parse_range, plan, plan_streaming, preconditions, scan_etag, trim_http_ws, ByteRange,
    CondRequest, CondResult, FileMeta, PrecondOutcome, RangeOutcome, ServePlan,
    ACCEPT_RANGES_BYTES, ACCEPT_RANGES_NONE, MAX_CONTENT_RANGES,
};
pub use token::{
    create_transcode_params, params_from_token, parse_transcode_params, resolve_request_from_token,
    TranscodeParams, TOKEN_TTL_SECS,
};
pub use types::{
    ClientInfo, CodecProfile, DecisionOptions, DirectPlayProfile, ErrTokenInvalid, ErrTokenStale,
    Limitation, MediaFile, Profile, Request, ServerTranscoding, StreamDetails, TranscodeDecision,
    TranscodeOptions,
};

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn nd_stream_instantiates() {
        // Arrange: the owning-issue number this crate must expose.
        let expected: u8 = 14;
        // Act: read the crate's own scaffold marker.
        let actual: u8 = OWNED_BY_ISSUE;
        // Assert
        assert_eq!(
            actual, expected,
            "nd-stream instantiates without pulling in the world"
        );
    }
}
