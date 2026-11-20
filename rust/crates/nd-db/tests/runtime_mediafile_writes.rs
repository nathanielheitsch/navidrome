//! DB-10 — the EXECUTED media-file Put (`persistence/mediafile_repository.go:184-197`
//! via `sql_base_repository.go:577-589,618-642` + `sql_participations.go:78-125` +
//! `sql_tags.go:57-79`) through the DB-00 typed transaction seam.
//!
//! Go truth under test, SOURCE-VERIFIED at the pinned lines and then EXECUTED-Rust
//! against a fresh schema (`nd_dbschema::schema::apply_fresh_schema`, one isolated temp
//! file per invocation, `_foreign_keys=on` — Go's own DSN parameter, `consts/consts.go:14`):
//! * `putByMatch` identity: a NON-empty id calls `put` directly and the `Eq{path,
//!   library_id}` probe never runs (`sql_base_repository.go:578-580`); otherwise the
//!   probe runs on the same transaction, absence is tolerated (`:585-588`), and a HARD
//!   read failure is returned rather than swallowed (`:585-587`).
//! * A found id is REUSED: the write targets the found row and the returned id is the
//!   found one, which is Go's `m.ID = id` (`mediafile_repository.go:192`) feeding both
//!   junction legs (`:193-196`).
//! * `Put` leg order is exactly base (UPDATE-first, INSERT on a zero-row match) →
//!   participants DELETE → optional participants INSERT → tags DELETE → optional tags
//!   INSERT, and any leg failure aborts the whole Put. The tags-leg poison trigger adds
//!   an `EXISTS` check on the NEW participant link, so it can ONLY fire after that INSERT
//!   has executed: the later failure itself is the ordering evidence, not an assumption.
//! * `Put` stamps ONLY `created_at`, and only when `CreatedAt.IsZero()`
//!   (`mediafile_repository.go:185-187`). `IsZero()` is a VALUE test on the year-one time, so
//!   the Rust port's two spellings of "unset" BOTH stamp: `None` (a NULL read) and an
//!   explicit `Some(time::zero())`. A real timestamp — even a near-year-one one — passes
//!   through. `created_at` and `birth_time` are dropped from the UPDATE set
//!   (`selectUpdateColumns`, `sql_base_repository.go:605-608`), while `updated_at` passes
//!   through the record unchanged — so an update that carries a new `updated_at` DOES rewrite
//!   it, and that asymmetry is asserted rather than assumed.
//! * An EMPTY id takes Go's random-id arm rather than being refused: `put` mints
//!   `id2.NewRandom()` (`sql_base_repository.go:635-638`) — `nd_id::new_random()` here —
//!   INSIDE the caller's transaction, skips the UPDATE (`:623`) and inserts under it. A failed
//!   attempt therefore rolls the minted id back with the row and publishes nothing, and only
//!   the committed result carries an id.
//! * Each junction DELETE runs unconditionally, so a record with empty participants/tags
//!   CLEARS stale links rather than leaving them (`sql_participations.go:79-89`,
//!   `sql_tags.go:58-74`). Only `genre` is indexed at all
//!   (`sql_tags::INDEXED_TAG_NAMES`), so non-genre names stay JSON-only.
//! * Both junction INSERTs are `INSERT … SELECT` over `json_each` INNER-joined to
//!   `artist`/`tag`, so unknown ids are dropped silently by the join while the stored JSON
//!   columns keep them — Go's asymmetry, pinned here rather than assumed. The dictionary
//!   rows for the record's NON-indexed names are seeded through the production DB-06 write,
//!   so their absence from `media_file_tags` proves the indexed-name filter rather than a
//!   JOIN miss.
//! * No FTS statement is issued: the schema's `media_file_fts_ai`/`_au`/`_ad` triggers
//!   (`nd-dbschema/src/schema_ddl.rs:82-84`) index the base row's own derived columns, so
//!   `media_file_fts` is only observable through `MATCH`, its row count, and its own FTS5
//!   shadow tables (`content=''` makes it CONTENTLESS — the index holds no copy of the
//!   content, so no content row is claimed to be readable from it). Its `WHEN` guard list does
//!   NOT include `tags` or `year`. The guard is pinned by a POSITIVE and a NEGATIVE
//!   observation of the SAME observable: a guarded change measurably GROWS the index
//!   (`media_file_fts_data` bytes / `media_file_fts_idx` entries), and a year-only update
//!   leaves those bytes and entries EXACTLY as they were. An unchanged row count alone would
//!   not distinguish "the trigger did not fire" from "it fired and rewrote the same docid", so
//!   it is never the sole evidence — see `media_year_only_update_reindexes_nothing`.
//!
//! **Documented port deviations, asserted rather than hidden.** An empty id is Go's
//! generated-id arm and is REPRODUCED (`sql_base_repository.go:635-638`), not refused, so
//! `MediaFileWriteError` has no `EmptyId` variant; the retry loop may therefore mint a
//! DIFFERENT id per attempt, which is Go's behavior (`persistence.go:207-228`) and is only
//! observable through the successful transaction's result. Go's `Put` also mutates `m.ID`; the
//! port borrows the record and reports the id in the result instead, which is asserted by
//! `put_by_match_reuses_path_library_identity` and by the generated-id tests.
//!
//! Fixture graph comes from PRODUCTION writes: DB-05 `library_repo::put_in_tx`,
//! DB-08 `artist_repo::put_artist_in_tx` (+ DB-05 `add_artist_in_tx` for the library link),
//! and DB-06 `tag_repo::add_in_tx` with real `nd_id::new_tag_id` ids. `user`/`user_library`
//! have no production write API yet, so — as in `runtime_library_writes.rs:113-129` — they
//! are fixture INSERTs. Pre-existing `media_file` rows for the identity tests are hand-
//! INSERTs because this lane's own API is the thing under test (the `insert_mediafile_raw`
//! precedent, `runtime_hydrate_album_mediafile.rs:352`). Test-local DDL (poison triggers) is
//! allowed as in `runtime_library_writes.rs:768-776`; `nd-dbschema` is untouched.
//!
//! `MediaFileRepo::get` proves library visibility through the caller-built `user_library`
//! clause (same as `runtime_hydrate_album_mediafile.rs:1488-1491`); it swallows artwork
//! failures, so it proves visibility, never artwork.
//!
//! There is no durable Go oracle for media-file Put (`testdata/` holds none), so nothing
//! here is labeled a Go golden — every claim is SOURCE-VERIFIED or EXECUTED-Rust. rusqlite's
//! `hooks` feature is not enabled in this manifest and this lane may not edit it, so
//! "no insert statement ran" is asserted from the renderer's `Option` legs plus the
//! observable junction state, and a trigger's NON-execution is claimed only from the FTS5
//! shadow-table state that a real re-index demonstrably changes (paired with the positive
//! control in the same test) — never from an unchanged row count alone. Recorded
//! as coverage gaps in the lane report.

use std::convert::Infallible;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use nd_db::album_repo::{participant_names_from_participants, NamedParticipant};
use nd_db::artist_repo::{put_artist_in_tx, ArtistColumn, ArtistRecord, ArtistWriteError};
use nd_db::execution::{execute, query_optional};
use nd_db::library_repo::{
    add_artist_in_tx, put_in_tx, LibraryColumn, LibraryRecord, LibraryWriteError,
};
use nd_db::mediafile_repo::{
    full_title, put_match_sql, put_mediafile_in_tx, put_sql, MediaFileError, MediaFileRecord,
    MediaFileRepo, MediaFileWriteError, MediaFileWriteResult, MediaIdentity,
};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sql_feeders::mediafile_fts_columns;
use nd_db::sql_participations::{Participant, Participants, Role};
use nd_db::sql_tags::{self, DbTags};
use nd_db::sqlrepo::Value;
use nd_db::store::{is_busy, SQLStore, TX_MAX_RETRIES};
use nd_db::tag_repo::{self, Tag};
use nd_db::{time, DbOperationError, DbRuntimeError, TransactionStage};
use nd_dbschema::schema::apply_fresh_schema;
use rusqlite::ffi;
use rusqlite::types::Value as SqlValue;
use rusqlite::Error as SqliteError;

/// Deterministic caller clocks. `Utc::now()` is never passed to `put_mediafile_in_tx`
/// anywhere in this file, so an implementation that stamped the commit clock would store a
/// value no test could name and every "stamped"/"unchanged" assertion would be vacuous.
fn at(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, s)
            .unwrap(),
    )
}

/// The scanner's save stamp: far OLDER than the commit clock of a test running today. Used
/// as `now`, i.e. the value a `created_at: None` record must be stamped with.
fn scan_stamp() -> DateTime<Utc> {
    at(2019, 1, 2, 3, 4, 5)
}

/// Older than `scan_stamp`: the record's stored `created_at`, which must survive an update
/// leg untouched and must NOT be replaced by `now`.
fn record_stamp() -> DateTime<Utc> {
    at(2017, 5, 6, 7, 8, 9)
}

/// Newer than the other two, used for the second Put's stamps. Still in the past, so a
/// commit-clock stamp can never accidentally equal it.
fn later_stamp() -> DateTime<Utc> {
    at(2021, 9, 10, 11, 12, 13)
}

/// The file's ctime: `birth_time` is NOT NULL in the DDL and is dropped from UPDATE sets, so
/// it must survive every update leg byte-identically.
fn birth_stamp() -> DateTime<Utc> {
    at(2015, 3, 4, 5, 6, 7)
}

/// Distinct from every other clock: the record's `updated_at`, which Go DOES pass through
/// the record on update (it is NOT in the drop list). A second Put carrying this value must
/// land it, which is what separates "dropped columns stayed" from "the whole row was frozen".
fn incoming_stamp() -> DateTime<Utc> {
    at(2013, 11, 12, 13, 14, 15)
}

fn dsn(path: &std::path::Path) -> String {
    format!("file:{}?mode=rwc&_foreign_keys=on", path.display())
}

/// One isolated on-disk DB per invocation, freshly migrated, pooled
/// (the `runtime_album_writes.rs:127-139` fixture shape).
fn fresh_store(label: &str) -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        &format!("nd-db-mediafile-writes-{label}-"),
        ".db",
    ));
    if path.exists() {
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("cannot clear {path:?}: {e}"));
    }
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn(&path), 2)).unwrap();
    apply_fresh_schema(&first).unwrap();
    pool.put_back(first).expect("return configured handle");
    (SQLStore::new(pool), path)
}

fn cleanup(store: SQLStore, path: std::path::PathBuf) {
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// Run a raw block on one pooled lease. Panics on a lease failure; never called from inside
/// a transaction (that would re-check out the same pool).
fn lease<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("lease failed: {e}"))
}

/// A raw fixture statement (poison triggers, `user`/`user_library` rows, and the
/// pre-existing `media_file` rows the identity tests need because this lane's own API is the
/// thing under test). Libraries, artists and tags come from DB-05/06/08 production writes.
fn fixture(store: &SQLStore, sql: &str, args: &[SqlValue]) {
    lease(store, |conn| {
        conn.execute(sql, rusqlite::params_from_iter(args.iter()))
            .unwrap_or_else(|e| panic!("{sql} --{args:?}--: {e}"));
    });
}

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.to_string())
}

fn integer(value: i64) -> SqlValue {
    SqlValue::Integer(value)
}

/// One scalar probe with no binds (counts, `typeof`).
fn scalar<T>(store: &SQLStore, sql: &str) -> T
where
    T: rusqlite::types::FromSql,
{
    lease(store, |conn| {
        conn.query_row(sql, [], |row| row.get(0))
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    })
}

/// Count read with several bound parameters.
fn count_rows(store: &SQLStore, sql: &str, args: &[SqlValue]) -> i64 {
    lease(store, |conn| {
        conn.query_row(sql, rusqlite::params_from_iter(args.iter()), |row| {
            row.get(0)
        })
        .unwrap_or_else(|e| panic!("{sql} --{args:?}--: {e}"))
    })
}

/// What one stored cell must look like. `NULL` and the empty string are DIFFERENT
/// expectations, which is what makes the `None`-bind variants non-vacuous. Column names are
/// this file's own constants (identifier-safe interpolation); the row id is bound.
enum Want {
    Text(String),
    Null,
    Int(i64),
    Real(f64),
    Bool(bool),
}

fn want_text(value: &str) -> Want {
    Want::Text(value.to_string())
}

fn assert_cell(store: &SQLStore, column: &str, id: &str, want: Want) {
    lease(store, |conn| {
        let mut statement = conn
            .prepare(&format!("SELECT {column} FROM media_file WHERE id = ?"))
            .unwrap_or_else(|e| panic!("prepare {column}: {e}"));
        let cell = statement
            .query_row([text(id)], |row| row.get::<_, Option<SqlValue>>(0))
            .unwrap_or_else(|e| panic!("{column} for {id}: {e}"));
        match want {
            Want::Null => assert!(
                matches!(cell, None | Some(SqlValue::Null)),
                "{column} for {id} must be NULL, got {cell:?}"
            ),
            Want::Text(expected) => match cell {
                Some(SqlValue::Text(value)) => {
                    assert_eq!(value, expected, "{column} for {id}")
                }
                other => panic!("{column} for {id} must be TEXT {expected:?}, got {other:?}"),
            },
            Want::Int(expected) => match cell {
                Some(SqlValue::Integer(value)) => {
                    assert_eq!(value, expected, "{column} for {id}")
                }
                other => panic!("{column} for {id} must be INTEGER {expected}, got {other:?}"),
            },
            Want::Real(expected) => match cell {
                Some(SqlValue::Real(value)) => {
                    assert_eq!(value, expected, "{column} for {id}")
                }
                other => panic!("{column} for {id} must be REAL {expected}, got {other:?}"),
            },
            Want::Bool(expected) => match cell {
                Some(SqlValue::Integer(value)) => {
                    assert_eq!(value, i64::from(expected), "{column} for {id}")
                }
                other => {
                    panic!("{column} for {id} must be INTEGER 0/1, got {other:?}")
                }
            },
        }
    });
}

