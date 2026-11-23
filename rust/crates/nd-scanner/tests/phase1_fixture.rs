//! SC-05 happy-path vertical: one folder walked on a real filesystem, tag-read with
//! the real backend, bridged, projected, committed through DB-14, and read back.
//!
//! Every assertion is per-leg on purpose: dropping the album write, the queue leg, the
//! FTS trigger row, or the tag rows fails its own assertion, so a green file cannot
//! mean "some legs ran". Ids are compared as exact sets, so a minted duplicate fails.

mod common;

use common::{
    children, count, external_root, file_stamp, fresh_store, job, job_with_album_pid,
    job_without_birth, job_without_symlinks, later_stamp, mapping, pool_healthy, pragma,
    pragma_in_lease, remove_db, remove_external_root, reopen_store, scalar, scan_ok, seed_library,
    stamp, strings, Tree, ALL_FORMATS, OVERRIDE_ALBUM_PID,
};

/// The library's PID-derived album ids are computed by the test from the same mapping
/// code the ingest uses, but asserted against the committed Go oracle values in
/// `nd-mapping/tests/data/go_oracle.json`, so a broken spec fails the test rather than
/// moving with the implementation.
const ALBUM_ID_NO_RELEASE_DATE: &str = "1GrQX6RbY8HNgNugy6o63Z";
const ALBUM_ID_WITH_RELEASE_DATE: &str = "4xkV1gQrJ8zGZMSg80SENc";
/// `media_file.pid` — the track PID from the Go oracle (`mapped.pid`).
const TRACK_PID_SHARED: &str = "60ZCo7w97fNb2P5XeOp1wW";
const TRACK_PID_M4A: &str = "6rA48DfWEf5XVoSsJCyNWu";
/// Go's default album PID spec (`consts/consts.go:157`), used as `prev_album_pid` below.
const DEFAULT_ALBUM_PID: &str = nd_id::DEFAULT_ALBUM_PID;

