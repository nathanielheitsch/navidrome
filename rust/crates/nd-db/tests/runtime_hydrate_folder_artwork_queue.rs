//! S-D3g2 — EXECUTED read sides for `folder_repo` and `artwork_queue_repo`:
//! statements from the builders run against a real SQLite DB built by
//! `nd_dbschema`'s fresh schema, rows hydrated through the shared seam.
//!
//! Go truth under test: `folderRepository`
//! (`persistence/folder_repository.go:80-132`, `:231-247`, `:276-299`) and
//! `artworkQueueRepository` (`persistence/artwork_queue_repository.go:35-45`,
//! `:108-127`, `:198-231`). Go ground truth for every expectation here was
//! captured by running the pinned Go repository itself
//! (`go run -tags "netgo sqlite_fts5" .` in `/tmp/sd3g2_oracle9`, saved output
//! `/tmp/g2_folder_queue_out.txt`, saved trace
//! `/tmp/g2_folder_queue_trace.txt`; the parenthesization proof is
//! `/tmp/sd3g2_paren` → `/tmp/g2_paren_proof.txt`).
//!
//! Three things this file is really about, each asserted against live rows
//! rather than a rendered string: (1) the `applyLibraryFilter` leg actually
//! restricts the executed row set (admin vs restricted user vs headless);
//! (2) `GetFolderUpdateInfo`'s map is keyed by FOLDER ID and a bad
//! `image_files` cell aborts the whole map; (3) `source_filter_sql`'s
//! parenthesization fix changes the executed row set, before vs after, over one
//! identical fixture.

use nd_db::artwork_queue_repo::{self};
use nd_db::folder_repo::{self, FolderRepo};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sqlrepo::Value;
use nd_db::store::SQLStore;
use nd_dbschema::schema::apply_fresh_schema;

const T0: &str = "2024-01-01 00:00:00+00:00";
const T1: &str = "2024-02-02 00:00:00+00:00";
const T2: &str = "2023-01-01 00:00:00+00:00";

/// One isolated on-disk DB per invocation: a unique temp path, freshly
/// migrated, pooled so `SEEDEDRAND`/`NATSORT` are registered per connection
/// (`schema-contract.md` §4). Every test ends with `remove_file`.
fn fresh_store() -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        "nd-db-folder-queue-",
        ".db",
    ));
    if path.exists() {
        std::fs::remove_file(&path)
            .unwrap_or_else(|e| panic!("cannot clear stale {:?}: {e}", path));
    }
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let pool = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    apply_fresh_schema(pool.checkout().unwrap().conn()).unwrap();
    (SQLStore::new(pool), path)
}

/// Run one closure on a pooled lease. Lease failures panic; the closure's own
/// `Result` is handed back untouched for assertion.
fn lease<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("store lease failed: {e}"))
}

/// Execute a raw INSERT/DELETE with typed args; failures panic with the statement.
fn insert(store: &SQLStore, sql: &str, args: &[rusqlite::types::Value]) -> usize {
    store
        .with_connection_sync(|conn| conn.execute(sql, rusqlite::params_from_iter(args.iter())))
        .unwrap_or_else(|e| panic!("{sql} --{args:?}--: {e}"))
}

fn v(s: &str) -> rusqlite::types::Value {
    rusqlite::types::Value::Text(s.to_string())
}

/// [`v`] over an owned string.
fn vs(s: String) -> rusqlite::types::Value {
    rusqlite::types::Value::Text(s)
}

fn i64(i: i64) -> rusqlite::types::Value {
    rusqlite::types::Value::Integer(i)
}

/// Library 1 (`/music-A`) plus a second library, so the library filter has
/// something to exclude.
fn seed_libraries(store: &SQLStore) {
    insert(
        store,
        "INSERT INTO library (id, name, path, remote_path, last_scan_at) VALUES (?, ?, ?, ?, ?)",
        &[i64(1), v("Alpha"), v("/music-A"), v(""), v(T0)],
    );
    insert(
        store,
        "INSERT INTO library (id, name, path, remote_path, last_scan_at) VALUES (?, ?, ?, ?, ?)",
        &[i64(2), v("Beta"), v("/music-B"), v(""), v(T0)],
    );
}

/// Two users: `adm` sees every library (`applyLibraryFilter` skips the clause,
/// `sql_base_repository.go:262-270`), `lim` only library 1, and a headless read
/// has no user at all.
fn seed_users(store: &SQLStore) {
    for (id, is_admin) in [("adm", 1), ("lim", 0)] {
        insert(
            store,
            "INSERT INTO user (id, user_name, name, email, password, is_admin, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            &[v(id), v(id), v(id), vs(format!("{id}@x.y")), v("C"), i64(is_admin), v(T0), v(T0)],
        );
    }
    insert(
        store,
        "INSERT INTO user_library (user_id, library_id) VALUES (?, ?)",
        &[v("adm"), i64(1)],
    );
    insert(
        store,
        "INSERT INTO user_library (user_id, library_id) VALUES (?, ?)",
        &[v("lim"), i64(1)],
    );
}