/// The record's persisted column set, asserted cell by cell. Every field of
/// [`mediafile_record`] carries a DIFFERENT value, so a rotated positional argument list
/// cannot land on a value that happens to match another column.
fn assert_full_record(store: &SQLStore, id: &str, path: &str, library_id: i64) {
    let created = time::to_text(&record_stamp());
    let updated = time::to_text(&incoming_stamp());
    let birth = time::to_text(&birth_stamp());
    let cases: Vec<(&str, Want)> = vec![
        ("id", want_text(id)),
        ("path", want_text(path)),
        ("title", want_text(&format!("Title-{id}"))),
        ("album", want_text(&format!("Album-{id}"))),
        ("artist", want_text(&format!("TrackArtist-{id}"))),
        ("artist_id", want_text(&format!("arid-{id}"))),
        ("album_artist", want_text(&format!("AlArtist-{id}"))),
        ("album_id", want_text(&format!("al-{id}"))),
        ("has_cover_art", Want::Bool(true)),
        ("track_number", Want::Int(17)),
        ("disc_number", Want::Int(3)),
        ("year", Want::Int(1969)),
        ("size", Want::Int(12_345_678)),
        ("suffix", want_text(&format!("sfx-{id}"))),
        ("duration", Want::Real(456.75)),
        ("bit_rate", Want::Int(1411)),
        ("genre", want_text(&format!("Rock-{id}"))),
        ("compilation", Want::Bool(false)),
        ("created_at", Want::Text(created.clone())),
        ("updated_at", Want::Text(updated.clone())),
        ("album_artist_id", want_text(&format!("aa-{id}"))),
        ("date", want_text(&format!("date-{id}"))),
        ("original_year", Want::Int(1968)),
        ("original_date", want_text(&format!("orig-{id}"))),
        ("release_year", Want::Int(1970)),
        ("release_date", want_text(&format!("rel-{id}"))),
        ("order_album_name", want_text(&format!("oal-{id}"))),
        ("order_album_artist_name", want_text(&format!("oaa-{id}"))),
        ("order_artist_name", want_text(&format!("oar-{id}"))),
        ("sort_album_name", want_text(&format!("sal-{id}"))),
        ("sort_artist_name", want_text(&format!("sar-{id}"))),
        ("sort_album_artist_name", want_text(&format!("saa-{id}"))),
        ("sort_title", want_text(&format!("sti-{id}"))),
        ("disc_subtitle", want_text(&format!("dsub-{id}"))),
        ("catalog_num", want_text(&format!("cat-{id}"))),
        ("comment", want_text(&format!("cmt-{id}"))),
        ("order_title", want_text(&format!("oti-{id}"))),
        ("mbz_recording_id", want_text(&format!("mr-{id}"))),
        ("mbz_album_id", want_text(&format!("mal-{id}"))),
        ("mbz_artist_id", want_text(&format!("mar-{id}"))),
        ("mbz_album_artist_id", want_text(&format!("maa-{id}"))),
        ("mbz_album_type", want_text(&format!("mat-{id}"))),
        ("mbz_album_comment", want_text(&format!("mac-{id}"))),
        ("mbz_release_track_id", want_text(&format!("mrt-{id}"))),
        ("channels", Want::Int(2)),
        ("lyrics", want_text(&format!("lyr-{id}"))),
        ("sample_rate", Want::Int(44_100)),
        ("library_id", Want::Int(library_id)),
        ("folder_id", want_text(&format!("fold-{id}"))),
        ("pid", want_text(&format!("pid-{id}"))),
        ("missing", Want::Bool(false)),
        ("mbz_release_group_id", want_text(&format!("mrg-{id}"))),
        ("tags", Want::Text(tags_json())),
        ("participants", Want::Text(PARTICIPANTS_JSON.to_string())),
        ("explicit_status", want_text(&format!("exp-{id}"))),
        ("birth_time", Want::Text(birth.clone())),
        ("rg_album_gain", Want::Real(0.5)),
        ("rg_album_peak", Want::Real(0.9)),
        ("rg_track_gain", Want::Real(-0.3)),
        ("rg_track_peak", Want::Real(0.7)),
        ("bpm", Want::Int(120)),
        ("bit_depth", Want::Int(24)),
        ("codec", want_text(&format!("codec-{id}"))),
        (
            "probe_data",
            want_text(&format!(r#"{{"codec":"flac-{id}"}}"#)),
        ),
        ("search_participants", want_text("Main Act Second Act")),
    ];
    for (column, want) in cases {
        assert_cell(store, column, id, want);
    }
}

/// The `participants` column the fixture record must marshal: role-sorted keys, input order
/// within a role, `subRole` omitted when empty (`sql_participations.rs:322-336`).
const PARTICIPANTS_JSON: &str = r#"{"albumartist":[{"id":"ar-main","name":"Main Act"}],"artist":[{"id":"ar-main","name":"Main Act","subRole":"Vocals"}],"composer":[{"id":"ar-second","name":"Second Act"}]}"#;

/// The `tags` column the fixture record must marshal: name keys in sorted order (the
/// `BTreeMap`), values in INPUT order (serde serializes the `Vec` as-is, and Go's
/// `encoding/json` does the same for a slice), and each id from the SAME authority Go uses
/// (`model.NewTag` = `nd_id::new_tag_id`).
fn tags_json() -> String {
    format!(
        "{{\"albumversion\":[{{\"id\":\"{}\",\"value\":\"Deluxe\"}}],\
         \"genre\":[{{\"id\":\"{}\",\"value\":\"Rock\"}},{{\"id\":\"{}\",\"value\":\"Jazz\"}}],\
         \"subtitle\":[{{\"id\":\"{}\",\"value\":\"Live Version\"}}]}}",
        nd_id::new_tag_id("albumversion", "Deluxe"),
        nd_id::new_tag_id("genre", "Rock"),
        nd_id::new_tag_id("genre", "Jazz"),
        nd_id::new_tag_id("subtitle", "Live Version"),
    )
}

/// DB-05 library row through the PRODUCTION `put_in_tx`.
fn seed_library(store: &SQLStore, id: i64, tag: &str) {
    let record = LibraryRecord {
        id,
        name: format!("lib-{tag}-{id}"),
        path: format!("/lib-{tag}-{id}"),
        remote_path: String::new(),
        last_scan_at: record_stamp(),
        last_scan_started_at: record_stamp(),
        full_scan_in_progress: false,
        updated_at: record_stamp(),
        created_at: record_stamp(),
        total_songs: 0,
        total_albums: 0,
        total_artists: 0,
        total_folders: 0,
        total_files: 0,
        total_missing_files: 0,
        total_size: 0,
        total_duration: 0.0,
        default_new_users: false,
        pid_album: String::new(),
        pid_track: String::new(),
        scanned_pid_album: String::new(),
        scanned_pid_track: String::new(),
    };
    store
        .try_with_tx_sync::<nd_db::library_repo::LibraryWriteResult, LibraryWriteError>(|tx| {
            put_in_tx(tx, &record, &record_stamp(), &[] as &[LibraryColumn])
        })
        .unwrap_or_else(|e| panic!("seed_library({id}) failed: {e}"));
}

/// `Infallible` domain types escalated to the callback's error type: `Infallible` has no
/// inhabitant, so the `Domain` arm is unreachable.
fn escalate<E: std::error::Error + 'static>(
    err: DbOperationError<Infallible>,
) -> DbOperationError<E> {
    match err {
        DbOperationError::Sqlite(source) => DbOperationError::Sqlite(source),
        DbOperationError::Domain(unreachable) => match unreachable {},
    }
}

/// A real artist created through DB-08's PRODUCTION `put_artist_in_tx`, plus the DB-05
/// library link (Go's scanner order, `scanner/phase_1_folders.go:401-412`).
fn seed_artist(store: &SQLStore, id: &str, lib: i64) {
    let record = ArtistRecord {
        id: id.to_string(),
        name: format!("Name-{id}"),
        sort_artist_name: String::new(),
        order_artist_name: format!("Order-{id}"),
        mbz_artist_id: String::new(),
        biography: String::new(),
        small_image_url: String::new(),
        medium_image_url: String::new(),
        large_image_url: String::new(),
        external_url: String::new(),
        similar_artists: "[]".to_string(),
        external_info_updated_at: None,
        missing: false,
        uploaded_image: String::new(),
        created_at: Some(record_stamp()),
        updated_at: Some(record_stamp()),
        ..ArtistRecord::default()
    };
    store
        .try_with_tx_sync::<nd_db::artist_repo::ArtistWriteResult, ArtistWriteError>(|tx| {
            let result = put_artist_in_tx(tx, &record, &scan_stamp(), &[] as &[ArtistColumn])?;
            add_artist_in_tx(tx, lib, &result.id).map_err(escalate)?;
            Ok(result)
        })
        .unwrap_or_else(|e| panic!("seed_artist({id}) failed: {e}"));
}

/// A dictionary tag with a real `nd_id::new_tag_id` id (Go's `model.NewTag` authority — the
/// caller's job, not the repository's), created through DB-06's PRODUCTION `add_in_tx`.
/// Returns the id, which is also the expected `media_file_tags` value.
fn seed_tag(store: &SQLStore, lib: i64, name: &str, value: &str) -> String {
    let id = nd_id::new_tag_id(name, value);
    let tags: Vec<Tag> = vec![Tag {
        id: id.clone(),
        tag_name: name.to_string(),
        tag_value: value.to_string(),
        album_count: 0,
        song_count: 0,
    }];
    store
        .try_with_tx_sync::<(), Infallible>(|tx| tag_repo::add_in_tx(tx, lib, &tags))
        .unwrap_or_else(|e| panic!("seed_tag({name}={value}) failed: {e}"));
    id
}

/// A dictionary tag created through DB-06's PRODUCTION `add_in_tx` with an EXPLICIT id
/// (id derivation is the caller's job in Go too, `model.NewTag`). Used by the tags-insert
/// fault test, which needs a junction row the trigger can name.
fn seed_tag_with_id(store: &SQLStore, lib: i64, id: &str, name: &str, value: &str) {
    let tags: Vec<Tag> = vec![Tag {
        id: id.to_string(),
        tag_name: name.to_string(),
        tag_value: value.to_string(),
        album_count: 0,
        song_count: 0,
    }];
    store
        .try_with_tx_sync::<(), Infallible>(|tx| tag_repo::add_in_tx(tx, lib, &tags))
        .unwrap_or_else(|e| panic!("seed_tag_with_id({id}) failed: {e}"));
}

/// `user`/`user_library` rows: neither has a production write API yet, so these are fixture
/// INSERTs (`runtime_library_writes.rs:113-129` precedent). Without the grant the library
/// filter matches nothing and the visibility test would be vacuous rather than a proof.
fn seed_user(store: &SQLStore, id: &str, lib: i64) {
    fixture(
        store,
        "INSERT INTO \"user\" (id, user_name, name, email, password, is_admin, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            text(id),
            text(id),
            text(id),
            text(&format!("{id}@x.y")),
            text("CIPHER"),
            integer(i64::from(lib == 1)),
            text(&time::to_text(&record_stamp())),
            text(&time::to_text(&record_stamp())),
        ],
    );
    fixture(
        store,
        "INSERT INTO user_library (user_id, library_id) VALUES (?, ?)",
        &[text(id), integer(lib)],
    );
}

/// A pre-existing `media_file` row, hand-INSERTed on the columns the identity tests need.
/// This is the `insert_mediafile_raw` precedent (`runtime_hydrate_album_mediafile.rs:352`):
/// the row exists BEFORE the API under test runs, so an implementation that ignored the
/// match probe cannot pass by having written the row itself.
fn seed_mediafile(store: &SQLStore, id: &str, lib: i64, path: &str, title: &str) {
    fixture(
        store,
        "INSERT INTO media_file (id, library_id, path, title, created_at, updated_at, birth_time, tags) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            text(id),
            integer(lib),
            text(path),
            text(title),
            text(&time::to_text(&record_stamp())),
            text(&time::to_text(&record_stamp())),
            text(&time::to_text(&birth_stamp())),
            text("{}"),
        ],
    );
}

/// The `user_library`-scoped filter `MediaFileRepo::get` needs — the clause
/// `runtime_hydrate_album_mediafile.rs:1488-1491` uses.
fn media_lib_filter(user_id: &str) -> (String, Vec<Value>) {
    (
        "media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
            .to_string(),
        vec![Value::from(user_id)],
    )
}

/// The production read side. Panics on `NotFound`, which is why the caller must hold a grant.
fn read_back(store: &SQLStore, id: &str, user_id: &str) -> MediaFileRecord {
    lease(store, |conn| {
        MediaFileRepo::new(conn)
            .get(id, Some(user_id), Some(media_lib_filter(user_id)), false)
            .unwrap_or_else(|e| panic!("MediaFileRepo::get({id}) for {user_id}: {e}"))
    })
}

/// The production read as a `Result`, for the negative (cross-library) assertions.
fn read_attempt(
    store: &SQLStore,
    id: &str,
    user_id: &str,
) -> Result<MediaFileRecord, MediaFileError> {
    lease(store, |conn| {
        MediaFileRepo::new(conn).get(id, Some(user_id), Some(media_lib_filter(user_id)), false)
    })
}

/// Fixture roles: `albumartist` (no sub-role), `artist` (WITH a sub-role, so the sub-role
/// cannot reach the FTS text), and `composer`. `ar-main` therefore appears twice — the
/// stale-role case is real, not staged.
fn fixture_participants() -> Participants {
    let mut participants = Participants::new();
    participants.insert(
        Role::from("albumartist"),
        vec![Participant {
            id: "ar-main".to_string(),
            name: "Main Act".to_string(),
            sub_role: String::new(),
        }],
    );
    participants.insert(
        Role::from("artist"),
        vec![Participant {
            id: "ar-main".to_string(),
            name: "Main Act".to_string(),
            sub_role: "Vocals".to_string(),
        }],
    );
    participants.insert(
        Role::from("composer"),
        vec![Participant {
            id: "ar-second".to_string(),
            name: "Second Act".to_string(),
            sub_role: String::new(),
        }],
    );
    participants
}

/// The `NamedParticipant` FTS view of the SAME participants the JSON and the junction rows
/// come from (the S-D2b invariant). Sort names are deliberately empty — the column stores
/// only `{id, name, subRole}`, which is the documented divergence.
fn fixture_participant_names(participants: &Participants) -> Vec<NamedParticipant> {
    participant_names_from_participants(participants)
}

/// The fixture's `tags`: `genre` is the ONLY indexed name (`sql_tags::INDEXED_TAG_NAMES`),
/// `subtitle` feeds `full_title` when `append_subtitle` is on, and `albumversion` is a
/// media-file JSON-only name (Go's album-version tag is indexed for albums only).
fn fixture_tags() -> DbTags {
    let mut tags = DbTags::new();
    tags.insert(
        "genre".to_string(),
        vec!["Rock".to_string(), "Jazz".to_string()],
    );
    tags.insert("albumversion".to_string(), vec!["Deluxe".to_string()]);
    tags.insert("subtitle".to_string(), vec!["Live Version".to_string()]);
    tags
}