/// `single_folder_single_mp3_end_to_end`: fresh schema + library, one mp3 in
/// `album/one`, one scan. Every leg is asserted separately.
#[test]
fn single_folder_single_mp3_end_to_end() {
    let tree = Tree::new("single-mp3");
    tree.put("album/one", "known.mp3");
    let (store, db) = fresh_store("single-mp3", 2);
    seed_library(&store, 1, "single-mp3", &tree.root);
    let mapping = mapping();

    let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));

    // Leg 0: the ingest's own report.
    assert_eq!(outcome.audio_count, 1, "one audio file was walked");
    assert_eq!(outcome.new_media_count, 1, "no prior child existed");
    assert!(outcome.reimported_paths.is_empty());
    assert!(outcome.folder_inserted, "the folder row was newly inserted");
    assert!(!outcome.hash.is_empty(), "folder.hash is written");
    assert_eq!(
        outcome.album_ids,
        vec![ALBUM_ID_NO_RELEASE_DATE.to_string()]
    );
    assert_eq!(outcome.missing_count, 0);
    assert_eq!(outcome.tag_failures, Vec::new(), "clean tag read");
    assert!(outcome.committed.warnings.is_empty(), "no warn-only DB arm");

    // Leg 1: the folder row, with Go's NewFolder split and the walker's counts.
    let folder_id = outcome.folder_id.clone();
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT name FROM folder WHERE id = '{folder_id}'")
        ),
        Some("one".to_string()),
        "folder.name is the basename (model/folder.go:56-57)"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT path FROM folder WHERE id = '{folder_id}'")
        ),
        Some("album".to_string()),
        "folder.path is the cleaned directory"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT parent_id FROM folder WHERE id = '{folder_id}'")
        ),
        Some(nd_db::folder_repo::folder_id(1, "", "album")),
        "parent_id is the parent folder's hashed id"
    );
    assert_eq!(
        count(
            &store,
            &format!("SELECT num_audio_files FROM folder WHERE id = '{folder_id}'")
        ),
        1
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT hash FROM folder WHERE id = '{folder_id}'")
        ),
        Some(outcome.hash.clone()),
        "the fingerprint written equals the one reported"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT image_files FROM folder WHERE id = '{folder_id}'")
        ),
        Some("[]".to_string()),
        "no images: Go's empty map renders as []"
    );

    // Leg 2: the media_file row, read back through the frozen DB-11 visitor.
    let kids = children(&store, &folder_id);
    assert_eq!(kids.len(), 1, "DB-11 system visitor sees exactly one child");
    let track = &kids[0];
    assert_eq!(track.path, "album/one/known.mp3");
    assert_eq!(track.library_id, 1);
    assert_eq!(track.folder_id, folder_id);
    assert_eq!(track.album_id, ALBUM_ID_NO_RELEASE_DATE);
    assert_eq!(track.album, "Synthetic Album");
    assert_eq!(
        track.title, "Real café",
        "the UTF-8 title survives the round trip"
    );
    assert_eq!(track.artist, "Artist One");
    assert_eq!(track.album_artist, "Album Artist");
    assert_eq!(track.suffix, "mp3");
    assert_eq!(track.track_number, 2);
    assert_eq!(track.year, 2024);
    assert_eq!(
        track.size, 7833,
        "size comes from the storage stat, not the tags"
    );
    // 135, not the Go oracle's 136: Lofty rounds the MPEG frame bitrate differently
    // than TagLib. That divergence is pinned upstream at `mapping_bridge.rs:328-329`
    // and `nd-mapping/tests/go_oracle.rs:374`, and it is a real value, not an absence.
    assert_eq!(track.bit_rate, 135);
    assert_eq!(track.channels, 2);
    assert_eq!(track.bit_depth, None, "mp3 reports no bit depth");
    assert!(
        track.duration > 0.0,
        "duration comes from the audio properties"
    );
    assert_eq!(
        track.pid, TRACK_PID_SHARED,
        "media_file.pid is the track PID"
    );
    assert!(!track.missing);
    assert_ne!(
        track.birth_time,
        stamp(),
        "macOS reports a creation time, so the storage fact wins over the scan stamp"
    );
    assert_eq!(
        track.created_at,
        Some(stamp()),
        "Go's Put stamps a zero CreatedAt with the batch stamp (:184-190)"
    );
    assert_eq!(
        track.updated_at,
        Some(file_stamp()),
        "updated_at is the file's own mtime, NOT the batch stamp"
    );

    // Leg 3: the album draft.
    assert_eq!(
        count(
            &store,
            &format!("SELECT song_count FROM album WHERE id = '{ALBUM_ID_NO_RELEASE_DATE}'")
        ),
        1,
        "one-track-per-folder draft (SC-03); multi-track is SC-04"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT album_artist FROM album WHERE id = '{ALBUM_ID_NO_RELEASE_DATE}'")
        ),
        Some("Album Artist".to_string())
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT folder_ids FROM album WHERE id = '{ALBUM_ID_NO_RELEASE_DATE}'")
        ),
        Some(format!("[\"{folder_id}\"]")),
        "a.SingleFolderIds / FolderIDs is the single folder id"
    );
    assert_eq!(
        count(
            &store,
            &format!("SELECT library_id FROM album WHERE id = '{ALBUM_ID_NO_RELEASE_DATE}'")
        ),
        1
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT created_at FROM album WHERE id = '{ALBUM_ID_NO_RELEASE_DATE}'")
        ),
        Some(nd_db::time::to_text(&track.birth_time)),
        "album.created_at is the SAME resolved birth as the track's (one resolution)"
    );

    // Leg 4: artists, plus the library_artist link.
    let artist_ids = strings(&store, "SELECT id FROM artist ORDER BY id");
    assert_eq!(artist_ids.len(), 2, "albumartist + artist");
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM library_artist WHERE library_id = 1"
        ),
        2,
        "AddArtist ran for each artist (:408)"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file_artists"),
        2,
        "the participant junction rows were written by the media Put"
    );

    // Leg 5: tags — dictionary rows, the media_file.tags cell, and the genre junction.
    assert_eq!(
        outcome.tag_count, 3,
        "genre + disctotal + tracktotal (Go oracle)"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM tag"),
        3,
        "tag_repo::Add wrote the dictionary (:396)"
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM library_tag WHERE library_id = 1"
        ),
        3,
        "the library_tag counts were maintained"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file_tags"),
        1,
        "the indexed (genre) tag junction row"
    );
    let tags_cell = scalar(
        &store,
        &format!("SELECT tags FROM media_file WHERE id = '{}'", track.id),
    )
    .expect("tags cell");
    assert!(
        tags_cell.contains("\"genre\""),
        "tags JSON holds the genre name"
    );
    assert!(
        tags_cell.contains("Rock"),
        "tags JSON holds the genre value"
    );

    // Leg 6: FTS, maintained by the schema's own triggers.
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file_fts"),
        1,
        "media_file_fts_ai fired for the inserted row"
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM album_fts WHERE album_fts MATCH 'Synthetic'"
        ),
        1,
        "album_fts is searchable"
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM artist_fts WHERE artist_fts MATCH 'Artist'"
        ),
        2,
        "both artist rows (Artist One, Album Artist) are searchable"
    );

    // Leg 7: the artwork queue — artist first, then album, priority 50.
    assert_eq!(
        outcome.queued_item_count, 3,
        "2 artists + 1 album (:412-425)"
    );
    assert!(
        outcome.enqueued_if_missing,
        "FullScanInProgress selects EnqueueIfMissing (:467-469)"
    );
    let queue = strings(
        &store,
        "SELECT item_kind || '|' || item_id || '|' || priority FROM artwork_queue ORDER BY item_kind, item_id",
    );
    assert_eq!(queue.len(), 3);
    assert!(
        queue.iter().all(|row| row.ends_with("|50")),
        "every scanner item carries priority 50, got {queue:?}"
    );
    assert!(
        queue
            .iter()
            .any(|row| row.starts_with("al|") && row.contains(ALBUM_ID_NO_RELEASE_DATE)),
        "the album's own queue item is present (kind prefix `al`)"
    );
    assert_eq!(
        queue.iter().filter(|row| row.starts_with("ar|")).count(),
        2,
        "one queue item per artist (kind prefix `ar`)"
    );

    // Leg 8: nothing was published pre-commit and the pool is unharmed.
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file"), 1);
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// `four_formats_per_folder`: all four committed fixtures, one per folder. SC-04 is not
/// implemented, and mp3/flac/ogg all hash to the SAME default-spec album id, so four
/// files in one folder would be a multi-track group (asserted separately as
/// [`ProjectionError::MultiTrackGroup`] in `phase1_faults.rs`). One file per folder still
/// exercises the whole ingest for every format, and the two distinct album ids (the m4a
/// carries `releasedate`) prove the album leg is not writing one blanket row.
#[test]
fn four_formats_per_folder() {
    let tree = Tree::new("four-formats");
    for fixture in ALL_FORMATS {
        tree.put(&folder_for(fixture), fixture);
    }
    let (store, db) = fresh_store("four-formats", 2);
    seed_library(&store, 1, "four-formats", &tree.root);
    let mapping = mapping();

    let mut tracks: std::collections::BTreeMap<String, nd_db::mediafile_repo::MediaFileRecord> =
        std::collections::BTreeMap::new();
    let mut folder_ids = Vec::new();
    let mut queue_total = 0;
    for (index, fixture) in ALL_FORMATS.iter().enumerate() {
        let folder = folder_for(fixture);
        let outcome = scan_ok(&store, &job(1, &tree.root, &folder, stamp(), &mapping));
        assert_eq!(outcome.audio_count, 1, "{fixture} walked");
        assert_eq!(outcome.tag_failures, Vec::new(), "{fixture} parsed cleanly");
        assert_eq!(
            outcome.album_ids.len(),
            1,
            "{fixture} drafts exactly one album"
        );
        assert_eq!(outcome.new_media_count, 1, "{fixture} is a fresh child");
        folder_ids.push(outcome.folder_id.clone());
        for kid in children(&store, &outcome.folder_id) {
            tracks.insert(kid.suffix.clone(), kid);
        }
        // Each scan hands 2 artists + 1 album to the queue; full-scan mode uses
        // EnqueueIfMissing, so a re-scan adds nothing.
        queue_total += outcome.queued_item_count;
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM media_file"),
            i64::try_from(index + 1).expect("index fits"),
            "one media row per scanned folder, no cross-folder leakage"
        );
    }

    assert_eq!(tracks.len(), 4);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file"), 4);
    assert_eq!(folder_ids.len(), 4);
    let unique_folders: std::collections::BTreeSet<String> = folder_ids.iter().cloned().collect();
    assert_eq!(unique_folders.len(), 4, "each folder has its own hashed id");

    let mp3 = tracks.get("mp3").expect("mp3 row").clone();
    let flac = tracks.get("flac").expect("flac row").clone();
    let m4a = tracks.get("m4a").expect("m4a row").clone();
    let ogg = tracks.get("ogg").expect("ogg row").clone();

    // Format-specific columns, one assertion per format.
    // Bitrates are the Rust backend's values. Each one differs from the Go oracle by a
    // gap already declared upstream (`mapping_bridge.rs:322-383`,
    // `nd-mapping/tests/go_oracle.rs:374-395`): Lofty rounds differently. They are
    // asserted here so the vertical stores what the backend reported, verbatim.
    assert_eq!(mp3.bit_rate, 135, "Go's TagLib value is 136 (declared gap)");
    assert_eq!(mp3.bit_depth, None);
    assert_eq!(mp3.channels, 2);
    assert_eq!(mp3.album_id, ALBUM_ID_NO_RELEASE_DATE);
    assert_eq!(mp3.pid, TRACK_PID_SHARED);

    assert_eq!(
        flac.bit_rate, 124,
        "Go's TagLib value is 125 (declared gap)"
    );
    assert_eq!(flac.bit_depth, Some(16), "flac carries a bit depth");
    assert_eq!(flac.pid, TRACK_PID_SHARED);

    assert_eq!(m4a.bit_rate, 94, "Go's TagLib value is 95 (declared gap)");
    assert_eq!(
        m4a.bit_depth, None,
        "Lofty reports 0 for AAC-in-MP4, so the port leaves it absent instead of Go's 16"
    );
    assert_eq!(
        m4a.album_id, ALBUM_ID_WITH_RELEASE_DATE,
        "m4a's releasedate tag changes the default album PID"
    );
    assert_eq!(m4a.pid, TRACK_PID_M4A, "a distinct track PID per fixture");

    assert_eq!(
        ogg.bit_rate, 0,
        "Lofty reports 0 where TagLib derives 30 (declared gap)"
    );
    assert_eq!(ogg.bit_depth, None);
    assert_eq!(ogg.pid, TRACK_PID_SHARED);

    // Paths are per-folder, so a stat/path mix-up cannot pass.
    assert_eq!(mp3.path, "album/known_mp3/known.mp3");
    assert_eq!(flac.path, "album/known_flac/known.flac");
    assert_eq!(m4a.path, "album/known_m4a/known.m4a");
    assert_eq!(ogg.path, "album/known_ogg/known.ogg");

    // Sizes are all distinct.
    let sizes: Vec<i64> = [&mp3, &flac, &m4a, &ogg].iter().map(|t| t.size).collect();
    let unique: std::collections::BTreeSet<i64> = sizes.iter().copied().collect();
    assert_eq!(
        unique.len(),
        4,
        "each fixture's size is stored distinctly: {sizes:?}"
    );
    assert!(
        [&mp3, &flac, &m4a, &ogg].iter().all(|t| t.duration > 0.0),
        "no zero durations"
    );

    // Two album rows exist — not one blanket row, not four.
    assert_eq!(count(&store, "SELECT COUNT(*) FROM album"), 2);
    assert_eq!(
        count(
            &store,
            &format!("SELECT song_count FROM album WHERE id = '{ALBUM_ID_NO_RELEASE_DATE}'")
        ),
        1,
        "still one track per album draft (SC-04 owns aggregation)"
    );
    assert_eq!(
        count(
            &store,
            &format!("SELECT song_count FROM album WHERE id = '{ALBUM_ID_WITH_RELEASE_DATE}'")
        ),
        1
    );

    // Artists are shared across folders, so the artist rows stay at 2.
    assert_eq!(count(&store, "SELECT COUNT(*) FROM artist"), 2);
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM library_artist WHERE library_id = 1"
        ),
        2
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file_artists"),
        8,
        "2 per track"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file_tags"),
        4,
        "1 genre tag per track"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM tag"),
        4,
        "genre, disctotal, tracktotal, encodedby"
    );

    // The queue PK collapses the repeated Enqueue calls into 3 distinct items.
    assert_eq!(
        queue_total, 12,
        "4 scans x (2 artists + 1 album) handed to the queue"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM artwork_queue"),
        4,
        "2 artists + 2 albums; PRIMARY KEY (item_kind, item_id, image_type) collapses the\n         repeated Enqueue calls"
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file_fts"), 4);
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// One fixture per folder: mp3/flac/ogg share one album id, so four files in one folder
/// would be a multi-track group, which SC-04 (not this lane) aggregates.
fn folder_for(fixture: &str) -> String {
    format!("album/{}", fixture.replace('.', "_"))
}

/// `replay_is_idempotent`: two scans of the same tree, second with a new stamp. Ids are
/// reused through the Match arm, nothing duplicates, transitions re-consume to nothing,
/// and the row/FTS/queue counts are exactly stable.
#[test]
fn replay_is_idempotent() {
    let tree = Tree::new("replay");
    tree.put("album/one", "known.mp3");
    let (store, db) = fresh_store("replay", 2);
    seed_library(&store, 1, "replay", &tree.root);
    let mapping = mapping();

    let first = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));
    let id_after_first: Vec<String> = strings(&store, "SELECT id FROM media_file ORDER BY id");
    let album_after_first: Vec<String> = strings(&store, "SELECT id FROM album ORDER BY id");
    let artist_after_first: Vec<String> = strings(&store, "SELECT id FROM artist ORDER BY id");
    let queue_after_first: Vec<String> = strings(
        &store,
        "SELECT item_kind || '|' || item_id FROM artwork_queue ORDER BY 1",
    );
    let tag_after_first: Vec<String> = strings(&store, "SELECT id FROM tag ORDER BY id");
    assert_eq!(id_after_first.len(), 1);
    assert_eq!(first.new_media_count, 1);

    let second = scan_ok(
        &store,
        &job(1, &tree.root, "album/one", later_stamp(), &mapping),
    );

    // Exact id sets: a minted duplicate fails here, not merely a count.
    assert_eq!(
        strings(&store, "SELECT id FROM media_file ORDER BY id"),
        id_after_first,
        "the Match arm reused the existing id (:181-189)"
    );
    assert_eq!(
        strings(&store, "SELECT id FROM album ORDER BY id"),
        album_after_first
    );
    assert_eq!(
        strings(&store, "SELECT id FROM artist ORDER BY id"),
        artist_after_first
    );
    assert_eq!(
        strings(&store, "SELECT id FROM tag ORDER BY id"),
        tag_after_first,
        "tag Add is `on conflict (id) do nothing`"
    );
    assert_eq!(
        strings(
            &store,
            "SELECT item_kind || '|' || item_id FROM artwork_queue ORDER BY 1"
        ),
        queue_after_first,
        "no duplicate queue items"
    );

    // Counts are stable across the replay.
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM album"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM artist"), 2);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM tag"), 3);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file_fts"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM artwork_queue"), 3);
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file_artists"),
        2,
        "the junction rows were replaced, not appended"
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file_tags"), 1);

    // The second pass re-imported the existing child and declared no transition.
    assert_eq!(second.new_media_count, 0);
    assert_eq!(
        second.reimported_paths,
        vec!["album/one/known.mp3".to_string()]
    );
    assert!(
        second.album_transitions.is_empty(),
        "the album id is unchanged, so nothing to reassign"
    );
    assert!(second.committed.consumed_transitions.is_empty());
    assert_eq!(
        second.folder_id, first.folder_id,
        "the folder id is path-derived"
    );
    assert_eq!(
        second.committed.inserted_media_ids,
        Vec::<String>::new(),
        "no INSERT leg ran on the second pass"
    );

    // created_at is the FIRST scan's stamp, not the replay's: Go's Put only stamps a
    // zero CreatedAt (:184-190), so a replayed row keeps its original creation time.
    let created = scalar(&store, "SELECT created_at FROM media_file");
    assert_eq!(created, Some(nd_db::time::to_text(&stamp())));
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// `album_transition_on_folder_path_change`: the PID spec changes between scans, so the
/// derived album id changes for the *same* file. That exercises both Go arms of
/// `entry.albumIDMap` — the stored child's `album_id` (`:311-312`) — and the persist
/// side: `ReassignAnnotation` moves the user's annotation and `CopyAttributes` carries
/// `created_at` to the new album (`:488-500`).
#[test]
fn album_transition_on_folder_path_change() {
    let tree = Tree::new("transition");
    tree.put("album/first", "known.mp3");
    let (store, db) = fresh_store("transition", 2);
    seed_library(&store, 1, "transition", &tree.root);
    let mapping = mapping();

    // Scan 1: Go's default album spec.
    let first = scan_ok(
        &store,
        &job(1, &tree.root, "album/first", stamp(), &mapping),
    );
    let prev_album = first.album_ids[0].clone();
    assert_eq!(prev_album, ALBUM_ID_NO_RELEASE_DATE);

    // A user annotation on the OLD album, so the reassignment leg has something to move.
    let user = "u-transition";
    let text = |value: &str| nd_db::sqlrepo::Value::Text(value.to_string());
    common::execute(
        &store,
        "INSERT INTO user (id, user_name, name, email, password, is_admin, created_at, updated_at) \
         VALUES (?1, ?1, 'U', 'u@example.invalid', 'p', 1, '2024-01-01 00:00:00', '2024-01-01 00:00:00')",
        &[text(user)],
    );
    common::execute(
        &store,
        "INSERT INTO annotation (user_id, item_id, item_type, play_count, rating, starred) \
         VALUES (?1, ?2, 'album', 3, 4, 1)",
        &[text(user), text(&prev_album)],
    );
    let prev_created = scalar(
        &store,
        &format!("SELECT created_at FROM album WHERE id = '{prev_album}'"),
    )
    .expect("previous album created_at");

    // Scan 2: same file, same folder, a DIFFERENT album PID spec — exactly Go's
    // `lib.ScannedPIDAlbum != pid.Album` case, which also sets `prevAlbumPIDConf`.
    let second = scan_ok(
        &store,
        &job_with_album_pid(
            1,
            &tree.root,
            "album/first",
            later_stamp(),
            &mapping,
            OVERRIDE_ALBUM_PID,
            Some(DEFAULT_ALBUM_PID),
        ),
    );

    let new_album = second.album_ids[0].clone();
    assert_ne!(
        new_album, prev_album,
        "the override spec derives a different album id for the same file"
    );

    // Go's arm at :311-312: the stored child's album_id is the previous one.
    assert_eq!(
        second.album_transitions,
        vec![nd_db::scanner_writes::AlbumTransition {
            new_id: new_album.clone(),
            prev_id: prev_album.clone(),
        }],
        "one transition, keyed by the NEW id"
    );
    assert_eq!(
        second.committed.consumed_transitions, second.album_transitions,
        "the mapping was consumed inside the transaction (:504)"
    );
    assert!(
        second
            .committed
            .warnings
            .iter()
            .all(|w| w.stage_str() != "reassign_annotation"),
        "the annotation reassignment succeeded, so no warning arm"
    );

    // The annotation moved to the new album and left the old one.
    assert_eq!(
        count(
            &store,
            &format!(
                "SELECT COUNT(*) FROM annotation WHERE item_id = '{new_album}' AND user_id = '{user}'"
            )
        ),
        1,
        "ReassignAnnotation moved the row (:490)"
    );
    assert_eq!(
        count(
            &store,
            &format!(
                "SELECT COUNT(*) FROM annotation WHERE item_id = '{prev_album}' AND user_id = '{user}'"
            )
        ),
        0,
        "the old album no longer holds it"
    );
    assert_eq!(
        count(
            &store,
            &format!(
                "SELECT rating FROM annotation WHERE item_id = '{new_album}' AND user_id = '{user}'"
            )
        ),
        4,
        "the moved row keeps its rating"
    );

    // CopyAttributes of created_at: the new album carries the FIRST scan's stamp.
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT created_at FROM album WHERE id = '{new_album}'")
        ),
        Some(prev_created.clone()),
        "created_at was copied from the previous album (:496-500)"
    );
    assert_ne!(
        prev_created,
        nd_db::time::to_text(&later_stamp()),
        "the copied value is the original, not the second scan's stamp"
    );

    // The media row is the SAME row (path unchanged) but now points at the new album.
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file"),
        1,
        "Match arm reused the existing id"
    );
    assert_eq!(
        second.reimported_paths,
        vec!["album/first/known.mp3".to_string()]
    );
    assert_eq!(
        scalar(&store, "SELECT album_id FROM media_file"),
        Some(new_album.clone()),
        "the track points at the newly derived album"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM album"),
        2,
        "both album rows exist (the old one is orphaned, not deleted — that is phase 2/3)"
    );
    assert_eq!(children(&store, &second.folder_id).len(), 1);
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// `album_move_to_new_folder`: the file is physically moved to another folder, so its
/// path changes. The old row stays (SC-06's finalizer owns flagging), a new row is
/// inserted with a new album id, and Go's stored-child arm cannot fire for the new row
/// because no child exists at the new path.
#[test]
fn album_move_to_new_folder() {
    let tree = Tree::new("move");
    tree.put("album/first", "known.mp3");
    let (store, db) = fresh_store("move", 2);
    seed_library(&store, 1, "move", &tree.root);
    let mapping = mapping();

    let first = scan_ok(
        &store,
        &job(1, &tree.root, "album/first", stamp(), &mapping),
    );
    std::fs::remove_dir_all(tree.root.join("album/first")).expect("clear old folder");
    tree.put("album/second", "known.m4a");

    let second = scan_ok(
        &store,
        &job(1, &tree.root, "album/second", later_stamp(), &mapping),
    );
    assert_eq!(second.new_media_count, 1, "a new path is a new child");
    assert_eq!(
        second.album_ids,
        vec![ALBUM_ID_WITH_RELEASE_DATE.to_string()]
    );
    assert!(
        second.album_transitions.is_empty(),
        "no stored child at the new path, so no transition (Go's :311-312 arm needs one)"
    );
    assert_eq!(children(&store, &second.folder_id).len(), 1);
    assert_eq!(
        children(&store, &first.folder_id).len(),
        1,
        "the stale row stays"
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file"), 2);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM album"), 2);
    assert_eq!(
        scalar(
            &store,
            "SELECT album_id FROM media_file WHERE path = 'album/second/known.m4a'"
        ),
        Some(ALBUM_ID_WITH_RELEASE_DATE.to_string())
    );
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// `missing_child_marked`: a fixture is deleted from the tree between runs, so the DB
/// child no longer has a file and the batch's missing leg flags it.
#[test]
fn missing_child_marked() {
    let tree = Tree::new("missing");
    tree.put("album/one", "known.mp3");
    tree.put("album/two", "known.m4a");
    let (store, db) = fresh_store("missing", 2);
    seed_library(&store, 1, "missing", &tree.root);
    let mapping = mapping();

    let first = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));
    let other = scan_ok(&store, &job(1, &tree.root, "album/two", stamp(), &mapping));
    assert_eq!(first.audio_count, 1);
    assert_eq!(children(&store, &first.folder_id).len(), 1);
    assert_eq!(
        children(&store, &other.folder_id).len(),
        1,
        "the sibling folder is scanned too"
    );

    std::fs::remove_file(tree.root.join("album/one/known.mp3")).expect("delete mp3");

    let second = scan_ok(
        &store,
        &job(1, &tree.root, "album/one", later_stamp(), &mapping),
    );
    assert_eq!(second.audio_count, 0, "no audio file remains in the folder");
    assert_eq!(second.missing_count, 1, "one child flagged");
    assert_eq!(second.committed.missing_rows_marked, 1);

    let flagged = strings(
        &store,
        "SELECT id FROM media_file WHERE missing = 1 ORDER BY id",
    );
    assert_eq!(flagged.len(), 1);
    let path = scalar(
        &store,
        &format!("SELECT path FROM media_file WHERE id = '{}'", flagged[0]),
    );
    assert_eq!(path, Some("album/one/known.mp3".to_string()));
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT id FROM media_file WHERE id = '{}'", flagged[0])
        ),
        Some(first.committed.media_ids[0].clone()),
        "the flagged row is the first scan's child, id and all"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file WHERE missing = 0"),
        1,
        "the sibling folder's row is untouched"
    );

    // Go also touches the missing tracks' albums (:450-456), and stamps the flag.
    assert_eq!(
        second.committed.touched_album_ids,
        vec![ALBUM_ID_NO_RELEASE_DATE.to_string()],
        "Touch ran for the missing track's album, and only that one"
    );
    let touched = scalar(
        &store,
        &format!("SELECT imported_at FROM album WHERE id = '{ALBUM_ID_NO_RELEASE_DATE}'"),
    );
    assert_eq!(
        touched,
        Some(nd_db::time::to_text(&later_stamp())),
        "Touch sets imported_at with this scan's stamp (album_repository.go:365)"
    );
    let media_updated = scalar(
        &store,
        &format!(
            "SELECT updated_at FROM media_file WHERE id = '{}'",
            flagged[0]
        ),
    );
    assert_eq!(
        media_updated,
        Some(nd_db::time::to_text(&later_stamp())),
        "MarkMissing stamps updated_at"
    );
    assert_eq!(
        children(&store, &first.folder_id).len(),
        1,
        "the row is flagged, not deleted"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file_fts"),
        2,
        "flagging keeps the row (and its FTS entry) alive"
    );
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// `born_unavailable_substitutes_scan_stamp`: the SC-03 P1 ruling inside the real
/// vertical. `localFS.noBirthTime` (`core/storage/local/local.go:129-145`) marks a
/// device that reports no creation time, and Go's `BirthTime()` then falls back to
/// `time.Now()` (`local.go:120-128`); the port substitutes the caller's injected scan
/// stamp instead. macOS/APFS *does* report birth times, so the job declares the device
/// birth-less rather than depending on the host filesystem.
#[test]
fn born_unavailable_substitutes_scan_stamp() {
    let tree = Tree::new("no-birth");
    tree.put("album/one", "known.mp3");
    let (store, db) = fresh_store("no-birth", 2);
    seed_library(&store, 1, "no-birth", &tree.root);
    let mapping = mapping();

    let outcome = scan_ok(
        &store,
        &job_without_birth(1, &tree.root, "album/one", stamp(), &mapping),
    );
    let kid = children(&store, &outcome.folder_id)
        .pop()
        .expect("one child");

    assert_eq!(
        kid.birth_time,
        stamp(),
        "media_file.birth_time is the scan stamp when storage reports no birth time"
    );
    assert_eq!(
        kid.updated_at,
        Some(file_stamp()),
        "the mtime is still the file's own, so only birth was substituted"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT birth_time FROM media_file WHERE id = '{}'", kid.id)
        ),
        Some(nd_db::time::to_text(&stamp()))
    );
    assert_eq!(
        scalar(
            &store,
            &format!(
                "SELECT created_at FROM album WHERE id = '{}'",
                outcome.album_ids[0]
            )
        ),
        Some(nd_db::time::to_text(&stamp())),
        "the album draft shares the SAME substituted instant (one resolution, SC-03)"
    );

    // A second scan with a new stamp does NOT overwrite the stored birth time: Go's
    // `selectUpdateColumns` drops `created_at` and `birth_time` from the UPDATE set
    // (`sql_base_repository.go:605,608`, ported at `mediafile_repo.rs:1388-1408`), so
    // the substitute is written once, on the INSERT leg only.
    let second = scan_ok(
        &store,
        &job_without_birth(1, &tree.root, "album/one", later_stamp(), &mapping),
    );
    let kid2 = children(&store, &second.folder_id)
        .pop()
        .expect("one child");
    assert_eq!(kid2.id, kid.id, "same row (Match arm)");
    assert_eq!(
        kid2.birth_time,
        stamp(),
        "birth_time is INSERT-only, so the first scan's substitute stands"
    );
    assert_eq!(
        kid2.created_at,
        Some(stamp()),
        "created_at is likewise INSERT-only"
    );
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// `symlinked_folder_ingests_with_logical_paths`: the folder itself is a symlink to a
/// directory outside nothing (still under the root). Go preserves the logical name
/// everywhere (`walk_dir_tree.go:276-290` returns the entry's own path, and
/// `localFS.Stat` follows the link for the record's facts while the folder hash uses the
/// link's own `lstat`), so the DB rows must carry the LINK's path and never the target's.
///
/// `cfg(unix)`: it creates the link with `std::os::unix::fs::symlink`, which has no
/// portable equivalent here (Windows links need elevation and a different API).
#[cfg(unix)]
#[test]
fn symlinked_folder_ingests_with_logical_paths() {
    let tree = Tree::new("symlink-folder");
    let real = tree.dir("album/real");
    common::copy_pinned(&real, "known.mp3", "known.mp3");
    std::os::unix::fs::symlink(tree.root.join("album/real"), tree.root.join("album/link"))
        .expect("symlink the folder");
    let (store, db) = fresh_store("symlink-folder", 2);
    seed_library(&store, 1, "symlink-folder", &tree.root);
    let mapping = mapping();

    let outcome = scan_ok(&store, &job(1, &tree.root, "album/link", stamp(), &mapping));

    assert_eq!(outcome.audio_count, 1);
    assert_eq!(outcome.new_media_count, 1);
    assert!(
        outcome.warnings.is_empty(),
        "a followed folder link is not a warning"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT path FROM folder WHERE id = '{}'", outcome.folder_id)
        ),
        Some("album".to_string()),
        "folder.path is the link's cleaned directory, not the target's"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT name FROM folder WHERE id = '{}'", outcome.folder_id)
        ),
        Some("link".to_string()),
        "folder.name is the link's own basename"
    );
    let kid = children(&store, &outcome.folder_id)
        .pop()
        .expect("one child");
    assert_eq!(
        kid.path, "album/link/known.mp3",
        "media_file.path keeps the logical link path"
    );
    assert_eq!(kid.size, 7833, "size comes through the link");
    assert_eq!(
        kid.updated_at,
        Some(file_stamp()),
        "the mtime comes from the followed endpoint"
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file_fts"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM artwork_queue"), 3);
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// SC-05 §4 "reopen DB with fresh connection, query using real executed repos": the
/// ingest's own `SQLStore` is DROPPED and a second `Pool` is opened over the same file
/// with no schema pass ([`reopen_store`]), so every read below runs on handles this test
/// never handed the writer. Reads go through the executed repository types
/// (`FolderRepo`/`AlbumRepo`/`ArtistRepo`/`MediaFileRepo`/`LibraryRepo`) and the same
/// frozen DB-11 visitor the ingest used, not through the store that wrote the rows.
///
/// The connection-policy half is per-lease rather than per-migration: each statement
/// reports the pragma of the handle actually serving it, twice concurrently, because the
/// plan's gate is "effective `foreign_keys`/journal/synchronous/busy on every lease"
/// (`scanner.md` "DB every-handle connection policy"), which a fresh-schema connection
/// alone cannot prove.
#[test]
fn reopens_with_fresh_pool_reads_back_through_executed_repos() {
    let tree = Tree::new("reopen");
    tree.put("album/one", "known.mp3");
    let (store, db) = fresh_store("reopen", 2);
    seed_library(&store, 1, "reopen", &tree.root);
    let mapping = mapping();

    let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));
    let folder_id = outcome.folder_id.clone();
    let media_id = outcome.committed.media_ids[0].clone();
    let artist_ids = strings(&store, "SELECT id FROM artist ORDER BY id");
    drop(store);

    let reopened = reopen_store(&db, 2);

    // Every handle this pool hands out carries the FK pragma the DDL's cascades need.
    let policy = pragma_in_lease(&reopened, |read| {
        vec![
            read("SELECT CAST(foreign_keys AS TEXT) FROM pragma_foreign_keys"),
            read("SELECT CAST(journal_mode AS TEXT) FROM pragma_journal_mode"),
            read("SELECT CAST(synchronous AS TEXT) FROM pragma_synchronous"),
        ]
    });
    assert_eq!(
        policy,
        vec!["1".to_string(), "delete".to_string(), "1".to_string()],
        "the reopened pool's first lease carries foreign_keys=1, delete journal, synchronous=1"
    );
    // A second lease, held CONCURRENTLY with the first, reports the same policy: a
    // migration-time pragma would say nothing about the later handles.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let holder_store = reopened.clone();
    let holder = std::thread::Builder::new()
        .name("sc05-reopen-holder".to_string())
        .spawn(move || {
            pragma_in_lease(&holder_store, |read| {
                let value = read("SELECT CAST(foreign_keys AS TEXT) FROM pragma_foreign_keys");
                ready_tx.send(()).expect("signal that a lease is held");
                std::thread::sleep(std::time::Duration::from_millis(250));
                value
            })
        })
        .expect("spawn lease holder");
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the holder really holds a lease");
    let concurrent = pragma_in_lease(&reopened, |read| {
        vec![
            read("SELECT CAST(foreign_keys AS TEXT) FROM pragma_foreign_keys"),
            read("SELECT CAST(timeout AS TEXT) FROM pragma_busy_timeout"),
        ]
    });
    assert_eq!(
        concurrent,
        vec!["1".to_string(), "5000".to_string()],
        "the second concurrent lease has its own FK pragma and the driver busy timeout"
    );
    assert!(
        matches!(holder.join(), Ok(value) if value == "1"),
        "the holder's own lease reported foreign_keys=1 and returned"
    );
    assert_eq!(
        reopened.idle_count().expect("idle count"),
        2,
        "both returned"
    );

    // Build evidence, per §9: `sqlite_version()` and the FTS compile options, read off
    // this DB's own connection rather than asserted from the toolchain.
    assert_eq!(
        pragma(
            &reopened,
            "SELECT CAST(COUNT(*) AS TEXT) FROM pragma_module_list WHERE name = 'fts5'"
        ),
        "1",
        "fts5 is a registered module on this connection"
    );
    let fts_compile = strings(
        &reopened,
        "SELECT compile_options FROM pragma_compile_options() \
         WHERE compile_options LIKE 'ENABLE_FTS%' ORDER BY 1",
    );
    assert!(
        fts_compile.iter().any(|o| o == "ENABLE_FTS5"),
        "the library is compiled with FTS5, got {fts_compile:?}"
    );
    println!(
        "reopened build evidence: sqlite_version = {}, fts compile = {fts_compile:?}",
        pragma(&reopened, "SELECT sqlite_version()")
    );

    // Executed repo reads, each asserting real values rather than just `is_ok`.
    let folder = reopened
        .with_connection_sync(|conn| {
            Ok(nd_db::folder_repo::FolderRepo::new(conn).get(&folder_id, None))
        })
        .expect("folder lease")
        .expect("FolderRepo::get reads the row the ingest wrote");
    assert_eq!(folder.name, "one");
    assert_eq!(folder.library_id, 1);
    assert_eq!(folder.num_audio_files, 1);
    assert_eq!(
        folder.hash, outcome.hash,
        "the fingerprint survived the reopen"
    );
    assert!(!folder.missing);

    let album = reopened
        .with_connection_sync(|conn| {
            Ok(nd_db::album_repo::AlbumRepo::new(conn).get(
                &outcome.album_ids[0],
                None,
                false,
                None,
            ))
        })
        .expect("album lease")
        .expect("AlbumRepo::get");
    assert_eq!(album.name, "Synthetic Album");
    assert_eq!(album.song_count, 1);
    assert_eq!(album.album_artist, "Album Artist");
    assert_eq!(album.library_id, 1);

    // Both artists, each through `ArtistRepo::get`, and the NAME SET is pinned: the ids
    // are minted by the DB, so only the credit names are stable to assert against.
    let mut artist_names = Vec::new();
    for id in &artist_ids {
        let artist = reopened
            .with_connection_sync(|conn| {
                Ok(nd_db::artist_repo::ArtistRepo::new(conn).get(id, &None, None))
            })
            .expect("artist lease")
            .unwrap_or_else(|e| panic!("ArtistRepo::get({id}) reads a minted artist: {e}"));
        artist_names.push(artist.name.clone());
    }
    artist_names.sort();
    assert_eq!(
        artist_names,
        vec!["Album Artist".to_string(), "Artist One".to_string()],
        "the two credits round-trip through the executed repo, not as anonymous rows"
    );
    assert_eq!(artist_ids.len(), 2, "two artists survived the reopen");

    let track = reopened
        .with_connection_sync(|conn| {
            Ok(nd_db::mediafile_repo::MediaFileRepo::new(conn).get(&media_id, None, None, false))
        })
        .expect("media lease")
        .expect("MediaFileRepo::get by the durable id the ingest returned");
    assert_eq!(track.title, "Real café");
    assert_eq!(track.path, "album/one/known.mp3");
    assert_eq!(track.album_id, outcome.album_ids[0]);
    assert_eq!(track.size, 7833);

    let library = reopened
        .with_connection_sync(|conn| Ok(nd_db::library_repo::LibraryRepo::new(conn).get(1)))
        .expect("library lease")
        .expect("the seeded library row is still readable");
    assert_eq!(library.name, "reopen");
    // SC-05 is phase1-only: the ingest must NOT stamp a library scan complete.
    assert_eq!(library.last_scan_at, stamp(), "no ScanEnd ran");

    // The frozen DB-11 visitor on the NEW pool sees the same single child.
    let kids = children(&reopened, &folder_id);
    assert_eq!(kids.len(), 1, "visitor read-back on the reopened pool");
    assert_eq!(
        kids[0].id, media_id,
        "the same durable id, not a re-minted one"
    );

    assert!(pool_healthy(&reopened));
    remove_db(&db);
}

