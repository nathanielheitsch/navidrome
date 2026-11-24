//! SC-03 — executed proof for the singleton album/artist drafts.
//!
//! Go's phase-1 helpers (`scanner/phase_1_folders.go:328-346`) turn a folder's mapped
//! tracks into the entities the folder transaction writes. This lane is the **one track
//! per folder** subset; multi-track aggregation is SC-04. Ground truth is the committed
//! Go oracle (`rust/crates/nd-mapping/tests/data/go_oracle.json`), cross-checked against
//! its text rather than this lane's output.
//!
//! No DB writes, no id resolution, no album equality: `nd_model::album::equals` compares
//! ids only (`nd-model/src/album.rs:174-176`) and is not Go's change detection — decisions.md
//! **S4** assigns semantic comparison to SC-04, and `album.rs` is untouched here.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, TimeZone, Utc};
use nd_db::album_repo::NamedParticipant;
use nd_db::artist_repo::ArtistRecord;
use nd_mapping::config::{MappingContext, MappingOptions, PidOptions};
use nd_mapping::mappings::Mappings;
use nd_mapping::mdfile::MediaFile;
use nd_mapping::metadata::Metadata;
use nd_mapping::participants::{Credit, RoleCredits};
use nd_scanner::aggregate::{AggregateError, TrackFacts};
use nd_scanner::mapping_bridge::{effective_birth_time, TrackStat};
use nd_scanner::projection::{
    album_draft_from_track, album_drafts, all_artists, all_names, artist_drafts, merge_credits,
};

/// Repo root, derived from this crate's manifest dir (never an absolute path).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|p| p.join("go.mod").is_file())
        .expect("repo root containing go.mod")
        .to_path_buf()
}

fn oracle_text() -> &'static str {
    static TEXT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TEXT.get_or_init(|| {
        let path = repo_root().join("rust/crates/nd-mapping/tests/data/go_oracle.json");
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} must exist: {e}", path.display()))
    })
    .as_str()
}

fn oracle_object(source: &str) -> &str {
    let text = oracle_text();
    let marker = format!("\"source\": \"{source}\"");
    let start = text
        .find(&marker)
        .unwrap_or_else(|| panic!("{source} must appear in go_oracle.json"));
    let rest = &text[start..];
    let end = rest[marker.len()..]
        .find("\"source\": ")
        .map(|i| marker.len() + i)
        .unwrap_or(rest.len());
    &rest[..end]
}

#[track_caller]
fn assert_oracle(source: &str, needle: &str) {
    assert!(
        oracle_object(source).contains(needle),
        "oracle row {source} must contain {needle:?}"
    );
}

/// Owned `/tmp` fixture root (a per-folder layout needs real files for real stats).
fn temp_root(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let root = std::env::temp_dir().join(format!(
        "nd-scanner-projection-{}-{}-{name}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&root).expect("create fixture root");
    root
}

fn cleanup(root: &Path) {
    let _ = fs::remove_dir_all(root);
}

fn at(unix: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(unix, 0).unwrap()
}

/// A synthetic `Info` (backend-shaped), as the mapping consumes it.
fn synthetic_info(
    tags: &[(&str, &[&str])],
    props: (f64, i64, i64, i64, i64, &str),
    picture: bool,
) -> nd_tagreader::Info {
    nd_tagreader::Info {
        tags: tags
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string(),
                    v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
                )
            })
            .collect(),
        audio_properties: nd_tagreader::AudioProperties {
            duration: props.0,
            bit_rate: props.1,
            channels: props.2,
            sample_rate: props.3,
            bit_depth: props.4,
            codec: props.5.to_string(),
        },
        has_picture: picture,
    }
}

/// Map a synthetic `Info` the way the scanner does, then project it.
fn mapped(
    path: &str,
    info: &nd_tagreader::Info,
    ctx: &MappingContext,
    pid: PidOptions,
) -> (Metadata, MediaFile) {
    let stat = TrackStat {
        size: 1000,
        modified: Some(at(1_700_000_000)),
        birth_time: Some(at(1_600_000_000)),
    };
    let md = Metadata::new(path, info, stat.to_mapping_file_stat(), ctx);
    let mf = md.to_media_file(1, "FOLDER-1", pid.effective());
    (md, mf)
}

fn draft_stat(
    size: i64,
    modified: Option<DateTime<Utc>>,
    birth: Option<DateTime<Utc>>,
) -> TrackStat {
    TrackStat {
        size,
        modified,
        birth_time: birth,
    }
}

const IMPORTED: DateTime<Utc> = DateTime::from_timestamp(1_800_000_000, 0).expect("valid stamp");

/// A fixed instant for a stat's mtime/birth, so no test depends on wall time.
fn now() -> DateTime<Utc> {
    at(1_900_000_000)
}

