//! DB-11 — EXECUTED typed scanner reads, the cursor/cancellation resource
//! contract, and the API-safe scoped point read ([`nd_db::scanner_queries`]).
//!
//! Go truth under test, SOURCE-VERIFIED at the cited lines of this checkout:
//! `persistence/sql_base_repository.go:261-283` (`applyLibraryFilter`), `:288-299`
//! (`userSeesAllLibraries`), `:302-313` (`visibleLibraryIDs`), `:411-439`
//! (`queryWithStableResults` — one prepared statement, `defer rows.Close()`, scan
//! error yielded beside the row), `persistence/artwork_hydration.go:15-17,19-34`
//! (`artworkChunkSize` = 600, `streamByIDs`), `persistence/mediafile_repository.go:309-316`
//! (`GetCursor`, NOT artwork-hydrated — pinned by `artwork_hydration_test.go:633`),
//! `:485-510` (`GetMissingAndMatching`: `ORDER BY pid`, library-scoped ONLY through
//! the subquery — the outer select carries no library predicate), `:512-563`
//! (`FindRecentFilesBy*`, `created_at > since` strict), `:365-404` (`FindByPaths`:
//! per-library IN-lists, `collate nocase`, `applyLibraryFilter` last), `:293-316`
//! (`GetAlbumIDsByFolder`, chunk 200, per-chunk ids appended),
//! `persistence/album_repository.go:392-403` (`GetTouchedAlbums`) and `:285-304`
//! (`GetSoleAlbumArtistIDsInSubtrees`, path chunk 100), plus the scanner consumers
//! `scanner/phase_1_folders.go:231-244`, `phase_2_missing_tracks.go:74-100,231-266`,
//! `phase_3_refresh_albums.go:49-59,79`.
//!
//! Every claim below is EXECUTED: statements come from the production builders, run
//! on a real SQLite DB built from the fresh DDL (`apply_fresh_schema`,
//! `_foreign_keys=on`, one temp file per invocation), and rows come back through the
//! production hydration seam. No Go oracle exists for these methods in this worktree
//! (the `persistence/*_test.go` suites are Ginkgo against a Go DB), so expectations
//! are pinned to Go source lines and labeled EXECUTED-RUST.
//!
//! Non-vacuity controls, each executed rather than argued:
//! * [`unfiltered_get_all_cannot_express_the_two_folder_target_set`] — the
//!   pre-existing unfiltered read really returns BOTH folders, so every narrowing
//!   assertion is the typed clause's doing;
//! * [`visitor_stops_before_bad_later_row_and_releases_lease`] — the row after the
//!   stop point is deliberately malformed, so a collect-then-truncate implementation
//!   would report a decode error instead of `Stopped`;
//! * [`empty_scope_denies_not_headless`] and
//!   [`visible_point_read_denies_other_library_for_all_aliases`] — the unscoped
//!   negative controls really do expose the unauthorized row, so denial is the
//!   clause's doing and not a missing row.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use nd_db::album_repo;
use nd_db::artist_repo;
use nd_db::folder_repo;
use nd_db::hydrate::MEDIAFILE_PREFIX;
use nd_db::mediafile_repo::{self, MediaFileRepo};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::scanner_queries::{
    self, album_ids_by_folders, artist_scope_clause, get_visible_media, media_by_paths,
    membership_clause, resolve_scope, resolve_user, sole_album_artist_ids_in_subtrees, split_paths,
    visit_media_in_album, visit_media_in_folder, visit_missing_and_matching,
    visit_recent_candidates, visit_touched_albums, Control, CursorError, MediaPhaseFilter,
    RecentMatcher, ScannerScope, ScopeAlias, ScopeDecision, UserScope, VisibleMediaError,
    VisitOutcome, ALBUM_IDS_FOLDER_CHUNK_SIZE, PATH_CHUNK_SIZE, SUBTREE_PATH_CHUNK_SIZE,
};
use nd_db::sqlrepo::Value;
use nd_db::store::SQLStore;
use nd_db::time;
use nd_dbschema::schema::apply_fresh_schema;
use rusqlite::types::Value as SqlValue;

/// Scan-clock constants: library `last_scan_at`/`last_scan_started_at` and the
/// rows' `created_at`/`imported_at` are all expressed against them.
const T0: &str = "2020-01-01 00:00:00+00:00";
const SCAN: &str = "2022-06-01 12:00:00+00:00";
/// Strictly after [`SCAN`]: included by `>` comparisons.
const AFTER: &str = "2023-01-01 00:00:00+00:00";

fn stamp(text: &str) -> DateTime<Utc> {
    time::parse(text).unwrap_or_else(|| panic!("{text} parses"))
}

fn dsn(path: &std::path::Path) -> String {
    format!("file:{}?mode=rwc&_foreign_keys=on", path.display())
}

/// One isolated on-disk DB per invocation with the production-shaped pool.
/// `schema = false` leaves it UNMIGRATED, which is what makes "this call issued no
/// statement at all" non-vacuous: any SQL at all then fails with `no such table`.
fn fresh_store(label: &str, schema: bool) -> (SQLStore, std::path::PathBuf) {
    fresh_store_with_size(label, schema, 2)
}

fn fresh_store_with_size(label: &str, schema: bool, size: usize) -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        &format!("nd-db-scanner-queries-{label}-"),
        ".db",
    ));
    if path.exists() {
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("cannot clear {path:?}: {e}"));
    }
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn(&path), size)).unwrap();
    if schema {
        apply_fresh_schema(&first).unwrap();
    }
    pool.put_back(first).expect("return configured handle");
    (SQLStore::new(pool), path)
}

/// The system authority every scanner-only read takes: a library id and nothing
/// else (F1). A scanner read cannot be widened by a user id or clause text.
fn system_scope(library_id: i64) -> ScannerScope {
    ScannerScope::for_library(library_id)
}

/// Resolve a user's read authority on a leased connection. `resolve` is a
/// synchronous read, so it runs inside the same lease the read uses.
fn user_scope(conn: &rusqlite::Connection, user_id: &str) -> UserScope {
    UserScope::for_user(user_id.to_string())
        .resolve(conn, ScopeAlias::MediaFile)
        .unwrap_or_else(|e| panic!("resolve_user({user_id}): {e}"))
}

fn lease<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("lease failed: {e}"))
}

/// A raw fixture statement (seed rows only — no read under test runs through here).
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

/// `Completed`'s count, panicking on any other outcome — the assertion helper the
/// size/boundary tests use.
fn completed(outcome: VisitOutcome) -> usize {
    match outcome {
        VisitOutcome::Completed { visited } => visited,
        VisitOutcome::Stopped { visited } => {
            panic!("expected normal completion, got Stopped({visited})")
        }
    }
}

fn seed_library(store: &SQLStore, id: i64, name: &str, path: &str, scan_at: &str) {
    fixture(
        store,
        "INSERT INTO library (id, name, path, remote_path, last_scan_at, last_scan_started_at, \
         created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            integer(id),
            text(name),
            text(path),
            text(""),
            text(scan_at),
            text(scan_at),
            text(T0),
            text(T0),
        ],
    );
}

fn seed_user(store: &SQLStore, id: &str, is_admin: bool) {
    fixture(
        store,
        "INSERT INTO \"user\" (id, user_name, name, email, password, is_admin, created_at, \
         updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            text(id),
            text(id),
            text(id),
            text(&format!("{id}@x.y")),
            text("CIPHER"),
            integer(i64::from(is_admin)),
            text(T0),
            text(T0),
        ],
    );
}

fn grant(store: &SQLStore, user_id: &str, library_id: i64) {
    fixture(
        store,
        "INSERT INTO user_library (user_id, library_id) VALUES (?, ?)",
        &[text(user_id), integer(library_id)],
    );
}

fn revoke(store: &SQLStore, user_id: &str, library_id: i64) {
    fixture(
        store,
        "DELETE FROM user_library WHERE user_id = ? AND library_id = ?",
        &[text(user_id), integer(library_id)],
    );
}

fn seed_folder(store: &SQLStore, id: &str, library_id: i64, path: &str, parent: &str) {
    fixture(
        store,
        "INSERT INTO folder (id, library_id, path, name, parent_id) VALUES (?, ?, ?, ?, ?)",
        &[
            text(id),
            integer(library_id),
            text(path),
            text("leaf"),
            text(parent),
        ],
    );
}

/// Minimal core columns `dbMediaFile` needs to scan at all (Go's non-pointer fields
/// reject NULL; the DDL defaults cover the rest, including the `participants`/`tags`
/// shims whose decoded form Go's `PostScan` requires).
fn seed_media(
    store: &SQLStore,
    id: &str,
    library_id: i64,
    folder_id: &str,
    path: &str,
    cells: &[(&str, &str)],
) {
    // A cell that names a base column OVERRIDES its value: SQLite takes the FIRST
    // occurrence of a repeated column name in an INSERT, so appending a duplicate
    // would silently keep the default.
    let mut columns = vec![
        "id",
        "library_id",
        "folder_id",
        "album_id",
        "path",
        "title",
        "created_at",
        "updated_at",
        "birth_time",
    ];
    let mut values = vec![
        text(id),
        integer(library_id),
        text(folder_id),
        text("al-1"),
        text(path),
        text("Title"),
        text(T0),
        text(T0),
        text(T0),
    ];
    for (column, value) in cells {
        match columns.iter().position(|existing| *existing == *column) {
            Some(index) => values[index] = text(value),
            None => {
                columns.push(column);
                values.push(text(value));
            }
        }
    }
    let placeholders = vec!["?"; columns.len()].join(", ");
    fixture(
        store,
        &format!(
            "INSERT INTO media_file ({}) VALUES ({})",
            columns.join(", "),
            placeholders
        ),
        &values,
    );
}