/// The fixture record. Every persisted field carries a value that is UNIQUE among the
/// columns (`runtime_album_writes.rs:581` precedent), so a rotated or swapped bind list
/// cannot accidentally satisfy another column's expectation.
fn mediafile_record(id: &str, library_id: i64, path: &str) -> MediaFileRecord {
    let participants = fixture_participants();
    let participant_names = fixture_participant_names(&participants);
    MediaFileRecord {
        id: id.to_string(),
        album: format!("Album-{id}"),
        album_artist: format!("AlArtist-{id}"),
        album_artist_id: format!("aa-{id}"),
        album_id: format!("al-{id}"),
        artist: format!("TrackArtist-{id}"),
        artist_id: format!("arid-{id}"),
        birth_time: birth_stamp(),
        bit_depth: Some(24),
        bit_rate: 1411,
        bpm: Some(120),
        catalog_num: format!("cat-{id}"),
        channels: 2,
        codec: format!("codec-{id}"),
        comment: format!("cmt-{id}"),
        compilation: false,
        created_at: Some(record_stamp()),
        date: format!("date-{id}"),
        disc_number: 3,
        disc_subtitle: format!("dsub-{id}"),
        duration: 456.75,
        explicit_status: format!("exp-{id}"),
        folder_id: format!("fold-{id}"),
        genre: format!("Rock-{id}"),
        has_cover_art: true,
        library_id,
        lyrics: format!("lyr-{id}"),
        mbz_recording_id: format!("mr-{id}"),
        mbz_release_track_id: format!("mrt-{id}"),
        mbz_album_id: format!("mal-{id}"),
        mbz_release_group_id: format!("mrg-{id}"),
        mbz_artist_id: format!("mar-{id}"),
        mbz_album_artist_id: format!("maa-{id}"),
        mbz_album_type: format!("mat-{id}"),
        mbz_album_comment: format!("mac-{id}"),
        missing: false,
        order_album_artist_name: format!("oaa-{id}"),
        order_album_name: format!("oal-{id}"),
        order_artist_name: format!("oar-{id}"),
        order_title: format!("oti-{id}"),
        original_date: format!("orig-{id}"),
        original_year: 1968,
        path: path.to_string(),
        pid: format!("pid-{id}"),
        probe_data: format!(r#"{{"codec":"flac-{id}"}}"#),
        release_date: format!("rel-{id}"),
        release_year: 1970,
        rg_album_gain: Some(0.5),
        rg_album_peak: Some(0.9),
        rg_track_gain: Some(-0.3),
        rg_track_peak: Some(0.7),
        sample_rate: 44_100,
        size: 12_345_678,
        sort_album_artist_name: format!("saa-{id}"),
        sort_album_name: format!("sal-{id}"),
        sort_artist_name: format!("sar-{id}"),
        sort_title: format!("sti-{id}"),
        suffix: format!("sfx-{id}"),
        tags: fixture_tags(),
        title: format!("Title-{id}"),
        track_number: 17,
        year: 1969,
        participants,
        participant_names,
        updated_at: Some(incoming_stamp()),
        library_path: String::new(),
        library_name: String::new(),
        annotation: nd_model::annotation::Annotation::default(),
        bookmark_position: 0,
        genres: Vec::new(),
        image: nd_db::artwork_hydration::ItemImage::default(),
        album_image: nd_db::artwork_hydration::ItemImage::default(),
    }
}

/// Commit one media-file Put through the typed seam exactly as a caller would: the identity,
/// the record, the caller's stamp, and the config-resolved subtitle switch. This seam takes
/// no column list (Go's `Put` passes no `colsToUpdate`).
fn put_committed(
    store: &SQLStore,
    identity: &MediaIdentity,
    record: &MediaFileRecord,
    stamp: &DateTime<Utc>,
    append_subtitle: bool,
) -> MediaFileWriteResult {
    store
        .try_with_tx_sync::<MediaFileWriteResult, MediaFileWriteError>(|tx| {
            put_mediafile_in_tx(tx, identity, record, stamp, append_subtitle)
        })
        .unwrap_or_else(|e| panic!("put_mediafile_in_tx({identity:?}) failed: {e}"))
}

/// `media_file_artists` rows of one track as a sorted `artist/role/subRole` set. `ON CONFLICT
/// DO NOTHING` over a `json_each` iteration has no row order in Go either, so sets are the
/// honest comparison.
fn artist_links(store: &SQLStore, media_file_id: &str) -> Vec<String> {
    let mut cells = lease(store, |conn| {
        let mut statement = conn
            .prepare(
                "SELECT artist_id, role, sub_role FROM media_file_artists WHERE media_file_id = ?",
            )
            .unwrap();
        let rows = statement
            .query_map([text(media_file_id)], |row| {
                Ok(format!(
                    "{}/{}/{}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?
                ))
            })
            .unwrap();
        rows.collect::<rusqlite::Result<Vec<String>>>().unwrap()
    });
    cells.sort();
    cells
}

/// `media_file_tags` rows of one track as a sorted tag-id set.
fn tag_links(store: &SQLStore, media_file_id: &str) -> Vec<String> {
    let mut cells = lease(store, |conn| {
        let mut statement = conn
            .prepare("SELECT tag_id FROM media_file_tags WHERE media_file_id = ?")
            .unwrap();
        let rows = statement
            .query_map([text(media_file_id)], |row| row.get::<_, String>(0))
            .unwrap();
        rows.collect::<rusqlite::Result<Vec<String>>>().unwrap()
    });
    cells.sort();
    cells
}

/// The contentless `media_file_fts` row count — trigger-maintenance volume only.
///
/// **This is NOT evidence that a re-index did or did not run.** A `('delete', rowid)` +
/// re-INSERT of the SAME document leaves `count(*)` at 1, so an unchanged count cannot
/// distinguish "the `WHEN` guard did not fire" from "the trigger fired and rewrote the same
/// docid". Only [`fts_index_fingerprint`] — the FTS5 shadow tables a real write appends
/// to — carries that distinction.
fn fts_count(store: &SQLStore) -> i64 {
    scalar(store, "SELECT count(*) FROM media_file_fts")
}

/// `MATCH` hit count for one query.
fn fts_match(store: &SQLStore, query: &str) -> i64 {
    lease(store, |conn| {
        conn.query_row(
            "SELECT count(*) FROM media_file_fts WHERE media_file_fts MATCH ?",
            [text(query)],
            |row| row.get(0),
        )
        .unwrap_or_else(|e| panic!("MATCH {query}: {e}"))
    })
}

/// The `media_file_fts` FTS5 **shadow-table** fingerprint: total bytes in
/// `media_file_fts_data`, term entries in `media_file_fts_idx`, and bytes in
/// `media_file_fts_docsize`.
///
/// This is the observable that distinguishes an executed index write from a skipped one on a
/// CONTENTLESS table, where the document's own text cannot be read back: every
/// `INSERT INTO media_file_fts(...)` — including the `('delete', rowid)` command the `_au`
/// trigger emits — appends a segment block and term entries, so a re-index of the SAME text
/// strictly GROWS these numbers. Verified against SQLite directly (a contentless FTS5 table
/// with the same `_ai`/`_ad`/`_au` shape): a guard-skipping UPDATE left all three values
/// byte-identical, while a hand-issued delete+re-INSERT of the identical text grew them.
///
/// The `_docsize` byte count is included because the `('delete', …)` command clears a docsize
/// entry and the re-INSERT rewrites it: it is stable across a no-op, and it makes the
/// fingerprint sensitive to a re-index even in the pathological case where segment bytes
/// happened to be merged away.
fn fts_index_fingerprint(store: &SQLStore) -> (i64, i64, i64) {
    lease(store, |conn| {
        let data: i64 = conn
            .query_row(
                "SELECT coalesce(sum(length(block)), -1) FROM media_file_fts_data",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let terms: i64 = conn
            .query_row("SELECT count(*) FROM media_file_fts_idx", [], |row| {
                row.get(0)
            })
            .unwrap();
        let docsize: i64 = conn
            .query_row(
                "SELECT coalesce(sum(length(sz)), -1) FROM media_file_fts_docsize",
                [],
                |row| row.get(0),
            )
            .unwrap();
        (data, terms, docsize)
    })
}

/// Rows of one `(path, library_id)` pair — the duplicate-row observable. `path` has no
/// unique index (`media_file_path` is a plain index), so this count is the only proof the
/// match leg ran. The id cell is read as a raw value so a deliberately undecodable row (see
/// `hard_match_read_error_is_not_tolerated`) can still be counted.
fn path_rows(store: &SQLStore, path: &str, library_id: i64) -> Vec<String> {
    let mut cells = lease(store, |conn| {
        let mut statement = conn
            .prepare("SELECT id FROM media_file WHERE library_id = ? AND path = ?")
            .unwrap();
        let rows = statement
            .query_map(rusqlite::params![library_id, path], |row| {
                row.get::<_, SqlValue>(0)
            })
            .unwrap();
        rows.collect::<rusqlite::Result<Vec<SqlValue>>>()
            .unwrap()
            .into_iter()
            .map(|cell| match cell {
                SqlValue::Text(value) => value,
                other => format!("{other:?}"),
            })
            .collect::<Vec<String>>()
    });
    cells.sort();
    cells
}

/// A whole-transaction snapshot of the observable state, for rollback equality.
#[derive(Debug, PartialEq, Eq)]
struct State {
    media: i64,
    artist_rows: i64,
    tag_rows: i64,
    fts: i64,
    /// The row's `title`, or `None` when the id matches nothing — the snapshot has to be
    /// readable for an id that was never written.
    cell: Option<String>,
}

fn state(store: &SQLStore, id: &str) -> State {
    State {
        media: scalar(store, "SELECT count(*) FROM media_file"),
        artist_rows: scalar(store, "SELECT count(*) FROM media_file_artists"),
        tag_rows: scalar(store, "SELECT count(*) FROM media_file_tags"),
        fts: fts_count(store),
        cell: lease(store, |conn| {
            conn.query_row(
                "SELECT title FROM media_file WHERE id = ?",
                [text(id)],
                |row| row.get::<_, String>(0),
            )
            .ok()
        }),
    }
}

/// The extended SQLite code, when the failure carries one — the assertion that the seam kept
/// the concrete cause rather than a stringified wrapper.
fn extended_code(err: &rusqlite::Error) -> Option<i32> {
    match err {
        SqliteError::SqliteFailure(code, _) => Some(code.extended_code),
        _ => None,
    }
}

/// Assert the executor's own shape: `Body` stage, the concrete retained extended code, and
/// no busy classification. `rusqlite::Error` is not `Clone`, so the classification helpers
/// take it by reference and nothing here hands one back.
fn assert_body_failure(err: &DbRuntimeError<MediaFileWriteError>, expected_extended: Option<i32>) {
    match err {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(
                *stage,
                TransactionStage::Body,
                "the executor owns the stage"
            );
            assert_eq!(
                extended_code(source),
                expected_extended,
                "the raw SQLite cause survives the seam unwrapped (Go's Put adds no fmt.Errorf, \
                 mediafile_repository.go:189-197): {source:?}"
            );
            assert!(
                !is_busy(source),
                "a constraint/trigger failure is not retried"
            );
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }
}

/// Assert the executor's own shape for a row-MAPPING failure on the probe: `Body` stage, the
/// concrete rusqlite mapping failure retained (not stringified), no busy classification. Go
/// maps a scan failure to a plain error too (`sql_base_repository.go:585-587` returns it), so
/// the arm carries a `rusqlite::Error` rather than a SQLite result code.
fn assert_probe_mapping_failure(err: &DbRuntimeError<MediaFileWriteError>) {
    match err {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(
                *stage,
                TransactionStage::Body,
                "the executor owns the stage"
            );
            assert!(
                matches!(
                    source,
                    SqliteError::InvalidColumnType(..) | SqliteError::FromSqlConversionFailure(..)
                ),
                "the concrete mapping cause survives the seam unwrapped: {source:?}"
            );
            assert!(!is_busy(source), "a mapping failure is not retried");
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }
}

/// Libraries 1 and 2, the artists the fixture participants reference, the dictionary tags the
/// fixture record's names need (indexed AND non-indexed), and one granted user per library.
fn seed_graph(store: &SQLStore) {
    seed_library(store, 1, "main");
    seed_library(store, 2, "decoy");
    seed_artist(store, "ar-main", 1);
    seed_artist(store, "ar-second", 1);
    seed_artist(store, "ar-third", 1);
    seed_tag(store, 1, "genre", "Rock");
    seed_tag(store, 1, "genre", "Jazz");
    // The NON-indexed names the fixture record carries, seeded through the PRODUCTION DB-06
    // write so their dictionary ids really exist. `media_only_genre_is_indexed` needs them
    // present in `tag`: a row absent from the dictionary is dropped by the INNER JOIN, which
    // is a different rule from the indexed-name filter.
    seed_tag(store, 1, "albumversion", "Deluxe");
    seed_tag(store, 1, "subtitle", "Live Version");
    seed_user(store, "u-one", 1);
    seed_user(store, "u-two", 2);
}

/// The junction set an untouched fixture track must have, spelled out so the rollback tests
/// cannot pass by the rows simply never having existed.
const ORIGINAL_ARTIST_LINKS: [&str; 3] = [
    "ar-main/albumartist/",
    "ar-main/artist/Vocals",
    "ar-second/composer/",
];

/// The `media_file_tags` set an untouched fixture track must have, spelled out for the same
/// reason (both genre values only).
fn original_tag_links() -> Vec<String> {
    vec![
        nd_id::new_tag_id("genre", "Jazz"),
        nd_id::new_tag_id("genre", "Rock"),
    ]
}

/// `fixture_participants` with one existing link REMOVED (`ar-second` as composer) and one
/// DISTINCT seeded dictionary artist ADDED (`ar-third` as conductor): the set differs from
/// [`ORIGINAL_ARTIST_LINKS`] in both directions, so a later-leg rollback cannot be confused
/// with a participant rewrite that silently never ran.
fn replaced_participants() -> Participants {
    let mut participants = Participants::new();
    participants.insert(
        Role::from("albumartist"),
        vec![Participant {
            id: "ar-main".to_string(),
            name: "Main Act".to_string(),
            sub_role: String::new(),
        }],
    );
    participants.insert(
        Role::from("conductor"),
        vec![Participant {
            id: "ar-third".to_string(),
            name: "Third Act".to_string(),
            sub_role: String::new(),
        }],
    );
    participants
}

/// The independently recomputed FTS triple for one record and subtitle switch, built from
/// the public feeder primitives rather than from the executed row. Recomputing it here — and
/// asserting the switch changes `full_text`/`search_normalized` but NOT `search_participants`
/// — is what makes the subtitle pin non-vacuous: an implementation that ignored the switch
/// would store the other arm's text, and an implementation that ignored the participant
/// names would fail the participant assertion.
fn expected_fts(
    record: &MediaFileRecord,
    append_subtitle: bool,
) -> nd_db::mediafile_repo::FtsColumns {
    let subtitle = record
        .tags
        .get("subtitle")
        .and_then(|values| values.first())
        .cloned()
        .unwrap_or_default();
    let title = nd_db::sql_feeders::full_title(&record.title, &subtitle, append_subtitle);
    let names = participant_names_from_participants(&record.participants)
        .iter()
        .map(|p| p.name.clone())
        .collect::<Vec<String>>();
    mediafile_fts_columns(
        &title,
        &record.album,
        &record.artist,
        &record.album_artist,
        &record.sort_title,
        &record.sort_album_name,
        &record.sort_artist_name,
        &record.sort_album_artist_name,
        &record.disc_subtitle,
        &names,
    )
}

// ---------------------------------------------------------------------------
// Identity: the `putByMatch` branches (`sql_base_repository.go:577-589`).
// ---------------------------------------------------------------------------

/// The whole point of DB-10: a row already exists at `(path, library)` with id `mf-old`, a
/// Put with `Match{path, library}` and a DIFFERENT `record.id` writes THAT row and reports
/// the FOUND id — Go's `m.ID = id` (`mediafile_repository.go:192`). Non-vacuity: omitting the
/// match SELECT would insert `mf-fresh` and give `count(*) = 2`, and mutating the caller's
/// record (Go does mutate `m.ID`) would fail the last assertion.
#[test]
fn put_by_match_reuses_path_library_identity() {
    let (db, path) = fresh_store("match-reuse");
    seed_graph(&db);
    seed_mediafile(&db, "mf-old", 1, "/music/a.mp3", "Old Title");
    let before = state(&db, "mf-old");
    assert_eq!(before.media, 1, "the pre-seeded row is really there");

    // `record.id` deliberately differs from the stored row's id.
    let record = mediafile_record("mf-fresh", 1, "/music/a.mp3");
    let result = put_committed(
        &db,
        &MediaIdentity::Match {
            path: "/music/a.mp3".to_string(),
            library_id: 1,
        },
        &record,
        &scan_stamp(),
        false,
    );

    assert_eq!(result.id, "mf-old", "Go's m.ID = id reuses the FOUND id");
    assert!(!result.inserted, "the UPDATE leg hit the found row");
    assert_eq!(
        path_rows(&db, "/music/a.mp3", 1),
        vec!["mf-old".to_string()],
        "no second row at the same path+library"
    );
    assert_eq!(
        count_rows(
            &db,
            "SELECT count(*) FROM media_file WHERE id = ?",
            &[text("mf-fresh")]
        ),
        0,
        "the record's own id never became a row"
    );
    assert_eq!(
        count_rows(
            &db,
            "SELECT count(*) FROM media_file_artists WHERE media_file_id = ?",
            &[text("mf-old")]
        ),
        3,
        "both junction legs are keyed to the FOUND id"
    );
    assert_eq!(artist_links(&db, "mf-old"), ORIGINAL_ARTIST_LINKS.to_vec());
    assert_eq!(tag_links(&db, "mf-old"), original_tag_links());
    assert_eq!(artist_links(&db, "mf-fresh"), Vec::<String>::new());
    assert_eq!(
        fts_count(&db),
        1,
        "the UPDATE re-indexed, it did not add a row"
    );
    assert_eq!(
        fts_match(&db, "\"Title-mf-fresh\""),
        1,
        "the new title is indexed"
    );
    assert_eq!(
        fts_match(&db, "\"Old Title\""),
        0,
        "the old entry was replaced"
    );

    // The caller's record is NOT mutated (Go mutates `m.ID`; the port reports the id).
    assert_eq!(record.id, "mf-fresh", "the input record keeps its own id");

    // The production read sees the rewritten row.
    let read = read_back(&db, "mf-old", "u-one");
    assert_eq!(read.title, "Title-mf-fresh");
    assert_eq!(read.id, "mf-old");

    cleanup(db, path);
}

/// The probe's `library_id` predicate is real (`put_match_sql`): the same path in two
/// libraries stays two rows, and a `Match` for library 1 never touches library 2's row.
#[test]
fn same_path_different_library_stays_distinct() {
    let (db, path_file) = fresh_store("two-libraries");
    seed_graph(&db);
    seed_mediafile(&db, "mf-lib1", 1, "/music/same.mp3", "Lib One Title");
    seed_mediafile(&db, "mf-lib2", 2, "/music/same.mp3", "Lib Two Title");

    let record = mediafile_record("mf-new", 1, "/music/same.mp3");
    let result = put_committed(
        &db,
        &MediaIdentity::Match {
            path: "/music/same.mp3".to_string(),
            library_id: 1,
        },
        &record,
        &scan_stamp(),
        false,
    );
    assert_eq!(result.id, "mf-lib1");
    assert!(!result.inserted);

    assert_eq!(
        path_rows(&db, "/music/same.mp3", 1),
        vec!["mf-lib1".to_string()],
        "library 1 holds exactly one row at that path"
    );
    assert_eq!(
        path_rows(&db, "/music/same.mp3", 2),
        vec!["mf-lib2".to_string()],
        "library 2 keeps its own row"
    );
    assert_cell(&db, "title", "mf-lib2", want_text("Lib Two Title"));
    assert_cell(&db, "title", "mf-lib1", want_text("Title-mf-new"));
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file"),
        2,
        "the Put added no row anywhere"
    );

    // The write is real but invisible to a user granted only the OTHER library: scoping is
    // the read side's job, and without the `user_library` grant the filter would be vacuous.
    assert!(matches!(
        read_attempt(&db, "mf-lib1", "u-two"),
        Err(MediaFileError::NotFound)
    ));
    assert_eq!(read_back(&db, "mf-lib2", "u-two").title, "Lib Two Title");

    cleanup(db, path_file);
}

/// `Id(id)` bypasses the probe (`sql_base_repository.go:578-580`): an unrelated row holding
/// the SAME `path`+`library_id` must stay untouched, and the supplied id is inserted with
/// `inserted: true`. Non-vacuity: an implementation that always ran the probe would land on
/// `mf-other` and leave `mf-x` absent.
#[test]
fn explicit_id_bypasses_match() {
    let (db, path) = fresh_store("explicit-id");
    seed_graph(&db);
    seed_mediafile(&db, "mf-other", 1, "/music/a.mp3", "Unrelated Row");

    let record = mediafile_record("mf-x", 1, "/music/a.mp3");
    let result = put_committed(
        &db,
        &MediaIdentity::Id("mf-x".to_string()),
        &record,
        &scan_stamp(),
        false,
    );
    assert_eq!(result.id, "mf-x");
    assert!(
        result.inserted,
        "a predefined id that misses is an INSERT, not a miss"
    );

    assert_eq!(
        path_rows(&db, "/music/a.mp3", 1),
        vec!["mf-other".to_string(), "mf-x".to_string()],
        "the colliding row is untouched, so the probe never ran"
    );
    assert_cell(&db, "title", "mf-other", want_text("Unrelated Row"));
    assert_cell(&db, "title", "mf-x", want_text("Title-mf-x"));
    assert_eq!(artist_links(&db, "mf-other"), Vec::<String>::new());
    assert_eq!(artist_links(&db, "mf-x"), ORIGINAL_ARTIST_LINKS.to_vec());
    assert_eq!(fts_count(&db), 2);

    // An explicit id that DOES exist updates that row even though another row holds the path.
    let mut renamed = mediafile_record("mf-x", 1, "/music/renamed.mp3");
    renamed.title = "Renamed Explicit".to_string();
    let second = put_committed(
        &db,
        &MediaIdentity::Id("mf-x".to_string()),
        &renamed,
        &later_stamp(),
        false,
    );
    assert_eq!(second.id, "mf-x");
    assert!(!second.inserted, "the UPDATE leg hit the explicit id");
    assert_cell(&db, "title", "mf-x", want_text("Renamed Explicit"));
    assert_cell(&db, "title", "mf-other", want_text("Unrelated Row"));

    cleanup(db, path);
}

/// The probe itself, executed on a transaction through the shared seam: it is the ONLY
/// identity source on the `Match` arm, its bind order is `[library_id, path]`, it is
/// case-sensitive (the column has no `COLLATE NOCASE`; only its `media_file_path_nocase`
/// index does), and the empty path is a REAL stored value that matches a stored `''` row.
#[test]
fn match_probe_is_library_scoped_case_sensitive_and_accepts_empty_path() {
    let (db, path) = fresh_store("probe-shape");
    seed_graph(&db);
    seed_mediafile(&db, "mf-a", 1, "/Music/A.mp3", "Case A");
    seed_mediafile(&db, "mf-b", 1, "/music/a.mp3", "Case B");
    seed_mediafile(&db, "mf-blank", 1, "", "Empty Path");

    let probe = |p: &str, lib: i64| -> Option<String> {
        lease(&db, |conn| {
            query_optional(conn, &put_match_sql(p, lib), |row| row.get::<_, String>(0))
                .unwrap_or_else(|e| panic!("probe {p}/{lib}: {e}"))
        })
    };

    assert_eq!(probe("/Music/A.mp3", 1).as_deref(), Some("mf-a"));
    assert_eq!(probe("/music/a.mp3", 1).as_deref(), Some("mf-b"));
    assert_eq!(
        probe("/Music/A.mp3", 2),
        None,
        "the library predicate is part of the probe"
    );
    assert_eq!(
        probe("/missing.mp3", 1),
        None,
        "absence is None, not an error"
    );
    assert_eq!(
        probe("", 1).as_deref(),
        Some("mf-blank"),
        "path='' matches the stored '' row exactly as Go's Eq does"
    );

    // The same arm through the production API: an empty path claims the stored '' row.
    let record = mediafile_record("mf-blank-new", 1, "");
    let result = put_committed(
        &db,
        &MediaIdentity::Match {
            path: String::new(),
            library_id: 1,
        },
        &record,
        &scan_stamp(),
        false,
    );
    assert_eq!(
        result.id, "mf-blank",
        "rejection of an empty path is a caller policy"
    );
    assert_eq!(path_rows(&db, "", 1), vec!["mf-blank".to_string()]);

    cleanup(db, path);
}

/// Two rows CAN hold the same `(path, library_id)` (no unique index), and Go's `queryOne`
/// takes the FIRST row without erroring (`dbx@v1.12.0/query.go:259-272`). rusqlite's
/// `query_row` does the same (`row.rs:101-106`, `get_expected_row`), so the port keeps Go's
/// first-row semantics rather than reporting a multi-match — pinned, not assumed.
#[test]
fn duplicate_path_rows_take_the_first_match_like_go() {
    let (db, path) = fresh_store("duplicate-path");
    seed_graph(&db);
    seed_mediafile(&db, "mf-first", 1, "/dup.mp3", "First");
    seed_mediafile(&db, "mf-second", 1, "/dup.mp3", "Second");

    let record = mediafile_record("mf-new", 1, "/dup.mp3");
    let result = put_committed(
        &db,
        &MediaIdentity::Match {
            path: "/dup.mp3".to_string(),
            library_id: 1,
        },
        &record,
        &scan_stamp(),
        false,
    );
    assert_eq!(
        result.id, "mf-first",
        "the first row wins, and no error is invented for a multi-match"
    );
    assert!(!result.inserted);
    assert_cell(&db, "title", "mf-first", want_text("Title-mf-new"));
    assert_cell(&db, "title", "mf-second", want_text("Second"));

    cleanup(db, path);
}

/// Repeated Puts at the same `(path, library)` produce ONE row and a STABLE id, and the
/// junction sets stay exactly the fixture's (no orphans, no duplicates) — the subplan's
/// `retry_match_same_result_no_orphan_id`, run through the retrying executor as well. The
/// executor only retries BUSY failures (`store.rs:234-246`), so no busy condition is faked
/// here: the claim is idempotence of the identity, not a manufactured retry count.
#[test]
fn repeated_match_produces_one_row_and_a_stable_id() {
    let (db, path) = fresh_store("repeat-match");
    seed_graph(&db);

    let identity = MediaIdentity::Match {
        path: "/music/repeat.mp3".to_string(),
        library_id: 1,
    };
    let record = mediafile_record("mf-provided", 1, "/music/repeat.mp3");

    let first = put_committed(&db, &identity, &record, &scan_stamp(), false);
    assert!(first.inserted, "nothing matched the first Put");
    assert_eq!(
        first.id, "mf-provided",
        "the record supplies an id, so Go's generated-id arm is never reached here"
    );

    let mut second = record.clone();
    second.title = "Repeat Twice".to_string();
    second.participants = replaced_participants();
    second.participant_names = fixture_participant_names(&second.participants);
    let retry = db
        .try_with_tx_retry_sync::<MediaFileWriteResult, MediaFileWriteError>(|tx| {
            put_mediafile_in_tx(tx, &identity, &second, &later_stamp(), false)
        })
        .unwrap_or_else(|e| panic!("retrying Put failed: {e}"));
    assert_eq!(retry.id, "mf-provided", "same identity across attempts");
    assert!(
        !retry.inserted,
        "the second Put hit the row the first created"
    );

    assert_eq!(
        path_rows(&db, "/music/repeat.mp3", 1),
        vec!["mf-provided".to_string()],
        "one row, not one per attempt"
    );
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM media_file"), 1);
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file_artists"),
        2
    );
    assert_eq!(
        artist_links(&db, "mf-provided"),
        vec![
            "ar-main/albumartist/".to_string(),
            "ar-third/conductor/".to_string(),
        ]
    );
    assert_eq!(fts_count(&db), 1, "no orphan FTS entry per attempt");
    assert_eq!(record.id, "mf-provided", "the caller's record is unmutated");

    cleanup(db, path);
}

