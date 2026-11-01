//! Artwork-id composition and parsing, ported from Go `model/artwork_id.go`.
//!
//! An artwork id has the textual shape `<kind-prefix>-<id>[_<suffix>]`, where
//! the suffix is either a 16-char lowercase-hex XXH3-64 content hash or a
//! legacy `<hexTimestamp>` token (the value `0` is ignored).

use std::time::{SystemTime, UNIX_EPOCH};

/// The typed artwork-kind domain from `model/artwork_id.go`.
///
/// Construct instances via the [`ARTWORK_KIND_*`](crate::ARTWORK_KIND_MEDIA_FILE) constants
/// so equality is by prefix+name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ArtworkKind {
    /// Short token used in artwork ids and the `item_artwork.item_kind` column.
    pub prefix: &'static str,
    /// Human-readable kind name (Go `Kind.String()`).
    pub name: &'static str,
}

/// `KindMediaFileArtwork` — media files.
pub const ARTWORK_KIND_MEDIA_FILE: ArtworkKind = ArtworkKind {
    prefix: "mf",
    name: "media_file",
};
/// Prefix of [`ARTWORK_KIND_MEDIA_FILE`].
pub const ARTWORK_KIND_MEDIA_FILE_PREFIX: &str = "mf";

/// `KindArtistArtwork` — artists.
pub const ARTWORK_KIND_ARTIST: ArtworkKind = ArtworkKind {
    prefix: "ar",
    name: "artist",
};
/// Prefix of [`ARTWORK_KIND_ARTIST`].
pub const ARTWORK_KIND_ARTIST_PREFIX: &str = "ar";

/// `KindAlbumArtwork` — albums.
pub const ARTWORK_KIND_ALBUM: ArtworkKind = ArtworkKind {
    prefix: "al",
    name: "album",
};
/// Prefix of [`ARTWORK_KIND_ALBUM`].
pub const ARTWORK_KIND_ALBUM_PREFIX: &str = "al";

/// `KindPlaylistArtwork` — playlists.
pub const ARTWORK_KIND_PLAYLIST: ArtworkKind = ArtworkKind {
    prefix: "pl",
    name: "playlist",
};
/// Prefix of [`ARTWORK_KIND_PLAYLIST`].
pub const ARTWORK_KIND_PLAYLIST_PREFIX: &str = "pl";

/// `KindDiscArtwork` — per-disc album artwork.
pub const ARTWORK_KIND_DISC: ArtworkKind = ArtworkKind {
    prefix: "dc",
    name: "disc",
};
/// Prefix of [`ARTWORK_KIND_DISC`].
pub const ARTWORK_KIND_DISC_PREFIX: &str = "dc";

/// `KindRadioArtwork` — radios.
pub const ARTWORK_KIND_RADIO: ArtworkKind = ArtworkKind {
    prefix: "ra",
    name: "radio",
};
/// Prefix of [`ARTWORK_KIND_RADIO`].
pub const ARTWORK_KIND_RADIO_PREFIX: &str = "ra";

fn kind_from_prefix(prefix: &str) -> Option<ArtworkKind> {
    Some(match prefix {
        ARTWORK_KIND_MEDIA_FILE_PREFIX => ARTWORK_KIND_MEDIA_FILE,
        ARTWORK_KIND_ARTIST_PREFIX => ARTWORK_KIND_ARTIST,
        ARTWORK_KIND_ALBUM_PREFIX => ARTWORK_KIND_ALBUM,
        ARTWORK_KIND_PLAYLIST_PREFIX => ARTWORK_KIND_PLAYLIST,
        ARTWORK_KIND_DISC_PREFIX => ARTWORK_KIND_DISC,
        ARTWORK_KIND_RADIO_PREFIX => ARTWORK_KIND_RADIO,
        _ => return None,
    })
}

