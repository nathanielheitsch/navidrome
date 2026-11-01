//! # nd-id
//!
//! Rust port of Navidrome's ID scheme (Go issue #02, `docs/rust-port/issues.md`):
//!
//! - [`Encode`]/[`Decode`]/[`NewRandom`] from `model/id/id.go` — 22-char
//!   zero-padded base62 rendering of a 16-byte value.
//! - [`NewHash`]/[`NewTagID`] from `model/id/id.go` — MD5 over the arguments,
//!   each joined by a U+200B (zero-width space) separator.
//! - Artwork-id composition/parsing from `model/artwork_id.go`.
//! - Participants logic from `model/participants.go`.
//! - PID prefix rules from `consts/consts.go:157-159` (including the
//!   `PIDTrack`/`PIDAlbum` `track_legacy`/`album_legacy` migration defaults in
//!   `db/migrations/20241026183640_support_new_scanner.go:96-97`).
//! - Sentinel ids as literal constants copied verbatim from the Go source
//!   (`consts/consts.go:214,:218`) — never recomputed at runtime.

use md5::Digest;
use num_bigint::BigUint;
use num_traits::Zero;
use rand::RngCore;

/// The canonical alphabet Go's `big.Int.Text(62)` renders: `0-9`, then `a-z`,
/// then `A-Z`. Kept as a constant for the alphabet-closure tests; encoding
/// itself uses `BigUint`'s `to_str_radix`, which uses the same ordering.
pub const BASE62_ALPHABET: &[u8] =
    b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// Length of every id produced by [`Encode`] (and hence [`NewHash`]).
pub const ID_LENGTH: usize = 22;

/// Byte length of the value encoded by [`Encode`].
pub const ID_BYTES: usize = 16;

/// U+200B ZERO WIDTH SPACE — the separator [`NewHash`] writes after every
/// argument, byte-identical to the Go implementation.
pub const HASH_SEPARATOR: &str = "\u{200b}";

/// Digit value of one base62 character, or `None` when it is outside the
/// Go `big.Int` base-62 alphabet.
fn base62_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'z' => Some(c - b'a' + 10),
        b'A'..=b'Z' => Some(c - b'A' + 36),
        _ => None,
    }
}

fn base62_encode(b: &[u8; ID_BYTES]) -> String {
    let mut v = BigUint::from_bytes_be(b);
    if v.is_zero() {
        return "0".repeat(ID_LENGTH);
    }
    let base = BigUint::from(62u32);
    let mut digits = Vec::new();
    while !v.is_zero() {
        let rem = &v % &base;
        digits.push(BASE62_ALPHABET[rem.to_string().parse::<usize>().unwrap()]);
        v /= &base;
    }
    digits.reverse();
    let s = String::from_utf8(digits).unwrap();
    if s.len() >= ID_LENGTH {
        return s;
    }
    format!("{}{}", "0".repeat(ID_LENGTH - s.len()), s)
}

/// Renders a 16-byte value as the canonical 22-char zero-padded base62 id,
/// exactly like Go's `fmt.Sprintf("%022s", new(big.Int).SetBytes(b).Text(62))`.
///
/// Values whose base62 representation exceeds 22 digits are returned as-is
/// (the Go `%022s` verb also leaves over-long strings unpadded).
pub fn encode(b: &[u8; ID_BYTES]) -> String {
    base62_encode(b)
}

