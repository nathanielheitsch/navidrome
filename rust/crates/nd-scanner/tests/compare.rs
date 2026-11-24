//! SC-04 — executed proof for the semantic equality arms.
//!
//! Every expectation is a **measured** Go verdict, not this lane's inference. The `C*`,
//! `M*`, `T*`, `P*`, `A*`, `D*` labels in the assertions are the identifiers of the
//! verdicts printed by the executed Go harness against this repository's `model` package
//! (`Album.Equals` `model/album.go:87-100`, `MediaFile.Hash/Equals/IsEquivalent`
//! `model/mediafile.go:230-253`, `Tags.Hash` `model/tag.go:127-136`,
//! `Participants.Hash` `model/participants.go:188-199`).
//! `docs/rust-port/subplans/SC-04-go-truth-brief.md` §C/§D records the same measurements;
//! one line of it disagreed with the executed code and is corrected in
//! `src/compare.rs`'s deviation 2.
//!
//! The comparator is a pure boolean over the record DTOs: no DB, no clock, no hash crate.

use chrono::{DateTime, TimeZone, Utc};
use nd_db::album_repo::{AlbumRecord, NamedParticipant};
use nd_db::artwork_hydration::ItemImage;
use nd_db::mediafile_repo::MediaFileRecord;
use nd_db::sql_participations::{Participant, Participants, Role};
use nd_db::sql_tags::{DbTags, Genre};
use nd_scanner::aggregate::zero_album;
use nd_scanner::compare::{album_equals, is_equivalent, media_equals};

const STAMP: DateTime<Utc> = DateTime::from_timestamp(1_800_000_000, 0).expect("valid stamp");

fn at(unix: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(unix, 0)
        .single()
        .expect("in chrono's range")
}

/// A populated album record, so a mutation of any included field is a real change rather
/// than a zero-to-zero comparison.
fn album() -> AlbumRecord {
    AlbumRecord {
        id: "ALBUM-1".to_string(),
        library_id: 1,
        name: "Album".to_string(),
        embed_art_path: "a/01.mp3".to_string(),
        album_artist: "Album Artist".to_string(),
        album_artist_id: "aa-1".to_string(),
        min_year: 1999,
        max_year: 2001,
        date: "1999".to_string(),
        min_original_year: 1990,
        max_original_year: 1990,
        original_date: "1990".to_string(),
        release_date: "2001-05".to_string(),
        compilation: false,
        comment: "Comment".to_string(),
        song_count: 3,
        duration: 100.0,
        size: 4096,
        discs: Some([(1, String::new())].into_iter().collect()),
        sort_album_name: "Sort".to_string(),
        sort_album_artist_name: "SortAA".to_string(),
        order_album_name: "Order".to_string(),
        order_album_artist_name: "OrderAA".to_string(),
        catalog_num: "CAT-1".to_string(),
        mbz_album_id: "mbz-a".to_string(),
        mbz_album_artist_id: "mbz-aa".to_string(),
        mbz_album_type: "Album".to_string(),
        mbz_album_comment: "MBZ comment".to_string(),
        mbz_release_group_id: "mbz-rg".to_string(),
        folder_ids: Some(vec!["F1".to_string(), "F2".to_string()]),
        explicit_status: "e".to_string(),
        rg_album_gain: Some(-8.0),
        rg_album_peak: Some(0.5),
        description: String::new(),
        small_image_url: String::new(),
        medium_image_url: String::new(),
        large_image_url: String::new(),
        external_url: String::new(),
        external_info_updated_at: None,
        genre: String::new(),
        tags: DbTags::new(),
        participants: Participants::new(),
        participant_names: Vec::new(),
        missing: false,
        imported_at: STAMP,
        created_at: Some(at(100)),
        updated_at: Some(at(200)),
        library_path: String::new(),
        library_name: String::new(),
        annotation: Default::default(),
        genres: Vec::new(),
        image: ItemImage::default(),
    }
}