/// A row no decoder can accept: an unknown participant role fails Go's UNGUARDED
/// `dbMediaFile.PostScan` participants arm (`mediafile_repository.go:55`,
/// `model.Role.UnmarshalText`), wrapped with `MEDIAFILE_PREFIX`.
fn seed_media_broken(store: &SQLStore, id: &str, library_id: i64, folder_id: &str, path: &str) {
    seed_media(
        store,
        id,
        library_id,
        folder_id,
        path,
        &[("participants", r#"{"bogus":[{"id":"a","name":"n"}]}"#)],
    );
}

fn seed_album(store: &SQLStore, id: &str, library_id: i64, imported_at: &str, folder_ids: &str) {
    fixture(
        store,
        "INSERT INTO album (id, library_id, name, created_at, updated_at, imported_at, folder_ids, \
         participants, tags) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            text(id),
            integer(library_id),
            text("Album"),
            text(T0),
            text(T0),
            text(imported_at),
            text(folder_ids),
            text(r#"{"albumartist":[{"id":"ar-1","name":"Sole","sub_role":""}]}"#),
            text("{}"),
        ],
    );
}

/// An album with TWO albumartists — excluded by the sole-artist fragment
/// (`json_array_length(participants, '$.albumartist') = 1`,
/// `album_repository.go:273-275`).
fn seed_album_duo(store: &SQLStore, id: &str, library_id: i64, folder_ids: &str) {
    fixture(
        store,
        "INSERT INTO album (id, library_id, name, created_at, updated_at, imported_at, folder_ids, \
         participants, tags) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            text(id),
            integer(library_id),
            text("Duo"),
            text(T0),
            text(T0),
            text(AFTER),
            text(folder_ids),
            text(
                r#"{"albumartist":[{"id":"ar-1","name":"A","sub_role":""},{"id":"ar-2","name":"B","sub_role":""}]}"#,
            ),
            text("{}"),
        ],
    );
}

/// Two libraries, one folder and one track each (paths matching what
/// `media_by_paths` searches), plus a granted non-admin, an admin and a
/// two-library grantee — the fixture every scope test shares.
fn two_library_fixture(label: &str) -> (SQLStore, std::path::PathBuf) {
    let (store, path) = fresh_store(label, true);
    seed_library(&store, 1, "A", "/a", SCAN);
    seed_library(&store, 2, "B", "/b", SCAN);
    seed_user(&store, "viewer", false);
    seed_user(&store, "boss", true);
    seed_user(&store, "grantee", false);
    grant(&store, "viewer", 1);
    grant(&store, "grantee", 1);
    grant(&store, "grantee", 2);
    seed_folder(&store, "f1", 1, "a/one", "");
    seed_folder(&store, "f2", 2, "b/two", "");
    seed_media(&store, "mf1", 1, "f1", "a/one/mf1.mp3", &[("pid", "p-a")]);
    seed_media(&store, "mf2", 2, "f2", "b/two/mf2.mp3", &[("pid", "p-b")]);
    (store, path)
}

#[test]
fn folder_album_filters_apply_before_page() {
    // Sizes 0/1/100/101 through the production visitors. Phase 1 filters by folder_id
    // (scanner/phase_1_folders.go:231-233), phase 3 by album_id
    // (phase_3_refresh_albums.go:79); both clauses narrow the SELECT, not the page.
    let (store, path) = fresh_store("phase-filters", true);
    seed_library(&store, 1, "A", "/a", SCAN);
    seed_folder(&store, "target", 1, "a/target", "");
    seed_folder(&store, "other", 1, "a/other", "");
    let cancel = nd_utils::context::Cancellation::new();

    for count in [0usize, 1, 100, 101] {
        for i in 0..count {
            seed_media(
                &store,
                &format!("t{i}"),
                1,
                "target",
                &format!("a/target/t{i}.mp3"),
                &[("album_id", "al-target"), ("pid", "p")],
            );
        }
        for i in 0..5 {
            seed_media(
                &store,
                &format!("o{i}"),
                1,
                "other",
                &format!("a/other/o{i}.mp3"),
                &[("album_id", "al-other"), ("pid", "p")],
            );
        }

        let (outcome, ids) = lease(&store, |conn| {
            let mut seen = Vec::new();
            let outcome = visit_media_in_folder(conn, "target", None, &cancel, |record| {
                seen.push(record.id.clone());
                Control::Continue
            })
            .unwrap_or_else(|e| panic!("visit_media_in_folder: {e}"));
            (outcome, seen)
        });
        assert_eq!(
            outcome,
            VisitOutcome::Completed { visited: count },
            "the folder filter must deliver exactly the target folder's rows (size {count})"
        );
        assert!(
            ids.iter().all(|id| id.starts_with('t')),
            "a foreign-folder row reached the callback: {ids:?}"
        );

        let (album_outcome, folders) = lease(&store, |conn| {
            let mut seen = Vec::new();
            let outcome = visit_media_in_album(conn, "al-target", None, &cancel, |record| {
                seen.push(record.folder_id.clone());
                Control::Continue
            })
            .unwrap_or_else(|e| panic!("visit_media_in_album: {e}"));
            (outcome, seen)
        });
        assert_eq!(
            album_outcome,
            VisitOutcome::Completed { visited: count },
            "the album filter must deliver exactly the target album's rows (size {count})"
        );
        assert!(
            folders.iter().all(|folder_id| folder_id == "target"),
            "a foreign-album row reached the album visitor: {folders:?}"
        );

        // Zero-size and full-completion cases must not be vacuous: the foreign rows
        // exist in the SAME table and are excluded by the clause, not by deletion.
        let unfiltered = completed(lease(&store, |conn| {
            visit_media_in_folder(conn, "other", None, &cancel, |record| {
                let _ = record;
                Control::Continue
            })
            .unwrap_or_else(|e| panic!("other folder: {e}"))
        }));
        assert_eq!(
            unfiltered, 5,
            "the sibling folder's rows are present and counted"
        );

        fixture(&store, "DELETE FROM media_file", &[]);
    }

    // Rendered clause + bind order, pinned beside the executed reads above. The
    // scope is the opaque authority; the clause text is only visible through the
    // doc(hidden) internal seam this test is allowed to use.
    let scope = lease(&store, |conn| user_scope(conn, "viewer"));
    let (sql, args) = scanner_queries::media_cursor_sql(
        MediaPhaseFilter::InFolder { folder_id: "f1" },
        Some(&scope),
        None,
    );
    assert!(
        sql.ends_with(
            "WHERE media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE \
             ul.user_id = ?) AND media_file.folder_id = ?"
        ),
        "{sql}"
    );
    assert_eq!(
        args,
        vec![Value::from("viewer"), Value::from("f1")],
        "the scope's user id binds before the predicate's value (clause render order)"
    );
    assert!(
        !sql.contains("1=1") && !sql.contains("1 = 1"),
        "a user arm must never render an unrestricted predicate: {sql}"
    );

    std::fs::remove_file(path).ok();
}

#[test]
fn unfiltered_get_all_cannot_express_the_two_folder_target_set() {
    // Non-vacuity control (subplan §7): the pre-existing read returns BOTH folders and
    // cannot express the phase-1 target set the typed visitor does.
    let (store, path) = two_library_fixture("unfiltered-control");
    let outcome = lease(&store, |conn| {
        MediaFileRepo::new(conn)
            .get_all(None, None, &["media_file.id"], 0, 0, false)
            .unwrap_or_else(|e| panic!("unfiltered get_all: {e}"))
    });
    let mut ids: Vec<String> = outcome.items.iter().map(|m| m.id.clone()).collect();
    ids.sort();
    assert_eq!(
        ids,
        vec!["mf1".to_string(), "mf2".to_string()],
        "the control must expose BOTH folders through the unfiltered read"
    );

    let cancel = nd_utils::context::Cancellation::new();
    let scoped = lease(&store, |conn| {
        let mut seen = Vec::new();
        visit_media_in_folder(conn, "f1", None, &cancel, |record| {
            seen.push(record.id.clone());
            Control::Continue
        })
        .unwrap_or_else(|e| panic!("visitor: {e}"));
        seen
    });
    assert_eq!(scoped, vec!["mf1".to_string()]);

    std::fs::remove_file(path).ok();
}

#[test]
fn missing_pid_order_and_recent_boundaries() {
    let (store, path) = fresh_store("missing-recent", true);
    seed_library(&store, 1, "A", "/a", SCAN);
    seed_library(&store, 2, "B", "/b", SCAN);
    seed_folder(&store, "f1", 1, "a/one", "");
    seed_folder(&store, "f2", 2, "b/two", "");

    // GetMissingAndMatching (`mediafile_repository.go:485-510`): the subquery takes the
    // library's MISSING pids; the outer select keeps `missing = ?` OR
    // `created_at > library.last_scan_started_at` (strict), ordered by pid.
    seed_media(
        &store,
        "m-old-missing",
        1,
        "f1",
        "a/one/1.mp3",
        &[("pid", "p2"), ("missing", "1"), ("created_at", T0)],
    );
    seed_media(
        &store,
        "m-newer-match",
        1,
        "f1",
        "a/one/2.mp3",
        &[("pid", "p2"), ("missing", "0"), ("created_at", AFTER)],
    );
    // EQUAL boundary: created_at == last_scan_started_at, non-missing ⇒ excluded.
    seed_media(
        &store,
        "m-equal-time",
        1,
        "f1",
        "a/one/3.mp3",
        &[("pid", "p2"), ("missing", "0"), ("created_at", SCAN)],
    );
    seed_media(
        &store,
        "m-p1-missing",
        1,
        "f1",
        "a/one/4.mp3",
        &[("pid", "p1"), ("missing", "1"), ("created_at", T0)],
    );
    // A second library's missing row shares pid p2: the OUTER select carries no
    // library predicate (SOURCE-VERIFIED `:501-507`), so it is included — the library
    // scoping happens only in the subquery. Pinned, not assumed.
    seed_media(
        &store,
        "other-lib",
        2,
        "f2",
        "b/two/1.mp3",
        &[("pid", "p2"), ("missing", "1"), ("created_at", T0)],
    );

    let cancel = nd_utils::context::Cancellation::new();
    let (outcome, rows) = lease(&store, |conn| {
        let mut seen = Vec::new();
        let outcome = visit_missing_and_matching(conn, &system_scope(1), &cancel, |record| {
            seen.push((record.pid.clone(), record.id.clone(), record.missing));
            Control::Continue
        })
        .unwrap_or_else(|e| panic!("visit_missing_and_matching: {e}"));
        (outcome, seen)
    });
    assert_eq!(outcome, VisitOutcome::Completed { visited: 4 });
    let pids: Vec<String> = rows.iter().map(|(pid, _, _)| pid.clone()).collect();
    assert_eq!(
        pids.first(),
        Some(&"p1".to_string()),
        "ORDER BY pid (`:508`) puts the lowest pid first: {pids:?}"
    );
    let mut sorted_pids = pids.clone();
    sorted_pids.sort();
    assert_eq!(
        sorted_pids,
        vec![
            "p1".to_string(),
            "p2".to_string(),
            "p2".to_string(),
            "p2".to_string()
        ],
        "the equal-time row is excluded and the three surviving rows keep pid order: {pids:?}"
    );
    let mut ids: Vec<String> = rows.iter().map(|(_, id, _)| id.clone()).collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "m-newer-match".to_string(),
            "m-old-missing".to_string(),
            "m-p1-missing".to_string(),
            "other-lib".to_string()
        ],
        "the strict `>` boundary excludes m-equal-time; the outer select's lack of a \
         library predicate includes library 2's missing row of a shared pid"
    );
    assert!(
        rows.iter()
            .any(|(pid, id, missing)| pid == "p2" && id == "m-old-missing" && *missing),
        "the missing row itself is part of the result set (Go groups it as `missing`)"
    );

    // FindRecentFilesByMBZTrackID (`:512-532`): other library, same mbz id, non-empty
    // mbz id, same suffix, `created_at > since` strict, non-missing.
    let since = stamp(SCAN);
    seed_media(
        &store,
        "r-newer",
        2,
        "f2",
        "b/two/r1.flac",
        &[
            ("mbz_release_track_id", "mbz-1"),
            ("suffix", "flac"),
            ("created_at", AFTER),
        ],
    );
    seed_media(
        &store,
        "r-equal",
        2,
        "f2",
        "b/two/r2.flac",
        &[
            ("mbz_release_track_id", "mbz-1"),
            ("suffix", "flac"),
            ("created_at", SCAN),
        ],
    );
    seed_media(
        &store,
        "r-same-lib",
        1,
        "f1",
        "a/one/r3.flac",
        &[
            ("mbz_release_track_id", "mbz-1"),
            ("suffix", "flac"),
            ("created_at", AFTER),
        ],
    );
    let mbz = mediafile_repo::RecentFileByMbz {
        library_id: 1,
        mbz_release_track_id: "mbz-1",
        suffix: "flac",
        since: &since,
    };
    let recent = lease(&store, |conn| {
        let mut seen = Vec::new();
        let outcome = visit_recent_candidates(conn, &RecentMatcher::Mbz(mbz), &cancel, |record| {
            seen.push(record.id.clone());
            Control::Continue
        })
        .unwrap_or_else(|e| panic!("visit_recent_candidates(mbz): {e}"));
        (completed(outcome), seen)
    });
    assert_eq!(
        recent,
        (1, vec!["r-newer".to_string()]),
        "equal `created_at` is excluded (strict `>`), and the missing row's own library is \
         excluded by NotEq (`:517`)"
    );

    // FindRecentFilesByProperties (`:534-563`): the mbz-EMPTY guard plus the intrinsic
    // property tuple.
    let by_properties = mediafile_repo::RecentFileByProperties {
        library_id: 1,
        title: "Title",
        size: 100,
        suffix: "mp3",
        disc_number: 0,
        track_number: 0,
        album: "",
        since: &since,
    };
    seed_media(
        &store,
        "p-newer",
        2,
        "f2",
        "b/two/p1.mp3",
        &[("size", "100"), ("suffix", "mp3"), ("created_at", AFTER)],
    );
    seed_media(
        &store,
        "p-has-mbz",
        2,
        "f2",
        "b/two/p2.mp3",
        &[
            ("size", "100"),
            ("suffix", "mp3"),
            ("mbz_release_track_id", "mbz-9"),
            ("created_at", AFTER),
        ],
    );
    let props = lease(&store, |conn| {
        let mut seen = Vec::new();
        let outcome = visit_recent_candidates(
            conn,
            &RecentMatcher::Properties(by_properties),
            &cancel,
            |record| {
                seen.push(record.id.clone());
                Control::Continue
            },
        )
        .unwrap_or_else(|e| panic!("visit_recent_candidates(properties): {e}"));
        (completed(outcome), seen)
    });
    assert_eq!(
        props,
        (1, vec!["p-newer".to_string()]),
        "the properties arm excludes candidates that already carry an MBZ track id (`:561`)"
    );

    std::fs::remove_file(path).ok();
}

