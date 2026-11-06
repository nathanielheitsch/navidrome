//! Port of `core/stream/legacy_client.go` (136 LOC): the legacy
//! Subsonic-parameter resolver — `buildLegacyClientInfo`,
//! `ResolveRequest`, `ResolveClientRequest`, and the private `resolve`
//! mapper (`legacy_client.go:105-134`), plus `applyServerOverride`
//! (`decider.go:157-179`, ported here because that is its only consumer).
//!
//! Divergences (frozen in `lib.rs`):
//!   - Go's ambient `context.Context` values (`request.PlayerFrom` /
//!     `request.TranscodingFrom`) become the explicit [`DecisionOptions`]
//!     parameter (`decider.go:157-161`, `legacy_client.go:71,109`).
//!   - Go reads `conf.Server.DefaultDownsamplingFormat` from the ambient
//!     config; the port takes it as an explicit parameter
//!     (`default_downsampling_format`) so tests can drive both the empty and
//!     non-empty arms (`legacy_client_test.go:77,100,127,151`).
//!   - Go's `log.Warn/Error/Debug` calls are omitted; the branch structure
//!     they annotate is preserved.
//!   - The decider is infallible in the port (probe verdict is explicit
//!     [`ProbeInput`], see `lib.rs`), so Go's MakeDecision-error arm at
//!     `legacy_client.go:117-121` becomes the [`ProbeInput::NotAvailable`]
//!     fallback-to-raw path.

use crate::decider::{ProbeInput, TranscodeDecider};
use crate::types::{
    cap_bitrate, ClientInfo, DecisionOptions, DirectPlayProfile, MediaFile, Profile, Request,
    ServerTranscoding, TranscodeOptions, PROTOCOL_HTTP,
};

/// `buildLegacyClientInfo` (`legacy_client.go:15-56`): translates legacy
/// Subsonic stream/download parameters into a [`ClientInfo`] for use with
/// [`make_decision`]. `player_max_bit_rate` is Go's `request.PlayerFrom(ctx)`
/// player row (`legacy_client.go:71`); `default_downsampling_format` is Go's
/// `conf.Server.DefaultDownsamplingFormat`.
#[must_use]
pub fn build_legacy_client_info(
    mf: &MediaFile,
    req_format: &str,
    req_bit_rate: i64,
    player_max_bit_rate: i64,
    default_downsampling_format: &str,
) -> ClientInfo {
    let mut ci = ClientInfo {
        name: "legacy".to_string(),
        ..ClientInfo::default()
    };

    // Determine target format for transcoding (`legacy_client.go:19-29`).
    let mut target_format = String::new();
    if !req_format.is_empty() {
        target_format = req_format.to_string();
    } else if req_bit_rate > 0
        && req_bit_rate < mf.bit_rate
        && !default_downsampling_format.is_empty()
    {
        target_format = default_downsampling_format.to_string();
    } else if player_max_bit_rate > 0
        && player_max_bit_rate < mf.bit_rate
        && !default_downsampling_format.is_empty()
    {
        // Server-side player MaxBitRate alone forces downsampling, even when
        // the client sent no format/bitrate params (`legacy_client.go:24-27`,
        // issue #5583, legacy /stream path).
        target_format = default_downsampling_format.to_string();
    }

    if !target_format.is_empty() {
        // Add a direct play profile for the source format when no explicit
        // format was requested (bitrate-only downsampling) or when the
        // requested format matches the source (`legacy_client.go:32-38`).
        if req_format.is_empty() || req_format.eq_ignore_ascii_case(mf.suffix.as_str()) {
            ci.direct_play_profiles = vec![DirectPlayProfile {
                containers: vec![mf.suffix.clone()],
                audio_codecs: vec![mf.audio_codec()],
                protocols: vec![PROTOCOL_HTTP.to_string()],
                ..DirectPlayProfile::default()
            }];
        }
        ci.transcoding_profiles = vec![Profile {
            container: target_format.clone(),
            audio_codec: target_format,
            protocol: PROTOCOL_HTTP.to_string(),
            ..Profile::default()
        }];
        if req_bit_rate > 0 {
            ci.max_audio_bitrate = req_bit_rate;
            ci.max_transcoding_audio_bitrate = req_bit_rate;
        }
    } else {
        // No transcoding requested — direct play everything
        // (`legacy_client.go:50-55`).
        ci.direct_play_profiles = vec![DirectPlayProfile {
            protocols: vec![PROTOCOL_HTTP.to_string()],
            ..DirectPlayProfile::default()
        }];
    }

    ci
}

