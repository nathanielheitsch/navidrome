//! DB-06 — the EXECUTED tag dictionary/library-link `Add`
//! (`persistence/tag_repository.go:26-50`) through the DB-00 typed transaction
//! seam and DB-05's production library seed.
//!
//! Go truth under test: `tagRepository.Add` chunks the input at 200
//! (`slices.Chunk(tags, 200)`, `:27`), runs the `tag` dictionary insert first
//! (`:28-36`) and the `library_tag` insert second (`:39-47`), both on whatever
//! `ctx`/transaction the caller supplied, with `on conflict (id) do nothing` and
//! `on conflict (tag_id, library_id) do nothing` making the pair idempotent. It
//! derives nothing (`t.ID` is bound verbatim, `:31`), validates nothing, dedupes
//! nothing at this boundary, and checks no affected-row count.
//!
//! Go facts here are SOURCE-VERIFIED at those lines plus `model/tag.go:63-71` and
//! `model/id/id.go:36-48` for the id authority; every behavioral claim below is
//! then EXECUTED against a real SQLite DB built from the fresh DDL
//! (`nd_dbschema::schema::apply_fresh_schema`, one isolated temp file per
//! invocation, `_foreign_keys=on` — Go's own default DSN parameter,
//! `consts/consts.go:14`), written through the production `nd_db::tag_repo` write
//! API and read back through the production `TagRepo`. Library rows come from
//! DB-05's `library_repo::put_in_tx`, never from a hand-built INSERT. No durable
//! Go oracle exists for tag `Add` (`persistence/tag_repository_test.go` is a
//! Ginkgo suite against a Go DB, and `testdata/` holds no tag-Add fixture), so
//! expectations are pinned to Go source lines and labeled EXECUTED-RUST; the only
//! external goldens used are the `decisions.md` L1 id goldens, which are
//! Go-measured for `id.NewTagID`, not for `Add`.
//!
//! Row counts are never used as proof of STATEMENT counts: the statement shape and
//! the 200/201 chunk split are pinned by the builder unit tests
//! (`tag_repo::add_emits_chunked_tag_and_library_tag_inserts`,
//! `add_splits_at_the_200_row_chunk_boundary`), and this file asserts the chunks a
//! call actually renders plus the rolled-back/committed rows that a second-chunk
//! failure implies.

use std::convert::Infallible;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use nd_db::library_repo::{
    put_in_tx, LibraryColumn, LibraryRecord, LibraryWriteError, LibraryWriteResult,
};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sql_restful::RestQueryOptions;
use nd_db::store::SQLStore;
use nd_db::tag_repo::{add_in_tx, add_sql, Tag, TagRepo, CHUNK_SIZE};
use nd_db::{time, DbRuntimeError, TransactionStage};
use nd_dbschema::schema::apply_fresh_schema;
use rusqlite::ffi;
use rusqlite::types::Value as SqlValue;
use rusqlite::Error as SqliteError;

/// Deterministic caller clock, for the library seed's stamps.
fn at(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32, nanos: u32) -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_nano_opt(h, min, s, nanos)
            .unwrap(),
    )
}

fn dsn(path: &std::path::Path) -> String {
    format!("file:{}?mode=rwc&_foreign_keys=on", path.display())
}

/// One isolated on-disk DB per invocation, pooled (the DB-05 fixture shape).
/// `schema = false` leaves the DB UNMIGRATED, which is what makes the "empty input
/// issues no statement" case non-vacuous.
fn fresh_store(label: &str, schema: bool) -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        &format!("nd-db-tag-writes-{label}-"),
        ".db",
    ));
    if path.exists() {
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("cannot clear {path:?}: {e}"));
    }
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn(&path), 2)).unwrap();
    if schema {
        apply_fresh_schema(&first).unwrap();
    }
    pool.put_back(first).expect("return configured handle");
    (SQLStore::new(pool), path)
}

/// Run one closure on a leased connection; store failures panic, the repository's
/// own error value comes back untouched for assertion.
fn lease<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("lease failed: {e}"))
}