/// A populated media record — a DB-read shape, since `genre`/`genres` are PostScan-derived
/// in Go (`mediafile_repository.go:49-66`).
fn media() -> MediaFileRecord {
    MediaFileRecord {
        id: "mf-1".to_string(),
        album: "Album".to_string(),
        album_artist: "Album Artist".to_string(),
        album_artist_id: "aa-1".to_string(),
        album_id: "ALBUM-1".to_string(),
        artist: "Artist".to_string(),
        artist_id: "ar-1".to_string(),
        birth_time: at(1_000),
        bit_depth: Some(16),
        bit_rate: 128,
        bpm: Some(90),
        catalog_num: "CAT-1".to_string(),
        channels: 2,
        codec: "mp3".to_string(),
        comment: "Comment".to_string(),
        compilation: false,
        created_at: Some(at(100)),
        date: "1999".to_string(),
        disc_number: 1,
        disc_subtitle: "Disc one".to_string(),
        duration: 100.2,
        explicit_status: "e".to_string(),
        folder_id: "F1".to_string(),
        genre: "Rock".to_string(),
        has_cover_art: true,
        library_id: 1,
        lyrics: "Lyrics".to_string(),
        mbz_recording_id: "mbz-r".to_string(),
        mbz_release_track_id: "mbz-rt".to_string(),
        mbz_album_id: "mbz-a".to_string(),
        mbz_release_group_id: "mbz-rg".to_string(),
        mbz_artist_id: "mbz-ar".to_string(),
        mbz_album_artist_id: "mbz-aa".to_string(),
        mbz_album_type: "Album".to_string(),
        mbz_album_comment: "MBZ comment".to_string(),
        missing: false,
        order_album_artist_name: "OrderAA".to_string(),
        order_album_name: "Order".to_string(),
        order_artist_name: "OrderA".to_string(),
        order_title: "OrderT".to_string(),
        original_date: "1990".to_string(),
        original_year: 1990,
        path: "album/01.mp3".to_string(),
        pid: "pid-1".to_string(),
        probe_data: "{}".to_string(),
        release_date: "2001-05".to_string(),
        release_year: 2001,
        rg_album_gain: Some(-8.0),
        rg_album_peak: Some(0.5),
        rg_track_gain: Some(-7.5),
        rg_track_peak: Some(0.9),
        sample_rate: 44_100,
        size: 4096,
        sort_album_artist_name: "SortAA".to_string(),
        sort_album_name: "Sort".to_string(),
        sort_artist_name: "SortA".to_string(),
        sort_title: "SortT".to_string(),
        suffix: "mp3".to_string(),
        tags: DbTags::from([("genre".to_string(), vec!["Rock".to_string()])]),
        title: "Song".to_string(),
        track_number: 1,
        year: 1999,
        participants: Participants::from([(
            Role::from("artist"),
            vec![Participant {
                id: "ar-1".to_string(),
                name: "Artist".to_string(),
                sub_role: "lead".to_string(),
            }],
        )]),
        participant_names: vec![NamedParticipant {
            id: "ar-1".to_string(),
            name: "Artist".to_string(),
            sort_name: String::new(),
            sub_role: "lead".to_string(),
        }],
        updated_at: Some(at(200)),
        library_path: "/music".to_string(),
        library_name: "Main".to_string(),
        annotation: Default::default(),
        bookmark_position: 0,
        genres: vec![Genre {
            id: "gid".to_string(),
            name: "Rock".to_string(),
        }],
        image: ItemImage::default(),
        // `album_image` is a media record field too; mirror the same zero state.
        album_image: ItemImage::default(),
    }
}

