//! DB-22 executed source-parity assertions, using the fresh production schema.
//! Go sources: sql_tags.go:165-175; scrobble_repository.go:17-83;
//! playqueue_repository.go:78-166; artwork_repository.go:33-40,58-72,112-182.
//! No Go oracle was run; these fixtures pin the source contracts independently.

use nd_db::artwork_repo::{self, ArtworkError, ArtworkRepo, ArtworkRow, ItemArtworkRow};
use nd_db::execution;
use nd_db::genre_repo::{GenreError, GenreRepo};
use nd_db::playqueue_repo::{self, PlayQueueError, PlayQueueRepo};
use nd_db::scrobble_repo::{self, ScrobbleError, ScrobbleRepo};
use nd_db::sql_restful::{Filter, QueryOptions};
use nd_dbschema::schema::{apply_fresh_schema, register_extensions};
use rusqlite::Connection;

fn fresh() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    register_extensions(&conn).unwrap();
    apply_fresh_schema(&conn).unwrap();
    conn
}

fn seed(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO library(id,name,path) VALUES (1,'One','/one'),(2,'Two','/two');
         INSERT INTO user(id,user_name,created_at,updated_at) VALUES
           ('u1','one','2024-01-01','2024-01-01'),('u2','two','2024-01-01','2024-01-01');
         INSERT INTO user_library(user_id,library_id) VALUES ('u1',1),('u2',2);
         INSERT INTO media_file(id,title,library_id,has_cover_art,created_at,updated_at) VALUES
           ('a','Alpha',1,true,'2024-01-01','2024-01-01'),
           ('b','Beta',2,false,'2024-01-01','2024-01-01');",
    )
    .unwrap();
}

#[test]
fn genre_access_join_filters_both_rows_and_nontrivial_aggregates() {
    let conn = fresh();
    let repo = GenreRepo::new(&conn);
    assert!(repo.get_all(None).unwrap().is_empty());
    assert!(matches!(
        repo.get(None, "absent"),
        Err(GenreError::NotFound)
    ));
    seed(&conn);
    conn.execute_batch(
        "INSERT INTO tag(id,tag_name,tag_value) VALUES
           ('shared','genre','Rock'),('private','genre','Jazz'),
           ('unused','genre','Unused'),('other','mood','Happy');
         INSERT INTO library_tag(tag_id,library_id,album_count,media_file_count) VALUES
           ('shared',1,2,7),('shared',2,11,23),('private',2,5,17),('other',1,99,99);",
    )
    .unwrap();
    // LEFT access joins leak Jazz and inflate Rock to (13,30).
    let one = repo.get_all(Some("u1")).unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(
        (&*one[0].name, one[0].album_count, one[0].song_count),
        ("Rock", 2, 7)
    );
    let shared = repo.get(Some("u2"), "shared").unwrap();
    assert_eq!((shared.album_count, shared.song_count), (11, 23));
    assert_eq!(repo.get_all(Some("u2")).unwrap().len(), 2);
    assert!(matches!(
        repo.get(Some("u1"), "private"),
        Err(GenreError::NotFound)
    ));
    assert!(matches!(
        repo.get(Some("u1"), "unused"),
        Err(GenreError::NotFound)
    ));
    let aggregate = repo.get(None, "shared").unwrap();
    assert_eq!((aggregate.album_count, aggregate.song_count), (13, 30));
    let unused = repo.get(None, "unused").unwrap();
    assert_eq!((unused.album_count, unused.song_count), (0, 0));
    conn.execute_batch("DROP TABLE library_tag").unwrap();
    assert!(matches!(
        repo.get_all(Some("u1")),
        Err(GenreError::Sqlite(_))
    ));
    assert!(matches!(
        repo.get(None, "shared"),
        Err(GenreError::Sqlite(_))
    ));
}