/// A raw fixture statement (users, `user_library` links, nonzero `library_tag`
/// counts, and the autocommit probe's cleanup). Library rows are ALWAYS created
/// through `put_in_tx`, and dictionary/link rows through `add_in_tx` — every test
/// calls it. The single exception is the autocommit leak probe in
/// `tag_missing_library_rolls_back_the_dictionary_leg_and_the_links`, which
/// deliberately executes `add_sql`'s rendered chunks itself to reproduce what
/// Go's `Add` does with no transaction, and cleans up with a DELETE so the
/// rollback assertions below it start from the same one-row state.
fn fixture_insert(store: &SQLStore, sql: &str, args: &[SqlValue]) {
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

/// One scalar probe on a FRESH lease (counts after a committed or rolled-back tx),
/// with optional bound args.
fn scalar<T>(store: &SQLStore, sql: &str) -> T
where
    T: rusqlite::types::FromSql,
{
    scalar_with(store, sql, &[])
}

fn scalar_with<T>(store: &SQLStore, sql: &str, args: &[SqlValue]) -> T
where
    T: rusqlite::types::FromSql,
{
    lease(store, |conn| {
        conn.query_row(sql, rusqlite::params_from_iter(args.iter()), |row| {
            row.get(0)
        })
        .unwrap_or_else(|e| panic!("{sql} --{args:?}--: {e}"))
    })
}

/// `user` row (`schema_ddl.rs:57`): `created_at`/`updated_at` are `not null`.
fn seed_user(store: &SQLStore, id: &str, is_admin: bool) {
    fixture_insert(
        store,
        "INSERT INTO \"user\" (id, user_name, name, email, password, is_admin, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            text(id),
            text(id),
            text(id),
            text(&format!("{id}@x.y")),
            text("CIPHER"),
            integer(i64::from(is_admin)),
            text(&time::to_text(&at(2024, 1, 1, 0, 0, 0, 0))),
            text(&time::to_text(&at(2024, 1, 2, 0, 0, 0, 0))),
        ],
    );
}

/// `user_library` membership for a NON-admin — deliberately a fixture write, so it
/// cannot be a side effect of `put_in_tx`'s admin-assignment leg.
fn seed_access(store: &SQLStore, user_id: &str, library_id: i64) {
    fixture_insert(
        store,
        "INSERT INTO user_library (user_id, library_id) VALUES (?, ?)",
        &[text(user_id), integer(library_id)],
    );
}

/// A record whose every field carries a DIFFERENT value (DB-05's helper), so the
/// seeded library's columns cannot alias each other.
fn record(id: i64, tag: &str) -> LibraryRecord {
    LibraryRecord {
        id,
        name: format!("name-{tag}"),
        path: format!("/path-{tag}"),
        remote_path: format!("remote-{tag}"),
        last_scan_at: at(2024, 3, 4, 5, 6, 7, 111_000_000),
        last_scan_started_at: at(2024, 3, 4, 5, 6, 8, 222_000_000),
        full_scan_in_progress: true,
        updated_at: at(2024, 3, 4, 5, 6, 9, 333_000_000),
        created_at: at(2024, 3, 4, 5, 6, 10, 444_000_000),
        total_songs: 11,
        total_albums: 12,
        total_artists: 13,
        total_folders: 14,
        total_files: 15,
        total_missing_files: 16,
        total_size: 17,
        total_duration: 18.5,
        default_new_users: true,
        pid_album: format!("pidalbum-{tag}"),
        pid_track: format!("pidtrack-{tag}"),
        scanned_pid_album: format!("scannedalbum-{tag}"),
        scanned_pid_track: format!("scannedtrack-{tag}"),
    }
}

/// Commit one library through DB-05's production `Put`; returns its id. The
/// autoincrement arm is what yields 1, 2, … in insert order.
fn put_library(store: &SQLStore, tag: &str) -> i64 {
    let now = at(2024, 7, 1, 12, 0, 0, 0);
    store
        .try_with_tx_sync::<LibraryWriteResult, LibraryWriteError>(|tx| {
            put_in_tx(tx, &record(0, tag), &now, &[LibraryColumn::Path])
        })
        .unwrap_or_else(|e| panic!("put_in_tx({tag}) failed: {e}"))
        .id
}

/// Commit one `add_in_tx` call through the typed seam exactly as a caller would —
/// this is the ONLY way dictionary and link rows are written in this file.
fn add_committed(store: &SQLStore, library_id: i64, tags: &[Tag]) {
    store
        .try_with_tx_sync::<(), Infallible>(|tx| add_in_tx(tx, library_id, tags))
        .unwrap_or_else(|e| {
            panic!(
                "add_in_tx(library {library_id}, {} tags) failed: {e}",
                tags.len()
            )
        });
}