/// The caller-side resolution the drafts must consume: identical to what
/// `bridge_media_file` computes, so the media row and the album row cannot drift.
fn resolve(stat: &TrackStat) -> Option<DateTime<Utc>> {
    effective_birth_time(stat, Some(now()))
}

/// Project a draft the way SC-05 would: the birth is the SAME resolved value the track
/// record gets (storage birth, else the injected substitute).
fn project(
    track: &MediaFile,
    stat: &TrackStat,
    mappings: &Mappings,
) -> nd_db::album_repo::AlbumRecord {
    album_draft_from_track(track, stat, resolve(stat), mappings, IMPORTED).expect("projects")
}

/// `singleton_album_draft_one_track` — Go `MediaFiles.ToAlbum()`
/// (`model/mediafile.go:320-406`), each aggregation arm reduced to its one-element
/// result. Oracle rows give the mapping-side field values.
#[test]
fn singleton_album_draft_one_track() {
    let ctx = MappingContext::new(MappingOptions::default());

    // The real fixture: one track, embedded picture absent, album artist present.
    let source = "rust/crates/nd-tagreader/tests/fixtures/real/known.mp3";
    assert_oracle(source, "\"album\": \"Synthetic Album\"");
    assert_oracle(source, "\"albumArtist\": \"Album Artist\"");
    assert_oracle(source, "\"albumId\": \"1GrQX6RbY8HNgNugy6o63Z\"");
    assert_oracle(source, "\"duration\": 0.44");

    let root = temp_root("known.mp3");
    let copy = root.join("known.mp3");
    fs::copy(
        repo_root()
            .join("rust")
            .join("crates/nd-tagreader/tests/fixtures/real/known.mp3"),
        &copy,
    )
    .expect("copy fixture");
    let info = nd_tagreader::read_metadata(&copy).expect("known.mp3 must read");
    let md = Metadata::new(
        "music/album/track",
        &info,
        TrackStat {
            size: 7833,
            modified: Some(at(1_700_000_000)),
            birth_time: Some(at(1_600_000_000)),
        }
        .to_mapping_file_stat(),
        &ctx,
    );
    let mf = md.to_media_file(1, "FOLDER-1", PidOptions::default().effective());
    let album = project(
        &mf,
        &draft_stat(7833, Some(at(1_700_000_000)), Some(at(1_600_000_000))),
        md.mappings(),
    );

    // Identity fields (Go `:344-346`).
    assert_eq!(album.id, "1GrQX6RbY8HNgNugy6o63Z", "a.ID = m.AlbumID");
    assert_eq!(album.library_id, 1, "a.LibraryID = m.LibraryID");
    assert_eq!(album.name, "Synthetic Album", "a.Name = m.Album");
    assert_eq!(album.album_artist, "Album Artist");
    assert_eq!(album.album_artist_id, "5WdtjTSFGJHIMAh76SendS");
    assert_eq!(album.song_count, 1, "SongCount: len(mfs)");
    assert_eq!(
        album.duration as f32, 0.44,
        "a.Duration += m.Duration (Go float32, f64 column affinity)"
    );
    assert_eq!(album.size, 7833, "a.Size += m.Size, storage-supplied");
    assert_eq!(
        album.folder_ids,
        Some(vec!["FOLDER-1".to_string()]),
        "FolderIDs from the track"
    );

    // Aggregation arms that collapse with one element.
    assert_eq!(album.min_year, 2024);
    assert_eq!(
        album.max_year, 2024,
        "minMax of one value is that value twice"
    );
    assert_eq!(
        album.date, "2024-03-02",
        "allOrNothing of one value is that value"
    );
    assert_eq!(album.min_original_year, 0);
    assert_eq!(album.max_original_year, 0);
    assert_eq!(album.mbz_album_id, "", "MostFrequent skips the zero value");
    assert_eq!(album.mbz_release_group_id, "");
    assert_eq!(album.rg_album_gain, None, "mostFrequentPtr of nil is nil");
    assert_eq!(album.rg_album_peak, None);
    assert!(!album.compilation);
    assert!(
        !album.missing,
        "a fresh import is not missing (phase 1's missing pass is SC-06)"
    );
    assert_eq!(
        album.comment, "Comment one\nComment two",
        "allOrNothing of one comment"
    );

    // Discs: Go seeds {1: ""} and adds the track's disc (`:323`, `:371-373`).
    let discs = album.discs.clone().expect("Go always seeds disc 1");
    assert_eq!(discs.keys().copied().collect::<Vec<_>>(), [1], "one disc");
    assert_eq!(discs[&1], "", "the disc subtitle is empty on this fixture");

    // Artwork: `firstArtPath` (`:500-517`) — no embedded picture, so no embed path.
    assert_eq!(
        album.embed_art_path, "",
        "no embedded picture → empty path, never the folder"
    );

    // Positive arm of the same helper (`:500-517`): with an embedded picture the draft's
    // EmbedArtPath is the track's logical path and disc (`firstArtPath` starts from
    // currentDisc == 0, so the first art-bearing track wins outright). The real fixture
    // has no picture, so this arm is exercised on a synthetic track.
    let art_info = synthetic_info(
        &[
            ("ALBUM", &["Art Album"]),
            ("ARTIST", &["A"]),
            ("TITLE", &["T"]),
        ],
        (10.0, 128, 2, 44100, 0, "MP3"),
        true,
    );
    let (art_md, art_mf) = mapped("art/01.mp3", &art_info, &ctx, PidOptions::default());
    assert!(art_mf.has_cover_art, "the synthetic Info carries a picture");
    let art_album = project(
        &art_mf,
        &draft_stat(1, Some(at(1)), Some(at(0))),
        art_md.mappings(),
    );
    assert_eq!(
        art_album.embed_art_path, art_mf.path,
        "has_cover_art=true → the track's logical path (not a file path, not empty)"
    );
    assert_ne!(
        art_album.embed_art_path, "",
        "the positive arm must actually produce a path"
    );

    // Dates from storage, not the scan: `newer(zero, mtime)` = mtime; `older(zero, birth)`
    // = birth (`:385-386`, `:487-498`).
    assert_eq!(
        album.updated_at,
        Some(at(1_700_000_000)),
        "UpdatedAt = newer(zero, m.UpdatedAt)"
    );
    assert_eq!(
        album.created_at,
        Some(at(1_600_000_000)),
        "CreatedAt = older(zero, m.BirthTime)"
    );

    // Read-side/hydration fields stay zero.
    assert_eq!(
        album.genre, "",
        "genre is PostScan-derived (album_repository.go:52-57)"
    );
    assert!(album.genres.is_empty());
    assert_eq!(
        album.image,
        nd_db::artwork_hydration::ItemImage::default(),
        "UNRESOLVED, not absent"
    );
    assert_eq!(
        album.annotation,
        Default::default(),
        "annotation is a structs:\"-\" promoted embed"
    );
    assert_eq!(album.library_path, "", "join-only");
    assert_eq!(album.library_name, "");
    assert_eq!(
        album.small_image_url, "",
        "external metadata is filled later"
    );
    assert_eq!(album.external_info_updated_at, None);

    // Tags keep only album-level names (`AlbumLevelTags`, `model/album.go:104-122`).
    assert_eq!(
        album.tags.get("genre").map(Vec::as_slice),
        Some(&["Rock".to_string()][..])
    );
    assert!(
        album.tags.contains_key("tracktotal"),
        "album-level total is kept"
    );
    assert!(album.tags.contains_key("disctotal"));
    assert!(
        !album.tags.contains_key("title"),
        "a first-class non-album tag is filtered out"
    );
    assert_eq!(
        album.imported_at, IMPORTED,
        "the caller's stamp (put_sql overwrites it anyway)"
    );

    // Participants: the column and the name list come from the same credits.
    assert_eq!(
        album
            .participants
            .keys()
            .map(|r| r.as_str())
            .collect::<Vec<_>>(),
        ["albumartist", "artist"]
    );
    assert_eq!(
        album.participant_names,
        nd_scanner::mapping_bridge::named_participants(&mf.participants),
        "the draft's participant names are the merged credits' own"
    );

    cleanup(&root);

    // ReplayGain: a genuine 0.0 is a real candidate, not treated as absent
    // (`mostFrequentPtr`, `:462-484`).
    let info = synthetic_info(
        &[
            ("ALBUM", &["RG Album"]),
            ("REPLAYGAIN_ALBUM_GAIN", &["0.0"]),
        ],
        (10.0, 128, 2, 44100, 0, "MP3"),
        false,
    );
    let (md, mf) = mapped("rg/01.mp3", &info, &ctx, PidOptions::default());
    assert_eq!(
        mf.rg_album_gain,
        Some(0.0),
        "the mapping keeps a zero gain as Some(0.0)"
    );
    let album = project(&mf, &draft_stat(1, Some(at(1)), Some(at(0))), md.mappings());
    assert_eq!(
        album.rg_album_gain,
        Some(0.0),
        "a real 0.0 survives, nil does not become 0"
    );

    // Distinct nonzero sentinels through the two album-side ReplayGain fields: a
    // gain/peak swap or a dropped field fails. Oracle rows pin Go's own values
    // (`synthetic:replaygain_and_r128`, `tests/fixtures/test.mp3`).
    assert_oracle("synthetic:replaygain_and_r128", "\"rgAlbumGain\": -7.5");
    assert_oracle("synthetic:replaygain_and_r128", "\"rgAlbumPeak\": 0.9");
    assert_oracle("synthetic:replaygain_and_r128", "\"rgTrackGain\": -3");
    assert_oracle("synthetic:replaygain_and_r128", "\"rgTrackPeak\": 1");
    let rg_info = synthetic_info(
        &[
            ("ALBUM", &["RG Album"]),
            ("REPLAYGAIN_ALBUM_GAIN", &["-7.5 dB"]),
            ("REPLAYGAIN_ALBUM_PEAK", &["0.9"]),
            ("REPLAYGAIN_TRACK_GAIN", &["-3 dB"]),
            ("REPLAYGAIN_TRACK_PEAK", &["1.0"]),
        ],
        (10.0, 128, 2, 44100, 0, "MP3"),
        false,
    );
    let (rg_md, rg_mf) = mapped("rg/02.mp3", &rg_info, &ctx, PidOptions::default());
    assert_eq!(
        (
            rg_mf.rg_album_gain,
            rg_mf.rg_album_peak,
            rg_mf.rg_track_gain,
            rg_mf.rg_track_peak
        ),
        (Some(-7.5), Some(0.9), Some(-3.0), Some(1.0)),
        "the four fields carry distinct values, `dB` stripped (metadata.go:110-113)"
    );
    let rg_album = project(
        &rg_mf,
        &draft_stat(1, Some(at(1)), Some(at(0))),
        rg_md.mappings(),
    );
    assert_eq!(
        rg_album.rg_album_gain,
        Some(-7.5),
        "mostFrequentPtr of one non-nil value"
    );
    assert_eq!(rg_album.rg_album_peak, Some(0.9));
    assert_ne!(
        rg_album.rg_album_gain, rg_album.rg_album_peak,
        "gain and peak are not swapped"
    );

    // An unparseable gain is nil, not 0 (`nullableFloat`, metadata.go:136-142; oracle
    // row `synthetic:replaygain_invalid_and_r128`: NaN → nil, "inf" peak → nil, and the
    // R128 fallback Q7.8 conversion wins for the track gain).
    assert_oracle(
        "synthetic:replaygain_invalid_and_r128_fallback",
        "\"rgTrackGain\": 0",
    );
    assert_oracle(
        "synthetic:replaygain_invalid_and_r128_fallback",
        "\"rgAlbumGain\": null",
    );
    let bad_info = synthetic_info(
        &[
            ("ALBUM", &["Bad Gain"]),
            ("REPLAYGAIN_TRACK_GAIN", &["NaN"]),
            ("REPLAYGAIN_TRACK_PEAK", &["inf"]),
            ("R128_TRACK_GAIN", &["-1280"]),
        ],
        (10.0, 128, 2, 44100, 0, "MP3"),
        false,
    );
    let (_bad_md, bad_mf) = mapped("rg/03.mp3", &bad_info, &ctx, PidOptions::default());
    assert_eq!(
        bad_mf.rg_track_peak, None,
        "\"inf\" parses to nil, not infinity"
    );
    assert_eq!(
        bad_mf.rg_album_gain, None,
        "no album gain tag at all → absent"
    );
    assert_eq!(
        bad_mf.rg_track_gain,
        Some(0.0),
        "-1280/256 + 5 = 0.0: an encoded zero from the R128 arm, distinct from the absent album gain"
    );

    // Compilation with several album-artist ids becomes Various Artists (`fixAlbumArtist`,
    // `:486-500`); a single one is left alone. Oracle rows pin both arms.
    assert_oracle(
        "synthetic:compilation_album_artist",
        "\"albumArtist\": \"Various Artists\"",
    );
    assert_oracle(
        "synthetic:compilation_album_artist",
        "\"albumArtistId\": \"63sqASlAfjbGMuLP4JhnZU\"",
    );
    assert_oracle(
        "synthetic:album_artist_falls_back_to_track_artist",
        "\"albumArtist\": \"Sola Cuties\"",
    );

    let info = synthetic_info(
        &[
            ("ALBUM", &["Self Titled"]),
            ("ARTIST", &["Sola Cuties"]),
            ("TITLE", &["No Album Artist"]),
        ],
        (10.0, 128, 2, 44100, 0, "MP3"),
        false,
    );
    let (md, mf) = mapped("st/01.mp3", &info, &ctx, PidOptions::default());
    let album = project(&mf, &draft_stat(1, Some(at(1)), Some(at(0))), md.mappings());
    // Go's metadata.go sets AlbumArtistID from the artist when the album-artist tag is
    // absent, so fixAlbumArtist keeps the value rather than substituting; assert the
    // oracle's own pair.
    assert_eq!(
        (album.album_artist.as_str(), album.album_artist_id.as_str()),
        ("Sola Cuties", "08MR9jCxjkF4uFQLcdC15o")
    );
    assert_eq!(album.name, "Self Titled");

    // A compilation whose album artists differ in id → Various Artists. Built directly:
    // the mapping cannot produce two album-artist credits from one tag value here.
    let mut credits = RoleCredits::default();
    credits.add(
        nd_id::ROLE_ALBUM_ARTIST,
        [
            Credit {
                id: "a1".into(),
                name: "One".into(),
                ..Credit::default()
            },
            Credit {
                id: "a2".into(),
                name: "Two".into(),
                ..Credit::default()
            },
        ],
    );
    let mut mf = mf.clone();
    mf.compilation = true;
    mf.album_artist_id = "a1".into();
    mf.album_artist = "One".into();
    mf.participants = credits;
    let album = project(&mf, &draft_stat(1, Some(at(1)), Some(at(0))), md.mappings());
    assert_eq!(
        album.album_artist,
        nd_id::VARIOUS_ARTISTS,
        "a compilation with >1 album-artist id is VA"
    );
    assert_eq!(album.album_artist_id, nd_id::VARIOUS_ARTISTS_ID);

    // A non-compilation album with no album-artist id falls back to the first artist
    // (`:488-492`): here the artist credit's own name/id.
    let mut credits = RoleCredits::default();
    credits.add(
        nd_id::ROLE_ARTIST,
        [Credit {
            id: "art-1".into(),
            name: "Solo".into(),
            ..Credit::default()
        }],
    );
    let mut mf = mf.clone();
    mf.compilation = false;
    mf.album_artist_id = String::new();
    mf.album_artist = String::new();
    mf.participants = credits;
    let album = project(&mf, &draft_stat(1, Some(at(1)), Some(at(0))), md.mappings());
    assert_eq!(
        (album.album_artist.as_str(), album.album_artist_id.as_str()),
        ("Solo", "art-1"),
        "fallback to Participants.First(RoleArtist)"
    );

    // (c) CreatedAt is the RESOLVED birth — the same value the track record's
    // birth_time carries — never the mtime, the caller's stamp, or an epoch.
    // Go: `a.CreatedAt = older(a.CreatedAt, m.BirthTime)` (`model/mediafile.go:390`),
    // `older` starting from the zero time (`:466-480`).
    let info = synthetic_info(
        &[("ALBUM", &["No Birth"])],
        (10.0, 128, 2, 44100, 0, "MP3"),
        false,
    );
    let (md, mf) = mapped("nb/01.mp3", &info, &ctx, PidOptions::default());

    // Nonzero birth: created_at == that birth, not the (later) mtime.
    let stat = draft_stat(10, Some(at(1_700_000_000)), Some(at(1_500_000_000)));
    let album = project(&mf, &stat, md.mappings());
    assert_eq!(
        album.created_at,
        resolve(&stat),
        "created_at is the resolved birth, the same instant the record's birth_time holds"
    );
    assert_eq!(album.created_at, Some(at(1_500_000_000)));
    assert_ne!(album.created_at, Some(at(1_700_000_000)), "not the mtime");
    assert_ne!(album.created_at, Some(IMPORTED), "not the caller's stamp");

    // Birth unavailable with a substitute injected: created_at is the substitute, i.e.
    // Go's wrapper-resolved time.Now() (`core/storage/local/local.go:117-128`).
    let stat = draft_stat(10, Some(at(1_500_000_000)), None);
    let album = project(&mf, &stat, md.mappings());
    assert_eq!(
        album.created_at,
        Some(now()),
        "the injected substitute is the resolved birth"
    );
    assert_ne!(
        album.created_at,
        Some(at(1_500_000_000)),
        "the mtime is never a birth stand-in"
    );

    // All-zero birth and no substitute: Go's ToAlbum leaves CreatedAt zero
    // (`model/mediafile_test.go:123-134`), so the draft carries None (NULL in the DDL) —
    // not the caller's stamp, not the epoch.
    let stat = draft_stat(10, None, None);
    let album = album_draft_from_track(
        &mf,
        &stat,
        effective_birth_time(&stat, None),
        md.mappings(),
        IMPORTED,
    )
    .expect("projects");
    assert_eq!(
        album.created_at, None,
        "all-zero births stay zero (Go's BeZero arm)"
    );
    assert_eq!(
        album.updated_at, None,
        "an absent mtime stays absent (Go's newer(zero, zero) is zero)"
    );
    assert_eq!(
        album.imported_at, IMPORTED,
        "imported_at is the caller's stamp, separately"
    );
}

