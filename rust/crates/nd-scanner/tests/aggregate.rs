//! SC-04 — executed proof for the multi-track album aggregation.
//!
//! Every expectation here is a **measured** Go result, not this lane's inference: the
//! vectors come from `model/mediafile_test.go` and from a scratch harness that ran the
//! real `MediaFiles.ToAlbum`, `TagList.GroupByFrequency`, `firstArtPath`, `minMax`,
//! `allOrNothing`, `mostFrequentPtr` and `slice.MostFrequent` against this repository's
//! Go source. `docs/rust-port/subplans/SC-04-go-truth-brief.md` §2 records the same
//! measurements; this file re-asserts them on the Rust port.
//!
//! No DB writes, no I/O, no clock: the aggregator is a pure function over the mapping DTO
//! plus the caller's per-track facts.

use chrono::{DateTime, TimeZone, Utc};
use nd_id::{ROLE_ALBUM_ARTIST, ROLE_ARTIST, VARIOUS_ARTISTS, VARIOUS_ARTISTS_ID};
use nd_mapping::config::{MappingContext, MappingOptions};
use nd_mapping::mdfile::MediaFile;
use nd_mapping::participants::{Credit, RoleCredits};
use nd_scanner::aggregate::{album_draft, AggregateError, TrackFacts};
use nd_scanner::projection::{album_drafts, ProjectionError};

/// The caller's fixed import stamp. `put_sql` overwrites it and Go's hash ignores it; it
/// is only here because `AlbumRecord::imported_at` is NOT NULL.
const IMPORTED: DateTime<Utc> = DateTime::from_timestamp(1_800_000_000, 0).expect("valid stamp");

fn at(unix: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(unix, 0)
        .single()
        .expect("in chrono's range")
}

/// A track shaped directly rather than through the mapper, so a test can pin one field
/// (a disc number, a date, a birth) without the mapping's own defaults interfering. The
/// mapper-built paths are exercised separately by `tests/projection_singleton.rs`.
fn track(path: &str) -> MediaFile {
    MediaFile {
        path: path.to_string(),
        album_id: "ALBUM-1".to_string(),
        library_id: 1,
        folder_id: "FOLDER-1".to_string(),
        album: "Album".to_string(),
        ..MediaFile::default()
    }
}

/// Facts for a track: size, mtime and the resolved birth it was given.
fn facts(size: i64, modified: Option<DateTime<Utc>>, birth: Option<DateTime<Utc>>) -> TrackFacts {
    TrackFacts {
        stat: nd_scanner::mapping_bridge::TrackStat {
            size,
            modified,
            birth_time: birth,
        },
        birth,
    }
}

/// Facts keyed by path, which is exactly what `album_draft`'s lookup needs: one bundle
/// per group member, never one per album.
fn facts_by_path(
    entries: &[(&str, TrackFacts)],
) -> impl Fn(&MediaFile) -> Result<TrackFacts, AggregateError> {
    let table: Vec<(String, TrackFacts)> = entries
        .iter()
        .map(|(path, facts)| ((*path).to_string(), facts.clone()))
        .collect();
    move |track: &MediaFile| {
        table
            .iter()
            .find(|(path, _)| path == &track.path)
            .map(|(_, facts)| facts.clone())
            .ok_or_else(|| AggregateError::MissingTrackFacts {
                path: track.path.clone(),
            })
    }
}

fn mappings() -> &'static nd_mapping::mappings::Mappings {
    static CTX: std::sync::OnceLock<MappingContext> = std::sync::OnceLock::new();
    CTX.get_or_init(|| MappingContext::new(MappingOptions::default()))
        .mappings()
}

fn aggregate(
    group: &[&MediaFile],
    entries: &[(&str, TrackFacts)],
) -> nd_db::album_repo::AlbumRecord {
    album_draft(group, facts_by_path(entries), mappings(), IMPORTED).expect("aggregates")
}