#[test]
fn scrobble_epoch_bounds_sort_pages_counts_and_typed_failures() {
    let conn = fresh();
    let repo = ScrobbleRepo::new(&conn);
    assert_eq!(repo.count_all("u1", None).unwrap(), 0);
    assert!(repo.get_all("u1", None).unwrap().is_empty());
    assert!(matches!(repo.get("u1", 1), Err(ScrobbleError::NotFound)));
    seed(&conn);
    for (user, time) in [
        ("u1", 99),
        ("u1", 100),
        ("u1", 150),
        ("u1", 200),
        ("u1", 201),
        ("u2", 150),
    ] {
        execution::execute(&conn, &scrobble_repo::record_scrobble_sql("a", user, time)).unwrap();
    }
    let from = scrobble_repo::from_ts(100);
    let to = scrobble_repo::to_ts(200);
    let mut options = QueryOptions {
        sort: "submission_time".into(),
        order: "desc".into(),
        max: 1,
        offset: 2,
        filters: Some(vec![
            Filter::Raw {
                sql: from.0,
                raw_args: from.1,
            },
            Filter::Raw {
                sql: to.0,
                raw_args: to.1,
            },
        ]),
        ..Default::default()
    };
    assert_eq!(repo.count_all("u1", Some(&options)).unwrap(), 3);
    let page = repo.get_all("u1", Some(&options)).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!((page[0].id, page[0].submission_time), (2, 100));
    assert_eq!(page[0].media_file_id, "a");
    assert!(
        page[0].user_id.is_empty(),
        "baseQuery does not select user_id"
    );
    options.order = "asc".into();
    let ascending = repo.get_all("u1", Some(&options)).unwrap();
    assert_eq!((ascending[0].id, ascending[0].submission_time), (4, 200));
    options.order = "desc".into();
    options.max = 0;
    options.offset = 0;
    let times = |options: &QueryOptions| {
        repo.get_all("u1", Some(options))
            .unwrap()
            .iter()
            .map(|s| s.submission_time)
            .collect::<Vec<_>>()
    };
    assert_eq!(times(&options), [200, 150, 100]);
    options.order = "asc".into();
    assert_eq!(times(&options), [100, 150, 200]);
    assert_eq!(repo.get("u1", 2).unwrap().submission_time, 100);
    assert!(matches!(repo.get("u2", 2), Err(ScrobbleError::NotFound)));
    conn.execute_batch("DROP TABLE scrobbles").unwrap();
    assert!(matches!(
        repo.count_all("u1", None),
        Err(ScrobbleError::Sqlite(_))
    ));
    assert!(matches!(
        repo.get_all("u1", None),
        Err(ScrobbleError::Sqlite(_))
    ));
    assert!(matches!(repo.get("u1", 2), Err(ScrobbleError::Sqlite(_))));
}

fn queue(conn: &Connection, csv: &str) {
    conn.execute("DELETE FROM playqueue WHERE user_id='u1'", [])
        .unwrap();
    conn.execute(
        "INSERT INTO playqueue(id,user_id,current,position,changed_by,items,created_at,updated_at)
         VALUES ('q','u1',2,37,'client',?,'2024-01-01 00:00:00+00:00','2024-02-01 00:00:00+00:00')",
        [csv],
    )
    .unwrap();
}

