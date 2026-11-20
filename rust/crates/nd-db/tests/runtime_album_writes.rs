//! DB-09 — the EXECUTED album Put (`persistence/album_repository.go:208-219` via
//! `sql_base_repository.go:618-642` + `sql_participations.go:78-125` +
//! `sql_tags.go:57-79`) through the DB-00 typed transaction seam.
//!
//! Go truth under test, SOURCE-VERIFIED at the pinned lines and then EXECUTED-Rust
//! against a fresh schema (`nd_dbschema::schema::apply_fresh_schema`, one isolated temp
//! file per invocation, `_foreign_keys=on` — Go's own DSN parameter, `consts/consts.go:14`):
//! * `Put` leg order is exactly base (UPDATE-first, INSERT on a zero-row match) →
//!   participants DELETE → optional participants INSERT → tags DELETE → optional tags
//!   INSERT, and any leg failure aborts the whole Put. The two tags-leg poison triggers add an
//!   `EXISTS` check on the NEW participant link, so they can ONLY fire after that INSERT has
//!   executed: the later failure itself is the ordering evidence, not an assumption.
//! * `Put` stamps ONLY `imported_at` (`:210`); `created_at` is dropped from the UPDATE set
//!   by the shared seam and `updated_at` passes through the record unchanged.
//! * Each junction DELETE runs unconditionally, so a record with empty participants/tags
//!   CLEARS stale links rather than leaving them (`sql_participations.go:79-89`,
//!   `sql_tags.go:58-74`).
//! * Both junction INSERTs are `INSERT … SELECT` over `json_each` INNER-joined to
//!   `artist`/`tag`, so unknown ids are dropped silently by the join while the stored JSON
//!   columns keep them — Go's asymmetry, pinned here rather than assumed. The dictionary rows
//!   for the record's NON-indexed tag names are seeded through the production DB-06 write, so
//!   their absence from `album_tags` proves the `indexed_tag_ids` name filter rather than a JOIN
//!   miss.
//! * No FTS statement is issued: the schema's `album_fts_ai`/`_au`/`_ad` triggers
//!   (`nd-dbschema/src/schema_ddl.rs:85-87`) index the base row's own derived columns, so
//!   `album_fts` is only observable through `MATCH` and its row count (`content=''` makes it
//!   a CONTENTLESS FTS5 table — the index holds no copy of the content, so no content row is
//!   claimed to be readable from it).
//!
//! **Corrected scout claim.** The DB-09 scout asserted a malformed `album.tags` yields
//! `SQLITE_CONSTRAINT_TRIGGER`. Executed against the real trigger DDL with SQLite 3.54.0 —
//! and again here in
//! `album_malformed_tags_cell_is_a_sqlite_error_not_a_trigger_class` — the trigger's
//! `json_each(NEW.tags, '$.albumversion')` fails with `malformed JSON`, primary code
//! `SQLITE_ERROR` (1) and NO `SQLITE_CONSTRAINT_*` extended code. `SQLITE_CONSTRAINT_TRIGGER`
//! (1811) belongs to a `RAISE(ABORT)` trigger body, which the same test pins for contrast.
//! The typed write path cannot even reach the malformed state:
//! [`crate::sql_tags::marshal_tags`] always emits valid JSON, so no invalid JSON is injected
//! into a typed `DbTags` here.
//!
//! Fixture graph comes from PRODUCTION writes: DB-05 `library_repo::put_in_tx`,
//! DB-08 `artist_repo::put_artist_in_tx` (+ DB-05 `add_artist_in_tx` for the library link),
//! and DB-06 `tag_repo::add_in_tx` with real `nd_id::new_tag_id` ids. `user`/`user_library`
//! have no production write API yet, so — as in `runtime_library_writes.rs:113-129` — they
//! are fixture INSERTs. Test-local DDL (poison triggers, `DROP TRIGGER`) is allowed as in
//! `runtime_library_writes.rs:768-776`; `nd-dbschema` is untouched.
//!
//! `AlbumRepo::get` proves library visibility through the caller-built `user_library`
//! clause (same as `runtime_hydrate_album_mediafile.rs:126-132`); it swallows artwork
//! failures, so it proves visibility, never artwork.
//!
//! There is no durable Go oracle for album Put (`testdata/` holds none), so nothing here is
//! labeled a Go golden — every claim is SOURCE-VERIFIED or EXECUTED-Rust. rusqlite's
//! `hooks` feature is not enabled in this manifest and this lane may not edit it, so
//! "no insert statement ran" is asserted from the renderer's `Option` legs plus the
//! observable junction state, and a trigger's NON-execution is only ever claimed as an
//! unchanged observable index result — never as proof a trigger body was skipped. Recorded as
//! coverage gaps in the lane report.

use std::collections::BTreeMap;
use std::convert::Infallible;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use nd_db::album_repo::{
    participant_names_from_participants, put_album_in_tx, put_sql, AlbumError, AlbumRecord,
    AlbumRepo, AlbumWriteError, AlbumWriteResult, NamedParticipant,
};
use nd_db::artist_repo::{put_artist_in_tx, ArtistColumn, ArtistRecord, ArtistWriteError};
use nd_db::execution::execute;
use nd_db::library_repo::{
    add_artist_in_tx, put_in_tx, LibraryColumn, LibraryRecord, LibraryWriteError,
};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sql_participations::{Participant, Participants, Role};
use nd_db::sql_tags::DbTags;
use nd_db::sqlrepo::Value;
use nd_db::store::{is_busy, SQLStore};
use nd_db::tag_repo::{self, Tag};
use nd_db::{time, DbOperationError, DbRuntimeError, TransactionStage};
use nd_dbschema::schema::apply_fresh_schema;
use rusqlite::ffi;
use rusqlite::types::Value as SqlValue;
use rusqlite::Error as SqliteError;

/// Deterministic caller clocks. `Utc::now()` is never passed to `put_album_in_tx` anywhere
/// in this file, so an implementation that stamped the commit clock would store a value no
/// test could name and every "stamped"/"unchanged" assertion would be vacuous.
fn at(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, s)
            .unwrap(),
    )
}

/// The scanner's save stamp: far OLDER than the commit clock of a test running today.
fn scan_stamp() -> DateTime<Utc> {
    at(2019, 1, 2, 3, 4, 5)
}

/// Older than both other stamps: the record's `created_at`/`updated_at`, which must survive
/// an update leg untouched.
fn record_stamp() -> DateTime<Utc> {
    at(2017, 5, 6, 7, 8, 9)
}

/// Newer than the other two, used for the second Put's `imported_at`. Still in the past, so
/// a commit-clock stamp can never accidentally equal it.
fn later_stamp() -> DateTime<Utc> {
    at(2021, 9, 10, 11, 12, 13)
}

/// Distinct from every other clock in this file: the second record's own `created_at`/
/// `updated_at` INPUT. Because it differs from the stored value, an update leg that wrote the
/// incoming `created_at` — or re-stamped it from the clock — cannot pass by coincidence.
fn incoming_stamp() -> DateTime<Utc> {
    at(2013, 11, 12, 13, 14, 15)
}

fn dsn(path: &std::path::Path) -> String {
    format!("file:{}?mode=rwc&_foreign_keys=on", path.display())
}

/// One isolated on-disk DB per invocation, freshly migrated, pooled
/// (the `runtime_artist_writes.rs:105-117` fixture shape).
fn fresh_store(label: &str) -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        &format!("nd-db-album-writes-{label}-"),
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

/// A raw fixture statement (poison triggers, `user`/`user_library` rows). Library, artist
/// and tag rows come from DB-05/06/08 production writes, never a hand-built INSERT.
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