/// The `tag` dictionary rows, ordered by value — the raw view of the first leg.
fn dictionary(store: &SQLStore) -> Vec<(String, String, String)> {
    lease(store, |conn| {
        let mut stmt = conn
            .prepare("SELECT id, tag_name, tag_value FROM tag ORDER BY tag_name, tag_value, id")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    })
}

/// The `library_tag` rows as `tag_id/library/albums/songs`, sorted.
fn links(store: &SQLStore) -> Vec<String> {
    lease(store, |conn| {
        let mut stmt = conn
            .prepare(
                "SELECT tag_id, library_id, album_count, media_file_count \
                 FROM library_tag ORDER BY tag_id, library_id",
            )
            .unwrap();
        let rows: Vec<(String, i64, i64, i64)> = stmt
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows.iter()
            .map(|(t, l, a, m)| format!("{t}/{l}/{a}/{m}"))
            .collect()
    })
}

/// `TagRepo::get_all` cells as `value/albums/songs`, sorted.
fn tag_cells(tags: &[Tag]) -> Vec<String> {
    let mut cells: Vec<String> = tags
        .iter()
        .map(|t| format!("{}/{}/{}", t.tag_value, t.album_count, t.song_count))
        .collect();
    cells.sort();
    cells
}

/// A dictionary tag with a real `nd_id::new_tag_id` id — the same authority Go's
/// `model.NewTag` (`model/tag.go:63-71`) uses, i.e. the caller's job, not the
/// repository's. The two count fields are nonzero on purpose: the write must
/// ignore them.
fn tag(name: &str, value: &str) -> Tag {
    Tag {
        id: nd_id::new_tag_id(name, value),
        tag_name: name.to_string(),
        tag_value: value.to_string(),
        album_count: 7,
        song_count: 8,
    }
}

/// The extended SQLite code — the assertion that the seam kept the concrete cause
/// rather than a stringified wrapper (Go's `Add` likewise returns the raw driver
/// error on its first leg, `tag_repository.go:33-36`).
fn extended_code(err: &rusqlite::Error) -> Option<i32> {
    match err {
        SqliteError::SqliteFailure(code, _) => Some(code.extended_code),
        _ => None,
    }
}

/// Assert a Body-stage SQLite failure carrying the given extended code.
fn assert_body_sqlite(err: &DbRuntimeError<Infallible>, want: Option<i32>) {
    match err {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(
                *stage,
                TransactionStage::Body,
                "the executor owns the stage"
            );
            assert_eq!(extended_code(source), want, "raw SQLite cause: {source}");
            assert!(
                !nd_db::store::is_busy(source),
                "a constraint failure is not a busy failure"
            );
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }
}

/// 0 / 1 / 200 / 201 tags, plus input duplication. Executed: chunk counts,
/// dictionary rows, link rows, and the `0,0` counts `Add` writes.
#[test]
fn tag_add_zero_one_200_201_and_duplicates() {
    let (store, path) = fresh_store("chunks", true);
    let lib = put_library(&store, "main");

    // 0 tags: no chunk renders, so the call issues no statement at all. Asserted on
    // an UNMIGRATED DB below; here it is asserted to change nothing.
    add_committed(&store, lib, &[]);
    assert_eq!(scalar::<i64>(&store, "SELECT count(*) FROM tag"), 0);
    assert_eq!(scalar::<i64>(&store, "SELECT count(*) FROM library_tag"), 0);
    assert_eq!(add_sql(lib, &[]).len(), 0, "empty input renders no chunk");

    // 1 tag: one dictionary row, one link row with zeroed counts.
    add_committed(&store, lib, &[tag("genre", "Rock")]);
    assert_eq!(
        dictionary(&store),
        vec![(
            nd_id::new_tag_id("genre", "Rock"),
            "genre".into(),
            "Rock".into()
        )]
    );
    assert_eq!(
        links(&store),
        vec![format!("{}/1/0/0", nd_id::new_tag_id("genre", "Rock"))],
        "album_count/media_file_count are literal 0,0 — the DTO's 7/8 are ignored"
    );
    assert_eq!(add_sql(lib, &[tag("genre", "Rock")]).len(), 1);

    // 200 tags: one chunk, and every row landed.
    let two_hundred: Vec<Tag> = (0..CHUNK_SIZE)
        .map(|i| tag("genre", &format!("V{i}")))
        .collect();
    add_committed(&store, lib, &two_hundred);
    assert_eq!(add_sql(lib, &two_hundred).len(), 1, "200 tags = one chunk");
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM tag WHERE tag_name = 'genre'"),
        201,
        "1 Rock + 200 generated"
    );
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM library_tag"),
        201,
        "one link row per dictionary row"
    );

    // 201 tags: the chunk split executes as 200 + 1, both chunks committed.
    let two_hundred_one: Vec<Tag> = (0..=CHUNK_SIZE)
        .map(|i| tag("mood", &format!("M{i}")))
        .collect();
    assert_eq!(
        add_sql(lib, &two_hundred_one).len(),
        2,
        "201 tags = two chunks"
    );
    add_committed(&store, lib, &two_hundred_one);
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM tag WHERE tag_name = 'mood'"),
        201,
        "chunk 2 landed too"
    );
    assert_eq!(
        scalar::<i64>(
            &store,
            "SELECT count(*) FROM library_tag WHERE tag_id IN \
         (SELECT id FROM tag WHERE tag_name = 'mood')"
        ),
        201
    );

    // Duplicated input is legal AT THE ADD BOUNDARY: Go performs no dedupe here
    // (`:28-36`) — the scanner dedupes by tag ID upstream in `loadTagsFromFiles`
    // (`scanner/phase_1_folders.go:294,305-306,323`), but a direct caller may repeat
    // rows — and `on conflict (id) do
    // nothing` collapses the repeats — EXECUTED-RUST here: two identical VALUES rows
    // in one statement produce one dictionary row and one link row.
    let repeated = vec![tag("decade", "1980s"), tag("decade", "1980s")];
    add_committed(&store, lib, &repeated);
    let decade = scalar::<i64>(
        &store,
        "SELECT count(*) FROM tag WHERE tag_name = 'decade' AND tag_value = '1980s'",
    );
    assert_eq!(decade, 1, "one dictionary row from two identical inputs");
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM library_tag"),
        201 + 201 + 1,
        "one link row per dictionary row, and nothing else moved"
    );

    std::fs::remove_file(&path).ok();
}

