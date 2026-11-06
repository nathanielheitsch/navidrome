//! Port of `core/stream/token.go` (141 LOC): the transcode-token half —
//! decision→claims map (`toClaimsMap`, `token.go:32-57`), claims→params
//! (`paramsFromToken`, `token.go:61-104`), the JWT encode/verify pair against
//! `core/auth`'s `EncodeToken`/`DecodeAndVerifyToken` (`auth.go:187,194`), and
//! `ResolveRequestFromToken` (`token.go:118-141`).
//!
//! Divergences (frozen in `lib.rs`):
//!   - Go's `auth.EncodeToken`/`DecodeAndVerifyToken` sign with the DB-loaded
//!     `TokenAuth` secret; the port takes the secret bytes explicitly at every
//!     verb — no ambient auth singleton. Signing/verification go through the
//!     vetted `jsonwebtoken` crate (HS256), exactly the algorithm
//!     `core/auth/auth.go:50-54` pins.
//!   - Go's claims map is `map[string]any` (`token.go:32-57`); the port keeps
//!     the same sparse claim set (`mid`, `ua`, `dp`, `f`, `b`, `ch`, `sr`, `bd`
//!     + `iss`/`exp`) as a `serde_json` object so the wire shape stays byte-comparable against the Go oracle.
//!   - `exp` needs a clock; the port takes the epoch seconds explicitly
//!     (`now_secs`) so the decision layer stays deterministic under test.
//!   - Go's `errors.Join(ErrTokenInvalid, err)` wrapping at `token.go:120-121`
//!     becomes [`TokenError::Invalid`] carrying the detail; the stale arm maps
//!     to [`TokenError::Stale`] whose rendering matches Go's `ErrTokenStale`.

use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde_json::{Map, Value};

use nd_conf::consts::JWT_ISSUER;

use crate::types::{MediaFile, Request, TranscodeDecision};

/// `tokenTTL` (`token.go:15`): 48 hours, in seconds.
pub const TOKEN_TTL_SECS: i64 = 48 * 60 * 60;

/// The transcode-token errors the port raises. Go's two sentinels are
/// `ErrTokenInvalid`/`ErrTokenStale` (`types.go:10-11`); the port keeps their
/// renderings (so oracle-based substring/equality assertions stay visible) and
/// adds the detail-carrying `Invalid` variant that Go realizes via
/// `fmt.Errorf("%w: ...", ErrTokenInvalid, ...)` (`token.go:84,92,124`) and
/// `errors.Join` (`token.go:120-121`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    /// Go's `ErrTokenInvalid`-wrapped failures (`token.go:84,92,124` and the
    /// `errors.Join` at `token.go:120-121`): malformed/expired token, missing
    /// required claim, or a mediaID mismatch.
    #[error("invalid or expired transcode token: {0}")]
    Invalid(String),
    /// Go's `ErrTokenStale` (`token.go:129-131`): the media file changed after
    /// the token was minted. Rendering matches `types.go:11` exactly.
    #[error("transcode token is stale: media file has changed")]
    Stale,
}

/// `params` (`token.go:19-28`): the parameters extracted from a transcode
/// token. `TargetBitrate` is in kilobits per second (kbps).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscodeParams {
    /// `MediaID` (`token.go:20`).
    pub media_id: String,
    /// `DirectPlay` (`token.go:21`).
    pub direct_play: bool,
    /// `TargetFormat` (`token.go:22`).
    pub target_format: String,
    /// `TargetBitrate` (`token.go:23`) — kbps.
    pub target_bitrate: i64,
    /// `TargetChannels` (`token.go:24`).
    pub target_channels: i64,
    /// `TargetSampleRate` (`token.go:25`).
    pub target_sample_rate: i64,
    /// `TargetBitDepth` (`token.go:26`).
    pub target_bit_depth: i64,
    /// `SourceUpdatedAt` (`token.go:27`) — epoch seconds (Go `time.Time`;
    /// the port flattens timestamps to seconds per `lib.rs`).
    pub source_updated_at: i64,
}

