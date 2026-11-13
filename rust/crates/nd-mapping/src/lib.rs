//! #16 — the tag→model mapping layer: `Info` (from `nd-tagreader`) → cleaned
//! [`metadata::Metadata`] → [`mdfile::MediaFile`].
//!
//! Ports of the Go sources, with the mirrored file:line cited at each
//! behaviour:
//! - [`metadata`] — `model/metadata/metadata.go` (the `Info` boundary, tag
//!   cleaning, typed accessors, truncation and type validation).
//! - [`mappings`] — `resources/mappings.yaml` + `model/tag_mappings.go`
//!   (canonical names, aliases, split regexes, per-tag type/max length).
//! - [`tag_names`] — `model/tag.go:184-275`.
//! - [`participants`] — `model/metadata/map_participants.go` (roles, artist
//!   precedence, performer sub-roles, MBID sync).
//! - [`persistent_ids`] — `model/metadata/persistent_ids.go` +
//!   `model/metadata/legacy_ids.go`.
//! - [`map_mediafile`] — `model/metadata/map_mediafile.go` (`ToMediaFile`).
//! - [`mdfile`] — the mapping output DTO (`model/mediafile.go` fields written
//!   by the mapping).
//! - [`file_types`] — `model/file_types.go` + `resources/mime_types.yaml`.
//! - [`title_case`] — `golang.org/x/text/cases.Title(language.Und)` as used by
//!   the performer path.
//! - [`uuid`] — `github.com/google/uuid` parsing/canonicalization for UUID tags.
//! - [`config`] — the injected options replacing Go's `conf.Server` reads (the
//!   #S1 explicit-construction freeze).
//!
//! ## Backend gaps are surfaced, never invented
//!
//! `nd-tagreader` cannot supply some values Go gets from TagLib. This layer
//! keeps them absent (`Option`) instead of fabricating a default, and #17 must
//! fill them from storage before persisting:
//! - `MediaFile::size`, `birth_time`, `updated_at` — Go reads
//!   `Info.FileInfo` (`core/storage/local/local.go:90-109`), which the Rust
//!   backend does not produce.
//! - `MediaFile::bit_depth` — `None` when the backend reports 0. AAC-in-MP4 is
//!   the live case: Lofty reports 0 where Go's TagLib reports 16
//!   (`nd-tagreader/src/backend.rs` gap notes), so persisting a 0 would write a
//!   wrong bit depth into `media_file.bit_depth`.
//! - ASF/WMA: the backend errors rather than returning an `Info`, so no
//!   `Metadata` and no `MediaFile` exist for those files. Nothing is persisted
//!   for them today (Go, by contrast, indexes WMA with TagLib).
//! - Bitrate drift (FLAC 103/104, Vorbis 16/19 kbps) is passed through as the
//!   backend reports it. It is a *wrong value*, not an absent one: #17 would
//!   persist whatever the backend says into `media_file.bit_rate`.

pub mod config;
pub mod file_types;
pub mod map_mediafile;
pub mod mappings;
pub mod mdfile;
pub mod metadata;
pub mod participants;
pub mod persistent_ids;
pub mod tag_names;
pub mod title_case;
pub mod title_tables;
pub mod uuid;

pub use config::{MappingContext, MappingOptions, PidConfig, PidOptions, TagOverride};
pub use map_mediafile::date_year;
pub use mdfile::{MediaFile, Movement, DEFAULT_LIBRARY_ID};
pub use metadata::{FileStat, Metadata, Pair, Tags, ZWSP};
pub use participants::{
    map_display_album_artist, map_display_artist, map_participants, Credit, RoleCredits,
};
pub use persistent_ids::{
    album_id, artist_id, compute_pid, legacy_album_id, legacy_track_id, track_pid,
    validate_pid_spec,
};

/// Convenience: read real tags with `nd-tagreader` and map them in one step.
///
/// `file_stat` is the caller's storage data; pass [`FileStat::default()`] only
/// in tests that do not care about size/times.
///
/// # Errors
///
/// Propagates [`lofty::error::FileParseError`] — including the unsupported-ASF
/// case, which is why WMA never reaches the mapping at all.
pub fn map_file(
    path: &std::path::Path,
    file_stat: FileStat,
    ctx: &MappingContext,
    pid: PidConfig<'_>,
) -> Result<MediaFile, lofty::error::FileParseError> {
    let info = nd_tagreader::read_metadata(path)?;
    let md = Metadata::new(path.to_string_lossy().into_owned(), &info, file_stat, ctx);
    Ok(md.to_media_file(DEFAULT_LIBRARY_ID, "", pid))
}

#[cfg(test)]
mod crate_tests {
    use super::*;

    #[test]
    fn mapping_layer_instantiates_with_defaults() {
        // Act: build metadata from an empty Info and map it.
        let info = nd_tagreader::Info {
            tags: std::collections::BTreeMap::new(),
            audio_properties: nd_tagreader::AudioProperties {
                duration: 1.5,
                bit_rate: 128,
                channels: 2,
                sample_rate: 44100,
                bit_depth: 0,
                codec: "MP3".to_string(),
            },
            has_picture: false,
        };
        let md = Metadata::new(
            "music/unknown.mp3",
            &info,
            FileStat::default(),
            &MappingContext::default(),
        );
        let mf = md.to_media_file(1, "FOLDERID", PidOptions::default().effective());

        // Assert: Go's fallbacks for a tag-less file
        // (`map_mediafile.go:26`, `map_participants.go:149-151`).
        assert_eq!(mf.title, "unknown");
        assert_eq!(mf.album, nd_id::UNKNOWN_ALBUM);
        assert_eq!(mf.artist, nd_id::UNKNOWN_ARTIST);
        assert_eq!(mf.album_artist, nd_id::UNKNOWN_ARTIST);
        assert_eq!(mf.track_number, 0);
        assert_eq!(mf.bit_depth, None, "bit_depth 0 must stay absent");
        assert_eq!(mf.size, None, "storage size must not be fabricated");
        // Go truncates the ns->ms conversion twice (`metadata.go:96`), so 1.5s
        // survives but the value is a truncated float32.
        assert_eq!(mf.duration, 1.5);
        assert_eq!(mf.bit_rate, 128);
        assert_eq!(mf.codec, "MP3");
        assert_eq!(mf.suffix, "mp3");
        assert_eq!(mf.lyrics, "[]");
        assert!(
            !mf.participants.is_empty(),
            "artist roles are always present"
        );
    }
}
