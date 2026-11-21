//! SC-03 — executed proof for the `MediaFile + FileStat → MediaFileRecord` bridge.
//!
//! Ground truth is the committed Go oracle
//! (`rust/crates/nd-mapping/tests/data/go_oracle.json`, produced by the Go harness in
//! `rust/crates/nd-mapping/tests/testdata/go_oracle/`), not this lane's output. Each
//! test cross-checks its expected values against the oracle text with [`assert_oracle`],
//! so a changed oracle value fails here even while the Rust code still "works".
//!
//! Real fixtures are read through the real `nd_tagreader::read_metadata` backend — never
//! a stubbed `Info`. Storage facts come from a real `LocalDir` snapshot of a `/tmp`
//! materialized copy, so `size`/`mtime`/`birth_time` are actual OS facts. Fixtures
//! resolve from `CARGO_MANIFEST_DIR`, never from an absolute path.
//!
//! Declared backend gaps are asserted as *declared*: `gaps()` in
//! `rust/crates/nd-mapping/tests/go_oracle.rs:367-425` records that the Rust backend
//! reports a different bitrate/bit-depth than Go's TagLib. The bridge passes the backend
//! value through verbatim (inventing Go's number is not this lane's to do), so the Rust
//! value and the Go oracle value are asserted on opposite sides of every gap.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, TimeZone, Utc};
use nd_db::mediafile_repo::MediaFileRecord;
use nd_db::sql_participations::{Participant, RoleBridgeError};
use nd_mapping::config::{MappingContext, MappingOptions, PidOptions};
use nd_mapping::mdfile::MediaFile;
use nd_mapping::metadata::FileStat as MappingFileStat;
use nd_mapping::metadata::Metadata;
use nd_mapping::participants::{Credit, RoleCredits};
use nd_scanner::mapping_bridge::{
    as_i32_checked, bridge_media_file, participants_from_credits, TrackStat,
};
use nd_scanner::storage::{BirthTimeUnavailable, Follow, LocalDir, LogicalPath, StatUnavailable};

/// The four real fixtures, tracked and hash-pinned.
const KNOWN_DIR: &str = "crates/nd-tagreader/tests/fixtures/real";

/// Go's oracle JSON, read once as text and sliced per source row.
fn oracle_text() -> &'static str {
    static TEXT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TEXT.get_or_init(|| {
        let path = repo_root().join("rust/crates/nd-mapping/tests/data/go_oracle.json");
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} must exist: {e}", path.display()))
    })
    .as_str()
}

/// Repo root (the directory holding `go.mod`), derived from this crate's manifest dir.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|p| p.join("go.mod").is_file())
        .expect("repo root containing go.mod")
        .to_path_buf()
}

/// The oracle row's JSON for `source`, sliced from its `"source"` key to the next row.
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

/// Assert the oracle row for `source` contains `needle` verbatim: the ground-truth
/// tie-in, transcribed from the oracle rather than from this lane's output.
#[track_caller]
fn assert_oracle(source: &str, needle: &str) {
    assert!(
        oracle_object(source).contains(needle),
        "oracle row {source} must contain {needle:?}"
    );
}

/// A real fixture from `fixtures/real` (mp3/flac/m4a/ogg).
fn known_fixture(extension: &str) -> PathBuf {
    repo_root()
        .join("rust")
        .join(KNOWN_DIR)
        .join(format!("known.{extension}"))
}

/// A Go-side fixture under `tests/fixtures` at the repo root.
fn go_fixture(name: &str) -> PathBuf {
    repo_root().join("tests/fixtures").join(name)
}