/// `aggregate_zero_one_many_path_sorted` — Go's empty group returns the zero `Album`
/// (`model/mediafile.go:321-323`, executed: `MediaFiles{}.ToAlbum().Equals(Album{})` with
/// nil `Discs`/`FolderIDs`/`Tags`/`Participants`), one track passes through, and the
/// three-track Go vector (`model/mediafile_test.go:94-110`) yields its measured values.
#[test]
fn aggregate_zero_one_many_path_sorted() {
    // Zero members: Go's zero Album, with the nil spellings preserved (None, not
    // Some(empty) — the parent-verified round-trip keeps them distinct).
    let empty = aggregate(&[], &[]);
    assert_eq!(empty.song_count, 0);
    assert_eq!(empty.duration, 0.0);
    assert_eq!(empty.size, 0);
    assert_eq!(empty.id, "");
    assert!(
        empty.discs.is_none(),
        "Go's empty group has a NIL Discs map"
    );
    assert!(
        empty.folder_ids.is_none(),
        "Go's empty group has a NIL FolderIDs slice"
    );
    assert!(empty.tags.is_empty());
    assert!(empty.participants.is_empty());
    assert!(
        !empty.missing,
        "a.Missing starts true but the loop never runs"
    );

    // One member: the singleton passthrough.
    let one = {
        let mut m = track("only.mp3");
        m.album = "Solo".to_string();
        m.year = 2024;
        m.date = "2024-03-02".to_string();
        m.duration = 0.44;
        m
    };
    let single = aggregate(
        &[&one],
        &[(
            "only.mp3",
            facts(7833, Some(at(1_700_000_000)), Some(at(1_600_000_000))),
        )],
    );
    assert_eq!(single.song_count, 1);
    assert_eq!(
        single.duration as f32, 0.44,
        "Go's Duration is float32, so the widened f64 carries the float32 value"
    );
    assert_eq!(single.size, 7833);
    assert_eq!(single.name, "Solo");
    assert_eq!(
        single.date, "2024-03-02",
        "allOrNothing of one value is that value"
    );
    assert_eq!((single.min_year, single.max_year), (2024, 2024));
    assert_eq!(single.created_at, Some(at(1_600_000_000)));
    assert_eq!(single.updated_at, Some(at(1_700_000_000)));

    // The measured Go vector, deliberately fed in scrambled path order so the sort is
    // load-bearing: 451 (float32), 4072, 1985/1986, empty date consensus, newest mtime,
    // oldest birth.
    let mut a = track("c.mp3");
    a.duration = 100.2;
    a.year = 1985;
    a.date = "1985-01-02".to_string();
    let mut b = track("a.mp3");
    b.duration = 200.2;
    b.year = 0;
    b.date = String::new();
    let mut c = track("b.mp3");
    c.duration = 150.6;
    c.year = 1986;
    c.date = "1986-01-02".to_string();

    let album = aggregate(
        &[&c, &a, &b],
        &[
            (
                "a.mp3",
                facts(2048, Some(at(1_750_000_500)), Some(at(1_750_000_500))),
            ),
            (
                "b.mp3",
                facts(1000, Some(at(1_750_000_900)), Some(at(1_750_000_100))),
            ),
            (
                "c.mp3",
                facts(1024, Some(at(1_750_000_300)), Some(at(1_750_000_500))),
            ),
        ],
    );
    assert_eq!(album.song_count, 3, "SongCount: len(mfs)");
    assert_eq!(
        album.duration, 451.0,
        "executed Go float32 sum of 100.2+200.2+150.6"
    );
    assert_eq!(album.size, 4072, "Size sum, not the first track's size");
    assert_eq!(album.min_year, 1985);
    assert_eq!(album.max_year, 1986);
    assert_eq!(
        album.date, "",
        "three distinct dates → allOrNothing is empty"
    );
    assert_eq!(
        album.updated_at,
        Some(at(1_750_000_900)),
        "newest mtime across the group (Go's newer, :389)"
    );
    assert_eq!(
        album.created_at,
        Some(at(1_750_000_100)),
        "oldest birth across the group, not the first track's (Go's older, :390)"
    );
    assert_eq!(
        album.min_original_year, 0,
        "minMax of an all-zero group is 0/0"
    );
    assert_eq!(album.max_original_year, 0);
}

/// Non-vacuity (`subplans/scanner.md` SC-04 §7): feeding the same tracks in a different
/// order yields a byte-identical record, and the caller's slice is NOT reordered — the
/// deviation Go has (its `slices.SortFunc` mutates the receiver, executed
/// `before=[z a m] after=[a m z]`) is precisely what this port must not do.
#[test]
fn permuting_input_does_not_change_the_aggregate() {
    let mut a = track("z.mp3");
    a.duration = 100.2;
    a.year = 1985;
    a.disc_number = 2;
    a.disc_subtitle = "Disc two".to_string();
    a.tags = nd_mapping::mdfile::Tags::from([(
        "genre".to_string(),
        vec!["Rock".to_string(), "Pop".to_string()],
    )]);
    let mut b = track("a.mp3");
    b.duration = 200.2;
    b.year = 1986;
    b.disc_number = 1;
    b.disc_subtitle = "Disc one".to_string();
    b.tags = nd_mapping::mdfile::Tags::from([("genre".to_string(), vec!["Rock".to_string()])]);
    let mut c = track("m.mp3");
    c.duration = 150.6;
    c.year = 1985;
    c.has_cover_art = true;
    c.explicit_status = "c".to_string();
    let mut d = track("q.mp3");
    d.duration = 50.0;
    d.year = 0;
    d.explicit_status = "e".to_string();

    let entries: &[(&str, TrackFacts)] = &[
        ("a.mp3", facts(10, Some(at(200)), Some(at(400)))),
        ("m.mp3", facts(20, Some(at(300)), Some(at(300)))),
        ("q.mp3", facts(30, Some(at(100)), Some(at(500)))),
        ("z.mp3", facts(40, Some(at(400)), Some(at(600)))),
    ];

    let forward = vec![&a, &b, &c, &d];
    let first = aggregate(&forward, entries);

    let orders: Vec<Vec<&MediaFile>> = vec![
        vec![&d, &c, &b, &a],
        vec![&b, &d, &a, &c],
        vec![&c, &a, &d, &b],
        vec![&a, &d, &c, &b],
    ];
    for order in &orders {
        assert_eq!(
            aggregate(order, entries),
            first,
            "the aggregate must not depend on the caller's order: {:?}",
            order.iter().map(|t| t.path.as_str()).collect::<Vec<_>>()
        );
    }

    // The caller's slice survives every call, in the order it was handed in.
    assert_eq!(
        forward.iter().map(|t| t.path.as_str()).collect::<Vec<_>>(),
        ["z.mp3", "a.mp3", "m.mp3", "q.mp3"],
        "the port sorts a COPY; Go mutates its receiver"
    );

    // Non-vacuity: the record is not order-insensitive by accident — the fields that
    // DO depend on path order moved, so a comparator that ignored everything would fail.
    assert_eq!(first.name, "Album");
    assert_eq!(first.song_count, 4);
    // Path order is a(200.2), m(150.6), q(50.0), z(100.2). Go accumulates float32, so
    // the sum is exactly 501 — an f64 accumulation of the same values gives
    // 500.99999999999994, which is why the arm widens a float32 running total.
    assert_eq!(first.duration, 501.0, "float32 accumulation in path order");
    assert_eq!(first.explicit_status, "e", "e beats c regardless of order");
}