/// Inverse of [`encode`]: parses a 22-char base62 id back into 16 bytes,
/// left-padded with zeros (`big.Int.FillBytes` semantics).
///
/// # Errors
///
/// Returns a [`IdError`] when the string is not a valid 22-char base62 id or
/// overflows 128 bits.
pub fn decode(s: &str) -> Result<[u8; ID_BYTES], IdError> {
    let invalid = |reason: &str| IdError::Invalid(s.to_string(), reason.to_string());
    if s.len() != ID_LENGTH {
        return Err(IdError::Invalid(
            s.to_string(),
            format!("invalid id length {}", s.len()),
        ));
    }
    if !s.bytes().all(|c| BASE62_ALPHABET.contains(&c)) {
        return Err(invalid("invalid base62 id"));
    }
    let mut v = BigUint::from(0u32);
    for &c in s.as_bytes() {
        let d = u32::from(base62_digit(c).ok_or_else(|| invalid("invalid base62 id"))?);
        v = v * 62u32 + d;
    }
    let bytes = v.to_bytes_be();
    if bytes.len() > ID_BYTES {
        return Err(IdError::Invalid(
            s.to_string(),
            "id overflows 128 bits".to_string(),
        ));
    }
    let mut out = [0u8; ID_BYTES];
    out[ID_BYTES - bytes.len()..].copy_from_slice(&bytes);
    Ok(out)
}

/// Generates a random 22-char id from 16 cryptographically-secure random
/// bytes (Go: `crypto/rand.Read` → `Encode`).
pub fn new_random() -> String {
    let mut b = [0u8; ID_BYTES];
    rand::rng().fill_bytes(&mut b);
    encode(&b)
}

/// MD5 over every argument, each followed by one U+200B zero-width space,
/// rendered as a 22-char base62 id. Byte-exact with Go's `id.NewHash`.
pub fn new_hash(data: &[&str]) -> String {
    let mut hasher = md5::Md5::new();
    for d in data {
        hasher.update(d.as_bytes());
        hasher.update(HASH_SEPARATOR.as_bytes());
    }
    encode(&hasher.finalize().into())
}

/// Convenience wrapper around [`new_hash`] for the common one-argument form.
pub fn new_hash1(data: &str) -> String {
    new_hash(&[data])
}

/// Tag id: `NewHash(strings.ToLower(name), strings.ToLower(value))`.
pub fn new_tag_id(name: &str, value: &str) -> String {
    new_hash(&[&name.to_lowercase(), &value.to_lowercase()])
}

/// Errors from [`decode`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IdError {
    /// The candidate id string failed validation.
    #[error("invalid id {0:?}: {1}")]
    Invalid(String, String),
}

/// Sentinel artist id for "Various Artists", copied verbatim from Go
/// `consts/consts.go:214`. Must never be recomputed at runtime.
pub const VARIOUS_ARTISTS_ID: &str = "63sqASlAfjbGMuLP4JhnZU";

/// Sentinel display name for the Various Artists pseudo-artist
/// (Go `consts/consts.go:213`).
pub const VARIOUS_ARTISTS: &str = "Various Artists";

/// Sentinel artist id for "[Unknown Artist]", copied verbatim from Go
/// `consts/consts.go:218`. Legal only while [`new_hash`] stays byte-exact.
pub const UNKNOWN_ARTIST_ID: &str = "7lsE5pS09fPS1VuFqwXbia";

/// Sentinel display name for the unknown-artist pseudo-artist
/// (Go `consts/consts.go:217`).
pub const UNKNOWN_ARTIST: &str = "[Unknown Artist]";

/// Sentinel album name (Go `consts/consts.go:216`).
pub const UNKNOWN_ALBUM: &str = "[Unknown Album]";

/// MusicBrainz id for the Various Artists entity (Go `consts/consts.go:219`).
pub const VARIOUS_ARTISTS_MBZ_ID: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

/// Default album PID (persistent id) rule — Go `consts/consts.go:158`.
///
/// `<tag>|<comparable tags>`: the first tag must be non-empty for the track to
/// get a PID, the remainder feed the hash of the comparable identity.
pub const DEFAULT_ALBUM_PID: &str =
    "musicbrainz_albumid|albumartistid,album,albumversion,releasedate";

/// Default track PID rule — Go `consts/consts.go:159`.
pub const DEFAULT_TRACK_PID: &str = "musicbrainz_trackid|albumid,discnumber,tracknumber,title";