/// Go's random-id arm is REPRODUCED (`sql_base_repository.go:635-638`), not refused: a
/// `Match` that finds NOTHING and a record with an empty `id` mints `nd_id::new_random()`
/// inside the transaction, SKIPS the UPDATE leg (`:623`), and inserts under it. Non-vacuity:
/// nothing pre-exists at that path, so the row and its junction set can only have come from
/// the minted id; a second `Put` at the same identity must land on THAT row rather than mint
/// another, which is what keeps the retry story free of orphan ids.
#[test]
fn match_miss_with_empty_id_generates_one_persisted_id() {
    let (db, path) = fresh_store("generated-id");
    seed_graph(&db);

    let identity = MediaIdentity::Match {
        path: "/music/gen.mp3".to_string(),
        library_id: 1,
    };
    let record = mediafile_record("", 1, "/music/gen.mp3");
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file"),
        0,
        "no row exists anywhere, so the id below cannot come from a stored one"
    );

    let first = put_committed(&db, &identity, &record, &scan_stamp(), false);
    assert!(first.inserted, "a generated id has no row to UPDATE");
    assert!(!first.id.is_empty(), "the result carries the minted id");
    assert_eq!(
        first.id.len(),
        22,
        "Go's `id.Encode` renders 16 bytes as 22 base62 chars: {:?}",
        first.id
    );
    nd_id::decode(&first.id).unwrap_or_else(|e| panic!("the minted id is not decodable: {e}"));

    // The row exists under the minted id, and BOTH junction legs are keyed to it — Go's
    // `values["id"] = id` (`:637`) plus `m.ID = id` feeding `updateParticipants`/`updateTags`.
    assert_eq!(
        path_rows(&db, "/music/gen.mp3", 1),
        vec![first.id.clone()],
        "the row is keyed by the minted id"
    );
    assert_eq!(artist_links(&db, &first.id), ORIGINAL_ARTIST_LINKS.to_vec());
    assert_eq!(tag_links(&db, &first.id), original_tag_links());
    assert_eq!(fts_count(&db), 1, "the `_ai` trigger indexed the new row");
    assert_eq!(fts_match(&db, "\"Title-\""), 1, "the new row is searchable");
    let read = read_back(&db, &first.id, "u-one");
    assert_eq!(read.id, first.id, "the production read sees the minted id");
    assert_eq!(read.created_at, Some(record_stamp()));

    // The caller's record is still unmutated (Go writes `m.ID`; the port reports it).
    assert_eq!(record.id, "", "the input record keeps its empty id");

    // A repeat `Put` at the same identity FINDS that row: the found id wins over the record's
    // empty id, so no second id is minted and no second row appears.
    let mut again = record.clone();
    again.title = "Generated Twice".to_string();
    let second = db
        .try_with_tx_retry_sync::<MediaFileWriteResult, MediaFileWriteError>(|tx| {
            put_mediafile_in_tx(tx, &identity, &again, &later_stamp(), false)
        })
        .unwrap_or_else(|e| panic!("retrying the generated-id Put failed: {e}"));
    assert_eq!(
        second.id, first.id,
        "the FOUND id wins, so the retry reuses the committed identity"
    );
    assert!(
        !second.inserted,
        "the UPDATE leg hit the row the first Put created"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file"),
        1,
        "no orphan id accumulated across attempts"
    );
    assert_eq!(
        path_rows(&db, "/music/gen.mp3", 1),
        vec![first.id.clone()],
        "one row at that path+library"
    );
    assert_eq!(fts_count(&db), 1);
    assert_eq!(fts_match(&db, "\"Generated Twice\""), 1);

    cleanup(db, path);
}

/// `Id("")` is an arm Go cannot express — its `Put` always builds the `Eq{path, library_id}`
/// filter, so an empty `m.ID` falls through to the probe. The port keeps `Id` meaning "no
/// probe" and generates (`sql_base_repository.go:635-638`), which is DOCUMENTED here. The
/// non-vacuity hook is a real row holding the SAME `path`+`library_id`: an implementation that
/// ran the probe would land on `mf-other` and leave the minted id absent.
#[test]
fn empty_id_on_the_id_arm_generates_without_running_the_probe() {
    let (db, path) = fresh_store("generated-id-no-probe");
    seed_graph(&db);
    seed_mediafile(&db, "mf-other", 1, "/music/a.mp3", "Unrelated Row");
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file"),
        1,
        "the colliding row exists BEFORE the API under test runs"
    );

    let record = mediafile_record("", 1, "/music/a.mp3");
    let result = put_committed(
        &db,
        &MediaIdentity::Id(String::new()),
        &record,
        &scan_stamp(),
        false,
    );
    assert!(result.inserted);
    assert!(!result.id.is_empty() && result.id != "mf-other");
    nd_id::decode(&result.id).unwrap_or_else(|e| panic!("the minted id is not decodable: {e}"));

    // The colliding row was NOT claimed: its own cells and junction rows are unchanged, and
    // its id is still the only row at that path other than the minted one. `state` is NOT
    // usable here — its counts are global and the new row legitimately changes them.
    assert_cell(&db, "title", "mf-other", want_text("Unrelated Row"));
    // The hand-seeded row's cells are untouched.
    assert_cell(
        &db,
        "updated_at",
        "mf-other",
        Want::Text(time::to_text(&record_stamp())),
    );
    assert_cell(
        &db,
        "created_at",
        "mf-other",
        Want::Text(time::to_text(&record_stamp())),
    );
    assert_eq!(
        artist_links(&db, "mf-other"),
        Vec::<String>::new(),
        "no junction leg was keyed to the colliding row"
    );
    assert_eq!(tag_links(&db, "mf-other"), Vec::<String>::new());
    assert_eq!(
        path_rows(&db, "/music/a.mp3", 1).len(),
        2,
        "two rows at one path, because `path` has no unique index and no probe was run"
    );
    assert_eq!(
        artist_links(&db, &result.id),
        ORIGINAL_ARTIST_LINKS.to_vec()
    );
    assert_eq!(artist_links(&db, "mf-other"), Vec::<String>::new());
    assert_eq!(fts_count(&db), 2, "both rows are indexed");

    cleanup(db, path);
}

/// A `Match` that FINDS a row never reaches the generated-id arm, even with an empty
/// `record.id`: the found id is authoritative (Go's `m.ID = id`, `mediafile_repository.go:192`).
#[test]
fn a_found_id_is_authoritative_over_an_empty_record_id() {
    let (db, path) = fresh_store("found-id-wins");
    seed_graph(&db);
    seed_mediafile(&db, "mf-found", 1, "/music/found.mp3", "Found");

    let found = put_committed(
        &db,
        &MediaIdentity::Match {
            path: "/music/found.mp3".to_string(),
            library_id: 1,
        },
        &mediafile_record("", 1, "/music/found.mp3"),
        &scan_stamp(),
        false,
    );
    assert_eq!(found.id, "mf-found", "a found id satisfies the write");
    assert!(!found.inserted);
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file"),
        1,
        "no second row was minted"
    );
    assert_cell(&db, "title", "mf-found", want_text("Title-"));
    assert_eq!(
        artist_links(&db, "mf-found"),
        ORIGINAL_ARTIST_LINKS.to_vec()
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file_artists"),
        3,
        "both junction legs keyed to the FOUND id"
    );

    cleanup(db, path);
}

/// A HARD probe failure is NOT tolerated (`sql_base_repository.go:585-587` returns the error
/// rather than falling through to `put`). A row whose `id` cell is a BLOB makes the mapping
/// fail — rusqlite's `String::column` rejects a non-TEXT cell (`from_sql.rs:172-176`), so the
/// `Match` arm surfaces a Body-stage SQLite failure and writes nothing at all.
#[test]
fn hard_match_read_error_is_not_tolerated() {
    let (db, path) = fresh_store("probe-error");
    seed_graph(&db);
    fixture(
        &db,
        "INSERT INTO media_file (id, library_id, path, title, tags) VALUES (?, ?, ?, ?, ?)",
        &[
            SqlValue::Blob(vec![0x00, 0xff]),
            integer(1),
            text("/bad.mp3"),
            text("Undecodable Id"),
            text("{}"),
        ],
    );
    let before = state(&db, "mf-writer");
    assert_eq!(before.media, 1, "the undecodable row is really there");

    let record = mediafile_record("mf-writer", 1, "/bad.mp3");
    let failed = db.try_with_tx_sync::<MediaFileWriteResult, MediaFileWriteError>(|tx| {
        put_mediafile_in_tx(
            tx,
            &MediaIdentity::Match {
                path: "/bad.mp3".to_string(),
                library_id: 1,
            },
            &record,
            &scan_stamp(),
            false,
        )
    });
    let err = failed.expect_err("a decode failure on the probe is a hard error, not absence");
    assert_probe_mapping_failure(&err);
    assert_eq!(
        state(&db, "mf-writer"),
        before,
        "no insert fell through the tolerated-absence path"
    );
    assert_eq!(path_rows(&db, "/bad.mp3", 1).len(), 1);
    assert_eq!(
        fts_count(&db),
        1,
        "the undecodable row's index entry is untouched"
    );
    lease(&db, |conn| {
        assert!(conn.is_autocommit(), "the lease came back reusable")
    });

    cleanup(db, path);
}

// ---------------------------------------------------------------------------
// Timestamps: the `created_at` stamp and the UPDATE-set drop list.
// ---------------------------------------------------------------------------