/// Owned `/tmp` root holding a copy of `src`, so a real stat can be taken (and an mtime
/// set) without touching tracked fixtures.
fn materialize(name: &str, src: &Path) -> (PathBuf, PathBuf) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let root = std::env::temp_dir().join(format!(
        "nd-scanner-bridge-{}-{}-{name}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&root).expect("create fixture root");
    let file = root.join(name);
    fs::copy(src, &file).expect("copy fixture");
    (root, file)
}

fn cleanup(root: &Path) {
    let _ = fs::remove_dir_all(root);
}

/// Real storage facts for a materialized copy, taken through the SC-02 storage seam and
/// projected by the bridge's own constructor.
fn track_stat(root: &Path, name: &str) -> TrackStat {
    let dir = LocalDir::from_path(root).expect("LocalDir::from_path");
    let logical = LogicalPath::parse(name).expect("logical path");
    let snapshot = dir.snapshot(&logical, Follow::Yes).expect("snapshot");
    assert!(snapshot.is_regular_file(), "{name} must be a regular file");
    TrackStat::from_storage_stat(&snapshot.stat).expect("a real file's stat is projectable")
}

/// The pipeline Go runs per file: read tags → `Metadata::new` → `ToMediaFile` → bridge
/// (`loadTagsFromFiles`, `scanner/phase_1_folders.go:292-324`).
fn bridge_real(
    file: &Path,
    logical: &str,
    stat: &TrackStat,
    library_id: i32,
    folder_id: &str,
    pid: PidOptions,
) -> (MediaFile, MediaFileRecord) {
    // The default clock an SC-05 caller would supply (a scan-start stamp); the
    // substitution tests pass `None` explicitly via `bridge_only_with_now`.
    bridge_real_at(file, logical, stat, library_id, folder_id, pid, Some(now()))
}

/// The same pipeline with an explicit clock, for the substitution tests.
#[allow(clippy::too_many_arguments)]
fn bridge_real_at(
    file: &Path,
    logical: &str,
    stat: &TrackStat,
    library_id: i32,
    folder_id: &str,
    pid: PidOptions,
    now: Option<DateTime<Utc>>,
) -> (MediaFile, MediaFileRecord) {
    let info = nd_tagreader::read_metadata(file)
        .unwrap_or_else(|e| panic!("{} must read: {e}", file.display()));
    let ctx = MappingContext::new(MappingOptions::default());
    let md = Metadata::new(logical, &info, stat.to_mapping_file_stat(), &ctx);
    let mf = md.to_media_file(library_id, folder_id, pid.effective());
    let rec =
        bridge_media_file(&md, &mf, stat, now).expect("a real fixture must bridge without error");
    (mf, rec)
}

/// Backend audio properties in the shape `nd-tagreader` reports them:
/// `(duration_seconds, bit_rate, channels, sample_rate, bit_depth, codec)`.
type Props = (f64, i64, i64, i64, i64, &'static str);

/// A synthetic `Info` (backend-shaped) for the narrowing/role edges.
fn synthetic_info(tags: &[(&str, &[&str])], props: Props, picture: bool) -> nd_tagreader::Info {
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

/// Map a synthetic `Info` and bridge it with an explicit stat.
fn bridge_synthetic(
    info: &nd_tagreader::Info,
    path: &str,
    stat: &TrackStat,
    library_id: i32,
    folder_id: &str,
    pid: PidOptions,
) -> (MediaFile, MediaFileRecord) {
    bridge_synthetic_at(info, path, stat, library_id, folder_id, pid, Some(now()))
}

/// The same with an explicit clock.
#[allow(clippy::too_many_arguments)]
fn bridge_synthetic_at(
    info: &nd_tagreader::Info,
    path: &str,
    stat: &TrackStat,
    library_id: i32,
    folder_id: &str,
    pid: PidOptions,
    now: Option<DateTime<Utc>>,
) -> (MediaFile, MediaFileRecord) {
    let ctx = MappingContext::new(MappingOptions::default());
    let md = Metadata::new(path, info, stat.to_mapping_file_stat(), &ctx);
    let mf = md.to_media_file(library_id, folder_id, pid.effective());
    let rec = bridge_media_file(&md, &mf, stat, now).expect("a synthetic record must bridge");
    (mf, rec)
}

/// Bridge with an explicit `Metadata`, for the error paths.
fn bridge_only(
    info: &nd_tagreader::Info,
    path: &str,
    mapping_stat: MappingFileStat,
    stat: &TrackStat,
    library_id: i32,
    folder_id: &str,
    pid: PidOptions,
) -> Result<MediaFileRecord, nd_scanner::mapping_bridge::BridgeError> {
    bridge_only_at(
        info,
        path,
        mapping_stat,
        stat,
        library_id,
        folder_id,
        pid,
        Some(now()),
    )
}

/// The same with an explicit clock, for the birth-substitution arms.
#[allow(clippy::too_many_arguments)]
fn bridge_only_at(
    info: &nd_tagreader::Info,
    path: &str,
    mapping_stat: MappingFileStat,
    stat: &TrackStat,
    library_id: i32,
    folder_id: &str,
    pid: PidOptions,
    now: Option<DateTime<Utc>>,
) -> Result<MediaFileRecord, nd_scanner::mapping_bridge::BridgeError> {
    let ctx = MappingContext::new(MappingOptions::default());
    let md = Metadata::new(path, info, mapping_stat, &ctx);
    let mf = md.to_media_file(library_id, folder_id, pid.effective());
    bridge_media_file(&md, &mf, stat, now)
}

/// The deterministic substitute instant an SC-05 caller would inject as its scan-start
/// stamp. Every test passes it explicitly: the bridge never reads a clock, so no test
/// can depend on wall time.
fn now() -> DateTime<Utc> {
    at(1_900_000_000)
}

/// A stat with real instants, for tests that need no filesystem.
fn stat(size: i64, modified_unix: i64, birth_unix: i64) -> TrackStat {
    TrackStat {
        size,
        modified: Some(Utc.timestamp_opt(modified_unix, 0).unwrap()),
        birth_time: Some(Utc.timestamp_opt(birth_unix, 0).unwrap()),
    }
}

fn at(unix: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(unix, 0).unwrap()
}

const DEFAULT_STAT: TrackStat = TrackStat {
    size: 100,
    modified: None,
    birth_time: None,
};

/// `mp3_flac_m4a_ogg_roundtrip_record_fields`.
///
/// Go oracle Its covered (`model/metadata/map_mediafile_test.go`, plus the oracle rows of
/// the four real fixtures): title/album fallbacks (`:23-45`), `trackNumber`/`discNumber`
/// through `NumAndTotal` (`model/metadata/metadata.go:145-156`, tested at `:114-118`),
/// the audio properties (`map_mediafile.go:68-79`, tested at `:176-190`), `Lyrics`
/// (`:162-190`), dates (`:203-220`), deprecated artist fields (`:93-102`), tag pruning
/// (`:104-112`), and the storage-derived `Size`/`BirthTime`/`UpdatedAt` (`:80-82`, filled
/// by `core/storage/local/local.go:90-109`).
///
/// Non-vacuity: each format asserts `duration`/`bit_rate`/`sample_rate`/`channels` as four
/// distinct values (a field swap fails), `size` as the real byte count (a
/// `FileStat::default()` fails), and `birth_time`/`updated_at` as the snapshot's own
/// instants (a scan timestamp fails).
#[test]
fn mp3_flac_m4a_ogg_roundtrip_record_fields() {
    struct Expect {
        ext: &'static str,
        oracle_source: &'static str,
        size: i64,
        duration: f32,
        /// The Rust backend's bitrate, transcribed from `gaps()`.
        rust_bit_rate: i32,
        /// The Go oracle's bitrate, for the opposite side of the declared gap.
        go_bit_rate: i64,
        sample_rate: i32,
        channels: i32,
        bit_depth: Option<i32>,
        go_bit_depth: Option<i32>,
        codec: &'static str,
        pid: &'static str,
        album_id: &'static str,
    }
    let expects = [
        Expect {
            ext: "mp3",
            oracle_source: "rust/crates/nd-tagreader/tests/fixtures/real/known.mp3",
            size: 7833,
            duration: 0.44,
            rust_bit_rate: 135,
            go_bit_rate: 136,
            sample_rate: 44100,
            channels: 2,
            bit_depth: None,
            go_bit_depth: None,
            codec: "MP3",
            pid: "60ZCo7w97fNb2P5XeOp1wW",
            album_id: "1GrQX6RbY8HNgNugy6o63Z",
        },
        Expect {
            ext: "flac",
            oracle_source: "rust/crates/nd-tagreader/tests/fixtures/real/known.flac",
            size: 14793,
            duration: 0.4,
            rust_bit_rate: 124,
            go_bit_rate: 125,
            sample_rate: 44100,
            channels: 2,
            bit_depth: Some(16),
            go_bit_depth: Some(16),
            codec: "",
            pid: "60ZCo7w97fNb2P5XeOp1wW",
            album_id: "1GrQX6RbY8HNgNugy6o63Z",
        },
        Expect {
            ext: "m4a",
            oracle_source: "rust/crates/nd-tagreader/tests/fixtures/real/known.m4a",
            size: 6270,
            duration: 0.42,
            rust_bit_rate: 94,
            go_bit_rate: 95,
            sample_rate: 44100,
            channels: 2,
            // Declared gap: Lofty reports 0 for AAC-in-MP4, TagLib reports 16. Go would
            // persist 16; the port keeps it absent rather than fabricating it.
            bit_depth: None,
            go_bit_depth: Some(16),
            codec: "AAC",
            pid: "6rA48DfWEf5XVoSsJCyNWu",
            album_id: "4xkV1gQrJ8zGZMSg80SENc",
        },
        Expect {
            ext: "ogg",
            oracle_source: "rust/crates/nd-tagreader/tests/fixtures/real/known.ogg",
            size: 5114,
            duration: 0.4,
            // Declared gap: TagLib derives an average Vorbis bitrate (30); Lofty reports
            // 0 when the nominal bitrate is unknown. Passed through, not defaulted to Go.
            rust_bit_rate: 0,
            go_bit_rate: 30,
            sample_rate: 44100,
            channels: 2,
            bit_depth: None,
            go_bit_depth: None,
            codec: "",
            pid: "60ZCo7w97fNb2P5XeOp1wW",
            album_id: "1GrQX6RbY8HNgNugy6o63Z",
        },
    ];

    for expect in expects {
        let name = format!("known.{}", expect.ext);
        let (root, file) = materialize(&name, &known_fixture(expect.ext));
        let st = track_stat(&root, &name);
        let (mf, rec) = bridge_real(
            &file,
            &format!("music/album/track.{}", expect.ext),
            &st,
            1,
            "FOLDER-ID",
            PidOptions::default(),
        );

        // ---- storage facts (Go's Info.FileInfo) --------------------------------
        assert_eq!(
            rec.size, expect.size,
            "{}: size must be the real byte count",
            expect.ext
        );
        assert_eq!(
            rec.size, st.size,
            "{}: size must come from the snapshot",
            expect.ext
        );
        assert_eq!(
            rec.birth_time,
            st.birth_time.expect("a real file has a birth time"),
            "{}: birth_time is the snapshot's",
            expect.ext
        );
        assert_eq!(
            rec.updated_at, st.modified,
            "{}: updated_at is the file's mtime",
            expect.ext
        );
        assert_ne!(
            rec.birth_time,
            at(0),
            "{}: birth_time must never be the epoch",
            expect.ext
        );
        assert_ne!(
            rec.updated_at,
            Some(at(0)),
            "{}: updated_at must never be the epoch",
            expect.ext
        );

        // ---- backend facts, four distinct values -------------------------------
        assert_eq!(
            rec.duration as f32, expect.duration,
            "{}: duration",
            expect.ext
        );
        assert_eq!(
            rec.bit_rate, expect.rust_bit_rate,
            "{}: bit_rate (backend value)",
            expect.ext
        );
        assert_eq!(
            rec.sample_rate, expect.sample_rate,
            "{}: sample_rate",
            expect.ext
        );
        assert_eq!(rec.channels, expect.channels, "{}: channels", expect.ext);
        assert_eq!(rec.bit_depth, expect.bit_depth, "{}: bit_depth", expect.ext);
        assert_eq!(rec.codec, expect.codec, "{}: codec", expect.ext);
        assert_eq!(rec.suffix, expect.ext, "{}: suffix", expect.ext);
        // Field-swap control: the numeric fields are pairwise distinct.
        assert!(
            (rec.duration - f64::from(rec.bit_rate)).abs() > f64::EPSILON,
            "{}: duration and bit_rate must not be swapped",
            expect.ext
        );
        assert_ne!(
            rec.bit_rate, rec.sample_rate,
            "{}: bit_rate and sample_rate must not be swapped",
            expect.ext
        );
        assert_ne!(
            rec.channels, rec.sample_rate,
            "{}: channels and sample_rate must not be swapped",
            expect.ext
        );

        // ---- declared gaps: the oracle's Go value is NOT what the record holds --
        assert_oracle(
            expect.oracle_source,
            &format!("\"bitRate\": {}", expect.go_bit_rate),
        );
        let go_depth = expect
            .go_bit_depth
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string());
        assert_oracle(expect.oracle_source, &format!("\"bitDepth\": {go_depth}"));
        assert_ne!(
            i64::from(rec.bit_rate),
            expect.go_bit_rate,
            "{}: the declared bitrate gap must still be a gap. If the backend now matches \
             Go, gaps() in nd-mapping/tests/go_oracle.rs changed and this table must be \
             updated deliberately",
            expect.ext
        );

        // ---- mapping facts -----------------------------------------------------
        assert_eq!(rec.title, "Real café", "{}: title", expect.ext);
        assert_eq!(rec.album, "Synthetic Album", "{}: album", expect.ext);
        assert_eq!(rec.artist, "Artist One", "{}: display artist", expect.ext);
        assert_eq!(
            rec.album_artist, "Album Artist",
            "{}: display album artist",
            expect.ext
        );
        assert_eq!(
            rec.artist_id, "5CFLlrcAjywbI577YUqwsD",
            "{}: deprecated artist id",
            expect.ext
        );
        assert_eq!(
            rec.album_artist_id, "5WdtjTSFGJHIMAh76SendS",
            "{}: deprecated album-artist id",
            expect.ext
        );
        assert_eq!(
            rec.order_artist_name, "artist one",
            "{}: order_artist_name",
            expect.ext
        );
        assert_eq!(
            rec.track_number, 2,
            "{}: track_number via NumAndTotal",
            expect.ext
        );
        assert_eq!(
            rec.disc_number, 1,
            "{}: disc_number via NumAndTotal",
            expect.ext
        );
        assert_eq!(rec.date, "2024-03-02", "{}: date", expect.ext);
        assert_eq!(rec.year, 2024, "{}: year", expect.ext);
        assert_eq!(
            rec.comment, "Comment one\nComment two",
            "{}: comment",
            expect.ext
        );
        assert_eq!(
            rec.pid, expect.pid,
            "{}: PID with the default specs",
            expect.ext
        );
        assert_eq!(rec.album_id, expect.album_id, "{}: album id", expect.ext);
        assert_eq!(
            rec.path,
            format!("music/album/track.{}", expect.ext),
            "{}: relative path is not rewritten",
            expect.ext
        );
        assert_eq!(rec.library_id, 1, "{}: configured library", expect.ext);
        assert_eq!(
            rec.folder_id, "FOLDER-ID",
            "{}: folder id from the caller",
            expect.ext
        );
        assert_eq!(rec.explicit_status, "", "{}: explicit status", expect.ext);
        assert_eq!(
            rec.rg_track_gain, None,
            "{}: no ReplayGain tags",
            expect.ext
        );
        assert!(!rec.compilation, "{}: compilation", expect.ext);

        // Cross-check the values against the oracle row itself (Go's values where the
        // backend agrees).
        assert_oracle(expect.oracle_source, "\"trackNumber\": 2");
        assert_oracle(expect.oracle_source, "\"discNumber\": 1");
        assert_oracle(expect.oracle_source, "\"date\": \"2024-03-02\"");
        assert_oracle(expect.oracle_source, "\"sampleRate\": 44100");
        assert_oracle(expect.oracle_source, "\"channels\": 2");
        assert_oracle(
            expect.oracle_source,
            &format!("\"pid\": \"{}\"", expect.pid),
        );
        assert_oracle(
            expect.oracle_source,
            &format!("\"albumId\": \"{}\"", expect.album_id),
        );

        // Lyrics: Go drops known.mp3's USLT (unaliased `uslt` key) while Lofty exposes
        // `lyrics:xxx` — a declared Rust-only gain (RUST_ONLY_TAGS).
        assert_oracle(expect.oracle_source, "\"lyrics\":");
        if expect.ext == "mp3" {
            assert!(
                rec.lyrics.contains("Real lyrics"),
                "{}: the Rust-only lyrics gain must survive the bridge",
                expect.ext
            );
            assert_oracle(expect.oracle_source, "\"lyrics\": \"[]\"");
            assert_ne!(
                rec.lyrics, "[]",
                "mp3 lyrics: Go drops them, Rust keeps them"
            );
        } else {
            assert!(
                rec.lyrics.contains("Real lyrics"),
                "{}: synced lyrics are carried verbatim",
                expect.ext
            );
        }

        // Tag pruning (`map_mediafile.go:104-112`): only album-level / non-first-class
        // tags survive.
        assert_eq!(
            rec.tags["genre"],
            ["Rock"],
            "{}: genre survives pruning",
            expect.ext
        );
        assert!(
            rec.tags.contains_key("disctotal"),
            "{}: album-level disc total survives",
            expect.ext
        );
        assert!(
            rec.tags.contains_key("tracktotal"),
            "{}: album-level track total survives",
            expect.ext
        );
        assert!(
            !rec.tags.contains_key("title"),
            "{}: first-class title is pruned",
            expect.ext
        );
        assert!(
            !rec.tags.contains_key("artist"),
            "{}: first-class artist is pruned",
            expect.ext
        );
        assert_oracle(expect.oracle_source, "\"genre\": [");

        // Participants: both roles, one credit each, column shape {id,name,subRole}.
        let roles: Vec<&str> = rec.participants.keys().map(|r| r.as_str()).collect();
        assert_eq!(
            roles,
            ["albumartist", "artist"],
            "{}: role keys in string-sorted order",
            expect.ext
        );
        assert_eq!(
            rec.participants[&nd_db::sql_participations::Role::from("artist")],
            [Participant {
                id: "5CFLlrcAjywbI577YUqwsD".into(),
                name: "Artist One".into(),
                sub_role: String::new(),
            }],
            "{}: the participants column keeps id/name/subRole only",
            expect.ext
        );
        assert_eq!(
            nd_scanner::mapping_bridge::all_names(&mf.participants),
            ["Album Artist", "Artist One"],
            "{}: AllNames in role-sorted first-seen order (albumartist sorts before artist): name, then its non-empty sort name",
            expect.ext
        );

        // Record-only and hydration-only fields stay zero (pins 5).
        assert_eq!(rec.id, "", "{}: id is SC-05/DB's", expect.ext);
        assert!(
            !rec.missing,
            "{}: a fresh import is not missing",
            expect.ext
        );
        assert_eq!(
            rec.created_at, None,
            "{}: created_at is DB-stamped",
            expect.ext
        );
        assert_eq!(rec.genre, "", "{}: genre is PostScan-derived", expect.ext);
        assert!(
            rec.genres.is_empty(),
            "{}: genres is PostScan-derived",
            expect.ext
        );
        assert_eq!(
            rec.probe_data, "",
            "{}: probe data is stream-side",
            expect.ext
        );

        cleanup(&root);
    }
}

/// `bpm_nil_negative_int32max` — Go `map_mediafile_test.go:119-135`: "maps the BPM tag
/// rounded to the nearest integer", "leaves BPM nil when the tag is absent", "leaves BPM
/// nil when the tag is zero or unparseable", "leaves BPM nil when the tag does not fit in
/// 32 bits", "leaves BPM nil when the tag is negative", "keeps the largest 32-bit BPM
/// value". Go's guard is `r > 0 && r <= MaxInt32` (`map_mediafile.go:40-44`).
#[test]
fn bpm_nil_negative_int32max() {
    let cases: &[(&str, Option<i32>)] = &[
        ("120.6", Some(121)),             // round half up to the nearest integer
        ("2147483647", Some(2147483647)), // the largest 32-bit value survives
        ("2147483648", None),             // MaxInt32 + 1 → nil, not wrapped
        ("4294967295", None),             // far outside int32 → nil
        ("0", None),                      // zero is not a BPM
        ("-120", None),                   // negative is rejected, never abs()'d
        ("fast", None),                   // unparseable → nil, not 0
    ];
    for (tag, want) in cases {
        let info = synthetic_info(&[("BPM", &[tag])], (10.0, 128, 2, 44100, 0, "MP3"), false);
        let (_mf, rec) = bridge_synthetic(
            &info,
            "x/01.mp3",
            &stat(100, 1_700_000_000, 1_600_000_000),
            1,
            "F",
            PidOptions::default(),
        );
        assert_eq!(rec.bpm, *want, "BPM tag {tag:?}");
    }
    // Absent tag → NULL (Go `BeNil`).
    let info = synthetic_info(&[], (10.0, 128, 2, 44100, 0, "MP3"), false);
    let (_mf, rec) = bridge_synthetic(
        &info,
        "x/01.mp3",
        &stat(100, 1_700_000_000, 1_600_000_000),
        1,
        "F",
        PidOptions::default(),
    );
    assert_eq!(rec.bpm, None, "an absent BPM stays NULL");

    // The oracle pins Go's own BPM value for the fixture that carries one.
    assert_oracle("tests/fixtures/test.mp3", "\"bpm\": 123");
    let file = go_fixture("test.mp3");
    let (root, copy) = materialize("test.mp3", &file);
    let st = track_stat(&root, "test.mp3");
    let (_mf, rec) = bridge_real(&copy, "test.mp3", &st, 1, "F", PidOptions::default());
    assert_eq!(
        rec.bpm,
        Some(123),
        "the real BPM fixture maps to Go's value"
    );
    cleanup(&root);
}

/// `mvin_movement_split` — Go `map_mediafile_test.go:160-174`: "splits an ID3 MVIN n/N
/// value into movement number and total", "prefers an explicit MOVEMENTTOTAL tag over the
/// n/N total". `movementnumber: ["1/4"]` yields `movementnumber: ["1"]` plus a derived
/// `movementtotal: ["4"]`, and `Movements()` pairs name/number/total.
#[test]
fn mvin_movement_split() {
    assert_oracle("synthetic:movements_split", "\"movementtotal\": [");
    let info = synthetic_info(
        &[
            ("ALBUM", &["Symphony Album"]),
            ("MOVEMENTNAME", &["Allegro"]),
            ("MOVEMENTNUMBER", &["1/4"]),
        ],
        (10.0, 128, 2, 44100, 0, "MP3"),
        false,
    );
    let (mf, rec) = bridge_synthetic(
        &info,
        "m/01.mp3",
        &stat(100, 1_700_000_000, 1_600_000_000),
        1,
        "F",
        PidOptions::default(),
    );
    // The canonical name is `movement` (mappings.yaml:235-236 aliases mvin/movement/
    // movementnumber/mvi into it), which is what the oracle row keys.
    assert_eq!(
        rec.tags["movement"],
        ["1"],
        "the n/N pair splits into the number"
    );
    assert_eq!(
        rec.tags["movementtotal"],
        ["4"],
        "the total is derived from the pair"
    );
    assert_eq!(rec.tags["movementname"], ["Allegro"]);
    assert_eq!(
        mf.movements(),
        [nd_mapping::mdfile::Movement {
            name: "Allegro".into(),
            number: 1,
            count: 4
        }],
        "Movements() pairs the three tag lists"
    );

    // An explicit total wins over the n/N total.
    let info = synthetic_info(
        &[
            ("MOVEMENTNAME", &["Allegro"]),
            ("MOVEMENT", &["2/4"]),
            ("MOVEMENTTOTAL", &["5"]),
        ],
        (10.0, 128, 2, 44100, 0, "MP3"),
        false,
    );
    let (mf, rec) = bridge_synthetic(
        &info,
        "m/02.mp3",
        &stat(100, 1_700_000_000, 1_600_000_000),
        1,
        "F",
        PidOptions::default(),
    );
    assert_eq!(
        rec.tags["movement"],
        ["2"],
        "the explicit pair still splits"
    );
    assert_eq!(
        rec.tags["movementtotal"],
        ["5"],
        "an explicit MOVEMENTTOTAL wins"
    );
    assert_eq!(
        mf.movements(),
        [nd_mapping::mdfile::Movement {
            name: "Allegro".into(),
            number: 2,
            count: 5
        }]
    );

    // The oracle row's own two-movement fixture: the cleaned tags keep the raw n/N pair
    // (only the mapped entity splits it), the mapped tags carry the split pair, and
    // Movements() pairs name/number/count.
    assert_oracle("synthetic:movements_split", "\"1/4\"");
    assert_oracle("synthetic:movements_split", "\"2/4\"");
    assert_oracle("synthetic:movements_split", "\"name\": \"I. Allegro\"");
    assert_oracle("synthetic:movements_split", "\"count\": 4");
    assert_oracle("synthetic:movements_split", "\"number\": 2");
}

/// `bit_depth_zero_to_nil` — Go `map_mediafile_test.go:181,186`: bit depth 24 is stored,
/// bit depth 0 (lossy codecs) leaves the pointer nil, so the column binds NULL rather
/// than a wrong 0 (`map_mediafile.go:73-75`).
#[test]
fn bit_depth_zero_to_nil() {
    for (depth, want) in [(24i64, Some(24)), (0, None)] {
        let info = synthetic_info(&[], (10.0, 128, 2, 44100, depth, "MP3"), false);
        let (_mf, rec) = bridge_synthetic(
            &info,
            "x/01.mp3",
            &stat(100, 1_700_000_000, 1_600_000_000),
            1,
            "F",
            PidOptions::default(),
        );
        assert_eq!(rec.bit_depth, want, "bit_depth {depth}");
    }
    // A negative depth is not Go's `> 0` arm either: absent, and no error.
    let info = synthetic_info(&[], (10.0, 128, 2, 44100, -1, "MP3"), false);
    let (_mf, rec) = bridge_synthetic(
        &info,
        "x/01.mp3",
        &stat(100, 1_700_000_000, 1_600_000_000),
        1,
        "F",
        PidOptions::default(),
    );
    assert_eq!(
        rec.bit_depth, None,
        "a negative depth stays absent like Go's zero pointer"
    );
}

/// `i32_overflow_is_typed_error_not_default` — subplan §4 "malformed is error, not
/// default". Go's `int(props.X)` (`map_mediafile.go:71-76`) cannot overflow because
/// TagLib reports `int`; Lofty reports `i64`, so the bridge's checked narrowing must error
/// rather than wrap (as `nd-mapping`'s `as_i32`, `map_mediafile.rs:281-283`, still does)
/// or fall back to 0.
#[test]
fn i32_overflow_is_typed_error_not_default() {
    let overflow_cases: &[(&str, Props)] = &[
        (
            "sample_rate",
            (10.0, 128, 2, i64::from(i32::MAX) + 1, 0, "MP3"),
        ),
        (
            "bit_rate",
            (10.0, i64::from(i32::MAX) + 1, 2, 44100, 0, "MP3"),
        ),
        (
            "channels",
            (10.0, 128, i64::from(i32::MAX) + 1, 44100, 0, "MP3"),
        ),
        (
            "bit_depth",
            (10.0, 128, 2, 44100, i64::from(i32::MAX) + 1, "MP3"),
        ),
    ];
    for (field, props) in overflow_cases {
        let info = synthetic_info(&[], *props, false);
        let st = stat(100, 1_700_000_000, 1_600_000_000);
        let err = bridge_only(
            &info,
            "x/01.mp3",
            st.to_mapping_file_stat(),
            &st,
            1,
            "F",
            PidOptions::default(),
        )
        .expect_err("an out-of-int32 backend value must not bridge");
        match err {
            nd_scanner::mapping_bridge::BridgeError::Overflow { field: got, value } => {
                assert_eq!(got, *field, "the error names the record column");
                assert_eq!(
                    value,
                    i64::from(i32::MAX) + 1,
                    "the error carries the offending value"
                );
                assert!(
                    !value.to_string().starts_with("0"),
                    "the error must not present a defaulted 0: {value}"
                );
            }
            other => panic!("{field}: expected Overflow, got {other:?}"),
        }
    }

    // A malformed integer *tag* is the mapping's `tag_number` arm (Go
    // `utils/number`: unparseable yields 0 and the error is dropped), so the bridge
    // receives a representable 0 and must NOT turn it into an error or a different
    // default. This pins the boundary: the bridge's typed error covers only what the
    // backend's i64 range makes unrepresentable.
    let ctx = MappingContext::new(MappingOptions::default());
    let info = synthetic_info(
        &[("TRACKNUMBER", &["abc"]), ("DISCNUMBER", &["1/abc"])],
        (10.0, 128, 2, 44100, 0, "MP3"),
        false,
    );
    let st = stat(100, 1_700_000_000, 1_600_000_000);
    let (_mf, rec) = bridge_synthetic(&info, "x/01.mp3", &st, 1, "F", PidOptions::default());
    assert_eq!(
        rec.track_number, 0,
        "Go drops the parse error and stores 0 for a malformed tag number"
    );
    assert_eq!(
        rec.disc_number, 1,
        "NumAndTotal takes the part before the slash"
    );

    // Prove the bridge is the checked one and nd-mapping's cast is not.
    let info = synthetic_info(
        &[],
        (10.0, 128, 2, i64::from(i32::MAX) + 1, 0, "MP3"),
        false,
    );
    let md = Metadata::new(
        "x/01.mp3",
        &info,
        stat(100, 1_700_000_000, 1_600_000_000).to_mapping_file_stat(),
        &ctx,
    );
    let mf = md.to_media_file(1, "F", PidOptions::default().effective());
    assert_eq!(
        mf.sample_rate,
        i32::MIN,
        "nd-mapping's as_i32 wraps (Go's int narrowing); the bridge must not"
    );
}

/// `storage_birth_time_is_honest_and_the_record_substitutes_the_injected_clock` —
/// pin 1, both layers, per the SC-03 P1 ruling.
///
/// **Storage-fact layer** (`nd-scanner/src/storage.rs:123,155`): `BirthTimeUnavailable`
/// stays an absence — never `UNIX_EPOCH`, never a scan timestamp. **Record layer**: Go's
/// wrapper substitutes `time.Now()` when the platform reported nothing
/// (`core/storage/local/local.go:117-128`) and `map_mediafile.go:81` reads that resolved
/// value (`metadata.go:90`), so the port reproduces that arm with a **caller-injected**
/// clock (D-TIME: the bridge never reads one). A substitute is required because
/// `media_file.birth_time` is NOT NULL; `Put` stamps `CreatedAt`, never `BirthTime`
/// (`mediafile_repository.go:184-186`).
#[test]
fn storage_birth_time_is_honest_and_the_record_substitutes_the_injected_clock() {
    let storage = nd_scanner::storage::FileStat {
        size: 4096,
        modified: Ok(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)),
        created: Err(BirthTimeUnavailable),
    };
    let st = TrackStat::from_storage_stat(&storage).expect("a missing birth time is not an error");
    assert_eq!(st.birth_time, None, "unavailable birth time must be None");
    assert_ne!(st.birth_time, Some(at(0)), "it must not become the epoch");

    // Mapping side: the absence renders as None, so MediaFile.birth_time stays None.
    let mapping_stat = st.to_mapping_file_stat();
    assert_eq!(mapping_stat.birth_time, None);
    assert_eq!(mapping_stat.size, Some(4096));
    assert_eq!(
        mapping_stat.mod_time.as_deref(),
        Some("2023-11-14T22:13:20+00:00")
    );

    // (a) BirthTimeUnavailable + an injected now → the record's birth_time IS the
    // injected instant: deterministic, no wall-clock read inside the bridge.
    let info = synthetic_info(&[], (10.0, 128, 2, 44100, 16, "MP3"), false);
    let rec = bridge_only_at(
        &info,
        "x/01.mp3",
        mapping_stat.clone(),
        &st,
        1,
        "F",
        PidOptions::default(),
        Some(now()),
    )
    .expect("Go's substitute arm must produce a record, not an error");
    assert_eq!(
        rec.birth_time,
        now(),
        "BirthTimeUnavailable → the injected stamp (Go's time.Now() arm)"
    );
    assert_ne!(rec.birth_time, at(0), "the substitute is never the epoch");
    assert_eq!(
        rec.updated_at, st.modified,
        "the mtime is untouched by the substitution"
    );

    // (b) No substitute supplied at all → a typed error naming the column, because the
    // bridge may not read a clock. This is the caller-contract arm, not a data defect.
    let err = bridge_only_at(
        &info,
        "x/01.mp3",
        mapping_stat,
        &st,
        1,
        "F",
        PidOptions::default(),
        None,
    )
    .expect_err("no storage birth and no injected substitute cannot yield a row");
    match err {
        nd_scanner::mapping_bridge::BridgeError::MissingBirthSubstitute { field } => {
            assert_eq!(
                field, "birth_time",
                "the error names the column SC-05 must supply"
            )
        }
        other => panic!("expected MissingBirthSubstitute, got {other:?}"),
    }

    // A missing mtime is the other honest absence: updated_at binds NULL, no error.
    let storage = nd_scanner::storage::FileStat {
        size: 4096,
        modified: Err(StatUnavailable),
        created: Ok(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000)),
    };
    let st = TrackStat::from_storage_stat(&storage).expect("a missing mtime is not an error");
    assert_eq!(st.modified, None);
    let (_mf, rec) = bridge_synthetic(&info, "x/01.mp3", &st, 1, "F", PidOptions::default());
    assert_eq!(rec.updated_at, None, "an absent mtime binds NULL");
    assert_eq!(
        Some(rec.birth_time),
        st.birth_time,
        "the birth time survives independently"
    );
    assert_eq!(rec.size, 4096, "size survives independently");

    // (c) A real storage birth time wins: the injected clock is IGNORED. Go's
    // substitution only fires when the platform reported nothing (local.go:117-123).
    let storage = nd_scanner::storage::FileStat {
        size: 4096,
        modified: Ok(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)),
        created: Ok(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000)),
    };
    let st = TrackStat::from_storage_stat(&storage).expect("a real birth time is projectable");
    let (_mf, rec) = bridge_synthetic_at(
        &info,
        "x/01.mp3",
        &st,
        1,
        "F",
        PidOptions::default(),
        Some(now()),
    );
    assert_eq!(
        rec.birth_time,
        st.birth_time.expect("the platform's birth time"),
        "an available birth time must not be replaced by the injected stamp"
    );
    assert_ne!(
        rec.birth_time,
        now(),
        "the injected clock is ignored when storage answered"
    );

    // A size past i64 is refused rather than wrapped.
    let storage = nd_scanner::storage::FileStat {
        size: u64::MAX,
        modified: Ok(SystemTime::now()),
        created: Err(BirthTimeUnavailable),
    };
    let err = TrackStat::from_storage_stat(&storage).expect_err("u64::MAX cannot be an i64 size");
    match err {
        nd_scanner::mapping_bridge::BridgeError::SizeOverflow { value } => {
            assert_eq!(value, u64::MAX)
        }
        other => panic!("expected SizeOverflow, got {other:?}"),
    }
}