/// A folder row written the way Go's `Put` writes one. `spec` bundles the three
/// counters so the helper stays under clippy's argument limit.
#[derive(Clone, Copy)]
struct FolderSpec {
    missing: bool,
    num_audio_files: i64,
    num_playlists: i64,
}

/// A present folder with the given counters.
fn present(counters: i64, playlists: i64) -> FolderSpec {
    FolderSpec {
        missing: false,
        num_audio_files: counters,
        num_playlists: playlists,
    }
}

/// A missing folder with the given counters (the `missing = false` arms of
/// `getFolderUpdateInfo`/`HasAudioOutsideFolders` must exclude it).
fn missing(audio: i64, playlists: i64) -> FolderSpec {
    FolderSpec {
        missing: true,
        num_audio_files: audio,
        num_playlists: playlists,
    }
}

/// Eight args because a folder row genuinely has these inputs; the port's write
/// record has the same shape (`FolderRecord`).
#[allow(clippy::too_many_arguments)] // mirrors the 13-column folder row
fn seed_folder(
    store: &SQLStore,
    id: &str,
    lib_id: i64,
    path: &str,
    name: &str,
    spec: FolderSpec,
    updated_at: &str,
    image_files: &str,
) {
    insert(
        store,
        "INSERT INTO folder (id, library_id, path, name, parent_id, missing, num_audio_files, \
         num_playlists, image_files, images_updated_at, hash, updated_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            v(id),
            i64(lib_id),
            v(path),
            v(name),
            v(""),
            i64(i64::from(spec.missing)),
            i64(spec.num_audio_files),
            i64(spec.num_playlists),
            v(image_files),
            v(T1),
            vs(format!("H-{id}")),
            v(updated_at),
            v(T0),
        ],
    );
}

/// The library filter for `lim` (restricted) — `None` is Go's admin/headless skip.
fn lim_filter() -> Option<folder_repo::Sql> {
    folder_repo::library_filter(Some("lim"))
}

#[test]
fn get_executes_and_reports_not_found() {
    let (store, path) = fresh_store();
    seed_libraries(&store);
    seed_users(&store);
    // lib 1 root + a lib 2 folder. Folder ids are FolderID hashes so a raw id
    // would be meaningless; use the repo's own id function (Go model.FolderID).
    let root = folder_repo::folder_id(1, "/music-A", "/");
    let other = folder_repo::folder_id(2, "/music-B", "/");
    seed_folder(&store, &root, 1, "/", "music-A", present(3, 1), T1, "[]");
    seed_folder(
        &store,
        &other,
        2,
        "/",
        "music-B",
        present(5, 0),
        T1,
        r#"["cover.jpg"]"#,
    );

    let record = lease(&store, |conn| FolderRepo::new(conn).get(&root, None)).expect("get hits");
    assert_eq!(record.id, root);
    assert_eq!(record.library_id, 1);
    // library_path is join-only (`structs:"-"`) and arrives from the JOIN.
    assert_eq!(record.library_path, "/music-A");
    assert_eq!(record.image_files, Some(vec![]));
    // Go's UpdateAt is NOT hydrated: ScanStruct maps it to `update_at`, which no
    // SELECT emits (hydrate.rs documents the asymmetry; executed here).
    assert_eq!(record.updated_at, nd_db::hydrate::go_zero_time());

    // The lib-2 folder carries its own decoded image_files.
    let other_record = lease(&store, |conn| FolderRepo::new(conn).get(&other, None)).expect("hit");
    assert_eq!(
        other_record.image_files,
        Some(vec!["cover.jpg".to_string()]),
        "PostScan decodes the JSON column"
    );

    // A restricted user's library filter EXCLUDES the lib-2 row (executed Go gave
    // `data not found` for the same shape).
    let miss = lease(&store, |conn| {
        FolderRepo::new(conn).get_by_path(2, "/music-B", "/", lim_filter())
    });
    assert!(matches!(
        miss,
        Err(nd_db::folder_repo::FolderError::NotFound)
    ));
    // …and the same id is readable without the filter.
    assert!(lease(&store, |conn| FolderRepo::new(conn).get(&other, None)).is_ok());

    std::fs::remove_file(&path).ok();
}