#[test]
fn persisted_queue_csv_keeps_exact_tokens_and_hydrated_order() {
    let conn = fresh();
    let repo = PlayQueueRepo::new(&conn);
    assert!(matches!(repo.retrieve("u1"), Err(PlayQueueError::NotFound)));
    seed(&conn);
    // Whole-column trim guard does not trim individual tokens (Go :122-127).
    for (csv, expected) in [
        ("", vec![]),
        (" \t\n", vec![]),
        (" a,,b, ", vec![" a", "", "b", " "]),
        (",", vec!["", ""]),
        ("b,a,b,missing", vec!["b", "a", "b", "missing"]),
    ] {
        queue(&conn, csv);
        let stored = repo.retrieve("u1").unwrap();
        assert_eq!(stored.items, csv);
        let ids = playqueue_repo::column_to_items(stored.items);
        assert_eq!(ids, expected);
        if !csv.trim().is_empty() {
            assert_eq!(playqueue_repo::items_to_column(&ids), csv);
        }
    }
    conn.execute_batch("INSERT INTO annotation(user_id,item_id,item_type,rating,play_count) VALUES ('u1','a','media_file',4,9)").unwrap();
    execution::execute(
        &conn,
        &artwork_repo::put_item_artwork_sql(
            &ItemArtworkRow {
                item_kind: "mf".into(),
                item_id: "a".into(),
                image_type: "primary".into(),
                hash: "cover".into(),
                ..Default::default()
            },
            &nd_db::time::zero(),
        ),
    )
    .unwrap();
    // Explicit unrestricted (admin) read policy; annotation user is u1.
    let (stored, outcome) = repo
        .retrieve_with_media_files("u1", Some("u1"), None, true)
        .unwrap();
    assert_eq!(
        (stored.current, stored.position, &*stored.changed_by),
        (2, 37, "client")
    );
    assert!(stored.created_at < stored.updated_at);
    assert!(outcome.artwork_error.is_none());
    assert_eq!(
        outcome
            .items
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        ["b", "a", "b"]
    );
    assert_eq!(outcome.items[1].title, "Alpha");
    assert_eq!(outcome.items[1].library_path, "/one");
    assert_eq!(outcome.items[1].image.image_hash, "cover");
    assert_eq!(outcome.items[1].annotation.rating, 4);
    assert_eq!(outcome.items[1].annotation.play_count, 9);
    // Non-admin policy is supplied by the caller, just as for MediaFileRepo.
    // Filtering a stored cross-library id must not leak it through GetAll.
    let restricted = repo
        .retrieve_with_media_files(
            "u1",
            Some("u1"),
            Some((
                "media_file.library_id IN (SELECT library_id FROM user_library WHERE user_id = ?)"
                    .into(),
                vec![nd_db::sqlrepo::Value::Text("u1".into())],
            )),
            true,
        )
        .unwrap()
        .1;
    assert_eq!(restricted.items.len(), 1);
    assert_eq!(restricted.items[0].id, "a");
    queue(&conn, "  ");
    assert!(repo
        .retrieve_with_media_files("u1", Some("u1"), None, true)
        .unwrap()
        .1
        .items
        .is_empty());
    assert!(matches!(repo.retrieve("u2"), Err(PlayQueueError::NotFound)));
    conn.execute_batch("DROP TABLE playqueue").unwrap();
    assert!(matches!(
        repo.retrieve("u1"),
        Err(PlayQueueError::Sqlite(_))
    ));
}

#[test]
fn queue_media_lookup_chunks_at_500_and_propagates_failure_without_partial_rows() {
    let conn = fresh();
    seed(&conn);
    let mut ids = vec!["b".to_string(); 500];
    ids.extend(["a".into(), "missing".into(), "b".into()]);
    queue(&conn, &playqueue_repo::items_to_column(&ids));
    let statements = playqueue_repo::load_tracks_sql(&ids).unwrap();
    assert_eq!(
        statements.iter().map(|s| s.1.len()).collect::<Vec<_>>(),
        [500, 3]
    );
    // Boundaries are pinned above; production GetAll is exercised across them.
    let repo = PlayQueueRepo::new(&conn);
    let tracks = repo
        .retrieve_with_media_files("u1", Some("u1"), None, true)
        .unwrap()
        .1
        .items;
    assert_eq!(tracks.len(), 502);
    assert_eq!(tracks[500].id, "a");
    assert_eq!(tracks[501].id, "b");
    // Only the second chunk contains a. First-chunk b remains readable, but
    // a malformed numeric field in a must fail the entire strict queue read.
    conn.execute("UPDATE media_file SET bit_rate='broken' WHERE id='a'", [])
        .unwrap();
    let first = nd_db::mediafile_repo::MediaFileRepo::new(&conn)
        .get_all(
            Some("u1"),
            Some(("media_file.id = ?".into(), vec!["b".into()])),
            &[],
            0,
            0,
            true,
        )
        .unwrap();
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].id, "b");
    assert!(matches!(
        repo.retrieve_with_media_files("u1", Some("u1"), None, true),
        Err(PlayQueueError::MediaFile(
            nd_db::mediafile_repo::MediaFileError::Sqlite(_)
        ))
    ));
    conn.execute_batch("DROP TABLE media_file").unwrap();
    assert!(matches!(
        repo.retrieve_with_media_files("u1", Some("u1"), None, true),
        Err(PlayQueueError::MediaFile(_))
    ));
}