/// Non-vacuity of the aggregation itself (`subplans/scanner.md` SC-04 §7): projecting
/// only the first member — what the singleton arm does — fails the count, the duration
/// and the sums.
#[test]
fn first_track_only_aggregate_fails_count_and_sums() {
    let mut a = track("a.mp3");
    a.duration = 100.2;
    a.size = Some(1024);
    let mut b = track("b.mp3");
    b.duration = 200.2;
    b.size = Some(2048);

    let entries = &[
        ("a.mp3", facts(1024, Some(at(1)), Some(at(1)))),
        ("b.mp3", facts(2048, Some(at(2)), Some(at(2)))),
    ];
    let whole = aggregate(&[&a, &b], entries);
    let first_only = aggregate(&[&a], &entries[..1]);

    assert_eq!(whole.song_count, 2);
    assert_ne!(
        first_only.song_count, whole.song_count,
        "count must be 2, not 1"
    );
    assert_ne!(
        first_only.duration, whole.duration,
        "duration must be the sum"
    );
    assert_ne!(first_only.size, whole.size, "size must be the sum");
    assert!(
        first_only.duration < whole.duration,
        "the first-track-only duration is strictly smaller, so the assertion above is not a coincidence"
    );
}

/// `spanning_folders_discs_missing_compilation`: `FolderIDs` is first-seen unique over the
/// POST-SORT order (`:396`, executed `[F1 F2]` from a `[z a m]` group), a repeated disc
/// keeps the LAST path-sorted subtitle (`:374-376`), `DiscNumber <= 0` adds nothing, and
/// `Compilation` is the OR (`:359`).
#[test]
fn spanning_folders_discs_missing_compilation() {
    let mut z = track("z.mp3");
    z.folder_id = "F2".to_string();
    z.disc_number = 1;
    z.disc_subtitle = "First".to_string();
    z.compilation = false;
    let mut a = track("a.mp3");
    a.folder_id = "F1".to_string();
    a.disc_number = 1;
    a.disc_subtitle = "Second".to_string();
    a.compilation = true;
    let mut m = track("m.mp3");
    m.folder_id = "F1".to_string();
    m.disc_number = 0;
    m.disc_subtitle = "ignored".to_string();
    m.compilation = false;
    let mut q = track("q.mp3");
    q.folder_id = "F3".to_string();
    q.disc_number = -1;
    q.disc_subtitle = "also ignored".to_string();

    let album = aggregate(
        &[&z, &a, &m, &q],
        &[
            ("a.mp3", facts(1, None, None)),
            ("m.mp3", facts(1, None, None)),
            ("q.mp3", facts(1, None, None)),
            ("z.mp3", facts(1, None, None)),
        ],
    );

    assert_eq!(
        album.folder_ids,
        Some(vec!["F1".to_string(), "F3".to_string(), "F2".to_string()]),
        "first-seen unique over the POST-SORT order: a(F1), m(F1), q(F3), z(F2)"
    );
    let discs = album
        .discs
        .clone()
        .expect("a non-empty group always seeds disc 1");
    assert_eq!(
        discs.keys().copied().collect::<Vec<_>>(),
        [1],
        "DiscNumber <= 0 adds nothing (executed: a 0 and a -1 group leaves only disc 1)"
    );
    assert_eq!(
        discs[&1], "First",
        "last member in PATH order (z.mp3) owns disc 1's subtitle; a.mp3's \"Second\" is overwritten"
    );
    assert!(album.compilation, "Compilation is the OR across the group");
    assert!(
        !album.missing,
        "freshly imported tracks are not missing, and Go ANDs the group"
    );
    assert_eq!(
        album.created_at, None,
        "all-zero births leave CreatedAt zero (Go's older arms, :469-480)"
    );
    assert_eq!(album.updated_at, None, "absent mtimes stay absent");
}

/// `art_lowest_disc_then_path`: the art-bearing member with the lowest positive disc
/// wins, and a tie on disc breaks by lower `Path` (`firstArtPath`, `:502-520`).
#[test]
fn art_lowest_disc_then_path() {
    let art = |path: &str, disc: i32| {
        let mut m = track(path);
        m.has_cover_art = true;
        m.disc_number = disc;
        m
    };
    // Executed: the lower disc beats a lower path.
    let high = art("a.mp3", 3);
    let low = art("z.mp3", 1);
    let album = aggregate(
        &[&high, &low],
        &[
            ("a.mp3", facts(1, None, None)),
            ("z.mp3", facts(1, None, None)),
        ],
    );
    assert_eq!(
        album.embed_art_path, "z.mp3",
        "lowest positive disc wins over path"
    );

    // Same disc → lowest path.
    let x = art("b.mp3", 1);
    let y = art("a.mp3", 1);
    let album = aggregate(
        &[&x, &y],
        &[
            ("b.mp3", facts(1, None, None)),
            ("a.mp3", facts(1, None, None)),
        ],
    );
    assert_eq!(album.embed_art_path, "a.mp3", "equal disc → lower path");

    // A member without art never sets the path, even when it sorts first.
    let no_art = track("0.mp3");
    let with_art = art("z.mp3", 1);
    let album = aggregate(
        &[&no_art, &with_art],
        &[
            ("0.mp3", facts(1, None, None)),
            ("z.mp3", facts(1, None, None)),
        ],
    );
    assert_eq!(album.embed_art_path, "z.mp3");

    // No member has art → empty, never a folder path.
    let album = aggregate(
        &[&track("a.mp3"), &track("b.mp3")],
        &[
            ("a.mp3", facts(1, None, None)),
            ("b.mp3", facts(1, None, None)),
        ],
    );
    assert_eq!(album.embed_art_path, "");
}