/// `created_at`: the record's value when `Some` and a real timestamp, the caller's `now`
/// when `IsZero()` — which in Rust means `None` OR an explicit `Some(time::zero())`, since
/// Go's `CreatedAt.IsZero()` is a VALUE test on the year-one time
/// (`mediafile_repository.go:185-187`) and its decoder can hand that value back — and the
/// stamp is the CALLER's clock rather than the commit clock (`scan_stamp` is years in the
/// past). On the UPDATE arm `created_at` and `birth_time` are dropped
/// (`sql_base_repository.go:605-608`) while `updated_at` passes through the record — the
/// asymmetry is what makes "immutable columns survived" a proof rather than a coincidence.
#[test]
fn created_at_uses_record_value_or_caller_now_and_immutable_columns_survive() {
    let (db, path) = fresh_store("timestamps");
    seed_graph(&db);

    // Insert arm, `created_at: None` → the caller's stamp, byte-for-byte.
    let mut fresh = mediafile_record("mf-zero", 1, "/music/zero.mp3");
    fresh.created_at = None;
    assert_eq!(
        count_rows(
            &db,
            "SELECT count(*) FROM media_file WHERE id = ?",
            &[text("mf-zero")]
        ),
        0,
        "the row does not exist yet, so the stamp below cannot come from a stored value"
    );
    let result = put_committed(
        &db,
        &MediaIdentity::Id("mf-zero".to_string()),
        &fresh,
        &scan_stamp(),
        false,
    );
    assert!(result.inserted);
    assert_cell(
        &db,
        "created_at",
        "mf-zero",
        Want::Text(time::to_text(&scan_stamp())),
    );
    assert_cell(
        &db,
        "birth_time",
        "mf-zero",
        Want::Text(time::to_text(&birth_stamp())),
    );
    assert_cell(
        &db,
        "updated_at",
        "mf-zero",
        Want::Text(time::to_text(&incoming_stamp())),
    );

    // Insert arm, `created_at: Some` → preserved, NOT replaced by `now`.
    let set = mediafile_record("mf-set", 1, "/music/set.mp3");
    put_committed(
        &db,
        &MediaIdentity::Id("mf-set".to_string()),
        &set,
        &scan_stamp(),
        false,
    );
    assert_cell(
        &db,
        "created_at",
        "mf-set",
        Want::Text(time::to_text(&record_stamp())),
    );

    // Update arm: newer `now`, `created_at`, `updated_at`, and `birth_time` inputs.
    // `created_at` and `birth_time` must not overwrite insert-time values; `updated_at` may.
    let mut second = mediafile_record("mf-zero", 1, "/music/zero.mp3");
    second.created_at = Some(later_stamp());
    second.updated_at = Some(later_stamp());
    second.birth_time = later_stamp();
    second.title = "Stamped Once".to_string();
    let updated = put_committed(
        &db,
        &MediaIdentity::Id("mf-zero".to_string()),
        &second,
        &later_stamp(),
        false,
    );
    assert!(!updated.inserted);
    assert_cell(
        &db,
        "created_at",
        "mf-zero",
        Want::Text(time::to_text(&scan_stamp())),
    );
    assert_cell(
        &db,
        "birth_time",
        "mf-zero",
        Want::Text(time::to_text(&birth_stamp())),
    );
    assert_cell(
        &db,
        "updated_at",
        "mf-zero",
        Want::Text(time::to_text(&later_stamp())),
    );
    assert_cell(&db, "title", "mf-zero", want_text("Stamped Once"));

    cleanup(db, path);
}

/// The SECOND zero representation: an explicit Go zero `time.Time` — `Some(time::zero())`,
/// what a decoder of a Go-written zero timestamp produces — is `IsZero()` TRUE in Go, so the
/// production `Put` boundary must stamp it to the caller's `now` exactly like `None`. A port
/// that only handled `None` (e.g. `unwrap_or(now)`) stores year-one TEXT here, and because
/// that text sorts BEFORE every real timestamp it silently breaks
/// `Gt{"created_at": since}` (`mediafile_repository.go:533,559`) and `ORDER BY created_at`
/// (`crate::time`'s header) — so this is an executed real-schema proof, not a renderer claim.
///
/// Non-vacuity: `at(1, 1, 1, 0, 0, 0)` and `time::zero()` are the same instant, so the
/// "stamped" assertion cannot be satisfied by the input value; the row is also read back
/// through the production read, and a NEAR-zero-but-real timestamp in the same test proves
/// the guard is `IsZero()` rather than "anything early".
#[test]
fn explicit_go_zero_created_at_is_stamped_to_caller_now() {
    let (db, path) = fresh_store("go-zero-created");
    seed_graph(&db);

    // Insert arm: `Some(time::zero())` is Go's zero value, so `now` wins.
    let mut zero_value = mediafile_record("mf-gozero", 1, "/music/gozero.mp3");
    zero_value.created_at = Some(time::zero());
    assert_eq!(
        count_rows(
            &db,
            "SELECT count(*) FROM media_file WHERE id = ?",
            &[text("mf-gozero")]
        ),
        0,
        "the row does not exist yet, so the stamp below cannot come from a stored value"
    );
    let result = put_committed(
        &db,
        &MediaIdentity::Id("mf-gozero".to_string()),
        &zero_value,
        &scan_stamp(),
        false,
    );
    assert!(result.inserted);
    assert_cell(
        &db,
        "created_at",
        "mf-gozero",
        Want::Text(time::to_text(&scan_stamp())),
    );
    // The year-one text must NOT be in the cell at all: a port that stored it would make
    // this count zero rather than one.
    assert_eq!(
        count_rows(
            &db,
            "SELECT count(*) FROM media_file WHERE id = ? AND created_at = ?",
            &[text("mf-gozero"), text(&time::to_text(&time::zero()))]
        ),
        0,
        "the explicit Go zero value was not stored"
    );
    assert_eq!(time::to_text(&time::zero()), "0001-01-01 00:00:00+00:00");
    // The production read decodes the stamped TEXT back, not NULL and not year-one.
    let read = read_back(&db, "mf-gozero", "u-one");
    assert_eq!(read.created_at, Some(scan_stamp()));

    // UPDATE arm: the drop list keeps the stored stamp even when the INPUT is the Go zero
    // value, so `created_at` stays immutable across the UPDATE leg.
    let mut again = mediafile_record("mf-gozero", 1, "/music/gozero.mp3");
    again.created_at = Some(time::zero());
    again.updated_at = Some(later_stamp());
    again.title = "Zero Value Twice".to_string();
    let updated = put_committed(
        &db,
        &MediaIdentity::Id("mf-gozero".to_string()),
        &again,
        &later_stamp(),
        false,
    );
    assert!(!updated.inserted, "the UPDATE leg hit the existing row");
    assert_cell(
        &db,
        "created_at",
        "mf-gozero",
        Want::Text(time::to_text(&scan_stamp())),
    );
    assert_cell(
        &db,
        "updated_at",
        "mf-gozero",
        Want::Text(time::to_text(&later_stamp())),
    );

    // A REAL timestamp that merely happens to be early is NOT zero, so it passes through
    // untouched — the guard is `IsZero()`, not "older than some threshold".
    let mut near_zero = mediafile_record("mf-nearzero", 1, "/music/nearzero.mp3");
    near_zero.created_at = Some(at(1, 1, 2, 0, 0, 0));
    put_committed(
        &db,
        &MediaIdentity::Id("mf-nearzero".to_string()),
        &near_zero,
        &scan_stamp(),
        false,
    );
    assert_cell(
        &db,
        "created_at",
        "mf-nearzero",
        Want::Text(time::to_text(&at(1, 1, 2, 0, 0, 0))),
    );

    cleanup(db, path);
}

/// The `media_file_fts_au` `WHEN` guard list (`nd-dbschema/src/schema_ddl.rs:84`) covers
/// title/album/artist/sort_*/disc_subtitle/search_participants/search_normalized. A Put that
/// changes ONLY `year`/`original_year` touches none of them, so it re-indexes nothing.
///
/// **Why the evidence is a fingerprint and not a count.** `media_file_fts` is `content=''`,
/// so the indexed text is not readable back, and `count(*)` stays 1 whether the trigger
/// skipped or fired and rewrote the same docid — an unchanged count proves NOTHING about
/// execution. This test therefore compares [`fts_index_fingerprint`] (the FTS5 shadow tables,
/// which every `INSERT INTO media_file_fts(...)` appends to) and PAIRS the negative
/// observation with a positive control in the same DB: a change the guard list DOES cover grows
/// the fingerprint. Without the control, an index that never got written would satisfy the
/// unchanged-fingerprint assertion exactly as an unguarded hand-written re-index would.
///
/// Non-vacuity of the negative leg itself: the record is re-sent with `year` changed TWICE
/// (and `updated_at`, which is not in the list), so "it grew once and stopped" cannot
/// masquerade as guard behavior, and the same Put with a covered column changed is the
/// counter-example. `tags` deliberately stays IDENTICAL here — see
/// `media_tag_change_does_reindex_because_it_feeds_search_normalized`: a tag change is NOT a
/// no-op, because `subtitle` feeds `full_title` and therefore the covered `search_normalized`.
#[test]
fn media_year_only_update_reindexes_nothing() {
    let (db, path) = fresh_store("fts-guard");
    seed_graph(&db);
    let record = mediafile_record("mf-fts", 1, "/music/fts.mp3");
    put_committed(
        &db,
        &MediaIdentity::Id("mf-fts".to_string()),
        &record,
        &scan_stamp(),
        false,
    );
    assert_eq!(fts_count(&db), 1);
    assert_eq!(fts_match(&db, "\"Title-mf-fts\""), 1);
    let after_insert = fts_index_fingerprint(&db);
    assert!(
        after_insert.0 > 0 && after_insert.1 > 0 && after_insert.2 > 0,
        "the insert's own `_ai` trigger wrote an index: {after_insert:?}"
    );

    // POSITIVE CONTROL: a change the guard list DOES cover must grow the fingerprint. This is
    // what makes the unchanged fingerprint below a statement about the guard rather than about
    // an index that never gets written.
    let mut guarded = mediafile_record("mf-fts", 1, "/music/fts.mp3");
    guarded.title = "Guarded Title Changed".to_string();
    put_committed(
        &db,
        &MediaIdentity::Id("mf-fts".to_string()),
        &guarded,
        &scan_stamp(),
        false,
    );
    let after_guarded = fts_index_fingerprint(&db);
    assert!(
        after_guarded.0 > after_insert.0 && after_guarded.1 > after_insert.1,
        "the `_au` trigger really ran on a covered column: {after_insert:?} -> {after_guarded:?}"
    );
    assert_eq!(fts_count(&db), 1, "one entry per row, re-indexed in place");
    assert_eq!(fts_match(&db, "\"Guarded Title Changed\""), 1);
    assert_eq!(fts_match(&db, "\"Title-mf-fts\""), 0, "old entry replaced");

    // NEGATIVE: `year`/`original_year` only — every covered column (including the derived
    // `search_normalized`, which the unchanged `tags` feed) is byte-identical, so the
    // fingerprint must be EXACTLY the previous one, not merely the same row count.
    let mut year_only = mediafile_record("mf-fts", 1, "/music/fts.mp3");
    year_only.title = "Guarded Title Changed".to_string();
    year_only.year = 2024;
    year_only.original_year = 2023;
    year_only.updated_at = Some(incoming_stamp());
    put_committed(
        &db,
        &MediaIdentity::Id("mf-fts".to_string()),
        &year_only,
        &later_stamp(),
        false,
    );

    assert_eq!(
        fts_index_fingerprint(&db),
        after_guarded,
        "the guard list did not fire: no segment byte, term entry, or docsize byte was added"
    );
    assert_eq!(fts_count(&db), 1);
    assert_eq!(
        fts_match(&db, "\"Guarded Title Changed\""),
        1,
        "the entry is still indexed"
    );
    assert_cell(&db, "year", "mf-fts", Want::Int(2024));
    assert_cell(
        &db,
        "updated_at",
        "mf-fts",
        Want::Text(time::to_text(&incoming_stamp())),
    );

    // A SECOND uncovered change, so a one-time growth followed by stability is ruled out.
    let mut year_again = year_only.clone();
    year_again.year = 2025;
    year_again.original_year = 2024;
    year_again.updated_at = Some(later_stamp());
    put_committed(
        &db,
        &MediaIdentity::Id("mf-fts".to_string()),
        &year_again,
        &later_stamp(),
        false,
    );
    assert_eq!(
        fts_index_fingerprint(&db),
        after_guarded,
        "still no index write on a second uncovered change"
    );
    assert_cell(&db, "year", "mf-fts", Want::Int(2025));
    assert_eq!(
        tag_links(&db, "mf-fts"),
        original_tag_links(),
        "the junction legs ran on both puts, which is the point: junction writes touch no FTS \
         trigger"
    );

    cleanup(db, path);
}

/// The other side of the guard, and the correction of a tempting over-claim. `tags` is NOT in
/// `media_file_fts_au`'s `WHEN` list (`nd-dbschema/src/schema_ddl.rs:84`), so whether a tag
/// change re-indexes depends entirely on whether it changes a COVERED column: the record's
/// `subtitle` tag feeds `full_title` (`full_title`/Go's `FullTitle()`) and therefore the
/// covered `full_text`/`search_normalized` ONLY when `append_subtitle` is on (the
/// config-resolved `ScrobbleAppendSubtitle`-style switch, `mediafile_repository.go:161-172`).
/// Both directions are pinned here, so "the triggers own the index" stays honest:
/// * subtitle OFF: clearing tags changes only the JSON column — the fingerprint is EXACTLY
///   unchanged, yet the junction DELETE still ran. A test that claimed "a tag change never
///   touches the index" would be wrong in the other arm; a claim that any re-index ran here
///   would be wrong in this one.
/// * subtitle ON: the same tag change moves `full_text` and `search_normalized`, both COVERED,
///   so the trigger fires and the fingerprint strictly grows while the entry count stays 1.
#[test]
fn media_tag_change_reindexes_only_through_the_subtitle_switch() {
    let (db, path) = fresh_store("fts-tags");
    seed_graph(&db);
    let record = mediafile_record("mf-tag", 1, "/music/tag.mp3");
    assert!(
        record.tags.contains_key("subtitle"),
        "the fixture record carries a subtitle tag"
    );
    put_committed(
        &db,
        &MediaIdentity::Id("mf-tag".to_string()),
        &record,
        &scan_stamp(),
        false,
    );
    assert_eq!(tag_links(&db, "mf-tag"), original_tag_links());
    let before = fts_index_fingerprint(&db);

    let mut no_tags = mediafile_record("mf-tag", 1, "/music/tag.mp3");
    no_tags.tags = DbTags::new();

    // subtitle OFF: `full_title` ignores the tags, so no covered column moves.
    put_committed(
        &db,
        &MediaIdentity::Id("mf-tag".to_string()),
        &no_tags,
        &later_stamp(),
        false,
    );
    let after_off = fts_index_fingerprint(&db);
    assert_eq!(
        after_off, before,
        "with the switch off the tag change moves no covered column: no index write at all"
    );
    assert_cell(&db, "tags", "mf-tag", want_text("{}"));
    assert_eq!(
        tag_links(&db, "mf-tag"),
        Vec::<String>::new(),
        "the tags DELETE ran even though the index did not move"
    );
    let want_off = expected_fts(&no_tags, false);
    assert_cell(&db, "full_text", "mf-tag", want_text(&want_off.full_text));
    assert_cell(
        &db,
        "search_normalized",
        "mf-tag",
        want_text(&want_off.search_normalized),
    );

    // subtitle ON: the same tag set now feeds `full_title`, so `full_text` and
    // `search_normalized` move and the COVERED-column guard fires. The fingerprint growing
    // while `count(*)` stays 1 is exactly the distinction a count cannot make.
    let mut with_subtitle = no_tags.clone();
    with_subtitle.tags = fixture_tags();
    put_committed(
        &db,
        &MediaIdentity::Id("mf-tag".to_string()),
        &with_subtitle,
        &later_stamp(),
        true,
    );
    let after_on = fts_index_fingerprint(&db);
    assert!(
        after_on.0 > after_off.0 && after_on.1 > after_off.1,
        "restoring the tags WITH the switch on changed the covered derived columns, so the \
         `_au` trigger ran: {after_off:?} -> {after_on:?}"
    );
    assert_eq!(fts_count(&db), 1, "re-indexed in place, not appended");
    let want_on = expected_fts(&with_subtitle, true);
    assert_cell(&db, "full_text", "mf-tag", want_text(&want_on.full_text));
    assert_cell(
        &db,
        "search_normalized",
        "mf-tag",
        want_text(&want_on.search_normalized),
    );
    assert_eq!(
        tag_links(&db, "mf-tag"),
        original_tag_links(),
        "the junction INSERT ran in the same transaction that re-indexed"
    );

    cleanup(db, path);
}