/// The junction tables under their **actual schema names** — `album_artists`,
/// `album_tags`, `media_file_artists`, `media_file_tags`, `library_artist`,
/// `library_tag` (`nd-dbschema/src/schema_ddl.rs:61-74`). No `library_artists` exists;
/// inventing one would pass a count of nothing.
///
/// `album_tags` holds only names flagged `album: true` in the mapping table (Go
/// `AlbumLevelTags`, `model/album.go:104-122`), so the genre is there and
/// `disctotal`/`tracktotal` are not; `media_file_tags` holds only the indexed genre,
/// while the non-indexed names stay in the `tags` JSON cell.
#[test]
fn junction_rows_use_the_actual_schema_names() {
    let tree = Tree::new("junctions");
    tree.put("album/one", "known.mp3");
    let (store, db) = fresh_store("junctions", 2);
    seed_library(&store, 1, "junctions", &tree.root);
    let mapping = mapping();

    let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));
    let album = outcome.album_ids[0].clone();
    let track_id = outcome.committed.media_ids[0].clone();
    let genre_id = strings(&store, "SELECT id FROM tag WHERE tag_name = 'genre'")
        .pop()
        .expect("the genre dictionary row");

    // album_artists: one row per credit, with Go's role names.
    let album_artists = strings(
        &store,
        "SELECT album_id || '|' || artist_id || '|' || role FROM album_artists ORDER BY role",
    );
    assert_eq!(
        album_artists.len(),
        2,
        "albumartist + artist, got {album_artists:?}"
    );
    assert!(
        album_artists
            .iter()
            .all(|row| row.starts_with(&format!("{album}|"))),
        "every album_artists row belongs to the minted album: {album_artists:?}"
    );
    assert_eq!(album_artists[0].split('|').nth(2), Some("albumartist"));
    assert_eq!(album_artists[1].split('|').nth(2), Some("artist"));

    // media_file_artists: the same two credits, keyed by the durable media id.
    let media_artists = strings(
        &store,
        "SELECT media_file_id || '|' || artist_id || '|' || role FROM media_file_artists ORDER BY role",
    );
    assert_eq!(media_artists.len(), 2);
    assert!(
        media_artists
            .iter()
            .all(|row| row.starts_with(&format!("{track_id}|"))),
        "media_file_artists rows key the committed media id: {media_artists:?}"
    );

    // album_tags: album-level tags only.
    let album_tags = strings(
        &store,
        "SELECT album_id || '|' || tag_id FROM album_tags ORDER BY 1",
    );
    assert_eq!(
        album_tags,
        vec![format!("{album}|{genre_id}")],
        "the album's only album-level tag is the genre"
    );

    // media_file_tags: indexed genre only; the other names stay in the JSON cell.
    let media_tags = strings(
        &store,
        "SELECT media_file_id || '|' || tag_id FROM media_file_tags ORDER BY 1",
    );
    assert_eq!(
        media_tags,
        vec![format!("{track_id}|{genre_id}")],
        "exactly the indexed genre junction row"
    );
    let tags_cell = scalar(
        &store,
        &format!("SELECT tags FROM media_file WHERE id = '{track_id}'"),
    )
    .expect("tags cell");
    for name in ["disctotal", "tracktotal"] {
        assert!(
            tags_cell.contains(name),
            "{name} stays in the tags JSON, not in media_file_tags"
        );
    }

    // library_artist links both artists; library_tag links all three dictionary rows.
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM library_artist WHERE library_id = 1 AND artist_id IN \
             (SELECT id FROM artist)"
        ),
        2,
        "AddArtist over the actual library_artist table"
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM library_tag WHERE library_id = 1 AND tag_id IN (SELECT id FROM tag)"
        ),
        3,
        "tag Add's library_tag leg over the actual table"
    );

    // Every junction row points at a row that exists: no orphan by construction.
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM album_artists WHERE album_id NOT IN (SELECT id FROM album)"
        ),
        0
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM album_artists WHERE artist_id NOT IN (SELECT id FROM artist)"
        ),
        0
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM album_tags WHERE tag_id NOT IN (SELECT id FROM tag)"
        ),
        0
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file_artists WHERE media_file_id NOT IN (SELECT id FROM media_file)"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file_tags WHERE media_file_id NOT IN (SELECT id FROM media_file)"), 0);
    assert!(
        strings(&store, "PRAGMA foreign_key_check").is_empty(),
        "no FK violations"
    );
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// `phase1_fts_tokens_follow_commit_and_update`: MATCH is joined back through
/// `rowid → entity id`, so a token that only exists in the FTS shadow table (a ghost)
/// cannot satisfy the assertion, and neither can a token still indexed under an OLD
/// value. Both a track field (title) and an album field (name) are updated in place —
/// same entity id — so the `*_fts_au` trigger's delete-then-insert pair is what is
/// being proved.
///
/// The album id is pinned with an explicit album PID spec (`albumartist`, so the title
/// and artist changes cannot move it) exactly so: an id change would make this an album
/// transition rather than an in-place FTS update.
#[test]
fn fts_tokens_follow_commit_and_update() {
    let tree = Tree::new("fts-update");
    let dir = tree.dir("album/one");
    let path = common::copy_pinned(&dir, "known.mp3", "known.mp3");
    let (store, db) = fresh_store("fts-update", 2);
    seed_library(&store, 1, "fts-update", &tree.root);
    let mapping = mapping();
    let album_spec = "albumartist";

    let first = scan_ok(
        &store,
        &job_with_album_pid(
            1,
            &tree.root,
            "album/one",
            stamp(),
            &mapping,
            album_spec,
            None,
        ),
    );
    let album = first.album_ids[0].clone();
    let track_id = first.committed.media_ids[0].clone();

    let search = |table: &str, token: &str| -> Vec<String> {
        strings(
            &store,
            &format!(
                "SELECT id FROM {table} WHERE rowid IN \
                 (SELECT rowid FROM {table}_fts WHERE {table}_fts MATCH '{token}')"
            ),
        )
    };
    assert_eq!(
        search("media_file", "Real"),
        vec![track_id.clone()],
        "title token indexed on insert"
    );
    assert_eq!(
        search("album", "Synthetic"),
        vec![album.clone()],
        "album name indexed on insert"
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM media_file_fts WHERE rowid NOT IN (SELECT rowid FROM media_file)"
        ),
        0
    );

    // Flip one byte of the TIT2 value and one of the TALB value: the OLD tokens must
    // stop matching and the NEW ones must, through the same entity ids.
    let flip = |frame: &[u8; 4], byte: u8| {
        let mut bytes = std::fs::read(&path).expect("read fixture");
        let pos = bytes
            .windows(4)
            .position(|w| w == frame)
            .expect("ID3v2 frame present");
        bytes[pos + 11] = byte;
        std::fs::write(&path, bytes).expect("write edited fixture");
    };
    flip(b"TIT2", b'X');
    flip(b"TALB", b'Q');

    let second = scan_ok(
        &store,
        &job_with_album_pid(
            1,
            &tree.root,
            "album/one",
            later_stamp(),
            &mapping,
            album_spec,
            None,
        ),
    );
    assert_eq!(
        second.album_ids,
        vec![album.clone()],
        "the pinned spec keeps the SAME album id, so this is an in-place update"
    );
    assert_eq!(
        second.committed.media_ids,
        vec![track_id.clone()],
        "the SAME media id, updated by Match"
    );
    assert_eq!(second.new_media_count, 0);

    assert!(
        search("media_file", "Real").is_empty(),
        "old title token no longer searchable"
    );
    assert_eq!(
        search("media_file", "Xeal"),
        vec![track_id.clone()],
        "new title token searchable"
    );
    assert!(
        search("album", "Synthetic").is_empty(),
        "old album token no longer searchable"
    );
    assert_eq!(
        search("album", "Qynthetic"),
        vec![album.clone()],
        "new album token searchable"
    );
    // An unchanged field keeps matching, so the update leg did not blank the row.
    assert_eq!(search("album", "Album"), vec![album.clone()]);

    // One indexed row per entity and no ghosts in any of the three FTS tables.
    for table in ["media_file", "album", "artist"] {
        assert_eq!(
            count(&store, &format!("SELECT COUNT(*) FROM {table}_fts")),
            count(&store, &format!("SELECT COUNT(*) FROM {table}")),
            "{table}_fts holds exactly one row per {table}"
        );
        assert_eq!(
            count(
                &store,
                &format!("SELECT COUNT(*) FROM {table}_fts WHERE rowid NOT IN (SELECT rowid FROM {table})")
            ),
            0,
            "no {table}_fts ghost row"
        );
    }
    assert!(strings(&store, "PRAGMA foreign_key_check").is_empty());
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// `foreign_key_check` on a committed vertical, plus the honest scope note: the schema
/// gives `folder.parent_id` NO foreign key (`schema_ddl.rs:61` declares only
/// `library_id REFERENCES library(id)`), so `PRAGMA foreign_key_check` structurally
/// cannot report a dangling ancestor. The assertion below pins that fact rather than
/// implying the pragma covers `parent_id`.
#[test]
fn foreign_key_check_is_clean_and_parent_id_has_no_fk() {
    let tree = Tree::new("fk-check");
    tree.put("album/one", "known.mp3");
    let (store, db) = fresh_store("fk-check", 2);
    seed_library(&store, 1, "fk-check", &tree.root);
    let mapping = mapping();

    let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));
    assert!(
        outcome.folder_inserted,
        "a folder row was inserted, so the check has a subject"
    );
    assert!(
        strings(&store, "PRAGMA foreign_key_check").is_empty(),
        "no FK violation after a committed vertical"
    );
    // Every FK the schema actually declares on `folder`.
    let folder_fks = strings(
        &store,
        "SELECT CAST(\"from\" AS TEXT) || '|' || CAST(\"table\" AS TEXT) \
         FROM pragma_foreign_key_list('folder') ORDER BY 1",
    );
    assert_eq!(
        folder_fks,
        vec!["library_id|library".to_string()],
        "folder declares exactly one FK"
    );
    assert!(
        !folder_fks.iter().any(|row| row.starts_with("parent_id|")),
        "parent_id is a plain varchar with no FK: a dangling ancestor is invisible to \
         foreign_key_check (declared limitation, not a passing assertion about it)"
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM media_file WHERE library_id NOT IN (SELECT id FROM library)"
        ),
        0
    );
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// The artwork DELETE leg of Go's persistence (`phase_1_folders.go:436-441`, ported as
/// `nd_db::scanner_writes::delete_for_items_in_tx`) is observable on this lane: a
/// `mf`-kind `item_artwork` row is seeded for the committed track, the folder is
/// rescanned (a re-import), and the leg reports and performs the deletion. The queue
/// leg is unchanged, so this asserts DELETE and not an accidental re-enqueue.
#[test]
fn track_artwork_is_invalidated_on_reimport() {
    let tree = Tree::new("artwork-delete");
    tree.put("album/one", "known.mp3");
    let (store, db) = fresh_store("artwork-delete", 2);
    seed_library(&store, 1, "artwork-delete", &tree.root);
    let mapping = mapping();

    let first = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));
    let track_id = first.committed.media_ids[0].clone();
    let text = |value: &str| nd_db::sqlrepo::Value::Text(value.to_string());
    common::execute(
        &store,
        "INSERT INTO item_artwork (item_kind, item_id, image_type, hash, source) \
         VALUES ('mf', ?1, 'primary', 'seeded-hash', 'cover')",
        &[text(&track_id)],
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM item_artwork WHERE item_kind = 'mf'"
        ),
        1,
        "a pre-existing artwork row for the track, so the delete leg has work to do"
    );
    let queue_before = count(&store, "SELECT COUNT(*) FROM artwork_queue");

    let second = scan_ok(
        &store,
        &job(1, &tree.root, "album/one", later_stamp(), &mapping),
    );
    assert_eq!(
        second.committed.artwork_rows_deleted, 1,
        "the delete leg reported one invalidated track artwork row (:436-441)"
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM item_artwork WHERE item_kind = 'mf'"
        ),
        0,
        "the row is really gone"
    );
    assert!(
        second.committed.warnings.is_empty(),
        "the artwork arm committed rather than degrading: {:?}",
        second
            .committed
            .warnings
            .iter()
            .map(|w| w.stage_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM artwork_queue"),
        queue_before,
        "EnqueueIfMissing re-added nothing on the replay"
    );
    assert_eq!(second.committed.media_ids, vec![track_id]);
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// `pool_healthy` has to be able to say `false`, or every `assert!(pool_healthy(..))`
/// in both files is decoration. A size-1 pool whose only handle is held by a parked
/// thread makes the helper's own lease block until the holder returns: `idle_count` is
/// still `Ok(0)` (the mutex is untouched), so the queue-mutex check alone would pass.
/// The holder releases after the probe starts, so the helper ends up `true` and the
/// pool is provably recoverable — the test proves the helper observes exhaustion rather
/// than returning a constant.
#[test]
fn pool_health_probe_reports_exhaustion_not_just_mutex_state() {
    let tree = Tree::new("pool-probe");
    tree.put("album/one", "known.mp3");
    let (store, db) = fresh_store("pool-probe", 1);
    seed_library(&store, 1, "pool-probe", &tree.root);

    let (hold_tx, hold_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder_store = store.clone();
    let holder = std::thread::Builder::new()
        .name("sc05-pool-holder".to_string())
        .spawn(move || {
            store_holds_one(&holder_store, hold_tx, release_rx);
        })
        .expect("spawn pool holder");
    hold_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the holder owns the only handle");
    assert_eq!(
        store.idle_count().expect("idle count is still readable"),
        0,
        "the queue reports zero idle handles — which a mutex-only check would call healthy"
    );

    // The probe itself, run while NO handle is idle. If `pool_healthy` only consulted
    // the queue's mutex it would return instantly; because it actually leases a
    // connection it must block until the holder returns one. The elapsed time is the
    // control that makes every `assert!(pool_healthy(..))` in these files meaningful.
    let probe_store = store.clone();
    let probe = std::thread::Builder::new()
        .name("sc05-pool-probe".to_string())
        .spawn(move || {
            let started = std::time::Instant::now();
            let healthy = pool_healthy(&probe_store);
            (healthy, started.elapsed())
        })
        .expect("spawn the health probe");
    std::thread::sleep(std::time::Duration::from_millis(150));
    release_tx.send(()).expect("release the holder");
    holder.join().expect("the holder returns its lease");
    let (healthy, waited) = probe.join().expect("the probe returns");
    assert!(healthy, "the probe ends healthy once a handle exists again");
    assert!(
        waited >= std::time::Duration::from_millis(100),
        "the probe really leased a connection: it blocked on the exhausted pool for          {waited:?} instead of reporting the mutex"
    );
    assert_eq!(
        store.idle_count().expect("idle count"),
        1,
        "the handle came back"
    );
    assert!(pool_healthy(&store), "the same pool is usable again");
    remove_db(&db);
}

/// Hold the pool's only lease, signal, wait for release, then return it. Kept out of
/// the closure so the lease's lifetime is obvious.
fn store_holds_one(
    store: &nd_db::store::SQLStore,
    hold_tx: std::sync::mpsc::Sender<()>,
    release_rx: std::sync::mpsc::Receiver<()>,
) {
    store
        .with_connection_sync(|conn| {
            let value = conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0));
            hold_tx.send(()).expect("signal that the lease is held");
            release_rx
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("release signal");
            Ok(value.unwrap_or(0))
        })
        .expect("the holder's statement runs");
}