#[test]
fn artwork_strict_reads_scope_kind_id_slot_and_coalesce_missing_cache() {
    let conn = fresh();
    let repo = ArtworkRepo::new(&conn);
    assert!(repo.get_mime_by_hash().unwrap().is_empty());
    assert!(repo
        .get_info_for_items("al", &["x".into()])
        .unwrap()
        .is_empty());
    assert!(matches!(repo.get_image("h"), Err(ArtworkError::NotFound)));
    assert!(matches!(
        repo.get_item_artwork("al", "x", "primary"),
        Err(ArtworkError::NotFound)
    ));
    let image = ArtworkRow {
        hash: "h".into(),
        mime: "image/png".into(),
        width: 123,
        height: 45,
        size_bytes: 678,
        blur_hash: "blur".into(),
        thumb_hash: "thumb".into(),
        dominant_color: "#123456".into(),
        created_at: nd_db::time::zero(),
    };
    execution::execute(
        &conn,
        &artwork_repo::put_image_sql(&image, &nd_db::time::zero()),
    )
    .unwrap();
    assert_eq!(repo.get_image("h").unwrap(), image);
    for (kind, id, slot, hash) in [
        ("al", "x", "primary", "h"),
        ("mf", "x", "primary", "other"),
        ("al", "x", "back", "back-hash"),
        ("al", "absent", "primary", ""),
        ("al", "uncached", "primary", "not-cached"),
    ] {
        let item = ItemArtworkRow {
            item_kind: kind.into(),
            item_id: id.into(),
            image_type: slot.into(),
            hash: hash.into(),
            source: "embedded".into(),
            source_path: "/one/a.flac".into(),
            ref_mtime: 123456,
            trace: "[1]".into(),
            last_failure: "[2]".into(),
            attempted_at: Some(nd_db::time::zero()),
            updated_at: nd_db::time::zero(),
        };
        execution::execute(
            &conn,
            &artwork_repo::put_item_artwork_sql(&item, &nd_db::time::zero()),
        )
        .unwrap();
        assert_eq!(repo.get_item_artwork(kind, id, slot).unwrap(), item);
    }
    assert!(matches!(
        repo.get_item_artwork("album", "x", "primary"),
        Err(ArtworkError::NotFound)
    ));
    assert!(matches!(
        repo.get_item_artwork("al", "missing", "primary"),
        Err(ArtworkError::NotFound)
    ));
    let info = repo
        .get_info_for_items(
            "al",
            &[
                "x".into(),
                "absent".into(),
                "uncached".into(),
                "missing".into(),
            ],
        )
        .unwrap();
    assert_eq!(info.len(), 3);
    assert_eq!(
        (&*info["x"].hash, &*info["x"].blur_hash, info["x"].width),
        ("h", "blur", 123)
    );
    assert!(info["absent"].absent());
    assert!(!info["uncached"].absent());
    assert_eq!(
        (&*info["uncached"].blur_hash, info["uncached"].height),
        ("", 0)
    );
    assert_eq!(repo.get_mime_by_hash().unwrap()["h"], "image/png");
    conn.execute_batch("DROP TABLE artwork").unwrap();
    assert!(matches!(repo.get_image("h"), Err(ArtworkError::Sqlite(_))));
    assert!(matches!(
        repo.get_mime_by_hash(),
        Err(ArtworkError::Sqlite(_))
    ));
    assert!(matches!(
        repo.get_info_for_items("al", &["x".into()]),
        Err(ArtworkError::Sqlite(_))
    ));
    // No ids means no statements, even when the tables are unavailable.
    assert!(repo.get_info_for_items("al", &[]).unwrap().is_empty());
    conn.execute_batch("DROP TABLE item_artwork").unwrap();
    assert!(matches!(
        repo.get_item_artwork("al", "x", "primary"),
        Err(ArtworkError::Sqlite(_))
    ));
}