// ---------------------------------------------------------------------------
// Full-record insert, junction maintenance, and the FTS/junction asymmetry.
// ---------------------------------------------------------------------------

/// A full insert lands all 63 persisted columns cell-by-cell plus the three derived FTS
/// columns, the participants and tags junction sets, and exactly one `media_file_fts` entry —
/// the trigger's work, not a hand-written index row.
#[test]
fn insert_lands_full_record_junctions_and_fts() {
    let (db, path) = fresh_store("full-insert");
    seed_graph(&db);
    let record = mediafile_record("mf-full", 1, "/music/full.mp3");
    let result = put_committed(
        &db,
        &MediaIdentity::Id("mf-full".to_string()),
        &record,
        &scan_stamp(),
        false,
    );
    assert!(result.inserted);
    assert_eq!(result.id, "mf-full");

    assert_full_record(&db, "mf-full", "/music/full.mp3", 1);

    let fts = expected_fts(&record, false);
    assert_cell(
        &db,
        "full_text",
        "mf-full",
        Want::Text(fts.full_text.clone()),
    );
    assert_cell(
        &db,
        "search_participants",
        "mf-full",
        Want::Text(fts.search_participants.clone()),
    );
    assert_cell(
        &db,
        "search_normalized",
        "mf-full",
        Want::Text(fts.search_normalized.clone()),
    );
    // Deduplicated participant names, and the sub-role is NEVER a name source (Go's
    // `Participants.AllNames`).
    assert_eq!(
        fts.search_participants, "Main Act Second Act",
        "sub_role 'Vocals' does not reach the FTS participant text"
    );

    assert_eq!(artist_links(&db, "mf-full"), ORIGINAL_ARTIST_LINKS.to_vec());
    assert_eq!(tag_links(&db, "mf-full"), original_tag_links());
    assert_eq!(fts_count(&db), 1);
    assert_eq!(fts_match(&db, "\"Title-mf-full\""), 1);

    // The production read round-trips the typed record, including junction-backed tags.
    let read = read_back(&db, "mf-full", "u-one");
    assert_eq!(read.tags, fixture_tags());
    assert_eq!(read.participants, fixture_participants());
    assert_eq!(read.created_at, Some(record_stamp()));
    assert_eq!(read.birth_time, birth_stamp());

    cleanup(db, path);
}

/// `append_subtitle` is the config-resolved `FullTitle()` switch, supplied as a typed bool:
/// it changes `full_text` AND `search_normalized` (the title is a `search_normalized` input)
/// but never `search_participants`. Both arms are compared against an independently
/// recomputed FTS triple, so an implementation that ignored the switch cannot pass.
#[test]
fn subtitle_switch_changes_full_text_only_through_the_record() {
    let (db, path) = fresh_store("subtitle");
    seed_graph(&db);

    let record = mediafile_record("mf-sub", 1, "/music/sub.mp3");
    assert_eq!(
        full_title(&record, true),
        "Title-mf-sub (Live Version)",
        "the subtitle comes from the record's own tags"
    );

    let off = expected_fts(&record, false);
    let on = expected_fts(&record, true);
    assert_ne!(off.full_text, on.full_text, "the switch must be observable");
    assert_eq!(off.search_participants, on.search_participants);

    put_committed(
        &db,
        &MediaIdentity::Id("mf-sub".to_string()),
        &record,
        &scan_stamp(),
        false,
    );
    assert_cell(
        &db,
        "full_text",
        "mf-sub",
        Want::Text(off.full_text.clone()),
    );
    assert_cell(
        &db,
        "search_normalized",
        "mf-sub",
        Want::Text(off.search_normalized.clone()),
    );

    put_committed(
        &db,
        &MediaIdentity::Id("mf-sub".to_string()),
        &record,
        &later_stamp(),
        true,
    );
    assert_cell(&db, "full_text", "mf-sub", Want::Text(on.full_text.clone()));
    assert_cell(
        &db,
        "search_normalized",
        "mf-sub",
        Want::Text(on.search_normalized.clone()),
    );
    assert_cell(
        &db,
        "search_participants",
        "mf-sub",
        Want::Text(on.search_participants.clone()),
    );
    assert_eq!(fts_count(&db), 1, "the update re-indexed the same rowid");

    cleanup(db, path);
}

/// `None` binds NULL for the four RG columns and the two nullable ints, `Some` binds the
/// value, and `created_at: None` is stamped with the caller's `now` on insert.
#[test]
fn optional_numeric_columns_bind_null_and_some_binds_values() {
    let (db, path) = fresh_store("optional-columns");
    seed_graph(&db);

    let mut none = mediafile_record("mf-null", 1, "/music/nulls.mp3");
    none.rg_album_gain = None;
    none.rg_album_peak = None;
    none.rg_track_gain = None;
    none.rg_track_peak = None;
    none.bpm = None;
    none.bit_depth = None;
    none.created_at = None;
    none.updated_at = None;
    put_committed(
        &db,
        &MediaIdentity::Id("mf-null".to_string()),
        &none,
        &scan_stamp(),
        false,
    );
    for column in [
        "rg_album_gain",
        "rg_album_peak",
        "rg_track_gain",
        "rg_track_peak",
        "bpm",
        "bit_depth",
        "updated_at",
    ] {
        assert_cell(&db, column, "mf-null", Want::Null);
    }
    assert_cell(
        &db,
        "created_at",
        "mf-null",
        Want::Text(time::to_text(&scan_stamp())),
    );

    let mut some = mediafile_record("mf-null", 1, "/music/nulls.mp3");
    some.rg_album_gain = Some(-7.5);
    some.rg_track_peak = Some(1.0);
    some.rg_album_peak = None;
    some.bpm = Some(90);
    some.bit_depth = Some(16);
    put_committed(
        &db,
        &MediaIdentity::Id("mf-null".to_string()),
        &some,
        &later_stamp(),
        false,
    );
    assert_cell(&db, "rg_album_gain", "mf-null", Want::Real(-7.5));
    assert_cell(&db, "rg_track_peak", "mf-null", Want::Real(1.0));
    assert_cell(&db, "bpm", "mf-null", Want::Int(90));
    assert_cell(&db, "bit_depth", "mf-null", Want::Int(16));
    assert_cell(&db, "rg_album_peak", "mf-null", Want::Null);

    cleanup(db, path);
}

/// A re-Put replaces BOTH junction sets: stale role rows and stale genre rows are GONE, and
/// the new set is present. Non-vacuity: the pre-existing set is spelled out in
/// [`ORIGINAL_ARTIST_LINKS`], so "delete never ran" and "insert ran nothing" both fail.
#[test]
fn re_put_replaces_roles_and_tags_and_leaves_no_stale_rows() {
    let (db, path) = fresh_store("replace-sets");
    seed_graph(&db);
    let record = mediafile_record("mf-rep", 1, "/music/rep.mp3");
    put_committed(
        &db,
        &MediaIdentity::Id("mf-rep".to_string()),
        &record,
        &scan_stamp(),
        false,
    );
    assert_eq!(artist_links(&db, "mf-rep"), ORIGINAL_ARTIST_LINKS.to_vec());
    assert_eq!(tag_links(&db, "mf-rep"), original_tag_links());

    let mut second = mediafile_record("mf-rep", 1, "/music/rep.mp3");
    second.participants = replaced_participants();
    second.participant_names = fixture_participant_names(&second.participants);
    let mut tags = DbTags::new();
    tags.insert("genre".to_string(), vec!["Jazz".to_string()]);
    second.tags = tags;
    put_committed(
        &db,
        &MediaIdentity::Id("mf-rep".to_string()),
        &second,
        &later_stamp(),
        false,
    );

    assert_eq!(
        artist_links(&db, "mf-rep"),
        vec![
            "ar-main/albumartist/".to_string(),
            "ar-third/conductor/".to_string(),
        ],
        "the composer and the sub-role artist rows are gone"
    );
    assert_eq!(
        tag_links(&db, "mf-rep"),
        vec![nd_id::new_tag_id("genre", "Jazz")],
        "the stale genre row is gone"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file_artists"),
        2
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file_tags"),
        1
    );
    assert_cell(
        &db,
        "tags",
        "mf-rep",
        Want::Text(sql_tags::marshal_tags(second.tags.clone())),
    );

    cleanup(db, path);
}

/// Empty participants AND empty tags still run both DELETEs (Go returns only before building
/// the INSERT), so the junctions are emptied rather than left stale. The renderer's `Option`
/// legs are asserted gone too — with rusqlite's `hooks` feature unavailable in this manifest,
/// "no insert statement ran" is established from the `Option` legs plus the observable state,
/// which is recorded as a coverage gap rather than claimed as a statement-count proof.
#[test]
fn empty_participants_and_tags_clear_junctions_without_an_insert() {
    let (db, path) = fresh_store("empty-junctions");
    seed_graph(&db);
    let record = mediafile_record("mf-clear", 1, "/music/clear.mp3");
    put_committed(
        &db,
        &MediaIdentity::Id("mf-clear".to_string()),
        &record,
        &scan_stamp(),
        false,
    );
    assert_eq!(
        artist_links(&db, "mf-clear"),
        ORIGINAL_ARTIST_LINKS.to_vec()
    );
    assert_eq!(tag_links(&db, "mf-clear"), original_tag_links());

    // The renderer the executed wrapper uses: both insert legs are None.
    let bundle = put_sql("mf-clear", &record, scan_stamp(), false, &[]).expect("renders");
    let cleared_record = MediaFileRecord {
        participants: Participants::new(),
        participant_names: Vec::new(),
        tags: DbTags::new(),
        ..record.clone()
    };
    let cleared = put_sql("mf-clear", &cleared_record, scan_stamp(), false, &[]).expect("renders");
    assert!(
        cleared.participants_insert.is_none() && cleared.tags_insert.is_none(),
        "Go's early return skips only the INSERT legs"
    );
    assert!(bundle.participants_insert.is_some() && bundle.tags_insert.is_some());

    let result = put_committed(
        &db,
        &MediaIdentity::Id("mf-clear".to_string()),
        &cleared_record,
        &later_stamp(),
        false,
    );
    assert!(!result.inserted);
    assert_eq!(
        artist_links(&db, "mf-clear"),
        Vec::<String>::new(),
        "cleared"
    );
    assert_eq!(tag_links(&db, "mf-clear"), Vec::<String>::new(), "cleared");
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file_artists"),
        0
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file_tags"),
        0
    );
    assert_cell(&db, "participants", "mf-clear", want_text("{}"));
    assert_cell(&db, "tags", "mf-clear", want_text("{}"));
    assert_eq!(
        fts_count(&db),
        1,
        "the base row and its index entry survive"
    );

    cleanup(db, path);
}