/// `applyServerOverride` (`decider.go:157-179`): replaces the client's
/// profiles with the forced transcoding target and its bitrate caps. Go's
/// player MaxBitRate precedence (`decider.go:159-161`) is pre-flattened by
/// the caller into `opts.player_max_bit_rate` — the port applies it here with
/// the same "player wins when > 0" rule.
#[must_use]
pub fn apply_server_override(
    original: &ClientInfo,
    trc: &ServerTranscoding,
    opts: &DecisionOptions,
) -> ClientInfo {
    let mut max_bit_rate = trc.default_bit_rate;
    if opts.player_max_bit_rate > 0 {
        max_bit_rate = opts.player_max_bit_rate;
    }
    ClientInfo {
        name: original.name.clone(),
        platform: original.platform.clone(),
        max_audio_bitrate: max_bit_rate,
        max_transcoding_audio_bitrate: max_bit_rate,
        direct_play_profiles: vec![DirectPlayProfile {
            containers: vec![trc.target_format.clone()],
            audio_codecs: vec![trc.target_format.clone()],
            protocols: vec![PROTOCOL_HTTP.to_string()],
            ..DirectPlayProfile::default()
        }],
        transcoding_profiles: vec![Profile {
            container: trc.target_format.clone(),
            audio_codec: trc.target_format.clone(),
            protocol: PROTOCOL_HTTP.to_string(),
            ..Profile::default()
        }],
        ..ClientInfo::default()
    }
}

/// `ResolveRequest` (`legacy_client.go:61-92`): uses [`make_decision`] to
/// resolve legacy Subsonic stream parameters into a fully specified
/// [`Request`]. Recursive fallback to `default_downsampling_format` when the
/// requested format has no compatible profile (`legacy_client.go:81-89`).
pub fn resolve_request(
    decider: &TranscodeDecider,
    mf: &MediaFile,
    req_format: &str,
    req_bit_rate: i64,
    offset: i64,
    opts: &DecisionOptions,
    default_downsampling_format: &str,
) -> Request {
    let mut req = Request {
        offset,
        ..Request::default()
    };

    if req_format == "raw" {
        req.format = "raw".to_string();
        return req;
    }

    let client_info = build_legacy_client_info(
        mf,
        req_format,
        req_bit_rate,
        opts.player_max_bit_rate,
        default_downsampling_format,
    );
    if let Some(resolved) = resolve(decider, mf, &client_info, offset, opts) {
        return resolved;
    }

    // No compatible profile for the requested format — retry with
    // DefaultDownsamplingFormat (`legacy_client.go:81-89`).
    if !req_format.is_empty()
        && !default_downsampling_format.is_empty()
        && !req_format.eq_ignore_ascii_case(default_downsampling_format)
    {
        return resolve_request(
            decider,
            mf,
            default_downsampling_format,
            req_bit_rate,
            offset,
            opts,
            default_downsampling_format,
        );
    }

    // Ultimate fallback — raw (`legacy_client.go:91-92`).
    req.format = "raw".to_string();
    req
}

/// `ResolveClientRequest` (`legacy_client.go:96-101`): resolves a stream
/// request for a client that declared its own direct play and transcoding
/// profiles, falling back to raw when none fits.
pub fn resolve_client_request(
    decider: &TranscodeDecider,
    mf: &MediaFile,
    client_info: &ClientInfo,
    offset: i64,
    opts: &DecisionOptions,
) -> Request {
    match resolve(decider, mf, client_info, offset, opts) {
        Some(req) => req,
        None => Request {
            format: "raw".to_string(),
            offset,
            ..Request::default()
        },
    }
}