/// ReplayGain coverage (P2): the record's four pointer-backed fields
/// (`rg_album_gain`/`rg_album_peak`/`rg_track_gain`/`rg_track_peak`) carry **distinct**
/// nonzero sentinels, and a genuinely encoded zero is distinguished from an absent tag.
/// Go: `mapGain`/`NullableFloat` (`map_mediafile.go:64-66`, `:121-140`) and
/// `nullableFloat` (`model/metadata/metadata.go:136-142`) return nil on a parse failure —
/// never 0 — so `Some(0.0)` can only mean "a zero was encoded".
#[test]
fn replay_gain_fields_are_distinct_and_zero_is_distinguished_from_absent() {
    // Distinct nonzero sentinels through all four fields; the oracle row is the source.
    assert_oracle("synthetic:replaygain_and_r128", "\"rgAlbumGain\": -7.5");
    assert_oracle("synthetic:replaygain_and_r128", "\"rgAlbumPeak\": 0.9");
    assert_oracle("synthetic:replaygain_and_r128", "\"rgTrackGain\": -3");
    assert_oracle("synthetic:replaygain_and_r128", "\"rgTrackPeak\": 1");
    let info = synthetic_info(
        &[
            ("ALBUM", &["RG"]),
            ("REPLAYGAIN_ALBUM_GAIN", &["-7.5 dB"]),
            ("REPLAYGAIN_ALBUM_PEAK", &["0.9"]),
            ("REPLAYGAIN_TRACK_GAIN", &["-3 dB"]),
            ("REPLAYGAIN_TRACK_PEAK", &["1.0"]),
        ],
        (10.0, 128, 2, 44100, 0, "MP3"),
        false,
    );
    let st = stat(100, 1_700_000_000, 1_600_000_000);
    let (_mf, rec) = bridge_synthetic(&info, "rg/01.mp3", &st, 1, "F", PidOptions::default());
    assert_eq!(
        rec.rg_album_gain,
        Some(-7.5),
        "dB suffix stripped (metadata.go:110-113)"
    );
    assert_eq!(rec.rg_album_peak, Some(0.9));
    assert_eq!(rec.rg_track_gain, Some(-3.0));
    assert_eq!(rec.rg_track_peak, Some(1.0));
    // Non-vacuity: no two of the four are equal, so a field swap or a dropped field
    // fails. f64 is not Hash, so the pairwise check is explicit.
    let vals = [
        rec.rg_album_gain,
        rec.rg_album_peak,
        rec.rg_track_gain,
        rec.rg_track_peak,
    ];
    for (i, a) in vals.iter().enumerate() {
        for (j, b) in vals.iter().enumerate() {
            if i < j {
                assert_ne!(a, b, "fields {i} and {j} must carry distinct sentinels");
            }
        }
    }

    // No ReplayGain tags at all → all four NULL, and no zero is fabricated.
    let info = synthetic_info(
        &[("ALBUM", &["No RG"])],
        (10.0, 128, 2, 44100, 0, "MP3"),
        false,
    );
    let (_mf, rec) = bridge_synthetic(&info, "rg/02.mp3", &st, 1, "F", PidOptions::default());
    assert_eq!(
        [
            rec.rg_album_gain,
            rec.rg_album_peak,
            rec.rg_track_gain,
            rec.rg_track_peak
        ],
        [None, None, None, None],
        "absent tags bind NULL, not 0.0"
    );

    // A genuine encoded zero vs an absent tag: the oracle row
    // `synthetic:replaygain_invalid_and_r128_fallback` pins the R128 Q7.8 arm
    // (`-1280 / 256 + 5` = 0.0) as 0 while the untagged album gain stays null, and
    // "NaN"/"inf" parse to nil rather than 0 or infinity (`nullableFloat`).
    assert_oracle(
        "synthetic:replaygain_invalid_and_r128_fallback",
        "\"rgTrackGain\": 0",
    );
    assert_oracle(
        "synthetic:replaygain_invalid_and_r128_fallback",
        "\"rgAlbumGain\": null",
    );
    let info = synthetic_info(
        &[
            ("ALBUM", &["Bad Gain"]),
            ("REPLAYGAIN_TRACK_GAIN", &["NaN"]),
            ("REPLAYGAIN_TRACK_PEAK", &["inf"]),
            ("R128_TRACK_GAIN", &["-1280"]),
        ],
        (10.0, 128, 2, 44100, 0, "MP3"),
        false,
    );
    let (_mf, rec) = bridge_synthetic(&info, "rg/03.mp3", &st, 1, "F", PidOptions::default());
    assert_eq!(
        rec.rg_track_gain,
        Some(0.0),
        "an encoded zero survives as Some(0.0)"
    );
    assert_eq!(
        rec.rg_album_gain, None,
        "an untagged field stays NULL, distinct from the zero above"
    );
    assert_eq!(
        rec.rg_track_peak, None,
        "\"inf\" parses to nil, not infinity"
    );
    assert_ne!(
        rec.rg_track_gain, rec.rg_album_gain,
        "zero and absent are different states"
    );

    // The real fixture carries all four and the oracle pins Go's exact values.
    assert_oracle("tests/fixtures/test.mp3", "\"rgAlbumGain\": 3.21518");
    let (root, copy) = materialize("test.mp3", &go_fixture("test.mp3"));
    let st = track_stat(&root, "test.mp3");
    let (_mf, rec) = bridge_real(&copy, "test.mp3", &st, 1, "F", PidOptions::default());
    assert_eq!(rec.rg_album_gain, Some(3.21518));
    assert_eq!(rec.rg_album_peak, Some(0.9125));
    assert_eq!(rec.rg_track_gain, Some(-1.48));
    assert_eq!(rec.rg_track_peak, Some(0.4512));
    cleanup(&root);
}