#[test]
fn get_by_path_hashes_the_id_like_go() {
    let (store, path) = fresh_store();
    seed_libraries(&store);
    seed_users(&store);
    // Go: NewFolder(lib, "/music-A/rock").ID = FolderID(lib, "music-A/rock"),
    // i.e. the path is library-relative after the lib path is trimmed.
    let id = folder_repo::folder_id(1, "/music-A", "/music-A/rock");
    seed_folder(&store, &id, 1, "music-A", "rock", present(3, 0), T1, "[]");

    let record = lease(&store, |conn| {
        FolderRepo::new(conn).get_by_path(1, "/music-A", "/music-A/rock", None)
    })
    .expect("GetByPath hits by the hashed id");
    assert_eq!(record.id, id);
    assert_eq!(record.path, "music-A");
    assert_eq!(record.name, "rock");

    // A trailing slash on the library-rooted form cleans to the same id. Go's
    // executed FolderID (oracle /tmp/sd3g2_oracle11, saved
    // /tmp/g2_folderid_proof.txt): "/music-A/rock" and "/music-A/rock/" both give
    // 15ABnojgvDJ2bhfDjr0Ixl, while the UN-prefixed "music-A/rock" gives a
    // different hash (6OVmIcc60oxbHUjwZE3MVM) because TrimPrefix does not match —
    // so only the rooted form is asserted here.
    let same = lease(&store, |conn| {
        FolderRepo::new(conn).get_by_path(1, "/music-A", "/music-A/rock/", None)
    })
    .expect("the cleaned path resolves the same folder");
    assert_eq!(same.id, id);

    // No such folder: Go's queryOne → model.ErrNotFound (executed, oracle
    // /tmp/sd3g2_oracle9: `GetByPath err=data not found`).
    let miss = lease(&store, |conn| {
        FolderRepo::new(conn).get_by_path(1, "/music-A", "/music-A/nope", None)
    });
    assert!(matches!(
        miss,
        Err(nd_db::folder_repo::FolderError::NotFound)
    ));

    std::fs::remove_file(&path).ok();
}

/// The folder fixture the count/list tests share: two libraries, one missing
/// row, and a `num_audio_files` spread so a sort + page is observable.
fn seed_folder_page(store: &SQLStore) {
    // lib 1: root (audio 3), rock (audio 1), gone (missing, audio 9).
    seed_folder(store, "l1-a", 1, "/", "music-A", present(3, 1), T1, "[]");
    seed_folder(store, "l1-b", 1, "music-A", "rock", present(1, 0), T1, "[]");
    seed_folder(store, "l1-m", 1, "music-A", "gone", missing(9, 0), T1, "[]");
    // lib 2: one folder, so the library filter has a row to exclude.
    seed_folder(store, "l2-a", 2, "/", "music-B", present(5, 0), T1, "[]");
}

#[test]
fn get_all_applies_library_filter_filters_and_page() {
    let (store, path) = fresh_store();
    seed_libraries(&store);
    seed_users(&store);
    seed_folder_page(&store);

    // Admin, no options: all four rows including the missing one (Go applies no
    // missing guard here).
    let all = lease(&store, |conn| FolderRepo::new(conn).get_all(None, None)).expect("admin");
    assert_eq!(all.len(), 4);

    // Restricted user: library filter drops lib 2 (executed Go: 2 rows).
    let scoped = lease(&store, |conn| {
        FolderRepo::new(conn).get_all(lim_filter(), None)
    })
    .expect("restricted user");
    assert_eq!(
        scoped.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
        ["l1-a", "l1-b", "l1-m"]
    );

    // Filter `missing = false` + camelCase sort + page, exactly the executed Go
    // case: Sort "numAudioFiles" desc, Max 2 → the two non-missing lib-1 rows
    // ordered by descending audio count.
    let options = nd_db::sql_restful::QueryOptions {
        sort: "numAudioFiles".to_string(),
        order: "desc".to_string(),
        max: 2,
        filters: Some(vec![nd_db::sql_restful::Filter::Eq {
            column: "missing".to_string(),
            value: Value::Bool(false),
        }]),
        ..nd_db::sql_restful::QueryOptions::default()
    };
    let page = lease(&store, |conn| {
        FolderRepo::new(conn).get_all(lim_filter(), Some(&options))
    })
    .expect("filtered page");
    assert_eq!(
        page.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
        ["l1-a", "l1-b"],
        "sorted by num_audio_files desc, missing row excluded, page capped"
    );

    // The rendered leg names the snake_cased column and binds the filter value
    // BEFORE the library filter's user id (Go's selectFolder order).
    let (sql, args) = folder_repo::get_all_query_sql(lim_filter(), Some(&options)).expect("sql");
    assert_eq!(
        sql,
        "SELECT folder.*, library.path as library_path FROM folder \
         INNER JOIN library ON library.id = folder.library_id \
         WHERE missing = ? AND folder.library_id IN (SELECT ul.library_id FROM user_library ul \
         WHERE ul.user_id = ?) ORDER BY num_audio_files desc LIMIT 2"
    );
    assert_eq!(
        args,
        vec![Value::Bool(false), Value::Text("lim".to_string())]
    );

    std::fs::remove_file(&path).ok();
}