/// A folder with more than one track per album now runs SC-04's real `ToAlbum` loop.
/// SC-04 retired `ProjectionError::MultiTrackGroup`, so a two-track group projects
/// successfully — with an aggregated `SongCount`, not a singleton's.
#[test]
fn multi_track_group_aggregates() {
    let ctx = MappingContext::new(MappingOptions::default());
    let info = synthetic_info(
        &[("ALBUM", &["Same"]), ("ARTIST", &["A"]), ("TITLE", &["T1"])],
        (10.0, 128, 2, 44100, 0, "MP3"),
        false,
    );
    let (_md, mf_a) = mapped("same/01.mp3", &info, &ctx, PidOptions::default());
    let (_md, mf_b) = mapped("same/02.mp3", &info, &ctx, PidOptions::default());
    assert_eq!(
        mf_a.album_id, mf_b.album_id,
        "both tracks share one album id"
    );

    let drafts = album_drafts(
        &[&mf_a, &mf_b],
        facts_at(10, Some(at(1)), Some(at(0))),
        ctx.mappings(),
        IMPORTED,
    )
    .expect("SC-04 aggregates a two-track group");
    assert_eq!(drafts.len(), 1, "one draft for the group");
    assert_eq!(
        drafts[0].id, mf_a.album_id,
        "the draft is the grouped album"
    );
    assert_eq!(drafts[0].song_count, 2, "SongCount: len(mfs), not 1");
    assert_eq!(drafts[0].size, 20, "a.Size += m.Size across both members");
    assert_eq!(drafts[0].duration, 20.0, "duration is the group sum");
    assert_eq!(
        drafts[0].folder_ids,
        Some(vec![mf_a.folder_id.clone()]),
        "both members share one folder id, so Unique yields one entry"
    );
    assert_eq!(
        drafts[0].created_at,
        Some(at(0)),
        "oldest birth across the group (the injected substitute here)"
    );
    assert_eq!(
        drafts[0].updated_at,
        Some(at(1)),
        "newest mtime across the group"
    );
}

