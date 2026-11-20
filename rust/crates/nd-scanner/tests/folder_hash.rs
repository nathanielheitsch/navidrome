//! Folder fingerprint tests: exact Go preimage bytes, digests, and the time spelling.
//!
//! ## Oracle provenance
//!
//! Each golden records the **complete preimage bytes** plus the expected digest and the
//! expected `time.Time.String()` rendering, all captured by running the Go expression from
//! `scanner/folder_entry.go:106-151` (`fmt.Fprintf` into `crypto/md5`, `hex.EncodeToString`)
//! under `go1.27.4` (darwin/arm64) in a scratch program. No digest was guessed and none was
//! derived from Rust behaviour: the hash facts are constructed inputs, so every golden is
//! reproducible from the recorded preimage alone.
//!
//! `nd_id::new_hash1` is deliberately **not** used anywhere here — it inserts a U+200B
//! separator and base62-encodes (`nd-id/src/lib.rs:120-135`), so it could never equal a
//! Go-computed folder hash. The fingerprint is raw MD5 rendered as lowercase hex.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nd_scanner::folder::{
    go_time_string, EntryCategory, FolderCoverage, FolderEntry, FolderSnapshot,
    FolderSnapshotError, GoTimeError,
};
use nd_scanner::storage::{BirthTimeUnavailable, FileStat, LogicalPath, StatUnavailable};

fn at(secs: u64, nanos: u32) -> SystemTime {
    UNIX_EPOCH + Duration::new(secs, nanos)
}

/// `t0` from the captured goldens: `2023-11-14 22:13:20 +0000 UTC`.
fn t0() -> SystemTime {
    at(1_700_000_000, 0)
}

/// An instant `secs` seconds plus `nanos` nanoseconds **before** the Unix epoch. Go stores
/// pre-epoch times as (floor seconds, positive nanos), so Go's `time.Unix(-2, 500000000)`
/// — rendered `1969-12-31 23:59:58.5` — is `before_epoch(1, 500_000_000)`.
fn before_epoch(secs: u64, nanos: u32) -> SystemTime {
    UNIX_EPOCH - Duration::new(secs, nanos)
}

/// Go's zero `time.Time` has no `SystemTime` equivalent; the renderer takes an explicit
/// `None` for "no images", which hashes year 1 — asserted below.
fn no_images() -> Option<SystemTime> {
    None
}

fn stat(size: u64, modified: SystemTime) -> FileStat {
    FileStat {
        size,
        modified: Ok(modified),
        created: Err(BirthTimeUnavailable),
    }
}

fn entry(name: &str, category: EntryCategory, size: u64, modified: SystemTime) -> FolderEntry {
    FolderEntry {
        name: name.to_string(),
        path: LogicalPath::parse(name).expect("logical path"),
        classification_name: name.to_string(),
        stat: stat(size, modified),
        endpoint: None,
        outside_root: false,
        category,
    }
}