/// The executed `firstArtPath` quirk: a `DiscNumber == 0` art member resets the state to
/// disc `0`, so every LATER art member wins unconditionally. Executed vectors:
/// `{a:disc0},{b:disc0},{c:disc0}` → `"c"`; `{a:disc1 no art},{b:disc0 art},{c:disc2 art}`
/// → `"c"`.
#[test]
fn art_disc_zero_resets_the_state() {
    let art = |path: &str, disc: i32| {
        let mut m = track(path);
        m.has_cover_art = true;
        m.disc_number = disc;
        m
    };
    let a = art("a.mp3", 0);
    let b = art("b.mp3", 0);
    let c = art("c.mp3", 0);
    let album = aggregate(
        &[&a, &b, &c],
        &[
            ("a.mp3", facts(1, None, None)),
            ("b.mp3", facts(1, None, None)),
            ("c.mp3", facts(1, None, None)),
        ],
    );
    assert_eq!(
        album.embed_art_path, "c.mp3",
        "the reset lets the last member win"
    );

    let mut no_art = track("a.mp3");
    no_art.disc_number = 1;
    let b = art("b.mp3", 0);
    let c = art("c.mp3", 2);
    let album = aggregate(
        &[&no_art, &b, &c],
        &[
            ("a.mp3", facts(1, None, None)),
            ("b.mp3", facts(1, None, None)),
            ("c.mp3", facts(1, None, None)),
        ],
    );
    assert_eq!(
        album.embed_art_path, "c.mp3",
        "executed vector: disc 2 wins after a reset"
    );
}

/// `date_comment_all_or_nothing` (`:412-421`): disagreement → `""` (including an empty
/// value among non-empty ones), agreement → the value **verbatim**, malformed included.
#[test]
fn date_comment_all_or_nothing() {
    let mk = |path: &str, date: &str, comment: &str, release: &str, original: &str| {
        let mut m = track(path);
        m.date = date.to_string();
        m.comment = comment.to_string();
        m.release_date = release.to_string();
        m.original_date = original.to_string();
        m
    };

    // Two of three agree on the date → still empty, because one differs.
    let a = mk("a.mp3", "2024", "same", "2024-05", "2000");
    let b = mk("b.mp3", "2024", "same", "2024-05", "2000");
    let c = mk("c.mp3", "", "same", "2024-05", "2000");
    let album = aggregate(
        &[&a, &b, &c],
        &[
            ("a.mp3", facts(1, None, None)),
            ("b.mp3", facts(1, None, None)),
            ("c.mp3", facts(1, None, None)),
        ],
    );
    assert_eq!(album.date, "", "a differing member empties the consensus");
    assert_eq!(album.comment, "same", "everyone agrees → verbatim");
    assert_eq!(album.release_date, "2024-05");
    assert_eq!(album.original_date, "2000");

    // Malformed-but-equal passes through verbatim (executed).
    let x = mk("a.mp3", "not-a-date", "weird", "", "");
    let y = mk("b.mp3", "not-a-date", "weird", "", "");
    let album = aggregate(
        &[&x, &y],
        &[
            ("a.mp3", facts(1, None, None)),
            ("b.mp3", facts(1, None, None)),
        ],
    );
    assert_eq!(
        album.date, "not-a-date",
        "Go does not parse the string here"
    );
    assert_eq!(album.comment, "weird");

    // Empty-vs-non-empty is a disagreement, not a consensus of "" (executed).
    let p = mk("a.mp3", "", "one", "", "");
    let q = mk("b.mp3", "1985", "two", "", "");
    let album = aggregate(
        &[&p, &q],
        &[
            ("a.mp3", facts(1, None, None)),
            ("b.mp3", facts(1, None, None)),
        ],
    );
    assert_eq!(album.date, "");
    assert_eq!(album.comment, "");
}

/// `min_max_year_zero_and_negative_edges`, executed against `minMax` (`:423-433`), which
/// seeds BOTH bounds from `items[0]`.
#[test]
fn min_max_year_zero_and_negative_edges() {
    let years_into_album = |years: &[i32]| -> (i32, i32) {
        let group: Vec<MediaFile> = years
            .iter()
            .enumerate()
            .map(|(i, &year)| {
                let mut m = track(&format!("{:02}.mp3", i));
                m.year = year;
                m
            })
            .collect();
        let refs: Vec<&MediaFile> = group.iter().collect();
        let entries: Vec<(&str, TrackFacts)> = group
            .iter()
            .map(|m| (m.path.as_str(), facts(1, None, None)))
            .collect();
        let album = aggregate(&refs, &entries);
        (album.min_year, album.max_year)
    };
    assert_eq!(years_into_album(&[0, 0, 1999]), (1999, 1999));
    assert_eq!(years_into_album(&[2001, 0, 1999]), (1999, 2001));
    // Executed `minMax` (`model/mediafile.go:423-433`): a single-member negative group
    // yields `-5/-5`, but a negative group WITH a positive member keeps the positive max.
    assert_eq!(years_into_album(&[-5, 1999]), (-5, 1999));
    assert_eq!(years_into_album(&[-5]), (-5, -5));
    assert_eq!(years_into_album(&[-3, -2]), (-3, -2));
    assert_eq!(years_into_album(&[0, 0]), (0, 0));
    // The bug this lane fixes: the SC-03 arm seeded mx = 0 and produced (-5, 0).
    assert_ne!(years_into_album(&[-5, 1999]), (-5, 0));
}