/// Empty input issues NO statement, proved against a DB with no `tag` table at all:
/// a zero-chunk call cannot touch a table that does not exist, while the same call
/// with one tag fails on `no such table`. This is a statement-count proof, not a
/// row-count one.
#[test]
fn tag_add_of_nothing_touches_no_statement_even_without_a_schema() {
    let (store, path) = fresh_store("no-schema", false);

    let ok = store.try_with_tx_sync::<(), Infallible>(|tx| add_in_tx(tx, 1, &[]));
    assert!(ok.is_ok(), "empty input runs no statement: {:?}", ok.err());

    // Same seam, one tag: proves the DB really has no tag table, so the Ok above
    // came from issuing nothing rather than from a successful no-op statement.
    let failed =
        store.try_with_tx_sync::<(), Infallible>(|tx| add_in_tx(tx, 1, &[tag("genre", "Rock")]));
    let err = failed.expect_err("there is no tag table here");
    assert_body_sqlite(&err, Some(ffi::SQLITE_ERROR));
    assert!(
        matches!(err, DbRuntimeError::Sqlite { .. }),
        "the missing table is a SQLite failure, not a domain arm"
    );

    std::fs::remove_file(&path).ok();
}

/// Bound-value alignment: each tag's id/name/value reach their OWN column, and the
/// DTO's count fields are not persisted. Adversarial values (SQL metacharacters,
/// whitespace, Unicode, empty) double as the no-validation pin.
#[test]
fn tag_add_stores_each_field_in_its_own_column_and_ignores_counts() {
    let (store, path) = fresh_store("alignment", true);
    let lib = put_library(&store, "main");

    // Values a rotated argument list cannot survive: every field differs, and one
    // tag has an EMPTY value and one an EMPTY name, both storable (`schema_ddl.rs:55`
    // declares both columns `default '' not null`, and Go validates nothing in Add).
    let tags = vec![
        tag("genre", "'; DROP TABLE tag; --"),
        tag("genre", ""),
        tag("", "EmptyName"),
        tag("genre", "İstanbul Jazz"),
        tag("mood", "  spaced  "),
        tag("genre", "ΟΔΥΣΣ"),
    ];
    add_committed(&store, lib, &tags);

    let stored = dictionary(&store);
    let expected: Vec<(String, String, String)> = tags
        .iter()
        .map(|t| (t.id.clone(), t.tag_name.clone(), t.tag_value.clone()))
        .collect();
    let mut stored = stored;
    let mut expected = expected;
    stored.sort();
    expected.sort();
    assert_eq!(
        stored, expected,
        "id/name/value in their own columns, verbatim"
    );

    // EXECUTED-RUST: the Go-fidelity id authority (decisions.md L1 goldens,
    // Go-measured for `id.NewTagID`). `str::to_lowercase` would mint different ids.
    assert_eq!(
        nd_id::new_tag_id("genre", "ΟΔΥΣΣ"),
        "0UEOhdgRzYCxNU4pOSozfz",
        "final sigma is NOT folded, as in Go"
    );
    assert_eq!(
        nd_id::new_tag_id("genre", "İstanbul Jazz"),
        "3zPshTNGcEwd2fDkQE5r0A",
        "U+0130 maps to a plain i, as in Go"
    );
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM tag WHERE tag_value = ''"),
        1,
        "an empty value is a storable value"
    );
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM tag WHERE tag_name = ''"),
        1,
        "so is an empty name"
    );
    assert_eq!(
        scalar::<i64>(
            &store,
            "SELECT count(*) FROM tag WHERE tag_value = '  spaced  '"
        ),
        1,
        "whitespace preserved, not trimmed"
    );

    // Counts: every link row is 0/0 although every input DTO carried 7/8.
    assert_eq!(
        scalar::<i64>(
            &store,
            "SELECT count(*) FROM library_tag WHERE album_count = 0 AND media_file_count = 0"
        ),
        6,
        "Add never persists the DTO counts"
    );
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM tag"),
        6,
        "the injected SQL text bound as values, so the table still exists"
    );
    assert_eq!(
        scalar::<i64>(
            &store,
            "SELECT count(*) FROM sqlite_master WHERE name = 'tag'"
        ),
        1
    );

    std::fs::remove_file(&path).ok();
}