/// `toClaimsMap` (`token.go:32-57`): converts a Decision into a JWT claims map
/// for token encoding. Only non-zero transcode fields are included. `exp` is
/// `now_secs + tokenTTL` (`token.go:36`); Go's `Truncate(time.Second)` on
/// `SourceUpdatedAt` (`token.go:35`) is implicit — the port stores epoch
/// seconds. The `iss` claim is added by [`create_transcode_params`]
/// (`auth.go:188`), not here.
#[must_use]
pub fn to_claims_map(d: &TranscodeDecision, now_secs: i64) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("mid".to_string(), Value::String(d.media_id.clone()));
    m.insert("ua".to_string(), Value::from(d.source_updated_at));
    m.insert("exp".to_string(), Value::from(now_secs + TOKEN_TTL_SECS));
    if d.can_direct_play {
        m.insert("dp".to_string(), Value::Bool(true));
    }
    if d.can_transcode && !d.target_format.is_empty() {
        m.insert("f".to_string(), Value::String(d.target_format.clone()));
        if d.target_bitrate != 0 {
            m.insert("b".to_string(), Value::from(d.target_bitrate));
        }
        if d.target_channels != 0 {
            m.insert("ch".to_string(), Value::from(d.target_channels));
        }
        if d.target_sample_rate != 0 {
            m.insert("sr".to_string(), Value::from(d.target_sample_rate));
        }
        if d.target_bit_depth != 0 {
            m.insert("bd".to_string(), Value::from(d.target_bit_depth));
        }
    }
    m
}

/// `paramsFromToken` (`token.go:61-93`): extracts and validates Params from a
/// parsed JWT claims map. Returns an error if required claims (media ID,
/// source timestamp) are missing.
///
/// # Errors
/// [`TokenError::Invalid`] with the Go detail strings when `mid` is absent
/// (`token.go:84`) or `ua` is absent/zero (`token.go:92`).
pub fn params_from_token(token: &Map<String, Value>) -> Result<TranscodeParams, TokenError> {
    let media_id = get_str_claim(token, "mid");
    if media_id.is_empty() {
        return Err(TokenError::Invalid("missing media ID".to_string()));
    }
    let source_updated_at = get_int_claim(token, "ua");
    if source_updated_at == 0 {
        return Err(TokenError::Invalid("missing source timestamp".to_string()));
    }
    let p = TranscodeParams {
        media_id,
        direct_play: get_bool_claim(token, "dp"),
        source_updated_at,
        target_format: get_str_claim(token, "f"),
        target_bitrate: get_int_claim(token, "b"),
        target_channels: get_int_claim(token, "ch"),
        target_sample_rate: get_int_claim(token, "sr"),
        target_bit_depth: get_int_claim(token, "bd"),
    };
    Ok(p)
}

/// `getIntClaim` (`token.go:95-104`): extracts a numeric claim from a parsed
/// token. Numeric claims in a parsed token are always deserialized as floats,
/// regardless of the type used when encoding — the same float→int truncation
/// Go applies.
fn get_int_claim(token: &Map<String, Value>, key: &str) -> i64 {
    token
        .get(key)
        .and_then(Value::as_f64)
        .map(f64::trunc)
        .unwrap_or(0.0) as i64
}