/// `unknown_role_is_error` — pin 3. Go's `Role.UnmarshalText` rejects a key outside
/// `AllRoles` (`model/participants.go:62-69`), so an unknown role id is a defect, not a
/// silently dropped credit. Consumes the existing checked bridge
/// (`nd-db/src/sql_participations.rs:627`), never a reimplementation.
#[test]
fn unknown_role_is_error() {
    let mut credits = RoleCredits::default();
    credits.add(
        nd_id::ROLE_ARTIST,
        [Credit {
            id: "a1".into(),
            name: "Artist".into(),
            ..Credit::default()
        }],
    );
    credits.add(
        nd_id::ROLE_INVALID,
        [Credit {
            id: "z9".into(),
            name: "Ghost".into(),
            ..Credit::default()
        }],
    );

    let err = participants_from_credits(&credits).expect_err("an unknown role must error");
    match err {
        nd_scanner::mapping_bridge::BridgeError::InvalidRole { source } => {
            assert_eq!(source, RoleBridgeError::Invalid("invalid".into()));
            assert_eq!(
                source.to_string(),
                "invalid role: invalid",
                "Go's UnmarshalText text"
            );
        }
        other => panic!("expected InvalidRole, got {other:?}"),
    }

    // A valid role set converts, and round-trips back to nd_id roles — proof the bridge
    // is the checked pair rather than a stringly-typed shortcut.
    let mut credits = RoleCredits::default();
    credits.add(
        nd_id::ROLE_COMPOSER,
        [Credit {
            id: "c1".into(),
            name: "C".into(),
            ..Credit::default()
        }],
    );
    let converted = participants_from_credits(&credits).expect("a valid role converts");
    assert_eq!(
        converted.keys().map(|r| r.as_str()).collect::<Vec<_>>(),
        ["composer"],
        "the DB role key is Go's JSON spelling"
    );
    assert_eq!(
        converted.values().flatten().count(),
        1,
        "the valid credit is not dropped"
    );
    let back = nd_db::sql_participations::into_id_role(converted.keys().next().expect("one role"))
        .expect("the DB role maps back to the domain role");
    assert_eq!(back, nd_id::ROLE_COMPOSER);
}