#[test]
fn count_all_rewrites_projection_and_counts_rows() {
    let (store, path) = fresh_store();
    seed_libraries(&store);
    seed_users(&store);
    seed_folder_page(&store);

    // Executed Go (oracle /tmp/sd3g2_oracle9): CountAll(adm)=3? No — Go counted 3
    // because its fixture had 3 non-missing rows; here the fixture has 4 rows and
    // CountAll has no missing guard, so 4 / 3 for the restricted user.
    let admin = lease(&store, |conn| FolderRepo::new(conn).count_all(None, None)).expect("admin");
    assert_eq!(admin, 4);
    let scoped = lease(&store, |conn| {
        FolderRepo::new(conn).count_all(lim_filter(), None)
    })
    .expect("restricted");
    assert_eq!(scoped, 3);

    // Filters survive, Max/Offset are dropped by `count`, and a caller sort leg
    // survives with folder.id appended (executed Go: CountAll(lim, options)=1).
    let options = nd_db::sql_restful::QueryOptions {
        sort: "name".to_string(),
        order: "desc".to_string(),
        max: 1,
        offset: 2,
        filters: Some(vec![nd_db::sql_restful::Filter::Eq {
            column: "missing".to_string(),
            value: Value::Bool(false),
        }]),
        ..nd_db::sql_restful::QueryOptions::default()
    };
    let counted = lease(&store, |conn| {
        FolderRepo::new(conn).count_all(lim_filter(), Some(&options))
    })
    .expect("filtered count");
    assert_eq!(counted, 2, "page bounds must not change the count");

    let (sql, args) = folder_repo::count_all_query_sql(lim_filter(), Some(&options)).expect("sql");
    assert_eq!(
        sql,
        "SELECT count(distinct folder.id) as count FROM folder \
         WHERE missing = ? AND folder.library_id IN (SELECT ul.library_id FROM user_library ul \
         WHERE ul.user_id = ?) ORDER BY name desc, folder.id"
    );
    assert_eq!(
        args,
        vec![Value::Bool(false), Value::Text("lim".to_string())]
    );

    std::fs::remove_file(&path).ok();
}

#[test]
fn get_folder_update_info_keys_by_folder_id_and_skips_missing() {
    let (store, path) = fresh_store();
    seed_libraries(&store);
    seed_users(&store);
    // Whole-library shape: root (updated after the scan), rock, and a MISSING
    // row that the `missing = false` arm must drop.
    seed_folder(
        &store,
        "l1-a",
        1,
        "/",
        "music-A",
        present(3, 0),
        T1,
        r#"["f.jpg"]"#,
    );
    seed_folder(&store, "l1-b", 1, "music-A", "rock", present(1, 0), T2, "");
    seed_folder(
        &store,
        "l1-m",
        1,
        "music-A",
        "gone",
        missing(9, 0),
        T1,
        "[]",
    );
    // A row in another library: `Eq{"library_id": lib.ID}` scopes the read to the
    // requested library, so it is absent — but note this read applies NO
    // user-access filter (`queryFolderUpdateInfo`, folder_repository.go:179, never
    // calls applyLibraryFilter), so a caller asking for library 2 would get its
    // rows even as a restricted user. Asserted separately below.
    seed_folder(&store, "l2-a", 2, "/", "music-B", present(5, 0), T1, "[]");

    let map = lease(&store, |conn| {
        FolderRepo::new(conn).get_folder_update_info(1, &[])
    })
    .expect("whole-library update info");

    // KEY SEMANTICS: keyed by folder ID, not by path
    // (`m[f.ID] = info`, folder_repository.go:199) — the brief's "keyed by path"
    // is wrong, and the executed row set proves it.
    assert_eq!(
        map.keys().collect::<Vec<_>>(),
        ["l1-a", "l1-b"],
        "id-keyed, missing row excluded, other library out of lib scope"
    );
    assert_eq!(
        map["l1-a"].image_files,
        Some(vec!["f.jpg".to_string()]),
        "decoded after the page is scanned"
    );
    // The empty shim is Go's skip branch (`:193`): nil slice, no decode at all.
    assert_eq!(map["l1-b"].image_files, None);
    assert_eq!(map["l1-b"].updated_at.to_rfc3339(), T2.replace(' ', "T"));
    assert_eq!(map["l1-a"].hash, "H-l1-a");

    // The whole-library shortcut also fires for "" and "." (Go :111-116), and a
    // batch read narrows to the target subtree.
    let dotted = lease(&store, |conn| {
        FolderRepo::new(conn).get_folder_update_info(1, &[".".to_string()])
    })
    .expect("root shortcut");
    assert_eq!(dotted, map);

    // A restricted caller asking for library 2 still gets lib 2's rows: no
    // applyLibraryFilter on this path (executed Go, oracle /tmp/sd3g2_oracle8).
    let other = lease(&store, |conn| {
        FolderRepo::new(conn).get_folder_update_info(2, &[])
    })
    .expect("no user-access filter on this read");
    assert_eq!(other.keys().collect::<Vec<_>>(), ["l2-a"]);

    // Batch arm: the target folder is matched by its FolderID HASH, and its
    // descendants by `path = target` / `path LIKE target/%` — Folder.Path is the
    // PARENT directory, so the target itself (path "music-A", name "rock") is
    // only reachable through the id arm.
    // The batch arm hashes the target's id; the key is Go's FolderID hash over
    // "1:music-A/rock" (BTreeMap order puts the hash first).
    let target = folder_repo::folder_id(1, "", "music-A/rock");
    seed_folder(
        &store,
        &target,
        1,
        "music-A",
        "rock",
        present(2, 0),
        T2,
        "[]",
    );
    seed_folder(
        &store,
        "deep",
        1,
        "music-A/rock",
        "deep",
        present(1, 0),
        T2,
        "[]",
    );
    seed_folder(
        &store,
        "dead",
        1,
        "music-A/rock",
        "dead",
        missing(1, 0),
        T2,
        "[]",
    );
    let batch = lease(&store, |conn| {
        FolderRepo::new(conn).get_folder_update_info(1, &["music-A/rock".to_string()])
    })
    .expect("batch read");
    assert_eq!(
        batch.keys().collect::<Vec<_>>(),
        [target.as_str(), "deep"],
        "id arm + descendant arm, missing row excluded"
    );

    // A wildcard in a target path is NOT escaped by Go's batch arm (Like{"path":
    // cleanPath + "/%"}, folder_repository.go:164), so `a%b` matches any subtree
    // starting with `a`. That divergence from escape_like_prefix is pinned here
    // with rows, and documented in hydrate-contract.md SS-D3g2.
    seed_folder(
        &store,
        "wild",
        1,
        "music-A/a%b",
        "sub",
        present(1, 0),
        T2,
        "[]",
    );
    let wildcard = lease(&store, |conn| {
        FolderRepo::new(conn).get_folder_update_info(1, &["music-A/a%b".to_string()])
    })
    .expect("wildcard target");
    assert!(wildcard.contains_key("wild"), "Go's unescaped LIKE matches");

    std::fs::remove_file(&path).ok();
}