/// `(label, mutation)` pairs, applied to the right-hand record only.
type Mutation = (&'static str, fn(&mut AlbumRecord));

/// Every field Go's `Album.Equals` includes (`model/album.go:19-72` minus
/// `hash:"ignore"`). Each mutation must be reported **changed** — the executed album
/// verdicts `C4`, `C5`, `C8`-`C12`, `C18`.
#[test]
fn album_equals_included_fields() {
    let mutations: Vec<Mutation> = vec![
        ("id", |a| a.id = "ALBUM-2".to_string()),
        ("library_id", |a| a.library_id = 2),
        ("name", |a| a.name = "Other".to_string()),
        ("embed_art_path", |a| {
            a.embed_art_path = "a/02.mp3".to_string()
        }),
        ("album_artist", |a| a.album_artist = "Other".to_string()),
        ("album_artist_id", |a| {
            a.album_artist_id = "aa-2".to_string()
        }),
        ("min_year", |a| a.min_year = 1998),
        ("max_year", |a| a.max_year = 2002),
        ("date", |a| a.date = "2000".to_string()),
        ("min_original_year", |a| a.min_original_year = 1991),
        ("max_original_year", |a| a.max_original_year = 1991),
        ("original_date", |a| a.original_date = "1991".to_string()),
        ("release_date", |a| a.release_date = "2002-05".to_string()),
        ("compilation", |a| a.compilation = true),
        ("comment", |a| a.comment = "Other".to_string()),
        ("song_count", |a| a.song_count = 4),
        ("size", |a| a.size = 4097),
        ("sort_album_name", |a| {
            a.sort_album_name = "Other".to_string()
        }),
        ("sort_album_artist_name", |a| {
            a.sort_album_artist_name = "Other".to_string()
        }),
        ("order_album_name", |a| {
            a.order_album_name = "Other".to_string()
        }),
        ("order_album_artist_name", |a| {
            a.order_album_artist_name = "Other".to_string()
        }),
        ("catalog_num", |a| a.catalog_num = "CAT-2".to_string()),
        ("mbz_album_id", |a| a.mbz_album_id = "mbz-b".to_string()),
        ("mbz_album_artist_id", |a| {
            a.mbz_album_artist_id = "mbz-b".to_string()
        }),
        ("mbz_album_type", |a| {
            a.mbz_album_type = "Single".to_string()
        }),
        ("mbz_album_comment", |a| {
            a.mbz_album_comment = "Other".to_string()
        }),
        ("mbz_release_group_id", |a| {
            a.mbz_release_group_id = "mbz-b".to_string()
        }),
        ("explicit_status", |a| a.explicit_status = "c".to_string()),
        ("missing", |a| a.missing = true),
        ("created_at", |a| a.created_at = Some(at(101))),
        ("updated_at", |a| a.updated_at = Some(at(201))),
        ("discs: subtitle change (C5)", |a| {
            a.discs = Some([(1, "B".to_string())].into_iter().collect())
        }),
        ("discs: nil vs {1:\"\"} (C4, A3)", |a| a.discs = None),
        ("discs: {1:\"\"} vs {} (A3)", |a| {
            a.discs = Some(Default::default())
        }),
        ("duration ±1.0 (C18)", |a| a.duration = 99.0),
        ("rg_album_gain: Some → None (C1)", |a| {
            a.rg_album_gain = None
        }),
        ("rg_album_gain: Some(0.0) (C1)", |a| {
            a.rg_album_gain = Some(0.0)
        }),
        ("rg_album_peak: Some → None (C1)", |a| {
            a.rg_album_peak = None
        }),
        ("folder_ids: drop one (brief C)", |a| {
            a.folder_ids = Some(vec!["F1".to_string()])
        }),
        ("folder_ids: Some → None (A1)", |a| a.folder_ids = None),
        ("folder_ids: Some(vec![]) (C7)", |a| {
            a.folder_ids = Some(vec![])
        }),
    ];

    let base = album();
    assert!(album_equals(&base, &base.clone()), "a record equals itself");
    for (label, mutation) in &mutations {
        let mut other = base.clone();
        mutation(&mut other);
        assert!(
            !album_equals(&base, &other),
            "included field {label} must be reported changed"
        );
        assert!(
            !album_equals(&other, &base),
            "included field {label} must be symmetric (executed C1/C2 both directions)"
        );
    }
    assert!(
        mutations.len() > 30,
        "the included table must cover Go's whole include set, not a token subset"
    );
}

/// Every field Go's `Album.Equals` ignores (`hash:"ignore"` at `album.go:16-22`,
/// `57-70`). Each mutation must be reported **unchanged** — the executed verdicts
/// `C13`-`C16`, `C19`-`C21`, `A4`, `A5`, `A6`, `A7`, and `D3` for the duration floor.
#[test]
fn album_equals_ignored_fields() {
    let mutations: Vec<Mutation> = vec![
        ("genre (C15/A6)", |a| a.genre = "Punk".to_string()),
        ("genres (C16/A7)", |a| {
            a.genres = vec![Genre {
                id: "g".to_string(),
                name: "Punk".to_string(),
            }]
        }),
        ("tags (A6)", |a| {
            a.tags = DbTags::from([("genre".to_string(), vec!["Punk".to_string()])])
        }),
        ("participants (A4)", |a| {
            a.participants = Participants::from([(
                Role::from("albumartist"),
                vec![Participant {
                    id: "x".to_string(),
                    name: "Y".to_string(),
                    sub_role: String::new(),
                }],
            )]);
        }),
        ("participant_names (Rust-side derived)", |a| {
            a.participant_names = vec![NamedParticipant {
                id: "x".to_string(),
                name: "Y".to_string(),
                sort_name: String::new(),
                sub_role: String::new(),
            }];
        }),
        ("description (C13)", |a| a.description = "Desc".to_string()),
        ("small_image_url (C13)", |a| {
            a.small_image_url = "s".to_string()
        }),
        ("medium_image_url (C13)", |a| {
            a.medium_image_url = "m".to_string()
        }),
        ("large_image_url (C13)", |a| {
            a.large_image_url = "l".to_string()
        }),
        ("external_url (C13)", |a| a.external_url = "u".to_string()),
        ("external_info_updated_at (C13)", |a| {
            a.external_info_updated_at = Some(at(500))
        }),
        ("imported_at (C14)", |a| a.imported_at = at(700)),
        ("library_path (C19)", |a| {
            a.library_path = "/other".to_string()
        }),
        ("library_name (C19)", |a| {
            a.library_name = "Other".to_string()
        }),
        ("annotation (C20)", |a| a.annotation.play_count = 7),
        ("annotation starred (C20)", |a| a.annotation.starred = true),
        ("image (C21)", |a| {
            a.image = ItemImage {
                image_hash: "h".to_string(),
                ..ItemImage::default()
            }
        }),
        ("duration ±0.4 floor (D3)", |a| a.duration = 100.4),
        ("duration floor 100.9 vs 100.0 (C17)", |a| {
            a.duration = 100.9
        }),
    ];

    let base = album();
    for (label, mutation) in &mutations {
        let mut other = base.clone();
        mutation(&mut other);
        assert!(
            album_equals(&base, &other),
            "ignored field {label} must be reported unchanged"
        );
        assert!(
            album_equals(&other, &base),
            "ignored field {label} must be symmetric"
        );

        // Non-vacuity (`subplans/scanner.md` SC-04 §7): the SAME ignored mutation on a
        // record that also differs in an included field must still report changed. This
        // fails a comparator that simply returns true.
        let mut changed = base.clone();
        mutation(&mut changed);
        changed.song_count += 1;
        assert!(
            !album_equals(&base, &changed),
            "ignored field {label} must not mask an included change"
        );
    }

    // And the base itself must not be equal to Go's zero album, so the ignored arm cannot
    // be passing because both sides are blank.
    assert!(
        !album_equals(&base, &zero_album(at(999))),
        "a populated album is not equal to the zero album"
    );
}

/// The duration floor boundary (`model/album.go:88-89`): flooring happens on both sides,
/// so values in the same whole-second bucket are equal and values across a second boundary
/// are not. Executed `C17` (`100.9` vs `100.1` equal) and `C18` (`100.0` vs `99.9` not).
#[test]
fn duration_floor_boundary() {
    let base = album();
    let with = |secs: f64| {
        let mut a = base.clone();
        a.duration = secs;
        a
    };
    assert!(
        album_equals(&with(100.9), &with(100.1)),
        "C17: same whole second"
    );
    assert!(
        album_equals(&with(100.999), &with(100.001)),
        "floor ignores the fraction"
    );
    assert!(
        !album_equals(&with(100.0), &with(99.9)),
        "C18: across a second"
    );
    assert!(
        !album_equals(&with(100.0), &with(101.0)),
        "a whole second apart"
    );
    // float32 quantization is applied before the floor, matching Go's field type.
    assert!(
        album_equals(&with(100.2), &with(100.2000001)),
        "D1-style quantization"
    );
}

/// `FolderIDs` `hash:"set"` semantics (`album.go:51`): order- and duplicate-insensitive,
/// but nil is not empty. Executed `C6`, `C7`, `A1`, `A2`.
#[test]
fn folder_ids_set_semantics() {
    let base = album();
    let with = |ids: Option<Vec<String>>| {
        let mut a = base.clone();
        a.folder_ids = ids;
        a
    };
    assert!(
        album_equals(
            &with(Some(vec!["F1".to_string(), "F2".to_string()])),
            &with(Some(vec![
                "F2".to_string(),
                "F1".to_string(),
                "F2".to_string()
            ]))
        ),
        "C6: reordered with a duplicate is equal"
    );
    assert!(
        album_equals(
            &with(Some(vec![
                "F1".to_string(),
                "F2".to_string(),
                "F2".to_string()
            ])),
            &with(Some(vec!["F1".to_string(), "F2".to_string()]))
        ),
        "C6: a duplicate is not a change"
    );
    assert!(
        album_equals(
            &with(Some(vec!["F2".to_string(), "F1".to_string()])),
            &with(Some(vec!["F1".to_string(), "F2".to_string()]))
        ),
        "order alone is not a change"
    );
    assert!(
        !album_equals(&with(None), &with(Some(vec![]))),
        "A1/C7: nil != []"
    );
    assert!(
        !album_equals(&with(Some(vec![])), &with(None)),
        "and the other way round"
    );
    assert!(
        album_equals(&with(Some(vec![])), &with(Some(vec![]))),
        "A2: [] == []"
    );
    assert!(!album_equals(
        &with(Some(vec![])),
        &with(Some(vec!["F1".to_string()]))
    ));
    assert!(
        album_equals(
            &with(Some(vec![
                "F1".to_string(),
                "F1".to_string(),
                "F2".to_string()
            ])),
            &with(Some(vec![
                "F2".to_string(),
                "F2".to_string(),
                "F1".to_string()
            ]))
        ),
        "duplicates on both sides still collapse"
    );
}

/// The pointer/zero asymmetry (`hashstructure`'s `IgnoreZeroValue` skips nil but hashes
/// `Some(0.0)`), executed `C1`, `C2`, `C3` — and the same rule for media's four pointers
/// (`M4`, `M5`, `M6`).
#[test]
fn rg_gain_none_vs_zero_not_equal() {
    let base = album();
    let mut zero = base.clone();
    zero.rg_album_gain = Some(0.0);
    let mut none = base.clone();
    none.rg_album_gain = None;
    assert!(!album_equals(&none, &zero), "C1: nil != Some(0.0)");
    assert!(!album_equals(&zero, &none), "C2: Some(0.0) != nil");
    let mut zero_b = base.clone();
    zero_b.rg_album_gain = Some(0.0);
    assert!(album_equals(&zero, &zero_b), "C3: Some(0.0) == Some(0.0)");

    let mut peak_zero = base.clone();
    peak_zero.rg_album_peak = Some(0.0);
    assert!(
        !album_equals(&base, &peak_zero),
        "the same for the peak column"
    );

    let m = media();
    let mut track_zero = m.clone();
    track_zero.rg_track_gain = Some(0.0);
    assert!(
        !media_equals(&m, &track_zero),
        "M6-style: gain None vs Some(0.0)"
    );
    let mut depth_zero = m.clone();
    depth_zero.bit_depth = Some(0);
    assert!(
        !media_equals(&m, &depth_zero),
        "M4: bit_depth None vs Some(0)"
    );
    let mut bpm_none = m.clone();
    bpm_none.bpm = None;
    assert!(!media_equals(&m, &bpm_none), "M5: bpm Some(90) vs None");
    let mut bpm_zero = m.clone();
    bpm_zero.bpm = Some(0);
    assert!(!media_equals(&m, &bpm_zero), "M5: bpm None vs Some(0)");
}

/// Go's zero album: `MediaFiles{}.ToAlbum()` equals `model.Album{}` (executed), and the
/// port's [`zero_album`] must behave the same way against a record whose fields are all
/// Go's zero — including `Discs`/`FolderIDs` staying nil on both sides.
#[test]
fn zero_album_equals_a_blank_record() {
    let zero = zero_album(at(1_234));
    let mut other = zero_album(at(5_678));
    // imported_at is ignored, so a different stamp stays equal (executed C14).
    assert!(album_equals(&zero, &other), "zero == zero");
    other.discs = Some(Default::default());
    assert!(
        !album_equals(&zero, &other),
        "A3: nil Discs != empty Discs — the empty-group spelling is load-bearing"
    );
    let mut other = zero_album(at(1_234));
    other.folder_ids = Some(vec![]);
    assert!(
        !album_equals(&zero, &other),
        "A1: nil FolderIDs != empty FolderIDs"
    );
}

/// `(label, mutation)` pairs for media records.
type MediaMutation = (&'static str, fn(&mut MediaFileRecord));

/// The fields Go's `MediaFile.Hash` includes. Executed `M10`, `M11`, `M16`-`M18`,
/// `M2`, `M3`: each is reported changed.
#[test]
fn media_equals_included_fields() {
    let mutations: Vec<MediaMutation> = vec![
        ("title (M10)", |m| m.title = "Other".to_string()),
        ("album", |m| m.album = "Other".to_string()),
        ("artist", |m| m.artist = "Other".to_string()),
        ("artist_id", |m| m.artist_id = "ar-2".to_string()),
        ("album_artist", |m| m.album_artist = "Other".to_string()),
        ("album_artist_id", |m| {
            m.album_artist_id = "aa-2".to_string()
        }),
        ("has_cover_art (M10)", |m| m.has_cover_art = false),
        ("track_number (M16)", |m| m.track_number = 2),
        ("disc_number (M16)", |m| m.disc_number = 2),
        ("disc_subtitle (M16)", |m| {
            m.disc_subtitle = "Two".to_string()
        }),
        ("year (M16)", |m| m.year = 2000),
        ("date", |m| m.date = "2000".to_string()),
        ("original_year", |m| m.original_year = 1991),
        ("original_date", |m| m.original_date = "1991".to_string()),
        ("release_year", |m| m.release_year = 2002),
        ("release_date", |m| m.release_date = "2002".to_string()),
        ("size (M10)", |m| m.size = 4097),
        ("suffix", |m| m.suffix = "flac".to_string()),
        ("duration is NOT floored (M19)", |m| m.duration = 100.1),
        ("bit_rate (M18)", |m| m.bit_rate = 96),
        ("sample_rate (M18)", |m| m.sample_rate = 48_000),
        ("bit_depth", |m| m.bit_depth = Some(24)),
        ("channels (M18)", |m| m.channels = 1),
        ("codec (M18)", |m| m.codec = "flac".to_string()),
        ("genre column (M3)", |m| m.genre = "Punk".to_string()),
        ("genres (M2 — structs:\"-\" but hashed)", |m| {
            m.genres = vec![Genre {
                id: "gid".to_string(),
                name: "Punk".to_string(),
            }];
        }),
        ("sort_title", |m| m.sort_title = "Other".to_string()),
        ("sort_album_name (M11)", |m| {
            m.sort_album_name = "Other".to_string()
        }),
        ("sort_artist_name", |m| {
            m.sort_artist_name = "Other".to_string()
        }),
        ("sort_album_artist_name", |m| {
            m.sort_album_artist_name = "Other".to_string()
        }),
        ("order_title (M11)", |m| m.order_title = "Other".to_string()),
        ("order_album_name (M11)", |m| {
            m.order_album_name = "Other".to_string()
        }),
        ("order_artist_name", |m| {
            m.order_artist_name = "Other".to_string()
        }),
        ("order_album_artist_name", |m| {
            m.order_album_artist_name = "Other".to_string()
        }),
        ("compilation (M17)", |m| m.compilation = true),
        ("comment (M16)", |m| m.comment = "Other".to_string()),
        ("lyrics (M16)", |m| m.lyrics = "Other".to_string()),
        ("bpm", |m| m.bpm = Some(120)),
        ("explicit_status (M17)", |m| {
            m.explicit_status = "c".to_string()
        }),
        ("catalog_num", |m| m.catalog_num = "CAT-2".to_string()),
        ("mbz_recording_id", |m| {
            m.mbz_recording_id = "mbz-b".to_string()
        }),
        ("mbz_release_track_id", |m| {
            m.mbz_release_track_id = "mbz-b".to_string()
        }),
        ("mbz_album_id", |m| m.mbz_album_id = "mbz-b".to_string()),
        ("mbz_release_group_id", |m| {
            m.mbz_release_group_id = "mbz-b".to_string()
        }),
        ("mbz_artist_id", |m| m.mbz_artist_id = "mbz-b".to_string()),
        ("mbz_album_artist_id", |m| {
            m.mbz_album_artist_id = "mbz-b".to_string()
        }),
        ("mbz_album_type (M11)", |m| {
            m.mbz_album_type = "Single".to_string()
        }),
        ("mbz_album_comment", |m| {
            m.mbz_album_comment = "Other".to_string()
        }),
        ("rg_album_gain", |m| m.rg_album_gain = Some(-9.0)),
        ("rg_album_peak", |m| m.rg_album_peak = Some(0.4)),
        ("rg_track_gain (M6)", |m| m.rg_track_gain = Some(-8.5)),
        ("rg_track_peak", |m| m.rg_track_peak = Some(0.8)),
        ("tags: a value added (D5)", |m| {
            m.tags = DbTags::from([(
                "genre".to_string(),
                vec!["Rock".to_string(), "Pop".to_string()],
            )]);
        }),
        ("tags: cross-name split (T2)", |m| {
            m.tags = DbTags::from([
                ("genre".to_string(), vec!["Rock".to_string()]),
                ("mood".to_string(), vec!["Pop".to_string()]),
            ]);
        }),
        ("tags: a duplicate value added (T3)", |m| {
            m.tags = DbTags::from([(
                "genre".to_string(),
                vec!["Rock".to_string(), "Rock".to_string()],
            )]);
        }),
        ("participants: subRole change (brief §D)", |m| {
            m.participants = Participants::from([(
                Role::from("artist"),
                vec![Participant {
                    id: "ar-1".to_string(),
                    name: "Artist".to_string(),
                    sub_role: "feat".to_string(),
                }],
            )]);
        }),
        ("participants: id change (brief §D)", |m| {
            m.participants = Participants::from([(
                Role::from("artist"),
                vec![Participant {
                    id: "ar-2".to_string(),
                    name: "Artist".to_string(),
                    sub_role: "lead".to_string(),
                }],
            )]);
        }),
        ("participants: role moved (P6)", |m| {
            m.participants = Participants::from([(
                Role::from("albumartist"),
                vec![Participant {
                    id: "ar-1".to_string(),
                    name: "Artist".to_string(),
                    sub_role: "lead".to_string(),
                }],
            )]);
        }),
        ("participants: entry dropped (P4-style)", |m| {
            m.participants = Participants::new()
        }),
    ];

    let base = media();
    assert!(media_equals(&base, &base.clone()), "a record equals itself");
    for (label, mutation) in &mutations {
        let mut other = base.clone();
        mutation(&mut other);
        assert!(
            !media_equals(&base, &other),
            "included field {label} must be reported changed"
        );
        assert!(
            !media_equals(&other, &base),
            "included field {label} is symmetric"
        );
    }
    assert!(
        mutations.len() > 40,
        "the media include set is large; a token table would not pin it"
    );
}

/// The fields Go marks `hash:"ignore"` (`mediafile.go:28-42`, `51`, `70`, `100-106`),
/// executed `M1`, `M7`-`M9`, `M12`: each is reported unchanged.
#[test]
fn media_equals_ignored_fields() {
    let mutations: Vec<MediaMutation> = vec![
        ("id (M1)", |m| m.id = "mf-2".to_string()),
        ("pid (M1)", |m| m.pid = "pid-2".to_string()),
        ("library_id (M1)", |m| m.library_id = 2),
        ("library_path (M12)", |m| {
            m.library_path = "/other".to_string()
        }),
        ("library_name (M12)", |m| {
            m.library_name = "Other".to_string()
        }),
        ("folder_id (M1)", |m| m.folder_id = "F2".to_string()),
        ("path (M1)", |m| m.path = "other/02.flac".to_string()),
        ("album_id (M1)", |m| m.album_id = "ALBUM-2".to_string()),
        ("probe_data (M8)", |m| {
            m.probe_data = "{\"x\":1}".to_string()
        }),
        ("missing (M7)", |m| m.missing = true),
        ("birth_time (M7)", |m| m.birth_time = at(2_000)),
        ("created_at (M7)", |m| m.created_at = Some(at(300))),
        ("updated_at (M7)", |m| m.updated_at = Some(at(400))),
        ("annotation (M9)", |m| m.annotation.play_count = 5),
        ("annotation rating (M9)", |m| m.annotation.rating = 4),
        ("bookmark_position (M9 — Bookmarkable)", |m| {
            m.bookmark_position = 30
        }),
        ("image (M9)", |m| {
            m.image = ItemImage {
                image_hash: "h".to_string(),
                ..ItemImage::default()
            }
        }),
        ("album_image (M9)", |m| {
            m.album_image = ItemImage {
                image_hash: "h2".to_string(),
                ..ItemImage::default()
            }
        }),
        ("participant_names (Rust-side FTS input, not hashed)", |m| {
            m.participant_names = vec![NamedParticipant {
                id: "other".to_string(),
                name: "Other".to_string(),
                sort_name: "S".to_string(),
                sub_role: "lead".to_string(),
            }];
        }),
    ];

    let base = media();
    for (label, mutation) in &mutations {
        let mut other = base.clone();
        mutation(&mut other);
        assert!(
            media_equals(&base, &other),
            "ignored field {label} must be unchanged"
        );
        assert!(
            media_equals(&other, &base),
            "ignored field {label} is symmetric"
        );

        // Non-vacuity: the same ignored mutation must not mask an included change.
        let mut changed = base.clone();
        mutation(&mut changed);
        changed.title = "Other".to_string();
        assert!(
            !media_equals(&base, &changed),
            "ignored field {label} must not mask an included change"
        );
    }
    assert!(
        !media_equals(&base, &zero_media()),
        "a populated media record is not equal to a blank one"
    );
}

/// A media record with every field at Go's zero, for the non-vacuity guard above.
fn zero_media() -> MediaFileRecord {
    MediaFileRecord {
        id: String::new(),
        album: String::new(),
        album_artist: String::new(),
        album_artist_id: String::new(),
        album_id: String::new(),
        artist: String::new(),
        artist_id: String::new(),
        birth_time: at(0),
        bit_depth: None,
        bit_rate: 0,
        bpm: None,
        catalog_num: String::new(),
        channels: 0,
        codec: String::new(),
        comment: String::new(),
        compilation: false,
        created_at: None,
        date: String::new(),
        disc_number: 0,
        disc_subtitle: String::new(),
        duration: 0.0,
        explicit_status: String::new(),
        folder_id: String::new(),
        genre: String::new(),
        has_cover_art: false,
        library_id: 0,
        lyrics: String::new(),
        mbz_recording_id: String::new(),
        mbz_release_track_id: String::new(),
        mbz_album_id: String::new(),
        mbz_release_group_id: String::new(),
        mbz_artist_id: String::new(),
        mbz_album_artist_id: String::new(),
        mbz_album_type: String::new(),
        mbz_album_comment: String::new(),
        missing: false,
        order_album_artist_name: String::new(),
        order_album_name: String::new(),
        order_artist_name: String::new(),
        order_title: String::new(),
        original_date: String::new(),
        original_year: 0,
        path: String::new(),
        pid: String::new(),
        probe_data: String::new(),
        release_date: String::new(),
        release_year: 0,
        rg_album_gain: None,
        rg_album_peak: None,
        rg_track_gain: None,
        rg_track_peak: None,
        sample_rate: 0,
        size: 0,
        sort_album_artist_name: String::new(),
        sort_album_name: String::new(),
        sort_artist_name: String::new(),
        sort_title: String::new(),
        suffix: String::new(),
        tags: DbTags::new(),
        title: String::new(),
        track_number: 0,
        year: 0,
        participants: Participants::new(),
        participant_names: Vec::new(),
        updated_at: None,
        library_path: String::new(),
        library_name: String::new(),
        annotation: Default::default(),
        bookmark_position: 0,
        genres: Vec::new(),
        image: ItemImage::default(),
        album_image: ItemImage::default(),
    }
}

/// The derived genre IS part of media equality (`M3`, `M2`) — unlike the album's, which
/// is `hash:"ignore"`. Both sides are DB-read rows, because Go derives them in PostScan
/// (`mediafile_repository.go:49-66`).
#[test]
fn genre_is_included_in_media_equality() {
    let base = media();
    let mut no_genre = base.clone();
    no_genre.genre = String::new();
    assert!(
        !media_equals(&base, &no_genre),
        "M3: the genre column is hashed"
    );

    let mut no_genres = base.clone();
    no_genres.genres = Vec::new();
    assert!(
        !media_equals(&base, &no_genres),
        "M2: Genres is hashed despite structs:\"-\""
    );

    // A genre-only tag edit changes the derived genre, so a media row IS refreshed…
    let mut from_tag = base.clone();
    from_tag.genre = "Punk".to_string();
    from_tag.genres = vec![Genre {
        id: "pid".to_string(),
        name: "Punk".to_string(),
    }];
    assert!(!media_equals(&base, &from_tag));

    // …while the album built from the same tags is not (A6/A7), which is the asymmetry the
    // brief's §D note is about.
    let album_base = album();
    let mut album_tagged = album_base.clone();
    album_tagged.genre = "Punk".to_string();
    album_tagged.genres = vec![Genre {
        id: "pid".to_string(),
        name: "Punk".to_string(),
    }];
    assert!(
        album_equals(&album_base, &album_tagged),
        "the album's genre/genres are hash:\"ignore\""
    );
}

/// `Tags.Hash` (`model/tag.go:127-136`): the ID sequence is sorted, so ordering is not
// significant, IDs are lowercased in BOTH name and value, and the empty-vs-nil rule has
/// three distinct outcomes. Executed `T1`-`T10`, `M14`, `M15`, `D5`.
#[test]
fn tags_hash_order_sensitive_but_id_case_insensitive() {
    let with = |tags: DbTags| {
        let mut m = media();
        m.tags = tags;
        m
    };
    let rock_pop = with(DbTags::from([(
        "genre".to_string(),
        vec!["Rock".to_string(), "Pop".to_string()],
    )]));

    // T1: reordering WITHIN a name is equal — the IDs are sorted before hashing. (This is
    // the executed verdict that corrects the brief's §D line; see compare.rs deviation 2.)
    assert!(
        media_equals(
            &rock_pop,
            &with(DbTags::from([(
                "genre".to_string(),
                vec!["Pop".to_string(), "Rock".to_string()]
            )]))
        ),
        "T1: within-name reorder is equal because the ID list is sorted"
    );

    // T5: names and values are lowercased into the tag ID.
    assert!(
        media_equals(
            &rock_pop,
            &with(DbTags::from([(
                "GENRE".to_string(),
                vec!["POP".to_string(), "ROCK".to_string()]
            )]))
        ),
        "T5: case-folded name and values give the same IDs"
    );

    // T2: the same two values split across two names is a different ID sequence.
    assert!(
        !media_equals(
            &rock_pop,
            &with(DbTags::from([
                ("genre".to_string(), vec!["Rock".to_string()]),
                ("mood".to_string(), vec!["Pop".to_string()]),
            ]))
        ),
        "T2: a cross-name split changes the IDs"
    );

    // T3/T4: duplicates are part of the sequence, so adding or collapsing one changes it.
    assert!(
        !media_equals(
            &rock_pop,
            &with(DbTags::from([(
                "genre".to_string(),
                vec!["Rock".to_string(), "Pop".to_string(), "Pop".to_string()]
            )]))
        ),
        "T3: a duplicate value is a change"
    );
    assert!(
        !media_equals(
            &rock_pop,
            &with(DbTags::from([(
                "genre".to_string(),
                vec!["Rock".to_string()]
            )]))
        ),
        "T4: collapsing a value is a change"
    );

    // M14/T6: an empty map equals no map at all (Go's `len(t) == 0` short-circuit).
    assert!(
        media_equals(&with(DbTags::new()), &with(DbTags::new())),
        "T6/M14: Tags{{}} == nil"
    );
    // M15/T7: a name with an empty value list is NOT the empty map.
    assert!(
        !media_equals(
            &with(DbTags::new()),
            &with(DbTags::from([("genre".to_string(), vec![])]))
        ),
        "T7/M15: {{genre: []}} != nil"
    );
    // T8/T10: two different empty names are indistinguishable, and an extra empty name is
    // ignored next to a populated one.
    assert!(
        media_equals(
            &with(DbTags::from([("genre".to_string(), vec![])])),
            &with(DbTags::from([("mood".to_string(), vec![])]))
        ),
        "T8: any empty name hashes to the same empty ID sequence"
    );
    assert!(
        media_equals(
            &with(DbTags::from([(
                "genre".to_string(),
                vec!["Rock".to_string()]
            )])),
            &with(DbTags::from([
                ("genre".to_string(), vec!["Rock".to_string()]),
                ("mood".to_string(), vec![]),
            ]))
        ),
        "T10: an extra empty name contributes nothing"
    );
    // D5: content vs no content.
    assert!(!media_equals(&with(DbTags::new()), &rock_pop), "D5");
}

/// `Participants.Hash` (`model/participants.go:188-199`): only `subRole` + `id` are
/// hashed, so artist names and per-role insertion order are irrelevant, while the role
/// assignment and duplicate entries are significant. Executed `P1`-`P6`, `M13`.
#[test]
fn participants_hash_ignores_names() {
    let with = |participants: Participants| {
        let mut m = media();
        m.participants = participants;
        m
    };
    // Entries sharing a role accumulate into that role's list, so the ordering arm below
    // really reorders two entries inside one role key.
    let artists = |entries: &[(&str, &str, &str)]| -> Participants {
        let mut map: Participants = Participants::new();
        for (role, id, sub) in entries {
            map.entry(Role::from(*role)).or_default().push(Participant {
                id: (*id).to_string(),
                name: "ignored".to_string(),
                sub_role: (*sub).to_string(),
            });
        }
        map
    };
    let base = with(artists(&[("artist", "x", "lead")]));

    // P3: names are not in the hash — swap them all and stay equal.
    assert!(
        media_equals(
            &base,
            &with(Participants::from([(
                Role::from("artist"),
                vec![Participant {
                    id: "x".to_string(),
                    name: "Completely Different".to_string(),
                    sub_role: "lead".to_string(),
                }]
            )]))
        ),
        "P3: artist names are not hashed"
    );

    // P2: order within a role is sorted away.
    let two = |order: &[(&str, &str, &str)]| artists(order);
    assert!(
        media_equals(
            &with(two(&[("artist", "x", "lead"), ("artist", "y", "feat")])),
            &with(two(&[("artist", "y", "feat"), ("artist", "x", "lead")]))
        ),
        "P2: insertion order within a role is sorted"
    );
    // The same set spread over two role keys is a different hash (P6).
    assert!(
        !media_equals(
            &with(two(&[("artist", "x", "lead"), ("artist", "y", "feat")])),
            &with(two(&[
                ("artist", "x", "lead"),
                ("albumartist", "y", "feat")
            ]))
        ),
        "P6: the role is part of the hash"
    );

    // P4: a duplicate entry is part of the sequence.
    assert!(
        !media_equals(
            &base,
            &with(Participants::from([(
                Role::from("artist"),
                vec![
                    Participant {
                        id: "x".to_string(),
                        name: "A".to_string(),
                        sub_role: "lead".to_string()
                    },
                    Participant {
                        id: "x".to_string(),
                        name: "A".to_string(),
                        sub_role: "lead".to_string()
                    },
                ]
            )]))
        ),
        "P4: a duplicate entry changes the hash"
    );

    // P5/M13: nil and empty participants are the same thing (Rust spells both empty).
    assert!(
        media_equals(&with(Participants::new()), &base_empty_participants()),
        "P5/M13"
    );

    // A sub-role change is significant even with the same id (brief §D).
    assert!(
        !media_equals(&base, &with(artists(&[("artist", "x", "feat")]))),
        "only subRole:id matters, and subRole changed"
    );
}

/// The media record with no participants, for the nil/empty arm.
fn base_empty_participants() -> MediaFileRecord {
    let mut m = media();
    m.participants = Participants::new();
    m.participant_names = Vec::new();
    m
}

/// `IsEquivalent` (`model/mediafile.go:250-253` → `utils.BaseName`), the executed matrix:
/// directory and extension are irrelevant, the base name must match exactly.
#[test]
fn is_equivalent_basename_matrix() {
    let with = |path: &str| {
        let mut m = media();
        m.path = path.to_string();
        m
    };
    assert!(
        is_equivalent(&with("a/b.mp3"), &with("x/y/b.flac")),
        "directory and suffix ignored"
    );
    assert!(
        is_equivalent(&with("a/b.mp3"), &with("a/b.mp4")),
        "suffix ignored"
    );
    assert!(
        is_equivalent(&with("b"), &with("b.mp3")),
        "no extension on one side"
    );
    assert!(is_equivalent(&with("a/b"), &with("a/b")), "identical");
    assert!(
        !is_equivalent(&with("a/b.mp3"), &with("a/b-extra.mp3")),
        "a different base name"
    );
    assert!(
        !is_equivalent(&with(""), &with("x")),
        "an empty path never matches"
    );
    assert!(
        !is_equivalent(&with("a/b.mp3"), &with("a/c.mp3")),
        "the basename is what is compared, not the directory"
    );
    // A trailing slash: Go's BaseName of `a/b/` is `b`, matching `a/b`.
    assert!(
        is_equivalent(&with("a/b/"), &with("a/b")),
        "a trailing separator"
    );
    // Equivalence is independent of every other field, including ignored ones — and,
    // unlike media_equals, of the content fields too.
    let mut other = with("a/b.mp3");
    other.title = "Totally different".to_string();
    other.size = 999_999;
    assert!(
        is_equivalent(&with("a/b.mp3"), &other),
        "IsEquivalent is a path-only match, used by the missing-track matching"
    );
}

/// The float32 quantization arm: Go's `Duration` field is `float32`
/// (`model/mediafile.go:64`) while the column is `real`, so a stored `f64` must compare
/// the way Go's field does. Executed `D1` (`100.2` vs `100.2000001` equal) and `D2`
/// (`100.2` vs `100.2001` unequal), with no flooring for media (`M19`).
#[test]
fn media_duration_is_quantized_not_floored() {
    let with = |secs: f64| {
        let mut m = media();
        m.duration = secs;
        m
    };
    assert!(
        media_equals(&with(100.2), &with(100.2000001)),
        "D1: below the float32 quantum"
    );
    assert!(!media_equals(&with(100.2), &with(100.2001)), "D2: above it");
    assert!(
        !media_equals(&with(100.2), &with(100.1)),
        "M19: no floor for media"
    );
    assert!(!media_equals(&with(100.0), &with(99.9)), "M20");
}