/// Only `genre` is indexed (`sql_tags::INDEXED_TAG_NAMES`), so the record's non-genre names
/// stay JSON-only even though their dictionary rows EXIST (seeded through the production
/// DB-06 write) — the indexed-name filter, not a JOIN miss. Unknown ids in both sets are
/// dropped SILENTLY by the INNER JOINs while the JSON columns keep them: Go's asymmetry
/// (`sql_participations.go:117`, `sql_tags.go:69`), and `Put` never re-derives the JSON from
/// the junction result.
#[test]
fn only_genre_is_indexed_and_unknown_ids_are_dropped_by_the_join() {
    let (db, path) = fresh_store("join-filter");
    seed_graph(&db);

    let mut participants = fixture_participants();
    // APPEND the unknown id to the existing composer role: `insert` on the `BTreeMap` would
    // REPLACE the role's list and silently drop `ar-second`, making the JOIN-drop assertion
    // read as a missing known row instead.
    let mut composers = participants
        .get(&Role::from("composer"))
        .cloned()
        .unwrap_or_default();
    composers.push(Participant {
        id: "ar-ghost".to_string(),
        name: "Ghost Act".to_string(),
        sub_role: String::new(),
    });
    participants.insert(Role::from("composer"), composers);
    let mut tags = fixture_tags();
    tags.insert(
        "genre".to_string(),
        vec!["Rock".to_string(), "Ghost Genre".to_string()],
    );

    let record = MediaFileRecord {
        participants: participants.clone(),
        participant_names: fixture_participant_names(&participants),
        tags,
        ..mediafile_record("mf-ghost", 1, "/music/ghost.mp3")
    };
    put_committed(
        &db,
        &MediaIdentity::Id("mf-ghost".to_string()),
        &record,
        &scan_stamp(),
        false,
    );

    assert_eq!(
        artist_links(&db, "mf-ghost"),
        vec![
            "ar-main/albumartist/".to_string(),
            "ar-main/artist/Vocals".to_string(),
            "ar-second/composer/".to_string(),
        ],
        "ar-ghost is dropped by the INNER JOIN, and the known rows prove the insert ran"
    );
    assert_eq!(
        tag_links(&db, "mf-ghost"),
        vec![nd_id::new_tag_id("genre", "Rock")],
        "only the indexed genre survives; 'Ghost Genre' has no dictionary row, and the \
         seeded albumversion/subtitle rows are not indexed names"
    );
    // The JSON columns keep everything the record carried, junction result or not (Go's `Put`
    // never re-derives the JSON from the junction rows).
    assert_cell(
        &db,
        "participants",
        "mf-ghost",
        want_text(&nd_db::sql_participations::marshal_participants(
            &record.participants,
        )),
    );
    assert_cell(
        &db,
        "tags",
        "mf-ghost",
        want_text(&sql_tags::marshal_tags(record.tags.clone())),
    );
    assert!(
        lease(&db, |conn| conn
            .query_row(
                "SELECT participants FROM media_file WHERE id = 'mf-ghost'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap())
        .contains("ar-ghost"),
        "the participants JSON still lists the id the junction dropped"
    );
    assert!(
        lease(&db, |conn| conn
            .query_row(
                "SELECT tags FROM media_file WHERE id = 'mf-ghost'",
                [],
                |row| { row.get::<_, String>(0) }
            )
            .unwrap())
        .contains(&nd_id::new_tag_id("subtitle", "Live Version")),
        "the tags JSON carries the \
         non-indexed name's id"
    );
    assert_eq!(fts_count(&db), 1);

    cleanup(db, path);
}

/// Duplicate participant/tag entries within one Put collapse on the junction UNIQUE keys
/// (`artist_tracks`, `media_file_tags`) via `ON CONFLICT DO NOTHING` — no error, one row
/// each — while the JSON columns keep the duplicates Go's marshaller emits.
#[test]
fn duplicate_roles_and_tags_collapse_on_conflict_without_error() {
    let (db, path) = fresh_store("conflict-collapse");
    seed_graph(&db);

    let mut participants = Participants::new();
    participants.insert(
        Role::from("artist"),
        vec![
            Participant {
                id: "ar-main".to_string(),
                name: "Main Act".to_string(),
                sub_role: String::new(),
            },
            Participant {
                id: "ar-main".to_string(),
                name: "Main Act".to_string(),
                sub_role: String::new(),
            },
        ],
    );
    let mut tags = DbTags::new();
    tags.insert(
        "genre".to_string(),
        vec!["Rock".to_string(), "Rock".to_string()],
    );

    let record = MediaFileRecord {
        participants: participants.clone(),
        participant_names: fixture_participant_names(&participants),
        tags,
        ..mediafile_record("mf-dup", 1, "/music/dup.mp3")
    };
    let result = put_committed(
        &db,
        &MediaIdentity::Id("mf-dup".to_string()),
        &record,
        &scan_stamp(),
        false,
    );
    assert!(result.inserted);
    assert_eq!(
        artist_links(&db, "mf-dup"),
        vec!["ar-main/artist/".to_string()]
    );
    assert_eq!(
        tag_links(&db, "mf-dup"),
        vec![nd_id::new_tag_id("genre", "Rock")]
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file_artists"),
        1
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file_tags"),
        1
    );

    cleanup(db, path);
}

// ---------------------------------------------------------------------------
// Fault injection: every leg failure rolls the whole bundle back.
// ---------------------------------------------------------------------------

/// Poison the participants **DELETE** — the FIRST junction leg, which Go runs unconditionally
/// (`sql_participations.go:79-89`) before deciding whether an INSERT is needed. The abort
/// therefore proves the DELETE was actually attempted against the three pre-existing rows: an
/// implementation that skipped the delete would hit no `BEFORE DELETE` trigger at all and the
/// Put would succeed, so the failure itself is the non-vacuity evidence.
///
/// Rollback must restore the base UPDATE that already ran, leave the junction set intact, and
/// leave the tags legs untouched.
#[test]
fn participants_delete_failure_rolls_back_base_and_leaves_junctions_intact() {
    let (db, path) = fresh_store("participants-delete");
    seed_graph(&db);
    let record = mediafile_record("mf-old", 1, "/music/old.mp3");
    put_committed(
        &db,
        &MediaIdentity::Id("mf-old".to_string()),
        &record,
        &scan_stamp(),
        false,
    );
    let before = state(&db, "mf-old");
    assert_eq!(
        before.artist_rows, 3,
        "the rows the DELETE must touch exist"
    );
    assert_eq!(before.tag_rows, 2, "and so do the tag rows");
    let fingerprint = fts_index_fingerprint(&db);

    fixture(
        &db,
        "CREATE TRIGGER fixture_mf_participants_delete_fails BEFORE DELETE ON media_file_artists \
         FOR EACH ROW WHEN OLD.media_file_id = 'mf-old' \
         BEGIN SELECT RAISE(ABORT, 'fixture: participants delete leg fails'); END",
        &[],
    );

    // A rewrite that changes BOTH junction sets and a base column, so a rollback that only
    // half-restored something is visible.
    let mut updated = mediafile_record("mf-old", 1, "/music/old.mp3");
    updated.title = "Delete Never Finished".to_string();
    updated.participants = replaced_participants();
    updated.participant_names = fixture_participant_names(&updated.participants);
    updated.tags = DbTags::new();

    let failed = db.try_with_tx_sync::<MediaFileWriteResult, MediaFileWriteError>(|tx| {
        put_mediafile_in_tx(
            tx,
            &MediaIdentity::Id("mf-old".to_string()),
            &updated,
            &later_stamp(),
            false,
        )
    });
    let err = failed.expect_err("the participants DELETE aborts the transaction");
    assert_body_failure(&err, Some(ffi::SQLITE_CONSTRAINT_TRIGGER));

    assert_eq!(
        state(&db, "mf-old"),
        before,
        "base row and BOTH junction counts reverted — the UPDATE that already ran is undone"
    );
    // The base UPDATE's new title is not visible.
    assert_cell(&db, "title", "mf-old", want_text("Title-mf-old"));
    // Neither is its new `updated_at`: the UPDATE leg really ran and rolled back.
    assert_cell(
        &db,
        "updated_at",
        "mf-old",
        Want::Text(time::to_text(&incoming_stamp())),
    );
    assert_eq!(
        artist_links(&db, "mf-old"),
        ORIGINAL_ARTIST_LINKS.to_vec(),
        "the deleted role rows came back"
    );
    assert_eq!(tag_links(&db, "mf-old"), original_tag_links());
    assert_eq!(
        fts_index_fingerprint(&db),
        fingerprint,
        "the aborted transaction added no index write: the base UPDATE's `_au` work rolled back \
         with it"
    );
    assert_eq!(fts_match(&db, "\"Delete Never Finished\""), 0);
    assert_eq!(
        fts_match(&db, "\"Third Act\""),
        0,
        "no new participant text"
    );
    lease(&db, |conn| {
        assert!(conn.is_autocommit(), "the lease came back reusable")
    });

    // Same write with the trigger gone: it lands, so the abort was the only obstacle.
    fixture(
        &db,
        "DROP TRIGGER fixture_mf_participants_delete_fails",
        &[],
    );
    put_committed(
        &db,
        &MediaIdentity::Id("mf-old".to_string()),
        &updated,
        &later_stamp(),
        false,
    );
    assert_eq!(artist_links(&db, "mf-old").len(), 2, "stale roles cleared");
    assert_eq!(
        tag_links(&db, "mf-old"),
        Vec::<String>::new(),
        "tags cleared"
    );
    assert_cell(&db, "title", "mf-old", want_text("Delete Never Finished"));

    cleanup(db, path);
}

/// Poison the participants INSERT. Everything before it really ran — base UPDATE,
/// participants DELETE — so the rollback assertions can tell "rolled back" apart from "the
/// rewrite silently never happened". The tags legs never ran at all.
#[test]
fn participants_insert_failure_rolls_back_base_and_delete() {
    let (db, path) = fresh_store("participants-insert");
    seed_graph(&db);
    seed_artist(&db, "ar-poison", 1);
    let record = mediafile_record("mf-old", 1, "/music/old.mp3");
    put_committed(
        &db,
        &MediaIdentity::Id("mf-old".to_string()),
        &record,
        &scan_stamp(),
        false,
    );
    fixture(
        &db,
        "CREATE TRIGGER fixture_mf_participants_insert_fails BEFORE INSERT ON media_file_artists \
         FOR EACH ROW WHEN NEW.artist_id = 'ar-poison' \
         BEGIN SELECT RAISE(ABORT, 'fixture: participants insert leg fails'); END",
        &[],
    );

    let mut updated = mediafile_record("mf-old", 1, "/music/old.mp3");
    updated.title = "Poisoned Title".to_string();
    let mut participants = fixture_participants();
    participants.insert(
        Role::from("conductor"),
        vec![Participant {
            id: "ar-poison".to_string(),
            name: "Poison Act".to_string(),
            sub_role: String::new(),
        }],
    );
    updated.participants = participants.clone();
    updated.participant_names = fixture_participant_names(&participants);

    let failed = db.try_with_tx_sync::<MediaFileWriteResult, MediaFileWriteError>(|tx| {
        put_mediafile_in_tx(
            tx,
            &MediaIdentity::Id("mf-old".to_string()),
            &updated,
            &later_stamp(),
            false,
        )
    });
    let err = failed.expect_err("the participants INSERT aborts the transaction");
    assert_body_failure(&err, Some(ffi::SQLITE_CONSTRAINT_TRIGGER));

    assert_cell(&db, "title", "mf-old", want_text("Title-mf-old"));
    assert_cell(
        &db,
        "updated_at",
        "mf-old",
        Want::Text(time::to_text(&incoming_stamp())),
    );
    assert_eq!(
        artist_links(&db, "mf-old"),
        ORIGINAL_ARTIST_LINKS.to_vec(),
        "the DELETE that already ran rolled back with the failure"
    );
    assert_eq!(
        tag_links(&db, "mf-old"),
        original_tag_links(),
        "the tags legs never ran"
    );
    assert_eq!(fts_count(&db), 1);
    assert_eq!(
        fts_match(&db, "\"Second Act\""),
        1,
        "the OLD participant text is still indexed"
    );
    assert_eq!(
        fts_match(&db, "\"Poison Act\""),
        0,
        "nothing partial survived"
    );
    lease(&db, |conn| {
        assert!(conn.is_autocommit(), "the lease came back reusable")
    });

    // Same write with the trigger gone: it succeeds and the new set lands.
    fixture(
        &db,
        "DROP TRIGGER fixture_mf_participants_insert_fails",
        &[],
    );
    put_committed(
        &db,
        &MediaIdentity::Id("mf-old".to_string()),
        &updated,
        &later_stamp(),
        false,
    );
    assert_eq!(artist_links(&db, "mf-old").len(), 4);
    assert_eq!(fts_match(&db, "\"Poison Act\""), 1);

    cleanup(db, path);
}

/// Poison the tags DELETE — the last unconditional leg before the final insert. The trigger's
/// `WHEN` clause demands that the NEW conductor link already exists in this transaction, so
/// the abort can ONLY happen if the participants INSERT ran first: the later failure itself
/// proves Go's leg order (base → participants → tags).
#[test]
fn tags_delete_failure_rolls_back_base_and_participants() {
    let (db, path) = fresh_store("tags-delete");
    seed_graph(&db);
    let record = mediafile_record("mf-old", 1, "/music/old.mp3");
    put_committed(
        &db,
        &MediaIdentity::Id("mf-old".to_string()),
        &record,
        &scan_stamp(),
        false,
    );
    fixture(
        &db,
        "CREATE TRIGGER fixture_mf_tags_delete_fails BEFORE DELETE ON media_file_tags \
         FOR EACH ROW WHEN OLD.media_file_id = 'mf-old' AND EXISTS (\
             SELECT 1 FROM media_file_artists WHERE media_file_id = 'mf-old' \
               AND artist_id = 'ar-third' AND role = 'conductor') \
         BEGIN SELECT RAISE(ABORT, 'fixture: tags delete leg fails'); END",
        &[],
    );

    let mut updated = mediafile_record("mf-old", 1, "/music/old.mp3");
    updated.title = "Tags Never Finished".to_string();
    updated.participants = replaced_participants();
    updated.participant_names = fixture_participant_names(&updated.participants);

    let failed = db.try_with_tx_sync::<MediaFileWriteResult, MediaFileWriteError>(|tx| {
        put_mediafile_in_tx(
            tx,
            &MediaIdentity::Id("mf-old".to_string()),
            &updated,
            &later_stamp(),
            false,
        )
    });
    let err = failed.expect_err("the tags DELETE aborts the transaction");
    assert_body_failure(&err, Some(ffi::SQLITE_CONSTRAINT_TRIGGER));

    assert_eq!(
        state(&db, "mf-old"),
        State {
            media: 1,
            artist_rows: 3,
            tag_rows: 2,
            fts: 1,
            cell: Some("Title-mf-old".to_string()),
        },
        "base row, participants and tags all reverted"
    );
    assert_eq!(artist_links(&db, "mf-old"), ORIGINAL_ARTIST_LINKS.to_vec());
    assert_eq!(tag_links(&db, "mf-old"), original_tag_links());
    assert_eq!(fts_match(&db, "\"Third Act\""), 0);

    fixture(&db, "DROP TRIGGER fixture_mf_tags_delete_fails", &[]);
    put_committed(
        &db,
        &MediaIdentity::Id("mf-old".to_string()),
        &updated,
        &later_stamp(),
        false,
    );
    assert_eq!(artist_links(&db, "mf-old").len(), 2);
    assert_eq!(tag_links(&db, "mf-old"), original_tag_links());

    cleanup(db, path);
}

/// Poison the tags INSERT. The base write, both participants legs and the tags DELETE all
/// really ran, so the rollback has to restore the OLD tag set as well as the base row. The
/// pre-seeded tag rows are spelled out, so a silent no-op cannot pass.
#[test]
fn tags_insert_failure_rolls_back_base_participants_and_tags_delete() {
    let (db, path) = fresh_store("tags-insert");
    seed_graph(&db);
    let record = mediafile_record("mf-old", 1, "/music/old.mp3");
    put_committed(
        &db,
        &MediaIdentity::Id("mf-old".to_string()),
        &record,
        &scan_stamp(),
        false,
    );
    // SQLite cannot bind inside a trigger body, so the ids are spelled in the DDL. The WHEN
    // clause demands (a) the NEW conductor link already exists in this transaction — so the
    // abort can ONLY happen after the participants INSERT really ran — and (b) the named
    // dictionary row exists. Both are checked against tables the tags DELETE does not touch,
    // since by the time the tags INSERT runs its own DELETE has already emptied the junction.
    fixture(
        &db,
        "CREATE TRIGGER fixture_mf_tags_insert_fails BEFORE INSERT ON media_file_tags \
         FOR EACH ROW WHEN EXISTS (\
             SELECT 1 FROM media_file_artists WHERE media_file_id = 'mf-old' \
               AND artist_id = 'ar-third' AND role = 'conductor') \
           AND EXISTS (SELECT 1 FROM tag WHERE id = 'tag-original') \
         BEGIN SELECT RAISE(ABORT, 'fixture: tags insert leg fails'); END",
        &[],
    );
    // Name the pre-existing tag row the rollback must restore. The dictionary row is created
    // through the PRODUCTION DB-06 write with that explicit id, so the rename satisfies the
    // FK and the TRIGGER is what fails.
    seed_tag_with_id(&db, 1, "tag-original", "genre", "Original");
    fixture(
        &db,
        "UPDATE media_file_tags SET tag_id = 'tag-original' WHERE media_file_id = 'mf-old' AND \
         tag_id = ?",
        &[text(&nd_id::new_tag_id("genre", "Jazz"))],
    );
    assert_eq!(tag_links(&db, "mf-old"), {
        let mut ids = vec![
            "tag-original".to_string(),
            nd_id::new_tag_id("genre", "Rock"),
        ];
        ids.sort();
        ids
    });

    let mut updated = mediafile_record("mf-old", 1, "/music/old.mp3");
    updated.title = "Tags Poisoned".to_string();
    updated.participants = replaced_participants();
    updated.participant_names = fixture_participant_names(&updated.participants);

    let failed = db.try_with_tx_sync::<MediaFileWriteResult, MediaFileWriteError>(|tx| {
        put_mediafile_in_tx(
            tx,
            &MediaIdentity::Id("mf-old".to_string()),
            &updated,
            &later_stamp(),
            false,
        )
    });
    let err = failed.expect_err("the tags INSERT aborts the transaction");
    assert_body_failure(&err, Some(ffi::SQLITE_CONSTRAINT_TRIGGER));

    assert_cell(&db, "title", "mf-old", want_text("Title-mf-old"));
    assert_cell(
        &db,
        "created_at",
        "mf-old",
        Want::Text(time::to_text(&record_stamp())),
    );
    assert_eq!(
        artist_links(&db, "mf-old"),
        ORIGINAL_ARTIST_LINKS.to_vec(),
        "the participants INSERT rolled back"
    );
    assert_eq!(
        tag_links(&db, "mf-old"),
        {
            // `tag_links` compares sorted sets, so the expectation is sorted too.
            let mut ids = vec![
                "tag-original".to_string(),
                nd_id::new_tag_id("genre", "Rock"),
            ];
            ids.sort();
            ids
        },
        "the tags DELETE rolled back, so the pre-existing rows are back"
    );
    assert_eq!(fts_count(&db), 1);
    assert_eq!(fts_match(&db, "\"Title-mf-old\""), 1);

    fixture(&db, "DROP TRIGGER fixture_mf_tags_insert_fails", &[]);
    put_committed(
        &db,
        &MediaIdentity::Id("mf-old".to_string()),
        &updated,
        &later_stamp(),
        false,
    );
    assert_eq!(tag_links(&db, "mf-old"), original_tag_links());

    cleanup(db, path);
}

/// Poison the base INSERT leg: nothing lands at all — no base row, no junction rows, no FTS
/// entry — and the same write succeeds once the trigger is gone.
#[test]
fn base_insert_failure_rolls_back_base_junctions_and_fts() {
    let (db, path) = fresh_store("base-insert");
    seed_graph(&db);
    let record = mediafile_record("mf-old", 1, "/music/old.mp3");
    put_committed(
        &db,
        &MediaIdentity::Id("mf-old".to_string()),
        &record,
        &scan_stamp(),
        false,
    );
    let before = state(&db, "mf-old");
    // SQLite forbids bound variables inside a trigger body, so the id is spelled in the DDL.
    fixture(
        &db,
        "CREATE TRIGGER fixture_mf_insert_fails BEFORE INSERT ON media_file \
         FOR EACH ROW WHEN NEW.id = 'mf-poison' \
         BEGIN SELECT RAISE(ABORT, 'fixture: media_file base insert leg fails'); END",
        &[],
    );

    let poison = mediafile_record("mf-poison", 1, "/music/poison.mp3");
    let failed = db.try_with_tx_sync::<MediaFileWriteResult, MediaFileWriteError>(|tx| {
        put_mediafile_in_tx(
            tx,
            &MediaIdentity::Id("mf-poison".to_string()),
            &poison,
            &scan_stamp(),
            false,
        )
    });
    let err = failed.expect_err("the base INSERT leg aborts the transaction");
    assert_body_failure(&err, Some(ffi::SQLITE_CONSTRAINT_TRIGGER));

    assert_eq!(
        state(&db, "mf-old"),
        before,
        "the pre-seeded track is untouched"
    );
    assert_eq!(state(&db, "mf-poison").media, before.media);
    assert_eq!(
        state(&db, "mf-poison"),
        State {
            media: before.media,
            artist_rows: 3,
            tag_rows: 2,
            fts: 1,
            cell: None,
        },
        "no base row, no junction rows for the aborted id, no extra FTS entry"
    );
    assert_eq!(fts_match(&db, "Zebracross"), 0);
    assert_eq!(
        fts_match(&db, "\"Second Act\""),
        1,
        "the OLD entry survives"
    );

    fixture(&db, "DROP TRIGGER fixture_mf_insert_fails", &[]);
    let ok = put_committed(
        &db,
        &MediaIdentity::Id("mf-poison".to_string()),
        &poison,
        &scan_stamp(),
        false,
    );
    assert!(ok.inserted);
    assert_eq!(ok.id, "mf-poison");
    assert_eq!(
        artist_links(&db, "mf-poison"),
        ORIGINAL_ARTIST_LINKS.to_vec()
    );
    assert_eq!(fts_count(&db), 2);

    cleanup(db, path);
}

/// Poison the base UPDATE leg — the FIRST statement that touches an existing row. Nothing
/// else runs, so the row keeps every original cell including the junction sets.
#[test]
fn base_update_failure_rolls_back_everything() {
    let (db, path) = fresh_store("base-update");
    seed_graph(&db);
    let record = mediafile_record("mf-old", 1, "/music/old.mp3");
    put_committed(
        &db,
        &MediaIdentity::Id("mf-old".to_string()),
        &record,
        &scan_stamp(),
        false,
    );
    let before = state(&db, "mf-old");
    fixture(
        &db,
        "CREATE TRIGGER fixture_mf_update_fails BEFORE UPDATE ON media_file \
         FOR EACH ROW WHEN OLD.id = 'mf-old' \
         BEGIN SELECT RAISE(ABORT, 'fixture: media_file base update leg fails'); END",
        &[],
    );

    let mut updated = mediafile_record("mf-old", 1, "/music/old.mp3");
    updated.title = "Never Written".to_string();
    updated.participants = replaced_participants();
    updated.participant_names = fixture_participant_names(&updated.participants);

    let failed = db.try_with_tx_sync::<MediaFileWriteResult, MediaFileWriteError>(|tx| {
        put_mediafile_in_tx(
            tx,
            &MediaIdentity::Id("mf-old".to_string()),
            &updated,
            &later_stamp(),
            false,
        )
    });
    let err = failed.expect_err("the UPDATE leg aborts before anything else runs");
    assert_body_failure(&err, Some(ffi::SQLITE_CONSTRAINT_TRIGGER));
    assert_eq!(
        state(&db, "mf-old"),
        before,
        "the whole snapshot is unchanged"
    );
    assert_eq!(artist_links(&db, "mf-old"), ORIGINAL_ARTIST_LINKS.to_vec());
    assert_eq!(fts_match(&db, "\"Third Act\""), 0);

    fixture(&db, "DROP TRIGGER fixture_mf_update_fails", &[]);
    cleanup(db, path);
}

/// The `media_file_tags` FK is real (`schema_ddl.rs`: `tag_id varchar references tag (id)`),
/// but the typed tags leg CANNOT reach a violation: Go's INSERT is
/// `INSERT … SELECT … FROM json_each(?) JOIN tag ON tag.id = value` (`sql_tags.go:69`), so the
/// INNER JOIN only ever emits ids the dictionary has — unknown genre values are dropped
/// silently, exactly like unknown artists, and the statement succeeds. A dangling junction row
/// is provable only with raw SQL, which is what this probe does: it pins that FK enforcement is
/// ON (the DSN's `_foreign_keys=on`) and that such an error reaches the `Sqlite` arm with its
/// extended code and no busy classification.
#[test]
fn tags_join_drops_unknown_ids_so_the_fk_is_only_reachable_by_raw_sql() {
    let (db, path) = fresh_store("tag-fk");
    seed_graph(&db);
    let record = mediafile_record("mf-old", 1, "/music/old.mp3");
    put_committed(
        &db,
        &MediaIdentity::Id("mf-old".to_string()),
        &record,
        &scan_stamp(),
        false,
    );

    // The typed API with an unseeded genre value: no error, no junction row, and the base
    // write still lands — the JOIN filtered it before the FK could fire.
    let mut tags = fixture_tags();
    tags.insert(
        "genre".to_string(),
        vec!["Rock".to_string(), "Unseeded".to_string()],
    );
    let updated = MediaFileRecord {
        tags,
        title: "Joined Away".to_string(),
        ..mediafile_record("mf-old", 1, "/music/old.mp3")
    };
    let result = put_committed(
        &db,
        &MediaIdentity::Id("mf-old".to_string()),
        &updated,
        &later_stamp(),
        false,
    );
    assert!(!result.inserted);
    assert_cell(&db, "title", "mf-old", want_text("Joined Away"));
    assert_eq!(
        tag_links(&db, "mf-old"),
        vec![nd_id::new_tag_id("genre", "Rock")],
        "the unknown genre id never reached the FK"
    );

    // The same FK with raw SQL, where nothing filters the id first.
    let failed = db.try_with_tx_sync::<(), MediaFileWriteError>(|tx| {
        execute(
            tx,
            &(
                "INSERT INTO media_file_tags (media_file_id, tag_id) VALUES (?, ?)".to_string(),
                vec![Value::from("mf-old"), Value::from("tag-does-not-exist")],
            ),
        )
        .map_err(DbOperationError::sqlite)?;
        Ok(())
    });
    let err = failed.expect_err("FK enforcement is ON, so a raw dangling id must fail");
    assert_body_failure(&err, Some(ffi::SQLITE_CONSTRAINT_FOREIGNKEY));
    assert_eq!(
        tag_links(&db, "mf-old"),
        vec![nd_id::new_tag_id("genre", "Rock")],
        "the failed raw insert rolled back with the transaction"
    );

    cleanup(db, path);
}

/// The base table's own FK (`media_file.library_id REFERENCES library`) stays on the `Sqlite`
/// arm with its extended code intact, exactly as Go returns it raw — busy classification must
/// survive the same seam, so the concrete `rusqlite::Error` is never stringified.
#[test]
fn unknown_library_is_a_raw_foreign_key_failure() {
    let (db, path) = fresh_store("library-fk");
    seed_graph(&db);
    let record = mediafile_record("mf-nolib", 99, "/music/nolib.mp3");
    let failed = db.try_with_tx_sync::<MediaFileWriteResult, MediaFileWriteError>(|tx| {
        put_mediafile_in_tx(
            tx,
            &MediaIdentity::Id("mf-nolib".to_string()),
            &record,
            &scan_stamp(),
            false,
        )
    });
    let err = failed.expect_err("library 99 does not exist");
    assert_body_failure(&err, Some(ffi::SQLITE_CONSTRAINT_FOREIGNKEY));
    assert_eq!(
        state(&db, "mf-nolib"),
        State {
            media: 0,
            artist_rows: 0,
            tag_rows: 0,
            fts: 0,
            cell: None,
        },
        "nothing partial survived the aborted insert"
    );
    assert!(!is_busy(match &err {
        DbRuntimeError::Sqlite { source, .. } => source,
        other => panic!("expected a SQLite failure, got {other:?}"),
    }));

    cleanup(db, path);
}

/// The retry driver's own backoff seam (`store::Delay`, `store.rs:296-300`): zero so a
/// `TX_MAX_RETRIES + 1` exhaustion costs no real sleeps.
fn zero_delay() -> std::time::Duration {
    std::time::Duration::ZERO
}

/// A `Match` miss with an empty `record.id` mints `nd_id::new_random()` INSIDE the attempt
/// (`sql_base_repository.go:635-638`). In this test the real writer lock rejects the initial
/// INSERT, after minting but before the participant/tag/FTS legs. Exhaustion returns no result,
/// so no generated id reaches the caller; the attempted writes leave no rows behind. This
/// proves no ID publication/persistence for this pre-write BUSY path, not rollback of a fully
/// completed bundle after a commit failure.
///
/// The lock is REAL, not simulated — the same shape `busy_retry_mapping.rs:83-125` uses: one
/// on-disk file opened twice, `conn_a` holding an uncommitted write transaction, and
/// `busy_timeout = 0` so SQLite reports BUSY (primary code 5) rather than waiting. The typed
/// driver runs on a second ordinary connection with the zero-delay seam because
/// `SQLStore::try_with_tx_retry_sync` uses the production 5s/10s/15s backoff
/// (`store.rs:429-439`) and this lane may not change it.
///
/// Non-vacuity: after the lock is released the SAME Put runs once more and lands exactly ONE
/// row, ONE junction set and ONE index entry. The exhausted attempts produced no visible or
/// persisted ID; a leak would show as extra rows or extra index bytes.
#[test]
fn busy_exhausted_attempts_publish_no_minted_id() {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        "nd-db-mediafile-writes-busy-retry-",
        ".db",
    ));
    if path.exists() {
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("cannot clear {path:?}: {e}"));
    }
    // `busy_timeout = 0` on every pooled handle, so a contended write fails immediately
    // instead of waiting out the driver default; FK stays ON as elsewhere in this file.
    let config = nd_db::pool::PoolConfig {
        path: dsn(&path),
        size: 2,
        options: Some(nd_dbschema::schema::DbOpenOptions {
            journal_mode: Some(nd_dbschema::schema::JournalMode::Delete),
            busy_timeout: 0,
            synchronous: nd_dbschema::schema::Synchronous::Normal,
            foreign_keys: Some(true),
            cache_size: None,
        }),
    };
    let (pool, first) = nd_db::pool::Pool::new(&config).unwrap();
    apply_fresh_schema(&first).unwrap();
    pool.put_back(first).expect("return configured handle");
    let db = SQLStore::new(pool);
    seed_graph(&db);

    let identity = MediaIdentity::Match {
        path: "/music/retry.mp3".to_string(),
        library_id: 1,
    };
    let record = mediafile_record("", 1, "/music/retry.mp3");

    // `conn_a`: an open, uncommitted write transaction holds the write state.
    let mut locker = nd_dbschema::schema::open(&dsn(&path)).unwrap();
    locker
        .busy_timeout(std::time::Duration::ZERO)
        .expect("no busy wait on the locker");
    {
        let tx_a = locker
            .transaction()
            .expect("the locker must be able to open a transaction");
        tx_a.execute(
            "INSERT INTO property(id, value) VALUES(?, ?)",
            rusqlite::params!["Db10BusyLockProbe", "held"],
        )
        .expect("the locker must write inside its transaction");

        // `conn_b`: the contended side, running the PRODUCTION typed retry driver.
        let mut contender = nd_dbschema::schema::open(&dsn(&path)).unwrap();
        contender
            .busy_timeout(std::time::Duration::ZERO)
            .expect("no busy wait on the contender");
        let delay: &nd_db::store::Delay = &|_attempt: u32| -> std::time::Duration { zero_delay() };
        let exhausted = nd_db::store::try_with_tx_retry::<MediaFileWriteResult, MediaFileWriteError>(
            &mut contender,
            delay,
            |tx| put_mediafile_in_tx(tx, &identity, &record, &scan_stamp(), false),
        );

        let err = match exhausted {
            Ok(result) => {
                panic!("the contended Put must not succeed while the lock is held, got {result:?}")
            }
            Err(err) => err,
        };
        match &err {
            DbRuntimeError::BusyExhausted {
                attempts,
                stage,
                source,
            } => {
                assert_eq!(
                    *attempts,
                    TX_MAX_RETRIES + 1,
                    "1 try + txMaxRetries (`persistence.go:205`): {err}"
                );
                assert_eq!(
                    *stage,
                    TransactionStage::Body,
                    "the refusal is a body write"
                );
                assert!(
                    is_busy(source),
                    "the lock refusal classifies busy: {source:?}"
                );
            }
            other => panic!("expected a busy exhaustion, got {other:?}"),
        }
        // The contender's own connection came back usable: its transaction was rolled back.
        assert!(
            contender.is_autocommit(),
            "the failed attempt released its lock state"
        );
    }
    // The locker is dropped (rolling its own transaction back) so reads below are unobstructed.
    drop(locker);

    // Nothing from any failed attempt is visible: no row at the path, no junction rows, no
    // index write.
    assert_eq!(
        path_rows(&db, "/music/retry.mp3", 1),
        Vec::<String>::new(),
        "no attempt left a row behind"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file"),
        0,
        "the fixture graph has no media rows, so any leaked minted id would show here"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file_artists"),
        0,
        "no junction rows from an aborted attempt"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file_tags"),
        0
    );
    // A freshly-created contentless FTS5 table already holds its own config block in
    // `_data` (measured: 7 bytes, no terms, no docsize rows), so the empty baseline is that
    // fingerprint rather than all-zeros. The assertion that matters is that no attempt moved
    // it, and the post-commit check below shows one row moving it.
    let empty_index = fts_index_fingerprint(&db);
    assert_eq!(
        empty_index.1, 0,
        "no term entries: nothing was indexed by any attempt"
    );
    assert_eq!(
        empty_index.2, -1,
        "no docsize row: no attempt published a document"
    );
    assert_eq!(
        fts_count(&db),
        0,
        "no FTS row at all, so `count(*)` here is corroborating rather than the load-bearing \
         claim"
    );

    // Release the lock and run the same Put once: exactly ONE row, ONE junction set, ONE
    // index entry — four attempts, one publication.
    let result = put_committed(&db, &identity, &record, &scan_stamp(), false);
    assert!(result.inserted);
    nd_id::decode(&result.id).unwrap_or_else(|e| panic!("the minted id is not decodable: {e}"));
    assert_eq!(
        path_rows(&db, "/music/retry.mp3", 1),
        vec![result.id.clone()],
        "one row at the contended path"
    );
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM media_file"), 1);
    assert_eq!(
        artist_links(&db, &result.id),
        ORIGINAL_ARTIST_LINKS.to_vec()
    );
    assert_eq!(tag_links(&db, &result.id), original_tag_links());
    assert_eq!(fts_count(&db), 1, "one index entry, not one per attempt");
    assert_eq!(fts_match(&db, "\"Title-\""), 1);

    cleanup(db, path);
}

