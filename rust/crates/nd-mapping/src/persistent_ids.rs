//! Port of `model/metadata/persistent_ids.go` and
//! `model/metadata/legacy_ids.go` — canonical (persistent) and legacy ID
//! assignment.
//!
//! This is the step where a mistake silently diverges databases
//! (`docs/rust-port/issues.md` #16), so the hash inputs are reproduced
//! byte-for-byte from Go rather than "equivalently".

use md5::Digest;
use nd_id::encode;
use nd_utils::str::{clear, to_lower};

use crate::config::PidConfig;
use crate::mdfile::MediaFile;
use crate::metadata::Metadata;

/// Separator between PID fields (`strings.Join(values, "\\")`,
/// `persistent_ids.go:52`).
const FIELD_SEP: &str = "\\";

/// Hash function injected instead of Go's `id.NewHash` closure parameter
/// (`persistent_ids.go:18`), so tests can assert the raw hash input.
pub type HashFunc = dyn Fn(&str) -> String;

/// Go's default: `id.NewHash` over the whole PID string (`:59`).
#[must_use]
pub fn default_hash(pid: &str) -> String {
    nd_id::new_hash1(pid)
}

/// `computePID` (`persistent_ids.go:31-60`).
///
/// `spec` is a pipe-separated list of fields; each field is a comma-separated
/// attribute list. The first field with any non-empty attribute value wins, and
/// its attribute values are joined with `\`. The library id is prefixed when
/// `prepend_lib_id` is set.
pub fn compute_pid(
    mf: &MediaFile,
    md: &Metadata,
    spec: &str,
    album_spec: &str,
    prepend_lib_id: bool,
    hash: &HashFunc,
) -> String {
    match spec {
        "track_legacy" => return legacy_track_id(mf, prepend_lib_id),
        "album_legacy" => return legacy_album_id(mf, md, prepend_lib_id),
        _ => {}
    }
    let mut pid = String::new();
    for field in spec.split('|') {
        let attributes: Vec<&str> = field.split(',').collect();
        let mut values = Vec::with_capacity(attributes.len());
        let mut has_value = false;
        for attr in attributes {
            let v = get_pid_attr(mf, md, attr, prepend_lib_id, spec, album_spec, hash);
            if !v.is_empty() {
                has_value = true;
            }
            values.push(v);
        }
        if has_value {
            pid = values.join(FIELD_SEP);
            break;
        }
    }
    if prepend_lib_id {
        pid = format!("{}{FIELD_SEP}{pid}", mf.library_id);
    }
    hash(&pid)
}

/// `getPIDAttr` (`persistent_ids.go:62-81`). Attribute names are trimmed and
/// lowercased; anything else is read as a tag.
fn get_pid_attr(
    mf: &MediaFile,
    md: &Metadata,
    attr: &str,
    prepend_lib_id: bool,
    spec: &str,
    album_spec: &str,
    hash: &HashFunc,
) -> String {
    // Go: strings.TrimSpace(strings.ToLower(attr)) (`persistent_ids.go:67`).
    // `to_lower` is Go's ToLower, not `str::to_lowercase`: the hashed values
    // below are persisted as `pid`/`album_id`.
    let attr = attr.trim();
    let attr = to_lower(attr);
    match attr.as_str() {
        "albumid" => {
            if spec == album_spec {
                // Go logs an error and ignores the recursive attribute
                // (`persistent_ids.go:66-69`, regression #4920).
                return String::new();
            }
            compute_pid(mf, md, album_spec, album_spec, prepend_lib_id, hash)
        }
        "folder" => dir(&mf.path),
        // Go hashes the attribute value here (`persistent_ids.go:74`), so
        // `computePID` hashes this already-hashed value again at `:59`.
        "albumartistid" => hash(&clear(&to_lower(&mf.album_artist))),
        "title" => mf.title.clone(),
        "album" => clear(&to_lower(&md.string(crate::tag_names::TAG_ALBUM.as_str()))),
        _ => md.string(attr.as_str()),
    }
}

/// `filepath.Dir` (`persistent_ids.go:72`): the parent path, `.` when the name
/// has no separator, and the root preserved (`/` stays `/`).
fn dir(path: &str) -> String {
    match path.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => path[..i].to_string(),
        None => ".".to_string(),
    }
}

/// `trackPID` (`persistent_ids.go:115-117`).
#[must_use]
pub fn track_pid(mf: &MediaFile, md: &Metadata, pid: PidConfig<'_>) -> String {
    compute_pid(mf, md, pid.track, pid.album, true, &default_hash)
}

/// `albumID` (`persistent_ids.go:119-121`).
#[must_use]
pub fn album_id(mf: &MediaFile, md: &Metadata, pid_conf: &str) -> String {
    compute_pid(mf, md, pid_conf, pid_conf, true, &default_hash)
}

