//! DB-02 scanner timestamp runtime contract against the fresh production schema.
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use nd_db::{
    album_repo, folder_repo,
    hydrate::RowGet,
    library_repo, mediafile_repo,
    pool::{Pool, PoolConfig},
    sqlrepo::Value,
    store::SQLStore,
    time,
};
use nd_dbschema::schema::apply_fresh_schema;

fn at(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32, nanos: u32) -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_nano_opt(h, min, s, nanos)
            .unwrap(),
    )
}

fn store() -> (SQLStore, std::path::PathBuf) {
    let path =
        std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-db-scan-clock-", ".db"));
    if path.exists() {
        std::fs::remove_file(&path).unwrap();
    }
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let (pool, conn) = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    apply_fresh_schema(&conn).unwrap();
    conn.execute(
        "INSERT INTO library(name,path) VALUES('clock','/clock')",
        [],
    )
    .unwrap();
    pool.put_back(conn).unwrap();
    (SQLStore::new(pool), path)
}

fn run<T>(
    store: &SQLStore,
    f: impl FnOnce(&rusqlite::Connection) -> Result<T, rusqlite::Error>,
) -> T {
    store
        .with_connection_sync(f)
        .unwrap_or_else(|e| panic!("DB operation failed: {e}"))
}

fn raw(v: &Value) -> rusqlite::types::Value {
    match v {
        Value::Text(s) => rusqlite::types::Value::Text(s.clone()),
        Value::Integer(n) => rusqlite::types::Value::Integer(*n),
        Value::Real(n) => rusqlite::types::Value::Real(*n),
        Value::Bool(b) => rusqlite::types::Value::Integer(i64::from(*b)),
        Value::Null => rusqlite::types::Value::Null,
    }
}

fn execute(store: &SQLStore, statement: &(String, Vec<Value>)) -> usize {
    run(store, |c| {
        c.execute(
            &statement.0,
            rusqlite::params_from_iter(statement.1.iter().map(raw)),
        )
    })
}

fn count(store: &SQLStore) -> i64 {
    let (sql, args) = library_repo::scan_in_progress_count_sql();
    run(store, |c| {
        c.query_row(
            &sql,
            rusqlite::params_from_iter(args.iter().map(raw)),
            |r| r.get(0),
        )
    })
}