#[test]
fn touched_albums_and_image_id_reads_scope_chunks() {
    let (store, path) = fresh_store("touched-chunks", true);
    seed_library(&store, 1, "A", "/a", SCAN);
    seed_library(&store, 2, "B", "/b", SCAN);

    // GetTouchedAlbums (`album_repository.go:392-403`): `album.imported_at >
    // library.last_scan_at`, scoped to `library.id = ?`.
    seed_album(&store, "al-touched", 1, AFTER, r#"["f1"]"#);
    seed_album(&store, "al-equal", 1, SCAN, r#"["f1"]"#);
    seed_album(&store, "al-other-lib", 2, AFTER, r#"["f2"]"#);
    let cancel = nd_utils::context::Cancellation::new();
    let touched = lease(&store, |conn| {
        let mut seen = Vec::new();
        let outcome = visit_touched_albums(conn, &system_scope(1), false, &cancel, |record| {
            seen.push(record.id.clone());
            Control::Continue
        })
        .unwrap_or_else(|e| panic!("visit_touched_albums: {e}"));
        (outcome, seen)
    });
    assert_eq!(
        touched.0,
        VisitOutcome::Completed { visited: 1 },
        "equal `imported_at` is excluded (`:398`) and another library's album is excluded \
         by `library.id = ?`"
    );
    assert_eq!(touched.1, vec!["al-touched".to_string()]);

    // GetAlbumIDsByFolder (`mediafile_repository.go:293-316`): chunk 200/201, own
    // folder AND direct child, missing folders and other libraries excluded.
    seed_folder(&store, "f1", 1, "a/one", "");
    seed_folder(&store, "f1-child", 1, "a/one/disc1", "f1");
    seed_folder(&store, "f2", 2, "b/two", "");
    seed_folder(&store, "f-missing", 1, "a/gone", "");
    fixture(
        &store,
        "UPDATE folder SET missing = 1 WHERE id = 'f-missing'",
        &[],
    );
    seed_media(
        &store,
        "mf-a",
        1,
        "f1",
        "a/one/a.mp3",
        &[("album_id", "al-1")],
    );
    seed_media(
        &store,
        "mf-child",
        1,
        "f1-child",
        "a/one/disc1/b.mp3",
        &[("album_id", "al-2")],
    );
    seed_media(
        &store,
        "mf-other-lib",
        2,
        "f2",
        "b/two/c.mp3",
        &[("album_id", "al-9")],
    );
    seed_media(
        &store,
        "mf-missing-folder",
        1,
        "f-missing",
        "a/gone/d.mp3",
        &[("album_id", "al-3"), ("missing", "1")],
    );

    let chunk_counts = lease(&store, |conn| {
        let one = album_ids_by_folders(
            conn,
            &system_scope(1),
            &(0..ALBUM_IDS_FOLDER_CHUNK_SIZE)
                .map(|i| format!("f{i}"))
                .collect::<Vec<String>>(),
        );
        let split = album_ids_by_folders(
            conn,
            &system_scope(1),
            &(0..(ALBUM_IDS_FOLDER_CHUNK_SIZE + 1))
                .map(|i| format!("f{i}"))
                .collect::<Vec<String>>(),
        );
        (one.remaining_chunks(), split.remaining_chunks())
    });
    assert_eq!(
        chunk_counts,
        (1, 2),
        "chunk boundary at {ALBUM_IDS_FOLDER_CHUNK_SIZE} (`slices.Chunk(folderIDs, 200)`, \
         `:297`)"
    );

    let drained = lease(&store, |conn| {
        // 199 filler ids + f1 (chunk 1) + f-missing (chunk 2): proves both chunks
        // execute and that the missing folder contributes nothing.
        let mut folder_ids: Vec<String> = (0..199).map(|i| format!("filler{i}")).collect();
        folder_ids.push("f1".to_string());
        folder_ids.push("f-missing".to_string());
        let mut cursor = album_ids_by_folders(conn, &system_scope(1), &folder_ids);
        let mut collected = Vec::new();
        let mut chunks = 0usize;
        while !cursor.is_complete() {
            let outcome = cursor
                .visit_chunk(&cancel, |id| {
                    collected.push(id);
                    Control::Continue
                })
                .unwrap_or_else(|e| panic!("visit_chunk: {e}"));
            match outcome {
                VisitOutcome::Completed { visited } => {
                    assert!(
                        visited <= ALBUM_IDS_FOLDER_CHUNK_SIZE,
                        "chunk delivered {visited}"
                    );
                    chunks += 1;
                }
                VisitOutcome::Stopped { visited } => {
                    panic!("cancelled mid-read after {visited} ids")
                }
            }
        }
        // `SELECT distinct` is per STATEMENT: two identical ids inside ONE chunk
        // produce one result set. The same id placed in TWO chunks is appended twice
        // (Go appends each chunk's ids, `:315`), so cross-chunk duplicates survive.
        let mut filler: Vec<String> = (0..199).map(|i| format!("filler{i}")).collect();
        filler.push("f1".to_string());
        let mut dup = album_ids_by_folders(
            conn,
            &system_scope(1),
            &["f1".to_string(), "f1".to_string()],
        );
        let mut same_chunk = Vec::new();
        dup.visit_chunk(&cancel, |id| {
            same_chunk.push(id);
            Control::Continue
        })
        .unwrap_or_else(|e| panic!("distinct probe: {e}"));
        let mut single = album_ids_by_folders(conn, &system_scope(1), &[filler[0].clone()]);
        let mut none_chunk = Vec::new();
        single
            .visit_chunk(&cancel, |id| {
                none_chunk.push(id);
                Control::Continue
            })
            .unwrap_or_else(|e| panic!("empty-chunk probe: {e}"));
        // f1 in chunk 1 (slot 0) and again in chunk 2 (slot 200).
        let mut spaced = vec!["f1".to_string()];
        spaced.extend((0..199).map(|i| format!("filler{i}")));
        spaced.push("f1".to_string());
        let mut cross = album_ids_by_folders(conn, &system_scope(1), &spaced);
        let mut cross_collected = Vec::new();
        while !cross.is_complete() {
            cross
                .visit_chunk(&cancel, |id| {
                    cross_collected.push(id);
                    Control::Continue
                })
                .unwrap_or_else(|e| panic!("cross-chunk probe: {e}"));
        }
        (chunks, collected, same_chunk, none_chunk, cross_collected)
    });
    assert_eq!(drained.0, 2, "201 ids visit two chunks");
    let mut album_ids = drained.1.clone();
    album_ids.sort();
    assert_eq!(
        album_ids,
        vec!["al-1".to_string(), "al-2".to_string()],
        "f1 and its direct child f1-child match (`f.parent_id IN (…)`); al-9's folder is in \
         library 2 and al-3's folder is missing"
    );
    let mut same_chunk = drained.2.clone();
    same_chunk.sort();
    assert_eq!(
        same_chunk,
        vec!["al-1".to_string(), "al-2".to_string()],
        "two identical folder ids in ONE chunk render one statement, so `SELECT distinct` \
         yields each album id once"
    );
    assert_eq!(
        drained.3,
        Vec::<String>::new(),
        "a chunk of ids that match no folder contributes nothing"
    );
    assert_eq!(
        drained.4,
        vec![
            "al-1".to_string(),
            "al-2".to_string(),
            "al-1".to_string(),
            "al-2".to_string()
        ],
        "the same id in TWO chunks is appended once per chunk (`:315`), NOT deduplicated \
         across chunks"
    );

    // Post-completion use is Closed (D-CURSOR(3)), never a silent empty Completed.
    let closed = lease(&store, |conn| {
        let mut first = Vec::new();
        let mut cursor = album_ids_by_folders(conn, &system_scope(1), &["f1".to_string()]);
        cursor
            .visit_chunk(&cancel, |id| {
                first.push(id.clone());
                Control::Continue
            })
            .unwrap_or_else(|e| panic!("first chunk: {e}"));
        let after = cursor.visit_chunk(&cancel, |id| {
            let _ = id;
            Control::Continue
        });
        (after, first)
    });
    let mut first = closed.1.clone();
    first.sort();
    assert_eq!(
        first,
        vec!["al-1".to_string(), "al-2".to_string()],
        "the first chunk delivers f1 and its direct child"
    );
    assert!(
        matches!(closed.0, Err(CursorError::Closed)),
        "D-CURSOR(3): post-completion use is Closed, got {:?}",
        closed.0
    );

    // GetSoleAlbumArtistIDsInSubtrees (`album_repository.go:285-304`): path chunks
    // 100/101, subtree prefix match, and the whole-library arm for a path that
    // cleans to ".".
    let subtree_chunks = lease(&store, |conn| {
        let one = sole_album_artist_ids_in_subtrees(
            conn,
            &system_scope(1),
            &(0..SUBTREE_PATH_CHUNK_SIZE)
                .map(|i| format!("a/p{i}"))
                .collect::<Vec<String>>(),
        );
        let two = sole_album_artist_ids_in_subtrees(
            conn,
            &system_scope(1),
            &(0..(SUBTREE_PATH_CHUNK_SIZE + 1))
                .map(|i| format!("a/p{i}"))
                .collect::<Vec<String>>(),
        );
        (one.remaining_chunks(), two.remaining_chunks())
    });
    assert_eq!(
        subtree_chunks,
        (1, 2),
        "subtree path chunk boundary at {SUBTREE_PATH_CHUNK_SIZE} (`folder_repository.go:204-206`)"
    );

    let (subtree, whole_library) = lease(&store, |conn| {
        let mut cursor =
            sole_album_artist_ids_in_subtrees(conn, &system_scope(1), &["a/one".to_string()]);
        let mut collected = Vec::new();
        while !cursor.is_complete() {
            cursor
                .visit_chunk(&cancel, |id| {
                    collected.push(id);
                    Control::Continue
                })
                .unwrap_or_else(|e| panic!("subtree chunk: {e}"));
        }
        let mut whole =
            sole_album_artist_ids_in_subtrees(conn, &system_scope(1), &[".".to_string()]);
        let mut all = Vec::new();
        while !whole.is_complete() {
            whole
                .visit_chunk(&cancel, |id| {
                    all.push(id.clone());
                    Control::Continue
                })
                .unwrap_or_else(|e| panic!("whole-library chunk: {e}"));
        }
        (collected, all)
    });
    assert_eq!(
        subtree,
        vec!["ar-1".to_string()],
        "the subtree path a/one matched folders f1 (al-touched and al-equal), yielding one \
         distinct sole albumartist"
    );
    assert_eq!(
        whole_library,
        vec!["ar-1".to_string()],
        "a path that cleans to \".\" selects the whole library (`folder_repository.go:214-216`)"
    );

    // A two-albumartist album contributes nothing: give it its own folder and path.
    seed_folder(&store, "f-duo", 1, "a/duo", "");
    seed_album_duo(&store, "al-duo", 1, r#"["f-duo"]"#);
    let duo_only = lease(&store, |conn| {
        let mut cursor =
            sole_album_artist_ids_in_subtrees(conn, &system_scope(1), &["a/duo".to_string()]);
        let mut collected = Vec::new();
        while !cursor.is_complete() {
            cursor
                .visit_chunk(&cancel, |id| {
                    collected.push(id);
                    Control::Continue
                })
                .unwrap_or_else(|e| panic!("duo chunk: {e}"));
        }
        collected
    });
    assert!(
        duo_only.is_empty(),
        "a two-albumartist album matches no sole id (`:273-275`), got {duo_only:?}"
    );

    std::fs::remove_file(path).ok();
}

#[test]
fn visitor_stops_before_bad_later_row_and_releases_lease() {
    let (store, path) = fresh_store("early-stop-pool1", true);
    seed_library(&store, 1, "A", "/a", SCAN);
    seed_folder(&store, "f1", 1, "a/one", "");
    seed_media(&store, "mf-1", 1, "f1", "a/one/1.mp3", &[]);
    seed_media(&store, "mf-2", 1, "f1", "a/one/2.mp3", &[]);
    // Deliberately LAST (highest rowid): a collect-then-truncate implementation would
    // decode it and report an error instead of `Stopped`.
    seed_media_broken(&store, "zz-broken", 1, "f1", "a/one/3.mp3");

    let cancel = nd_utils::context::Cancellation::new();
    let seen = lease(&store, |conn| {
        let mut seen = Vec::new();
        let outcome = visit_media_in_folder(conn, "f1", None, &cancel, |record| {
            seen.push(record.id.clone());
            if seen.len() == 1 {
                Control::Stop
            } else {
                Control::Continue
            }
        })
        .unwrap_or_else(|e| panic!("an early stop must not error, got {e}"));
        assert_eq!(outcome, VisitOutcome::Stopped { visited: 1 });
        seen
    });
    assert_eq!(
        seen,
        vec!["mf-1".to_string()],
        "true early stop: the callback saw exactly one row"
    );

    // Cancel raised from inside the callback: the NEXT row is fetched, sees the flag
    // and is neither decoded nor delivered.
    let cancel2 = nd_utils::context::Cancellation::new();
    let (outcome, seen2) = lease(&store, |conn| {
        let mut seen = Vec::new();
        let outcome = visit_media_in_folder(conn, "f1", None, &cancel2, |record| {
            seen.push(record.id.clone());
            cancel2.cancel();
            Control::Continue
        })
        .unwrap_or_else(|e| panic!("a cancelled read must not error, got {e}"));
        (outcome, seen)
    });
    assert_eq!(
        outcome,
        VisitOutcome::Stopped { visited: 1 },
        "cancellation stops the read before decoding the next row"
    );
    assert_eq!(seen2.len(), 1);

    // A flag already set before the read stops with zero records while rows exist.
    let pre = nd_utils::context::Cancellation::new();
    pre.cancel();
    let pre_outcome = lease(&store, |conn| {
        visit_media_in_folder(conn, "f1", None, &pre, |record| {
            panic!(
                "no record may be delivered after cancellation, got {:?}",
                record.id
            )
        })
        .unwrap_or_else(|e| panic!("pre-cancelled read: {e}"))
    });
    assert_eq!(pre_outcome, VisitOutcome::Stopped { visited: 0 });

    // Lease release on the Stop path, on a size-1 pool: the idle count is 0 while the
    // visitor and its callback run, and 1 again after — including the callback's own
    // probe, which proves the lease is not returned early (a nested checkout inside
    // the callback would block forever on this pool).
    let (one, one_path) = fresh_store_with_size("stop-lease", true, 1);
    seed_library(&one, 1, "A", "/a", SCAN);
    seed_folder(&one, "f1", 1, "a/one", "");
    seed_media(&one, "mf-1", 1, "f1", "a/one/1.mp3", &[]);
    seed_media(&one, "mf-2", 1, "f1", "a/one/2.mp3", &[]);
    assert_eq!(one.idle_count().unwrap_or_else(|e| panic!("{e}")), 1);
    let outcome = one
        .with_connection_sync(|conn| {
            assert_eq!(
                one.idle_count().unwrap_or_else(|e| panic!("{e}")),
                0,
                "the leased handle must not be idle mid-visitor"
            );
            visit_media_in_folder(conn, "f1", None, &cancel, |record| {
                assert_eq!(
                    one.idle_count().unwrap_or_else(|e| panic!("{e}")),
                    0,
                    "the callback must not see the lease returned"
                );
                let _ = record;
                Control::Stop
            })
            .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))
        })
        .unwrap_or_else(|e| panic!("with_connection_sync or visitor: {e}"));
    assert_eq!(outcome, VisitOutcome::Stopped { visited: 1 });
    assert_eq!(
        one.idle_count().unwrap_or_else(|e| panic!("{e}")),
        1,
        "the Stop path dropped the statement and returned the handle"
    );

    std::fs::remove_file(one_path).ok();
    std::fs::remove_file(path).ok();
}

#[test]
fn visitor_decode_error_is_first_and_closes() {
    let (store, path) = fresh_store_with_size("decode-error-pool1", true, 1);
    seed_library(&store, 1, "A", "/a", SCAN);
    seed_folder(&store, "f1", 1, "a/one", "");
    // Malformed FIRST: Go's per-row ScanStruct aborts there, so no earlier row becomes
    // a result and the failure is NOT folded into end-of-stream (D-CURSOR(3)).
    seed_media_broken(&store, "aaa-broken", 1, "f1", "a/one/1.mp3");
    seed_media(&store, "mf-2", 1, "f1", "a/one/2.mp3", &[]);

    let cancel = nd_utils::context::Cancellation::new();
    let outcome = lease(&store, |conn| {
        visit_media_in_folder(conn, "f1", None, &cancel, |record| {
            panic!(
                "no record may be delivered before the failure, got {:?}",
                record.id
            )
        })
    });
    match outcome {
        Err(CursorError::Sqlite(err)) => {
            let message = err.to_string();
            assert!(
                message.contains(MEDIAFILE_PREFIX),
                "Go's PostScan prefix must survive: {message}"
            );
            assert!(
                message.contains("parsing participants:"),
                "the decode failure must be reported, not swallowed: {message}"
            );
        }
        other => panic!("a decode failure must be Err(CursorError::Sqlite), got {other:?}"),
    }

    // Same failure also proves the error path releases the lease (size-1 pool).
    let idle_during = store
        .with_connection_sync(|conn| {
            let mid = store.idle_count().unwrap_or_else(|e| panic!("{e}"));
            let err = visit_media_in_folder(conn, "f1", None, &cancel, |record| {
                let _ = record;
                Control::Continue
            });
            assert!(err.is_err(), "the malformed row must fail the read");
            Ok(mid)
        })
        .unwrap_or_else(|e| panic!("lease: {e}"));
    assert_eq!(idle_during, 0, "idle while leased");
    assert_eq!(
        store.idle_count().unwrap_or_else(|e| panic!("{e}")),
        1,
        "the error path returned the handle"
    );

    // A Stop after a successful prefix does NOT surface the later malformed row: the
    // first-error order is the row order, so stopping early is a real early stop.
    let (clean, clean_path) = fresh_store("decode-error-order", true);
    seed_library(&clean, 1, "A", "/a", SCAN);
    seed_folder(&clean, "f1", 1, "a/one", "");
    seed_media(&clean, "mf-1", 1, "f1", "a/one/1.mp3", &[]);
    seed_media_broken(&clean, "mf-2", 1, "f1", "a/one/2.mp3");
    seed_media_broken(&clean, "mf-3", 1, "f1", "a/one/3.mp3");
    let outcome = lease(&clean, |conn| {
        visit_media_in_folder(conn, "f1", None, &cancel, |record| {
            let _ = record;
            Control::Stop
        })
        .unwrap_or_else(|e| panic!("stop-before-first-error: {e}"))
    });
    assert_eq!(outcome, VisitOutcome::Stopped { visited: 1 });
    // Without the stop, the FIRST malformed row (rowid 2) is the reported failure.
    let failed = lease(&clean, |conn| {
        visit_media_in_folder(conn, "f1", None, &cancel, |record| {
            let _ = record;
            Control::Continue
        })
    });
    assert!(
        matches!(failed, Err(CursorError::Sqlite(_))),
        "the earliest failing row wins: {failed:?}"
    );

    std::fs::remove_file(clean_path).ok();
    std::fs::remove_file(path).ok();
}

#[test]
fn bounded_channel_cancel_closes_without_deadlock() {
    // The SC seam this module must support: a `spawn_blocking` producer that
    // `blocking_send`s rows into a capacity-1 channel and maps a send failure
    // (receiver dropped) to `Control::Stop`. Backpressure and the phase abort policy
    // are SC's (`SC.query-consumption`); what is proven here is that the visitor
    // stops, drops the statement, and returns its lease — on pool size 1 AND 2, with
    // no deadlock and no lost producer.
    //
    // A multi-thread runtime is required: `blocking_send` panics on a
    // current-thread runtime. Real (unpaused) clock, so the timeouts below are
    // genuine deadlock guards.
    for size in [1usize, 2] {
        let dir = std::path::PathBuf::from(nd_utils::files::temp_file_name(
            &format!("nd-db-scanner-channel-{size}-"),
            ".db",
        ));
        let (pool, first) = Pool::new(&PoolConfig::with_size(dsn(&dir), size)).unwrap();
        apply_fresh_schema(&first).unwrap();
        pool.put_back(first).expect("return configured handle");
        let store = SQLStore::new(pool);

        store
            .with_connection_sync(|conn| {
                conn.execute(
                    "INSERT INTO library (id, name, path, remote_path, last_scan_at, \
                     last_scan_started_at) VALUES (1, 'A', '/a', '', ?, ?)",
                    rusqlite::params![SCAN, SCAN],
                )?;
                conn.execute(
                    "INSERT INTO folder (id, library_id, path, name) VALUES ('f1', 1, 'a/one', 'one')",
                    rusqlite::params![],
                )?;
                for i in 0..50 {
                    conn.execute(
                        "INSERT INTO media_file (id, library_id, folder_id, album_id, path, \
                         title, created_at, updated_at, birth_time) VALUES (?, 1, 'f1', 'al-1', ?, 'T', ?, ?, ?)",
                        rusqlite::params![
                            format!("mf{i}"),
                            format!("a/one/mf{i}.mp3"),
                            T0,
                            T0,
                            T0
                        ],
                    )?;
                }
                Ok(())
            })
            .unwrap_or_else(|e| panic!("seed: {e}"));

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_time()
            .build()
            .unwrap_or_else(|e| panic!("runtime: {e}"));

        let cancel = Arc::new(nd_utils::context::Cancellation::new());
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(1);
        let store_producer = store.clone();
        let cancel_producer = Arc::clone(&cancel);
        let producer = runtime.spawn_blocking(move || {
            let mut delivered = 0usize;
            let outcome = store_producer
                .with_connection_sync(|conn| {
                    assert_eq!(
                        store_producer
                            .idle_count()
                            .unwrap_or_else(|e| panic!("{e}")),
                        size - 1,
                        "the producer holds exactly one lease while visiting (pool {size})"
                    );
                    visit_media_in_folder(conn, "f1", None, &cancel_producer, |record| {
                        match tx.blocking_send(record.id) {
                            Ok(()) => {
                                delivered += 1;
                                Control::Continue
                            }
                            Err(_) => Control::Stop,
                        }
                    })
                    .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))
                })
                .unwrap_or_else(|e| panic!("producer lease or visitor: {e}"));
            (outcome, delivered)
        });

        // Consumer: take 5 rows, then drop the receiver (SC's abort signal).
        let got = runtime.block_on(async {
            let mut got = Vec::new();
            while got.len() < 5 {
                let id = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                    .await
                    .unwrap_or_else(|_| panic!("pool {size}: no row within 5s (deadlock)"))
                    .unwrap_or_else(|| panic!("pool {size}: channel closed early"));
                got.push(id);
            }
            drop(rx);
            got
        });
        let (outcome, delivered) = runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), producer)
                .await
                .unwrap_or_else(|_| panic!("pool {size}: producer never returned (deadlock)"))
                .unwrap_or_else(|e| panic!("producer task: {e}"))
        });
        assert_eq!(got.len(), 5, "the consumer received exactly its 5 rows");
        assert_eq!(
            got.iter()
                .collect::<std::collections::HashSet<&String>>()
                .len(),
            5,
            "the delivered prefix has no duplicates: {got:?}"
        );
        // `visited` counts rows HANDED to the callback; the row whose send failed is
        // one of them, so it is counted even though it never reached the consumer.
        assert_eq!(
            outcome,
            VisitOutcome::Stopped {
                visited: delivered + 1
            },
            "receiver-drop stops the read at the row whose send failed: {delivered} \
             accepted sends, {} visited",
            delivered + 1
        );
        // Scheduling-tolerant by design (F6): the cap-1 slot frees when the consumer
        // receives row 5, so the producer may legally attempt a 6th send BEFORE the
        // receiver is dropped and have it accepted. Pinning `delivered` to exactly 5
        // would assert a scheduler interleaving, not the contract. What IS pinned:
        // the consumer's prefix is exactly 5 rows, the read stopped far short of the
        // 50 available, and the lease came back (idle_count == size below).
        assert!(
            (5..=6).contains(&delivered),
            "producer delivered {delivered} accepted sends: expected 5 (slot closed at \
             receiver drop) or 6 (the slot freed by row 5's receive accepted one more \
             send before the drop)"
        );
        assert!(
            delivered < 50,
            "the read stopped well before exhausting 50 rows ({delivered})"
        );
        assert_eq!(
            store.idle_count().unwrap_or_else(|e| panic!("{e}")),
            size,
            "the lease is back in the pool after the receiver-drop stop"
        );

        // The cancellation flag stops it too, from another thread. Cancelling BEFORE
        // awaiting is deliberate: the blocking task may already be mid-statement,
        // which is exactly why dropping a future is not presented as stopping SQLite
        // (`store.rs:313`). The flag — not the drop — is the stop signal.
        let cancel2 = Arc::new(nd_utils::context::Cancellation::new());
        let store2 = store.clone();
        let flag = Arc::clone(&cancel2);
        let reader = runtime.spawn_blocking(move || {
            store2
                .with_connection_sync(|conn| {
                    visit_media_in_folder(conn, "f1", None, &flag, |record| {
                        let _ = record;
                        Control::Continue
                    })
                    .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))
                })
                .unwrap_or_else(|e| panic!("lease or visitor: {e}"))
        });
        cancel2.cancel();
        let outcome2 = runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), reader)
                .await
                .unwrap_or_else(|_| panic!("pool {size}: cancelled reader hung"))
                .unwrap_or_else(|e| panic!("reader task: {e}"))
        });
        assert!(
            matches!(
                outcome2,
                VisitOutcome::Stopped { .. } | VisitOutcome::Completed { .. }
            ),
            "cancellation yields a typed outcome, not a lost task: {outcome2:?}"
        );
        assert_eq!(
            store.idle_count().unwrap_or_else(|e| panic!("{e}")),
            size,
            "pool {size} fully returned after cancellation"
        );

        drop(runtime);
        std::fs::remove_file(dir).ok();
    }
}