/// Second library links without touching the first library's counts, a re-`Add` of
/// the same id with DIFFERENT fields cannot rewrite the dictionary row, and a
/// re-`Add` of the first library cannot zero the counts: both inserts are
/// `on conflict … DO NOTHING` (`:28-29`, `:39-40`), and `UpdateCounts` (DB-17) owns
/// those columns.
#[test]
fn tag_second_library_links_without_overwriting_counts() {
    let (store, path) = fresh_store("two-libraries", true);
    let lib1 = put_library(&store, "one");
    let lib2 = put_library(&store, "two");
    let t = tag("genre", "Rock");

    add_committed(&store, lib1, std::slice::from_ref(&t));
    // Nonzero per-library counts are only producible by DB-17's `UpdateCounts`,
    // which is not implemented yet, so the fixture sets them (fixture_insert is the
    // only non-production write in this file).
    fixture_insert(
        &store,
        "UPDATE library_tag SET album_count = 5, media_file_count = 9 \
         WHERE tag_id = ? AND library_id = ?",
        &[text(&t.id), integer(lib1)],
    );

    add_committed(&store, lib2, std::slice::from_ref(&t));
    // Discriminating control: the SAME dictionary id re-Added with a DIFFERENT
    // value that does not exist anywhere in the dictionary, so no UNIQUE can mask
    // it (`on conflict (id) do nothing`, `:28-29`). The stored dictionary tuple
    // must survive verbatim — a hypothetical `do update set … = excluded…`
    // variant would overwrite it here silently — and the only new row is lib 2's
    // link. Supplied counts are nonzero to prove they stay ignored.
    let mismatched = Tag {
        id: t.id.clone(),
        tag_name: "genre".to_string(),
        tag_value: "NotRock".to_string(),
        album_count: 3,
        song_count: 4,
    };
    add_committed(&store, lib2, std::slice::from_ref(&mismatched));

    // Asserted BEFORE any further Add, so a hypothetical `do update set … =
    // excluded…` cannot be masked by a later re-Add of the original payload.
    assert_eq!(
        dictionary(&store),
        vec![(t.id.clone(), "genre".into(), "Rock".into())],
        "the original dictionary payload survives an id collision with a different value"
    );
    assert_eq!(
        scalar_with::<String>(
            &store,
            "SELECT tag_value FROM tag WHERE id = ?",
            &[text(&t.id)]
        ),
        "Rock",
        "the existing id keeps its stored value (DO NOTHING, not DO UPDATE)"
    );
    assert_eq!(
        scalar::<i64>(
            &store,
            "SELECT count(*) FROM tag WHERE tag_value = 'NotRock'",
        ),
        0,
        "the mismatched value was not written under the existing id"
    );

    add_committed(&store, lib1, std::slice::from_ref(&t));

    assert_eq!(
        links(&store),
        vec![format!("{}/1/5/9", t.id), format!("{}/2/0/0", t.id),],
        "lib 1 keeps 5/9 through both a cross-library Add and a re-Add; lib 2 links at 0/0"
    );
    assert_eq!(
        scalar_with::<i64>(
            &store,
            "SELECT count(*) FROM tag WHERE id = ?",
            &[text(&t.id)]
        ),
        1,
        "the dictionary row is not duplicated"
    );

    // Plain repeated Add on its own is idempotent too (retry after a BUSY failure
    // re-runs the same chunk pair).
    add_committed(&store, lib1, &[t.clone(), tag("genre", "Pop")]);
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM tag"),
        2,
        "Rock plus the new Pop"
    );
    assert_eq!(
        links(&store),
        vec![
            format!("{}/1/0/0", nd_id::new_tag_id("genre", "Pop")),
            format!("{}/1/5/9", t.id),
            format!("{}/2/0/0", t.id),
        ],
        "still 5/9, and only Pop is new"
    );

    std::fs::remove_file(&path).ok();
}