/// Value the `PIDTrack` property was seeded with by migration
/// `20241026183640_support_new_scanner.go:96` — the pre-new-scanner rule,
/// meaning "identity is the file path" semantics for the old tables.
pub const PID_TRACK_LEGACY: &str = "track_legacy";

/// Value the `PIDAlbum` property was seeded with by migration
/// `20241026183640_support_new_scanner.go:97`.
pub const PID_ALBUM_LEGACY: &str = "album_legacy";

mod artwork;
mod participants;

pub use artwork::{
    new_artwork_id, new_disc_artwork_id, parse_artwork_id, parse_disc_artwork_id, ArtworkId,
    ArtworkKind, ARTWORK_KIND_ALBUM, ARTWORK_KIND_ALBUM_PREFIX, ARTWORK_KIND_ARTIST,
    ARTWORK_KIND_ARTIST_PREFIX, ARTWORK_KIND_DISC, ARTWORK_KIND_DISC_PREFIX,
    ARTWORK_KIND_MEDIA_FILE, ARTWORK_KIND_MEDIA_FILE_PREFIX, ARTWORK_KIND_PLAYLIST,
    ARTWORK_KIND_PLAYLIST_PREFIX, ARTWORK_KIND_RADIO, ARTWORK_KIND_RADIO_PREFIX,
};
pub use participants::{
    Participant, Participants, Role, ALL_ROLES, ROLE_ALBUM_ARTIST, ROLE_ARRANGER, ROLE_ARTIST,
    ROLE_COMPOSER, ROLE_CONDUCTOR, ROLE_DIRECTOR, ROLE_DJMIXER, ROLE_ENGINEER, ROLE_INVALID,
    ROLE_LYRICIST, ROLE_MAIN_CREDIT, ROLE_MIXER, ROLE_PERFORMER, ROLE_PRODUCER, ROLE_REMIXER,
};

#[cfg(test)]
mod tests {
    use super::*;

    // Golden pairs produced by the actual Go code (`model/id/id.go`),
    // verified with a scratch `go run` against this repo:
    //   id.NewHash("various artists")     -> 746UnkTmvtNH4yRiXA1H4I
    //   id.NewHash("[unknown artist]")    -> 7lsE5pS09fPS1VuFqwXbia
    //   id.NewHash("a", "b")              -> 1T4wiSaqmhWz9C39sgglP2
    //   id.NewHash("")                     -> 6l2eDCFmY72hS7z8186KZd
    //   id.NewHash("x")                    -> 2RQbvv8EUxGWhPmoKIZRyq

    #[test]
    fn golden_new_hash_pairs() {
        assert_eq!(new_hash1("various artists"), "746UnkTmvtNH4yRiXA1H4I");
        assert_eq!(new_hash1("[unknown artist]"), "7lsE5pS09fPS1VuFqwXbia");
        assert_eq!(new_hash(&["a", "b"]), "1T4wiSaqmhWz9C39sgglP2");
        assert_eq!(new_hash(&[""]), "6l2eDCFmY72hS7z8186KZd");
        assert_eq!(new_hash(&["x"]), "2RQbvv8EUxGWhPmoKIZRyq");
    }

    #[test]
    fn new_hash_uses_zero_width_space_separator() {
        // "a" + U+200B + "b" + U+200B must equal new_hash(&["a","b"]).
        let mut h = md5::Md5::new();
        h.update(b"a");
        h.update("\u{200b}".as_bytes());
        h.update(b"b");
        h.update("\u{200b}".as_bytes());
        assert_eq!(encode(&h.finalize().into()), new_hash(&["a", "b"]));
        // ...and must NOT equal a plain-comma or no-separator hash.
        assert_ne!(new_hash(&["a", "b"]), new_hash(&["ab"]));
    }