/// The renderer contract this wrapper executes stays unchanged: the six-leg bundle, Go's
/// `created_at`-only stamp, the `created_at`/`birth_time` UPDATE drop, and `put_match_sql`'s
/// statement shape. Pins the seam the DB-09 sibling also relies on.
#[test]
fn legacy_renderer_bundle_stays_compatible() {
    let record = mediafile_record("mf-render", 1, "/music/render.mp3");
    let mut zero = record.clone();
    zero.created_at = None;

    let bundle = put_sql("mf-render", &record, scan_stamp(), false, &[]).expect("renders");
    assert!(
        bundle.update.0.starts_with("UPDATE media_file SET "),
        "{:?}",
        bundle.update.0
    );
    assert!(
        bundle.update.0.ends_with(" WHERE id = ?"),
        "{:?}",
        bundle.update.0
    );
    assert!(
        !bundle.update.0.contains("created_at = ?"),
        "dropped by UPDATE_DROP"
    );
    assert!(
        !bundle.update.0.contains("birth_time = ?"),
        "dropped by UPDATE_DROP"
    );
    assert!(
        bundle.insert.0.starts_with("INSERT INTO media_file "),
        "{:?}",
        bundle.insert.0
    );
    assert_eq!(
        bundle.update.1.len(),
        65,
        "66 columns (63 persisted + the three derived FTS \
        columns) minus created_at and birth_time, id last"
    );
    assert_eq!(bundle.update.1.last(), Some(&Value::from("mf-render")));
    assert_eq!(bundle.insert.1.len(), 67, "66 columns plus id");
    assert_eq!(bundle.insert.1.last(), Some(&Value::from("mf-render")));
    assert_eq!(
        bundle.participants_delete.0,
        "DELETE FROM media_file_artists WHERE media_file_id = ?"
    );
    assert_eq!(
        bundle.tags_delete.0,
        "DELETE FROM media_file_tags WHERE media_file_id = ?"
    );
    assert!(bundle.participants_insert.is_some() && bundle.tags_insert.is_some());
    assert!(
        !bundle.update.0.contains("media_file_fts") && !bundle.insert.0.contains("media_file_fts"),
        "the bundle never mentions the FTS table"
    );

    // `created_at` is the only stamp `put_sql` applies, and only when the record's `IsZero()`
    // — which here means `None` OR an explicit `Some(time::zero())`.
    let stamped = put_sql("mf-render", &zero, scan_stamp(), false, &[]).expect("renders");
    let want = Value::Text(time::to_text(&scan_stamp()));
    assert!(
        stamped.insert.1.contains(&want),
        "created_at stamped to the caller's now"
    );
    let mut zero_value = record.clone();
    zero_value.created_at = Some(time::zero());
    let stamped_zero =
        put_sql("mf-render", &zero_value, scan_stamp(), false, &[]).expect("renders");
    assert!(
        stamped_zero.insert.1.contains(&want),
        "the explicit Go zero value is stamped the same way as None"
    );
    assert!(
        !stamped_zero
            .insert
            .1
            .contains(&Value::Text(time::to_text(&time::zero()))),
        "the year-one value itself is not stored"
    );
    assert!(
        !bundle
            .insert
            .1
            .contains(&Value::Text(time::to_text(&Utc::now()))),
        "nothing in the bundle came from a clock this seam read"
    );

    let (sql, args) = put_match_sql("/music/render.mp3", 7);
    assert_eq!(
        sql,
        "SELECT id FROM media_file WHERE library_id = ? AND path = ?"
    );
    assert_eq!(
        args,
        vec![Value::Integer(7), Value::from("/music/render.mp3")]
    );
}