#[test]
fn update_info_decode_failure_aborts_the_whole_map() {
    let (store, path) = fresh_store();
    seed_libraries(&store);
    seed_users(&store);
    seed_folder(&store, "l1-a", 1, "/", "music-A", present(3, 0), T1, "[]");
    seed_folder(
        &store,
        "l1-b",
        1,
        "music-A",
        "rock",
        present(1, 0),
        T1,
        "not-json",
    );

    let err = lease(&store, |conn| {
        FolderRepo::new(conn).get_folder_update_info(1, &[])
    })
    .expect_err("a bad cell aborts the map (Go returns nil, err at :196-197)");
    let text = err.to_string();
    assert!(
        text.contains("parsing folder image_files:"),
        "Go's update-info wrapper, not PostScan's prefix: {text}"
    );
    assert!(
        !text.contains("parsing folder image files from db:"),
        "the bulk-read prefix belongs to dbFolder.PostScan only: {text}"
    );

    std::fs::remove_file(&path).ok();
}

#[test]
fn has_audio_outside_folders_counts_like_go_exists() {
    let (store, path) = fresh_store();
    seed_libraries(&store);
    seed_users(&store);
    // Parent subtree `music-A/rock`: the parent itself has no audio, one child
    // does, and a missing child has audio but is excluded.
    seed_folder(
        &store,
        "parent",
        1,
        "music-A",
        "rock",
        present(0, 0),
        T1,
        "[]",
    );
    seed_folder(
        &store,
        "child",
        1,
        "music-A/rock",
        "live",
        present(1, 0),
        T1,
        "[]",
    );
    seed_folder(
        &store,
        "gone",
        1,
        "music-A/rock",
        "dead",
        missing(4, 0),
        T1,
        "[]",
    );

    // No exclusions: Go's NotEq with a nil slice renders squirrel's sqlTrue arm
    // `(1=1)`, so the subtree's audio row counts. Executed Go: true.
    let ok = lease(&store, |conn| {
        FolderRepo::new(conn).has_audio_outside_folders(1, "music-A/rock", &[])
    })
    .expect("nil-exclusion probe");
    assert!(ok, "the (1=1) arm excludes nothing, so audio is found");

    // Excluding the only audio row flips it to false (executed Go: false).
    let ok = lease(&store, |conn| {
        FolderRepo::new(conn).has_audio_outside_folders(1, "music-A/rock", &["child".to_string()])
    })
    .expect("excluded probe");
    assert!(
        !ok,
        "the sole audio row is excluded, the missing row never counted"
    );

    // Excluding the parent only still finds the child (executed Go: true).
    let ok = lease(&store, |conn| {
        FolderRepo::new(conn).has_audio_outside_folders(1, "music-A/rock", &["parent".to_string()])
    })
    .expect("parent-only exclusion");
    assert!(ok);

    // Another library's subtree is out of scope.
    let ok = lease(&store, |conn| {
        FolderRepo::new(conn).has_audio_outside_folders(2, "music-B", &[])
    })
    .expect("empty library");
    assert!(!ok);

    std::fs::remove_file(&path).ok();
}