    #[test]
    fn golden_encode_decode_round_trips() {
        // From Go: big.Int.Text(62) over the given 16 bytes.
        let cases: &[(&str, &str)] = &[
            ("00000000000000000000000000000000", "0000000000000000000000"),
            ("ffffffffffffffffffffffffffffffff", "7N42dgm5tFLK9N8MT7fHC7"),
            ("0102030405060708090a0b0c0d0e0f10", "01TUwCKr0qGUD2dQQItYSQ"),
            ("e82d45ac6aaa021b507e14f606e488a8", "746UnkTmvtNH4yRiXA1H4I"),
        ];
        for (hexs, expected) in cases {
            let mut b = [0u8; 16];
            for i in 0..16 {
                b[i] = u8::from_str_radix(&hexs[i * 2..i * 2 + 2], 16).unwrap();
            }
            assert_eq!(&encode(&b), expected, "encode({hexs})");
            assert_eq!(decode(expected).unwrap(), b, "decode({expected})");
        }
    }

    #[test]
    fn encode_yields_22_chars_in_alphabet() {
        for s in [
            new_hash1("anything"),
            new_random(),
            encode(&[0u8; 16]),
            encode(&[0xff; 16]),
        ] {
            assert_eq!(s.len(), 22, "length of {s}");
            assert!(
                s.bytes().all(|c| (48..=57).contains(&c)
                    || (97..=122).contains(&c)
                    || (65..=90).contains(&c)),
                "alphabet closure for {s}"
            );
        }
    }

    #[test]
    fn new_random_unique_and_shaped() {
        let a = new_random();
        let b = new_random();
        assert_eq!(a.len(), 22);
        assert_ne!(a, b);
        assert!(decode(&a).is_ok());
    }

    #[test]
    fn decode_rejects_bad_input() {
        assert!(decode("").is_err());
        assert!(decode("short").is_err());
        assert!(decode("012345678901234567890").is_err()); // 21 chars
                                                           // 'Z' is a valid base62 digit... but a 23-char string fails on length:
        assert!(decode("0000000000000000000000Z").is_err());
        // '+' is outside the base62 alphabet:
        assert!(decode("00000000000000000000+0").is_err());
    }

    #[test]
    fn new_tag_id_lowercases() {
        assert_eq!(new_tag_id("Genre", "ROCK"), new_hash(&["genre", "rock"]));
        assert_ne!(new_tag_id("Genre", "ROCK"), new_hash(&["Genre", "ROCK"]));
    }

    #[test]
    fn sentinels_are_literals_and_match_go() {
        assert_eq!(VARIOUS_ARTISTS_ID, "63sqASlAfjbGMuLP4JhnZU");
        // UnknownArtistID = NewHash(strings.ToLower(UnknownArtist)) — the
        // literal must equal the Go-derived value we verified with `go run`.
        assert_eq!(UNKNOWN_ARTIST_ID, "7lsE5pS09fPS1VuFqwXbia");
        // ...and must equal what the algorithm produces for the display name.
        assert_eq!(UNKNOWN_ARTIST_ID, new_hash1(&UNKNOWN_ARTIST.to_lowercase()));
        assert_eq!(VARIOUS_ARTISTS, "Various Artists");
        assert_eq!(UNKNOWN_ARTIST, "[Unknown Artist]");
        assert_eq!(UNKNOWN_ALBUM, "[Unknown Album]");
        assert_eq!(
            VARIOUS_ARTISTS_MBZ_ID,
            "89ad4ac3-39f7-470e-963a-56509c546377"
        );
    }

    #[test]
    fn pid_rules_match_consts() {
        assert_eq!(
            DEFAULT_ALBUM_PID,
            "musicbrainz_albumid|albumartistid,album,albumversion,releasedate"
        );
        assert_eq!(
            DEFAULT_TRACK_PID,
            "musicbrainz_trackid|albumid,discnumber,tracknumber,title"
        );
        assert_eq!(PID_TRACK_LEGACY, "track_legacy");
        assert_eq!(PID_ALBUM_LEGACY, "album_legacy");
    }
}