/// The unreported-directory-mtime fallback is Go's zero `time.Time`, not the Unix epoch.
/// [`crate`]`::folder::FolderSnapshot` is public, so a snapshot whose `dir_stat` reports
/// no mtime is built directly here: the fingerprint preimage must render Go's
/// `0001-01-01 00:00:00 +0000 UTC` and must NOT contain `1970-01-01`, which is what a
/// silent epoch fallback would hash instead of Go's value.
#[test]
fn unreported_directory_mtime_fingerprints_go_zero_not_the_epoch() {
    use nd_scanner::folder::{FolderCoverage, FolderSnapshot};
    use nd_scanner::storage::{BirthTimeUnavailable, FileStat, LogicalPath, StatUnavailable};

    let snapshot = FolderSnapshot {
        library_id: 1,
        path: LogicalPath::parse("album/x").expect("logical path"),
        dir_stat: FileStat {
            size: 64,
            modified: Err(StatUnavailable),
            created: Err(BirthTimeUnavailable),
        },
        // The instant phase1 substitutes when `dir_stat.modified` is unavailable:
        // Go's zero `time.Time` as a `SystemTime` — 62_135_596_800s before the epoch.
        mod_time: std::time::UNIX_EPOCH - std::time::Duration::from_secs(62_135_596_800),
        images_updated_at: None,
        num_sub_folders: 0,
        entries: Vec::new(),
        coverage: FolderCoverage::Complete,
        warnings: Vec::new(),
    };
    let preimage = snapshot.hash_preimage().expect("preimage renders");
    let text = String::from_utf8(preimage).expect("preimage is UTF-8");
    assert!(
        text.starts_with("0001-01-01 00:00:00 +0000 UTC"),
        "the modTime slot renders Go's zero time, got {text:?}"
    );
    assert!(
        !text.contains("1970-01-01"),
        "no Unix-epoch timestamp leaked into the fingerprint: {text:?}"
    );
    assert_eq!(
        snapshot.hash().expect("hash"),
        "c9e122666846c6107bcede3a959aa808",
        "the whole preimage hashes to the pinned Go-shaped value"
    );

    // Non-vacuity: the SAME snapshot with a Unix-epoch `mod_time` — what a silent
    // fallback would produce — must hash DIFFERENTLY. Without this half, pinning one
    // hash cannot tell Go's zero time from any other instant.
    let epoch_fallback = FolderSnapshot {
        mod_time: std::time::UNIX_EPOCH,
        ..snapshot.clone()
    };
    let epoch_text = String::from_utf8(epoch_fallback.hash_preimage().expect("epoch preimage"))
        .expect("epoch preimage is UTF-8");
    assert!(
        epoch_text.starts_with("1970-01-01 00:00:00 +0000 UTC"),
        "the epoch variant renders the wrong spelling, which is exactly the bug: {epoch_text:?}"
    );
    assert_ne!(
        epoch_fallback.hash().expect("epoch hash"),
        snapshot.hash().expect("go-zero hash"),
        "a Unix-epoch fallback changes the fingerprint, so the pinned hash really pins the fallback"
    );
}