/// Library 1's `last_scan_at` is `T0`; a folder with `updated_at` > that is
/// "touched". `library_path` is NOT NULL in the DDL, so both rows join.
fn seed_cursor(store: &SQLStore) {
    seed_folder(store, "touched", 1, "/", "music-A", present(3, 1), T1, "[]");
    seed_folder(
        store,
        "stale",
        1,
        "/",
        "music-A-old",
        present(3, 4),
        T2,
        "[]",
    );
    seed_folder(
        store,
        "no-playlists",
        1,
        "music-A",
        "rock",
        present(3, 0),
        T1,
        "[]",
    );
    seed_folder(
        store,
        "missing-playlists",
        1,
        "music-A",
        "gone",
        missing(3, 2),
        T1,
        "[]",
    );
    // A lib-2 folder that satisfies every predicate, for the filter test.
    seed_folder(store, "lib2", 2, "/", "music-B", present(5, 2), T1, "[]");
}

/// Sort ids for comparison: Go's cursor statements carry NO ORDER BY (neither
/// `GetTouchedWithPlaylists` nor `GetAllWithPlaylists` calls OrderBy), so row
/// order is SQLite's rowid order and is not part of the contract.
fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

#[test]
fn cursor_drain_yields_rows_and_stops_at_the_first_failure() {
    let (store, path) = fresh_store();
    seed_libraries(&store);
    seed_users(&store);
    seed_cursor(&store);

    // GetAllWithPlaylists: non-missing AND num_playlists > 0. `stale` qualifies
    // even though its update predates the scan (that leg belongs to the touched
    // cursor only). Executed Go (oracle /tmp/sd3g2_oracle9) yielded the same set.
    let drain = lease(&store, |conn| {
        FolderRepo::new(conn).drain_all_with_playlists(None)
    })
    .expect("statement prepares");
    assert!(drain.error.is_none(), "clean drain: {:?}", drain.error);
    let ids = sorted(drain.folders.iter().map(|f| f.id.clone()).collect());
    assert_eq!(ids, ["lib2", "stale", "touched"]);

    // GetTouchedWithPlaylists adds `folder.updated_at > library.last_scan_at`,
    // which drops `stale` (updated T2 < scan T0? no — T2 predates T0).
    let drain = lease(&store, |conn| {
        FolderRepo::new(conn).drain_touched_with_playlists(None)
    })
    .expect("statement prepares");
    assert!(drain.error.is_none(), "clean drain: {:?}", drain.error);
    let ids = sorted(drain.folders.iter().map(|f| f.id.clone()).collect());
    assert_eq!(ids, ["lib2", "touched"]);

    // The library filter restricts the drained set for a restricted user.
    let drain = lease(&store, |conn| {
        FolderRepo::new(conn).drain_all_with_playlists(lim_filter())
    })
    .expect("filtered cursor");
    assert_eq!(
        sorted(drain.folders.iter().map(|f| f.id.clone()).collect()),
        ["stale", "touched"]
    );

    // A bad image_files cell aborts the drain: Go's cursor yields the rows before
    // it and then the PostScan error, and stops (`sql_base_repository.go:427-433`).
    seed_folder(
        &store,
        "broken",
        1,
        "music-A",
        "bad",
        present(1, 1),
        T1,
        "{oops",
    );
    let drain = lease(&store, |conn| {
        FolderRepo::new(conn).drain_all_with_playlists(None)
    })
    .expect("statement prepares");
    let err = drain.error.expect("PostScan failure ends the drain");
    assert!(
        err.to_string()
            .contains(nd_db::hydrate::FOLDER_IMAGE_FILES_PREFIX),
        "Go's PostScan wrapper: {err}"
    );
    assert!(
        drain.folders.iter().all(|f| f.id != "broken"),
        "the failing row is never yielded"
    );

    std::fs::remove_file(&path).ok();
}