/// One bound-parameter string column read.
fn scalar_bound<T>(store: &SQLStore, sql: &str, bind: &str) -> T
where
    T: rusqlite::types::FromSql,
{
    lease(store, |conn| {
        conn.query_row(sql, [text(bind)], |row| row.get(0))
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
            .prepare(&format!("SELECT {column} FROM album WHERE id = ?"))
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
/// [`album_record`] carries a DIFFERENT value, so a rotated positional argument list cannot
/// land on a value that happens to match another column.
fn assert_full_record(store: &SQLStore, id: &str, imported_at: &DateTime<Utc>) {
    let stamp = time::to_text(&record_stamp());
    let cases: Vec<(&str, Want)> = vec![
        ("id", want_text(id)),
        ("library_id", Want::Int(1)),
        ("name", want_text(&format!("Name-{id}"))),
        ("embed_art_path", want_text(&format!("embed-{id}"))),
        ("album_artist", want_text(&format!("Artist-{id}"))),
        ("album_artist_id", want_text(&format!("aa-{id}"))),
        ("min_year", Want::Int(1969)),
        ("max_year", Want::Int(1971)),
        ("date", want_text(&format!("date-{id}"))),
        ("min_original_year", Want::Int(1968)),
        ("max_original_year", Want::Int(1967)),
        ("original_date", want_text(&format!("orig-{id}"))),
        ("release_date", want_text(&format!("rel-{id}"))),
        ("compilation", Want::Bool(true)),
        ("comment", want_text(&format!("cmt-{id}"))),
        ("song_count", Want::Int(17)),
        ("duration", Want::Real(1234.5)),
        ("size", Want::Int(87_654_321)),
        ("sort_album_name", want_text(&format!("sort-{id}"))),
        ("sort_album_artist_name", want_text(&format!("sortaa-{id}"))),
        ("order_album_name", want_text(&format!("order-{id}"))),
        (
            "order_album_artist_name",
            want_text(&format!("orderaa-{id}")),
        ),
        ("catalog_num", want_text(&format!("cat-{id}"))),
        ("mbz_album_id", want_text(&format!("mbz-{id}"))),
        ("mbz_album_artist_id", want_text(&format!("mbzaa-{id}"))),
        ("mbz_album_type", want_text(&format!("mbzt-{id}"))),
        ("mbz_album_comment", want_text(&format!("mbzc-{id}"))),
        ("mbz_release_group_id", want_text(&format!("mbzrg-{id}"))),
        ("explicit_status", want_text(&format!("exp-{id}"))),
        ("description", want_text(&format!("desc-{id}"))),
        ("small_image_url", want_text(&format!("small-{id}"))),
        ("medium_image_url", want_text(&format!("medium-{id}"))),
        ("large_image_url", want_text(&format!("large-{id}"))),
        ("external_url", want_text(&format!("ext-{id}"))),
        ("external_info_updated_at", Want::Text(stamp.clone())),
        ("genre", want_text(&format!("colgenre-{id}"))),
        ("missing", Want::Bool(false)),
        ("rg_album_gain", Want::Real(-8.25)),
        ("rg_album_peak", Want::Real(0.75)),
        ("created_at", Want::Text(stamp.clone())),
        ("updated_at", Want::Text(stamp)),
        ("imported_at", Want::Text(time::to_text(imported_at))),
        ("folder_ids", want_text(FOLDER_IDS_JSON)),
        ("discs", want_text(DISCS_JSON)),
        ("participants", want_text(PARTICIPANTS_JSON)),
        ("tags", want_text(&tags_json())),
    ];
    for (column, want) in cases {
        assert_cell(store, column, id, want);
    }
    // The derived FTS columns: the codec (separately golden-tested in
    // runtime_marshal_records.rs) is the expectation, and the values must come from the
    // RECORD's own participants/tags/discs — the S-D2b consistency invariant.
    let record = album_record(id);
    let expected = nd_db::sql_feeders::album_fts_columns(
        &record.name,
        &record.sort_album_name,
        &record.album_artist,
        &nd_db::album_repo::participant_names(&record.participant_names),
        &["disc-one", "disc-ten", "disc-two"],
        &["Deluxe"],
        &["CAT-1"],
    );
    assert_cell(
        store,
        "search_participants",
        id,
        Want::Text(expected.search_participants.clone()),
    );
    assert_cell(
        store,
        "search_normalized",
        id,
        Want::Text(expected.search_normalized.clone()),
    );
    // Deduplicated, byte-sorted join of `AllNames` (the port's declared divergence from Go's
    // non-reproducible map order), and the sub-role is NEVER a name source.
    assert_eq!(
        expected.search_participants, "Main Act Second Act full",
        "sub_role 'Vocals' does not reach the FTS participant text"
    );
}

/// The `participants` column the fixture record must marshal: role-sorted keys, input order
/// within a role, `subRole` omitted when empty (`sql_participations.rs:322-336`).
const PARTICIPANTS_JSON: &str = r#"{"albumartist":[{"id":"ar-main","name":"Main Act"}],"composer":[{"id":"ar-main","name":"Main Act","subRole":"Vocals"},{"id":"ar-second","name":"Second Act"}]}"#;
const FOLDER_IDS_JSON: &str = r#"["fo-1","fo-2"]"#;
/// Stringified key order `1`, `10`, `2` — Go's `encoding/json` map-key order
/// (`sql_json::marshal_discs`).
const DISCS_JSON: &str = r#"{"1":"disc-one","10":"disc-ten","2":"disc-two"}"#;

/// The `tags` column the fixture record must marshal: name keys in sorted order (the
/// `BTreeMap`), values in INPUT order (serde serializes the `Vec` as-is, and Go's
/// `encoding/json` does the same for a slice), and each id from the SAME authority Go uses
/// (`model.NewTag` = `nd_id::new_tag_id`).
fn tags_json() -> String {
    format!(
        "{{\"albumversion\":[{{\"id\":\"{}\",\"value\":\"Deluxe\"}}],\
         \"catalognumber\":[{{\"id\":\"{}\",\"value\":\"CAT-1\"}}],\
         \"genre\":[{{\"id\":\"{}\",\"value\":\"Rock\"}},{{\"id\":\"{}\",\"value\":\"Jazz\"}}]}}",
        nd_id::new_tag_id("albumversion", "Deluxe"),
        nd_id::new_tag_id("catalognumber", "CAT-1"),
        nd_id::new_tag_id("genre", "Rock"),
        nd_id::new_tag_id("genre", "Jazz"),
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
/// Returns the id, which is also the expected `album_tags` value.
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

/// The `user_library`-scoped filter `AlbumRepo::get` needs — the clause
/// `runtime_hydrate_album_mediafile.rs:126-132` uses.
fn album_lib_filter(user_id: &str) -> (String, Vec<Value>) {
    (
        "album.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
            .to_string(),
        vec![Value::from(user_id)],
    )
}

/// The production read side. Panics on `NotFound`, which is why the caller must hold a grant.
fn read_back(store: &SQLStore, id: &str, user_id: &str) -> AlbumRecord {
    lease(store, |conn| {
        AlbumRepo::new(conn)
            .get(id, Some(user_id), false, Some(album_lib_filter(user_id)))
            .unwrap_or_else(|e| panic!("AlbumRepo::get({id}) for {user_id}: {e}"))
    })
}

/// The production read as a `Result`, for the negative (cross-library) assertions.
fn read_attempt(store: &SQLStore, id: &str, user_id: &str) -> Result<AlbumRecord, AlbumError> {
    lease(store, |conn| {
        AlbumRepo::new(conn).get(id, Some(user_id), false, Some(album_lib_filter(user_id)))
    })
}

/// Fixture roles: `albumartist` (no sub-role) and `composer` (one entry with a sub-role, one
/// without). `ar-main` therefore appears twice — the stale-role case is real, not staged.
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
        Role::from("composer"),
        vec![
            Participant {
                id: "ar-main".to_string(),
                name: "Main Act".to_string(),
                sub_role: "Vocals".to_string(),
            },
            Participant {
                id: "ar-second".to_string(),
                name: "Second Act".to_string(),
                sub_role: String::new(),
            },
        ],
    );
    participants
}

/// The `NamedParticipant` FTS view of the SAME participants the JSON and the junction rows
/// come from (the S-D2b invariant); `AllNames` also contributes a non-empty sort name.
fn fixture_participant_names(
    participants: &Participants,
    sort_name: &str,
) -> Vec<NamedParticipant> {
    participant_names_from_participants(participants)
        .into_iter()
        .map(|mut p| {
            p.sort_name = sort_name.to_string();
            p
        })
        .collect()
}

/// Genre plus the two non-indexed tag names that feed the FTS columns. Only `genre` is
/// indexed into `album_tags` (`sql_tags.rs:72`), which is what the junction assertions key on.
fn fixture_tags() -> DbTags {
    let mut tags = DbTags::new();
    tags.insert(
        "genre".to_string(),
        vec!["Rock".to_string(), "Jazz".to_string()],
    );
    tags.insert("albumversion".to_string(), vec!["Deluxe".to_string()]);
    tags.insert("catalognumber".to_string(), vec!["CAT-1".to_string()]);
    tags
}

/// An album record whose every persisted field carries a DIFFERENT value.
fn album_record(id: &str) -> AlbumRecord {
    let participants = fixture_participants();
    let participant_names = fixture_participant_names(&participants, "full");
    let mut discs = BTreeMap::new();
    discs.insert(1, "disc-one".to_string());
    discs.insert(2, "disc-two".to_string());
    discs.insert(10, "disc-ten".to_string());
    AlbumRecord {
        id: id.to_string(),
        library_id: 1,
        name: format!("Name-{id}"),
        embed_art_path: format!("embed-{id}"),
        album_artist: format!("Artist-{id}"),
        album_artist_id: format!("aa-{id}"),
        min_year: 1969,
        max_year: 1971,
        date: format!("date-{id}"),
        min_original_year: 1968,
        max_original_year: 1967,
        original_date: format!("orig-{id}"),
        release_date: format!("rel-{id}"),
        compilation: true,
        comment: format!("cmt-{id}"),
        song_count: 17,
        duration: 1234.5,
        size: 87_654_321,
        discs: Some(discs),
        sort_album_name: format!("sort-{id}"),
        sort_album_artist_name: format!("sortaa-{id}"),
        order_album_name: format!("order-{id}"),
        order_album_artist_name: format!("orderaa-{id}"),
        catalog_num: format!("cat-{id}"),
        mbz_album_id: format!("mbz-{id}"),
        mbz_album_artist_id: format!("mbzaa-{id}"),
        mbz_album_type: format!("mbzt-{id}"),
        mbz_album_comment: format!("mbzc-{id}"),
        mbz_release_group_id: format!("mbzrg-{id}"),
        folder_ids: Some(vec!["fo-1".to_string(), "fo-2".to_string()]),
        explicit_status: format!("exp-{id}"),
        rg_album_gain: Some(-8.25),
        rg_album_peak: Some(0.75),
        description: format!("desc-{id}"),
        small_image_url: format!("small-{id}"),
        medium_image_url: format!("medium-{id}"),
        large_image_url: format!("large-{id}"),
        external_url: format!("ext-{id}"),
        external_info_updated_at: Some(record_stamp()),
        genre: format!("colgenre-{id}"),
        tags: fixture_tags(),
        participants,
        participant_names,
        missing: false,
        imported_at: scan_stamp(),
        created_at: Some(record_stamp()),
        updated_at: Some(record_stamp()),
        library_path: String::new(),
        library_name: String::new(),
        annotation: nd_model::annotation::Annotation::default(),
        genres: Vec::new(),
        image: nd_db::artwork_hydration::ItemImage::default(),
    }
}

/// Commit one album Put through the typed seam exactly as a caller would: the caller's stamp,
/// the record's own identity, and nothing else — this seam takes no column list.
fn put_committed(
    store: &SQLStore,
    record: &AlbumRecord,
    stamp: &DateTime<Utc>,
) -> AlbumWriteResult {
    store
        .try_with_tx_sync::<AlbumWriteResult, AlbumWriteError>(|tx| {
            put_album_in_tx(tx, record, stamp)
        })
        .unwrap_or_else(|e| panic!("put_album_in_tx({}) failed: {e}", record.id))
}

/// `album_artists` rows of one album as a sorted `artist/role/subRole` set. `ON CONFLICT
/// DO NOTHING` over a `json_each` iteration has no row order in Go either, so sets are the
/// honest comparison.
fn artist_links(store: &SQLStore, album_id: &str) -> Vec<String> {
    let mut cells = lease(store, |conn| {
        let mut statement = conn
            .prepare("SELECT artist_id, role, sub_role FROM album_artists WHERE album_id = ?")
            .unwrap();
        let rows = statement
            .query_map([text(album_id)], |row| {
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

/// `album_tags` rows of one album as a sorted tag-id set.
fn tag_links(store: &SQLStore, album_id: &str) -> Vec<String> {
    let mut cells = lease(store, |conn| {
        let mut statement = conn
            .prepare("SELECT tag_id FROM album_tags WHERE album_id = ?")
            .unwrap();
        let rows = statement
            .query_map([text(album_id)], |row| row.get::<_, String>(0))
            .unwrap();
        rows.collect::<rusqlite::Result<Vec<String>>>().unwrap()
    });
    cells.sort();
    cells
}

/// The contentless `album_fts` row count — the trigger-maintenance observable.
fn fts_count(store: &SQLStore) -> i64 {
    scalar(store, "SELECT count(*) FROM album_fts")
}

/// `MATCH` hit count for one query.
fn fts_match(store: &SQLStore, query: &str) -> i64 {
    lease(store, |conn| {
        conn.query_row(
            "SELECT count(*) FROM album_fts WHERE album_fts MATCH ?",
            [text(query)],
            |row| row.get(0),
        )
        .unwrap_or_else(|e| panic!("MATCH {query}: {e}"))
    })
}

/// A whole-transaction snapshot of the observable state, for rollback equality.
#[derive(Debug, PartialEq, Eq)]
struct State {
    albums: i64,
    artist_rows: i64,
    tag_rows: i64,
    fts: i64,
    /// The row's `name`, or `None` when the id matches nothing — the snapshot has to be
    /// readable for an id that was never written.
    cell: Option<String>,
}

fn state(store: &SQLStore, id: &str) -> State {
    State {
        albums: scalar(store, "SELECT count(*) FROM album"),
        artist_rows: scalar(store, "SELECT count(*) FROM album_artists"),
        tag_rows: scalar(store, "SELECT count(*) FROM album_tags"),
        fts: fts_count(store),
        cell: lease(store, |conn| {
            conn.query_row("SELECT name FROM album WHERE id = ?", [text(id)], |row| {
                row.get::<_, String>(0)
            })
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

/// The primary SQLite result code, derived from rusqlite's retained EXTENDED code
/// (`extended_code & 0xFF` is the primary class). `rusqlite::ErrorCode` cannot express this
/// one: `malformed JSON` is `SQLITE_ERROR` (1) with no extended subcode, and rusqlite maps
/// that to `ErrorCode::Unknown` — EXECUTED-Rust here, `code.code == Unknown` while
/// `extended_code == 1`. Reading `code.code as i32` would therefore report 23, not 1.
fn primary_code(err: &rusqlite::Error) -> Option<i32> {
    match err {
        SqliteError::SqliteFailure(code, _) => Some(code.extended_code & 0xff),
        _ => None,
    }
}

/// Assert the executor's own shape: `Body` stage, the concrete retained extended code, and
/// no busy classification. `rusqlite::Error` is not `Clone`, so the classification helpers
/// take it by reference and nothing here hands one back.
fn assert_body_failure(err: &DbRuntimeError<AlbumWriteError>, expected_extended: Option<i32>) {
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
                "the raw SQLite cause survives the seam unwrapped (Go's Put adds no fmt.Errorf, album_repository.go:211-219): {source:?}"
            );
            assert!(
                !is_busy(source),
                "a constraint/trigger failure is not retried"
            );
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }
}

/// The `(primary code, extended code, message)` of a Body-stage SQLite failure, for the
/// malformed-JSON classification that needs the PRIMARY code (`SQLITE_ERROR`).
fn body_codes(err: &DbRuntimeError<AlbumWriteError>) -> (Option<i32>, Option<i32>, String) {
    match err {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(
                *stage,
                TransactionStage::Body,
                "the executor owns the stage"
            );
            assert!(!is_busy(source), "not a busy failure");
            (
                primary_code(source),
                extended_code(source),
                source.to_string(),
            )
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }
}

/// Run one raw statement inside a caller transaction through the shared execution seam —
/// used only for the malformed-JSON probe, which the typed API cannot produce.
fn raw_in_tx(store: &SQLStore, sql: &str, args: Vec<Value>) -> DbRuntimeError<AlbumWriteError> {
    let statement = (sql.to_string(), args);
    let failed = store.try_with_tx_sync::<(), AlbumWriteError>(|tx| {
        execute(tx, &statement).map_err(DbOperationError::sqlite)?;
        Ok(())
    });
    failed.expect_err("the statement must fail")
}

/// Libraries 1 and 2, the artists the fixture participants reference, the dictionary tags the
/// fixture genres AND the fixture non-indexed tag names need, and one granted user per library.
fn seed_graph(store: &SQLStore) {
    seed_library(store, 1, "main");
    seed_library(store, 2, "decoy");
    seed_artist(store, "ar-main", 1);
    seed_artist(store, "ar-second", 1);
    seed_artist(store, "ar-third", 1);
    seed_tag(store, 1, "genre", "Rock");
    seed_tag(store, 1, "genre", "Jazz");
    // The NON-indexed names the fixture record carries, seeded through the PRODUCTION DB-06
    // write so their dictionary ids really exist. `album_unknown_tag_name_is_filtered_before_
    // the_join` needs them present in `tag`: a row that is absent from the dictionary is
    // dropped by the INNER JOIN, which is a different rule from the indexed-name filter.
    seed_tag(store, 1, "albumversion", "Deluxe");
    seed_tag(store, 1, "catalognumber", "CAT-1");
    seed_user(store, "u-one", 1);
    seed_user(store, "u-two", 2);
}

/// The junction set an untouched fixture album must have, spelled out so the rollback tests
/// cannot pass by the rows simply never having existed.
const ORIGINAL_ARTIST_LINKS: [&str; 3] = [
    "ar-main/albumartist/",
    "ar-main/composer/Vocals",
    "ar-second/composer/",
];

/// The `album_tags` set an untouched fixture album must have, spelled out for the same reason.
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

const REPLACED_ARTIST_LINKS: [&str; 2] = ["ar-main/albumartist/", "ar-third/conductor/"];

#[test]
fn album_put_insert_lands_full_record_junctions_and_fts() {
    let (db, path) = fresh_store("insert");
    seed_graph(&db);

    let record = album_record("al-main");
    let result = put_committed(&db, &record, &scan_stamp());
    assert!(
        result.inserted,
        "a predefined id that misses takes the INSERT arm"
    );
    assert_eq!(result.id, "al-main");

    assert_full_record(&db, "al-main", &scan_stamp());

    // Junction sets, keyed to the returned id (Go's `al.ID = id` feeds both legs).
    assert_eq!(
        artist_links(&db, "al-main"),
        ORIGINAL_ARTIST_LINKS.to_vec(),
        "multiple roles and sub-roles are one row each"
    );
    assert_eq!(
        tag_links(&db, "al-main"),
        original_tag_links(),
        "only the INDEXED tag name reaches album_tags"
    );
    // Non-vacuity for that claim: the record's NON-indexed entries (`albumversion`/Deluxe,
    // `catalognumber`/CAT-1) were seeded through the PRODUCTION DB-06 write, so their
    // dictionary ids exist in `tag`. Their absence from `album_tags` above therefore proves
    // the indexed-name filter (`sql_tags::indexed_tag_ids`, which only walks
    // `INDEXED_TAG_NAMES = ["genre"]`) rather than a dictionary JOIN miss.
    assert_eq!(
        count_rows(
            &db,
            "SELECT count(*) FROM tag WHERE id = ? OR id = ?",
            &[
                text(&nd_id::new_tag_id("albumversion", "Deluxe")),
                text(&nd_id::new_tag_id("catalognumber", "CAT-1")),
            ]
        ),
        2,
        "both non-indexed dictionary rows really exist"
    );
    assert_eq!(
        scalar_bound::<i64>(
            &db,
            "SELECT count(*) FROM album_artists WHERE album_id = ?",
            "al-main"
        ),
        3
    );

    // FTS is maintained by the schema's own `album_fts_ai` trigger: one base insert = one
    // index row, and the record-derived columns are actually searchable. This lane writes no
    // FTS statement, so a hand-written index row cannot be what proves this.
    assert_eq!(
        fts_count(&db),
        1,
        "the INSERT trigger indexed exactly one row"
    );
    assert_eq!(
        fts_match(&db, "Zebracross"),
        0,
        "non-vacuity: an absent token misses"
    );
    assert_eq!(
        fts_match(&db, "Deluxe"),
        1,
        "album_version is derived from the tags cell"
    );
    assert_eq!(
        fts_match(&db, "\"Second Act\""),
        1,
        "search_participants is indexed"
    );

    // The production read side sees the same row for a user granted library 1.
    let got = read_back(&db, "al-main", "u-one");
    assert_eq!(got.id, "al-main");
    assert_eq!(got.name, "Name-al-main");
    assert_eq!(got.imported_at, scan_stamp());
    assert_eq!(got.created_at, Some(record_stamp()));
    assert_eq!(got.updated_at, Some(record_stamp()));
    assert_eq!(got.rg_album_gain, Some(-8.25));
    assert_eq!(got.rg_album_peak, Some(0.75));
    assert_eq!(
        got.folder_ids,
        Some(vec!["fo-1".to_string(), "fo-2".to_string()])
    );

    cleanup(db, path);
}

#[test]
fn album_optional_columns_bind_null_and_nil_vs_empty_json_stays_distinct() {
    let (db, path) = fresh_store("nulls");
    seed_graph(&db);

    // `None` → SQL NULL for the nullable columns; `None` folder_ids/discs → the JSON literal
    // `null` (those columns are `NOT NULL` jsonb, and Go nil marshals to `null`). Empty maps
    // marshal `{}`.
    let mut record = album_record("al-null");
    record.rg_album_gain = None;
    record.rg_album_peak = None;
    record.external_info_updated_at = None;
    record.created_at = None;
    record.updated_at = None;
    record.folder_ids = None;
    record.discs = None;
    record.tags = DbTags::new();
    record.participants = Participants::new();
    record.participant_names = Vec::new();

    let result = put_committed(&db, &record, &scan_stamp());
    assert!(result.inserted);
    assert_cell(&db, "rg_album_gain", "al-null", Want::Null);
    assert_cell(&db, "rg_album_peak", "al-null", Want::Null);
    assert_cell(&db, "external_info_updated_at", "al-null", Want::Null);
    assert_cell(&db, "created_at", "al-null", Want::Null);
    assert_cell(&db, "updated_at", "al-null", Want::Null);
    assert_cell(&db, "folder_ids", "al-null", Want::Text("null".to_string()));
    assert_cell(&db, "discs", "al-null", Want::Text("null".to_string()));
    assert_cell(&db, "tags", "al-null", Want::Text("{}".to_string()));
    assert_cell(&db, "participants", "al-null", Want::Text("{}".to_string()));
    assert_eq!(artist_links(&db, "al-null"), Vec::<String>::new());
    assert_eq!(tag_links(&db, "al-null"), Vec::<String>::new());
    assert_eq!(
        fts_count(&db),
        1,
        "still one index row, written by the trigger"
    );

    // `Some(empty)` is a DIFFERENT cell from `None`: the JSON `[]`, not SQL NULL.
    record.folder_ids = Some(Vec::new());
    put_committed(&db, &record, &later_stamp());
    assert_cell(&db, "folder_ids", "al-null", Want::Text("[]".to_string()));

    cleanup(db, path);
}

#[test]
fn album_update_leg_keeps_created_at_and_input_record_and_stamps_only_caller_imported_at() {
    let (db, path) = fresh_store("update");
    seed_graph(&db);

    let record = album_record("al-up");
    let first = put_committed(&db, &record, &scan_stamp());
    assert!(first.inserted);

    // A second Put of the SAME id takes the UPDATE arm. The name keeps NO token in common
    // with the old one, and the OLD name is a token that WAS indexed, so the re-index is
    // observable in both directions rather than coincidental.
    assert_eq!(
        fts_match(&db, "\"Name-al-up\""),
        1,
        "baseline: the original name is indexed before the rename"
    );
    let mut updated = record.clone();
    updated.name = "Zebracross Renamed".to_string();
    updated.max_year = 1999;
    // Deliberately DIFFERENT incoming stamps: the stored values must stay the ORIGINAL ones.
    updated.created_at = Some(incoming_stamp());
    updated.updated_at = Some(incoming_stamp());
    let second = put_committed(&db, &updated, &later_stamp());
    assert!(
        !second.inserted,
        "the UPDATE leg matched, so the INSERT never ran"
    );
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM album"), 1);

    assert_cell(
        &db,
        "name",
        "al-up",
        Want::Text("Zebracross Renamed".to_string()),
    );
    assert_cell(&db, "max_year", "al-up", Want::Int(1999));
    // `created_at` is dropped from the UPDATE set by the shared seam (`marshal::UPDATE_DROP`):
    // the incoming record carried `incoming_stamp()`, so the stored value can only be the
    // ORIGINAL one. A seam that wrote the input column instead of dropping it would fail here.
    assert_cell(
        &db,
        "created_at",
        "al-up",
        Want::Text(time::to_text(&record_stamp())),
    );
    // Go's `Put` stamps neither `created_at` nor `updated_at`; `updated_at` DOES pass through
    // the record's value, which is why it changed to the incoming stamp while `created_at`
    // did not. The two assertions are meaningful only because the stamps differ.
    assert_cell(
        &db,
        "updated_at",
        "al-up",
        Want::Text(time::to_text(&incoming_stamp())),
    );
    assert_ne!(incoming_stamp(), record_stamp());
    // `imported_at` is the CALLER's stamp, and both caller stamps here predate the wall clock,
    // which a commit-clock implementation could not reproduce.
    assert_cell(
        &db,
        "imported_at",
        "al-up",
        Want::Text(time::to_text(&later_stamp())),
    );
    assert!(later_stamp() < Utc::now() && scan_stamp() < Utc::now());

    // The CALLER's records are borrowed and never mutated — Go mutates `al.ImportedAt` and
    // `al.ID` on the model; the port keeps inputs immutable, so a retry sees the same input.
    assert_eq!(
        record.imported_at,
        scan_stamp(),
        "input record is unmutated"
    );
    assert_eq!(
        updated.imported_at,
        scan_stamp(),
        "input record is unmutated"
    );
    assert_eq!(
        updated.created_at,
        Some(incoming_stamp()),
        "input record is unmutated"
    );
    assert_eq!(
        updated.updated_at,
        Some(incoming_stamp()),
        "input record is unmutated"
    );
    assert_eq!(
        record.created_at,
        Some(record_stamp()),
        "input record is unmutated"
    );
    assert_eq!(
        record.participants,
        fixture_participants(),
        "input Vec is unmutated"
    );
    assert_eq!(record.tags, fixture_tags(), "input map is unmutated");

    // FTS: the update leg re-indexes the SAME rowid (delete + insert), never a second row, and
    // the OLD indexed name token is gone while the new one is present.
    assert_eq!(fts_count(&db), 1, "an update re-indexes one row, not two");
    assert_eq!(fts_match(&db, "Zebracross"), 1, "the new name is indexed");
    assert_eq!(
        fts_match(&db, "\"Name-al-up\""),
        0,
        "the token that WAS indexed (asserted above) is gone from the index"
    );
    assert_eq!(
        fts_match(&db, "Abbeyroad"),
        0,
        "non-vacuity: a token never seeded anywhere misses"
    );

    // An update that changes ONLY year/count columns leaves the observable index result
    // unchanged: same row count and the same MATCH answers. The WHEN guard on
    // `album_fts_au` (`schema_ddl.rs:87`) is source-inspected — without statement hooks this
    // asserts that the index RESULT is unchanged, not that no trigger body executed.
    let before = fts_count(&db);
    let year_before = fts_match(&db, "Zebracross");
    let mut year_only = updated.clone();
    year_only.min_year = 1962;
    year_only.max_year = 1962;
    year_only.song_count = 99;
    year_only.duration = 999.0;
    let third = put_committed(&db, &year_only, &record_stamp());
    assert!(!third.inserted);
    assert_eq!(fts_count(&db), before, "the index row count is unchanged");
    assert_eq!(
        fts_match(&db, "Zebracross"),
        year_before,
        "the indexed name still matches exactly as before"
    );
    assert_eq!(
        fts_match(&db, "\"Name-al-up\""),
        0,
        "the removed token did not come back on a non-indexed update"
    );
    assert_cell(&db, "min_year", "al-up", Want::Int(1962));

    cleanup(db, path);
}

#[test]
fn album_predefined_id_miss_inserts_then_update_succeeds() {
    let (db, path) = fresh_store("predefined");
    seed_graph(&db);

    // Go `sql_base_repository.go:630-641`: a zero-row UPDATE is the INSERT fallback with the
    // SAME predefined id — not a miss, not a NotFound, and not a regenerated id.
    let record = album_record("al-pre");
    let miss = put_committed(&db, &record, &scan_stamp());
    assert!(miss.inserted);
    assert_eq!(miss.id, "al-pre");
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM album"), 1);

    let hit = put_committed(&db, &record, &later_stamp());
    assert!(
        !hit.inserted,
        "the second Put of the same id takes the UPDATE arm"
    );
    assert_eq!(hit.id, "al-pre", "the port never generates an id");
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM album"), 1);
    assert_eq!(
        artist_links(&db, "al-pre"),
        ORIGINAL_ARTIST_LINKS.to_vec(),
        "the junction legs re-ran idempotently under the UNIQUE constraints"
    );
    assert_eq!(fts_count(&db), 1);

    cleanup(db, path);
}

#[test]
fn album_empty_participants_and_tags_clear_stale_links_without_an_insert() {
    let (db, path) = fresh_store("clear");
    seed_graph(&db);
    put_committed(&db, &album_record("al-clear"), &scan_stamp());
    assert_eq!(
        artist_links(&db, "al-clear").len(),
        3,
        "precondition: links exist"
    );
    assert_eq!(
        tag_links(&db, "al-clear").len(),
        2,
        "precondition: tag rows exist"
    );

    let mut emptied = album_record("al-clear");
    emptied.participants = Participants::new();
    emptied.participant_names = Vec::new();
    emptied.tags = DbTags::new();

    // The renderer's `Option` legs are `None` here — Go's early return
    // (`sql_participations.go:87-89`, `sql_tags.go:72-74`) — while both DELETEs stay `Some`:
    // the delete runs and only the insert is skipped. Statement COUNTING is a coverage gap
    // (rusqlite `hooks` is not enabled in this manifest), so the observable fact is the empty
    // junction with a present base row.
    let bundle = put_sql("al-clear", &emptied, scan_stamp(), &[]).expect("renders");
    assert!(bundle.participants_insert.is_none());
    assert!(bundle.tags_insert.is_none());

    let result = put_committed(&db, &emptied, &later_stamp());
    assert!(!result.inserted);
    assert_eq!(
        artist_links(&db, "al-clear"),
        Vec::<String>::new(),
        "the unconditional DELETE cleared the stale role rows"
    );
    assert_eq!(
        tag_links(&db, "al-clear"),
        Vec::<String>::new(),
        "the stale tag rows are gone too"
    );
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM album"), 1);
    assert_eq!(fts_count(&db), 1, "junction writes touch no FTS trigger");
    // The JSON columns follow the record (empty `{}`), not the previous junction state.
    assert_cell(
        &db,
        "participants",
        "al-clear",
        Want::Text("{}".to_string()),
    );
    assert_cell(&db, "tags", "al-clear", Want::Text("{}".to_string()));
    assert_eq!(
        fts_match(&db, "\"Second Act\""),
        0,
        "search_participants was re-indexed to the empty value"
    );

    cleanup(db, path);
}

#[test]
fn album_re_put_replaces_roles_and_tags_and_leaves_no_stale_rows() {
    let (db, path) = fresh_store("replace");
    // `ar-third` comes from `seed_graph`; only the extra genre needs seeding here.
    seed_graph(&db);
    seed_tag(&db, 1, "genre", "Folk");

    put_committed(&db, &album_record("al-rep"), &scan_stamp());

    // `ar-main` was albumartist AND composer (one with a sub-role); the re-save keeps it as
    // composer only, drops `ar-second`, and swaps the genre set. Every stale row must be gone
    // — that is exactly what the unconditional DELETE is for.
    let mut replaced = album_record("al-rep");
    let mut participants = Participants::new();
    participants.insert(
        Role::from("composer"),
        vec![Participant {
            id: "ar-main".to_string(),
            name: "Main Act".to_string(),
            sub_role: String::new(),
        }],
    );
    participants.insert(
        Role::from("arranger"),
        vec![Participant {
            id: "ar-third".to_string(),
            name: "Third Act".to_string(),
            sub_role: "Strings".to_string(),
        }],
    );
    replaced.participants = participants.clone();
    replaced.participant_names = fixture_participant_names(&participants, "");
    let mut tags = DbTags::new();
    tags.insert("genre".to_string(), vec!["Folk".to_string()]);
    replaced.tags = tags;

    put_committed(&db, &replaced, &later_stamp());

    assert_eq!(
        artist_links(&db, "al-rep"),
        vec![
            "ar-main/composer/".to_string(),
            "ar-third/arranger/Strings".to_string()
        ],
        "no albumartist row and no ar-second row survives"
    );
    assert_eq!(
        tag_links(&db, "al-rep"),
        vec![nd_id::new_tag_id("genre", "Folk")],
        "the Rock/Jazz link rows are gone"
    );
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM album_tags"), 1);
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM album_artists"), 2);

    cleanup(db, path);
}

#[test]
fn album_unknown_artist_and_tag_ids_are_dropped_by_the_inner_join() {
    let (db, path) = fresh_store("unknown");
    seed_graph(&db);

    let mut record = album_record("al-unk");
    let mut participants = fixture_participants();
    participants.insert(
        Role::from("conductor"),
        vec![Participant {
            id: "ar-ghost".to_string(),
            name: "Ghost Act".to_string(),
            sub_role: String::new(),
        }],
    );
    record.participants = participants.clone();
    record.participant_names = fixture_participant_names(&participants, "");
    // A genre whose dictionary row was never added: `new_tag_id` is deterministic, so the id
    // is well-formed and simply absent from `tag`.
    let mut tags = fixture_tags();
    tags.insert(
        "genre".to_string(),
        vec!["Rock".to_string(), "Ghost Genre".to_string()],
    );
    record.tags = tags;

    let result = put_committed(&db, &record, &scan_stamp());
    assert!(result.inserted, "the base row is written regardless");
    assert_eq!(
        artist_links(&db, "al-unk"),
        ORIGINAL_ARTIST_LINKS.to_vec(),
        "ar-ghost is dropped by the INNER JOIN to artist, and the known rows still landed"
    );
    assert_eq!(
        tag_links(&db, "al-unk"),
        vec![nd_id::new_tag_id("genre", "Rock")],
        "the absent dictionary id is dropped by the INNER JOIN to tag, and the known id landed"
    );

    // Go's asymmetry: `Put` never re-derives the JSON from the junction result, so the
    // unknown entries are STILL in the stored columns.
    let stored =
        scalar_bound::<String>(&db, "SELECT participants FROM album WHERE id = ?", "al-unk");
    assert!(stored.contains("ar-ghost"), "the JSON keeps the unknown id");
    let stored_tags = scalar_bound::<String>(&db, "SELECT tags FROM album WHERE id = ?", "al-unk");
    assert!(
        stored_tags.contains("Ghost Genre"),
        "the tags cell keeps the unknown value"
    );
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM album_artists"), 3);
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM album_tags"), 1);

    cleanup(db, path);
}

#[test]
fn album_is_visible_only_to_a_user_granted_its_library() {
    let (db, path) = fresh_store("scope");
    seed_graph(&db);
    let mut decoy = album_record("al-decoy");
    decoy.library_id = 2;
    put_committed(&db, &decoy, &scan_stamp());
    put_committed(&db, &album_record("al-target"), &scan_stamp());
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM album"), 2);

    let target = read_back(&db, "al-target", "u-one");
    assert_eq!(target.library_id, 1);
    assert_eq!(
        target.library_path, "/lib-main-1",
        "the LEFT join is hydrated"
    );

    let error = read_attempt(&db, "al-target", "u-two")
        .expect_err("a user granted only library 2 must not see library 1's album");
    assert!(
        matches!(error, AlbumError::NotFound),
        "the exclusion is NotFound, not a statement failure: {error:?}"
    );
    // The decoy is the other way round, so the filter is discriminating in both directions.
    assert_eq!(read_back(&db, "al-decoy", "u-two").library_id, 2);
    assert!(matches!(
        read_attempt(&db, "al-decoy", "u-one"),
        Err(AlbumError::NotFound)
    ));

    cleanup(db, path);
}

#[test]
fn album_empty_id_is_refused_before_any_statement() {
    let (db, path) = fresh_store("empty-id");
    seed_graph(&db);
    let before = state(&db, "al-blank");

    let record = AlbumRecord {
        id: String::new(),
        ..album_record("al-blank")
    };
    let failed = db.try_with_tx_sync::<AlbumWriteResult, AlbumWriteError>(|tx| {
        put_album_in_tx(tx, &record, &scan_stamp())
    });
    let err = failed.expect_err("an empty id is a contract violation, not a generated id");
    assert!(
        matches!(err, DbRuntimeError::Domain(AlbumWriteError::EmptyId)),
        "the domain arm carries the concrete cause: {err:?}"
    );
    assert_eq!(
        state(&db, "al-blank"),
        before,
        "Go's random-id arm never ran: nothing was written anywhere"
    );

    cleanup(db, path);
}

#[test]
fn album_base_insert_leg_failure_rolls_back_base_junctions_and_fts() {
    let (db, path) = fresh_store("base-insert");
    seed_graph(&db);
    put_committed(&db, &album_record("al-old"), &scan_stamp());
    let before = state(&db, "al-old");
    // SQLite forbids bound variables inside a trigger body, so the id is spelled in the DDL.
    fixture(
        &db,
        "CREATE TRIGGER fixture_album_insert_fails BEFORE INSERT ON album \
         FOR EACH ROW WHEN NEW.id = 'al-poison' \
         BEGIN SELECT RAISE(ABORT, 'fixture: album base insert leg fails'); END",
        &[],
    );

    let record = album_record("al-poison");
    let failed = db.try_with_tx_sync::<AlbumWriteResult, AlbumWriteError>(|tx| {
        put_album_in_tx(tx, &record, &scan_stamp())
    });
    let err = failed.expect_err("the base INSERT leg aborts the transaction");
    assert_body_failure(&err, Some(ffi::SQLITE_CONSTRAINT_TRIGGER));

    assert_eq!(
        state(&db, "al-old"),
        before,
        "the pre-seeded album is untouched, so the rollback is observable"
    );
    assert_eq!(fts_match(&db, "Zebracross"), 0);
    assert_eq!(
        fts_match(&db, "\"Second Act\""),
        1,
        "the OLD index entry survives, so the index was not cleared"
    );
    lease(&db, |conn| {
        assert!(conn.is_autocommit(), "the lease came back reusable")
    });

    // Same write with the trigger gone: it succeeds with the same id.
    fixture(&db, "DROP TRIGGER fixture_album_insert_fails", &[]);
    let ok = put_committed(&db, &record, &scan_stamp());
    assert!(ok.inserted);
    assert_eq!(ok.id, "al-poison");
    assert_eq!(
        artist_links(&db, "al-poison"),
        ORIGINAL_ARTIST_LINKS.to_vec()
    );
    assert_eq!(fts_count(&db), 2);

    cleanup(db, path);
}

#[test]
fn album_base_update_leg_failure_rolls_back_everything() {
    let (db, path) = fresh_store("base-update");
    seed_graph(&db);
    put_committed(&db, &album_record("al-old"), &scan_stamp());
    fixture(
        &db,
        "CREATE TRIGGER fixture_album_update_fails BEFORE UPDATE ON album \
         FOR EACH ROW WHEN OLD.id = 'al-old' \
         BEGIN SELECT RAISE(ABORT, 'fixture: album base update leg fails'); END",
        &[],
    );

    let mut updated = album_record("al-old");
    updated.name = "Zebracross Renamed".to_string();
    let failed = db.try_with_tx_sync::<AlbumWriteResult, AlbumWriteError>(|tx| {
        put_album_in_tx(tx, &updated, &later_stamp())
    });
    let err = failed.expect_err("the UPDATE leg aborts before anything else runs");
    assert_body_failure(&err, Some(ffi::SQLITE_CONSTRAINT_TRIGGER));

    assert_cell(&db, "name", "al-old", Want::Text("Name-al-old".to_string()));
    assert_cell(
        &db,
        "imported_at",
        "al-old",
        Want::Text(time::to_text(&scan_stamp())),
    );
    assert_eq!(artist_links(&db, "al-old"), ORIGINAL_ARTIST_LINKS.to_vec());
    assert_eq!(
        tag_links(&db, "al-old"),
        original_tag_links(),
        "the tags legs never ran, because the base UPDATE aborted first"
    );
    assert_eq!(fts_count(&db), 1);
    assert_eq!(
        fts_match(&db, "Zebracross"),
        0,
        "the failed rename is not indexed"
    );

    fixture(&db, "DROP TRIGGER fixture_album_update_fails", &[]);
    let ok = put_committed(&db, &updated, &later_stamp());
    assert!(!ok.inserted);
    assert_cell(
        &db,
        "name",
        "al-old",
        Want::Text("Zebracross Renamed".to_string()),
    );

    cleanup(db, path);
}

/// Poison the participants DELETE. The original rows must still be there afterwards, which
/// is what proves the DELETE ran and was rolled back rather than never having run.
#[test]
fn album_participants_delete_failure_rolls_back_base_and_keeps_old_links() {
    let (db, path) = fresh_store("participants-delete");
    seed_graph(&db);
    put_committed(&db, &album_record("al-old"), &scan_stamp());
    fixture(
        &db,
        "CREATE TRIGGER fixture_album_participants_delete_fails BEFORE DELETE ON album_artists \
         FOR EACH ROW WHEN OLD.album_id = 'al-old' \
         BEGIN SELECT RAISE(ABORT, 'fixture: participants delete leg fails'); END",
        &[],
    );

    let mut updated = album_record("al-old");
    updated.name = "Zebracross Renamed".to_string();
    let failed = db.try_with_tx_sync::<AlbumWriteResult, AlbumWriteError>(|tx| {
        put_album_in_tx(tx, &updated, &later_stamp())
    });
    let err = failed.expect_err("the participants DELETE aborts the transaction");
    assert_body_failure(&err, Some(ffi::SQLITE_CONSTRAINT_TRIGGER));

    // The base UPDATE ran BEFORE the failure and rolled back with it.
    assert_cell(&db, "name", "al-old", Want::Text("Name-al-old".to_string()));
    assert_cell(
        &db,
        "imported_at",
        "al-old",
        Want::Text(time::to_text(&scan_stamp())),
    );
    assert_eq!(
        artist_links(&db, "al-old"),
        ORIGINAL_ARTIST_LINKS.to_vec(),
        "ABORT undoes the aborted statement too, so the ORIGINAL rows are back. That the
         fixture trigger fired AT ALL is the proof the DELETE really ran against this album: its
         WHEN clause reads OLD.album_id, which only matches rows the DELETE actually selected."
    );
    assert_eq!(fts_count(&db), 1);
    assert_eq!(fts_match(&db, "\"Second Act\""), 1);
    assert_eq!(fts_match(&db, "Zebracross"), 0);

    fixture(
        &db,
        "DROP TRIGGER fixture_album_participants_delete_fails",
        &[],
    );
    put_committed(&db, &updated, &later_stamp());
    assert_cell(
        &db,
        "name",
        "al-old",
        Want::Text("Zebracross Renamed".to_string()),
    );

    cleanup(db, path);
}

/// Poison the participants INSERT: the base write and the participants DELETE must roll back,
/// and the tags legs (which never ran) must leave nothing partial.
#[test]
fn album_participants_insert_failure_rolls_back_base_and_delete() {
    let (db, path) = fresh_store("participants-insert");
    seed_graph(&db);
    seed_artist(&db, "ar-poison", 1);
    put_committed(&db, &album_record("al-old"), &scan_stamp());
    fixture(
        &db,
        "CREATE TRIGGER fixture_album_participants_insert_fails BEFORE INSERT ON album_artists \
         FOR EACH ROW WHEN NEW.artist_id = 'ar-poison' \
         BEGIN SELECT RAISE(ABORT, 'fixture: participants insert leg fails'); END",
        &[],
    );

    let mut updated = album_record("al-old");
    updated.name = "Zebracross Renamed".to_string();
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
    updated.participant_names = fixture_participant_names(&participants, "");

    let failed = db.try_with_tx_sync::<AlbumWriteResult, AlbumWriteError>(|tx| {
        put_album_in_tx(tx, &updated, &later_stamp())
    });
    let err = failed.expect_err("the participants INSERT aborts the transaction");
    assert_body_failure(&err, Some(ffi::SQLITE_CONSTRAINT_TRIGGER));

    assert_cell(&db, "name", "al-old", Want::Text("Name-al-old".to_string()));
    assert_eq!(
        artist_links(&db, "al-old"),
        ORIGINAL_ARTIST_LINKS.to_vec(),
        "the DELETE that already ran rolled back with the failure"
    );
    assert_eq!(
        tag_links(&db, "al-old"),
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

    fixture(
        &db,
        "DROP TRIGGER fixture_album_participants_insert_fails",
        &[],
    );
    put_committed(&db, &updated, &later_stamp());
    assert_eq!(artist_links(&db, "al-old").len(), 4);

    cleanup(db, path);
}

/// Poison the tags DELETE — the last unconditional leg before the final insert. Everything
/// before it really ran (base write, participants DELETE, participants INSERT), so the record
/// changes BOTH the participant set and a unique indexed token, and the rollback assertions can
/// tell "rolled back" apart from "the rewrite silently never happened".
#[test]
fn album_tags_delete_failure_rolls_back_participants() {
    let (db, path) = fresh_store("tags-delete");
    seed_graph(&db);
    put_committed(&db, &album_record("al-old"), &scan_stamp());
    let before = state(&db, "al-old");
    // The WHEN clause demands that the NEW conductor link already exists in this transaction,
    // so the abort can ONLY happen if the participants INSERT leg really executed first — the
    // later failure itself proves Go's leg order (base → participants → tags).
    fixture(
        &db,
        "CREATE TRIGGER fixture_album_tags_delete_fails BEFORE DELETE ON album_tags \
         FOR EACH ROW WHEN OLD.album_id = 'al-old' AND EXISTS (\
             SELECT 1 FROM album_artists WHERE album_id = 'al-old' \
               AND artist_id = 'ar-third' AND role = 'conductor') \
         BEGIN SELECT RAISE(ABORT, 'fixture: tags delete leg fails'); END",
        &[],
    );

    // Base token + participant set BOTH change: the new name token and the new participant
    // token are absent from the index before the call, and the old ones present.
    assert_eq!(fts_match(&db, "\"Name-al-old\""), 1, "old name indexed");
    assert_eq!(
        fts_match(&db, "\"Second Act\""),
        1,
        "old participant indexed"
    );
    assert_eq!(fts_match(&db, "Zebracross"), 0, "new name absent");
    assert_eq!(fts_match(&db, "\"Third Act\""), 0, "new participant absent");
    assert_eq!(
        artist_links(&db, "al-old"),
        ORIGINAL_ARTIST_LINKS.to_vec(),
        "baseline participant set"
    );
    assert_eq!(
        tag_links(&db, "al-old"),
        original_tag_links(),
        "baseline tag set"
    );

    let mut updated = album_record("al-old");
    updated.name = "Zebracross Renamed".to_string();
    let participants = replaced_participants();
    updated.participants = participants.clone();
    updated.participant_names = fixture_participant_names(&participants, "");
    let failed = db.try_with_tx_sync::<AlbumWriteResult, AlbumWriteError>(|tx| {
        put_album_in_tx(tx, &updated, &later_stamp())
    });
    let err = failed.expect_err("the tags DELETE aborts the transaction");
    assert_body_failure(&err, Some(ffi::SQLITE_CONSTRAINT_TRIGGER));

    // The participants DELETE+INSERT had ALREADY run inside this transaction; both roll back.
    assert_eq!(state(&db, "al-old"), before, "whole-transaction rollback");
    assert_cell(&db, "name", "al-old", Want::Text("Name-al-old".to_string()));
    assert_cell(
        &db,
        "imported_at",
        "al-old",
        Want::Text(time::to_text(&scan_stamp())),
    );
    assert_eq!(
        artist_links(&db, "al-old"),
        ORIGINAL_ARTIST_LINKS.to_vec(),
        "the executed participants rewrite came back to the ORIGINAL set"
    );
    assert_eq!(
        tag_links(&db, "al-old"),
        original_tag_links(),
        "the rolled-back tags DELETE left the ORIGINAL link rows"
    );
    assert_eq!(fts_count(&db), 1);
    assert_eq!(
        fts_match(&db, "\"Name-al-old\""),
        1,
        "old name still indexed"
    );
    assert_eq!(
        fts_match(&db, "\"Second Act\""),
        1,
        "old participant text still indexed"
    );
    assert_eq!(
        fts_match(&db, "Zebracross"),
        0,
        "the failed rename is not indexed"
    );
    assert_eq!(
        fts_match(&db, "\"Third Act\""),
        0,
        "the failed participant rewrite is not indexed"
    );
    lease(&db, |conn| {
        assert!(conn.is_autocommit(), "the lease came back reusable")
    });

    fixture(&db, "DROP TRIGGER fixture_album_tags_delete_fails", &[]);
    let ok = put_committed(&db, &updated, &later_stamp());
    assert!(!ok.inserted);
    assert_cell(
        &db,
        "name",
        "al-old",
        Want::Text("Zebracross Renamed".to_string()),
    );
    assert_eq!(
        artist_links(&db, "al-old"),
        REPLACED_ARTIST_LINKS.to_vec(),
        "with the trigger gone, the participants rewrite lands"
    );

    cleanup(db, path);
}

/// Poison the tags INSERT, i.e. the LAST leg: everything before it — base write, both
/// participants legs, and the tags DELETE — rolls back. The record changes the base token, the
/// participant set, and the tag set, so each rolled-back leg is distinguishable from a leg that
/// never ran.
#[test]
fn album_tags_insert_failure_rolls_back_base_participants_and_tags_delete() {
    let (db, path) = fresh_store("tags-insert");
    seed_graph(&db);
    let poison_tag = seed_tag(&db, 1, "genre", "Poison");
    put_committed(&db, &album_record("al-old"), &scan_stamp());
    let before = state(&db, "al-old");
    // Same ordering proof as the DELETE case: the trigger can only fire once the new
    // conductor link is present in this transaction, i.e. after the participants INSERT ran.
    fixture(
        &db,
        &format!(
            "CREATE TRIGGER fixture_album_tags_insert_fails BEFORE INSERT ON album_tags \
             FOR EACH ROW WHEN NEW.tag_id = '{poison_tag}' AND EXISTS (\
                 SELECT 1 FROM album_artists WHERE album_id = 'al-old' \
                   AND artist_id = 'ar-third' AND role = 'conductor') \
             BEGIN SELECT RAISE(ABORT, 'fixture: tags insert leg fails'); END"
        ),
        &[],
    );

    // Positive baseline for every token the failing Put would add.
    assert_eq!(fts_match(&db, "\"Name-al-old\""), 1, "old name indexed");
    assert_eq!(
        fts_match(&db, "\"Second Act\""),
        1,
        "old participant indexed"
    );
    assert_eq!(fts_match(&db, "Zebracross"), 0, "new name absent");
    assert_eq!(fts_match(&db, "\"Third Act\""), 0, "new participant absent");
    assert_eq!(
        artist_links(&db, "al-old"),
        ORIGINAL_ARTIST_LINKS.to_vec(),
        "baseline participant set"
    );
    assert_eq!(
        tag_links(&db, "al-old"),
        original_tag_links(),
        "baseline tag set"
    );

    let mut updated = album_record("al-old");
    updated.name = "Zebracross Renamed".to_string();
    let participants = replaced_participants();
    updated.participants = participants.clone();
    updated.participant_names = fixture_participant_names(&participants, "");
    let mut tags = fixture_tags();
    tags.insert(
        "genre".to_string(),
        vec!["Rock".to_string(), "Poison".to_string()],
    );
    updated.tags = tags;

    let failed = db.try_with_tx_sync::<AlbumWriteResult, AlbumWriteError>(|tx| {
        put_album_in_tx(tx, &updated, &later_stamp())
    });
    let err = failed.expect_err("the tags INSERT aborts the transaction");
    assert_body_failure(&err, Some(ffi::SQLITE_CONSTRAINT_TRIGGER));

    assert_eq!(state(&db, "al-old"), before, "whole-transaction rollback");
    assert_cell(&db, "name", "al-old", Want::Text("Name-al-old".to_string()));
    assert_cell(
        &db,
        "imported_at",
        "al-old",
        Want::Text(time::to_text(&scan_stamp())),
    );
    assert_eq!(
        artist_links(&db, "al-old"),
        ORIGINAL_ARTIST_LINKS.to_vec(),
        "the executed participants rewrite came back to the ORIGINAL set"
    );
    assert_eq!(
        tag_links(&db, "al-old"),
        original_tag_links(),
        "the rolled-back tags DELETE left the ORIGINAL link rows"
    );
    assert_eq!(fts_count(&db), 1);
    assert_eq!(
        fts_match(&db, "\"Name-al-old\""),
        1,
        "old name still indexed"
    );
    assert_eq!(
        fts_match(&db, "\"Second Act\""),
        1,
        "old participant text still indexed"
    );
    assert_eq!(
        fts_match(&db, "Zebracross"),
        0,
        "the failed rename is not indexed"
    );
    assert_eq!(
        fts_match(&db, "\"Third Act\""),
        0,
        "the failed participant rewrite is not indexed"
    );
    assert!(
        !tag_links(&db, "al-old").contains(&poison_tag),
        "the failed tag did not land"
    );
    lease(&db, |conn| {
        assert!(conn.is_autocommit(), "the lease came back reusable")
    });

    fixture(&db, "DROP TRIGGER fixture_album_tags_insert_fails", &[]);
    let ok = put_committed(&db, &updated, &later_stamp());
    assert!(!ok.inserted);
    assert!(tag_links(&db, "al-old").contains(&poison_tag));
    assert_eq!(
        artist_links(&db, "al-old"),
        REPLACED_ARTIST_LINKS.to_vec(),
        "with the trigger gone, the participants rewrite lands"
    );

    cleanup(db, path);
}

/// The scout's FTS claim, checked against the real trigger DDL rather than taken as gospel.
/// A malformed `album.tags` cell makes `album_fts_au`'s
/// `json_each(NEW.tags, '$.albumversion')` fail (`schema_ddl.rs:87` has no `json_valid`
/// guard): the statement aborts and the caller's transaction rolls back. The class is
/// `SQLITE_ERROR` (`malformed JSON`), NOT `SQLITE_CONSTRAINT_TRIGGER` — and a fixture
/// `RAISE(ABORT)` on the same table DOES produce 1811, so the distinction is real rather than
/// an artifact of how rusqlite reports any trigger failure. This is a raw-DDL fact: the typed
/// write path cannot produce a malformed tags cell because `sql_tags::marshal_tags` always
/// emits valid JSON, so nothing injects invalid JSON into a typed `DbTags` here.
/// `runtime_marshal.rs:1185-1197` is the precedent for needing `DROP TRIGGER` to land a
/// deliberately corrupted row.
#[test]
fn album_malformed_tags_cell_is_a_sqlite_error_not_a_trigger_class() {
    let (db, path) = fresh_store("malformed-tags");
    seed_graph(&db);
    put_committed(&db, &album_record("al-old"), &scan_stamp());
    assert_eq!(fts_count(&db), 1);
    assert_eq!(
        fts_match(&db, "Deluxe"),
        1,
        "precondition: album_version is indexed"
    );

    let tags_before = scalar_bound::<String>(&db, "SELECT tags FROM album WHERE id = ?", "al-old");
    let before = state(&db, "al-old");

    let err = raw_in_tx(
        &db,
        "UPDATE album SET tags = '{bad' WHERE id = ?",
        vec![Value::from("al-old")],
    );
    let (primary, extended, message) = body_codes(&err);
    assert_eq!(
        primary,
        Some(ffi::SQLITE_ERROR),
        "EXECUTED-Rust: malformed JSON is the SQLITE_ERROR class, not a trigger class"
    );
    assert_ne!(
        extended,
        Some(ffi::SQLITE_CONSTRAINT_TRIGGER),
        "corrected scout claim: 1811 belongs to RAISE(ABORT), not to this failure"
    );
    assert!(
        message.contains("malformed JSON"),
        "the concrete message is retained: {message}"
    );

    assert_eq!(
        state(&db, "al-old"),
        before,
        "the whole transaction rolled back: cell, junctions and index are unchanged"
    );
    assert_eq!(
        scalar_bound::<String>(&db, "SELECT tags FROM album WHERE id = ?", "al-old"),
        tags_before,
        "the tags cell still parses, i.e. the corrupt write never landed"
    );
    assert_eq!(
        fts_match(&db, "Deluxe"),
        1,
        "the OLD index entry is still searchable"
    );
    lease(&db, |conn| {
        assert!(conn.is_autocommit(), "the lease came back reusable")
    });

    // Contrast: a fixture RAISE(ABORT) trigger on the same table IS the trigger class.
    fixture(
        &db,
        "CREATE TRIGGER fixture_trigger_class BEFORE UPDATE ON album \
         FOR EACH ROW WHEN OLD.id = 'al-old' \
         BEGIN SELECT RAISE(ABORT, 'fixture: trigger class'); END",
        &[],
    );
    let triggered = raw_in_tx(
        &db,
        "UPDATE album SET name = ? WHERE id = ?",
        vec![Value::from("Trigger Class Name"), Value::from("al-old")],
    );
    assert_body_failure(&triggered, Some(ffi::SQLITE_CONSTRAINT_TRIGGER));
    fixture(&db, "DROP TRIGGER fixture_trigger_class", &[]);
    assert_cell(&db, "name", "al-old", Want::Text("Name-al-old".to_string()));

    cleanup(db, path);
}

#[test]
fn album_adversarial_and_unicode_names_are_bound_and_fts_consistent() {
    let (db, path) = fresh_store("unicode");
    seed_graph(&db);

    // Values that would break a statement that interpolated instead of binding.
    let mut record = album_record("al-adv");
    record.name = "Odyssia \"Sigma\" 'quote' \\ back".to_string();
    record.album_artist = "İstanbul Act".to_string();
    record.participant_names = fixture_participant_names(&record.participants, "İstanbul");

    let result = put_committed(&db, &record, &scan_stamp());
    assert!(result.inserted);
    assert_cell(&db, "name", "al-adv", Want::Text(record.name.clone()));
    assert_cell(
        &db,
        "album_artist",
        "al-adv",
        Want::Text("İstanbul Act".to_string()),
    );
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM album"), 1);
    assert_eq!(artist_links(&db, "al-adv"), ORIGINAL_ARTIST_LINKS.to_vec());

    // The derived columns are the deterministic PostMapArgs outputs, and the index reflects
    // them through the schema's own triggers.
    let search = scalar_bound::<String>(
        &db,
        "SELECT search_normalized FROM album WHERE id = ?",
        "al-adv",
    );
    assert!(
        !search.is_empty(),
        "search_normalized is derived, not left empty"
    );
    assert_eq!(fts_count(&db), 1);
    assert_eq!(
        fts_match(&db, "İstanbul"),
        1,
        "the participant text is indexed"
    );

    let got = read_back(&db, "al-adv", "u-one");
    assert_eq!(got.name, record.name, "the adversarial name round-trips");

    cleanup(db, path);
}

#[test]
fn album_legacy_renderer_bundle_stays_compatible() {
    let (db, path) = fresh_store("legacy");
    seed_graph(&db);

    // The renderer seam that runtime_marshal.rs drives still produces the exact
    // six-statement bundle, so DB-09's wrapper is a consumer of it, not a replacement.
    let record = album_record("al-legacy");
    let bundle = put_sql("al-legacy", &record, scan_stamp(), &[]).expect("renders");
    assert!(bundle.update.0.starts_with("UPDATE album SET "));
    assert!(bundle.update.0.ends_with(" WHERE id = ?"));
    assert!(bundle.insert.0.starts_with("INSERT INTO album ("));
    assert!(!bundle.update.0.contains("created_at = ?"), "UPDATE_DROP");
    assert_eq!(
        bundle.participants_delete.0,
        "DELETE FROM album_artists WHERE album_id = ?"
    );
    assert_eq!(bundle.participants_delete.1, vec![Value::from("al-legacy")]);
    assert_eq!(
        bundle.tags_delete.0,
        "DELETE FROM album_tags WHERE album_id = ?"
    );
    assert_eq!(bundle.tags_delete.1, vec![Value::from("al-legacy")]);
    assert!(bundle.participants_insert.is_some());
    assert!(bundle.tags_insert.is_some());

    // Driving those legs by hand and driving the wrapper persist the same state.
    put_committed(&db, &record, &scan_stamp());
    assert_eq!(
        artist_links(&db, "al-legacy"),
        ORIGINAL_ARTIST_LINKS.to_vec()
    );
    assert_eq!(tag_links(&db, "al-legacy"), original_tag_links());

    cleanup(db, path);
}