/// A missing `library_id` fails the SECOND leg inside the caller's transaction and
/// rolls the dictionary leg back with it. Non-vacuous: the EXECUTED-RUST autocommit
/// probe below runs the SAME rendered chunks with no transaction and shows the
/// dictionary row surviving the FK failure — Go's `Add` has exactly that leak when
/// nothing wraps it — so only the tx-bound API prevents the orphan.
#[test]
fn tag_missing_library_rolls_back_the_dictionary_leg_and_the_links() {
    let (store, path) = fresh_store("missing-library", true);
    let lib = put_library(&store, "main");
    let keep = tag("genre", "Keep");
    add_committed(&store, lib, std::slice::from_ref(&keep));

    let missing: i64 = 9_999;
    let rolled: Vec<Tag> = vec![
        tag("genre", "Roll"),
        tag("mood", "RollToo"),
        tag("decade", "2020s"),
    ];

    // Autocommit probe on the same rendered chunks: dictionary leg commits, the link
    // leg fails, and the dictionary rows LEAK. Test-local fixture execution of
    // `add_sql`'s output, which is precisely what Go's Add does untransacted.
    let leaked = lease(&store, |conn| {
        let mut chunks = add_sql(missing, &rolled).into_iter();
        let first = chunks.next().expect("one chunk");
        nd_db::execution::execute(conn, &first.tag_insert).expect("dictionary leg commits");
        let err = nd_db::execution::execute(conn, &first.library_tag_insert)
            .expect_err("link leg hits the FK");
        let dictionary_rows: i64 = conn
            .query_row("SELECT count(*) FROM tag", [], |row| row.get(0))
            .unwrap();
        (extended_code(&err), dictionary_rows)
    });
    assert_eq!(leaked.0, Some(ffi::SQLITE_CONSTRAINT_FOREIGNKEY));
    assert_eq!(
        leaked.1, 4,
        "the autocommit dictionary rows leaked — the tx is what prevents this"
    );
    fixture_insert(
        &store,
        "DELETE FROM tag WHERE tag_name <> 'genre' OR tag_value <> 'Keep'",
        &[],
    );
    assert_eq!(scalar::<i64>(&store, "SELECT count(*) FROM tag"), 1);

    let failed = store.try_with_tx_sync::<(), Infallible>(|tx| add_in_tx(tx, missing, &rolled));
    let err = failed.expect_err("library_tag FK rejects the missing library");
    assert_body_sqlite(&err, Some(ffi::SQLITE_CONSTRAINT_FOREIGNKEY));

    // Fresh lease: the three dictionary rows written BEFORE the failure are gone.
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM tag"),
        1,
        "only the pre-transaction dictionary row survives"
    );
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM library_tag"),
        1,
        "no link row for the missing library"
    );
    assert_eq!(
        dictionary(&store),
        vec![(keep.id.clone(), "genre".into(), "Keep".into())],
        "the pre-existing row is untouched"
    );
    lease(&store, |conn| {
        assert!(conn.is_autocommit(), "the lease came back reusable");
    });

    // The very same rows succeed once the library exists, which is what proves the
    // FK — not a typo or a missing table — was the failure.
    let lib2 = put_library(&store, "second");
    add_committed(
        &store,
        lib2,
        &[
            tag("genre", "Roll"),
            tag("mood", "RollToo"),
            tag("decade", "2020s"),
        ],
    );
    assert_eq!(scalar::<i64>(&store, "SELECT count(*) FROM tag"), 4);

    std::fs::remove_file(&path).ok();
}