/// `mbz_and_rg_most_frequent`: `slice.MostFrequent` skips the zero value and a tie goes to
/// the first value reaching the running max (`utils/slice/slice.go:83-105`);
/// `mostFrequentPtr` counts by dereferenced value, so a genuine `0.0` is a candidate and
/// an all-nil group is `None` (`:439-461`).
#[test]
fn mbz_and_rg_most_frequent() {
    let mk = |path: &str, mbz: &str, group: &str, gain: Option<f64>, peak: Option<f64>| {
        let mut m = track(path);
        m.mbz_album_id = mbz.to_string();
        m.mbz_release_group_id = group.to_string();
        m.rg_album_gain = gain;
        m.rg_album_peak = peak;
        m
    };
    let f = |_path: &str| facts(1, None, None);

    // Tie → first reached max: {id2, id1} → "id2".
    let a = mk("a.mp3", "id2", "g2", Some(-8.0), Some(0.0));
    let b = mk("b.mp3", "id1", "g1", Some(-8.0), Some(1.0));
    let album = aggregate(&[&a, &b], &[("a.mp3", f("a.mp3")), ("b.mp3", f("b.mp3"))]);
    assert_eq!(
        album.mbz_album_id, "id2",
        "tie goes to the first value reaching the max"
    );
    assert_eq!(album.mbz_release_group_id, "g2");
    assert_eq!(album.rg_album_gain, Some(-8.0), "-8 twice beats -5 once");
    assert_eq!(
        album.rg_album_peak,
        Some(0.0),
        "a genuine 0.0 counted twice beats a single 1.0"
    );

    // Zero value skipped; a repeated non-empty value wins.
    let x = mk("a.mp3", "z", "", None, None);
    let y = mk("b.mp3", "", "", None, None);
    let z = mk("c.mp3", "z", "", Some(0.0), None);
    let album = aggregate(
        &[&x, &y, &z],
        &[
            ("a.mp3", f("a.mp3")),
            ("b.mp3", f("b.mp3")),
            ("c.mp3", f("c.mp3")),
        ],
    );
    assert_eq!(album.mbz_album_id, "z", "MostFrequent skips the empty id");
    assert_eq!(album.mbz_release_group_id, "", "only empty values → empty");
    assert_eq!(
        album.rg_album_gain,
        Some(0.0),
        "a single genuine 0.0 survives"
    );
    assert_eq!(album.rg_album_peak, None, "all-nil → nil");

    // Mixed nil and a value: nils are skipped entirely.
    let p = mk("a.mp3", "", "", None, Some(0.5));
    let q = mk("b.mp3", "", "", Some(-3.0), None);
    let album = aggregate(&[&p, &q], &[("a.mp3", f("a.mp3")), ("b.mp3", f("b.mp3"))]);
    assert_eq!(album.rg_album_gain, Some(-3.0));
    assert_eq!(album.rg_album_peak, Some(0.5));
}

/// `explicit_status_e_wins` (`:383-387`): `"e"` beats `"c"` in either order, `"c"` only
/// when the current value is not `"e"`, and an empty value is a no-op.
#[test]
fn explicit_status_e_wins() {
    let with = |paths: &[(&str, &str)]| -> String {
        let group: Vec<MediaFile> = paths
            .iter()
            .map(|(path, status)| {
                let mut m = track(path);
                m.explicit_status = status.to_string();
                m
            })
            .collect();
        let refs: Vec<&MediaFile> = group.iter().collect();
        let entries: Vec<(&str, TrackFacts)> = group
            .iter()
            .map(|m| (m.path.as_str(), facts(1, None, None)))
            .collect();
        aggregate(&refs, &entries).explicit_status
    };
    assert_eq!(with(&[("a.mp3", "e"), ("b.mp3", "c")]), "e");
    assert_eq!(with(&[("a.mp3", "c"), ("b.mp3", "e")]), "e");
    assert_eq!(with(&[("a.mp3", "c"), ("b.mp3", "c")]), "c");
    assert_eq!(
        with(&[("a.mp3", ""), ("b.mp3", "")]),
        "",
        "empty is a no-op"
    );
    assert_eq!(with(&[("a.mp3", ""), ("b.mp3", "c")]), "c");
    assert_eq!(with(&[("a.mp3", "e"), ("b.mp3", "")]), "e");
}