#[test]
fn paths_case_unicode_library_resolution() {
    let (store, path) = two_library_fixture("paths");
    let cancel = nd_utils::context::Cancellation::new();

    // Case-insensitive match (`path collate nocase`, `:385`).
    let nocase = lease(&store, |conn| {
        let mut seen = Vec::new();
        media_by_paths(
            conn,
            &["a/one/MF1.MP3".to_string()],
            None,
            &cancel,
            |record| {
                seen.push(record.id.clone());
                Control::Continue
            },
        )
        .unwrap_or_else(|e| panic!("nocase read: {e}"));
        seen
    });
    assert_eq!(
        nocase,
        vec!["mf1".to_string()],
        "collate nocase matches a differently-cased path"
    );

    // Non-ASCII basename: byte-exact match, no case folding.
    fixture(
        &store,
        "INSERT INTO media_file (id, library_id, folder_id, album_id, path, title, created_at, \
         updated_at, birth_time) VALUES ('mf-uni', 1, 'f1', 'al-1', ?, 'Ünï', ?, ?, ?)",
        &[text("a/one/Ångström/01.mp3"), text(T0), text(T0), text(T0)],
    );
    let uni = lease(&store, |conn| {
        let mut seen = Vec::new();
        media_by_paths(
            conn,
            &["a/one/Ångström/01.mp3".to_string()],
            None,
            &cancel,
            |record| {
                seen.push(record.id.clone());
                Control::Continue
            },
        )
        .unwrap_or_else(|e| panic!("unicode read: {e}"));
        seen
    });
    assert_eq!(uni, vec!["mf-uni".to_string()]);

    // Library-qualified path, searched BOTH ways (`:364-374`) but delivered once.
    let qualified = lease(&store, |conn| {
        let mut seen = Vec::new();
        media_by_paths(
            conn,
            &["2:b/two/MF2.MP3".to_string()],
            None,
            &cancel,
            |record| {
                seen.push(record.id.clone());
                Control::Continue
            },
        )
        .unwrap_or_else(|e| panic!("qualified read: {e}"));
        seen
    });
    assert_eq!(
        qualified,
        vec!["mf2".to_string()],
        "the qualified and unqualified groups overlap on one row; dedup delivers it once"
    );

    // Scope arm hides library 2 from a library-1-only viewer, even through a path that
    // names library 2 explicitly. Negative control right after.
    let denied = lease(&store, |conn| {
        let viewer_scope = user_scope(conn, "viewer");
        let mut seen = Vec::new();
        media_by_paths(
            conn,
            &["2:b/two/MF2.MP3".to_string()],
            Some(&viewer_scope),
            &cancel,
            |record| {
                seen.push(record.id.clone());
                Control::Continue
            },
        )
        .unwrap_or_else(|e| panic!("scoped read: {e}"));
        seen
    });
    assert!(
        denied.is_empty(),
        "the membership clause hides an ungranted library even from a qualified path"
    );
    let control = lease(&store, |conn| {
        let mut seen = Vec::new();
        media_by_paths(
            conn,
            &["2:b/two/MF2.MP3".to_string()],
            None,
            &cancel,
            |record| {
                seen.push(record.id.clone());
                Control::Continue
            },
        )
        .unwrap_or_else(|e| panic!("control read: {e}"));
        seen
    });
    assert_eq!(
        control,
        vec!["mf2".to_string()],
        "negative control: without the clause the same path DOES expose the row"
    );

    // Path chunks 200/201: the renderer splits, and the chunked union delivers the same
    // rows a single statement would.
    let mut many: Vec<String> = Vec::new();
    for i in 0..201 {
        let p = format!("a/one/big{i}.mp3");
        seed_media(&store, &format!("big{i}"), 1, "f1", &p, &[]);
        many.push(p);
    }
    let (rendered, delivered) = lease(&store, |conn| {
        let (by_lib, unqualified) = split_paths(&many[..200]);
        let one = mediafile_repo::find_by_paths_scoped_sql(&by_lib, &unqualified, None)
            .unwrap_or_else(|| panic!("200 paths render"));
        let (by_lib, unqualified) = split_paths(&many[..201]);
        let two = mediafile_repo::find_by_paths_scoped_sql(&by_lib, &unqualified, None)
            .unwrap_or_else(|| panic!("201 paths render"));
        let a = completed(
            media_by_paths(conn, &many[..200], None, &cancel, |record| {
                let _ = record;
                Control::Continue
            })
            .unwrap_or_else(|e| panic!("200 paths: {e}")),
        );
        let b = completed(
            media_by_paths(conn, &many, None, &cancel, |record| {
                let _ = record;
                Control::Continue
            })
            .unwrap_or_else(|e| panic!("201 paths: {e}")),
        );
        (
            (
                one.0.matches("collate nocase IN").count(),
                two.0.matches("collate nocase IN").count(),
            ),
            (a, b),
        )
    });
    assert_eq!(
        rendered,
        (1, 1),
        "each statement carries exactly one unqualified IN-list; the 201st path splits at \
         {PATH_CHUNK_SIZE} into a SECOND statement (declared chunking deviation)"
    );
    assert_eq!(
        delivered,
        (200, 201),
        "the chunked union delivers the same row count a single statement would"
    );

    // Empty batch issues NO statement: run it on an UNMIGRATED DB, where any SQL at
    // all fails with `no such table`.
    let (bare, bare_path) = fresh_store("paths-unmigrated", false);
    let empty = bare
        .with_connection_sync(|conn| {
            Ok(media_by_paths(conn, &[], None, &cancel, |record| {
                let _ = record;
                Control::Continue
            }))
        })
        .unwrap_or_else(|e| panic!("an empty batch must issue no SQL at all: {e}"))
        .unwrap_or_else(|e| panic!("empty batch on an unmigrated DB: {e}"));
    assert_eq!(
        empty,
        VisitOutcome::Completed { visited: 0 },
        "an empty path batch must not touch the database (`:393-395`)"
    );
    std::fs::remove_file(bare_path).ok();
    std::fs::remove_file(path).ok();
}