/// `participants_order_matches_go_all_names` — pin 3, Go `Participants.AllNames()`
/// (`model/participants.go:175-186`): each participant's name, then its non-empty sort
/// name; unique, in first-seen order over Go's nondeterministic map iteration (the port
/// pins role-sorted order). `sub_role` is never a name source. MBID pairing and sub-role
/// order follow `map_participants_test.go:586-653`.
#[test]
fn participants_order_matches_go_all_names() {
    // Oracle row `synthetic:roles_and_mbid_sync`: 14 credits across 10 roles; artist and
    // albumartist share one id, so AllArtists compacts to 12 (asserted in
    // tests/projection_singleton.rs).
    let source = "synthetic:roles_and_mbid_sync";
    assert_oracle(source, "\"name\": \"John Lennon\"");
    assert_oracle(source, "\"sort_artist_name\": \"Lennon, John\"");
    assert_oracle(
        source,
        "\"mbz_artist_id\": \"b10abc96-3c5b-4a6c-a5f9-c70461532f84\"",
    );

    let info = synthetic_info(
        &[
            ("ALBUM", &["Abbey Road"]),
            ("ARTIST", &["The Beatles"]),
            ("COMPOSER", &["John Lennon / Paul McCartney"]),
            ("COMPOSERSORT", &["Lennon, John"]),
            ("ARRANGER", &["Billy Strayhorn"]),
            ("PRODUCER", &["George Martin"]),
            (
                "MUSICBRAINZ_ARTISTID",
                &["b10abc96-3c5b-4a6c-a5f9-c70461532f84"],
            ),
        ],
        (180.0, 1000, 2, 96000, 24, "FLAC"),
        false,
    );
    let (mf, rec) = bridge_synthetic(
        &info,
        "r/01.flac",
        &stat(100, 1_700_000_000, 1_600_000_000),
        1,
        "F",
        PidOptions::default(),
    );

    // MBID pairing is per-role: the composer split by the artist joiner does NOT inherit
    // the artist MBID positionally (buildArtists, map_participants.go:156-174, pairs
    // sort/MBID tags with names within each role's own tag).
    let composers = mf
        .participants
        .get(nd_id::ROLE_COMPOSER)
        .expect("composer role");
    assert_eq!(
        composers
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["John Lennon", "Paul McCartney"],
        "the joiner splits one value into two credits in tag order"
    );
    assert_eq!(
        composers[0].sort_artist_name, "Lennon, John",
        "the sort tag pairs positionally"
    );
    assert_eq!(
        composers[1].sort_artist_name, "",
        "no second sort value → empty, not reused"
    );
    assert_eq!(
        composers[0].mbz_artist_id, "",
        "no composer MBID tag → empty"
    );
    assert_eq!(
        mf.participants
            .first(nd_id::ROLE_ARTIST)
            .expect("artist")
            .mbz_artist_id,
        "b10abc96-3c5b-4a6c-a5f9-c70461532f84",
        "the artist's own MBID is paired with its name"
    );
    assert_eq!(
        mf.participants
            .first(nd_id::ROLE_ALBUM_ARTIST)
            .expect("albumartist")
            .id,
        mf.participants
            .first(nd_id::ROLE_ARTIST)
            .expect("artist")
            .id,
        "Go's album-artist fallback reuses the artist id"
    );

    // AllNames: name then non-empty sort name, deduplicated, role-sorted first-seen.
    let names = nd_scanner::mapping_bridge::all_names(&mf.participants);
    let pos = |n: &str| {
        names
            .iter()
            .position(|v| v == n)
            .unwrap_or_else(|| panic!("{n} in AllNames: {names:?}"))
    };
    assert_eq!(
        pos("John Lennon") + 1,
        pos("Lennon, John"),
        "sort name immediately follows its participant's name"
    );
    assert_eq!(
        names.len(),
        names.iter().collect::<std::collections::HashSet<_>>().len(),
        "AllNames is deduplicated"
    );
    assert!(names.contains(&"George Martin".to_string()));
    assert!(
        !names.iter().any(|n| n.contains('(') || n.contains('/')),
        "sub-role text is never a name source"
    );
    assert!(
        names.iter().all(|n| !n.is_empty()),
        "no empty name reaches the FTS input"
    );

    // The record carries the JSON column and the same-derived name list side by side.
    assert_eq!(
        rec.participant_names
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        mf.participants
            .iter()
            .flat_map(|(_r, list)| list.iter().map(|c| c.name.as_str()))
            .collect::<Vec<_>>(),
        "NamedParticipant names are the credits' names in the same order"
    );
    assert!(
        rec.participant_names
            .iter()
            .any(|p| p.sort_name == "Lennon, John"),
        "sort names ride on the record, not only in the derived list"
    );
    assert!(
        rec.participant_names.iter().all(|p| p.sub_role.is_empty()),
        "this fixture has no sub-roles"
    );

    // Sub-role pairing with MBIDs (Go `map_participants_test.go:613-631`): each
    // `PERFORMER:<sub>` value takes the `MUSICBRAINZ_PERFORMERID:<sub>` value at the same
    // index, and the sub-role is title-cased.
    assert_oracle(
        "synthetic:performer_pairs_and_subroles",
        "\"sub_role\": \"Guitar\"",
    );
    assert_oracle(
        "synthetic:performer_pairs_and_subroles",
        "\"mbz_artist_id\": \"mbid-guitar-1\"",
    );
    let info = synthetic_info(
        &[
            ("ARTIST", &["Led Zeppelin"]),
            ("PERFORMER:guitar", &["Jimmy Page"]),
            ("PERFORMER:vocals", &["Robert Plant"]),
            ("MUSICBRAINZ_PERFORMERID:guitar", &["mbid-guitar-1"]),
            ("MUSICBRAINZ_PERFORMERID:vocals", &["mbid-vocals-1"]),
        ],
        (476.0, 900, 2, 48000, 24, "FLAC"),
        false,
    );
    let (mf, rec) = bridge_synthetic(
        &info,
        "p/01.flac",
        &stat(100, 1_700_000_000, 1_600_000_000),
        1,
        "F",
        PidOptions::default(),
    );
    let performers = mf
        .participants
        .get(nd_id::ROLE_PERFORMER)
        .expect("performer role");
    assert_eq!(
        performers
            .iter()
            .map(|c| (
                c.name.as_str(),
                c.sub_role.as_str(),
                c.mbz_artist_id.as_str()
            ))
            .collect::<Vec<_>>(),
        [
            ("Jimmy Page", "Guitar", "mbid-guitar-1"),
            ("Robert Plant", "Vocals", "mbid-vocals-1"),
        ],
        "sub-role order follows the pair-tag order, MBIDs paired per sub-role"
    );
    // The column keeps sub_role and nothing else (`persistence/sql_participations.go:13-17`).
    assert_eq!(
        rec.participants[&nd_db::sql_participations::Role::from("performer")],
        [
            Participant {
                id: performers[0].id.clone(),
                name: "Jimmy Page".into(),
                sub_role: "Guitar".into()
            },
            Participant {
                id: performers[1].id.clone(),
                name: "Robert Plant".into(),
                sub_role: "Vocals".into()
            },
        ],
        "sub-role is column data; sort/order/MBID are not"
    );
    assert!(
        nd_scanner::mapping_bridge::all_names(&mf.participants)
            .iter()
            .all(|n| !n.contains("Guitar")),
        "the sub-role is not part of AllNames"
    );
}