/// A parsed or composed artwork id (`model.ArtworkID`). The `last_update`
/// field is legacy: populated only when parsing old `_<hexTimestamp>` tokens.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArtworkId {
    /// Typed kind; the prefix appears in the string form.
    pub kind: ArtworkKind,
    /// Encoded entity id (e.g. an album id); empty renders as the empty string.
    pub id: String,
    /// Content-hash suffix; empty string means unknown/none.
    pub hash: String,
    /// Legacy `_`-suffix timestamp, if the token parsed as one.
    pub last_update: Option<u64>,
}

impl std::fmt::Display for ArtworkId {
    /// `<prefix>-<id>` plus `_<hash>` when a hash is present; empty id renders
    /// as the empty string (Go `ArtworkID.String()`).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.id.is_empty() {
            return Ok(());
        }
        write!(f, "{}-{}", self.kind.prefix, self.id)?;
        if !self.hash.is_empty() {
            write!(f, "_{}", self.hash)?;
        }
        Ok(())
    }
}

/// Builds an [`ArtworkId`] for `kind` and the entity's id
/// (Go `model.NewArtworkID`; the legacy `lastUpdate` pointer argument is kept
/// as an explicit optional value).
pub fn new_artwork_id(kind: ArtworkKind, id: &str, last_update: Option<u64>) -> ArtworkId {
    ArtworkId {
        kind,
        id: id.to_string(),
        hash: String::new(),
        last_update,
    }
}

/// Parses `<kind>-<id>[_<suffix>]` (Go `model.ParseArtworkID`).
///
/// # Errors
///
/// [`ParseError::Invalid`] when the id has no `-` separator, and
/// [`ParseError::UnknownKind`] when the kind prefix is not one of the six
/// known kinds.
pub fn parse_artwork_id(id: &str) -> Result<ArtworkId, ParseError> {
    let Some((prefix, rest)) = id.split_once('-') else {
        return Err(ParseError::Invalid(id.to_string()));
    };
    let Some(kind) = kind_from_prefix(prefix) else {
        return Err(ParseError::UnknownKind(prefix.to_string()));
    };
    let mut parsed = ArtworkId {
        kind,
        id: rest.to_string(),
        hash: String::new(),
        last_update: None,
    };
    if let Some((raw_id, suffix)) = rest.split_once('_') {
        parsed.id = raw_id.to_string();
        // Hash detection must come first: a 16-hex value with the high bit set
        // overflows int64 in the legacy timestamp path.
        if is_image_hash(suffix) {
            parsed.hash = suffix.to_string();
        } else if suffix != "0" {
            if let Ok(ts) = i64::from_str_radix(suffix, 16) {
                parsed.last_update = Some(u64::try_from(ts).expect("hex int64 fits u64"));
            }
        }
    }
    Ok(parsed)
}