/// Chunk 2's table-level `tags_name_value` UNIQUE (`schema_ddl.rs:55`) is NOT
/// covered by the `(id)` conflict target (`:28-29`), so it hard-fails the whole
/// `Add` and rolls back chunk 1's 200 dictionary rows AND its 200 link rows while
/// the pre-existing dictionary row survives. This is inherited Go behavior, pinned
/// rather than fixed: widening the conflict target would silently swallow a caller's
/// inconsistent id.
#[test]
fn tag_second_chunk_unique_failure_rolls_back_everything() {
    let (store, path) = fresh_store("second-chunk", true);
    let lib = put_library(&store, "main");
    let rock = tag("genre", "Rock");
    add_committed(&store, lib, std::slice::from_ref(&rock));
    assert_eq!(scalar::<i64>(&store, "SELECT count(*) FROM tag"), 1);

    // The inconsistent id: name/value already stored under the real id, but a
    // different id (a hand-made dictionary row reaches the same state).
    let imposter = Tag {
        id: nd_id::new_tag_id("genre", "Other"),
        tag_name: "genre".to_string(),
        tag_value: "Rock".to_string(),
        album_count: 0,
        song_count: 0,
    };
    let mut tags: Vec<Tag> = (0..CHUNK_SIZE)
        .map(|i| tag("genre", &format!("V{i}")))
        .collect();
    tags.push(imposter.clone()); // row 201 ⇒ chunk 2
    assert_eq!(add_sql(lib, &tags).len(), 2, "the failure sits in chunk 2");

    let failed = store.try_with_tx_sync::<(), Infallible>(|tx| add_in_tx(tx, lib, &tags));
    let err = failed.expect_err("tags_name_value rejects a second id for the same value");
    assert_body_sqlite(&err, Some(ffi::SQLITE_CONSTRAINT_UNIQUE));

    // Fresh lease: chunk 1's rows are all gone, and the pre-existing row survives.
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM tag"),
        1,
        "chunk 1's 200 dictionary rows rolled back"
    );
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM library_tag"),
        1,
        "chunk 1's 200 link rows rolled back too"
    );
    assert_eq!(
        dictionary(&store),
        vec![(rock.id.clone(), "genre".into(), "Rock".into())],
        "the pre-existing dictionary row is intact"
    );
    assert_eq!(
        scalar::<String>(
            &store,
            "SELECT id FROM tag WHERE tag_name = 'genre' AND tag_value = 'Rock'",
        ),
        rock.id,
        "DO NOTHING left the stored id, and the imposter never landed"
    );
    assert_eq!(
        scalar_with::<i64>(
            &store,
            "SELECT count(*) FROM tag WHERE id = ?",
            &[text(&imposter.id)],
        ),
        0,
        "the inconsistent id was rolled back with everything else"
    );

    std::fs::remove_file(&path).ok();
}