/// `album_tags_frequency_and_case_folding` (`model/tag.go:26-57` +
/// `model/album.go:115-122`): frequency is keyed on the tag ID, so case-only differences
/// COMBINE while the display value is the last occurrence's; value order is frequency
/// descending then first-seen; names without `album: true` are dropped.
#[test]
fn album_tags_frequency_and_case_folding() {
    let tag = |path: &str, values: Vec<String>| {
        let mut m = track(path);
        m.tags = nd_mapping::mdfile::Tags::from([("genre".to_string(), values)]);
        m
    };
    // Executed Go vector: {Punk} + {Rock} + {Alternative, Rock} → [Rock, Punk, Alternative].
    let a = tag("a.mp3", vec!["Punk".to_string()]);
    let b = tag("b.mp3", vec!["Rock".to_string()]);
    let c = tag("c.mp3", vec!["Alternative".to_string(), "Rock".to_string()]);
    let album = aggregate(
        &[&a, &b, &c],
        &[
            ("a.mp3", facts(1, None, None)),
            ("b.mp3", facts(1, None, None)),
            ("c.mp3", facts(1, None, None)),
        ],
    );
    assert_eq!(
        album.tags.get("genre").map(Vec::as_slice),
        Some(
            &[
                "Rock".to_string(),
                "Punk".to_string(),
                "Alternative".to_string()
            ][..]
        ),
        "frequency desc, then first-seen (executed)"
    );
    assert_eq!(album.genre, "", "genre stays PostScan-derived on a draft");
    assert!(album.genres.is_empty());

    // Case-only differences share one tag ID and combine; the LAST occurrence's value is
    // displayed (executed `{synthwave} + {Synthwave}` → `[Synthwave]`).
    let x = tag("a.mp3", vec!["synthwave".to_string()]);
    let y = tag("b.mp3", vec!["Synthwave".to_string()]);
    let album = aggregate(
        &[&x, &y],
        &[
            ("a.mp3", facts(1, None, None)),
            ("b.mp3", facts(1, None, None)),
        ],
    );
    assert_eq!(
        album.tags.get("genre").map(Vec::as_slice),
        Some(&["Synthwave".to_string()][..]),
        "one id, combined frequency, last display value"
    );

    // A same-track duplicate collapses, and a name that is not album-level is dropped
    // while an album-level one survives.
    let mut m = track("a.mp3");
    m.tags = nd_mapping::mdfile::Tags::from([
        (
            "genre".to_string(),
            vec!["Rock".to_string(), "Rock".to_string()],
        ),
        ("subtitle".to_string(), vec!["Hidden".to_string()]),
        ("albumversion".to_string(), vec!["Deluxe".to_string()]),
        ("tracktotal".to_string(), vec!["12".to_string()]),
    ]);
    let album = aggregate(&[&m], &[("a.mp3", facts(1, None, None))]);
    assert_eq!(
        album.tags.get("genre").map(Vec::as_slice),
        Some(&["Rock".to_string()][..]),
        "a duplicate value collapses to one tag id"
    );
    assert!(
        !album.tags.contains_key("subtitle"),
        "`subtitle` has no `album: true` in mappings.yaml, so SetTags drops it"
    );
    assert!(album.tags.contains_key("albumversion"));
    assert!(album.tags.contains_key("tracktotal"));
    assert_eq!(
        album.tags.keys().collect::<Vec<_>>(),
        ["albumversion", "genre", "tracktotal"],
        "the three album-level names survive, subtitle does not"
    );
}

/// `fix_album_artist_both_arms` (`:484-498`): a non-compilation album with an empty album
/// artist id takes the first artist credit (Go's zero `Artist{}` when the role is absent),
/// and a compilation with more than one distinct album-artist id becomes
/// `Various Artists` / `63sqASlAfjbGMuLP4JhnZU`.
#[test]
fn fix_album_artist_both_arms() {
    let credit = |id: &str, name: &str| Credit {
        id: id.to_string(),
        name: name.to_string(),
        sort_artist_name: String::new(),
        order_artist_name: String::new(),
        mbz_artist_id: String::new(),
        sub_role: String::new(),
    };

    // Arm 1: not a compilation, no album-artist id → the first artist credit wins, and
    // the last-track-wins block is what left the id empty in the first place.
    let mut a = track("a.mp3");
    a.album_artist = String::new();
    a.album_artist_id = String::new();
    a.compilation = false;
    let mut credits = RoleCredits::default();
    credits.add(ROLE_ARTIST, [credit("artist-1", "Artist Name")]);
    a.participants = credits;
    let album = aggregate(&[&a], &[("a.mp3", facts(1, None, None))]);
    assert_eq!(album.album_artist, "Artist Name");
    assert_eq!(album.album_artist_id, "artist-1");

    // Arm 1b: no artist role at all → Go's zero Artist{}.
    let album = aggregate(&[&track("a.mp3")], &[("a.mp3", facts(1, None, None))]);
    assert_eq!(album.album_artist, "");
    assert_eq!(album.album_artist_id, "");

    // Arm 1c: a non-empty album-artist id is left alone even when the artist role differs.
    let mut b = track("a.mp3");
    b.album_artist = "Album Artist".to_string();
    b.album_artist_id = "aa-1".to_string();
    b.compilation = false;
    let mut credits = RoleCredits::default();
    credits.add(ROLE_ARTIST, [credit("artist-1", "Track Artist")]);
    b.participants = credits;
    let album = aggregate(&[&b], &[("a.mp3", facts(1, None, None))]);
    assert_eq!(album.album_artist, "Album Artist");
    assert_eq!(album.album_artist_id, "aa-1");

    // Arm 2: compilation with two distinct album-artist ids → Various Artists (executed
    // `{X1,X2} → "Various Artists"`).
    let mut c = track("a.mp3");
    c.album_artist = "X".to_string();
    c.album_artist_id = "X1".to_string();
    c.compilation = true;
    let mut credits = RoleCredits::default();
    credits.add(ROLE_ALBUM_ARTIST, [credit("X1", "X"), credit("X2", "Y")]);
    c.participants = credits;
    let album = aggregate(&[&c], &[("a.mp3", facts(1, None, None))]);
    assert_eq!(album.album_artist, VARIOUS_ARTISTS);
    assert_eq!(album.album_artist_id, VARIOUS_ARTISTS_ID);

    // Arm 2b: compilation with ONE distinct id (a duplicated id is not two) → unchanged.
    let mut d = track("a.mp3");
    d.album_artist = "X".to_string();
    d.album_artist_id = "X1".to_string();
    d.compilation = true;
    let mut credits = RoleCredits::default();
    credits.add(ROLE_ALBUM_ARTIST, [credit("X1", "X")]);
    d.participants = credits;
    let album = aggregate(&[&d], &[("a.mp3", facts(1, None, None))]);
    assert_eq!(
        album.album_artist, "X",
        "one distinct id is not Various Artists"
    );
    assert_eq!(album.album_artist_id, "X1");
}