#[test]
fn artwork_200_id_chunks_never_return_partial_maps_on_late_scan_failure() {
    let conn = fresh();
    let ids = (0..201).map(|i| format!("id-{i}")).collect::<Vec<_>>();
    for id in &ids {
        conn.execute(
            "INSERT INTO item_artwork(item_kind,item_id,hash) VALUES ('al',?,'h')",
            [id],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO artwork(hash,mime,width) VALUES ('h','image/jpeg',8)",
        [],
    )
    .unwrap();
    let statements = artwork_repo::get_info_for_items_sql("al", &ids).unwrap();
    assert_eq!(
        statements.iter().map(|s| s.1.len()).collect::<Vec<_>>(),
        [202, 3]
    );
    let repo = ArtworkRepo::new(&conn);
    assert_eq!(repo.get_info_for_items("al", &ids).unwrap().len(), 201);
    // A bad numeric cache value appears only in chunk two: successful chunk one
    // must not escape as a partial map. SQLite's dynamic types permit this seed.
    conn.execute(
        "INSERT INTO artwork(hash,mime,width) VALUES ('bad','image/jpeg','not-a-number')",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE item_artwork SET hash='bad' WHERE item_id=?",
        [&ids[200]],
    )
    .unwrap();
    assert!(matches!(
        repo.get_info_for_items("al", &ids),
        Err(ArtworkError::Sqlite(
            rusqlite::Error::FromSqlConversionFailure(..)
        ))
    ));
}

#[test]
fn artwork_reads_preserve_persisted_times_and_reject_null_nonpointer_times() {
    let conn = fresh();
    let repo = ArtworkRepo::new(&conn);
    let created = chrono::DateTime::parse_from_rfc3339("2024-01-02T03:04:05.123+03:00")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let updated = chrono::DateTime::parse_from_rfc3339("2024-02-03T04:05:06.456Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let image = ArtworkRow {
        hash: "timed".into(),
        mime: "image/png".into(),
        created_at: created,
        ..Default::default()
    };
    execution::execute(&conn, &artwork_repo::put_image_sql(&image, &created)).unwrap();
    assert_eq!(repo.get_image("timed").unwrap().created_at, created);
    let item = ItemArtworkRow {
        item_kind: "al".into(),
        item_id: "timed".into(),
        image_type: "primary".into(),
        attempted_at: Some(created),
        updated_at: updated,
        ..Default::default()
    };
    execution::execute(&conn, &artwork_repo::put_item_artwork_sql(&item, &updated)).unwrap();
    assert_eq!(
        repo.get_item_artwork("al", "timed", "primary").unwrap(),
        item
    );
    // Read independently persisted offset text, not merely writer round trips.
    conn.execute_batch("UPDATE artwork SET created_at='2024-01-02 03:04:05.123+03:00';")
        .unwrap();
    assert_eq!(repo.get_image("timed").unwrap().created_at, created);
    for column in ["attempted_at", "updated_at"] {
        // Reset the other field before testing NULL in this one.
        conn.execute(
            "UPDATE item_artwork SET attempted_at=?, updated_at=?",
            rusqlite::params![created, updated],
        )
        .unwrap();
        conn.execute(&format!("UPDATE item_artwork SET {column}=NULL"), [])
            .unwrap();
        let error = repo.get_item_artwork("al", "timed", "primary").unwrap_err();
        match error {
            ArtworkError::Sqlite(rusqlite::Error::FromSqlConversionFailure(_, _, source)) => {
                assert!(source.to_string().contains(column));
            }
            other => panic!("expected {column} scan failure, got {other:?}"),
        }
    }
    // Non-NULL invalid driver timestamp text scans as Go's zero time.
    conn.execute_batch("UPDATE item_artwork SET attempted_at='invalid', updated_at='invalid';")
        .unwrap();
    let zero = repo.get_item_artwork("al", "timed", "primary").unwrap();
    assert_eq!(zero.attempted_at, Some(nd_db::time::zero()));
    assert_eq!(zero.updated_at, nd_db::time::zero());
}

#[test]
fn scrobble_sort_supports_plain_camel_snake_comma_and_relative_directions() {
    let conn = fresh();
    seed(&conn);
    for (media, time) in [("a", 200), ("b", 100), ("a", 200), ("a", 150)] {
        execution::execute(
            &conn,
            &scrobble_repo::record_scrobble_sql(media, "u1", time),
        )
        .unwrap();
    }
    let repo = ScrobbleRepo::new(&conn);
    for (sort, order, expected) in [
        ("id", "  DeSc  ", vec![4, 3, 2, 1]),
        ("submissionTime,id", "desc", vec![3, 1, 4, 2]),
        ("submission_time, id", " desc ", vec![3, 1, 4, 2]),
        ("submissionTime desc, id asc", "desc", vec![2, 4, 3, 1]),
        ("mediaFileId,submissionTime,id", "asc", vec![4, 1, 3, 2]),
    ] {
        let options = QueryOptions {
            sort: sort.into(),
            order: order.into(),
            ..Default::default()
        };
        let rows = repo.get_all("u1", Some(&options)).unwrap();
        assert_eq!(
            rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            expected,
            "sort={sort:?}, order={order:?}"
        );
        assert_eq!(repo.count_all("u1", Some(&options)).unwrap(), 4);
    }
    for sort in ["submissionTime", "submission_time"] {
        let options = QueryOptions {
            sort: sort.into(),
            order: "  DESC  ".into(),
            ..Default::default()
        };
        let rows = repo.get_all("u1", Some(&options)).unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| row.submission_time)
                .collect::<Vec<_>>(),
            [200, 200, 150, 100]
        );
    }
    let options = QueryOptions {
        sort: "id; DROP TABLE scrobbles".into(),
        ..Default::default()
    };
    assert!(matches!(
        repo.get_all("u1", Some(&options)),
        Err(ScrobbleError::Sqlite(_))
    ));
    assert_eq!(repo.count_all("u1", None).unwrap(), 4);
}