fn snapshot(
    mod_time: SystemTime,
    images_updated_at: Option<SystemTime>,
    num_sub_folders: usize,
    entries: Vec<FolderEntry>,
) -> FolderSnapshot {
    FolderSnapshot {
        library_id: 1,
        path: LogicalPath::parse("album").expect("logical path"),
        dir_stat: stat(4_096, mod_time),
        mod_time,
        images_updated_at,
        num_sub_folders,
        entries,
        coverage: FolderCoverage::Complete,
        warnings: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Exact digests, each with its recorded preimage
// ---------------------------------------------------------------------------

/// `(label, Go preimage bytes, Go digest)` — captured from Go with complete inputs.
const GO_HASH_GOLDEN: &[(&str, &str, &str)] = &[
    (
        "empty",
        "2023-11-14 22:13:20 +0000 UTC:0:0:0001-01-01 00:00:00 +0000 UTC",
        "14a87641a740400b5633fab1c086d422",
    ),
    (
        "one-audio",
        "2023-11-14 22:13:20 +0000 UTC:0:0:2023-11-14 22:13:20 +0000 UTCa.mp3:100:2023-11-14 22:13:20 +0000 UTC",
        "b68d18ddf50c756504990e81908068c8",
    ),
    (
        "one-audio-nanos",
        "2023-11-14 22:13:20 +0000 UTC:0:0:2023-11-14 22:13:20 +0000 UTCa.mp3:100:2023-11-14 22:13:20.123456789 +0000 UTC",
        "46efa73bec3f68f321ae2de3c30247b7",
    ),
    (
        "many-mixed",
        "2023-11-14 22:13:20 +0000 UTC:1:2:2023-11-14 22:13:20 +0000 UTCa.mp3:100:2023-11-14 22:13:20 +0000 UTCb.flac:250:2023-11-14 22:13:21 +0000 UTCcover.jpg:40:2023-11-14 22:13:22 +0000 UTCalbum.m3u:12:2023-11-14 22:13:23 +0000 UTC",
        "8b7f811a638fb1e0966f00d974646d4d",
    ),
    (
        "unicode-names",
        "2023-11-14 22:13:20 +0000 UTC:0:1:2023-11-14 22:13:20 +0000 UTCünïcode.mp3:7:2023-11-14 22:13:20 +0000 UTC日本語.jpg:9:2023-11-14 22:13:20 +0000 UTC",
        "ac030648dd4d402ce7568e68eebc13b6",
    ),
    (
        "playlist-only",
        "2023-11-14 22:13:20 +0000 UTC:1:0:2023-11-14 22:13:20 +0000 UTCt.m3u:12:2023-11-14 22:13:20 +0000 UTC",
        "991161bf2a004cac021caebd946a9ea0",
    ),
    (
        "playlist-mtime-only",
        "2023-11-14 22:13:20 +0000 UTC:1:0:2023-11-14 22:13:20 +0000 UTCt.m3u:12:2023-11-14 22:13:25 +0000 UTC",
        "84aaa3e93bd6121b98e931bfe8873de5",
    ),
    (
        "playlist-size-only",
        "2023-11-14 22:13:20 +0000 UTC:1:0:2023-11-14 22:13:20 +0000 UTCt.m3u:13:2023-11-14 22:13:20 +0000 UTC",
        "b5e4cc7ef9b4b90427643cbbf36a532f",
    ),
    (
        "audio-neg-epoch",
        "1969-12-31 23:59:58.5 +0000 UTC:0:0:0001-01-01 00:00:00 +0000 UTCold.mp3:5:1969-12-31 23:59:59 +0000 UTC",
        "f2631cf09a1b4237c6110b5ff8d73b41",
    ),
];

/// The snapshot whose facts produce each golden's preimage.
fn snapshot_for(label: &str) -> FolderSnapshot {
    match label {
        "empty" => snapshot(t0(), no_images(), 0, Vec::new()),
        "one-audio" => snapshot(
            t0(),
            Some(t0()),
            0,
            vec![entry("a.mp3", EntryCategory::Audio, 100, t0())],
        ),
        "one-audio-nanos" => snapshot(
            t0(),
            Some(t0()),
            0,
            vec![entry(
                "a.mp3",
                EntryCategory::Audio,
                100,
                at(1_700_000_000, 123_456_789),
            )],
        ),
        "many-mixed" => snapshot(
            t0(),
            Some(t0()),
            2,
            vec![
                entry("b.flac", EntryCategory::Audio, 250, at(1_700_000_001, 0)),
                entry("a.mp3", EntryCategory::Audio, 100, t0()),
                entry("cover.jpg", EntryCategory::Image, 40, at(1_700_000_002, 0)),
                entry(
                    "album.m3u",
                    EntryCategory::Playlist,
                    12,
                    at(1_700_000_003, 0),
                ),
            ],
        ),
        "unicode-names" => snapshot(
            t0(),
            Some(t0()),
            1,
            vec![
                entry("ünïcode.mp3", EntryCategory::Audio, 7, t0()),
                entry("日本語.jpg", EntryCategory::Image, 9, t0()),
            ],
        ),
        "playlist-only" => snapshot(
            t0(),
            Some(t0()),
            0,
            vec![entry("t.m3u", EntryCategory::Playlist, 12, t0())],
        ),
        "playlist-mtime-only" => snapshot(
            t0(),
            Some(t0()),
            0,
            vec![entry(
                "t.m3u",
                EntryCategory::Playlist,
                12,
                at(1_700_000_005, 0),
            )],
        ),
        "playlist-size-only" => snapshot(
            t0(),
            Some(t0()),
            0,
            vec![entry("t.m3u", EntryCategory::Playlist, 13, t0())],
        ),
        "audio-neg-epoch" => snapshot(
            before_epoch(1, 500_000_000),
            no_images(),
            0,
            vec![entry(
                "old.mp3",
                EntryCategory::Audio,
                5,
                before_epoch(1, 0),
            )],
        ),
        other => panic!("no fixture recipe for {other:?}"),
    }
}

#[test]
fn preimage_bytes_and_digest_match_the_captured_go_goldens() {
    for (label, preimage, digest) in GO_HASH_GOLDEN {
        let snapshot = snapshot_for(label);
        let actual = snapshot
            .hash_preimage()
            .unwrap_or_else(|err| panic!("{label}: {err}"));
        assert_eq!(
            String::from_utf8(actual).expect("preimage is UTF-8"),
            *preimage,
            "{label}: the preimage must equal the captured Go bytes"
        );
        assert_eq!(
            snapshot
                .hash()
                .unwrap_or_else(|err| panic!("{label}: {err}")),
            *digest,
            "{label}: MD5 over that preimage"
        );
    }
}

#[test]
fn a_hash_is_lowercase_hex_md5_and_never_an_nd_id_hash() {
    let hash = snapshot_for("one-audio").hash().expect("hash");
    assert_eq!(hash.len(), 32, "MD5 rendered as 32 hex characters");
    assert!(
        hash.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
        "lowercase hex only: {hash}"
    );
    // `nd_id::new_hash1` base62-encodes with a U+200B separator, so its output can never
    // look like this. Asserted structurally, without pulling in nd-id.
    assert!(
        !hash.chars().any(|c| ('g'..='z').contains(&c))
            && hash.chars().any(|c| ('a'..='f').contains(&c)),
        "hex alphabet only, so base62 output is ruled out: {hash}"
    );
}

#[test]
fn insertion_order_does_not_change_the_hash() {
    // Go sorts each category's map keys before hashing (`scanner/folder_entry.go:117-123`), so the
    // fingerprint must not depend on the order the OS returned entries in.
    let ordered = snapshot(
        t0(),
        Some(t0()),
        2,
        vec![
            entry("a.mp3", EntryCategory::Audio, 100, t0()),
            entry("b.flac", EntryCategory::Audio, 250, at(1_700_000_001, 0)),
            entry("cover.jpg", EntryCategory::Image, 40, at(1_700_000_002, 0)),
            entry(
                "album.m3u",
                EntryCategory::Playlist,
                12,
                at(1_700_000_003, 0),
            ),
        ],
    );
    let shuffled = snapshot(
        t0(),
        Some(t0()),
        2,
        vec![
            entry(
                "album.m3u",
                EntryCategory::Playlist,
                12,
                at(1_700_000_003, 0),
            ),
            entry("cover.jpg", EntryCategory::Image, 40, at(1_700_000_002, 0)),
            entry("b.flac", EntryCategory::Audio, 250, at(1_700_000_001, 0)),
            entry("a.mp3", EntryCategory::Audio, 100, t0()),
        ],
    );
    assert_eq!(
        ordered.hash().expect("hash"),
        shuffled.hash().expect("hash"),
        "sorted logical identities, arbitrary input order"
    );
    assert_eq!(
        ordered.hash().expect("hash"),
        snapshot_for("many-mixed").hash().expect("hash"),
        "and that order reproduces the golden's digest"
    );
}

#[test]
fn playlist_mtime_change_alone_changes_the_hash_with_folder_mtime_preserved() {
    // Non-vacuity (`scanner/folder_entry_test.go:392,409`): an edit whose `rsync -a` preserved the
    // folder mtime must still be detected.
    let before = snapshot_for("playlist-only");
    let after = snapshot_for("playlist-mtime-only");
    assert_eq!(before.mod_time, after.mod_time, "folder mtime preserved");
    assert_eq!(
        before.category(EntryCategory::Playlist).len(),
        after.category(EntryCategory::Playlist).len(),
        "playlist count unchanged"
    );
    assert_eq!(
        before.entries[0].stat.size, after.entries[0].stat.size,
        "playlist size unchanged"
    );
    assert_ne!(
        before.hash().expect("hash"),
        after.hash().expect("hash"),
        "a playlist mtime change alone must be detected"
    );
}

#[test]
fn playlist_size_change_alone_changes_the_hash() {
    let before = snapshot_for("playlist-only");
    let after = snapshot_for("playlist-size-only");
    assert_eq!(before.mod_time, after.mod_time);
    assert_ne!(
        before.hash().expect("hash"),
        after.hash().expect("hash"),
        "a playlist size change alone must be detected"
    );
}

#[test]
fn each_category_detects_size_and_mtime_changes() {
    for category in [
        EntryCategory::Audio,
        EntryCategory::Image,
        EntryCategory::Playlist,
    ] {
        let name = match category {
            EntryCategory::Audio => "t.flac",
            EntryCategory::Image => "cover.jpg",
            EntryCategory::Playlist => "t.m3u",
            EntryCategory::Other => unreachable!(),
        };
        let base = snapshot(t0(), Some(t0()), 0, vec![entry(name, category, 10, t0())]);
        let size_changed = snapshot(t0(), Some(t0()), 0, vec![entry(name, category, 11, t0())]);
        let mtime_changed = snapshot(
            t0(),
            Some(t0()),
            0,
            vec![entry(name, category, 10, at(1_700_000_010, 0))],
        );
        assert_ne!(
            base.hash().expect("hash"),
            size_changed.hash().expect("hash"),
            "{category:?} size must matter"
        );
        assert_ne!(
            base.hash().expect("hash"),
            mtime_changed.hash().expect("hash"),
            "{category:?} mtime must matter"
        );
    }
}

#[test]
fn sub_folder_and_playlist_counts_are_part_of_the_preimage() {
    let base = snapshot(
        t0(),
        Some(t0()),
        0,
        vec![entry("t.m3u", EntryCategory::Playlist, 12, t0())],
    );
    let with_child = snapshot(
        t0(),
        Some(t0()),
        1,
        vec![entry("t.m3u", EntryCategory::Playlist, 12, t0())],
    );
    assert_ne!(
        base.hash().expect("hash"),
        with_child.hash().expect("hash"),
        "numSubFolders is hashed"
    );

    // A new subfolder contributes no file entries at all, so only the count field moves.
    let empty = snapshot(t0(), no_images(), 0, Vec::new());
    let empty_with_child = snapshot(t0(), no_images(), 1, Vec::new());
    assert_ne!(
        empty.hash().expect("hash"),
        empty_with_child.hash().expect("hash")
    );
}

#[test]
fn the_zero_image_state_is_not_replaced_by_the_epoch() {
    // Go hashes a zero `time.Time` as year 1; substituting the epoch would silently change
    // every hash of a folder with no images.
    let no_image_folder = snapshot(t0(), no_images(), 0, Vec::new());
    let epoch_images = snapshot(t0(), Some(UNIX_EPOCH), 0, Vec::new());
    assert_ne!(
        no_image_folder.hash().expect("hash"),
        epoch_images.hash().expect("hash"),
        "`None` is Go's zero time, not 1970-01-01"
    );
    assert!(
        String::from_utf8(no_image_folder.hash_preimage().expect("preimage"))
            .expect("utf8")
            .ends_with(":0001-01-01 00:00:00 +0000 UTC"),
        "the absent-image field renders as year 1"
    );
    // And the golden 'empty' digest is exactly that state.
    assert_eq!(
        no_image_folder.hash().expect("hash"),
        "14a87641a740400b5633fab1c086d422"
    );
}

#[test]
fn an_uncategorized_entry_contributes_no_hash_line() {
    // Go writes only audio/image/playlist lines; a text file changes nothing.
    let base = snapshot(
        t0(),
        Some(t0()),
        0,
        vec![entry("a.mp3", EntryCategory::Audio, 1, t0())],
    );
    let with_other = snapshot(
        t0(),
        Some(t0()),
        0,
        vec![
            entry("a.mp3", EntryCategory::Audio, 1, t0()),
            entry("notes.txt", EntryCategory::Other, 99, at(1_700_000_999, 0)),
        ],
    );
    assert_eq!(
        base.hash().expect("hash"),
        with_other.hash().expect("hash"),
        "the three categories only"
    );
}

#[test]
fn unicode_names_hash_without_replacement_character_collisions() {
    let real = snapshot(
        t0(),
        Some(t0()),
        0,
        vec![entry("ünïcode.mp3", EntryCategory::Audio, 7, t0())],
    );
    let lossy = snapshot(
        t0(),
        Some(t0()),
        0,
        vec![entry(
            "\u{fffd}n\u{fffd}code.mp3",
            EntryCategory::Audio,
            7,
            t0(),
        )],
    );
    assert_ne!(
        real.hash().expect("hash"),
        lossy.hash().expect("hash"),
        "a lossy render would collide two distinct names"
    );
}

// ---------------------------------------------------------------------------
// Go's `time.Time.String()` UTC spelling
// ---------------------------------------------------------------------------

/// `(description, seconds, nanoseconds, expected Go rendering)`, all captured from Go.
const GO_TIME_GOLDEN: &[(&str, i64, u32, &str)] = &[
    (
        "whole second",
        1_700_000_000,
        0,
        "2023-11-14 22:13:20 +0000 UTC",
    ),
    ("epoch", 0, 0, "1970-01-01 00:00:00 +0000 UTC"),
    (
        ".1",
        1_700_000_000,
        100_000_000,
        "2023-11-14 22:13:20.1 +0000 UTC",
    ),
    (
        ".12",
        1_700_000_000,
        120_000_000,
        "2023-11-14 22:13:20.12 +0000 UTC",
    ),
    (
        ".123",
        1_700_000_000,
        123_000_000,
        "2023-11-14 22:13:20.123 +0000 UTC",
    ),
    (
        ".123456789",
        1_700_000_000,
        123_456_789,
        "2023-11-14 22:13:20.123456789 +0000 UTC",
    ),
    (
        "1ns keeps all nine digits",
        1_700_000_000,
        1,
        "2023-11-14 22:13:20.000000001 +0000 UTC",
    ),
    (
        "trailing zeros trimmed",
        1_700_000_000,
        100,
        "2023-11-14 22:13:20.0000001 +0000 UTC",
    ),
    (
        "one second before epoch",
        -1,
        0,
        "1969-12-31 23:59:59 +0000 UTC",
    ),
    (
        "1.5s before epoch: floor seconds, positive nanos",
        -2,
        500_000_000,
        "1969-12-31 23:59:58.5 +0000 UTC",
    ),
];

#[test]
fn go_time_spelling_matches_the_captured_renderings() {
    for (description, secs, nanos, expected) in GO_TIME_GOLDEN {
        let time = if *secs >= 0 {
            UNIX_EPOCH + Duration::new(*secs as u64, *nanos)
        } else {
            // Go's stored pair is (floor seconds, positive nanos), so (-2, 500000000) is
            // two seconds before the epoch plus half a second.
            UNIX_EPOCH - Duration::new((-(*secs)) as u64, 0)
                + Duration::from_nanos(u64::from(*nanos))
        };
        assert_eq!(
            go_time_string(time).unwrap_or_else(|err| panic!("{description}: {err:?}")),
            *expected,
            "{description}"
        );
    }
}

#[test]
fn go_time_spelling_is_not_rfc3339() {
    let text = go_time_string(t0()).expect("rendered");
    // RFC3339 would put `T` at the date/time boundary and `Z` at the end. Go's spelling has
    // a space there and an explicit offset plus zone name (`UTC` itself contains a T, so the
    // position is what matters, not the character's presence).
    assert_eq!(
        text.as_bytes().get(10),
        Some(&b' '),
        "RFC3339 would use a T separator: {text}"
    );
    assert!(!text.ends_with('Z'), "RFC3339 would use a Z suffix: {text}");
    assert!(
        text.ends_with(" +0000 UTC"),
        "Go's offset and zone field: {text}"
    );
}

#[test]
fn the_epoch_and_a_year_one_instant_are_distinguishable() {
    assert_eq!(
        go_time_string(UNIX_EPOCH).expect("epoch"),
        "1970-01-01 00:00:00 +0000 UTC"
    );
    // Go's zero `time.Time` is `0001-01-01 00:00:00 +0000 UTC`, and that instant IS
    // representable as a pre-epoch SystemTime: 62_135_596_800 s before the Unix epoch.
    assert_eq!(
        go_time_string(before_epoch(62_135_596_800, 0)).expect("year 1"),
        "0001-01-01 00:00:00 +0000 UTC",
        "a year-1 instant must render Go's zero-time spelling, not clamp to the epoch"
    );
    // Year 0 renders zero-padded, like Go's `%04d`, and is still distinct from the epoch.
    assert_eq!(
        go_time_string(before_epoch(62_135_596_801, 0)).expect("year 0"),
        "0000-12-31 23:59:59 +0000 UTC",
        "below year 1 the padding rule still applies and nothing clamps"
    );
}

#[test]
fn an_unrepresentable_timestamp_is_a_typed_error_not_a_clamp() {
    let extreme = UNIX_EPOCH + Duration::from_secs(8_000_000_000_000_000);
    assert_eq!(go_time_string(extreme), Err(GoTimeError::OutOfRange));
}

#[test]
fn a_missing_mtime_is_a_typed_hash_error_not_a_zero_time() {
    let snapshot = FolderSnapshot {
        library_id: 1,
        path: LogicalPath::parse("album").expect("path"),
        dir_stat: stat(1, t0()),
        mod_time: t0(),
        images_updated_at: Some(t0()),
        num_sub_folders: 0,
        entries: vec![FolderEntry {
            name: "a.mp3".to_string(),
            path: LogicalPath::parse("a.mp3").expect("path"),
            classification_name: "a.mp3".to_string(),
            stat: FileStat {
                size: 10,
                modified: Err(StatUnavailable),
                created: Err(BirthTimeUnavailable),
            },
            endpoint: None,
            outside_root: false,
            category: EntryCategory::Audio,
        }],
        coverage: FolderCoverage::EntriesIncomplete,
        warnings: Vec::new(),
    };
    assert_eq!(
        snapshot.hash(),
        Err(FolderSnapshotError::TimestampUnavailable {
            path: LogicalPath::parse("a.mp3").expect("path"),
        }),
        "Unavailable captured mtime is refused, not a name-only or fabricated-time hash"
    );
}

#[test]
fn a_size_beyond_go_int64_is_reported() {
    let snapshot = FolderSnapshot {
        library_id: 1,
        path: LogicalPath::parse("album").expect("path"),
        dir_stat: stat(1, t0()),
        mod_time: t0(),
        images_updated_at: Some(t0()),
        num_sub_folders: 0,
        entries: vec![entry("huge.mp3", EntryCategory::Audio, u64::MAX, t0())],
        coverage: FolderCoverage::Complete,
        warnings: Vec::new(),
    };
    assert!(
        matches!(
            snapshot.hash(),
            Err(FolderSnapshotError::SizeOutOfRange { size, .. }) if size == u64::MAX
        ),
        "Go would write an int64 overflow; the gap must be reported"
    );
}

#[test]
fn snapshot_helpers_report_the_go_folder_predicates() {
    let empty = snapshot(t0(), no_images(), 0, Vec::new());
    assert!(empty.has_no_files() && empty.is_empty());
    let with_child = snapshot(t0(), no_images(), 1, Vec::new());
    assert!(
        with_child.has_no_files() && !with_child.is_empty(),
        "subfolders count toward is_empty but not has_no_files"
    );
    let with_image = snapshot(
        t0(),
        Some(t0()),
        0,
        vec![entry("cover.jpg", EntryCategory::Image, 3, t0())],
    );
    assert!(!with_image.has_no_files());
    assert_eq!(with_image.image_names(), vec!["cover.jpg"]);
    assert_eq!(with_image.num_audio_files(), 0);
}