/// `genre_stays_empty_at_scan_side` — pin 4. Go derives genre on the READ side only:
/// `dbMediaFile.PostScan` calls `Tags.ToGenres()` and overwrites the column
/// (`persistence/mediafile_repository.go:64`, `model/tag.go:138-148`); `grep -n Genre
/// scanner/*.go model/metadata/*.go` returns zero hits. So the scan-side record carries an
/// empty genre even when the genre tag is present. DB-17/DB-20 own the counts.
#[test]
fn genre_stays_empty_at_scan_side() {
    let source = "rust/crates/nd-tagreader/tests/fixtures/real/known.flac";
    assert_oracle(source, "\"genre\": [");
    let (root, file) = materialize("known.flac", &known_fixture("flac"));
    let st = track_stat(&root, "known.flac");
    let (_mf, rec) = bridge_real(&file, "a/b.flac", &st, 1, "F", PidOptions::default());
    assert_eq!(rec.genre, "", "the scan side never derives genre");
    assert!(rec.genres.is_empty(), "genres is a read-side derivation");
    assert_eq!(
        rec.tags["genre"],
        ["Rock"],
        "the tag itself is still persisted"
    );
    cleanup(&root);

    // A split, multi-value genre behaves the same: the tags carry it, the column does not.
    assert_oracle("synthetic:genre_split_and_dupes", "\"genre\": [");
    let info = synthetic_info(
        &[("GENRE", &["Rock; Pop/Rock, Rock"]), ("TITLE", &["Genres"])],
        (10.0, 128, 2, 44100, 0, "MP3"),
        false,
    );
    let (_mf, rec) = bridge_synthetic(
        &info,
        "s/01.mp3",
        &stat(100, 1_700_000_000, 1_600_000_000),
        1,
        "F",
        PidOptions::default(),
    );
    assert_eq!(
        rec.tags["genre"],
        ["Rock", "Pop"],
        "split + dedup happen in the mapping, not here"
    );
    assert_eq!(rec.genre, "", "no scan-side genre derivation");
    assert!(rec.genres.is_empty());
}

/// `artwork_fields_zero_even_with_has_picture` — pin 5. `has_cover_art` is the mapping's
/// `Info.HasPicture` (`map_mediafile.go:69`); `image`/`album_image` zero means
/// UNRESOLVED, not known-absent (`nd-db/src/artwork_hydration.rs:90-118`, the DB-11
/// declared deviation). The artwork queue leg is DB-14's, not the record's.
#[test]
fn artwork_fields_zero_even_with_has_picture() {
    // With an embedded picture.
    let source = "tests/fixtures/test.mp3";
    assert_oracle(source, "\"hasCoverArt\": true");
    let (root, copy) = materialize("test.mp3", &go_fixture("test.mp3"));
    let st = track_stat(&root, "test.mp3");
    let (mf, rec) = bridge_real(&copy, "test.mp3", &st, 1, "F", PidOptions::default());
    assert!(mf.has_cover_art, "the backend reports an embedded picture");
    assert!(rec.has_cover_art, "that fact reaches the record");
    assert_eq!(
        rec.image,
        nd_db::artwork_hydration::ItemImage::default(),
        "image stays UNRESOLVED, not resolved"
    );
    assert_eq!(
        rec.album_image,
        nd_db::artwork_hydration::ItemImage::default(),
        "album_image stays UNRESOLVED"
    );
    assert!(
        !rec.image.image_absent,
        "zero must never claim known-absent"
    );
    assert_eq!(rec.image.image_hash, "", "no hash is invented");
    assert_eq!(rec.size, 64223, "real size from the snapshot");
    assert_oracle(source, "\"size\": 64223");
    cleanup(&root);

    // Without an embedded picture: has_cover_art false, images still zero (unresolved).
    let source = "rust/crates/nd-tagreader/tests/fixtures/real/known.mp3";
    assert_oracle(source, "\"hasCoverArt\": false");
    let (root, copy) = materialize("known.mp3", &known_fixture("mp3"));
    let st = track_stat(&root, "known.mp3");
    let (mf, rec) = bridge_real(&copy, "known.mp3", &st, 1, "F", PidOptions::default());
    assert!(!mf.has_cover_art, "no embedded picture");
    assert!(
        !rec.has_cover_art,
        "the record does not fabricate cover art"
    );
    assert_eq!(rec.image, nd_db::artwork_hydration::ItemImage::default());
    assert_eq!(
        rec.album_image,
        nd_db::artwork_hydration::ItemImage::default()
    );
    assert!(
        !rec.image.image_absent && !rec.album_image.image_absent,
        "still unresolved, not absent"
    );
    cleanup(&root);
}