#[test]
fn visible_point_read_denies_other_library_for_all_aliases() {
    let (store, path) = two_library_fixture("point-read");

    // A viewer of library 1 only cannot read library 2's row…
    let denied = lease(&store, |conn| {
        get_visible_media(conn, "mf2", "viewer", false)
    });
    assert!(
        matches!(denied, Err(VisibleMediaError::NotFound)),
        "a scoped point read must deny an ungranted library, got {denied:?}"
    );
    // …while the SAME row reads for an admin, a headless context, and a grantee of both.
    let admin = lease(&store, |conn| get_visible_media(conn, "mf2", "boss", false));
    assert_eq!(admin.unwrap().id, "mf2");
    let headless = lease(&store, |conn| get_visible_media(conn, "mf2", "-1", false));
    assert_eq!(headless.unwrap().id, "mf2");
    let granted_all = lease(&store, |conn| {
        get_visible_media(conn, "mf2", "grantee", false)
    });
    assert_eq!(granted_all.unwrap().id, "mf2");

    // Negative control for "omitted alias": the low-level read with NO clause really
    // does expose the unauthorized row, so the denial above is the clause's doing.
    let control = lease(&store, |conn| {
        MediaFileRepo::new(conn)
            .get("mf2", None, None, false)
            .unwrap_or_else(|e| panic!("unscoped control: {e}"))
    });
    assert_eq!(control.id, "mf2");

    // Same annotation/bookmark identity Go's Get produces for a logged-in user, and
    // skipped for the headless id (`sql_annotations.go:63-65`).
    fixture(
        &store,
        "INSERT INTO annotation (user_id, item_id, item_type, play_count, rating, starred) \
         VALUES (?, 'mf1', 'media_file', 3, 5, 1)",
        &[text("viewer")],
    );
    let annotated = lease(&store, |conn| {
        get_visible_media(conn, "mf1", "viewer", false)
    });
    let annotated = annotated.unwrap_or_else(|e| panic!("annotated read: {e}"));
    assert_eq!(annotated.annotation.play_count, 3);
    assert_eq!(annotated.annotation.rating, 5);
    let headless_row = lease(&store, |conn| get_visible_media(conn, "mf1", "-1", false));
    let headless_row = headless_row.unwrap_or_else(|e| panic!("headless read: {e}"));
    assert_eq!(
        headless_row.annotation.play_count, 0,
        "Go's withAnnotation returns early for invalidUserId"
    );

    // cover_flag == enable_media_file_cover_art, and Go's swallow behavior.
    fixture(
        &store,
        "UPDATE media_file SET has_cover_art = 1 WHERE id = 'mf1'",
        &[],
    );
    fixture(
        &store,
        "INSERT INTO item_artwork (item_kind, item_id, image_type, hash, source) VALUES \
         ('mf', 'mf1', 'primary', 'HASH-1', 'embedded')",
        &[],
    );
    let on = lease(&store, |conn| {
        get_visible_media(conn, "mf1", "viewer", true)
    });
    assert_eq!(
        on.unwrap_or_else(|e| panic!("flag on: {e}"))
            .image
            .image_hash,
        "HASH-1"
    );
    let off = lease(&store, |conn| {
        get_visible_media(conn, "mf1", "viewer", false)
    });
    assert_eq!(
        off.unwrap_or_else(|e| panic!("flag off: {e}"))
            .image
            .image_hash,
        "",
        "the flag OFF leaves artwork unresolved (conf/configuration.go:1003)"
    );
    fixture(
        &store,
        "DELETE FROM item_artwork WHERE item_kind = 'mf'",
        &[],
    );
    let unresolved = lease(&store, |conn| {
        get_visible_media(conn, "mf1", "viewer", true)
    });
    assert_eq!(
        unresolved
            .unwrap_or_else(|e| panic!("unresolved read: {e}"))
            .image
            .image_hash,
        "",
        "a missing artwork row is UNRESOLVED, not an error (artwork_hydration.go:62-66)"
    );

    // The three clause shapes, rendered by the builders and EXECUTED over aliased
    // queries. (HTTP-level alias wiring is API-02's; only the clause shapes are pinned.)
    assert_eq!(ScopeAlias::MediaFile.table(), mediafile_repo::TABLE);
    assert_eq!(ScopeAlias::FolderAlias.table(), "f");
    let (self_clause, self_args) = membership_clause(ScopeAlias::MediaFile, "u1");
    assert_eq!(
        self_clause,
        "media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
    );
    assert_eq!(self_args, vec![Value::from("u1")]);
    let (alias, alias_args) = membership_clause(ScopeAlias::FolderAlias, "u1");
    assert_eq!(
        alias,
        "f.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
    );
    assert_eq!(alias_args, vec![Value::from("u1")]);
    let (artist_empty, artist_empty_args) = artist_scope_clause(&[]);
    assert_eq!(
        artist_empty,
        artist_repo::FALSE_EXPR,
        "empty ids render match-nothing"
    );
    assert!(artist_empty_args.is_empty());
    let (artist_some, artist_args) = artist_scope_clause(&[1, 2]);
    assert!(
        artist_some.starts_with("EXISTS (SELECT 1 FROM library_artist"),
        "{artist_some}"
    );
    assert_eq!(artist_args, vec![Value::Integer(1), Value::Integer(2)]);
    // An authority resolved for a user carries the clause for ITS alias only, and
    // exposes it through the doc(hidden) seam — never through the public surface.
    let folder_authority = lease(&store, |conn| {
        UserScope::for_user("viewer".to_string())
            .resolve(conn, ScopeAlias::FolderAlias)
            .unwrap_or_else(|e| panic!("folder-alias resolve: {e}"))
    });
    assert_eq!(
        folder_authority.rendered_clause(),
        Some((
            "f".to_string(),
            alias.clone(),
            vec![Value::from("viewer".to_string())]
        )),
        "the FolderAlias authority renders the f-qualified clause for ITS user"
    );
    let self_authority = lease(&store, |conn| user_scope(conn, "viewer"));
    assert_eq!(
        self_authority.rendered_clause().map(|(table, _, _)| table),
        Some("media_file".to_string()),
        "the MediaFile authority renders the self-table clause"
    );
    assert_eq!(
        self_authority.decision(),
        ScopeDecision::Denied,
        "a one-of-two grantee is not unrestricted"
    );

    let counts = lease(&store, |conn| {
        let (clause, args) = membership_clause(ScopeAlias::FolderAlias, "viewer");
        let sql = format!("SELECT count(*) FROM folder f WHERE {clause}");
        let granted: i64 =
            conn.query_row(&sql, rusqlite::params_from_iter(args.iter()), |row| {
                row.get(0)
            })?;
        let (_clause, args) = membership_clause(ScopeAlias::FolderAlias, "nobody");
        let none: i64 = conn.query_row(&sql, rusqlite::params_from_iter(args.iter()), |row| {
            row.get(0)
        })?;
        conn.execute(
            "INSERT INTO artist (id, name) VALUES ('ar-1', 'Sole')",
            rusqlite::params![],
        )?;
        conn.execute(
            "INSERT INTO library_artist (library_id, artist_id) VALUES (1, 'ar-1')",
            rusqlite::params![],
        )?;
        let (clause, args) = artist_scope_clause(&[1]);
        let artist_sql = format!("SELECT count(*) FROM artist WHERE {clause}");
        let artist_granted: i64 = conn.query_row(
            &artist_sql,
            rusqlite::params_from_iter(args.iter()),
            |row| row.get(0),
        )?;
        let (clause, empty_args) = artist_scope_clause(&[]);
        let artist_empty_sql = format!("SELECT count(*) FROM artist WHERE {clause}");
        let artist_none: i64 = conn.query_row(
            &artist_empty_sql,
            rusqlite::params_from_iter(empty_args.iter()),
            |row| row.get(0),
        )?;
        Ok::<_, rusqlite::Error>((granted, none, artist_granted, artist_none))
    })
    .unwrap_or_else(|e| panic!("alias executions: {e}"));
    assert_eq!(
        counts,
        (1, 0, 1, 0),
        "the f-alias clause counts only the granted library's folder; the artist EXISTS \
         counts a granted artist and NOTHING for an empty id list"
    );

    std::fs::remove_file(path).ok();
}