/// Go's `Participants.Merge` (`model/participants.go:129-147`): dedup by `ID + SubRole`
/// with the FIRST occurrence winning; the same id under a different sub-role is a
/// separate entry (executed).
#[test]
fn participant_merge_dedups_by_id_and_subrole_first_wins() {
    let credit = |id: &str, name: &str, sub: &str| Credit {
        id: id.to_string(),
        name: name.to_string(),
        sort_artist_name: String::new(),
        order_artist_name: String::new(),
        mbz_artist_id: String::new(),
        sub_role: sub.to_string(),
    };
    // Same path-sorted order: track A carries (x,lead,"First"), track B carries
    // (x,lead,"Second") and (x,feat,"OtherSub"). Go keeps "First" and both sub-roles.
    let mut a = track("a.mp3");
    let mut a_credits = RoleCredits::default();
    a_credits.add_with_sub_role(ROLE_ARTIST, "lead", [credit("x", "First", "")]);
    a.participants = a_credits;

    let mut b = track("b.mp3");
    let mut b_credits = RoleCredits::default();
    b_credits.add_with_sub_role(ROLE_ARTIST, "lead", [credit("x", "Second", "")]);
    b_credits.add_with_sub_role(ROLE_ARTIST, "feat", [credit("x", "OtherSub", "")]);
    b.participants = b_credits;

    let album = aggregate(
        &[&a, &b],
        &[
            ("a.mp3", facts(1, None, None)),
            ("b.mp3", facts(1, None, None)),
        ],
    );
    let artists = &album.participants[&nd_db::sql_participations::Role::from(ROLE_ARTIST.as_str())];
    assert_eq!(
        artists
            .iter()
            .map(|p| (p.id.as_str(), p.name.as_str(), p.sub_role.as_str()))
            .collect::<Vec<_>>(),
        vec![("x", "First", "lead"), ("x", "OtherSub", "feat")],
        "first occurrence wins on id+subRole; a different subRole is a separate entry"
    );
    assert_eq!(
        album
            .participant_names
            .iter()
            .map(|n| (n.id.as_str(), n.name.as_str()))
            .collect::<Vec<_>>(),
        vec![("x", "First"), ("x", "OtherSub")],
        "the FTS name list comes from the same merged credits"
    );
}

/// `mixed_group_is_rejected` — the deliberate deviation (`subplans/scanner.md` SC-04 §4):
/// Go has no mixed-group guard, the port refuses it as a programmer/input error.
#[test]
fn mixed_group_is_rejected() {
    let mut a = track("a.mp3");
    a.album_id = "ALBUM-1".to_string();
    let mut b = track("b.mp3");
    b.album_id = "ALBUM-2".to_string();
    let err = album_draft(
        &[&a, &b],
        facts_by_path(&[
            ("a.mp3", facts(1, None, None)),
            ("b.mp3", facts(1, None, None)),
        ]),
        mappings(),
        IMPORTED,
    )
    .expect_err("a mixed album is a caller defect");
    match err {
        AggregateError::MixedGroup {
            album_id,
            path,
            other_album_id,
            other_library_id,
        } => {
            assert_eq!(album_id, "ALBUM-1");
            assert_eq!(path, "b.mp3");
            assert_eq!(other_album_id, "ALBUM-2");
            assert_eq!(other_library_id, 1);
        }
        other => panic!("expected MixedGroup, got {other:?}"),
    }

    // The same refusal for a disagreeing library, with the album ids matching.
    let mut c = track("a.mp3");
    c.library_id = 1;
    let mut d = track("b.mp3");
    d.library_id = 7;
    let err = album_draft(
        &[&c, &d],
        facts_by_path(&[
            ("a.mp3", facts(1, None, None)),
            ("b.mp3", facts(1, None, None)),
        ]),
        mappings(),
        IMPORTED,
    )
    .expect_err("one album cannot span two libraries");
    assert!(
        matches!(err, AggregateError::MixedGroup { .. }),
        "expected MixedGroup, got {err:?}"
    );
}

/// A member with no supplied facts is refused rather than given a fabricated zero-size
/// snapshot — the arm `phase1.rs` used to reach for `unwrap_or_default()`.
#[test]
fn a_member_without_facts_is_refused() {
    let a = track("a.mp3");
    let b = track("b.mp3");
    let err = album_draft(
        &[&a, &b],
        facts_by_path(&[("a.mp3", facts(1, None, None))]),
        mappings(),
        IMPORTED,
    )
    .expect_err("b.mp3 has no facts");
    match err {
        AggregateError::MissingTrackFacts { path } => assert_eq!(path, "b.mp3"),
        other => panic!("expected MissingTrackFacts, got {other:?}"),
    }
}

/// `song_count_overflow` / size overflow: Go's `SongCount` is a 64-bit `int` and its
/// `Size` sum wraps; the record's column is `i32`/`i64`, so both sums are checked. The
/// count arm needs more than `i32::MAX` members and is therefore pinned on the checked
/// conversion itself; the size arm is executed with real values.
#[test]
fn sums_are_checked_instead_of_wrapping() {
    // Size: two members whose sum passes i64::MAX.
    let a = track("a.mp3");
    let b = track("b.mp3");
    let entries: &[(&str, TrackFacts)] = &[
        ("a.mp3", facts(i64::MAX, None, None)),
        ("b.mp3", facts(1, None, None)),
    ];
    let err = album_draft(&[&a, &b], facts_by_path(entries), mappings(), IMPORTED)
        .expect_err("a wrapping size sum must not be stored");
    assert!(
        matches!(err, AggregateError::SizeOverflow { tracks: 2 }),
        "expected SizeOverflow, got {err:?}"
    );

    // SongCount: `i32::try_from` is the gate Go's 64-bit int never needs. A group of
    // 2^31 members cannot be built in a test, so the narrowing itself is asserted here
    // and the aggregator's use of it is the same expression as above.
    assert!(i32::try_from(i64::from(i32::MAX) + 1).is_err());
    assert_eq!(i32::try_from(2usize).expect("fits"), 2);
}