/// The `item_artwork` fixture for the parenthesization proof, identical to the Go
/// oracle's (`/tmp/sd3g2_paren`, saved `/tmp/g2_paren_proof.txt`): five rows across
/// kinds `al`/`mf`/`ra`, split so SQL precedence decides the answer.
fn seed_item_artwork(store: &SQLStore) {
    let row = |kind: &str, id: &str, hash: &str, source: &str, last_failure: &str| {
        insert(
            store,
            "INSERT INTO item_artwork (item_kind, item_id, image_type, hash, source, last_failure) \
             VALUES (?, ?, 'primary', ?, ?, ?)",
            &[v(kind), v(id), v(hash), v(source), v(last_failure)],
        )
    };
    row("mf", "m1", "", "", r#"["boom"]"#);
    row("mf", "m2", "h", "embedded", "[]");
    row("al", "a1", "", "cover", r#"["boom"]"#);
    row("al", "a2", "h", "cover", "[]");
    row("ra", "r1", "", "", r#"["boom"]"#);
}

fn sources(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn count_by_source_counts_only_the_requested_kind() {
    let (store, path) = fresh_store();
    seed_item_artwork(&store);

    // Executed Go values, one identical fixture (/tmp/g2_paren_proof.txt).
    let counts = lease(&store, |conn| {
        let repo = artwork_queue_repo::ArtworkQueueRepo::new(conn);
        [
            repo.count_by_source("al", &sources(&[])).unwrap(),
            repo.count_by_source("al", &sources(&["cover"])).unwrap(),
            repo.count_by_source("al", &sources(&["!failed"])).unwrap(),
            repo.count_by_source("al", &sources(&["cover", "!failed"]))
                .unwrap(),
            repo.count_by_source("mf", &sources(&["cover", "!failed"]))
                .unwrap(),
            repo.count_by_source("ra", &sources(&["cover", "!failed"]))
                .unwrap(),
            repo.count_by_source("al", &sources(&["embedded", "cover"]))
                .unwrap(),
        ]
    });
    assert_eq!(counts, [2, 2, 1, 2, 1, 1, 2]);

    // The wrapped text, matching Go's executed squirrel render
    // (/tmp/g2_source_filter_render.txt) and the executed repository trace
    // (/tmp/g2_folder_queue_trace.txt).
    let (sql, args) =
        artwork_queue_repo::count_by_source_sql("al", &sources(&["cover", "!failed"]));
    assert_eq!(
        sql,
        "SELECT count(*) as count FROM item_artwork \
         WHERE (item_kind = ? AND (source IN (?) OR (hash = ? AND last_failure <> ?)))"
    );
    assert_eq!(
        args,
        vec![
            Value::Text("al".to_string()),
            Value::Text("cover".to_string()),
            Value::Text(String::new()),
            Value::Text(String::new()),
        ]
    );

    // The BEFORE row set, executed over the same data: the flat form counted rows
    // of other kinds. Rendered and run directly so the delta is a measured fact.
    let flat = lease(&store, |conn| {
        nd_db::execution::query_optional(
            conn,
            &(
                "SELECT count(*) as count FROM item_artwork \
                 WHERE item_kind = ? AND (source IN (?)) OR (hash = ? AND last_failure <> ?)"
                    .to_string(),
                vec![
                    Value::Text("al".to_string()),
                    Value::Text("cover".to_string()),
                    Value::Text(String::new()),
                    Value::Text(String::new()),
                ],
            ),
            |row| row.get::<_, i64>("count"),
        )
        .unwrap()
        .unwrap_or_default()
    });
    assert_eq!(flat, 4, "the pre-fix flat form leaked kinds al/mf/ra");

    std::fs::remove_file(&path).ok();
}

#[test]
fn sources_in_use_and_queue_reads_match_go() {
    let (store, path) = fresh_store();
    seed_item_artwork(&store);
    let at = |h: u32, m: u32| -> chrono::DateTime<chrono::Utc> {
        use chrono::TimeZone;
        chrono::Utc.from_utc_datetime(
            &chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
                .unwrap()
                .and_hms_opt(h, m, 0)
                .unwrap(),
        )
    };

    // SourcesInUse: distinct source for the kind. Executed Go returned [cover]
    // for `al` (/tmp/g2_folder_queue_out.txt).
    let mut found = lease(&store, |conn| {
        artwork_queue_repo::ArtworkQueueRepo::new(conn).sources_in_use("al")
    })
    .expect("sources in use");
    found.sort();
    assert_eq!(found, ["cover".to_string()]);

    // Queue rows across kinds and priorities, mirroring the Go oracle's
    // CountQueued fixture ([{al 10 1} {al 0 1}] and [{al 0 1} {ra 0 1}]).
    for (kind, id, priority, minute) in [
        ("mf", "m1", 5, 0),
        ("al", "a1", 0, 1),
        ("al", "a2", 10, 2),
        ("ra", "r1", 0, 3),
    ] {
        insert(
            &store,
            "INSERT INTO artwork_queue (item_kind, item_id, image_type, priority, attempts, \
             retry_at, enqueued_at, trace) VALUES (?, ?, 'primary', ?, 0, ?, ?, '[]')",
            &[
                v(kind),
                v(id),
                i64(priority),
                vs(nd_db::time::to_text(&at(0, minute))),
                vs(nd_db::time::to_text(&at(0, minute))),
            ],
        );
    }

    let stats = lease(&store, |conn| {
        artwork_queue_repo::ArtworkQueueRepo::new(conn).count_queued(&sources(&[]), &[])
    })
    .expect("unfiltered group");
    assert_eq!(
        stats,
        [
            artwork_queue_repo::ArtworkQueueStat {
                item_kind: "al".into(),
                priority: 10,
                count: 1
            },
            artwork_queue_repo::ArtworkQueueStat {
                item_kind: "al".into(),
                priority: 0,
                count: 1
            },
            artwork_queue_repo::ArtworkQueueStat {
                item_kind: "mf".into(),
                priority: 5,
                count: 1
            },
            artwork_queue_repo::ArtworkQueueStat {
                item_kind: "ra".into(),
                priority: 0,
                count: 1
            },
        ],
        "ORDER BY item_kind, priority desc"
    );

    // Kind filter (Go: [{al 10 1} {al 0 1}]) and priority filter
    // (Go: [{al 0 1} {ra 0 1}]).
    let kinds = lease(&store, |conn| {
        artwork_queue_repo::ArtworkQueueRepo::new(conn)
            .count_queued(&sources(&["al"]), &[])
            .unwrap()
    });
    assert_eq!(kinds.len(), 2);
    let prios = lease(&store, |conn| {
        artwork_queue_repo::ArtworkQueueRepo::new(conn)
            .count_queued(&sources(&[]), &[0])
            .unwrap()
    });
    assert_eq!(
        prios
            .iter()
            .map(|s| (s.item_kind.clone(), s.priority))
            .collect::<Vec<_>>(),
        [("al".to_string(), 0), ("ra".to_string(), 0)]
    );

    // Count over the whole queue (Go: 4).
    assert_eq!(
        lease(&store, |conn| artwork_queue_repo::ArtworkQueueRepo::new(
            conn
        )
        .count())
        .unwrap(),
        4
    );

    // Get: the triple-hits row, then a miss → data not found (Go's exact values,
    // /tmp/g2_folder_queue_out.txt).
    //
    // TEXT divergence, recorded rather than changed: Go's executed trace is
    // `SELECT * FROM artwork_queue WHERE image_type = {:p0} AND item_id = {:p1}
    // AND item_kind = {:p2}` (`/tmp/g2_folder_queue_trace.txt`) — squirrel renders
    // an `Eq` map in sorted-key order, so `image_type` leads — while the port emits
    // declaration order. Text-only: a WHERE conjunction is order-independent and
    // both forms select the same row, executed below. Aligning it would reorder a
    // pre-existing builder's bound args for no behavioral gain, so it is left.
    let (get_stmt, get_args) = artwork_queue_repo::get_sql("al", "a2", "primary");
    assert_eq!(
        get_stmt,
        "SELECT * FROM artwork_queue WHERE item_kind = ? AND item_id = ? AND image_type = ?"
    );
    assert_eq!(
        get_args,
        vec![
            Value::Text("al".to_string()),
            Value::Text("a2".to_string()),
            Value::Text("primary".to_string()),
        ]
    );
    let item = lease(&store, |conn| {
        artwork_queue_repo::ArtworkQueueRepo::new(conn).get("al", "a2", "primary")
    })
    .expect("queue row");
    assert_eq!(item.item_kind, "al");
    assert_eq!(item.priority, 10);
    assert_eq!(item.attempts, 0);
    assert_eq!(item.retry_at, Some(at(0, 2)));
    assert_eq!(item.trace, "[]");
    let miss = lease(&store, |conn| {
        artwork_queue_repo::ArtworkQueueRepo::new(conn).get("al", "zzz", "primary")
    });
    assert!(matches!(
        miss,
        Err(artwork_queue_repo::ArtworkQueueError::NotFound)
    ));

    // The shared filter: CountQueued's preview equals the rows PurgeQueued
    // deletes, and the re-count is empty (Go's comment at :209).
    let preview = lease(&store, |conn| {
        artwork_queue_repo::ArtworkQueueRepo::new(conn)
            .count_queued(&sources(&["al"]), &[10])
            .unwrap()
    });
    let purged = lease(&store, |conn| {
        artwork_queue_repo::ArtworkQueueRepo::new(conn)
            .purge_queued(&sources(&["al"]), &[10])
            .unwrap()
    });
    assert_eq!(preview.iter().map(|s| s.count).sum::<i64>(), purged);
    let after = lease(&store, |conn| {
        artwork_queue_repo::ArtworkQueueRepo::new(conn)
            .count_queued(&sources(&["al"]), &[10])
            .unwrap()
    });
    assert!(after.is_empty());

    std::fs::remove_file(&path).ok();
}