/// Scoped read-back through the production `TagRepo`: the INNER `user_library` join
/// (`sql_tags.go:171-174`) exposes only the user's own libraries — including the
/// counts — while the headless read (`None` = Go's `invalidUserId`) sees both, and a
/// non-target decoy appears in neither scoped result.
#[test]
fn tag_add_read_back_matches_library_access() {
    let (store, path) = fresh_store("scoped-read", true);
    let lib1 = put_library(&store, "one");
    let lib2 = put_library(&store, "two");
    let decoy = tag("genre", "Decoy");
    add_committed(&store, lib2, &[tag("genre", "Lib2"), decoy.clone()]);

    // lib 1 also gets a tag that lib 2 does not have.
    add_committed(&store, lib1, &[tag("genre", "Lib1")]);

    // A NON-admin with access to library 1 only.
    seed_user(&store, "u1", false);
    seed_access(&store, "u1", lib1);

    let scoped = lease(&store, |conn| {
        TagRepo::new(conn).get_all_plain("genre", Some("u1"))
    })
    .expect("scoped read");
    assert_eq!(
        tag_cells(&scoped),
        vec!["Lib1/0/0"],
        "lib 2's rows, including the decoy, are invisible to a lib-1-only user"
    );
    assert_eq!(
        lease(&store, |conn| TagRepo::new(conn)
            .count_all(Some("u1"), None))
        .expect("scoped count"),
        1
    );

    // Headless: no `user_library` join at all (`sql_tags.go:170-175`), so every
    // dictionary row the LEFT `library_tag` join reaches.
    let headless = lease(&store, |conn| {
        TagRepo::new(conn).get_all_plain("genre", None)
    })
    .expect("headless read");
    assert_eq!(
        tag_cells(&headless),
        vec!["Decoy/0/0", "Lib1/0/0", "Lib2/0/0"]
    );
    assert_eq!(
        lease(&store, |conn| TagRepo::new(conn).count_all(None, None)).expect("headless count"),
        3
    );

    // Counts through the same scope: the fixture-updated lib-1 counts sum for a user
    // who can see lib 1, and the decoy's lib-2 counts stay out of it.
    fixture_insert(
        &store,
        "UPDATE library_tag SET album_count = 5, media_file_count = 9 \
         WHERE tag_id = ? AND library_id = ?",
        &[text(&decoy.id), integer(lib2)],
    );
    let scoped = lease(&store, |conn| {
        TagRepo::new(conn).get_all("genre", Some("u1"), None)
    })
    .expect("REST-shaped scoped read");
    assert_eq!(tag_cells(&scoped), vec!["Lib1/0/0"]);
    let headless = lease(&store, |conn| {
        TagRepo::new(conn).get_all(
            "genre",
            None,
            Some(&RestQueryOptions {
                sort: "name".to_string(),
                order: "desc".to_string(),
                ..RestQueryOptions::default()
            }),
        )
    })
    .expect("headless REST read");
    assert_eq!(
        tag_cells(&headless),
        vec!["Decoy/5/9", "Lib1/0/0", "Lib2/0/0"]
    );

    std::fs::remove_file(&path).ok();
}

/// A caller-supplied id that disagrees with its name/value is stored VERBATIM:
/// `Add` binds `t.ID` (`:31`) and neither recomputes, validates, nor rejects it, so
/// a mismatch is only a problem where an existing UNIQUE says it is.
#[test]
fn tag_add_stores_a_supplied_inconsistent_id_verbatim() {
    let (store, path) = fresh_store("inconsistent-id", true);
    let lib = put_library(&store, "main");
    let odd = Tag {
        id: "caller-chosen-id".to_string(),
        tag_name: "genre".to_string(),
        tag_value: "Whatever".to_string(),
        album_count: 0,
        song_count: 0,
    };
    add_committed(&store, lib, std::slice::from_ref(&odd));

    assert_eq!(
        dictionary(&store),
        vec![(
            "caller-chosen-id".to_string(),
            "genre".into(),
            "Whatever".into()
        )],
        "no recomputation: the supplied id is the stored id"
    );
    assert_eq!(links(&store), vec!["caller-chosen-id/1/0/0"]);

    // The same (name, value) under a DIFFERENT, deterministic id is the hard error
    // the UNIQUE gives, not a silent overwrite (the previous test's chunk-2 case).
    let second = tag("genre", "Whatever");
    let failed = store
        .try_with_tx_sync::<(), Infallible>(|tx| add_in_tx(tx, lib, std::slice::from_ref(&second)));
    assert_body_sqlite(
        &failed.expect_err("tags_name_value is table-level"),
        Some(ffi::SQLITE_CONSTRAINT_UNIQUE),
    );
    assert_eq!(
        dictionary(&store),
        vec![(
            "caller-chosen-id".to_string(),
            "genre".into(),
            "Whatever".into()
        )],
        "the failed call changed nothing"
    );

    std::fs::remove_file(&path).ok();
}

/// `add_in_tx` has no domain arm to carry: `Infallible` makes
/// `DbOperationError::Domain` structurally uninhabited, exactly like DB-05's
/// `LibraryScanError`, and a successful call carries no value.
#[test]
fn tag_add_has_no_domain_arm() {
    let (store, path) = fresh_store("no-domain-arm", true);
    let lib = put_library(&store, "main");
    let result =
        store.try_with_tx_sync::<(), Infallible>(|tx| add_in_tx(tx, lib, &[tag("genre", "Rock")]));
    assert_eq!(result.expect("the write succeeds"), ());

    // The only failure the seam can report for this API is a SQLite one.
    let failed =
        store.try_with_tx_sync::<(), Infallible>(|tx| add_in_tx(tx, 4_242, &[tag("genre", "X")]));
    match failed.expect_err("FK rejects the missing library") {
        DbRuntimeError::Sqlite { .. } => {}
        other => panic!("expected a SQLite arm, got {other:?}"),
    }

    std::fs::remove_file(&path).ok();
}