/// The facts lookup SC-05 now hands `album_drafts`: one bundle per group **member**,
/// with the birth resolved the same way the track record resolves it.
fn facts_at(
    size: i64,
    modified: Option<DateTime<Utc>>,
    birth_time: Option<DateTime<Utc>>,
) -> impl Fn(&MediaFile) -> Result<TrackFacts, AggregateError> {
    move |_track: &MediaFile| {
        let stat = draft_stat(size, modified, birth_time);
        Ok(TrackFacts {
            stat: stat.clone(),
            birth: effective_birth_time(&stat, Some(now())),
        })
    }
}

/// `singleton_artist_drafts_id_stable_sorted` — Go `createArtistsFromMediaFiles`
/// (`phase_1_folders.go:340-346`) merges every track's participants
/// (`model/participants.go:129-133`) and emits `Participants.AllArtists()` (`:150-163`):
/// a **stable** sort by id, then compaction on equal ids.
#[test]
fn singleton_artist_drafts_id_stable_sorted() {
    // Oracle row `synthetic:roles_and_mbid_sync`: 14 credits across 10 roles; artist and
    // albumartist share one id (The Beatles), so compaction yields 12 distinct artists.
    let source = "synthetic:roles_and_mbid_sync";
    assert_oracle(source, "\"name\": \"The Beatles\"");
    assert_oracle(source, "\"id\": \"08dZp5lAXmcWaUr33fbXmy\"");
    assert_oracle(source, "\"id\": \"3GMWq8RGcnuv4rLcrie0ll\"");

    let ctx = MappingContext::new(MappingOptions::default());
    let info = synthetic_info(
        &[
            ("ALBUM", &["Abbey Road"]),
            ("ARTIST", &["The Beatles"]),
            ("COMPOSER", &["John Lennon / Paul McCartney"]),
            ("COMPOSERSORT", &["Lennon, John"]),
            ("ARRANGER", &["Billy Strayhorn"]),
            ("ARRANGERSORT", &["Strayhorn, Billy"]),
            ("PRODUCER", &["George Martin"]),
            ("ENGINEER", &["Geoff Emerick"]),
            ("DIRECTOR", &["Tony Visconti"]),
            ("DJMIXER", &["ZZZ"]),
            ("REMIXER", &["YYY"]),
            ("MIXER", &["XXX"]),
            ("LYRICIST", &["Ringo Starr"]),
            (
                "MUSICBRAINZ_ARTISTID",
                &["b10abc96-3c5b-4a6c-a5f9-c70461532f84"],
            ),
        ],
        (180.0, 1000, 2, 96000, 24, "FLAC"),
        false,
    );
    let (_md, mf) = mapped("r/01.flac", &info, &ctx, PidOptions::default());

    let merged = merge_credits(&[&mf]);
    let artists = all_artists(&merged);

    // Stable sort by id, then compact by id: strictly increasing ids, no duplicates.
    let ids: Vec<&str> = artists.iter().map(|c| c.id.as_str()).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(
        ids, sorted,
        "AllArtists is sorted by id (slices.SortStableFunc)"
    );
    assert_eq!(
        ids.len(),
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        "compacted on id (slices.CompactFunc)"
    );

    // Dedup-by-id keeps the FIRST stable occurrence: artist (role-sorted before composer)
    // wins over the composer sharing the id, so the surviving credit is the artist's.
    let beatles_id = mf
        .participants
        .first(nd_id::ROLE_ARTIST)
        .expect("artist")
        .id
        .clone();
    assert!(
        ids.contains(&beatles_id.as_str()),
        "the shared id survives once"
    );
    let winner = artists
        .iter()
        .find(|c| c.id == beatles_id)
        .expect("the shared id");
    assert_eq!(
        winner.name, "The Beatles",
        "first stable occurrence wins the compaction"
    );

    // The draft carries exactly the columns Go's restricted artist Put updates
    // (phase_1_folders.go:403-405 / scanner_writes::ARTIST_UPDATE_COLUMNS).
    let drafts: Vec<ArtistRecord> = artist_drafts(&[&mf]);
    assert_eq!(
        drafts.len(),
        artists.len(),
        "one artist row per surviving id"
    );
    assert_eq!(
        drafts.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
        ids,
        "the drafts follow AllArtists' order"
    );
    for (draft, credit) in drafts.iter().zip(&artists) {
        assert_eq!(draft.name, credit.name, "name");
        assert_eq!(
            draft.sort_artist_name, credit.sort_artist_name,
            "sort_artist_name"
        );
        assert_eq!(
            draft.order_artist_name, credit.order_artist_name,
            "order_artist_name"
        );
        assert_eq!(draft.mbz_artist_id, credit.mbz_artist_id, "mbz_artist_id");
        assert_eq!(
            draft.created_at, None,
            "created_at is repo-stamped (artist_repo.rs:885-889)"
        );
        assert_eq!(draft.updated_at, None, "updated_at is repo-stamped");
        assert_eq!(
            draft.annotation,
            Default::default(),
            "annotation is a structs:\"-\" promoted embed"
        );
        assert_eq!(
            draft.image,
            nd_db::artwork_hydration::ItemImage::default(),
            "hydration-only, UNRESOLVED"
        );
        assert_eq!(draft.biography, "", "external metadata is filled later");
        assert_eq!(draft.small_image_url, "");
        assert_eq!(
            draft.similar_artists, "",
            "similar artists are a later fill"
        );
        assert!(draft.stats.is_empty(), "stats are PostScan-derived");
        assert_eq!(draft.album_count, 0, "counts are DB-17/DB-20's");
        assert_eq!(draft.song_count, 0, "counts are DB-17/DB-20's");
        assert_eq!(draft.size, 0, "size is a stats-derived column");
    }

    // Sort/order names are the paired values, not copies of the name.
    let lennon = drafts
        .iter()
        .find(|a| a.name == "John Lennon")
        .expect("the composer");
    assert_eq!(lennon.sort_artist_name, "Lennon, John");
    assert_eq!(
        lennon.order_artist_name, "john lennon",
        "order name is the lowercased name"
    );

    // AllNames over the merged credits: name then non-empty sort name, unique.
    let names = all_names(&merged);
    assert!(names.contains(&"Lennon, John".to_string()));
    assert_eq!(
        names.len(),
        names.iter().collect::<std::collections::HashSet<_>>().len(),
        "AllNames is deduplicated"
    );
    // The shared artist/albumartist id contributes its name once only.
    assert_eq!(
        names.iter().filter(|n| *n == "The Beatles").count(),
        1,
        "a repeated name is deduplicated"
    );
    assert_eq!(
        names
            .iter()
            .position(|n| n == "The Beatles")
            .expect("beatles"),
        0,
        "albumartist sorts first among the roles, so its name is first"
    );

    // Merge dedups by id+sub-role with first-occurrence-wins (`:135-147`): re-merging the
    // same track contributes nothing new.
    let twice = merge_credits(&[&mf, &mf]);
    assert_eq!(
        twice.iter().map(|(_r, l)| l.len()).sum::<usize>(),
        merged.iter().map(|(_r, l)| l.len()).sum::<usize>(),
        "Go's add() dedups by id+subRole, so a duplicate track adds nothing"
    );
    assert_eq!(all_artists(&twice).len(), artists.len());

    // Distinct ids with the same name are NOT compacted (Go compares ids, not names),
    // while AllNames still dedups the name.
    let mut credits = RoleCredits::default();
    credits.add(
        nd_id::ROLE_ARTIST,
        [
            Credit {
                id: "x1".into(),
                name: "Same".into(),
                ..Credit::default()
            },
            Credit {
                id: "x2".into(),
                name: "Same".into(),
                ..Credit::default()
            },
        ],
    );
    let out = all_artists(&credits);
    assert_eq!(
        out.len(),
        2,
        "slices.CompactFunc compares IDs only, so two ids survive"
    );
    assert_eq!(
        all_names(&credits),
        vec!["Same".to_string()],
        "AllNames dedups the name"
    );

    // Sub-roles: Go compacts on id alone, so two sub-roles with the same id collapse to
    // the first (one artist row per id, not one per credit).
    let mut credits = RoleCredits::default();
    credits.add_with_sub_role(
        nd_id::ROLE_PERFORMER,
        "Guitar",
        [Credit {
            id: "p1".into(),
            name: "Page".into(),
            ..Credit::default()
        }],
    );
    credits.add_with_sub_role(
        nd_id::ROLE_PERFORMER,
        "Vocals",
        [Credit {
            id: "p1".into(),
            name: "Page".into(),
            ..Credit::default()
        }],
    );
    let out = all_artists(&credits);
    assert_eq!(
        out.len(),
        1,
        "same id + different sub-role still compacts (Go compares ID)"
    );
    assert_eq!(
        out[0].sub_role, "Guitar",
        "the first stable occurrence's sub-role is kept"
    );
    let names = all_names(&credits);
    assert_eq!(
        names,
        vec!["Page".to_string()],
        "sub-roles never contribute names"
    );
    assert_eq!(
        artist_drafts(&[&mf]).len(),
        all_artists(&merged).len(),
        "one artist draft per surviving id"
    );

    // The name list is the same derivation nd-db performs on the record's
    // NamedParticipants, so drafts and records cannot disagree.
    let named: Vec<NamedParticipant> = nd_scanner::mapping_bridge::named_participants(&merged);
    assert_eq!(
        all_names(&credits),
        nd_db::album_repo::participant_names(&nd_scanner::mapping_bridge::named_participants(
            &credits
        ))
    );
    assert!(
        named
            .iter()
            .all(|p| !p.name.is_empty() || p.sort_name.is_empty()),
        "an empty name with a sort name is not produced by these credits"
    );
}