/// Reports whether `s` is a 16-char lowercase-hex XXH3-64 content hash
/// (Go `isImageHash`).
fn is_image_hash(s: &str) -> bool {
    s.len() == 16
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Errors from [`parse_artwork_id`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseError {
    /// No `<kind>-` separator was found.
    #[error("invalid artwork id")]
    Invalid(String),
    /// The kind prefix is not one of the six known artwork kinds.
    #[error("invalid artwork kind")]
    UnknownKind(String),
}

/// Builds a disc artwork id: `<albumID>:<discNumber>` (Go `DiscArtworkID`).
pub fn new_disc_artwork_id(album_id: &str, disc_number: i32) -> String {
    format!("{album_id}:{disc_number}")
}

/// Parses a `<albumID>:<discNumber>` pair (Go `ParseDiscArtworkID`).
///
/// # Errors
///
/// [`ParseError::Invalid`] when the separator or number is missing/invalid.
pub fn parse_disc_artwork_id(id: &str) -> Result<(String, i32), ParseError> {
    let Some((album_id, num)) = id.split_once(':') else {
        return Err(ParseError::Invalid(id.to_string()));
    };
    if num.is_empty() {
        return Err(ParseError::Invalid(id.to_string()));
    }
    num.parse::<i32>()
        .map(|n| (album_id.to_string(), n))
        .map_err(|_| ParseError::Invalid(id.to_string()))
}

/// Unix timestamp of `secs` seconds since the epoch, as `SystemTime` — used by
/// callers that surface the legacy `last_update` field.
#[allow(dead_code)] // helper for future nd-model callers
pub fn legacy_timestamp(secs: u64) -> Option<SystemTime> {
    UNIX_EPOCH.checked_add(std::time::Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_parse_round_trip() {
        for (kind, prefix) in [
            (ARTWORK_KIND_MEDIA_FILE, "mf"),
            (ARTWORK_KIND_ARTIST, "ar"),
            (ARTWORK_KIND_ALBUM, "al"),
            (ARTWORK_KIND_PLAYLIST, "pl"),
            (ARTWORK_KIND_DISC, "dc"),
            (ARTWORK_KIND_RADIO, "ra"),
        ] {
            let art = ArtworkId {
                kind,
                id: "abc".into(),
                hash: String::new(),
                last_update: None,
            };
            let s = art.to_string();
            assert_eq!(s, format!("{prefix}-abc"));
            let parsed = parse_artwork_id(&s).unwrap();
            assert_eq!(parsed, art);
        }
    }

    #[test]
    fn display_with_hash_suffix() {
        let art = ArtworkId {
            kind: ARTWORK_KIND_ALBUM,
            id: "xyz".into(),
            hash: "0123456789abcdef".into(),
            last_update: None,
        };
        assert_eq!(art.to_string(), "al-xyz_0123456789abcdef");
        assert_eq!(parse_artwork_id("al-xyz_0123456789abcdef").unwrap(), art);
    }

    #[test]
    fn empty_id_renders_empty() {
        let art = ArtworkId::default();
        assert_eq!(art.to_string(), "");
    }

    #[test]
    fn parse_legacy_timestamp_and_zero_suffix() {
        // Legacy hex timestamp: e.g. "5f5e1000" -> 1600000000.
        let parsed = parse_artwork_id("al-xyz_5f5e1000").unwrap();
        assert_eq!(parsed.id, "xyz");
        assert_eq!(parsed.hash, "");
        assert_eq!(parsed.last_update, Some(0x5f5e_1000));
        assert!(legacy_timestamp(0x5f5e_1000).is_some());

        // Suffix "0" is ignored entirely.
        let parsed = parse_artwork_id("al-xyz_0").unwrap();
        assert_eq!(parsed.id, "xyz");
        assert_eq!(parsed.hash, "");
        assert_eq!(parsed.last_update, None);
    }

    #[test]
    fn parse_hash_preferred_over_timestamp() {
        // 16 lowercase hex chars parse as a hash even though they could be
        // misread as an overflowing timestamp.
        let parsed = parse_artwork_id("mf-id_f15f2d2ce97e6071").unwrap();
        assert_eq!(parsed.hash, "f15f2d2ce97e6071");
        assert_eq!(parsed.last_update, None);
        // Uppercase hex is NOT a hash -> falls through to the timestamp path
        // (and fails to parse as hex there, so both stay empty).
        let parsed = parse_artwork_id("mf-id_f15f2d2ce97e607F").unwrap();
        assert_eq!(parsed.hash, "");
        assert_eq!(parsed.last_update, None);
    }

    #[test]
    fn parse_errors() {
        assert!(matches!(
            parse_artwork_id("noseparator"),
            Err(ParseError::Invalid(_))
        ));
        assert!(matches!(
            parse_artwork_id("xx-id"),
            Err(ParseError::UnknownKind(_))
        ));
    }

    #[test]
    fn disc_artwork_ids() {
        let id = new_disc_artwork_id("al-xyz", 2);
        assert_eq!(id, "al-xyz:2");
        assert_eq!(
            parse_disc_artwork_id(&id).unwrap(),
            ("al-xyz".to_string(), 2)
        );
        assert!(parse_disc_artwork_id("al-xyz").is_err());
        assert!(parse_disc_artwork_id("al-xyz:").is_err());
        assert!(parse_disc_artwork_id("al-xyz:x").is_err());
    }
}
