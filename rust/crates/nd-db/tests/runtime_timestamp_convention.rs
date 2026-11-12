//! Timestamp storage EXECUTED against a real SQLite DB: fresh schema via
//! `nd_dbschema`, one isolated temp file per test — the harness shape of
//! `tests/runtime_player_repo.rs:34-46` (`apply_fresh_schema` + `Pool::new` +
//! `SQLStore::with_connection_sync`, `_foreign_keys=on` per `db/db.go:55`).
//!
//! What is under test:
//!
//! * round-trip through the real schema for `share`, `radio`, and `artist` —
//!   write, read back, compare the chrono values (not string assertions);
//! * every timestamp column a repository writes keeps `typeof() = text`;
//! * the non-regression case for the bug this port had: a TEXT-bound range
//!   predicate (`Gt{"media_file.created_at": since}`,
//!   `persistence/mediafile_repository.go:533,559`; `LtOrEq{"retry_at":
//!   time.Now()}`, `artwork_queue_repository.go:167`) must INCLUDE a row
//!   written through `crate::time`. Its non-vacuity arm writes the same
//!   instant as an epoch integer and shows the identical predicate EXCLUDES it.
//!
//! Fixture paths are never absolute: `store()` uses
//! `nd_utils::files::temp_file_name` (the existing harness precedent), and the
//! fresh DDL comes from `nd_dbschema` rather than a checked-in file.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use nd_db::artist_repo::{self, ArtistRecord, EXTERNAL_INFO_COLS};
use nd_db::artwork_queue_repo;
use nd_db::radio_repo::{self, RadioRecord};
use nd_db::share_repo::{ShareError, ShareRecord, ShareRepo, SHARE_TABLE};
use nd_db::sqlrepo::Value;
use nd_db::store::SQLStore;
use nd_db::{
    pool::{Pool, PoolConfig},
    time,
};
use nd_dbschema::schema::apply_fresh_schema;

/// Deterministic caller clock: Go's `time.Now()` at `share_repository.go:176`,
/// `radio_repository.go:90`, `artist_repository.go:240`.
fn at(year: u32, month: u32, day: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::from_ymd_opt(i32::try_from(year).unwrap(), month, day)
            .unwrap()
            .and_hms_opt(h, mi, s)
            .unwrap(),
    )
}

const T0: &str = "2024-01-01 00:00:00+00:00";

/// Execute a `(sql, args)` pair produced by a repository, through the same
/// `Value` → `rusqlite::types::Value` mapping the repositories use.
fn exec(store: &SQLStore, sql: &str, args: &[Value]) -> usize {
    run(store, |conn| {
        let count = conn.execute(sql, rusqlite::params_from_iter(args.iter().map(value_ref)))?;
        Ok(count)
    })
}

/// The UPDATE leg of Go's `put`: a no-match is a legitimate zero-row result,
/// so a failure other than "no rows" still surfaces (`sql_base_repository.go:619-627`).
fn exec_optional(store: &SQLStore, sql: &str, args: &[Value]) -> usize {
    run(store, |conn| {
        let count = conn.execute(sql, rusqlite::params_from_iter(args.iter().map(value_ref)))?;
        Ok(count)
    })
}

/// `rusqlite::types::Value` for a [`Value`] — the public repositories repeat
/// this mapping internally (`share_repo::bind_values`), so the test drives the
/// statements it produces without reaching into private helpers.
fn value_ref(value: &Value) -> rusqlite::types::Value {
    match value {
        Value::Text(s) => rusqlite::types::Value::Text(s.clone()),
        Value::Integer(i) => rusqlite::types::Value::Integer(*i),
        Value::Real(x) => rusqlite::types::Value::Real(*x),
        Value::Bool(b) => rusqlite::types::Value::Integer(i64::from(*b)),
        Value::Null => rusqlite::types::Value::Null,
    }
}