/// `resolve` (`legacy_client.go:105-134`): applies the server-side player
/// overrides to clientInfo and maps the decision to a Request. `None` when no
/// profile fits.
fn resolve(
    decider: &TranscodeDecider,
    mf: &MediaFile,
    client_info: &ClientInfo,
    offset: i64,
    opts: &DecisionOptions,
) -> Option<Request> {
    let mut req = Request {
        offset,
        ..Request::default()
    };
    // `legacy_client.go:107-114`: server override takes precedence over the
    // plain player cap.
    let effective: ClientInfo = match &opts.transcoding {
        Some(trc) if !trc.target_format.is_empty() => apply_server_override(client_info, trc, opts),
        _ if opts.player_max_bit_rate > 0 => {
            let mut modified = client_info.clone();
            if cap_bitrate(&mut modified, opts.player_max_bit_rate) {
                modified
            } else {
                client_info.clone()
            }
        }
        _ => client_info.clone(),
    };

    // `legacy_client.go:116-122`: the port's decider is infallible; the probe
    // verdict is supplied explicitly. Go's MakeDecision-error arm falls back
    // to raw — the NotAvailable probe state is the port's equivalent.
    let decision = decider.make_decision(
        mf,
        effective,
        TranscodeOptions { skip_probe: true },
        ProbeInput::NotAvailable,
    );
    let _ = &mut req;
    if decision.can_direct_play {
        req.format = "raw".to_string();
        Some(req)
    } else if decision.can_transcode {
        req.format = decision.target_format;
        req.bit_rate = decision.target_bitrate;
        req.sample_rate = decision.target_sample_rate;
        req.bit_depth = decision.target_bit_depth;
        req.channels = decision.target_channels;
        Some(req)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decider::{new_transcode_decider, parse_probe_data, Transcoding};

    /// The Go oracle's mock transcoding table
    /// (`tests/mock_transcoding_repo.go`, mirrored from `decider.rs` tests).
    fn mock_transcoding_rows() -> Vec<Transcoding> {
        let mk = |target: &str, bitrate: i64, command: &str| Transcoding {
            target_format: target.to_string(),
            default_bit_rate: bitrate,
            command: command.to_string(),
        };
        let flac_cmd = "ffmpeg -ss %t -i %s -map 0:a:0 -v 0 -c:a flac -f flac -".to_string();
        vec![
            mk("mp3", 160, ""),
            mk("oga", 128, ""),
            mk("opus", 96, ""),
            mk("flac", 0, flac_cmd.as_str()),
            mk("aac", 256, ""),
        ]
    }

    fn svc() -> TranscodeDecider {
        new_transcode_decider(mock_transcoding_rows())
    }

    /// `withProbe` (`decider_test.go:22-32`): pre-populate ProbeData from the
    /// media file's own fields so the probe step short-circuits. The port's
    /// decider reads the probe verdict from `probe_data`, so writing the JSON
    /// here reproduces the Go helper exactly.
    fn with_probe(mut mf: MediaFile) -> MediaFile {
        let probe = crate::probe_json::probe_json(&nd_ffmpeg::probe::AudioProbeResult {
            codec: mf.audio_codec(),
            profile: String::new(),
            bit_rate: mf.bit_rate,
            sample_rate: mf.sample_rate,
            bit_depth: mf.bit_depth.unwrap_or(0),
            channels: mf.channels,
        });
        mf.probe_data = probe;
        mf
    }

    fn mp3_44k(id: &str) -> MediaFile {
        with_probe(MediaFile {
            id: id.to_string(),
            suffix: "mp3".into(),
            codec: "MP3".into(),
            bit_rate: 320,
            channels: 2,
            sample_rate: 44_100,
            ..MediaFile::default()
        })
    }

    fn flac_1000(id: &str) -> MediaFile {
        with_probe(MediaFile {
            id: id.to_string(),
            suffix: "flac".into(),
            codec: "FLAC".into(),
            bit_rate: 1000,
            channels: 2,
            sample_rate: 44_100,
            bit_depth: Some(16),
            ..MediaFile::default()
        })
    }

    fn empty_opts() -> DecisionOptions {
        DecisionOptions::default()
    }

    #[test]
    fn sets_transcoding_profile_for_explicit_format_without_bitrate() {
        // Arrange (`legacy_client_test.go:17-29`)
        let mf = MediaFile {
            suffix: "flac".into(),
            bit_rate: 960,
            ..MediaFile::default()
        };
        // Act
        let ci = build_legacy_client_info(&mf, "mp3", 0, 0, "opus");
        // Assert
        assert_eq!(ci.name, "legacy");
        assert_eq!(ci.transcoding_profiles.len(), 1);
        assert_eq!(ci.transcoding_profiles[0].container, "mp3");
        assert_eq!(ci.transcoding_profiles[0].audio_codec, "mp3");
        assert_eq!(ci.transcoding_profiles[0].protocol, PROTOCOL_HTTP);
        assert_eq!(ci.max_audio_bitrate, 0);
        assert_eq!(ci.max_transcoding_audio_bitrate, 0);
        assert!(ci.direct_play_profiles.is_empty());
    }

    #[test]
    fn no_direct_play_profile_when_explicit_format_differs_from_source() {
        // Arrange (`legacy_client_test.go:32-38`)
        let mf = MediaFile {
            suffix: "flac".into(),
            bit_rate: 960,
            ..MediaFile::default()
        };
        // Act
        let ci = build_legacy_client_info(&mf, "opus", 0, 0, "opus");
        // Assert
        assert_eq!(ci.transcoding_profiles.len(), 1);
        assert_eq!(ci.transcoding_profiles[0].container, "opus");
        assert!(ci.direct_play_profiles.is_empty());
    }

    #[test]
    fn adds_direct_play_profile_when_explicit_format_matches_source() {
        // Arrange (`legacy_client_test.go:40-48`)
        let mf = MediaFile {
            suffix: "flac".into(),
            bit_rate: 960,
            ..MediaFile::default()
        };
        // Act
        let ci = build_legacy_client_info(&mf, "flac", 0, 0, "opus");
        // Assert
        assert_eq!(ci.transcoding_profiles.len(), 1);
        assert_eq!(ci.transcoding_profiles[0].container, "flac");
        assert_eq!(ci.direct_play_profiles.len(), 1);
        assert_eq!(ci.direct_play_profiles[0].containers, vec!["flac"]);
        assert_eq!(ci.direct_play_profiles[0].audio_codecs, vec!["flac"]);
    }

    #[test]
    fn sets_transcoding_profile_and_bitrate_for_explicit_format_with_bitrate() {
        // Arrange (`legacy_client_test.go:50-61`)
        let mf = MediaFile {
            suffix: "flac".into(),
            bit_rate: 960,
            ..MediaFile::default()
        };
        // Act
        let ci = build_legacy_client_info(&mf, "mp3", 192, 0, "opus");
        // Assert
        assert_eq!(ci.transcoding_profiles.len(), 1);
        assert_eq!(ci.transcoding_profiles[0].container, "mp3");
        assert_eq!(ci.transcoding_profiles[0].audio_codec, "mp3");
        assert_eq!(ci.max_audio_bitrate, 192);
        assert_eq!(ci.max_transcoding_audio_bitrate, 192);
        assert!(ci.direct_play_profiles.is_empty());
    }

    #[test]
    fn returns_direct_play_profile_when_no_format_and_no_bitrate() {
        // Arrange (`legacy_client_test.go:63-73`)
        let mf = MediaFile {
            suffix: "flac".into(),
            bit_rate: 960,
            ..MediaFile::default()
        };
        // Act
        let ci = build_legacy_client_info(&mf, "", 0, 0, "opus");
        // Assert
        assert_eq!(ci.direct_play_profiles.len(), 1);
        assert!(ci.direct_play_profiles[0].containers.is_empty());
        assert!(ci.direct_play_profiles[0].audio_codecs.is_empty());
        assert_eq!(ci.direct_play_profiles[0].protocols, vec![PROTOCOL_HTTP]);
        assert!(ci.transcoding_profiles.is_empty());
        assert_eq!(ci.max_audio_bitrate, 0);
    }

    #[test]
    fn uses_default_downsampling_format_for_bitrate_only_downsampling() {
        // Arrange (`legacy_client_test.go:75-89`)
        let mf = MediaFile {
            suffix: "flac".into(),
            bit_rate: 960,
            ..MediaFile::default()
        };
        // Act
        let ci = build_legacy_client_info(&mf, "", 128, 0, "opus");
        // Assert
        assert_eq!(ci.transcoding_profiles.len(), 1);
        assert_eq!(ci.transcoding_profiles[0].container, "opus");
        assert_eq!(ci.transcoding_profiles[0].audio_codec, "opus");
        assert_eq!(ci.transcoding_profiles[0].protocol, PROTOCOL_HTTP);
        assert_eq!(ci.max_audio_bitrate, 128);
        assert_eq!(ci.max_transcoding_audio_bitrate, 128);
        assert_eq!(ci.direct_play_profiles.len(), 1);
        assert_eq!(ci.direct_play_profiles[0].containers, vec!["flac"]);
    }

    #[test]
    fn returns_direct_play_when_bitrate_is_at_or_above_source() {
        // Arrange (`legacy_client_test.go:91-101`)
        let mf = MediaFile {
            suffix: "flac".into(),
            bit_rate: 960,
            ..MediaFile::default()
        };
        // Act
        let ci = build_legacy_client_info(&mf, "", 960, 0, "opus");
        // Assert
        assert_eq!(ci.direct_play_profiles.len(), 1);
        assert!(ci.direct_play_profiles[0].containers.is_empty());
        assert!(ci.direct_play_profiles[0].audio_codecs.is_empty());
        assert!(ci.transcoding_profiles.is_empty());
        assert_eq!(ci.max_audio_bitrate, 0);
    }

    #[test]
    fn uses_default_downsampling_format_when_player_max_bit_rate_is_below_source() {
        // Arrange (`legacy_client_test.go:103-115`)
        let mf = MediaFile {
            suffix: "flac".into(),
            bit_rate: 960,
            ..MediaFile::default()
        };
        // Act
        let ci = build_legacy_client_info(&mf, "", 0, 256, "opus");
        // Assert
        assert_eq!(ci.transcoding_profiles.len(), 1);
        assert_eq!(ci.transcoding_profiles[0].container, "opus");
        assert_eq!(ci.direct_play_profiles.len(), 1);
        assert_eq!(ci.direct_play_profiles[0].containers, vec!["flac"]);
    }

    #[test]
    fn does_not_downsample_when_player_max_bit_rate_is_at_or_above_source() {
        // Arrange (`legacy_client_test.go:117-126`)
        let mf = MediaFile {
            suffix: "flac".into(),
            bit_rate: 960,
            ..MediaFile::default()
        };
        // Act
        let ci = build_legacy_client_info(&mf, "", 0, 960, "opus");
        // Assert
        assert!(ci.transcoding_profiles.is_empty());
        assert_eq!(ci.direct_play_profiles.len(), 1);
        assert!(ci.direct_play_profiles[0].containers.is_empty());
    }

    #[test]
    fn does_not_downsample_when_default_downsampling_format_is_empty() {
        // Arrange (`legacy_client_test.go:128-136`)
        let mf = MediaFile {
            suffix: "flac".into(),
            bit_rate: 960,
            ..MediaFile::default()
        };
        // Act
        let ci = build_legacy_client_info(&mf, "", 0, 256, "");
        // Assert
        assert!(ci.transcoding_profiles.is_empty());
        assert_eq!(ci.direct_play_profiles.len(), 1);
        assert!(ci.direct_play_profiles[0].containers.is_empty());
    }

    #[test]
    fn prefers_explicit_request_format_over_player_cap() {
        // Arrange (`legacy_client_test.go:138-144`)
        let mf = MediaFile {
            suffix: "flac".into(),
            bit_rate: 960,
            ..MediaFile::default()
        };
        // Act
        let ci = build_legacy_client_info(&mf, "mp3", 0, 256, "opus");
        // Assert
        assert_eq!(ci.transcoding_profiles.len(), 1);
        assert_eq!(ci.transcoding_profiles[0].container, "mp3");
    }

    #[test]
    fn resolve_request_returns_raw_when_format_is_raw() {
        // Arrange (`legacy_client_test.go:150-155`)
        let mf = mp3_44k("1");
        // Act
        let req = resolve_request(&svc(), &mf, "raw", 0, 0, &empty_opts(), "opus");
        // Assert
        assert_eq!(req.format, "raw");
    }

    #[test]
    fn resolve_request_returns_raw_when_no_format_or_bitrate() {
        // Arrange (`legacy_client_test.go:157-162`)
        let mf = mp3_44k("1");
        // Act
        let req = resolve_request(&svc(), &mf, "", 0, 0, &empty_opts(), "opus");
        // Assert
        assert_eq!(req.format, "raw");
    }

    #[test]
    fn resolve_request_transcodes_to_requested_format() {
        // Arrange (`legacy_client_test.go:164-169`)
        let mf = flac_1000("1");
        // Act
        let req = resolve_request(&svc(), &mf, "opus", 0, 0, &empty_opts(), "opus");
        // Assert
        assert_eq!(req.format, "opus");
    }

    #[test]
    fn resolve_request_transcodes_to_requested_format_with_bitrate_limit() {
        // Arrange (`legacy_client_test.go:171-177`)
        let mf = flac_1000("1");
        // Act
        let req = resolve_request(&svc(), &mf, "mp3", 128, 0, &empty_opts(), "opus");
        // Assert
        assert_eq!(req.format, "mp3");
        assert_eq!(req.bit_rate, 128);
    }

    #[test]
    fn resolve_request_returns_raw_when_requested_format_matches_source() {
        // Arrange (`legacy_client_test.go:179-185`)
        let mf = mp3_44k("1");
        // Act
        let req = resolve_request(&svc(), &mf, "mp3", 320, 0, &empty_opts(), "opus");
        // Assert
        assert_eq!(req.format, "raw");
    }

    #[test]
    fn resolve_request_downsamples_when_only_bitrate_below_source() {
        // Arrange (`legacy_client_test.go:187-196`)
        let mf = flac_1000("1");
        // Act
        let req = resolve_request(&svc(), &mf, "", 128, 0, &empty_opts(), "opus");
        // Assert
        assert_eq!(req.format, "opus");
        assert_eq!(req.bit_rate, 128);
    }

    #[test]
    fn resolve_request_passes_offset_through() {
        // Arrange (`legacy_client_test.go:198-205`)
        let mf = flac_1000("1");
        // Act
        let req = resolve_request(&svc(), &mf, "opus", 128, 30, &empty_opts(), "opus");
        // Assert
        assert_eq!(req.format, "opus");
        assert_eq!(req.offset, 30);
    }

    #[test]
    fn override_forces_transcoding_to_a_different_format() {
        // Arrange (`legacy_client_test.go:210-218`)
        let mf = flac_1000("1");
        let opts = DecisionOptions {
            transcoding: Some(ServerTranscoding {
                target_format: "mp3".into(),
                default_bit_rate: 192,
            }),
            ..DecisionOptions::default()
        };
        // Act
        let req = resolve_request(&svc(), &mf, "", 0, 0, &opts, "opus");
        // Assert
        assert_eq!(req.format, "mp3");
        assert_eq!(req.bit_rate, 192);
    }

    #[test]
    fn override_allows_direct_play_when_source_matches_forced_format() {
        // Arrange (`legacy_client_test.go:220-227`)
        let mf = with_probe(MediaFile {
            id: "1".into(),
            suffix: "mp3".into(),
            codec: "MP3".into(),
            bit_rate: 128,
            channels: 2,
            sample_rate: 44_100,
            ..MediaFile::default()
        });
        let opts = DecisionOptions {
            transcoding: Some(ServerTranscoding {
                target_format: "mp3".into(),
                default_bit_rate: 256,
            }),
            ..DecisionOptions::default()
        };
        // Act
        let req = resolve_request(&svc(), &mf, "", 0, 0, &opts, "opus");
        // Assert
        assert_eq!(req.format, "raw");
    }

    #[test]
    fn override_transcodes_when_source_bitrate_exceeds_forced_cap() {
        // Arrange (`legacy_client_test.go:229-236`)
        let mf = mp3_44k("1");
        let opts = DecisionOptions {
            transcoding: Some(ServerTranscoding {
                target_format: "mp3".into(),
                default_bit_rate: 192,
            }),
            ..DecisionOptions::default()
        };
        // Act
        let req = resolve_request(&svc(), &mf, "", 0, 0, &opts, "opus");
        // Assert
        assert_eq!(req.format, "mp3");
        assert_eq!(req.bit_rate, 192);
    }

    #[test]
    fn override_player_max_bit_rate_wins_over_default_bit_rate() {
        // Arrange (`legacy_client_test.go:238-245`)
        let mf = flac_1000("1");
        let opts = DecisionOptions {
            transcoding: Some(ServerTranscoding {
                target_format: "mp3".into(),
                default_bit_rate: 192,
            }),
            player_max_bit_rate: 320,
        };
        // Act
        let req = resolve_request(&svc(), &mf, "", 0, 0, &opts, "opus");
        // Assert
        assert_eq!(req.format, "mp3");
        assert_eq!(req.bit_rate, 320);
    }

    #[test]
    fn override_with_no_caps_uses_format_default_bitrate() {
        // Arrange (`legacy_client_test.go:247-255`)
        let mf = flac_1000("1");
        let opts = DecisionOptions {
            transcoding: Some(ServerTranscoding {
                target_format: "mp3".into(),
                default_bit_rate: 0,
            }),
            ..DecisionOptions::default()
        };
        // Act
        let req = resolve_request(&svc(), &mf, "", 0, 0, &opts, "opus");
        // Assert: with no cap, lossless→lossy uses the format default bitrate
        // (160 for mp3 from the mock table).
        assert_eq!(req.format, "mp3");
        assert_eq!(req.bit_rate, 160);
    }

    #[test]
    fn no_override_applied_when_transcoding_absent() {
        // Arrange (`legacy_client_test.go:257-262`)
        let mf = flac_1000("1");
        // Act
        let req = resolve_request(&svc(), &mf, "", 0, 0, &empty_opts(), "opus");
        // Assert
        assert_eq!(req.format, "raw");
    }

    #[test]
    fn player_max_bit_rate_cap_applies_when_client_has_no_limit() {
        // Arrange (`legacy_client_test.go:267-274`)
        let mf = flac_1000("1");
        let opts = DecisionOptions {
            player_max_bit_rate: 320,
            ..DecisionOptions::default()
        };
        // Act
        let req = resolve_request(&svc(), &mf, "mp3", 0, 0, &opts, "opus");
        // Assert
        assert_eq!(req.format, "mp3");
        assert_eq!(req.bit_rate, 320);
    }

    #[test]
    fn client_limit_wins_when_more_restrictive_than_player_max_bit_rate() {
        // Arrange (`legacy_client_test.go:276-283`)
        let mf = flac_1000("1");
        let opts = DecisionOptions {
            player_max_bit_rate: 500,
            ..DecisionOptions::default()
        };
        // Act
        let req = resolve_request(&svc(), &mf, "mp3", 256, 0, &opts, "opus");
        // Assert
        assert_eq!(req.format, "mp3");
        assert_eq!(req.bit_rate, 256);
    }

    #[test]
    fn no_cap_applied_when_player_max_bit_rate_is_zero() {
        // Arrange (`legacy_client_test.go:285-291`)
        let mf = mp3_44k("1");
        // Act
        let req = resolve_request(&svc(), &mf, "", 0, 0, &empty_opts(), "opus");
        // Assert
        assert_eq!(req.format, "raw");
    }

    #[test]
    fn player_max_bit_rate_alone_downsamples_via_default_format() {
        // Arrange (`legacy_client_test.go:293-302`)
        let mf = flac_1000("1");
        let opts = DecisionOptions {
            player_max_bit_rate: 256,
            ..DecisionOptions::default()
        };
        // Act
        let req = resolve_request(&svc(), &mf, "", 0, 0, &opts, "opus");
        // Assert
        assert_eq!(req.format, "opus");
        assert_eq!(req.bit_rate, 256);
    }

    #[test]
    fn player_max_bit_rate_alone_serves_raw_without_default_format() {
        // Arrange (`legacy_client_test.go:304-311`)
        let mf = flac_1000("1");
        let opts = DecisionOptions {
            player_max_bit_rate: 256,
            ..DecisionOptions::default()
        };
        // Act
        let req = resolve_request(&svc(), &mf, "", 0, 0, &opts, "");
        // Assert
        assert_eq!(req.format, "raw");
    }

    #[test]
    fn unknown_format_falls_back_to_default_downsampling_format() {
        // Arrange (`legacy_client_test.go:316-323`)
        let mf = mp3_44k("1");
        // Act
        let req = resolve_request(&svc(), &mf, "xyz", 0, 0, &empty_opts(), "opus");
        // Assert
        assert_eq!(req.format, "opus");
    }

    #[test]
    fn unknown_format_falls_back_to_raw_when_default_format_empty() {
        // Arrange (`legacy_client_test.go:325-331`)
        let mf = mp3_44k("1");
        // Act
        let req = resolve_request(&svc(), &mf, "xyz", 0, 0, &empty_opts(), "");
        // Assert
        assert_eq!(req.format, "raw");
    }

    #[test]
    fn unknown_format_falls_back_to_raw_when_default_format_also_invalid() {
        // Arrange (`legacy_client_test.go:333-339`)
        let mf = mp3_44k("1");
        // Act
        let req = resolve_request(&svc(), &mf, "xyz", 0, 0, &empty_opts(), "xyz");
        // Assert
        assert_eq!(req.format, "raw");
    }

    #[test]
    fn fallback_preserves_bitrate() {
        // Arrange (`legacy_client_test.go:341-348`)
        let mf = flac_1000("1");
        // Act
        let req = resolve_request(&svc(), &mf, "xyz", 128, 0, &empty_opts(), "opus");
        // Assert
        assert_eq!(req.format, "opus");
        assert_eq!(req.bit_rate, 128);
    }

    #[test]
    fn resolve_client_request_direct_plays_through_container_aliases() {
        // Arrange (`legacy_client_test.go:353-366`)
        let mf = with_probe(MediaFile {
            id: "1".into(),
            suffix: "m4a".into(),
            codec: "AAC".into(),
            bit_rate: 256,
            channels: 2,
            sample_rate: 44_100,
            ..MediaFile::default()
        });
        let ci = ClientInfo {
            direct_play_profiles: vec![DirectPlayProfile {
                containers: vec!["mp4".into()],
                audio_codecs: vec!["aac".into()],
                ..DirectPlayProfile::default()
            }],
            transcoding_profiles: vec![Profile {
                container: "mp3".into(),
                audio_codec: "mp3".into(),
                protocol: PROTOCOL_HTTP.into(),
                ..Profile::default()
            }],
            ..ClientInfo::default()
        };
        // Act
        let req = resolve_client_request(&svc(), &mf, &ci, 5, &empty_opts());
        // Assert
        assert_eq!(req.format, "raw");
        assert_eq!(req.offset, 5);
    }

    #[test]
    fn resolve_client_request_transcodes_when_no_direct_play_matches() {
        // Arrange (`legacy_client_test.go:368-375`)
        let mf = flac_1000("1");
        let ci = ClientInfo {
            direct_play_profiles: vec![DirectPlayProfile {
                containers: vec!["mp3".into()],
                ..DirectPlayProfile::default()
            }],
            transcoding_profiles: vec![Profile {
                container: "mp3".into(),
                audio_codec: "mp3".into(),
                protocol: PROTOCOL_HTTP.into(),
                ..Profile::default()
            }],
            ..ClientInfo::default()
        };
        // Act
        let req = resolve_client_request(&svc(), &mf, &ci, 0, &empty_opts());
        // Assert
        assert_eq!(req.format, "mp3");
    }

    #[test]
    fn resolve_client_request_transcodes_over_bitrate_cap() {
        // Arrange (`legacy_client_test.go:377-386`)
        let mf = flac_1000("1");
        let ci = ClientInfo {
            max_audio_bitrate: 128,
            direct_play_profiles: vec![DirectPlayProfile {
                containers: vec!["flac".into()],
                ..DirectPlayProfile::default()
            }],
            transcoding_profiles: vec![Profile {
                container: "mp3".into(),
                audio_codec: "mp3".into(),
                protocol: PROTOCOL_HTTP.into(),
                ..Profile::default()
            }],
            ..ClientInfo::default()
        };
        // Act
        let req = resolve_client_request(&svc(), &mf, &ci, 0, &empty_opts());
        // Assert
        assert_eq!(req.format, "mp3");
        assert_eq!(req.bit_rate, 128);
    }

    #[test]
    fn resolve_client_request_applies_player_max_bit_rate_cap() {
        // Arrange (`legacy_client_test.go:388-395`)
        let mf = flac_1000("1");
        let ci = ClientInfo {
            direct_play_profiles: vec![DirectPlayProfile {
                containers: vec!["flac".into()],
                ..DirectPlayProfile::default()
            }],
            transcoding_profiles: vec![Profile {
                container: "mp3".into(),
                audio_codec: "mp3".into(),
                protocol: PROTOCOL_HTTP.into(),
                ..Profile::default()
            }],
            ..ClientInfo::default()
        };
        let opts = DecisionOptions {
            player_max_bit_rate: 192,
            ..DecisionOptions::default()
        };
        // Act
        let req = resolve_client_request(&svc(), &mf, &ci, 0, &opts);
        // Assert
        assert_eq!(req.format, "mp3");
        assert_eq!(req.bit_rate, 192);
    }

    #[test]
    fn parse_probe_data_is_none_for_empty_data() {
        // Sanity pin: `resolve` only uses stored probe_data; the empty-file
        // guard (`decider.go:182-184`) maps to None in the port.
        assert!(parse_probe_data("").is_none());
    }
}