/// The album drafts entry point groups by album id like Go does, and a one-track folder
/// projects exactly one album.
#[test]
fn album_drafts_groups_by_album_id() {
    let ctx = MappingContext::new(MappingOptions::default());
    let a = synthetic_info(
        &[
            ("ALBUM", &["Album A"]),
            ("ARTIST", &["Artist"]),
            ("TITLE", &["T1"]),
        ],
        (10.0, 128, 2, 44100, 0, "MP3"),
        false,
    );
    let b = synthetic_info(
        &[
            ("ALBUM", &["Album B"]),
            ("ARTIST", &["Artist"]),
            ("TITLE", &["T2"]),
        ],
        (20.0, 128, 2, 44100, 0, "MP3"),
        false,
    );
    let (_md_a, mf_a) = mapped("folder/01.mp3", &a, &ctx, PidOptions::default());
    let (_md_b, mf_b) = mapped("folder/02.mp3", &b, &ctx, PidOptions::default());
    assert_ne!(mf_a.album_id, mf_b.album_id, "two albums in one folder");

    let drafts = album_drafts(
        &[&mf_a, &mf_b],
        facts_at(10, Some(at(1_700_000_000)), Some(at(1_600_000_000))),
        ctx.mappings(),
        IMPORTED,
    )
    .expect("two singleton groups project");
    assert_eq!(drafts.len(), 2, "one album draft per album group");
    let ids: Vec<&str> = drafts.iter().map(|d| d.id.as_str()).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(
        ids, sorted,
        "grouped by album id, emitted in album-id order"
    );
    assert_eq!(
        drafts.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        if mf_a.album_id < mf_b.album_id {
            ["Album A", "Album B"]
        } else {
            ["Album B", "Album A"]
        },
        "each album id gets exactly one draft"
    );
    let mut wanted = [mf_a.album_id.as_str(), mf_b.album_id.as_str()].to_vec();
    wanted.sort_unstable();
    assert_eq!(
        ids, wanted,
        "the draft ids are the two tracks' album ids, in id order"
    );
    assert!(
        drafts.iter().all(|d| d.song_count == 1),
        "each group is a singleton"
    );
    // Per-group duration, not a folder total: each draft carries its own track's
    // duration, matched by album id rather than by emission position.
    let duration_of = |album_id: &str| {
        drafts
            .iter()
            .find(|d| d.id == album_id)
            .expect("the group")
            .duration
    };
    assert_eq!(duration_of(&mf_a.album_id), 10.0, "album A's own duration");
    assert_eq!(
        duration_of(&mf_b.album_id),
        20.0,
        "album B's own duration, not the folder sum"
    );

    // An empty folder projects nothing (Go's loop over a zero-length group never runs).
    assert!(
        album_drafts(&[], facts_at(0, None, None), ctx.mappings(), IMPORTED)
            .expect("empty folder")
            .is_empty()
    );
}