/// Projection-level regression for the per-member time arms: `album_drafts` must resolve
/// facts per member, so `created_at` is the OLDEST birth in the group and `updated_at` the
/// NEWEST mtime — host-independent, since the facts are supplied, not read from a
/// filesystem.
#[test]
fn album_drafts_resolves_times_per_member() {
    let mut a = track("a.mp3");
    a.album_id = "ALBUM-1".to_string();
    let mut b = track("b.mp3");
    b.album_id = "ALBUM-1".to_string();

    let drafts = album_drafts(
        &[&b, &a],
        facts_by_path(&[
            ("a.mp3", facts(10, Some(at(500)), Some(at(900)))),
            ("b.mp3", facts(20, Some(at(700)), Some(at(300)))),
        ]),
        mappings(),
        IMPORTED,
    )
    .expect("one group aggregates");
    assert_eq!(drafts.len(), 1, "both tracks share one album");
    assert_eq!(drafts[0].song_count, 2);
    assert_eq!(
        drafts[0].created_at,
        Some(at(300)),
        "oldest birth across the group"
    );
    assert_eq!(
        drafts[0].updated_at,
        Some(at(700)),
        "newest mtime across the group"
    );
    assert_eq!(drafts[0].size, 30, "size sums across the group");

    // All-zero births (and absent mtimes) → CreatedAt stays zero, i.e. None/NULL, not the
    // epoch and not the caller's stamp (executed `model/mediafile_test.go:123-134`).
    let drafts = album_drafts(
        &[&a, &b],
        facts_by_path(&[
            ("a.mp3", facts(1, None, None)),
            ("b.mp3", facts(1, None, None)),
        ]),
        mappings(),
        IMPORTED,
    )
    .expect("one group aggregates");
    assert_eq!(
        drafts[0].created_at, None,
        "an all-zero birth group stays zero"
    );
    assert_eq!(drafts[0].updated_at, None);
    assert_eq!(
        drafts[0].imported_at, IMPORTED,
        "imported_at is the caller's stamp, independently"
    );

    // Two groups in one folder still emit one draft per album, in album-id order (the
    // documented deviation from Go's nondeterministic first-occurrence map order).
    let mut x = track("x.mp3");
    x.album_id = "ALBUM-A".to_string();
    let mut y = track("y.mp3");
    y.album_id = "ALBUM-B".to_string();
    let drafts = album_drafts(
        &[&y, &x],
        facts_by_path(&[
            ("x.mp3", facts(1, None, None)),
            ("y.mp3", facts(1, None, None)),
        ]),
        mappings(),
        IMPORTED,
    )
    .expect("two singleton groups");
    assert_eq!(
        drafts.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
        ["ALBUM-A", "ALBUM-B"],
        "emitted in album-id order"
    );
    assert!(drafts.iter().all(|d| d.song_count == 1));

    // A mixed group surfaces through ProjectionError::Aggregate.
    // A single album group cannot disagree on album_id here (the grouping keys on it), so
    // the projection-level arm is a library disagreement: one album spanning two libs.
    let mut y_wrong_library = track("y.mp3");
    y_wrong_library.album_id = "ALBUM-A".to_string();
    y_wrong_library.library_id = 7;
    let err = album_drafts(
        &[&x, &y_wrong_library],
        facts_by_path(&[
            ("x.mp3", facts(1, None, None)),
            ("y.mp3", facts(1, None, None)),
        ]),
        mappings(),
        IMPORTED,
    );
    let Err(ProjectionError::Aggregate(AggregateError::MixedGroup {
        other_library_id, ..
    })) = err
    else {
        panic!("expected Aggregate(MixedGroup), got {err:?}");
    };
    assert_eq!(
        other_library_id, 7,
        "the refusal names the disagreeing library"
    );
}

/// `album_level_tags` stays consistent with `resources/mappings.yaml`'s `album: true` set:
/// the names this lane keeps must be exactly the mappings flagged album-level, so a
/// change to the YAML cannot silently widen what an album row stores.
#[test]
fn album_level_names_match_the_mappings() {
    let album_level: Vec<&str> = mappings()
        .all()
        .iter()
        .filter(|(_name, conf)| conf.album)
        .map(|(name, _conf)| name.as_str())
        .collect();
    assert!(
        album_level.contains(&"genre"),
        "genre must be album-level or the genre derivation would be dead"
    );
    assert!(album_level.contains(&"mood"));
    assert!(album_level.contains(&"tracktotal"));
    assert!(!album_level.contains(&"subtitle"));
    assert!(!album_level.contains(&"title"));
    assert!(!album_level.contains(&"albumartist"));

    // And the aggregator keeps exactly that set, from a group carrying a mixture.
    let mut m = track("a.mp3");
    let mut tags: nd_mapping::mdfile::Tags = nd_mapping::mdfile::Tags::new();
    for name in album_level.iter() {
        tags.insert((*name).to_string(), vec!["v".to_string()]);
    }
    let non_album = [
        "subtitle",
        "title",
        "albumartist",
        "artist",
        "track",
        "disc",
        "grouping_non_album",
    ];
    for name in non_album {
        assert!(
            !album_level.contains(&name),
            "{name} must not be album-level or the negative control below is vacuous"
        );
        tags.insert(name.to_string(), vec!["v".to_string()]);
    }
    m.tags = tags;
    let album = aggregate(&[&m], &[("a.mp3", facts(1, None, None))]);
    assert_eq!(
        album.tags.len(),
        album_level.len(),
        "exactly the album-level names survive"
    );
    for name in [
        "subtitle",
        "title",
        "albumartist",
        "artist",
        "track",
        "disc",
    ] {
        assert!(!album.tags.contains_key(name), "{name} is not album-level");
    }
}