#[test]
fn empty_scope_denies_not_headless() {
    let (store, path) = two_library_fixture("empty-scope");
    seed_user(&store, "zero-grants", false);

    // Zero grants against a NON-EMPTY library table: `userSeesAllLibraries`
    // (`:295-298`) is false, so the clause is applied and nothing is visible.
    let zero = lease(&store, |conn| {
        resolve_user(conn, "zero-grants".to_string(), ScopeAlias::MediaFile)
    })
    .unwrap_or_else(|e| panic!("resolve_user: {e}"));
    assert_eq!(zero.decision(), ScopeDecision::Denied);
    let (table, clause, args) = zero
        .rendered_clause()
        .expect("zero grants must render a deny clause, not an unrestricted authority");
    assert_eq!(table, mediafile_repo::TABLE);
    assert!(
        clause.contains(
            ".library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
        ),
        "{clause}"
    );
    assert_eq!(args, vec![Value::from("zero-grants")]);
    assert!(
        !clause.contains("1=1") && !clause.contains("1 = 1"),
        "no `1=1` may be emitted for a user arm: {clause}"
    );

    let cancel = nd_utils::context::Cancellation::new();
    let denied = lease(&store, |conn| {
        let mut seen = Vec::new();
        media_by_paths(
            conn,
            &["a/one/MF1.MP3".to_string()],
            Some(&zero),
            &cancel,
            |record| {
                seen.push(record.id.clone());
                Control::Continue
            },
        )
        .unwrap_or_else(|e| panic!("zero-grant read: {e}"));
        seen
    });
    assert!(
        denied.is_empty(),
        "an empty grant set matches nothing, not everything"
    );
    let point = lease(&store, |conn| {
        get_visible_media(conn, "mf1", "zero-grants", false)
    });
    assert!(
        matches!(point, Err(VisibleMediaError::NotFound)),
        "the point read denies too, got {point:?}"
    );

    // All-grants fast path with a non-empty table: clause OMITTED (None) — and the
    // visible read above is what makes None meaningful.
    let grantee = lease(&store, |conn| {
        resolve_user(conn, "grantee".to_string(), ScopeAlias::MediaFile)
    })
    .unwrap_or_else(|e| panic!("resolve_user: {e}"));
    assert_eq!(grantee.decision(), ScopeDecision::AllLibraries);
    assert_eq!(
        grantee.rendered_clause(),
        None,
        "all-grants omits the clause"
    );

    let admin = lease(&store, |conn| {
        resolve_user(conn, "boss".to_string(), ScopeAlias::MediaFile)
    })
    .unwrap_or_else(|e| panic!("resolve_user: {e}"));
    assert_eq!(admin.decision(), ScopeDecision::Admin);
    assert_eq!(admin.rendered_clause(), None);
    let headless = lease(&store, |conn| {
        resolve_user(conn, "-1".to_string(), ScopeAlias::MediaFile)
    })
    .unwrap_or_else(|e| panic!("resolve_user: {e}"));
    assert_eq!(headless.decision(), ScopeDecision::Headless);
    assert_eq!(headless.rendered_clause(), None);

    // A `user` row that grants nothing (and a row that does not exist) is DENIED, not
    // unrestricted, and not an error.
    let ghost = lease(&store, |conn| {
        resolve_user(conn, "ghost".to_string(), ScopeAlias::MediaFile)
    })
    .unwrap_or_else(|e| panic!("a missing user must not fail resolution: {e}"));
    assert_eq!(ghost.decision(), ScopeDecision::Denied);
    assert!(ghost.rendered_clause().is_some());

    // EMPTY library table: `count(*) == 0` ⇒ `false` ⇒ clause applied. The scope still
    // denies rather than becoming headless.
    let (empty_db, empty_path) = fresh_store("empty-library-table", true);
    seed_user(&empty_db, "solo", false);
    let empty = lease(&empty_db, |conn| {
        resolve_user(conn, "solo".to_string(), ScopeAlias::MediaFile)
    })
    .unwrap_or_else(|e| panic!("resolve_user: {e}"));
    assert_eq!(
        empty.decision(),
        ScopeDecision::Denied,
        "count(*) == 0 must NOT be treated as seeing everything (`:295-298`)"
    );
    assert!(empty.rendered_clause().is_some());
    std::fs::remove_file(empty_path).ok();
    std::fs::remove_file(path).ok();
}

#[test]
fn membership_change_next_request_reloads() {
    // No caching: every request re-resolves from `user`/`user_library`/`library`.
    let (store, path) = two_library_fixture("membership-change");

    let before = lease(&store, |conn| {
        get_visible_media(conn, "mf2", "grantee", false)
    });
    assert_eq!(
        before.unwrap_or_else(|e| panic!("granted both: {e}")).id,
        "mf2"
    );

    revoke(&store, "grantee", 2);
    let after = lease(&store, |conn| {
        get_visible_media(conn, "mf2", "grantee", false)
    });
    assert!(
        matches!(after, Err(VisibleMediaError::NotFound)),
        "the next request re-reads the grants and denies, got {after:?}"
    );
    let still_visible = lease(&store, |conn| {
        get_visible_media(conn, "mf1", "grantee", false)
    });
    assert_eq!(
        still_visible
            .unwrap_or_else(|e| panic!("retained grant: {e}"))
            .id,
        "mf1",
        "the retained grant still works"
    );

    grant(&store, "grantee", 2);
    let again = lease(&store, |conn| {
        get_visible_media(conn, "mf2", "grantee", false)
    });
    assert_eq!(
        again.unwrap_or_else(|e| panic!("re-granted: {e}")).id,
        "mf2",
        "re-granting restores visibility on the next request with no restart"
    );

    std::fs::remove_file(path).ok();
}