/// One isolated on-disk DB per invocation: a unique temp path, freshly
/// migrated, pooled through `nd_db::pool::Pool` so `SEEDEDRAND`/`NATSORT` are
/// registered per connection. `_foreign_keys=on` is Go's `db/db.go:55` DSN
/// parameter, so `share.user_id`'s CASCADE really runs.
fn store() -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-db-timestamp-", ".db"));
    if path.exists() {
        std::fs::remove_file(&path)
            .unwrap_or_else(|e| panic!("cannot clear stale {:?}: {e}", path));
    }
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let pool = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    apply_fresh_schema(pool.checkout().unwrap().conn()).unwrap();
    (SQLStore::new(pool), path)
}

/// Run a repository call on one pooled lease — verbatim the precedent of
/// `tests/runtime_player_repo.rs:125-132`.
fn on<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("store lease failed: {e}"))
}

/// Run a raw rusqlite block on one pooled lease, panicking on lease or
/// statement failure.
fn run<T>(
    store: &SQLStore,
    call: impl FnOnce(&rusqlite::Connection) -> Result<T, rusqlite::Error>,
) -> T {
    store
        .with_connection_sync(call)
        .unwrap_or_else(|e| panic!("store lease or statement failed: {e}"))
}

fn cleanup(store: SQLStore, path: std::path::PathBuf) {
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// `user_repo.rs` owns user creation, so tests seed the owner row directly.
fn insert_user(store: &SQLStore, id: &str) {
    let stamp = time::value(&at(2024, 1, 1, 0, 0, 0));
    run(store, |conn| {
        conn.execute(
            "INSERT INTO user(id, user_name, name, email, password, is_admin, \
             created_at, updated_at) VALUES(?,?,?,?,?,?,?,?)",
            rusqlite::params![
                id,
                id,
                id,
                format!("{id}@example.com"),
                "encrypted",
                Value::from(true),
                stamp.clone(),
                stamp
            ],
        )
        .unwrap_or_else(|e| panic!("user insert failed: {e}"));
        Ok(())
    });
}

/// `typeof()` of one column of one row, straight from SQLite.
fn typeof_column(store: &SQLStore, table: &str, column: &str, key: &str) -> String {
    on(store, |conn| {
        conn.query_row(
            &format!("SELECT typeof({column}) FROM {table} WHERE id = ?"),
            rusqlite::params![key],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|e| panic!("{table}.{column} typeof read failed: {e}"))
    })
}

fn stored_text(store: &SQLStore, table: &str, column: &str, key: &str) -> Option<String> {
    on(store, |conn| {
        conn.query_row(
            &format!("SELECT {column} FROM {table} WHERE id = ?"),
            rusqlite::params![key],
            |row| row.get::<_, Option<String>>(0),
        )
        .unwrap_or_else(|e| panic!("{table}.{column} read failed: {e}"))
    })
}

/// The queue table's key is `(item_kind, item_id, image_type)`, so the two
/// timestamp columns are read through that key rather than `id`.
fn queue_text(store: &SQLStore, column: &str, kind: &str, item_id: &str) -> Option<String> {
    on(store, |conn| {
        conn.query_row(
            &format!(
                "SELECT {column} FROM artwork_queue \
                 WHERE item_kind = ? AND item_id = ? AND image_type = 'primary'"
            ),
            rusqlite::params![kind, item_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
    })
}

fn share_record(id: &str, expires_at: Option<DateTime<Utc>>) -> ShareRecord {
    ShareRecord {
        id: id.to_string(),
        user_id: "u1".to_string(),
        expires_at,
        last_visited_at: None,
        created_at: at(2024, 1, 1, 0, 0, 0),
        updated_at: at(2024, 1, 1, 0, 0, 0),
        resource_ids: "al-1".to_string(),
        resource_type: "album".to_string(),
        contents: "Some Album".to_string(),
        format: "raw".to_string(),
        downloadable: false,
        description: "desc".to_string(),
        max_bit_rate: 0,
        visit_count: 0,
    }
}

fn radio_record(id: &str) -> RadioRecord {
    RadioRecord {
        id: id.to_string(),
        stream_url: "https://example.com/stream".to_string(),
        name: format!("Radio {id}"),
        home_page_url: "https://example.com".to_string(),
        uploaded_image: String::new(),
        created_at: at(2024, 1, 1, 0, 0, 0),
        updated_at: at(2024, 1, 1, 0, 0, 0),
    }
}

fn artist_record(id: &str) -> ArtistRecord {
    ArtistRecord {
        id: id.to_string(),
        name: "Eddie Van Halen".to_string(),
        order_artist_name: "Eddie Van Halen".to_string(),
        ..ArtistRecord::default()
    }
}

/// `ShareRepo::save`/`get` (`share_repository.go:168-179,26-33`) round-trip
/// through the real DDL: the datetimes come back as the same instants and the
/// optional ones as NULL (Go's nil `*time.Time`), never as integers.
#[test]
fn share_timestamps_round_trip_and_stay_text() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1");
    let expires = at(2024, 2, 1, 12, 0, 0);
    // Act: the repository's own Save (`share_repository.go:168-179`).
    let record = share_record("tok2", Some(expires));
    on(&store, |conn| ShareRepo::new(conn).save(&record)).expect("repo save");
    // Assert: TEXT in rusqlite's exact shape (space separator, +00:00 offset),
    // byte-identical to what the helper documents.
    assert_eq!(
        typeof_column(&store, SHARE_TABLE, "expires_at", "tok2"),
        "text"
    );
    assert_eq!(
        typeof_column(&store, SHARE_TABLE, "created_at", "tok2"),
        "text"
    );
    assert_eq!(
        stored_text(&store, SHARE_TABLE, "expires_at", "tok2").as_deref(),
        Some("2024-02-01 12:00:00+00:00")
    );
    assert_eq!(
        stored_text(&store, SHARE_TABLE, "expires_at", "tok2").as_deref(),
        Some(time::to_text(&expires).as_str()),
        "the stored text is the helper's shape, i.e. a direct bind's shape"
    );
    // Assert: read back through the repository as instants, not strings.
    let got = on(&store, |conn| ShareRepo::new(conn).get("tok2", None)).expect("share read");
    assert_eq!(got.share.expires_at, Some(expires));
    assert_eq!(got.share.created_at, at(2024, 1, 1, 0, 0, 0));
    assert_eq!(got.share.last_visited_at, None, "NULL is Go's nil");
    // Act: the visit bump (`core/share.go:48-51`) re-stamps through the
    // helper. The clock is BEFORE the 2024-02-01 12:00 expiry, so the token is
    // still live.
    let now = at(2024, 2, 1, 6, 0, 0);
    let (_, bumped) =
        on(&store, |conn| ShareRepo::new(conn).load("tok2", now, None)).expect("live token loads");
    // Assert:
    assert!(bumped, "the visit UPDATE ran");
    let reloaded = on(&store, |conn| ShareRepo::new(conn).get("tok2", None)).expect("re-read");
    assert_eq!(reloaded.share.last_visited_at, Some(now));
    assert_eq!(reloaded.share.updated_at, now);
    assert_eq!(reloaded.share.visit_count, 1);
    assert_eq!(
        typeof_column(&store, SHARE_TABLE, "last_visited_at", "tok2"),
        "text",
        "the bumped stamp is TEXT too"
    );
    // Assert: a NULL expiry loads at any clock (core/share.go:44 IsZero arm).
    on(&store, |conn| {
        ShareRepo::new(conn).save(&share_record("tok3", None))
    })
    .expect("never-expires save");
    assert_eq!(
        stored_text(&store, SHARE_TABLE, "expires_at", "tok3"),
        None,
        "None binds NULL, not the old 0 sentinel"
    );
    assert!(on(&store, |conn| ShareRepo::new(conn).load(
        "tok3",
        at(2099, 1, 1, 0, 0, 0),
        None
    ))
    .is_ok());
    // Assert: an expiry in the past is Expired, a future one loads.
    assert!(matches!(
        on(&store, |conn| ShareRepo::new(conn).load(
            "tok2",
            at(2024, 12, 1, 0, 0, 0),
            None
        )),
        Err(ShareError::Expired)
    ));
    assert!(on(&store, |conn| ShareRepo::new(conn).load(
        "tok2",
        at(2024, 2, 1, 11, 0, 0),
        None
    ))
    .is_ok());
    cleanup(store, path);
}

/// `radio_repo::put_sql` (`radio_repository.go:85-110`) executed on the real
/// schema: the UPDATE leg misses for a minted id so the INSERT leg runs, and
/// both stamps plus the artwork enqueue's `retry_at`/`enqueued_at` land TEXT.
#[test]
fn radio_put_round_trips_and_the_enqueue_is_text_too() {
    let (store, path) = store();
    let now = at(2024, 5, 1, 9, 0, 0);
    let put = radio_repo::put_sql(&radio_record(""), now, &[]);
    // Act: Go's `put` order — UPDATE first, INSERT when it matched no row
    // (`sql_base_repository.go:619-635`).
    assert_eq!(
        exec_optional(&store, &put.update.0, &put.update.1),
        0,
        "a minted id cannot match an existing row"
    );
    exec(&store, &put.insert.0, &put.insert.1);
    exec(&store, &put.enqueue_artwork.0, &put.enqueue_artwork.1);
    // Assert: the fresh row got created_at = now and updated_at = now
    // (radio_repository.go:90-92), both TEXT.
    assert_eq!(
        stored_text(&store, radio_repo::TABLE, "created_at", &put.id).as_deref(),
        Some("2024-05-01 09:00:00+00:00")
    );
    assert_eq!(
        stored_text(&store, radio_repo::TABLE, "updated_at", &put.id).as_deref(),
        Some("2024-05-01 09:00:00+00:00")
    );
    assert_eq!(
        typeof_column(&store, radio_repo::TABLE, "created_at", &put.id),
        "text"
    );
    assert_eq!(
        typeof_column(&store, radio_repo::TABLE, "updated_at", &put.id),
        "text"
    );
    // An existing row keeps its created_at and only gets updated_at re-stamped.
    let existing = radio_record(&put.id);
    let second = radio_repo::put_sql(&existing, at(2024, 5, 2, 9, 0, 0), &["name"]);
    assert_eq!(exec(&store, &second.update.0, &second.update.1), 1);
    assert_eq!(
        stored_text(&store, radio_repo::TABLE, "created_at", &put.id).as_deref(),
        Some("2024-05-01 09:00:00+00:00"),
        "created_at is never overwritten (sql_base_repository.go:601-604)"
    );
    assert_eq!(
        stored_text(&store, radio_repo::TABLE, "updated_at", &put.id).as_deref(),
        Some("2024-05-02 09:00:00+00:00")
    );
    // Assert: the queue row the save enqueued carries the same TEXT clock.
    assert_eq!(
        queue_text(&store, "retry_at", "ra", &put.id).as_deref(),
        Some("2024-05-01 09:00:00+00:00")
    );
    assert_eq!(
        queue_text(&store, "enqueued_at", "ra", &put.id).as_deref(),
        Some("2024-05-01 09:00:00+00:00")
    );
    // Act: `DequeueBatch`'s TEXT-bound eligibility bound (Go's
    // `LtOrEq{"retry_at": time.Now()}`, artwork_queue_repository.go:167) must
    // find that row. With an integer-stamped queue row it would be invisible.
    let queued = on(&store, |conn| {
        let (sql, args) = artwork_queue_repo::dequeue_batch_sql(
            10,
            &at(2024, 5, 1, 10, 0, 0),
            &["ra".to_string()],
        );
        conn.query_row(
            &sql,
            rusqlite::params_from_iter(args.iter().map(value_ref)),
            |r| r.get::<_, String>(1),
        )
        .ok()
    });
    assert_eq!(
        queued.as_deref(),
        Some(put.id.as_str()),
        "a TEXT-bound retry_at <= ? includes the row the radio save queued"
    );
    // Assert: an eligibility bound BEFORE the stamp finds nothing (the bound
    // really discriminates, so the assertion above is not vacuous).
    let too_early = on(&store, |conn| {
        let (sql, args) = artwork_queue_repo::dequeue_batch_sql(
            10,
            &at(2024, 4, 30, 0, 0, 0),
            &["ra".to_string()],
        );
        conn.query_row(
            &sql,
            rusqlite::params_from_iter(args.iter().map(value_ref)),
            |r| r.get::<_, String>(1),
        )
        .ok()
    });
    assert_eq!(too_early, None, "a bound before retry_at excludes the row");
    cleanup(store, path);
}

/// `artist_repo::put_sql` (`artist_repository.go:239-253`) executed: `Put`
/// stamps `CreatedAt = new(time.Now())`, `UpdatedAt = CreatedAt` (:240-241),
/// `UpdateExternalInfo` (:247-253) re-stamps `updated_at` and
/// `external_info_updated_at` while leaving `created_at` alone.
#[test]
fn artist_put_stamps_both_and_update_external_info_keeps_created_at() {
    let (store, path) = store();
    let now = at(2024, 4, 1, 6, 0, 0);
    let (update, insert) = artist_repo::put_sql(&artist_record("ar-1"), now, &[]);
    // Act: UPDATE misses (no row), so the INSERT leg runs — Go's `put`.
    assert_eq!(exec_optional(&store, &update.0, &update.1), 0);
    exec(&store, &insert.0, &insert.1);
    // Assert: both stamps are `now`, TEXT; unset external_info_updated_at is NULL.
    assert_eq!(
        typeof_column(&store, artist_repo::TABLE, "created_at", "ar-1"),
        "text"
    );
    assert_eq!(
        stored_text(&store, artist_repo::TABLE, "created_at", "ar-1").as_deref(),
        Some("2024-04-01 06:00:00+00:00")
    );
    assert_eq!(
        stored_text(&store, artist_repo::TABLE, "updated_at", "ar-1").as_deref(),
        Some("2024-04-01 06:00:00+00:00")
    );
    assert_eq!(
        stored_text(
            &store,
            artist_repo::TABLE,
            "external_info_updated_at",
            "ar-1"
        ),
        None,
        "Go's nil *time.Time is NULL, not the old 0 sentinel"
    );
    // Act: UpdateExternalInfo — the 7-column list plus the appended
    // updated_at, never created_at.
    let later = at(2024, 4, 2, 6, 0, 0);
    let mut stamped = artist_record("ar-1");
    stamped.biography = "Born in Pasadena".to_string();
    stamped.external_info_updated_at = Some(later);
    let (update, _) = artist_repo::put_sql(&stamped, later, &EXTERNAL_INFO_COLS);
    // Assert:
    assert_eq!(
        exec(&store, &update.0, &update.1),
        1,
        "the existing row matched the UPDATE leg"
    );
    assert_eq!(
        stored_text(&store, artist_repo::TABLE, "created_at", "ar-1").as_deref(),
        Some("2024-04-01 06:00:00+00:00"),
        "created_at is excluded from the UPDATE (sql_base_repository.go:601-604)"
    );
    assert_eq!(
        stored_text(&store, artist_repo::TABLE, "updated_at", "ar-1").as_deref(),
        Some("2024-04-02 06:00:00+00:00")
    );
    assert_eq!(
        stored_text(
            &store,
            artist_repo::TABLE,
            "external_info_updated_at",
            "ar-1"
        )
        .as_deref(),
        Some("2024-04-02 06:00:00+00:00")
    );
    assert_eq!(
        typeof_column(
            &store,
            artist_repo::TABLE,
            "external_info_updated_at",
            "ar-1"
        ),
        "text"
    );
    // Assert: RefreshStats' id select (`updated_at > …`,
    // artist_repository.go:479) over an explicit TEXT bound sees the touched
    // row — the same predicate shape Go's `RefreshStats` uses.
    let touched = on(&store, |conn| {
        conn.query_row(
            &format!(
                "SELECT count(*) FROM {} WHERE updated_at > ?",
                artist_repo::TABLE
            ),
            rusqlite::params![time::value(&at(2024, 4, 1, 0, 0, 0))],
            |r| r.get::<_, i64>(0),
        )
        .expect("refresh-stats style select runs")
    });
    assert_eq!(touched, 1, "the re-stamped row is included by a TEXT bound");
    cleanup(store, path);
}

/// The bug this port had, executed twice in one database: a row written
/// through `crate::time` is INCLUDED by a TEXT-bound range predicate, and a
/// row written as an epoch integer — the shape the port used before — is
/// EXCLUDED by the identical predicate. Same column, same bound, same SQL, so
/// the positive assertion is provably non-vacuous.
#[test]
fn text_bound_range_predicate_includes_helper_rows_and_excludes_integers() {
    let (store, path) = store();
    // Arrange: the Go writer's column and predicate shape —
    // `Gt{"media_file.created_at": since}` (mediafile_repository.go:533,559)
    // against a `created_at datetime` column (schema_ddl.rs:64).
    run(&store, |conn| {
        conn.execute(
            "CREATE TABLE probe (id text primary key, created_at datetime)",
            [],
        )?;
        Ok(())
    });
    let written = at(2024, 1, 1, 0, 0, 0);
    // Act 1: write through the shared helper (Value::Text, rusqlite's shape).
    exec(
        &store,
        "INSERT INTO probe (id, created_at) VALUES (?, ?)",
        &[Value::Text("helper".to_string()), time::value(&written)],
    );
    // Assert 1: the TEXT-bound Gt includes it.
    assert_eq!(
        probe_gt(&store, "helper"),
        Some("helper".to_string()),
        "a helper-written row must satisfy created_at > a TEXT bound"
    );
    // Act 2: the pre-convention shape — the same instant as an epoch integer.
    let epoch_seconds = written.timestamp();
    exec(
        &store,
        "INSERT INTO probe (id, created_at) VALUES (?, ?)",
        &[
            Value::Text("integer".to_string()),
            Value::Integer(epoch_seconds),
        ],
    );
    // Assert 2: typeof really stayed integer — INTEGER beats datetime affinity.
    assert_eq!(
        typeof_column(&store, "probe", "created_at", "integer"),
        "integer"
    );
    // Assert 3: the IDENTICAL predicate now excludes that row. This is the
    // non-vacuity evidence for assertion 1.
    assert_eq!(
        probe_gt(&store, "integer"),
        None,
        "an integer-stored row is invisible to a TEXT-bound range predicate"
    );
    // Assert 4: the same defect in the ORDER BY direction Go relies on.
    // INTEGER sorts BEFORE TEXT, so an integer-stored instant is non-chronological
    // in BOTH directions: add an integer row holding a much LATER instant than
    // the TEXT row and it comes first ascending / last descending, whichever way
    // the clock is read. (The parent's brief said DESC puts integer rows first;
    // executed against SQLite, DESC puts them LAST and ASC puts them FIRST —
    // either way the order is wrong, which is what these two assertions pin.)
    // Drop the equal-instant row so the ordering arm compares one TEXT row
    // (2024) against one INTEGER row holding a much newer instant (2100).
    run(&store, |conn| {
        conn.execute("DELETE FROM probe WHERE id = 'integer'", [])?;
        Ok(())
    });
    let future = at(2100, 1, 1, 0, 0, 0);
    exec(
        &store,
        "INSERT INTO probe (id, created_at) VALUES (?, ?)",
        &[
            Value::Text("integer_future".to_string()),
            Value::Integer(future.timestamp()),
        ],
    );
    assert_eq!(
        first_asc(&store),
        "integer_future",
        "ascending puts the integer row first even though it holds the NEWEST instant"
    );
    assert_eq!(
        first_desc(&store),
        "helper",
        "descending puts the TEXT row first, so the integer-stored newest row is never first"
    );
    // Assert 5: with only TEXT rows the chronology is right in both directions.
    run(&store, |conn| {
        conn.execute("DELETE FROM probe WHERE id LIKE 'integer%'", [])?;
        Ok(())
    });
    exec(
        &store,
        "INSERT INTO probe (id, created_at) VALUES (?, ?)",
        &[
            Value::Text("newer".to_string()),
            time::value(&at(2024, 6, 1, 0, 0, 0)),
        ],
    );
    assert_eq!(
        first_desc(&store),
        "newer",
        "TEXT rows order chronologically"
    );
    assert_eq!(
        first_asc(&store),
        "helper",
        "and ascending gives the oldest"
    );
    // Assert 6: the whole TEXT set satisfies the bound.
    let count = on(&store, |conn| {
        conn.query_row(
            "SELECT count(*) FROM probe WHERE created_at > ?",
            rusqlite::params![time::value(&at(2020, 1, 1, 0, 0, 0))],
            |r| r.get::<_, i64>(0),
        )
        .expect("count reads")
    });
    assert_eq!(count, 2, "both TEXT rows satisfy the bound");
    cleanup(store, path);
}

/// `SELECT id FROM probe WHERE created_at > ? AND id = ?`, the bound bound as
/// TEXT exactly as squirrel's `Gt` does.
fn probe_gt(store: &SQLStore, want_id: &str) -> Option<String> {
    on(store, |conn| {
        conn.query_row(
            "SELECT id FROM probe WHERE created_at > ? AND id = ?",
            rusqlite::params![time::value(&at(2020, 1, 1, 0, 0, 0)), want_id],
            |r| r.get::<_, String>(0),
        )
        .ok()
    })
}

fn first_desc(store: &SQLStore) -> String {
    order_first(store, "DESC")
}

fn first_asc(store: &SQLStore) -> String {
    order_first(store, "ASC")
}

fn order_first(store: &SQLStore, direction: &str) -> String {
    on(store, |conn| {
        conn.query_row(
            &format!("SELECT id FROM probe ORDER BY created_at {direction} LIMIT 1"),
            [],
            |r| r.get::<_, String>(0),
        )
        .expect("order reads")
    })
}

/// The convention's own text, asserted here too so a format change in
/// `crate::time` fails this suite as well.
#[test]
fn the_helper_shape_is_the_documented_one() {
    assert_eq!(time::to_text(&at(2024, 1, 1, 0, 0, 0)), T0);
    // to_rfc3339 is the OTHER shape: same instant, different bytes. Mixing the
    // two in one database is what the helper exists to prevent.
    assert_eq!(
        at(2024, 1, 1, 0, 0, 0).to_rfc3339(),
        "2024-01-01T00:00:00+00:00"
    );
    assert_ne!(
        time::to_text(&at(2024, 1, 1, 0, 0, 0)),
        T0.replace(' ', "T")
    );
    // All three readable shapes parse back to the same instant.
    assert_eq!(time::parse(T0), Some(at(2024, 1, 1, 0, 0, 0)));
    assert_eq!(
        time::parse("2024-01-01T00:00:00Z"),
        Some(at(2024, 1, 1, 0, 0, 0))
    );
    assert_eq!(
        time::parse("2024-01-01 00:00:00"),
        Some(at(2024, 1, 1, 0, 0, 0))
    );
}