/// An unreadable subdirectory must still contribute its own `lstat` mtime to the parent
/// folder's fingerprint, exactly as Go and the SC-02 walker do.
///
/// Go's `loadDir` (`scanner/walk_dir_tree.go:156-163`) reads
/// `if isDir && isDirReadable(...) { children; numSubFolders++ } else { fileInfo :=
/// entry.Info(); if fileInfo.ModTime().After(folder.modTime) { folder.modTime = ... } … }`.
/// The `else` arm is entered by a directory that cannot be **opened**, so its mtime raises
/// `folder.modTime` before any early `continue` decides the entry's fate. The SC-02 walker
/// ports exactly that (`walk.rs`: the `EnterOutcome::Skipped` arm falls through to
/// `accumulate_entry`, which raises `frame.mod_time`). If the ingest skipped the
/// accumulation instead, the same tree would fingerprint differently between the walker
/// that drives SC-06's change detection and this ingest — the divergence this test exists
/// to prevent.
///
/// Root/sudo can open a mode-`000` directory, so the test verifies unreadability FIRST and
/// skips with an explicit reason when the platform still allows it. Nothing is faked: the
/// warning is asserted to come from the real `EACCES` the storage layer reports.
#[cfg(unix)]
#[tokio::test]
async fn unreadable_subdirectory_contributes_its_mtime_and_matches_the_walker_hash() {
    use std::os::unix::fs::PermissionsExt;

    use common::later_file_time;
    use nd_scanner::contract::{LibraryRoot, ScanWarningKind, WalkRequest, WalkTarget};
    use nd_scanner::storage::LogicalPath;
    use nd_scanner::walk::WalkHandle;

    let tree = Tree::new("unreadable-subdir");
    let folder = tree.dir("album/one");
    common::copy_pinned(&folder, "known.mp3", "known.mp3");
    let blocked = tree.dir("album/one/blocked");
    // A real file inside: the point is that its contents are never seen, not that the
    // directory is empty.
    common::copy_pinned(&blocked, "known.flac", "hidden.flac");

    // Deterministic timestamps: the parent directory and the audio file both carry
    // `file_time`, and the blocked directory carries a strictly LATER mtime. So if the
    // ingest folds it in, `mod_time` is exactly the blocked directory's stamp; if it does
    // not, `mod_time` stays at `file_time`. A tie would prove nothing.
    common::set_dir_time(&folder, common::file_time());
    common::set_dir_time(&blocked, later_file_time());

    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000))
        .expect("chmod 000 the subdirectory");

    if std::fs::read_dir(&blocked).is_ok() {
        // Self-skip with an explicit reason: running the assertions here would prove
        // nothing about the unreadable arm, because the directory is openable.
        eprintln!(
            "SKIPPED unreadable_subdirectory_contributes_its_mtime_and_matches_the_walker_hash: \
             this user can open a mode-000 directory (root/sudo, or a platform without \
             POSIX permission enforcement), so the DirectoryUnreadable arm is unreachable. \
             The arm is covered on an unprivileged account, and the accumulation itself is \
             pinned by the SC-02 walker's own unreadable-directory test."
        );
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o755))
            .expect("restore permissions before cleanup");
        return;
    }

    let (store, db) = fresh_store("unreadable-subdir", 2);
    seed_library(&store, 1, "unreadable-subdir", &tree.root);
    let mapping = mapping();

    let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));

    // Independently derived oracle: the committed SC-02 walker over the SAME tree, run
    // WHILE the directory is still unreadable. It accumulates an unopenable directory's
    // mtime by construction (`EnterOutcome::Skipped` falls through to `accumulate_entry`),
    // so agreeing with it is agreeing with Go's `loadDir` ordering rather than with this
    // ingest's own choice. Running it AFTER the restore would walk a different tree: the
    // walker would descend into `blocked`, count it as a subfolder, and never `Info()` it.
    let request = WalkRequest {
        libraries: vec![LibraryRoot {
            id: 1,
            name: "unreadable-subdir".to_string(),
            root: tree.root.as_os_str().to_os_string(),
        }],
        targets: vec![WalkTarget {
            library_id: 1,
            path: LogicalPath::parse("album/one").expect("logical path"),
        }],
        follow_symlinks: true,
        ignore_dot_folders: true,
    };
    let mut handle = WalkHandle::start(request, nd_scanner::contract::CancelToken::new());
    let mut events = Vec::new();
    while let Some(event) = handle.next().await {
        events.push(event);
    }
    let summary = handle.terminal().await;

    // Restore before anything else touches the tree: a failing assertion below must not
    // leave `blocked` unremovable by the tree's Drop, and the walk above already ran.
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o755))
        .expect("restore permissions before cleanup");

    let unreadable: Vec<&nd_scanner::contract::ScanWarning> = outcome
        .warnings
        .iter()
        .filter(|warning| warning.kind == ScanWarningKind::DirectoryUnreadable)
        .collect();
    assert_eq!(
        unreadable.len(),
        1,
        "the real EACCES is reported once, got {:?}",
        outcome
            .warnings
            .iter()
            .map(|w| (w.kind, w.path.as_ref().map(|p| p.to_string())))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        unreadable[0].path.as_ref().map(|p| p.to_string()),
        Some("album/one/blocked".to_string()),
        "the warning names the unreadable directory"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT hash FROM folder WHERE id = '{}'", outcome.folder_id)
        ),
        Some(outcome.hash.clone()),
        "the fingerprint written to the DB is the walk's fingerprint"
    );

    assert!(
        summary.outcome.is_complete(),
        "the walker's own run must succeed: {}",
        summary.outcome.describe()
    );
    let walked = events
        .iter()
        .find(|event| event.snapshot.path.to_string() == "album/one")
        .expect("the walk emits the target folder");

    // The headline parity claim: same tree, two code paths, one fingerprint.
    assert_eq!(
        outcome.hash, walked.hash,
        "ingest and SC-02 walker fingerprint the same tree identically"
    );
    assert_eq!(
        walked.snapshot.mod_time,
        blocked
            .metadata()
            .expect("stat blocked")
            .modified()
            .expect("blocked mtime"),
        "non-vacuity: the blocked directory's LATER mtime is what raised the folder mtime"
    );
    assert_eq!(
        walked.snapshot.num_sub_folders, 0,
        "an unreadable directory is not counted as a subfolder"
    );
    assert_eq!(
        walked.snapshot.num_audio_files(),
        1,
        "the file inside the unreadable directory is never seen"
    );
    assert_eq!(
        summary
            .warnings_of(ScanWarningKind::DirectoryUnreadable)
            .len(),
        1,
        "the walker reports the same unreadable directory the ingest reported"
    );

    // The warning KIND is shared, not merely present: the capture routes a failed child listing
    // through the walker's own mapping (`walk::listing_failure_warning`), so an EACCES child is
    // `DirectoryUnreadable` on both paths and no other listing class is invented for it. A
    // capture that collapsed every listing failure into one arm — or picked the enumeration or
    // symlink class here — fails this.
    let kinds_of = |warnings: &[nd_scanner::contract::ScanWarning]| {
        warnings
            .iter()
            .map(|w| (w.kind, w.path.as_ref().map(|p| p.to_string())))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        kinds_of(&outcome.warnings),
        kinds_of(&summary.warnings)
            .into_iter()
            .filter(|(_, path)| path.as_deref() == Some("album/one/blocked"))
            .collect::<Vec<_>>(),
        "same kind, same path, for the same failed child listing: ingest {:?} vs walker {:?}",
        kinds_of(&outcome.warnings),
        kinds_of(&summary.warnings)
    );
    for kind in [
        ScanWarningKind::DirectoryEnumerationFailed,
        ScanWarningKind::SymlinkedTargetSkipped,
        ScanWarningKind::DepthLimitReached,
        ScanWarningKind::DirectoryCycleSkipped,
    ] {
        assert!(
            outcome.warnings.iter().all(|w| w.kind != kind),
            "a permission refusal is not reported as {kind:?}"
        );
    }
    assert_eq!(
        outcome.audio_count, 1,
        "the ingest saw exactly the one audio file"
    );
    assert!(
        walked.snapshot.mod_time > common::file_time(),
        "the folder mtime is strictly above the pinned file/dir stamp, so it came from the \
         unreadable directory: a pre-fix ingest left it at file_time and hashed differently"
    );
    assert!(
        outcome.hash != {
            // The pre-fix fingerprint, recomputed: the same folder with `mod_time` left at
            // `file_time` (only the audio file's mtime folded in) must hash DIFFERENTLY.
            // Without this half, hash agreement could be satisfied by either behavior.
            let mut oracle = walked.snapshot.clone();
            oracle.mod_time = common::file_time();
            oracle.hash().expect("pre-fix-shaped hash")
        },
        "non-vacuity: dropping the unreadable directory's mtime changes the fingerprint"
    );

    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// A **broken** followed symlink must be skipped exactly where Go's `loadDir` skips it —