#[test]
fn scanner_reads_never_hydrate_artwork() {
    // Go's GetCursor does NOT hydrate (`artwork_hydration_test.go:633`), so a scanner
    // read must leave both artwork fields unresolved even with artwork rows present —
    // while the hydrated read on the SAME DB resolves them.
    let (store, path) = two_library_fixture("no-artwork");
    fixture(
        &store,
        "UPDATE media_file SET has_cover_art = 1 WHERE id IN ('mf1', 'mf2')",
        &[],
    );
    for (kind, id, hash) in [
        ("mf", "mf1", "HASH-MF"),
        ("al", "al-1", "HASH-AL"),
        ("al", "al-2", "HASH-AL2"),
    ] {
        fixture(
            &store,
            "INSERT INTO item_artwork (item_kind, item_id, image_type, hash, source) VALUES (?, ?, 'primary', ?, 'embedded')",
            &[text(kind), text(id), text(hash)],
        );
    }

    let cancel = nd_utils::context::Cancellation::new();
    let visited = lease(&store, |conn| {
        let mut seen = Vec::new();
        visit_media_in_folder(conn, "f1", None, &cancel, |record| {
            seen.push((
                record.image.image_hash.clone(),
                record.album_image.image_hash.clone(),
            ));
            Control::Continue
        })
        .unwrap_or_else(|e| panic!("visitor: {e}"));
        seen
    });
    assert_eq!(
        visited,
        vec![("".to_string(), "".to_string())],
        "no artwork lookup on a Go-unhydrated scanner read"
    );

    let hydrated = lease(&store, |conn| {
        MediaFileRepo::new(conn)
            .get_all(None, None, &["media_file.id"], 0, 0, true)
            .unwrap_or_else(|e| panic!("get_all: {e}"))
    });
    let mf1 = hydrated
        .items
        .iter()
        .find(|m| m.id == "mf1")
        .unwrap_or_else(|| panic!("mf1 present"));
    assert_eq!(
        mf1.image.image_hash, "HASH-MF",
        "the control read DOES hydrate, so the fixture is not vacuous"
    );

    // GetMissingAndMatching is unhydrated too (`:494-517`).
    fixture(
        &store,
        "UPDATE media_file SET missing = 1, pid = 'p-x' WHERE id = 'mf2'",
        &[],
    );
    let missing = lease(&store, |conn| {
        let mut seen = Vec::new();
        visit_missing_and_matching(conn, &system_scope(2), &cancel, |record| {
            seen.push((record.id.clone(), record.image.image_hash.clone()));
            Control::Continue
        })
        .unwrap_or_else(|e| panic!("missing read: {e}"));
        seen
    });
    assert_eq!(missing, vec![("mf2".to_string(), "".to_string())]);

    std::fs::remove_file(path).ok();
}

#[test]
fn find_by_paths_without_scope_is_textually_unchanged() {
    // The additive renderer must keep the existing no-scope call byte-identical
    // (Go's admin/headless callers: cmd/missing.go:139, core/playlists/parse_m3u.go:97).
    let by_library = vec![(1i64, vec!["foo.mp3".to_string()])];
    let unqualified = vec!["bar.mp3".to_string()];
    let plain = mediafile_repo::find_by_paths_sql(&by_library, &unqualified)
        .unwrap_or_else(|| panic!("groups render"));
    let scoped_none = mediafile_repo::find_by_paths_scoped_sql(&by_library, &unqualified, None)
        .unwrap_or_else(|| panic!("scoped-with-None renders"));
    assert_eq!(plain, scoped_none, "no-scope behavior is unchanged");
    let (sql, args) = mediafile_repo::find_by_paths_scoped_sql(
        &by_library,
        &unqualified,
        Some(membership_clause(ScopeAlias::MediaFile, "u")),
    )
    .unwrap_or_else(|| panic!("scoped groups render"));
    assert_eq!(
        sql,
        "SELECT * FROM media_file WHERE ((path collate nocase IN (?) AND library_id = ?) \
         OR path collate nocase IN (?)) AND media_file.library_id IN (SELECT ul.library_id \
         FROM user_library ul WHERE ul.user_id = ?)"
    );
    assert_eq!(
        args,
        vec![
            Value::from("foo.mp3"),
            Value::Integer(1),
            Value::from("bar.mp3"),
            Value::from("u"),
        ],
        "path args bind first (`Where(query)` before `applyLibraryFilter`, `:397`)"
    );
    assert_eq!(
        mediafile_repo::find_by_paths_scoped_sql(
            &[],
            &[],
            Some(membership_clause(ScopeAlias::MediaFile, "u"))
        ),
        None,
        "a scope alone never issues a statement (`:393-395`)"
    );
}

#[test]
fn cursor_chunk_constants_match_go() {
    // D-CURSOR(4): cursor id chunk 600, artwork info batch 200, touch/missing 200,
    // subtree paths 100.
    assert_eq!(
        artist_repo::CURSOR_CHUNK_SIZE,
        600,
        "artworkChunkSize = artworkBatchSize*3 = 600 (artwork_hydration.go:15-17)"
    );
    assert_eq!(mediafile_repo::CHUNK_SIZE, 200);
    assert_eq!(ALBUM_IDS_FOLDER_CHUNK_SIZE, 200);
    assert_eq!(SUBTREE_PATH_CHUNK_SIZE, 100);
    assert_eq!(PATH_CHUNK_SIZE, 200);
}

#[test]
fn split_paths_qualifies_numeric_prefixes_and_keeps_every_path() {
    let (by_library, unqualified) = split_paths(&[
        "1:foo.mp3".to_string(),
        "2:bar.mp3".to_string(),
        "1999: A Life/01.mp3".to_string(),
        "plain.mp3".to_string(),
    ]);
    assert_eq!(
        by_library,
        vec![
            (1, vec!["foo.mp3".to_string()]),
            (2, vec!["bar.mp3".to_string()]),
            (1999, vec![" A Life/01.mp3".to_string()]),
        ],
        "numeric prefixes qualify, in ascending library order (`:383`)"
    );
    assert_eq!(
        unqualified,
        vec![
            "1:foo.mp3".to_string(),
            "2:bar.mp3".to_string(),
            "1999: A Life/01.mp3".to_string(),
            "plain.mp3".to_string(),
        ],
        "every path is also searched unqualified (`:379`)"
    );
    assert_eq!(split_paths(&[]).0, Vec::new(), "no paths, no groups");
}

#[test]
fn time_bound_and_headless_constant_are_typed() {
    // The typed chrono seam the scanner lane needs, and the headless identity.
    let from = stamp(SCAN);
    let until = stamp(AFTER);
    let bound = scanner_queries::TimeBound {
        from: &from,
        until: &until,
    };
    assert!(bound.contains(&stamp(SCAN)), "lower bound is inclusive");
    assert!(!bound.contains(&stamp(AFTER)), "upper bound is exclusive");
    assert!(!bound.contains(&stamp(T0)));
    assert_eq!(
        scanner_queries::HEADLESS_USER_ID,
        artist_repo::INVALID_USER_ID,
        "one constant for Go's invalidUserId (`sql_base_repository.go:51`)"
    );
}

#[test]
fn chunked_cursor_allows_cross_chunk_drift_on_one_lease() {
    // D-CURSOR(1): the IdChunked kind keeps ONE leased connection across chunks and
    // intentionally allows drift. A row inserted between chunks is therefore visible
    // to the later chunk — pinned as behavior, not assumed away.
    let (store, path) = fresh_store("drift", true);
    seed_library(&store, 1, "A", "/a", SCAN);
    seed_folder(&store, "f1", 1, "a/one", "");
    seed_folder(&store, "f2", 1, "a/two", "");
    seed_media(
        &store,
        "mf-1",
        1,
        "f1",
        "a/one/1.mp3",
        &[("album_id", "al-1")],
    );

    let cancel = nd_utils::context::Cancellation::new();
    // f1 in chunk 1 (slot 0) and f2 in chunk 2 (slot 200): two statements on one lease.
    let spaced = {
        let mut ids = vec!["f1".to_string()];
        ids.extend((0..199).map(|i| format!("filler{i}")));
        ids.push("f2".to_string());
        ids
    };
    let first_pass = lease(&store, |conn| {
        let mut cursor = album_ids_by_folders(conn, &system_scope(1), &spaced);
        let mut seen = Vec::new();
        // Chunk 1 only sees mf-1…
        cursor
            .visit_chunk(&cancel, |id| {
                seen.push(id);
                Control::Continue
            })
            .unwrap_or_else(|e| panic!("chunk 1: {e}"));
        // …then a write lands on the SAME connection between chunks…
        conn.execute(
            "INSERT INTO media_file (id, library_id, folder_id, album_id, path, title, created_at, \
             updated_at, birth_time) VALUES ('mf-2', 1, 'f2', 'al-2', 'a/two/2.mp3', 'T', ?, ?, ?)",
            rusqlite::params![T0, T0, T0],
        )
        .unwrap_or_else(|e| panic!("drift insert: {e}"));
        // …and chunk 2 sees it: no snapshot, one lease, drift allowed.
        cursor
            .visit_chunk(&cancel, |id| {
                seen.push(id);
                Control::Continue
            })
            .unwrap_or_else(|e| panic!("chunk 2: {e}"));
        seen
    });
    assert_eq!(first_pass, vec!["al-1".to_string(), "al-2".to_string()]);

    // Stopping mid-cursor leaves the cursor usable for the next chunk (the statement
    // closed, the lease did not).
    let mut second_pass: Vec<String> = Vec::new();
    let partial = lease(&store, |conn| {
        let mut cursor = album_ids_by_folders(conn, &system_scope(1), &spaced);
        let first = cursor
            .visit_chunk(&cancel, |id| {
                let _ = id;
                Control::Stop
            })
            .unwrap_or_else(|e| panic!("stop chunk: {e}"));
        assert_eq!(first, VisitOutcome::Stopped { visited: 1 });
        let second = cursor
            .visit_chunk(&cancel, |id| {
                second_pass.push(id.clone());
                Control::Continue
            })
            .unwrap_or_else(|e| panic!("continue after stop: {e}"));
        (first, second)
    });
    assert_eq!(partial.1, VisitOutcome::Completed { visited: 1 });

    std::fs::remove_file(path).ok();
}