fn cleanup(store: SQLStore, path: std::path::PathBuf) {
    drop(store);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn lifecycle_and_fresh_ddl_sentinel_keep_go_zero_predicate() {
    let (db, path) = store();
    // The fresh DDL sentinel is deliberately not normalized: Go's <> zero counts it.
    assert_eq!(count(&db), 1);
    let zero = time::zero();
    assert_eq!(time::to_text(&zero), "0001-01-01 00:00:00+00:00");
    let start = at(2024, 1, 2, 3, 4, 5, 123_000_000);
    let finish = at(2024, 1, 2, 3, 4, 6, 456_000_000);
    assert_eq!(
        execute(&db, &library_repo::scan_begin_sql(1, &start, true)),
        1
    );
    assert_eq!(count(&db), 1);
    let state: (String, i64) = run(&db, |c| {
        c.query_row(
            "SELECT last_scan_started_at, full_scan_in_progress FROM library WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    });
    assert_eq!(state, (time::to_text(&start), 1));
    assert_eq!(execute(&db, &library_repo::scan_end_sql(1, &finish)), 1);
    assert_eq!(count(&db), 0);
    let end: (String, String, i64) = run(&db, |c| {
        c.query_row(
            "SELECT last_scan_at, last_scan_started_at, full_scan_in_progress FROM library WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
    });
    assert_eq!(end, (time::to_text(&finish), time::to_text(&zero), 0));
    let second = at(2024, 1, 2, 3, 4, 7, 0);
    execute(&db, &library_repo::scan_begin_sql(1, &second, false));
    assert_eq!(count(&db), 1);
    let interrupted: (String, i64) = run(&db, |c| {
        c.query_row(
            "SELECT last_scan_started_at, full_scan_in_progress FROM library WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    });
    assert_eq!(interrupted, (time::to_text(&second), 0));

    // Old ScanEnd0 (INTEGER zero) does not satisfy the canonical TEXT zero comparison.
    run(&db, |c| {
        c.execute("UPDATE library SET last_scan_started_at = 0 WHERE id=1", [])
            .map(|_| ())
    });
    assert_eq!(
        count(&db),
        1,
        "old integer ScanEnd0 does not satisfy canonical TEXT zero"
    );
    let storage: String = run(&db, |c| {
        c.query_row(
            "SELECT typeof(last_scan_started_at) FROM library WHERE id=1",
            [],
            |r| r.get(0),
        )
    });
    assert_eq!(storage, "integer");
    cleanup(db, path);
}

#[test]
fn fractional_chronology_and_integer_negative_controls() {
    let (db, path) = store();
    let scan = at(2024, 2, 3, 4, 5, 6, 500_000_000);
    let older = at(2024, 2, 3, 4, 5, 6, 499_000_000);
    let equal = scan;
    let newer = at(2024, 2, 3, 4, 5, 6, 501_000_000);
    let text = |dt: &DateTime<Utc>| time::to_text(dt);
    run(&db, |c| {
        c.execute(
            "UPDATE library SET last_scan_at=? WHERE id=1",
            [text(&scan)],
        )?;
        c.execute(
            "INSERT INTO folder(id,library_id,path,name,missing,num_playlists,updated_at) VALUES('f-new',1,'/f','f',0,1,?),('f-equal',1,'/e','e',0,1,?),('f-old',1,'/o','o',0,1,?)",
            rusqlite::params![text(&newer), text(&equal), text(&older)],
        )?;
        c.execute(
            "INSERT INTO album(id,name,library_id,imported_at) VALUES('a-new','n',1,?),('a-equal','e',1,?),('a-old','o',1,?)",
            rusqlite::params![text(&newer), text(&equal), text(&older)],
        )?;
        Ok(())
    });
    let folder_query = folder_repo::get_touched_with_playlists_sql(None);
    let folder_ids: Vec<String> = run(&db, |c| {
        let mut stmt = c.prepare(&folder_query.0)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(folder_query.1.iter().map(raw)),
            |r| r.get(0),
        )?;
        rows.collect()
    });
    assert!(folder_ids.contains(&"f-new".to_string()));
    assert!(!folder_ids.contains(&"f-equal".to_string()));
    assert!(!folder_ids.contains(&"f-old".to_string()));

    let album_query = album_repo::get_touched_albums_sql(None, false, None, 1);
    let album_ids: Vec<String> = run(&db, |c| {
        let mut stmt = c.prepare(&album_query.0)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(album_query.1.iter().map(raw)),
            |r| r.get("id"),
        )?;
        rows.collect()
    });
    assert!(album_ids.contains(&"a-new".to_string()));
    assert!(!album_ids.contains(&"a-equal".to_string()));
    assert!(!album_ids.contains(&"a-old".to_string()));

    // INTEGER has NUMERIC affinity and is dropped by the same TEXT cutoff.
    run(&db, |c| {
        c.execute(
            "INSERT INTO folder(id,library_id,path,name,missing,num_playlists,updated_at) VALUES('f-int',1,'/i','i',0,1,?)",
            [scan.timestamp()],
        )
        .map(|_| ())
    });
    let included: i64 = run(&db, |c| {
        c.query_row(
            "SELECT count(*) FROM folder JOIN library ON library.id=folder.library_id WHERE folder.id='f-int' AND folder.updated_at > library.last_scan_at",
            [],
            |r| r.get(0),
        )
    });
    assert_eq!(
        included, 0,
        "INTEGER timestamp is dropped by the TEXT cutoff"
    );
    cleanup(db, path);
}

#[test]
fn recent_match_cutoff_is_strict_and_uses_text_datetime_bounds() {
    let (db, path) = store();
    run(&db, |c| {
        c.execute(
            "INSERT INTO library(name,path) VALUES('other','/other')",
            [],
        )
        .map(|_| ())
    });
    let since = at(2024, 3, 4, 5, 6, 7, 500_000_000);
    let earlier = at(2024, 3, 4, 5, 6, 7, 499_000_000);
    let later = at(2024, 3, 4, 5, 6, 7, 501_000_000);
    run(&db, |c| {
        c.execute(
            "INSERT INTO media_file(id,library_id,title,path,suffix,mbz_release_track_id,created_at) VALUES('recent-new',2,'Song','/new','flac','recording',?),('recent-equal',2,'Song','/equal','flac','recording',?),('recent-old',2,'Song','/old','flac','recording',?),('recent-int',2,'Song','/int','flac','recording',?)",
            rusqlite::params![
                time::to_text(&later),
                time::to_text(&since),
                time::to_text(&earlier),
                since.timestamp()
            ],
        )?;
        Ok(())
    });
    let query =
        mediafile_repo::find_recent_files_by_mbz_track_id_sql(&mediafile_repo::RecentFileByMbz {
            library_id: 1,
            mbz_release_track_id: "recording",
            suffix: "flac",
            since: &since,
        });
    let ids: Vec<String> = run(&db, |c| {
        let mut stmt = c.prepare(&query.0)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(query.1.iter().map(raw)), |r| {
            r.get("id")
        })?;
        rows.collect()
    });
    assert_eq!(ids, vec!["recent-new".to_string()]);
    cleanup(db, path);
}

#[test]
fn fresh_scanner_renderers_bind_datetime_stamps_as_text() {
    let stamp = at(2025, 6, 7, 8, 9, 10, 12_000_000);
    let expected = Value::Text(time::to_text(&stamp));
    assert_eq!(
        library_repo::scan_begin_sql(1, &stamp, false).1[0],
        expected
    );
    assert_eq!(library_repo::scan_end_sql(1, &stamp).1[0], expected);
    assert_eq!(
        library_repo::scan_end_sql(1, &stamp).1[2],
        library_repo::zero_time_value()
    );
    assert_eq!(
        mediafile_repo::mark_missing_sql(true, &["mf".to_string()], &stamp)[0].1[1],
        expected
    );
    assert_eq!(
        folder_repo::mark_missing_sql(true, &["f".to_string()], &stamp)[0].1[1],
        expected
    );
    assert_eq!(
        album_repo::touch_sql(&["al".to_string()], &stamp)[0].1[0],
        expected
    );
    // scrobble submission_time is deliberately an epoch INTEGER, not a datetime stamp.
    let scrobble = nd_db::scrobble_repo::record_scrobble_sql("mf", "u", stamp.timestamp());
    assert_eq!(scrobble.1.last(), Some(&Value::Integer(stamp.timestamp())));
}

fn stamp_state(
    store: &SQLStore,
    table: &str,
    column: &str,
    predicate: &str,
) -> (String, String, DateTime<Utc>) {
    let sql =
        format!("SELECT {column}, typeof({column}) AS stamp_type FROM {table} WHERE {predicate}");
    run(store, |c| {
        c.query_row(&sql, [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get_dt(column)?))
        })
    })
}

fn assert_stamp(
    store: &SQLStore,
    table: &str,
    column: &str,
    predicate: &str,
    expected: &DateTime<Utc>,
) {
    let (text, storage_type, hydrated) = stamp_state(store, table, column, predicate);
    assert_eq!(storage_type, "text", "{table}.{column} storage class");
    assert_eq!(text, time::to_text(expected), "{table}.{column} text");
    assert_eq!(hydrated, *expected, "{table}.{column} hydrated value");
}

#[test]
fn library_timestamp_writers_preserve_distinct_caller_stamps() {
    let (db, path) = store();
    let put_stamp = at(2024, 4, 5, 6, 7, 8, 101_000_000);
    let update = library_repo::LibraryUpdate {
        name: "clock",
        path: "/clock",
        remote_path: "",
        default_new_users: false,
        pid_album: "",
        pid_track: "",
        updated_at: put_stamp,
    };
    execute(&db, &library_repo::put_update_sql(1, &update, &[]));
    assert_stamp(&db, "library", "updated_at", "id=1", &put_stamp);

    let folder_stamp = at(2024, 4, 5, 6, 7, 9, 202_000_000);
    execute(
        &db,
        &library_repo::store_music_folder_sql("/clock/music", &folder_stamp),
    );
    assert_stamp(&db, "library", "updated_at", "id=1", &folder_stamp);
    let path_value: String = run(&db, |c| {
        c.query_row("SELECT path FROM library WHERE id=1", [], |row| row.get(0))
    });
    assert_eq!(path_value, "/clock/music");

    let stats_stamp = at(2024, 4, 5, 6, 7, 10, 303_000_000);
    execute(
        &db,
        &library_repo::refresh_stats_update_sql(&library_repo::RefreshStatsTotals {
            id: 1,
            songs: 2,
            albums: 3,
            artists: 4,
            folders: 5,
            files: 6,
            missing: 7,
            size: 8,
            duration: 9.5,
            updated_at: stats_stamp,
        }),
    );
    assert_stamp(&db, "library", "updated_at", "id=1", &stats_stamp);
    cleanup(db, path);
}

#[test]
fn album_folder_mediafile_and_playqueue_timestamp_writes_round_trip() {
    let (db, path) = store();
    let stamp = at(2024, 5, 6, 7, 8, 9, 456_000_000);
    let older = at(2024, 5, 6, 7, 8, 8, 456_000_000);
    run(&db, |c| {
        c.execute(
            "INSERT INTO folder(id,library_id,path,name,missing,updated_at) VALUES('f-clock',1,'/clock/f','f',1,?)",
            [time::to_text(&older)],
        )?;
        c.execute(
            "INSERT INTO album(id,name,library_id,folder_ids,imported_at) VALUES('a-clock','a',1,'[\"f-clock\"]',?)",
            [time::to_text(&older)],
        )?;
        c.execute(
            "INSERT INTO media_file(id,title,created_at,updated_at) VALUES('mf-clock','song',?,?)",
            rusqlite::params![time::to_text(&older), time::to_text(&older)],
        )?;
        c.execute(
            "INSERT INTO \"user\"(id,user_name,created_at,updated_at) VALUES('u-clock','u-clock',?,?)",
            rusqlite::params![time::to_text(&older), time::to_text(&older)],
        )?;
        Ok(())
    });

    execute(&db, &album_repo::touch_sql(&["a-clock".into()], &stamp)[0]);
    assert_stamp(&db, "album", "imported_at", "id='a-clock'", &stamp);
    let missing_stamp = at(2024, 5, 6, 7, 8, 10, 567_000_000);
    execute(
        &db,
        &album_repo::touch_by_missing_folder_sql(&missing_stamp),
    );
    assert_stamp(&db, "album", "imported_at", "id='a-clock'", &missing_stamp);

    let folder_stamp = at(2024, 5, 6, 7, 8, 11, 678_000_000);
    execute(
        &db,
        &folder_repo::mark_missing_sql(true, &["f-clock".into()], &folder_stamp)[0],
    );
    assert_stamp(&db, "folder", "updated_at", "id='f-clock'", &folder_stamp);
    let media_stamp = at(2024, 5, 6, 7, 8, 12, 789_000_000);
    execute(
        &db,
        &mediafile_repo::mark_missing_sql(true, &["mf-clock".into()], &media_stamp)[0],
    );
    assert_stamp(
        &db,
        "media_file",
        "updated_at",
        "id='mf-clock'",
        &media_stamp,
    );

    let queue_created = crate::time::zero();
    let queue_updated = at(2024, 5, 6, 7, 8, 13, 890_000_000);
    let mut queue = nd_db::playqueue_repo::PlayQueueRow::default();
    assert_eq!(queue.created_at, queue_created);
    assert_eq!(queue.updated_at, queue_created);
    queue.id = "pq-clock".into();
    queue.user_id = "u-clock".into();
    queue.items = "mf-clock".into();
    let statements =
        nd_db::playqueue_repo::store_sql(queue, false, String::new(), &[], &queue_updated);
    for statement in &statements {
        execute(&db, statement);
    }
    assert_stamp(
        &db,
        "playqueue",
        "created_at",
        "id='pq-clock'",
        &queue_created,
    );
    assert_stamp(
        &db,
        "playqueue",
        "updated_at",
        "id='pq-clock'",
        &queue_updated,
    );
    cleanup(db, path);
}

#[test]
fn artwork_timestamps_and_text_cutoff_purge_execute_against_fresh_schema() {
    let (db, path) = store();
    let cutoff = at(2024, 6, 7, 8, 9, 10, 500_000_000);
    let older = at(2024, 6, 7, 8, 9, 10, 499_000_000);
    let newer = at(2024, 6, 7, 8, 9, 10, 501_000_000);
    for (hash, created) in [
        ("orphan-old", &older),
        ("orphan-equal", &cutoff),
        ("orphan-new", &newer),
        ("referenced-old", &older),
    ] {
        let artwork = nd_db::artwork_repo::ArtworkRow {
            hash: hash.into(),
            mime: "image/jpeg".into(),
            ..Default::default()
        };
        execute(&db, &nd_db::artwork_repo::put_image_sql(&artwork, created));
    }
    let now = at(2024, 6, 7, 8, 9, 11, 234_000_000);
    for (id, attempted) in [
        ("item-none", None),
        ("item-zero", Some(time::zero())),
        ("item-stamped", Some(newer)),
        ("item-reference", None),
    ] {
        let item = nd_db::artwork_repo::ItemArtworkRow {
            item_kind: "mf".into(),
            item_id: id.into(),
            hash: if id == "item-reference" {
                "referenced-old"
            } else {
                ""
            }
            .into(),
            attempted_at: attempted,
            ..Default::default()
        };
        execute(&db, &nd_db::artwork_repo::put_item_artwork_sql(&item, &now));
    }
    for hash in ["orphan-old", "orphan-equal", "orphan-new", "referenced-old"] {
        assert_stamp(
            &db,
            "artwork",
            "created_at",
            &format!("hash='{hash}'"),
            if hash == "orphan-equal" {
                &cutoff
            } else if hash == "orphan-new" {
                &newer
            } else {
                &older
            },
        );
    }
    for id in ["item-none", "item-zero"] {
        assert_stamp(
            &db,
            "item_artwork",
            "attempted_at",
            &format!("item_id='{id}'"),
            &now,
        );
    }
    assert_stamp(
        &db,
        "item_artwork",
        "attempted_at",
        "item_id='item-stamped'",
        &newer,
    );
    assert_stamp(
        &db,
        "item_artwork",
        "updated_at",
        "item_id='item-stamped'",
        &now,
    );

    let purge = nd_db::artwork_repo::purge_orphans_sql(&cutoff);
    assert_eq!(execute(&db, &purge), 1);
    let remaining: Vec<String> = run(&db, |c| {
        let mut stmt = c.prepare("SELECT hash FROM artwork ORDER BY hash")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect()
    });
    assert_eq!(
        remaining,
        vec!["orphan-equal", "orphan-new", "referenced-old"]
    );
    assert_stamp(&db, "artwork", "created_at", "hash='orphan-equal'", &cutoff);
    assert_stamp(&db, "artwork", "created_at", "hash='orphan-new'", &newer);
    assert_stamp(
        &db,
        "artwork",
        "created_at",
        "hash='referenced-old'",
        &older,
    );
    cleanup(db, path);
}