#[test]
fn invalid_scrobble_sorts_fail_instead_of_succeeding_or_reinterpreting_expressions() {
    let conn = fresh();
    seed(&conn);
    execution::execute(&conn, &scrobble_repo::record_scrobble_sql("a", "u1", 123)).unwrap();
    for sort in [
        ",",
        ",,",
        "rowid",
        "id +1",
        "user_id",
        "id sideways",
        "id\tdesc",
    ] {
        let options = QueryOptions {
            sort: sort.into(),
            ..Default::default()
        };
        assert!(matches!(
            ScrobbleRepo::new(&conn).get_all("u1", Some(&options)),
            Err(ScrobbleError::Sqlite(_))
        ));
    }
}

#[test]
fn repository_scalar_reads_use_go_blob_coercions_and_keep_required_null_errors() {
    let conn = fresh();
    seed(&conn);
    queue(&conn, "b,a,b");
    conn.execute_batch(
        "UPDATE playqueue SET items=X'622c612c62', changed_by=X'636c69656e74',
           current=X'313233', position=X'343536';
         INSERT INTO tag(id,tag_name,tag_value) VALUES (X'67','genre',X'526f636b');
         INSERT INTO library_tag(tag_id,library_id,album_count,media_file_count)
           VALUES (X'67',1,X'313233',X'343536');
         INSERT INTO media_file(id,library_id) VALUES (X'626c6f622d61',1);
         INSERT INTO scrobbles(id,media_file_id,user_id,submission_time)
           VALUES (1,X'626c6f622d61','u1',X'313233');
         INSERT INTO artwork(hash,mime,width,height,size_bytes,blur_hash,thumb_hash,dominant_color)
           VALUES ('h',X'696d6167652f706e67',X'313233',X'343536',X'373839',
                   X'626c7572',X'7468756d62',X'23616263646566');
         INSERT INTO artwork(hash,mime,width,blur_hash)
           VALUES (X'626c6f622d68',X'696d6167652f6a706567',X'313233',X'6a6f696e6564');
         INSERT INTO item_artwork(item_kind,item_id,hash,source,source_path,ref_mtime,
                                  trace,last_failure,attempted_at,updated_at)
           VALUES ('al','x','h',X'656d626564646564',X'2f612e666c6163',X'313233',
                   X'5b315d',X'5b325d','2024-01-01','2024-01-02');",
    )
    .unwrap();
    let queue_repo = PlayQueueRepo::new(&conn);
    let stored = queue_repo.retrieve("u1").unwrap();
    assert_eq!(stored.items, "b,a,b");
    assert_eq!(stored.changed_by, "client");
    assert_eq!((stored.current, stored.position), (123, 456));
    assert_eq!(
        playqueue_repo::column_to_items(stored.items),
        ["b", "a", "b"]
    );
    let genres = GenreRepo::new(&conn).get_all(Some("u1")).unwrap();
    assert_eq!(genres.len(), 1);
    assert_eq!((&*genres[0].id, &*genres[0].name), ("g", "Rock"));
    assert_eq!((genres[0].album_count, genres[0].song_count), (123, 456));
    let scrobble = ScrobbleRepo::new(&conn).get("u1", 1).unwrap();
    assert_eq!(scrobble.media_file_id, "blob-a");
    assert_eq!(scrobble.submission_time, 123);
    assert_eq!(
        ScrobbleRepo::new(&conn).get_all("u1", None).unwrap(),
        [scrobble]
    );
    let artwork_repo = ArtworkRepo::new(&conn);
    let image = artwork_repo.get_image("h").unwrap();
    assert_eq!(image.mime, "image/png");
    assert_eq!(
        (image.width, image.height, image.size_bytes),
        (123, 456, 789)
    );
    assert_eq!(
        (
            &*image.blur_hash,
            &*image.thumb_hash,
            &*image.dominant_color
        ),
        ("blur", "thumb", "#abcdef")
    );
    let mime = artwork_repo.get_mime_by_hash().unwrap();
    assert_eq!(mime["h"], "image/png");
    assert_eq!(mime["blob-h"], "image/jpeg");
    let item = artwork_repo.get_item_artwork("al", "x", "primary").unwrap();
    assert_eq!((&*item.source, &*item.source_path), ("embedded", "/a.flac"));
    assert_eq!(item.ref_mtime, 123);
    assert_eq!((&*item.trace, &*item.last_failure), ("[1]", "[2]"));
    let info = artwork_repo
        .get_info_for_items("al", &["x".into()])
        .unwrap();
    assert_eq!((info["x"].width, info["x"].height), (123, 456));
    assert_eq!(info["x"].blur_hash, "blur");
    // A BLOB hash must join the BLOB cache key and decode in the info map.
    conn.execute_batch(
        "INSERT INTO item_artwork(item_kind,item_id,hash) VALUES ('al','y',X'626c6f622d68');",
    )
    .unwrap();
    let info = artwork_repo
        .get_info_for_items("al", &["y".into()])
        .unwrap();
    assert_eq!(info["y"].hash, "blob-h");
    assert_eq!(info["y"].width, 123);
    assert_eq!(info["y"].blur_hash, "joined");
    for column in ["items", "position"] {
        queue(&conn, "a");
        conn.execute(&format!("UPDATE playqueue SET {column}=NULL"), [])
            .unwrap();
        assert!(matches!(
            queue_repo.retrieve("u1"),
            Err(PlayQueueError::Sqlite(
                rusqlite::Error::FromSqlConversionFailure(..)
            ))
        ));
    }
    conn.execute_batch("UPDATE playqueue SET items='a',position=X'62726f6b656e';")
        .unwrap();
    assert!(matches!(
        queue_repo.retrieve("u1"),
        Err(PlayQueueError::Sqlite(
            rusqlite::Error::FromSqlConversionFailure(..)
        ))
    ));
}