#[test]
fn subtree_root_path_drops_earlier_args_in_mixed_batch() {
    // F2. Go `folder_repository.go:214-216` RETURNS a fresh
    // `And{Eq{folder.library_id}, Eq{folder.missing,false}}` the moment a path
    // cleans to ".", so earlier paths' conditions AND args are dropped. The Rust
    // builder used to pre-build the args vector and return it early, so a mixed
    // batch rendered the 2-parameter whole-library clause with 5+ args and every
    // executed read failed with InvalidParameterCount.
    let (store, path) = fresh_store("subtree-root-args", true);
    seed_library(&store, 1, "A", "/a", SCAN);
    seed_folder(&store, "f1", 1, "a/one", "");
    seed_folder(&store, "f1-child", 1, "a/one/disc1", "f1");
    seed_album(&store, "al-1", 1, AFTER, r#"["f1"]"#);

    // Render shapes first: root FIRST and root LAST both come out as the fresh
    // 2-arg whole-library clause; single-root is byte-identical to before.
    let (root_first, args_first) = folder_repo::folder_subtree_filter(
        1,
        &[".".to_string(), "a/one".to_string(), "a/two".to_string()],
    );
    let (root_last, args_last) = folder_repo::folder_subtree_filter(
        1,
        &["a/one".to_string(), "a/two".to_string(), "./".to_string()],
    );
    let (single, args_single) =
        folder_repo::folder_subtree_filter(1, &["".to_string(), ".".to_string()]);
    assert_eq!(
        root_first, "(folder.library_id = ? AND folder.missing = ?)",
        "a root path anywhere in the batch drops the path conditions"
    );
    assert_eq!(root_first, root_last, "root placement FIRST and LAST agree");
    assert_eq!(
        root_first, single,
        "single-root rendering is byte-identical"
    );
    assert_eq!(args_first, vec![Value::Integer(1), Value::Bool(false)]);
    assert_eq!(args_last, vec![Value::Integer(1), Value::Bool(false)]);
    assert_eq!(args_single, vec![Value::Integer(1), Value::Bool(false)]);

    // EXECUTED: the mixed batches really bind and run. The stale-args builder
    // failed here with InvalidParameterCount(4, 3) / (8, 3).
    let counts = lease(&store, |conn| {
        let mut out = Vec::new();
        for paths in [
            vec![".".to_string(), "a/one".to_string(), "a/two".to_string()],
            vec!["a/one".to_string(), "a/two".to_string(), "./".to_string()],
            vec!["".to_string()],
        ] {
            let (clause, args) = folder_repo::folder_subtree_filter(1, &paths);
            let sql = format!("SELECT count(*) FROM folder WHERE {clause}");
            out.push(
                conn.query_row(&sql, rusqlite::params_from_iter(args.iter()), |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap_or_else(|e| panic!("{paths:?} batch failed to bind: {e}")),
            );
        }
        Ok::<_, rusqlite::Error>(out)
    })
    .unwrap_or_else(|e| panic!("lease: {e}"));
    assert_eq!(
        counts,
        vec![2, 2, 2],
        "the root-path batch counts the whole library (f1 + f1-child), regardless of \
         where the root path sits in the batch"
    );

    // The reader that exposed the bug: a mixed batch through the typed cursor.
    // One root path anywhere in the batch means one chunk, so one visit suffices.
    let cancel = nd_utils::context::Cancellation::new();
    let ids = lease(&store, |conn| {
        let mut cursor = sole_album_artist_ids_in_subtrees(
            conn,
            &system_scope(1),
            &["a/one".to_string(), ".".to_string()],
        );
        let mut ids = Vec::new();
        cursor
            .visit_chunk(&cancel, |id| {
                ids.push(id);
                Control::Continue
            })
            .unwrap_or_else(|e| panic!("mixed root batch cursor: {e}"));
        ids
    });
    assert_eq!(ids, vec!["ar-1".to_string()]);

    std::fs::remove_file(path).ok();
}

#[test]
fn sole_artist_subtree_includes_missing_albums() {
    // F3. Go `album_repository.go:294-296` is `Where(And{soleAlbumArtistFilter,
    // inSubtree})` — no album-missing predicate — so the image-change queue also
    // sees albums whose folders vanished. The Rust statement carried
    // `AND album.missing = ?`, which over-restricted it.
    let (store, path) = fresh_store("subtree-missing-album", true);
    seed_library(&store, 1, "A", "/a", SCAN);
    seed_folder(&store, "f1", 1, "a/one", "");
    seed_folder(&store, "f2", 1, "a/two", "");
    seed_album(&store, "al-present", 1, AFTER, r#"["f1"]"#);
    seed_album(&store, "al-missing", 1, AFTER, r#"["f2"]"#);
    fixture(
        &store,
        "UPDATE album SET missing = 1 WHERE id = 'al-missing'",
        &[],
    );

    // Pin the rendered statement: no album.missing predicate at all.
    let statements =
        album_repo::get_sole_album_artist_ids_in_subtrees_sql(1, &["a/two".to_string()]);
    assert_eq!(statements.len(), 1);
    assert!(
        !statements[0].0.contains("album.missing"),
        "Go has no album-missing predicate (`:294-296`): {}",
        statements[0].0
    );
    assert_eq!(
        statements[0].1,
        vec![
            Value::Integer(1),
            Value::Bool(false),
            Value::from(folder_repo::folder_id(1, "", "a/two")),
            Value::from("a/two"),
            Value::from("a/two/%"),
        ],
        "the args are the subtree filter's own: library id, folder.missing, then the \
         per-path id/path/prefix triple — no extra missing bind for the album"
    );

    let cancel = nd_utils::context::Cancellation::new();
    let present_only = lease(&store, |conn| {
        let mut cursor =
            sole_album_artist_ids_in_subtrees(conn, &system_scope(1), &["a/one".to_string()]);
        let mut ids = Vec::new();
        while !cursor.is_complete() {
            cursor
                .visit_chunk(&cancel, |id| {
                    ids.push(id.clone());
                    Control::Continue
                })
                .unwrap_or_else(|e| panic!("present subtree: {e}"));
        }
        ids
    });
    assert_eq!(present_only, vec!["ar-1".to_string()]);

    let missing_subtree = lease(&store, |conn| {
        let mut cursor =
            sole_album_artist_ids_in_subtrees(conn, &system_scope(1), &["a/two".to_string()]);
        let mut ids = Vec::new();
        while !cursor.is_complete() {
            cursor
                .visit_chunk(&cancel, |id| {
                    ids.push(id.clone());
                    Control::Continue
                })
                .unwrap_or_else(|e| panic!("missing-album subtree: {e}"));
        }
        ids
    });
    assert_eq!(
        missing_subtree,
        vec!["ar-1".to_string()],
        "a sole-artist album with missing = 1 in the matching subtree is reported, as \
         Go reports it"
    );

    std::fs::remove_file(path).ok();
}

#[test]
fn chunk_error_is_terminal_and_never_resumes() {
    // F4. D-CURSOR(1): the id-chunked kind is a NON-resumable primitive and
    // scanner phases treat a cursor error as fatal. A failed chunk must close the
    // cursor: the first error is the reported one and every later call is
    // CursorError::Closed, never chunk 2's rows.
    let (store, path) = fresh_store("chunk-terminal-error", true);
    seed_library(&store, 1, "A", "/a", SCAN);
    seed_folder(&store, "f1", 1, "a/one", "");
    seed_folder(&store, "f2", 1, "a/two", "");
    seed_media(
        &store,
        "mf-1",
        1,
        "f1",
        "a/one/1.mp3",
        &[("album_id", "al-1")],
    );
    seed_media(
        &store,
        "mf-2",
        1,
        "f2",
        "a/two/2.mp3",
        &[("album_id", "al-2")],
    );

    // Chunk 1 must fail on a statement that cannot prepare: a folder chunk that
    // renders valid SQL is hard to poison without touching production builders, so
    // build the cursor from one deliberately broken statement plus one valid one
    // through the internal construction seam.
    let broken = || {
        (
            "SELECT distinct album_id FROM media_file WHERE no_such_column = ?".to_string(),
            vec![Value::from("f1")],
        )
    };
    let valid = mediafile_repo::get_album_ids_by_folder_sql(1, &["f2".to_string()])
        .into_iter()
        .next()
        .expect("one valid chunk statement");
    let cancel = nd_utils::context::Cancellation::new();
    let outcome = lease(&store, |conn| {
        let mut cursor = scanner_queries::IdChunkedCursor::<String>::new(
            conn,
            vec![broken(), valid],
            |row| row.get::<_, String>(0),
            None,
        );
        let first = cursor.visit_chunk(&cancel, |id| {
            let _ = id;
            Control::Continue
        });
        let second = cursor.visit_chunk(&cancel, |id| {
            panic!("a failed cursor must never deliver a later chunk, got {id}");
        });
        (
            first,
            second,
            cursor.is_complete(),
            cursor.remaining_chunks(),
        )
    });
    assert!(
        matches!(outcome.0, Err(CursorError::Sqlite(_))),
        "the first failure is the reported one: {:?}",
        outcome.0
    );
    assert!(
        matches!(outcome.1, Err(CursorError::Closed)),
        "D-CURSOR(1): after a chunk error the cursor is Closed, never resumed: {:?}",
        outcome.1
    );
    assert!(outcome.2, "a failed cursor reports itself complete");
    assert_eq!(
        outcome.3, 1,
        "the unread chunk is not consumed by the failed call"
    );

    // Same for a failure in the LAST chunk: the cursor is closed, not silently
    // "completed".
    let last_chunk = lease(&store, |conn| {
        let mut cursor = scanner_queries::IdChunkedCursor::<String>::new(
            conn,
            vec![
                mediafile_repo::get_album_ids_by_folder_sql(1, &["f1".to_string()])
                    .into_iter()
                    .next()
                    .expect("chunk 1"),
                broken(),
            ],
            |row| row.get::<_, String>(0),
            None,
        );
        let first = cursor.visit_chunk(&cancel, |id| {
            assert_eq!(id, "al-1");
            Control::Continue
        });
        let second = cursor.visit_chunk(&cancel, |id| {
            let _ = id;
            Control::Continue
        });
        let third = cursor.visit_chunk(&cancel, |id| {
            let _ = id;
            Control::Continue
        });
        (first, second, third)
    });
    assert_eq!(
        last_chunk
            .0
            .unwrap_or_else(|e| panic!("chunk 1 must succeed: {e}")),
        VisitOutcome::Completed { visited: 1 }
    );
    assert!(
        matches!(last_chunk.1, Err(CursorError::Sqlite(_))),
        "the last chunk's failure is reported: {:?}",
        last_chunk.1
    );
    assert!(
        matches!(last_chunk.2, Err(CursorError::Closed)),
        "and the cursor stays closed afterwards: {:?}",
        last_chunk.2
    );

    std::fs::remove_file(path).ok();
}

#[test]
fn scope_authority_exposes_no_sql_and_scanner_reads_take_only_a_library() {
    // F1, structurally. The public surface hands out no clause text and no
    // caller-supplied scope: an authority is only obtainable by resolving one, and
    // the system readers take `&ScannerScope`, whose only content is a library id.
    let (store, path) = two_library_fixture("opaque-scope");
    let scope = lease(&store, |conn| user_scope(conn, "viewer"));
    assert_eq!(scope.user_id(), "viewer");
    assert_eq!(scope.decision(), ScopeDecision::Denied);
    // A freshly-built identity has NO clause: constructing one grants nothing.
    let unauth = UserScope::for_user("mf2".to_string());
    assert_eq!(
        unauth.rendered_clause(),
        None,
        "for_user alone is not a read authority"
    );
    assert_eq!(unauth.decision(), ScopeDecision::Denied);

    // ScannerScope carries a library id and nothing else.
    let system = system_scope(7);
    assert_eq!(system.library_id(), 7);
    assert_eq!(system, ScannerScope::for_library(7));

    // An authority resolved for one alias cannot narrow a different alias's read:
    // the folder-alias clause would qualify the wrong table, so media reads use the
    // MediaFile authority's own clause (debug_assert pins the mismatch in debug).
    let folder_authority = lease(&store, |conn| {
        UserScope::for_user("viewer".to_string())
            .resolve(conn, ScopeAlias::FolderAlias)
            .unwrap_or_else(|e| panic!("folder alias resolve: {e}"))
    });
    assert_eq!(folder_authority.alias(), ScopeAlias::FolderAlias);
    assert_eq!(
        folder_authority
            .rendered_clause()
            .map(|(table, _, _)| table)
            .as_deref(),
        Some("f"),
        "the authority remembers its own table, so a mismatch is detectable"
    );

    // The raw resolver keeps `granted` as its only extra datum; the clause field is
    // private and `into_user_scope` is the only exit.
    let resolved = lease(&store, |conn| {
        resolve_scope(conn, ScopeAlias::MediaFile, "viewer")
    })
    .unwrap_or_else(|e| panic!("resolve_scope: {e}"));
    assert_eq!(resolved.granted, 1);
    assert_eq!(resolved.alias, ScopeAlias::MediaFile);
    let authority = resolved.into_user_scope("viewer".to_string());
    assert_eq!(authority.decision(), ScopeDecision::Denied);
    assert_eq!(authority.user_id(), "viewer");

    // get_visible_media stays `user_id: &str` and never returns clause text: its
    // error type has no SQL payload arm beyond the sqlite error itself.
    let denied = lease(&store, |conn| {
        get_visible_media(conn, "mf2", "viewer", false)
    });
    assert!(matches!(denied, Err(VisibleMediaError::NotFound)));

    std::fs::remove_file(path).ok();
}