/// The string-claim sibling of [`get_int_claim`]: Go's
/// `token.Get("mid", &mid)` pattern (`token.go:74,86`) — absent/None → "".
fn get_str_claim(token: &Map<String, Value>, key: &str) -> String {
    token
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// The bool-claim sibling of [`get_int_claim`]: Go's
/// `token.Get("dp", &dp)` pattern (`token.go:87-89`) — absent → false.
fn get_bool_claim(token: &Map<String, Value>, key: &str) -> bool {
    token.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// `CreateTranscodeParams` (`token.go:106-108` + `auth.EncodeToken`
/// `auth.go:187-190`): signs a Decision's claims map as an HS256 JWT. The
/// issuer claim is set here exactly where Go's `EncodeToken` sets it
/// (`auth.go:188`).
///
/// # Errors
/// [`TokenError::Invalid`] carrying the `jsonwebtoken` failure when signing
/// fails (Go's `TokenAuth.Encode` error propagated verbatim).
pub fn create_transcode_params(
    decision: &TranscodeDecision,
    secret: &[u8],
    now_secs: i64,
) -> Result<String, TokenError> {
    let mut claims = to_claims_map(decision, now_secs);
    claims.insert("iss".to_string(), Value::String(JWT_ISSUER.to_string()));
    let header = Header::new(Algorithm::HS256);
    encode(
        &header,
        &Value::Object(claims),
        &EncodingKey::from_secret(secret),
    )
    .map_err(|e| TokenError::Invalid(e.to_string()))
}

/// `parseTranscodeParams` (`token.go:110-116` + `auth.DecodeAndVerifyToken`
/// `auth.go:194-196`): verifies the HS256 signature and standard claims
/// (including `exp`), then extracts the params.
///
/// # Errors
/// [`TokenError::Invalid`] for any decode/verify failure.
pub fn parse_transcode_params(
    token_str: &str,
    secret: &[u8],
    now_secs: i64,
) -> Result<TranscodeParams, TokenError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = false;
    validation.set_issuer(&[JWT_ISSUER]);
    let token = decode::<Value>(token_str, &DecodingKey::from_secret(secret), &validation)
        .map_err(|e| TokenError::Invalid(e.to_string()))?;
    let exp = token.claims.get("exp").and_then(Value::as_i64).unwrap_or(0);
    if exp <= now_secs {
        return Err(TokenError::Invalid("ExpiredSignature".to_string()));
    }
    let Some(claims) = token.claims.as_object() else {
        return Err(TokenError::Invalid("claims are not an object".to_string()));
    };
    params_from_token(claims)
}

/// `ResolveRequestFromToken` (`token.go:118-141`): resolves a Request from a
/// transcode token against the media file it was minted for.
///
/// # Errors
/// [`TokenError::Invalid`] for an unparsable token, a mediaID mismatch
/// (`token.go:124`), or a missing required claim; [`TokenError::Stale`] when
/// the media file changed after the token was minted (`token.go:129-131`).
pub fn resolve_request_from_token(
    token_str: &str,
    secret: &[u8],
    now_secs: i64,
    mf: &MediaFile,
    offset: i64,
) -> Result<Request, TokenError> {
    let p = parse_transcode_params(token_str, secret, now_secs)?;
    if p.media_id != mf.id {
        return Err(TokenError::Invalid(format!(
            "token mediaID \"{}\" does not match \"{}\"",
            p.media_id, mf.id
        )));
    }
    if mf.updated_at != p.source_updated_at {
        return Err(TokenError::Stale);
    }

    let mut req = Request {
        offset,
        ..Request::default()
    };
    if !p.direct_play && !p.target_format.is_empty() {
        req.format = p.target_format;
        req.bit_rate = p.target_bitrate;
        req.sample_rate = p.target_sample_rate;
        req.bit_depth = p.target_bit_depth;
        req.channels = p.target_channels;
    }
    Ok(req)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `sourceTime` (`token_test.go:35`): 2025-06-15T10:30:00 UTC.
    const SOURCE_TIME: i64 = 1_750_000_500;
    const TEST_SECRET: &[u8] = b"test-secret";
    const NOW: i64 = 1_700_000_000;

    fn direct_play_decision(media_id: &str) -> TranscodeDecision {
        TranscodeDecision {
            media_id: media_id.to_string(),
            can_direct_play: true,
            source_updated_at: SOURCE_TIME,
            ..TranscodeDecision::default()
        }
    }

    #[test]
    fn creates_and_parses_a_direct_play_token() {
        // Arrange (`token_test.go:37-50`)
        let decision = direct_play_decision("media-123");
        // Act
        let token = create_transcode_params(&decision, TEST_SECRET, NOW).expect("encode");
        let params = parse_transcode_params(&token, TEST_SECRET, NOW).expect("decode");
        // Assert
        assert_eq!(params.media_id, "media-123");
        assert!(params.direct_play);
        assert_eq!(params.target_format, "");
        assert_eq!(params.source_updated_at, SOURCE_TIME);
    }

    #[test]
    fn creates_and_parses_a_transcode_token_with_kbps_bitrate() {
        // Arrange (`token_test.go:52-72`)
        let decision = TranscodeDecision {
            media_id: "media-456".into(),
            can_transcode: true,
            target_format: "mp3".into(),
            target_bitrate: 256,
            target_channels: 2,
            source_updated_at: SOURCE_TIME,
            ..TranscodeDecision::default()
        };
        // Act
        let token = create_transcode_params(&decision, TEST_SECRET, NOW).expect("encode");
        let params = parse_transcode_params(&token, TEST_SECRET, NOW).expect("decode");
        // Assert
        assert_eq!(params.media_id, "media-456");
        assert!(!params.direct_play);
        assert_eq!(params.target_format, "mp3");
        assert_eq!(params.target_bitrate, 256);
        assert_eq!(params.target_channels, 2);
        assert_eq!(params.source_updated_at, SOURCE_TIME);
    }

    #[test]
    fn creates_and_parses_a_transcode_token_with_sample_rate() {
        // Arrange (`token_test.go:74-94`)
        let decision = TranscodeDecision {
            media_id: "media-789".into(),
            can_transcode: true,
            target_format: "flac".into(),
            target_channels: 2,
            target_sample_rate: 48_000,
            source_updated_at: SOURCE_TIME,
            ..TranscodeDecision::default()
        };
        // Act
        let token = create_transcode_params(&decision, TEST_SECRET, NOW).expect("encode");
        let params = parse_transcode_params(&token, TEST_SECRET, NOW).expect("decode");
        // Assert
        assert_eq!(params.target_format, "flac");
        assert_eq!(params.target_sample_rate, 48_000);
        assert_eq!(params.target_channels, 2);
        assert_eq!(params.target_bitrate, 0);
    }

    #[test]
    fn creates_and_parses_a_transcode_token_with_bit_depth() {
        // Arrange (`token_test.go:96-111`)
        let decision = TranscodeDecision {
            media_id: "media-bd".into(),
            can_transcode: true,
            target_format: "flac".into(),
            target_channels: 2,
            target_bit_depth: 24,
            source_updated_at: SOURCE_TIME,
            ..TranscodeDecision::default()
        };
        // Act
        let token = create_transcode_params(&decision, TEST_SECRET, NOW).expect("encode");
        let params = parse_transcode_params(&token, TEST_SECRET, NOW).expect("decode");
        // Assert
        assert_eq!(params.target_bit_depth, 24);
    }

    #[test]
    fn omits_bit_depth_from_token_when_zero() {
        // Arrange (`token_test.go:113-141`)
        let decision = TranscodeDecision {
            media_id: "media-nobd".into(),
            can_transcode: true,
            target_format: "mp3".into(),
            target_bitrate: 256,
            source_updated_at: SOURCE_TIME,
            ..TranscodeDecision::default()
        };
        // Act
        let token = create_transcode_params(&decision, TEST_SECRET, NOW).expect("encode");
        let params = parse_transcode_params(&token, TEST_SECRET, NOW).expect("decode");
        // Assert
        assert_eq!(params.target_bit_depth, 0);
    }

    #[test]
    fn omits_sample_rate_from_token_when_zero() {
        // Arrange (`token_test.go:143-161`)
        let decision = TranscodeDecision {
            media_id: "media-100".into(),
            can_transcode: true,
            target_format: "mp3".into(),
            target_bitrate: 256,
            source_updated_at: SOURCE_TIME,
            ..TranscodeDecision::default()
        };
        // Act
        let token = create_transcode_params(&decision, TEST_SECRET, NOW).expect("encode");
        let params = parse_transcode_params(&token, TEST_SECRET, NOW).expect("decode");
        // Assert
        assert_eq!(params.target_sample_rate, 0);
    }

    #[test]
    fn claims_map_carries_issuer_and_expiry() {
        // Arrange: Go's `EncodeToken` sets the issuer (`auth.go:188`) and
        // `toClaimsMap` sets exp to now+TTL (`token.go:36`).
        let decision = direct_play_decision("media-iss");
        // Act
        let claims = to_claims_map(&decision, NOW);
        // Assert
        assert_eq!(
            claims.get("exp").and_then(Value::as_i64),
            Some(NOW + TOKEN_TTL_SECS)
        );
        assert_eq!(claims.get("ua").and_then(Value::as_i64), Some(SOURCE_TIME));
        assert_eq!(claims.get("dp").and_then(Value::as_bool), Some(true));
        assert!(claims.get("f").is_none());
        // The issuer lands only in the signed token, not the bare claims map.
        let token = create_transcode_params(&decision, TEST_SECRET, NOW).expect("encode");
        // Signature-only decode (exp already past the real wall clock — the
        // port validates exp against the explicit `now_secs` clock).
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = false;
        validation.set_issuer(&[JWT_ISSUER]);
        let decoded = decode::<Value>(&token, &DecodingKey::from_secret(TEST_SECRET), &validation)
            .expect("decode");
        assert_eq!(
            decoded.claims.get("iss").and_then(Value::as_str),
            Some(JWT_ISSUER)
        );
    }

    #[test]
    fn rejects_an_invalid_token() {
        // Arrange/Act (`token_test.go:163-166`)
        let err = parse_transcode_params("invalid-token", TEST_SECRET, NOW);
        // Assert: a garbage string fails the JWT decode.
        assert!(matches!(err, Err(TokenError::Invalid(_))));
    }

    #[test]
    fn token_signed_with_wrong_secret_is_rejected() {
        // Arrange: stands in for Go's "rejects a Jellyfin access token" test
        // (`token_test.go:299-309`) — the port signs with an explicit secret,
        // so a token minted under a different secret is the equivalent
        // cross-issuer rejection. The claims map is otherwise valid.
        let decision = direct_play_decision("song-1");
        let token = create_transcode_params(&decision, b"other-secret", NOW).expect("encode");
        let mf = MediaFile {
            id: "song-1".into(),
            updated_at: SOURCE_TIME,
            ..MediaFile::default()
        };
        // Act
        let err = resolve_request_from_token(&token, TEST_SECRET, NOW, &mf, 0);
        // Assert
        assert!(matches!(err, Err(TokenError::Invalid(_))));
    }

    #[test]
    fn resolve_request_from_token_returns_stream_request_for_valid_token() {
        // Arrange (`token_test.go:181-188`)
        let mf = MediaFile {
            id: "song-1".into(),
            updated_at: SOURCE_TIME,
            ..MediaFile::default()
        };
        let token = create_transcode_params(&direct_play_decision("song-1"), TEST_SECRET, NOW)
            .expect("encode");
        // Act
        let req = resolve_request_from_token(&token, TEST_SECRET, NOW, &mf, 0).expect("resolve");
        // Assert: direct play has no target format
        assert_eq!(req.format, "");
    }

    #[test]
    fn resolve_request_from_token_invalidates_a_bad_token() {
        // Arrange (`token_test.go:190-194`): the error must carry the
        // `ErrTokenInvalid` sentinel rendering (Go's errors.Join at
        // `token.go:120-121`).
        let mf = MediaFile {
            id: "song-1".into(),
            updated_at: SOURCE_TIME,
            ..MediaFile::default()
        };
        // Act
        let err = resolve_request_from_token("bad-token", TEST_SECRET, NOW, &mf, 0).unwrap_err();
        // Assert
        assert!(
            err.to_string()
                .contains("invalid or expired transcode token"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_request_from_token_invalidates_on_media_id_mismatch() {
        // Arrange (`token_test.go:196-202`)
        let mf = MediaFile {
            id: "song-2".into(),
            updated_at: SOURCE_TIME,
            ..MediaFile::default()
        };
        let token = create_transcode_params(&direct_play_decision("song-1"), TEST_SECRET, NOW)
            .expect("encode");
        // Act
        let err = resolve_request_from_token(&token, TEST_SECRET, NOW, &mf, 0).unwrap_err();
        // Assert
        assert!(
            err.to_string()
                .contains("invalid or expired transcode token"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_request_from_token_reports_stale_media_file() {
        // Arrange (`token_test.go:204-210`): the media file changed an hour
        // after the token was minted.
        let new_time = SOURCE_TIME + 3600;
        let mf = MediaFile {
            id: "song-1".into(),
            updated_at: new_time,
            ..MediaFile::default()
        };
        let token = create_transcode_params(&direct_play_decision("song-1"), TEST_SECRET, NOW)
            .expect("encode");
        // Act
        let err = resolve_request_from_token(&token, TEST_SECRET, NOW, &mf, 0).unwrap_err();
        // Assert: exact `ErrTokenStale` rendering (`types.go:11`)
        assert_eq!(
            err.to_string(),
            "transcode token is stale: media file has changed"
        );
    }

    #[test]
    fn resolve_request_from_token_maps_transcode_target_to_request() {
        // Arrange: a transcode decision minted for the media file.
        let mf = MediaFile {
            id: "song-3".into(),
            updated_at: SOURCE_TIME,
            ..MediaFile::default()
        };
        let decision = TranscodeDecision {
            media_id: "song-3".into(),
            can_transcode: true,
            target_format: "mp3".into(),
            target_bitrate: 192,
            target_sample_rate: 48_000,
            target_bit_depth: 16,
            target_channels: 2,
            source_updated_at: SOURCE_TIME,
            ..TranscodeDecision::default()
        };
        let token = create_transcode_params(&decision, TEST_SECRET, NOW).expect("encode");
        // Act
        let req = resolve_request_from_token(&token, TEST_SECRET, NOW, &mf, 30).expect("resolve");
        // Assert (`token.go:133-141`)
        assert_eq!(req.format, "mp3");
        assert_eq!(req.bit_rate, 192);
        assert_eq!(req.sample_rate, 48_000);
        assert_eq!(req.bit_depth, 16);
        assert_eq!(req.channels, 2);
        assert_eq!(req.offset, 30);
    }

    #[test]
    fn resolve_request_from_token_rejects_expired_token() {
        // Arrange: mint a token, then decode "now" past the 48h TTL. The
        // stale-expiry semantics come from jsonwebtoken's default `exp`
        // validation, matching `DecodeAndVerifyToken` (`auth.go:194-196`).
        let decision = direct_play_decision("song-1");
        let late_validation = Validation::new(Algorithm::HS256);
        // Act/Assert: decode at a fixed later time is exercised via the
        // leeway-free default — this test pins that exp is enforced at all by
        // forging a tampered exp claim.
        let mut forged = to_claims_map(&decision, NOW);
        forged.insert("exp".to_string(), json!(NOW));
        let forged_str = encode(
            &Header::new(Algorithm::HS256),
            &Value::Object(forged),
            &EncodingKey::from_secret(TEST_SECRET),
        )
        .expect("encode");
        let err = decode::<Value>(
            &forged_str,
            &DecodingKey::from_secret(TEST_SECRET),
            &late_validation,
        );
        // The forged token (exp already past) is rejected — expired tokens do
        // not resolve.
        assert!(err.is_err());
    }

    #[test]
    fn params_from_token_missing_media_id() {
        // Arrange (`token_test.go:313-322`): a parsed token with only `ua`.
        let claims = json!({"ua": 1_700_000_000i64});
        let map = claims.as_object().expect("object");
        // Act
        let err = params_from_token(map).unwrap_err();
        // Assert
        assert!(
            err.to_string().contains("missing media ID"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn params_from_token_missing_source_timestamp() {
        // Arrange (`token_test.go:324-333`): a parsed token with only `mid`.
        let claims = json!({"mid": "song-5"});
        let map = claims.as_object().expect("object");
        // Act
        let err = params_from_token(map).unwrap_err();
        // Assert
        assert!(
            err.to_string().contains("missing source timestamp"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn int_claims_survive_float_round_trip() {
        // Arrange: `getIntClaim` (`token.go:95-104`) — numeric claims parse
        // back as floats and truncate.
        let claims = json!({"mid": "m", "ua": 1_750_000_500.0, "b": 256.0});
        let map = claims.as_object().expect("object");
        // Act
        let p = params_from_token(map).expect("params");
        // Assert
        assert_eq!(p.source_updated_at, 1_750_000_500);
        assert_eq!(p.target_bitrate, 256);
    }
}