/// before `entry.Info()` raises `folder.modTime` — so its link mtime never enters the
/// fingerprint.
///
/// Go's order (`scanner/walk_dir_tree.go:143-163`) is: `isDirOrSymlinkToDir` (`:216-233`)
/// stats a followed link's endpoint and returns an ERROR for a dangling one; `loadDir`
/// logs `Scanner: Invalid symlink` and `continue`s (`:144-150`). The `entry.Info()` call
/// that folds the entry's `lstat` mtime into `folder.modTime` (`:157-163`) therefore never
/// runs for that entry. The SC-02 walker ports it (`walk.rs` `accumulate_entry` returns for
/// a broken followed link before touching `frame.mod_time`). An ingest that folds the
/// mtime first and skips afterwards fingerprints the same tree at a LATER `modTime` than
/// the walk that drives SC-06's change detection — a permanent hash mismatch on every
/// folder containing a dangling link.
///
/// The link's own mtime is deliberately LATER than the media's: the link is created after
/// the pinned fixture and the directory's mtime is then set back to `file_time`, so the
/// pre-fix behaviour (fold the link mtime) and the post-fix behaviour (skip it) produce
/// provably different hashes rather than coincidentally equal ones. With following
/// disabled Go does NOT skip the entry — `isDirOrSymlinkToDir` returns `(false, nil)` and
/// the mtime is folded — which is why the ingest's skip is gated on `follow_symlinks`.
///
/// `cfg(unix)`: `std::os::unix::fs::symlink` has no portable equivalent here.
#[cfg(unix)]
#[tokio::test]
async fn broken_followed_symlink_is_skipped_before_its_mtime_and_matches_the_walker_hash() {
    use common::assert_stamp_ordering;
    use nd_scanner::contract::{LibraryRoot, ScanWarningKind, WalkRequest, WalkTarget};
    use nd_scanner::storage::LogicalPath;
    use nd_scanner::walk::WalkHandle;

    assert_stamp_ordering();

    let tree = Tree::new("broken-link");
    let folder = tree.dir("album/one");
    common::copy_pinned(&folder, "known.mp3", "known.mp3");

    // A dangling link with an audio-looking name: classification would follow it, so it
    // reaches the walker/ingest as a symlink entry whose endpoint cannot be stat'd.
    std::os::unix::fs::symlink(
        tree.root.join("nowhere/gone.mp3"),
        folder.join("dangling.mp3"),
    )
    .expect("symlink to a nonexistent target");

    // Timestamp ordering that makes the test non-vacuous: the media file and the folder
    // directory both carry `file_time`, and the link — created afterwards — carries the
    // wall clock, which is strictly later. Resetting the folder's own mtime AFTER creating
    // the link is what keeps the link mtime above the directory's, so the entry's mtime is
    // the only thing that could push `mod_time` above `file_time`.
    common::set_dir_time(&folder, common::file_time());
    let link_time = std::fs::symlink_metadata(folder.join("dangling.mp3"))
        .expect("lstat the link")
        .modified()
        .expect("link mtime");
    assert!(
        link_time > common::file_time(),
        "the link's own mtime must be later than the pinned media/dir stamp: {link_time:?} vs {:?}",
        common::file_time()
    );

    let (store, db) = fresh_store("broken-link", 2);
    seed_library(&store, 1, "broken-link", &tree.root);
    let mapping = mapping();

    let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));

    let skipped: Vec<&nd_scanner::contract::ScanWarning> = outcome
        .warnings
        .iter()
        .filter(|warning| warning.kind == ScanWarningKind::SymlinkSkipped)
        .collect();
    assert_eq!(
        skipped.len(),
        1,
        "the dangling link is reported once, got {:?}",
        outcome
            .warnings
            .iter()
            .map(|w| (w.kind, w.path.as_ref().map(|p| p.to_string())))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        skipped[0].path.as_ref().map(|p| p.to_string()),
        Some("album/one/dangling.mp3".to_string()),
        "the warning names the link, not its nonexistent target"
    );
    assert_eq!(
        outcome.audio_count, 1,
        "the dangling link is not counted as an audio file"
    );
    assert_eq!(
        outcome.tag_failures,
        Vec::new(),
        "a skipped entry never reaches the tag read, so no import abort"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT hash FROM folder WHERE id = '{}'", outcome.folder_id)
        ),
        Some(outcome.hash.clone()),
        "the fingerprint written to the DB is the walk's fingerprint"
    );

    // Independent oracle: the committed SC-02 walker over the SAME tree. It skips a broken
    // followed link before accumulating its mtime by construction, so agreeing with it is
    // agreeing with Go's `loadDir` ordering rather than with this ingest's own choice.
    let request = WalkRequest {
        libraries: vec![LibraryRoot {
            id: 1,
            name: "broken-link".to_string(),
            root: tree.root.as_os_str().to_os_string(),
        }],
        targets: vec![WalkTarget {
            library_id: 1,
            path: LogicalPath::parse("album/one").expect("logical path"),
        }],
        follow_symlinks: true,
        ignore_dot_folders: true,
    };
    let mut handle = WalkHandle::start(request, nd_scanner::contract::CancelToken::new());
    let mut events = Vec::new();
    while let Some(event) = handle.next().await {
        events.push(event);
    }
    let summary = handle.terminal().await;

    assert!(
        summary.outcome.is_complete(),
        "the walker's own run must succeed: {}",
        summary.outcome.describe()
    );
    let walked = events
        .iter()
        .find(|event| event.snapshot.path.to_string() == "album/one")
        .expect("the walk emits the target folder");

    // Headline parity claim: same tree, two code paths, one fingerprint.
    assert_eq!(
        outcome.hash, walked.hash,
        "ingest and SC-02 walker fingerprint a tree with a dangling link identically"
    );
    assert_eq!(
        walked.snapshot.mod_time,
        common::file_time(),
        "non-vacuity: the folder mtime stayed at the media/dir stamp, so the link's later \
         mtime was NOT folded in"
    );
    assert!(
        walked
            .snapshot
            .entries
            .iter()
            .all(|entry| entry.name != "dangling.mp3"),
        "the dangling link contributes no hash line"
    );
    assert_eq!(
        walked.snapshot.num_audio_files(),
        1,
        "only the real audio file is counted"
    );
    assert_eq!(
        summary.warnings_of(ScanWarningKind::SymlinkSkipped).len(),
        1,
        "the walker reports the same dangling link the ingest reported"
    );

    assert!(
        outcome.hash != {
            // The pre-fix fingerprint recomputed: folding the link's LATER mtime into
            // `mod_time` must change the hash. Without this half, agreement between the
            // two paths could be satisfied by either ordering.
            let mut oracle = walked.snapshot.clone();
            oracle.mod_time = link_time;
            oracle.hash().expect("pre-fix-shaped hash")
        },
        "non-vacuity: folding the broken link's mtime changes the fingerprint, so the \
         pinned hash really pins the skip-before-Info() ordering"
    );

    // Following DISABLED is the other arm: Go counts the symlink as a file-shaped entry and
    // folds its mtime, so the ingest must NOT skip it the same way.
    let disabled = scan_ok(
        &store,
        &job_without_symlinks(1, &tree.root, "album/one", later_stamp(), &mapping),
    );
    assert!(
        disabled.hash != outcome.hash,
        "with following disabled the link's mtime IS folded in, so the fingerprint differs \
         from the followed-link scan"
    );

    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// An **unreadable directory with an audio- or image-looking name** must be classified,