/// `artistID` (`persistent_ids.go:124-127`): `computePID` with the single
/// `albumartistid` attribute over a `MediaFile` whose `AlbumArtist` is the
/// name. Unrolled, that is a **double** hash — the attribute value is itself
/// `id.NewHash(str.Clear(strings.ToLower(name)))` (`:74`), and `computePID`
/// hashes the joined spec value again (`:59`). Getting this single-hashed would
/// silently rename every artist.
#[must_use]
pub fn artist_id(name: &str) -> String {
    let mf = MediaFile {
        album_artist: name.to_string(),
        ..Default::default()
    };
    // The `albumartistid` spec reads no tags and `albumSpec` is empty, so an
    // empty metadata is equivalent to the caller's for this path.
    compute_pid(
        &mf,
        crate::metadata::Metadata::empty(),
        "albumartistid",
        "",
        false,
        &default_hash,
    )
}

/// `ValidatePIDSpec` (`persistent_ids.go:85-113`).
///
/// # Errors
///
/// A message matching Go's error strings, so configuration errors stay
/// recognisable to users reading Go-era docs.
pub fn validate_pid_spec(spec: &str, is_album: bool) -> Result<(), String> {
    match spec {
        "" => return Ok(()),
        "album_legacy" if is_album => return Ok(()),
        "track_legacy" if !is_album => return Ok(()),
        _ => {}
    }
    for field in spec.split('|') {
        for attr in field.split(',') {
            let attr = attr.trim();
            let attr = to_lower(attr);
            match attr.as_str() {
                "" => return Err(format!("empty attribute in {spec:?}")),
                "albumid" => {
                    if is_album {
                        return Err("albumid cannot be used in an album PID".to_string());
                    }
                }
                "folder" | "albumartistid" => {}
                _ => {
                    let Some(name) = crate::mappings::mappings().canonical_tag_name(&attr) else {
                        return Err(format!("unknown attribute {attr:?}"));
                    };
                    if is_album && name != attr {
                        return Err(format!(
                            "use the tag name {name:?} instead of its alias {attr:?}"
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Legacy IDs (`model/metadata/legacy_ids.go`)
// ---------------------------------------------------------------------------

/// MD5 of the key, rendered as the canonical 22-char base62 id
/// (`legacy_ids.go:23-24`: `id.Encode(md5.Sum(...))`).
fn md5_id(key: &str) -> String {
    let mut hasher = md5::Md5::new();
    hasher.update(key.as_bytes());
    encode(&hasher.finalize().into())
}

/// `legacyTrackID` (`legacy_ids.go:18-25`): the file path, prefixed with the
/// library id only for a non-default library.
#[must_use]
pub fn legacy_track_id(mf: &MediaFile, prepend_lib_id: bool) -> String {
    let mut key = mf.path.clone();
    if prepend_lib_id && mf.library_id != crate::mdfile::DEFAULT_LIBRARY_ID {
        key = format!("{}{FIELD_SEP}{key}", mf.library_id);
    }
    md5_id(&key)
}

/// `legacyAlbumID` (`legacy_ids.go:27-40`): `artist\album`, plus the release
/// date unless albums are grouped by release.
#[must_use]
pub fn legacy_album_id(mf: &MediaFile, md: &Metadata, prepend_lib_id: bool) -> String {
    let (_date, _original, release_date) = md.map_dates();
    // Go lowercases the whole `artist\album` key (`legacy_ids.go:29`) before
    // MD5'ing it into a persisted album id, so the lowercasing must be Go's
    // `to_lower` and must wrap the joined key (not each part).
    let mut album_path = to_lower(&format!(
        "{}{FIELD_SEP}{}",
        legacy_map_album_artist_name(md),
        legacy_map_album_name(md)
    ));
    if !md.options().group_album_releases && !release_date.is_empty() {
        album_path = format!("{album_path}{FIELD_SEP}{release_date}");
    }
    if prepend_lib_id && mf.library_id != crate::mdfile::DEFAULT_LIBRARY_ID {
        album_path = format!("{}{FIELD_SEP}{album_path}", mf.library_id);
    }
    md5_id(&album_path)
}

/// `legacyMapAlbumArtistName` (`legacy_ids.go:42-53`): album artist, then
/// `Various Artists` for compilations, then track artist, then unknown.
#[must_use]
pub fn legacy_map_album_artist_name(md: &Metadata) -> String {
    let mut values = vec![
        md.string(crate::tag_names::TAG_ALBUM_ARTIST.as_str()),
        String::new(),
        md.string(crate::tag_names::TAG_TRACK_ARTIST.as_str()),
        nd_id::UNKNOWN_ARTIST.to_string(),
    ];
    if md.bool(crate::tag_names::TAG_COMPILATION.as_str()) {
        values[1] = nd_id::VARIOUS_ARTISTS.to_string();
    }
    values
        .into_iter()
        .find(|v| !v.is_empty())
        .unwrap_or_default()
}

/// `legacyMapAlbumName` (`legacy_ids.go:55-60`).
#[must_use]
pub fn legacy_map_album_name(md: &Metadata) -> String {
    let album = md.string(crate::tag_names::TAG_ALBUM.as_str());
    if album.is_empty() {
        nd_id::UNKNOWN_ALBUM.to_string()
    } else {
        album
    }
}