/// `pid_effective_with_global_fallback` — pin 6. `PidOptions::effective_with`
/// (`nd-mapping/src/config.rs:154-196`) had ZERO callers in `src` or tests; this is its
/// first executed use. Go reads exactly `lib.ID` + `lib.EffectivePID()`
/// (`model/metadata/map_mediafile.go:20,89`, `model/library.go:47-52`): a library's own
/// spec wins, else the global `conf.Server.PID`, else the Go constants. There is no
/// `EnableFingerprinting`/`SubtitleSuffix` in this Go tree (scout grep-verified), and
/// `append_subtitle` is a Subsonic marshal-time knob
/// (`mediafile_repo.rs:1345-1364`, `scanner_writes.rs:387`) — out of this scope.
#[test]
fn pid_effective_with_global_fallback() {
    let legacy = PidOptions {
        album: "album_legacy".into(),
        track: "track_legacy".into(),
    };

    // Oracle rows: the same logical path in library 1 vs 2 with the legacy specs
    // produces different PIDs and album ids (the library id is prefixed).
    assert_oracle(
        "synthetic:pid_track_legacy",
        "\"pid\": \"1Ei9Y47OIPQcgys7sGiGuT\"",
    );
    assert_oracle(
        "synthetic:pid_track_legacy_lib2",
        "\"pid\": \"1vkpmQxxynYYkFpKayRrt0\"",
    );
    assert_oracle(
        "synthetic:pid_track_legacy",
        "\"albumId\": \"7b33IZqhcrOpSU5IzavwvE\"",
    );

    let st = stat(100, 1_700_000_000, 1_600_000_000);
    let ctx = MappingContext::new(MappingOptions::default());
    let info = synthetic_info(
        &[
            ("ALBUM", &["Legacy Album"]),
            ("ARTIST", &["Legacy Artist"]),
            ("TITLE", &["Legacy Track"]),
        ],
        (10.0, 128, 2, 44100, 0, "MP3"),
        false,
    );
    let md = Metadata::new(
        "albums/legacy/01.mp3",
        &info,
        st.to_mapping_file_stat(),
        &ctx,
    );

    // A library with its own spec: effective_with returns that spec verbatim.
    let config = legacy.effective_with(&legacy);
    assert_eq!(
        (config.album, config.track),
        ("album_legacy", "track_legacy")
    );
    let mf = md.to_media_file(1, "F", config);
    assert_eq!(mf.pid, "1Ei9Y47OIPQcgys7sGiGuT", "library 1 legacy PID");
    assert_eq!(mf.album_id, "7b33IZqhcrOpSU5IzavwvE");

    // The zero-caller leg: the library has NO spec of its own, so the global config's
    // specs apply — same result through the fallback path.
    let empty = PidOptions::default();
    let config = empty.effective_with(&legacy);
    assert_eq!(
        config.track, "track_legacy",
        "an empty library spec falls back to the global"
    );
    assert_eq!(config.album, "album_legacy");
    let mf_fallback = md.to_media_file(1, "F", config);
    assert_eq!(
        mf_fallback.pid, mf.pid,
        "the global fallback must produce the same PID as an own spec"
    );
    assert_eq!(mf_fallback.album_id, mf.album_id);

    // An empty global falls back to the Go constants, identical to effective().
    let empty_global = PidOptions::default();
    let config = empty.effective_with(&empty_global);
    assert_eq!(config.track, nd_id::DEFAULT_TRACK_PID);
    assert_eq!(config.album, nd_id::DEFAULT_ALBUM_PID);
    let defaults = empty.effective();
    assert_eq!(
        (config.track, config.album),
        (defaults.track, defaults.album)
    );

    // Library scoping reaches the record: library_id is the configured library, not
    // DEFAULT_LIBRARY_ID, and the folder id is the caller's, not "".
    let mf2 = md.to_media_file(2, "F", legacy.effective_with(&legacy));
    assert_eq!(mf2.pid, "1vkpmQxxynYYkFpKayRrt0", "library 2 legacy PID");
    let rec2 = bridge_media_file(&md, &mf2, &st, Some(now())).expect("bridges");
    assert_eq!(
        rec2.library_id, 2,
        "the configured library reaches the record as i64"
    );
    assert_eq!(rec2.pid, "1vkpmQxxynYYkFpKayRrt0");
    assert_ne!(
        rec2.library_id,
        i64::from(nd_mapping::mdfile::DEFAULT_LIBRARY_ID),
        "never the DEFAULT_LIBRARY_ID placeholder"
    );
    assert_eq!(rec2.folder_id, "F", "never the empty-folder-id placeholder");

    // A folder-spec album PID (Go `map_mediafile_test.go`: byFolder != byTags, and the
    // album id becomes the track pid).
    assert_oracle(
        "synthetic:pid_custom_folder",
        "\"pid\": \"0bOfw5n9UORBpw7KY08Ak5\"",
    );
    let folder_specs = PidOptions {
        album: "folder".into(),
        track: "folder|title".into(),
    };
    let info = synthetic_info(
        &[
            ("ALBUM", &["Abbey Road"]),
            ("ARTIST", &["The Beatles"]),
            ("TITLE", &["Come Together"]),
        ],
        (10.0, 128, 2, 44100, 0, "MP3"),
        false,
    );
    let md = Metadata::new(
        "music/beatles/abbey-road/01.mp3",
        &info,
        st.to_mapping_file_stat(),
        &ctx,
    );
    let mf = md.to_media_file(1, "F", folder_specs.effective_with(&PidOptions::default()));
    assert_eq!(mf.pid, "0bOfw5n9UORBpw7KY08Ak5");
    assert_eq!(
        mf.album_id, mf.pid,
        "the folder spec makes the album id equal to the track pid"
    );
    let rec = bridge_media_file(&md, &mf, &st, Some(now())).expect("bridges");
    assert_eq!(
        rec.pid, mf.pid,
        "the record carries the effective-PID value verbatim"
    );
    assert_eq!(rec.album_id, mf.album_id);
}

/// `wma_surfaces_as_error` — the committed error-path expectation in the style of
/// `nd-mapping/tests/go_oracle.rs:135` (`ASF_FIXTURE`). `nd-tagreader` has no ASF
/// support, so no `Info` exists — therefore no `Metadata`, no `MediaFile`, and no record:
/// nothing is fabricated for an unsupported format.
#[test]
fn wma_surfaces_as_error() {
    let file = go_fixture("test.wma");
    let err = nd_tagreader::read_metadata(&file)
        .expect_err("ASF/WMA must not read: the backend has no ASF support");
    assert!(!err.to_string().is_empty(), "the error must carry a reason");

    // A caller that hand-builds an Info cannot produce Go's WMA row either: with no real
    // storage facts and no injected substitute the record's NOT NULL birth_time refuses,
    // rather than writing a zero-filled row for a file the backend cannot read.
    let no_birth = DEFAULT_STAT;
    let info = synthetic_info(
        &[("TITLE", &["WMA Track"])],
        (1.02, 128, 1, 44100, 16, "WMA2"),
        true,
    );
    let err = bridge_only_at(
        &info,
        "tests/fixtures/test.wma",
        no_birth.to_mapping_file_stat(),
        &no_birth,
        1,
        "F",
        PidOptions::default(),
        None,
    )
    .expect_err("no storage facts and no substitute means no row, not a zero-filled row");
    assert!(
        matches!(
            err,
            nd_scanner::mapping_bridge::BridgeError::MissingBirthSubstitute { .. }
        ),
        "{err:?}"
    );
}

/// Pin 7: `mdfile::Tags` (`BTreeMap<String, Vec<String>>`) → `DbTags`
/// (`nd-db/src/sql_tags.rs:50`) is a free conversion, so a case-sensitive multi-value
/// fixture must round-trip with value order intact. Ordering follows the oracle's
/// documented rule: Go's map order is nondeterministic
/// (`nd-mapping/tests/go_oracle.rs:442-455`, `order_insensitive` at `:456`), and the
/// port's `BTreeMap` pins name order instead.
#[test]
fn tags_roundtrip_is_value_order_preserving() {
    assert_oracle("synthetic:genre_split_and_dupes", "\"genre\": [");
    let info = synthetic_info(
        &[
            ("GENRE", &["Rock; Pop/Rock, Rock"]),
            ("MOOD", &["Happy", "Chill"]),
            ("ALBUMVERSION", &["Deluxe"]),
            ("TRACKTOTAL", &["10"]),
            ("TITLE", &["Tagged"]),
            ("Language", &["English"]),
        ],
        (10.0, 128, 2, 44100, 0, "MP3"),
        false,
    );
    let (mf, rec) = bridge_synthetic(
        &info,
        "ci/01.mp3",
        &stat(100, 1_700_000_000, 1_600_000_000),
        1,
        "F",
        PidOptions::default(),
    );
    // Split + dedup are the mapping's (`model/metadata/metadata.go:317-343`); the bridge
    // preserves value order verbatim.
    assert_eq!(
        rec.tags["genre"],
        ["Rock", "Pop"],
        "value order preserved, duplicates dropped upstream"
    );
    assert_eq!(rec.tags["mood"], ["Happy", "Chill"]);
    assert_eq!(rec.tags["albumversion"], ["Deluxe"]);
    assert_eq!(rec.tags["tracktotal"], ["10"]);
    // Case-sensitive canonicalization happens in the mapping (lowercased tag names).
    assert!(
        rec.tags.contains_key("language"),
        "a mixed-case tag name is canonicalized, not dropped"
    );
    // First-class, non-album tags are pruned by the mapping (`map_mediafile.go:104-112`).
    assert!(
        !rec.tags.contains_key("title"),
        "the first-class title tag is pruned"
    );
    // The conversion is identity over the mapping's map — same keys, same values.
    assert_eq!(
        rec.tags, mf.tags,
        "DbTags is the same name→ordered-values shape"
    );
    // Deterministic key order (the port's pinned order, not Go's map order).
    let keys: Vec<&str> = rec.tags.keys().map(String::as_str).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted, "tag names are emitted in sorted order");
    // Non-vacuity: a value swap inside one name would fail here.
    assert_ne!(
        rec.tags["mood"],
        ["Chill", "Happy"],
        "value order is not reversed"
    );
}