/// not just warned about: Go's `loadDir` falls through the failed readability check into
/// the same file branch as any other entry (`scanner/walk_dir_tree.go:156-176`).
///
/// `if isDir && isDirReadable(...) { children; numSubFolders++ } else { Info(); mtime;
/// resolveEntryName(); switch { IsAudioFile -> audioFiles; IsValidPlaylist -> playlistFiles;
/// IsImageFile -> imageFiles + imagesUpdatedAt } }`. A directory named `songs.mp3` therefore
/// lands in `folder.audioFiles` and one named `cover.jpg` in `folder.imageFiles`, with the
/// directory's own `lstat` size and mtime as hash facts and `numSubFolders` NOT incremented.
/// `toFolder` (`folder_entry.go:94-104`) then persists exactly those: `num_audio_files`,
/// `image_files`, `images_updated_at`, and `folder.hash`.
///
/// The SC-02 walker has no early `continue` here either: an unopenable directory goes
/// straight through `accumulate_entry` into `frame.entries_classified` (`walk.rs`,
/// `EnterOutcome::Skipped`). Warning-and-continuing here instead would persist counts, an
/// image list, an image timestamp and a fingerprint that disagree with the walker's snapshot
/// of the same tree — and SC-06's change detection compares against the walker's hash.
///
/// Tag-reading the audio-named directory is Go's per-file skip arm, not a folder abort: the
/// extractor cannot open it and `continue`s (`adapters/gotaglib/gotaglib.go:34-43`), so the
/// entry contributes no media row while the folder still commits. (`fs.Stat` of a mode-`000`
/// directory succeeds, and the `fs.Stat` fallback inside `localFS.ReadTags` only runs for a
/// result the extractor actually returned, so Go's folder-level arm is not reachable here.)
///
/// Root can open a mode-`000` directory, so the test verifies unreadability FIRST and skips
/// with an explicit reason when the platform still allows it. Permissions are restored before
/// the assertions so a failure cannot leave the scratch tree undeletable.
#[cfg(unix)]
#[tokio::test]
async fn unreadable_directories_with_media_names_are_classified_like_the_walker_does() {
    use std::os::unix::fs::PermissionsExt;

    use common::assert_stamp_ordering;
    use nd_scanner::contract::{LibraryRoot, ScanWarningKind, WalkRequest, WalkTarget};
    use nd_scanner::folder::EntryCategory;
    use nd_scanner::storage::LogicalPath;
    use nd_scanner::walk::WalkHandle;

    assert_stamp_ordering();

    let tree = Tree::new("unreadable-names");
    let folder = tree.dir("album/one");
    common::copy_pinned(&folder, "known.mp3", "known.mp3");
    // Two directories that look like media files by name, each holding a real file that
    // must never be seen because the directory cannot be opened.
    let audio_dir = tree.dir("album/one/songs.mp3");
    common::copy_pinned(&audio_dir, "known.flac", "hidden.flac");
    let image_dir = tree.dir("album/one/cover.jpg");
    std::fs::write(image_dir.join("embedded.jpg"), b"not readable")
        .expect("write inside image dir");

    common::set_dir_time(&folder, common::file_time());
    common::set_dir_time(&audio_dir, common::later_file_time());
    common::set_dir_time(&image_dir, common::later_file_time());

    for blocked in [&audio_dir, &image_dir] {
        std::fs::set_permissions(blocked, std::fs::Permissions::from_mode(0o000))
            .expect("chmod 000 the directory");
    }

    let denied = [&audio_dir, &image_dir]
        .iter()
        .filter(|dir| std::fs::read_dir(dir).is_err())
        .copied()
        .count();
    if denied != 2 {
        // Self-skip with an explicit reason: the assertions below would prove nothing about
        // the classification-after-unreadable arm, because the directories are openable.
        eprintln!(
            "SKIPPED unreadable_directories_with_media_names_are_classified_like_the_walker_does: \
             this user can open a mode-000 directory (root/sudo, or a platform without POSIX \
             permission enforcement), so the arm is unreachable here. It is covered on an \
             unprivileged account."
        );
        for restored in [&audio_dir, &image_dir] {
            std::fs::set_permissions(restored, std::fs::Permissions::from_mode(0o755))
                .expect("restore permissions before cleanup");
        }
        return;
    }

    let (store, db) = fresh_store("unreadable-names", 2);
    seed_library(&store, 1, "unreadable-names", &tree.root);
    let mapping = mapping();

    let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));

    // Independent oracle: the committed SC-02 walker over the SAME tree, run while both
    // directories are still blocked. Classifying an unopenable directory is a property of
    // `accumulate_entry` by construction, so agreeing with the walker is agreeing with Go's
    // `loadDir` fall-through rather than with this ingest's own choice.
    let request = WalkRequest {
        libraries: vec![LibraryRoot {
            id: 1,
            name: "unreadable-names".to_string(),
            root: tree.root.as_os_str().to_os_string(),
        }],
        targets: vec![WalkTarget {
            library_id: 1,
            path: LogicalPath::parse("album/one").expect("logical path"),
        }],
        follow_symlinks: true,
        ignore_dot_folders: true,
    };
    let mut handle = WalkHandle::start(request, nd_scanner::contract::CancelToken::new());
    let mut events = Vec::new();
    while let Some(event) = handle.next().await {
        events.push(event);
    }
    let summary = handle.terminal().await;

    // Restore before the assertions: a failing assertion must not leave the tree
    // undeletable, and the walk above already observed the blocked state.
    for restored in [&audio_dir, &image_dir] {
        std::fs::set_permissions(restored, std::fs::Permissions::from_mode(0o755))
            .expect("restore permissions before cleanup");
    }

    assert!(
        summary.outcome.is_complete(),
        "the walker's own run must succeed: {}",
        summary.outcome.describe()
    );
    let walked = events
        .iter()
        .find(|event| event.snapshot.path.to_string() == "album/one")
        .expect("the walk emits the target folder");

    // Headline parity claim: same tree, two code paths, one fingerprint.
    assert_eq!(
        outcome.hash, walked.hash,
        "ingest and SC-02 walker fingerprint a tree with unreadable media-named directories \
         identically"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT hash FROM folder WHERE id = '{}'", outcome.folder_id)
        ),
        Some(outcome.hash.clone()),
        "the fingerprint written to the DB is that fingerprint"
    );

    // Classification, on both sides.
    assert_eq!(
        walked.snapshot.num_audio_files(),
        2,
        "the audio-named directory counts as an audio file, like Go's audioFiles map"
    );
    assert_eq!(
        outcome.audio_count, 2,
        "the ingest reports the same audio count the walker does"
    );
    assert_eq!(
        walked.snapshot.image_names(),
        vec!["cover.jpg"],
        "the image-named directory is indexed as an image file"
    );
    assert_eq!(
        walked.snapshot.num_sub_folders, 0,
        "neither unreadable directory is counted as a subfolder"
    );
    assert_eq!(
        walked
            .snapshot
            .category(EntryCategory::Audio)
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>(),
        vec!["known.mp3".to_string(), "songs.mp3".to_string()],
        "sorted audio names, with the unreadable directory among them"
    );

    // The persisted state Go's `toFolder` derives from that classification.
    assert_eq!(
        count(
            &store,
            &format!(
                "SELECT num_audio_files FROM folder WHERE id = '{}'",
                outcome.folder_id
            )
        ),
        2,
        "folder.num_audio_files includes the unreadable audio-named directory"
    );
    assert_eq!(
        scalar(
            &store,
            &format!(
                "SELECT image_files FROM folder WHERE id = '{}'",
                outcome.folder_id
            )
        ),
        Some("[\"cover.jpg\"]".to_string()),
        "folder.image_files carries the unreadable image-named directory"
    );
    let images_updated_at = scalar(
        &store,
        &format!(
            "SELECT images_updated_at FROM folder WHERE id = '{}'",
            outcome.folder_id
        ),
    );
    assert!(
        images_updated_at
            .as_ref()
            .is_some_and(|text| !text.starts_with("0001-01-01")),
        "folder.images_updated_at is the image timestamp, not Go's zero time: \
         {images_updated_at:?}"
    );

    // Non-vacuity: the pre-fix fingerprint — both directories merely warned, so neither
    // contributes a hash line and neither is classified — must hash DIFFERENTLY.
    assert!(
        outcome.hash != {
            let mut oracle = walked.snapshot.clone();
            oracle
                .entries
                .retain(|entry| entry.name != "songs.mp3" && entry.name != "cover.jpg");
            oracle.hash().expect("pre-fix-shaped hash")
        },
        "non-vacuity: dropping the unreadable directories from the classified entries changes \
         the fingerprint"
    );
    assert!(
        walked.snapshot.mod_time > common::file_time(),
        "the folder mtime is above the pinned stamp, so the directories' mtimes were folded in \
         too"
    );

    // Both warnings, once each, from the real EACCES.
    assert_eq!(
        outcome
            .warnings
            .iter()
            .filter(|w| w.kind == ScanWarningKind::DirectoryUnreadable)
            .count(),
        2,
        "each unreadable directory is reported once by the ingest"
    );
    assert_eq!(
        summary
            .warnings_of(ScanWarningKind::DirectoryUnreadable)
            .len(),
        2,
        "the walker reports the same two directories"
    );

    // Tag reading: the audio-named directory is Go's per-file skip arm, and the one real
    // file still produces exactly one media row.
    assert_eq!(
        outcome.tag_failures.len(),
        1,
        "only the unopenable directory fails to extract, got {:?}",
        outcome.tag_failures
    );
    assert_eq!(
        outcome.tag_failures[0].path.as_deref(),
        Some("album/one/songs.mp3"),
        "the skip names the audio-named directory"
    );
    assert_eq!(
        outcome.tag_failures[0].kind,
        nd_scanner::phase1::TagFailureKind::PerFileSkipped,
        "a directory the extractor cannot open is a per-file skip, not a folder-level abort"
    );
    assert_eq!(
        outcome.new_media_count, 1,
        "the folder still commits its one real track"
    );
    assert_eq!(
        children(&store, &outcome.folder_id)
            .iter()
            .map(|kid| kid.path.as_str())
            .collect::<Vec<_>>(),
        vec!["album/one/known.mp3"],
        "no media row is written for the unreadable directory"
    );

    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// A followed symlink whose **audio endpoint lies outside the library root** must be