#[test]
fn queue_preserves_distinct_artwork_errors_from_every_chunk_in_call_order() {
    let conn = fresh();
    seed(&conn);
    conn.execute_batch(
        "UPDATE media_file SET album_id='album-b' WHERE id='b';
         UPDATE media_file SET album_id='album-a' WHERE id='a';
         INSERT INTO artwork(hash,mime,width,height) VALUES
           ('bad-width','image/png','bad',10),('bad-height','image/png',10,'bad');
         INSERT INTO item_artwork(item_kind,item_id,hash) VALUES
           ('al','album-b','bad-width'),('al','album-a','bad-height');",
    )
    .unwrap();
    let mut ids = vec!["b".to_string(); 500];
    ids.extend(vec!["a".to_string(); 3]);
    queue(&conn, &playqueue_repo::items_to_column(&ids));
    let diagnostic_for = |id: &str| {
        nd_db::mediafile_repo::MediaFileRepo::new(&conn)
            .get_all(
                Some("u1"),
                Some(("media_file.id = ?".into(), vec![id.into()])),
                &[],
                0,
                0,
                false,
            )
            .unwrap()
            .artwork_error
            .expect("the malformed artwork must produce a diagnostic")
    };
    let first = diagnostic_for("b");
    let second = diagnostic_for("a");
    assert!(first.contains("width"));
    assert!(second.contains("height"));
    assert_ne!(first, second);
    let (_, outcome) = PlayQueueRepo::new(&conn)
        .retrieve_with_media_files("u1", Some("u1"), None, false)
        .unwrap();
    assert_eq!(outcome.items.len(), 503);
    assert_eq!(outcome.artwork_error, Some(format!("{first}; {second}")));
}