/// Storage facts are authoritative, and a mapping DTO that disagrees about the same file
/// is a caller defect rather than something to reconcile silently.
#[test]
fn storage_fact_mismatch_is_an_error() {
    let info = synthetic_info(&[], (10.0, 128, 2, 44100, 16, "MP3"), false);
    let st = stat(2000, 1_700_000_000, 1_600_000_000);
    // A mapping DTO carrying a different size (tags read from another file).
    let err = bridge_only(
        &info,
        "x/01.mp3",
        stat(999, 1_700_000_000, 1_600_000_000).to_mapping_file_stat(),
        &st,
        1,
        "F",
        PidOptions::default(),
    )
    .expect_err("size must agree between the mapping and the snapshot");
    match err {
        nd_scanner::mapping_bridge::BridgeError::StorageFactMismatch {
            field,
            mapping,
            storage,
        } => {
            assert_eq!(field, "size");
            assert_eq!(mapping, "999");
            assert_eq!(storage, "2000");
        }
        other => panic!("expected StorageFactMismatch, got {other:?}"),
    }

    // A mismatched birth time is caught the same way (the mapping is not a second source).
    let err = bridge_only(
        &info,
        "x/01.mp3",
        MappingFileStat {
            size: Some(2000),
            mod_time: Some("2023-11-14T22:13:20+00:00".into()),
            birth_time: Some("2019-01-01T00:00:00+00:00".into()),
        },
        &st,
        1,
        "F",
        PidOptions::default(),
    )
    .expect_err("birth time must agree");
    assert!(
        matches!(err, nd_scanner::mapping_bridge::BridgeError::StorageFactMismatch { field, .. } if field == "birth_time"),
        "{err:?}"
    );

    // (P2 regression) A malformed mapping mtime errors **regardless of the storage
    // side**: with no storage mtime the old code parsed nothing and quietly returned
    // `updated_at: None`, discarding the caller's unparseable value.
    let no_mtime = TrackStat {
        size: 2000,
        modified: None,
        birth_time: Some(at(1_600_000_000)),
    };
    let err = bridge_only(
        &info,
        "x/01.mp3",
        MappingFileStat {
            size: Some(2000),
            mod_time: Some("not-a-timestamp".into()),
            birth_time: Some("2020-09-13T12:26:40+00:00".into()),
        },
        &no_mtime,
        1,
        "F",
        PidOptions::default(),
    )
    .expect_err("a malformed mapping mtime must not be discarded when storage has none");
    assert!(
        matches!(
            err,
            nd_scanner::mapping_bridge::BridgeError::MalformedTimestamp { field, .. } if field == "updated_at"
        ),
        "{err:?}"
    );

    // (P2 decision, parent lean) A VALID mapping mtime with NO storage mtime is a
    // mismatch, not a permitted omission: Go's wrapper never substitutes an mtime
    // (`local.go:90-109` reads it straight from `fs.Stat`), so a present-but-mappable
    // mtime whose snapshot reports nothing means the two sides disagree. Only the
    // birth-time arm has a substitution (Go's `time.Now()`, `local.go:117-128`), and it
    // is exercised in the substitution test.
    let err = bridge_only(
        &info,
        "x/01.mp3",
        MappingFileStat {
            size: Some(2000),
            mod_time: Some("2023-11-14T22:13:20+00:00".into()),
            birth_time: Some("2020-09-13T12:26:40+00:00".into()),
        },
        &no_mtime,
        1,
        "F",
        PidOptions::default(),
    )
    .expect_err("a valid mapping mtime with no storage mtime is a disagreement");
    match err {
        nd_scanner::mapping_bridge::BridgeError::StorageFactMismatch {
            field,
            mapping,
            storage,
        } => {
            assert_eq!(field, "updated_at");
            assert_eq!(mapping, "2023-11-14T22:13:20+00:00");
            assert_eq!(storage, "unavailable", "the error states the missing side");
        }
        other => panic!("expected StorageFactMismatch, got {other:?}"),
    }

    // The birth arm is the opposite: no storage birth and no mapping birth, plus an
    // injected substitute, is Go's permitted substitution — not a mismatch.
    let no_birth_either = TrackStat {
        size: 2000,
        modified: None,
        birth_time: None,
    };
    let rec = bridge_only_at(
        &info,
        "x/01.mp3",
        MappingFileStat {
            size: Some(2000),
            mod_time: None,
            birth_time: None,
        },
        &no_birth_either,
        1,
        "F",
        PidOptions::default(),
        Some(now()),
    )
    .expect("an absent birth time with a substitute is Go's parity arm");
    assert_eq!(
        rec.birth_time,
        now(),
        "the substitute fills the NOT NULL column"
    );
    assert_eq!(
        rec.updated_at, None,
        "an absent mtime on both sides binds NULL, no error"
    );

    // A storage birth with no mapping birth is fine too: the storage fact wins and the
    // injected clock is ignored.
    let rec = bridge_only_at(
        &info,
        "x/01.mp3",
        MappingFileStat {
            size: Some(2000),
            mod_time: None,
            birth_time: None,
        },
        &no_mtime,
        1,
        "F",
        PidOptions::default(),
        Some(now()),
    )
    .expect("a storage birth needs no substitute");
    assert_eq!(
        rec.birth_time,
        no_mtime.birth_time.expect("the storage birth"),
        "storage birth wins"
    );
    assert_ne!(rec.birth_time, now(), "the injected clock is ignored here");

    // A malformed mapping-side timestamp string is an error, not a fallback.
    let err = bridge_only(
        &info,
        "x/01.mp3",
        MappingFileStat {
            size: Some(2000),
            mod_time: Some("2023-11-14 22:13:20".into()),
            birth_time: Some("2020-09-13T12:26:40+00:00".into()),
        },
        &st,
        1,
        "F",
        PidOptions::default(),
    )
    .expect_err("a malformed timestamp must error");
    assert!(
        matches!(err, nd_scanner::mapping_bridge::BridgeError::MalformedTimestamp { field, .. } if field == "updated_at"),
        "{err:?}"
    );
}

/// Pins 1/5: the record-only and hydration-only fields stay exactly where their owners
/// put them, and `as_i32_checked` is the narrowing the bridge uses.
#[test]
fn record_only_and_hydration_fields_stay_zero() {
    let info = synthetic_info(
        &[("TITLE", &["T"])],
        (12.345, 128, 2, 44100, 24, "MP3"),
        false,
    );
    let st = stat(1234, 1_700_000_000, 1_600_000_000);
    let (mf, rec) = bridge_synthetic(&info, "x/01.mp3", &st, 7, "FOLDER-7", PidOptions::default());

    // SC-05/DB-owned.
    assert_eq!(rec.id, "");
    assert!(!rec.missing);
    assert_eq!(rec.created_at, None);
    // Hydration-only (Go `structs:"-"`).
    assert_eq!(rec.library_path, "");
    assert_eq!(rec.library_name, "");
    assert_eq!(
        rec.annotation,
        Default::default(),
        "annotation is a structs:\"-\" embed"
    );
    assert_eq!(rec.bookmark_position, 0);
    assert!(rec.genres.is_empty());
    assert_eq!(rec.image, nd_db::artwork_hydration::ItemImage::default());
    assert_eq!(
        rec.album_image,
        nd_db::artwork_hydration::ItemImage::default()
    );
    // Not written by a scan.
    assert_eq!(rec.probe_data, "");
    // Deprecated MBIDs the mapping never fills on a MediaFile.
    assert_eq!(rec.mbz_artist_id, "");
    assert_eq!(rec.mbz_album_artist_id, "");
    assert_eq!(rec.mbz_album_comment, "");
    // Scope comes from the caller, never a constant.
    assert_eq!(rec.library_id, 7);
    assert_eq!(rec.folder_id, "FOLDER-7");
    // The mapping facts the record must carry verbatim.
    assert_eq!(rec.duration, f64::from(mf.duration));
    assert_eq!(rec.title, "T");
    assert_eq!(rec.path, "x/01.mp3");
    // The narrowing helper itself.
    assert_eq!(
        as_i32_checked("bit_rate", i64::from(i32::MAX)).expect("max fits"),
        i32::MAX
    );
    assert_eq!(
        as_i32_checked("bit_rate", i64::from(i32::MIN)).expect("min fits"),
        i32::MIN
    );
    assert!(as_i32_checked("bit_rate", i64::from(i32::MAX) + 1).is_err());
    assert!(as_i32_checked("bit_rate", i64::from(i32::MIN) - 1).is_err());
}