/// classified, flagged, and warned about — not silently reported as an in-root entry.
///
/// Go indexes such a link when the resolved target's extension is audio
/// (`scanner/walk_dir_tree_test.go:520`): `resolveEntryName` (`walk_dir_tree.go:253-298`)
/// classifies by the endpoint's base name and `localFS.ResolveSymlink`
/// (`core/storage/local/local.go:72-77`, `filepath.EvalSymlinks`) happily lands outside the
/// root. The SC-02 walker records the relation as `FolderEntry::outside_root` plus an
/// `OutsideRootEndpoint` warning (`walk.rs`, `is_outside_root`). `snapshot_folder` hardcoded
/// `outside_root: false`, so the ingest classified the same entry correctly but persisted a
/// fingerprint derived from it while emitting no warning at all — a folder whose audio comes
/// from outside the library reported as unremarkable, and an ingest/walk divergence on the
/// warning set SC-06 surfaces.
///
/// The flag itself drives only the warning (the hash facts stay the link's own `lstat`, in
/// both paths), so the parity claim is asserted as: same warning count on both sides, and one
/// identical fingerprint. The walker's snapshot entry carries the flag itself, which is
/// asserted there.
///
/// `cfg(unix)` for the link; the external directory is a separate `$TMPDIR` path, canonicalized
/// like [`Tree`] so the outside-root relation compares real identities.
#[cfg(unix)]
#[tokio::test]
async fn an_out_of_root_audio_symlink_is_flagged_and_fingerprints_like_the_walker() {
    use common::assert_stamp_ordering;
    use nd_scanner::contract::{LibraryRoot, ScanWarningKind, WalkRequest, WalkTarget};
    use nd_scanner::storage::LogicalPath;
    use nd_scanner::walk::WalkHandle;

    assert_stamp_ordering();

    let tree = Tree::new("outside-root-link");
    let outside = external_root("outside-root-link");
    let folder = tree.dir("album/one");
    // The folder's own track is the m4a fixture: its `releasedate` tag derives a DIFFERENT
    // album id from the flac endpoint's, so the two audio entries do not collide into one
    // multi-track album group (SC-04's aggregation is not this lane).
    common::copy_pinned(&folder, "known.m4a", "known.m4a");
    // A real media file OUTSIDE the library root, plus a link to it with a different name:
    // classification must follow the target's extension, not the link's.
    let target = common::copy_pinned(&outside, "known.flac", "real.flac");
    std::os::unix::fs::symlink(&target, folder.join("alias.mp3"))
        .expect("symlink outside the root");
    common::set_dir_time(&folder, common::file_time());

    let (store, db) = fresh_store("outside-root-link", 2);
    seed_library(&store, 1, "outside-root-link", &tree.root);
    let mapping = mapping();

    let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));

    let flagged: Vec<&nd_scanner::contract::ScanWarning> = outcome
        .warnings
        .iter()
        .filter(|warning| warning.kind == ScanWarningKind::OutsideRootEndpoint)
        .collect();
    assert_eq!(
        flagged.len(),
        1,
        "the out-of-root endpoint is warned about exactly once, got {:?}",
        outcome
            .warnings
            .iter()
            .map(|w| (w.kind, w.path.as_ref().map(|p| p.to_string())))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        flagged[0].path.as_ref().map(|p| p.to_string()),
        Some("album/one/alias.mp3".to_string()),
        "the warning names the link, whose logical path stays inside the root"
    );

    // Independent oracle: the committed SC-02 walker over the same tree.
    let request = WalkRequest {
        libraries: vec![LibraryRoot {
            id: 1,
            name: "outside-root-link".to_string(),
            root: tree.root.as_os_str().to_os_string(),
        }],
        targets: vec![WalkTarget {
            library_id: 1,
            path: LogicalPath::parse("album/one").expect("logical path"),
        }],
        follow_symlinks: true,
        ignore_dot_folders: true,
    };
    let mut handle = WalkHandle::start(request, nd_scanner::contract::CancelToken::new());
    let mut events = Vec::new();
    while let Some(event) = handle.next().await {
        events.push(event);
    }
    let summary = handle.terminal().await;

    assert!(
        summary.outcome.is_complete(),
        "the walker's own run must succeed: {}",
        summary.outcome.describe()
    );
    let walked = events
        .iter()
        .find(|event| event.snapshot.path.to_string() == "album/one")
        .expect("the walk emits the target folder");
    let alias = walked
        .snapshot
        .entries
        .iter()
        .find(|entry| entry.name == "alias.mp3")
        .expect("the alias is classified, not skipped");
    assert!(
        alias.outside_root,
        "the walker records the outside-root flag"
    );
    assert_eq!(
        alias.classification_name, "real.flac",
        "classification follows the endpoint's base name"
    );
    assert_eq!(
        summary
            .warnings_of(ScanWarningKind::OutsideRootEndpoint)
            .len(),
        1,
        "the walker reports the same single outside-root endpoint"
    );

    // Same tree, two code paths: one fingerprint, and the same warning count.
    assert_eq!(
        outcome.hash, walked.hash,
        "ingest and SC-02 walker fingerprint an out-of-root audio link identically"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT hash FROM folder WHERE id = '{}'", outcome.folder_id)
        ),
        Some(outcome.hash.clone()),
        "the fingerprint written to the DB is that fingerprint"
    );
    // The hash facts stay the link's own lstat in both paths, so the flag provably did not
    // change the fingerprint — only the warning.
    let link_size = std::fs::symlink_metadata(folder.join("alias.mp3"))
        .expect("lstat the link")
        .len();
    assert_eq!(alias.stat.size, link_size, "hash facts stay the link's own");
    assert_ne!(
        alias.stat.size,
        std::fs::metadata(&target).expect("stat the target").len(),
        "non-vacuity: the link's lstat size differs from the endpoint's, so the hash line \
         really is the link's"
    );

    // The entry is audio and is imported through the followed endpoint.
    assert_eq!(
        outcome.audio_count, 2,
        "the out-of-root link counts as an audio file, like Go"
    );
    assert_eq!(
        count(
            &store,
            &format!(
                "SELECT num_audio_files FROM folder WHERE id = '{}'",
                outcome.folder_id
            )
        ),
        2,
        "folder.num_audio_files agrees"
    );
    assert_eq!(
        outcome.tag_failures,
        Vec::new(),
        "the endpoint's tags read cleanly through the link"
    );
    let mut paths: Vec<String> = children(&store, &outcome.folder_id)
        .iter()
        .map(|kid| kid.path.clone())
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        vec!["album/one/alias.mp3".to_string(), "album/one/known.m4a".to_string()],
        "the out-of-root endpoint is imported under the LINK's logical path, never the          target's"
    );
    assert_eq!(
        outcome.album_ids.len(),
        2,
        "each track drafts its own album, so the linked endpoint does not collide with the          folder's own track into a multi-track group"
    );

    remove_external_root(&outside);
    assert!(pool_healthy(&store));
    remove_db(&db);
}
