//! Port of `persistence/library_repository.go` (329L) — the `library`
//! table plus its `user_library` admin-assignment join.
//!
//! Every Go method builds a squirrel statement (or hands one to dbx's
//! `Model(l).Insert()`) and materializes it through `toSQL()`
//! (`sql_base_repository.go:358-372`) as `(sql, args)`; execution, `WithTx`
//! and logging stay with `store.rs`/the caller, so this port stops at the
//! same [`Sql`] pairs, following the sibling-lane split (see `album_repo.rs`).
//!
//! Ported here: `Get`'s id select (`:35-40`), `GetAll`/`CountAll`
//! (`:186-196`), `Put`'s update-then-insert legs plus the admin
//! user-library CROSS JOIN upsert (`:57-113`), `StoreMusicFolder`'s
//! config-change guard (`:117-133`), `AddArtist`'s library_artist insert
//! (`:135-145`), the scan lifecycle updates `ScanBegin`/`ScanEnd`/
//! `SetScannedPID`/`ScanInProgress` (`:147-181`), `RefreshStats`'s eight
//! aggregate probes and its totals update (`:183-240`), `Delete`'s row
//! delete (`:242-270`) and `GetUsersWithLibraryAccess`'s user-library join
//! (`:198-208`).
//!
//! Not ported, with the reason: the `libCache`/`libLock` process-local path
//! cache (`:28-32`, `GetPath` `:42-66`) is runtime state, not SQL; `Put`'s
//! `ErrValidation` guard on library 1's path and `Delete`'s admin/ID-1
//! checks (`:61-67`, `:244-249`) are auth-layer policy the caller applies
//! before issuing the statements here; `Delete`'s artist-orphan reconcile
//! and plugin-reference cleanup (`:252-266`) belong to the artist/plugin
//! lanes; the REST shims `Count`/`Read`/`ReadAll` (`:210-224`) are
//! `sql_restful.rs`'s options parsing.
//!
//! Timestamps: Go binds `time.Time` (go-sqlite3 writes datetime text). This
//! repository still binds epoch-second integers — a documented divergence from
//! [`crate::time`]'s TEXT convention, listed as remaining work there — with
//! `ZERO_TIME` as the zero `time.Time` constant `ScanEnd`/`ScanInProgress`
//! bind (`:164-170`, `:177-179`). Converting it is a separate lane: `library`
//! timestamps feed `artist`'s `RefreshStats` comparison
//! (`artist_repository.go:479`), so both sides must move together.
//!
//! Safety rule (`sqlrepo.rs`): identifiers (table/column names) are this
//! repository's registered constants; every value binds to a `?`.

use crate::sqlrepo::Value;

/// The `library` table (`NewLibraryRepository`, `library_repository.go:26`).
pub const TABLE: &str = "library";

/// The `user_library` join table (`Put`'s admin assignment, `:94-103`).
pub const USER_LIBRARY_TABLE: &str = "user_library";

/// The `library_artist` join table (`AddArtist`, `:135-140`).
pub const LIBRARY_ARTIST_TABLE: &str = "library_artist";

/// `model.DefaultLibraryID` (`model/library.go:68`): the row `Put` guards
/// and `StoreMusicFolder` rewrites.
pub const DEFAULT_LIBRARY_ID: i64 = 1;

/// Go's zero `time.Time` (`time.Time{}`) as the crate's epoch-seconds unit:
/// the stamp `ScanEnd` writes and `ScanInProgress` compares against.
pub const ZERO_TIME: i64 = 0;

/// One materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// `Get` (`:35-40`): `newSelect(ctx).Columns("*").Where(Eq{"id": id})`.
#[must_use]
pub fn get_sql(id: i64) -> Sql {
    (
        format!("SELECT * FROM {TABLE} WHERE id = ?"),
        vec![Value::Integer(id)],
    )
}

/// `GetAll` (`:186-191`): `newSelect(ctx, ops...).Columns("*")` with the
/// caller's order-by expressions (already direction-carrying, per
/// `buildSortOrder` output) and `applyOptions`' Max/Offset.
#[must_use]
pub fn get_all_sql(order_by: &[&str], max: i64, offset: i64) -> Sql {
    let mut sql = format!("SELECT * FROM {TABLE}");
    // squirrel binds nothing for LIMIT/OFFSET (`select.go:139-147`), and this
    // select has no other clauses, so the arg list stays empty.
    let args: Vec<Value> = Vec::new();
    if !order_by.is_empty() {
        sql.push_str(&format!(" ORDER BY {}", order_by.join(", ")));
    }
    if max > 0 {
        // squirrel renders the bound as a decimal literal and binds nothing
        // (`select.go:139-147`), so `max` joins neither clause nor args list.
        sql.push_str(&format!(" LIMIT {max}"));
    }
    if offset > 0 {
        // Deviation (deliberate, cited): squirrel emits a bare `OFFSET n`
        // (`applyOptions` sets the bounds independently,
        // `sql_base_repository.go:151-156`), which SQLite rejects — a parse
        // error. SQLite is Navidrome's primary engine, so the port emits
        // `LIMIT -1` (its unlimited bound) ahead of the offset.
        if max <= 0 {
            sql.push_str(" LIMIT -1");
        }
        sql.push_str(&format!(" OFFSET {offset}"));
    }
    (sql, args)
}

/// `CountAll` (`:193-196`) via the base `count()`
/// (`sql_base_repository.go:565-576`): `count(distinct library.id) as count`
/// with pagination and order dropped.
#[must_use]
pub fn count_all_sql() -> Sql {
    (
        format!("SELECT count(distinct {TABLE}.id) as count FROM {TABLE}"),
        Vec::new(),
    )
}

/// The `Put` update-map payload (`library_repository.go:76-82`): the six
/// columns `selectUpdateColumns` filters plus the `updated_at` Go always
/// appends (`:83`). `Put` also stamps `CreatedAt`/`UpdatedAt` and the
/// ID-1 path validation (`:59-71`) — those are the caller's clock/policy.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryUpdate<'a> {
    /// `library.name`.
    pub name: &'a str,
    /// `library.path`.
    pub path: &'a str,
    /// `library.remote_path`.
    pub remote_path: &'a str,
    /// `library.default_new_users`.
    pub default_new_users: bool,
    /// `library.pid_album`.
    pub pid_album: &'a str,
    /// `library.pid_track`.
    pub pid_track: &'a str,
    /// `library.updated_at` (`l.UpdatedAt = time.Now()`, `:74`).
    pub updated_at: i64,
}

impl LibraryUpdate<'_> {
    /// The `selectUpdateColumns` map (`:76-82`) plus `updated_at` (`:83`),
    /// restricted to `cols_to_update` (snake_cased by the caller) — all
    /// columns when none are requested. `updated_at` always survives.
    fn set_pairs(&self, cols_to_update: &[&str]) -> Vec<(&'static str, Value)> {
        let all: Vec<(&'static str, Value)> = vec![
            ("name", Value::from(self.name)),
            ("path", Value::from(self.path)),
            ("remote_path", Value::from(self.remote_path)),
            ("default_new_users", Value::Bool(self.default_new_users)),
            ("pid_album", Value::from(self.pid_album)),
            ("pid_track", Value::from(self.pid_track)),
            ("updated_at", Value::Integer(self.updated_at)),
        ];
        if cols_to_update.is_empty() {
            return all;
        }
        all.into_iter()
            .filter(|(col, _)| *col == "updated_at" || cols_to_update.contains(col))
            .collect()
    }
}

/// `Put`'s UPDATE leg (`:85-87`): `Update(library).SetMap(cols).Where(Eq{"id":
/// l.ID})` — squirrel's `SetMap` renders the SET pairs in alphabetical key
/// order (`squirrel@v1.5.4/update.go:237`). Only the requested columns set;
/// zero rows affected is the caller's insert fallback (`:96-101`).
#[must_use]
pub fn put_update_sql(id: i64, update: &LibraryUpdate, cols_to_update: &[&str]) -> Sql {
    let mut pairs = update.set_pairs(cols_to_update);
    pairs.sort_by_key(|(col, _)| *col);
    let set = pairs
        .iter()
        .map(|(col, _)| format!("{col} = ?"))
        .collect::<Vec<_>>()
        .join(", ");
    let args = pairs
        .iter()
        .map(|(_, value)| value.clone())
        .chain([Value::Integer(id)])
        .collect();
    (format!("UPDATE {TABLE} SET {set} WHERE id = ?"), args)
}

/// `Put`'s INSERT legs (`:78-79`, `:96-101` — `r.db.Model(l).Insert()`):
/// the row columns in `model.Library`'s declaration order (`model/library.go:15-36`,
/// the order dbx's reflection renders), with `id` bound first when the row
/// carries one (the ID-999 insert case, `library_repository_test.go:113-137`)
/// and omitted for the autoincrement path (`:77-79`).
pub const INSERT_COLUMNS: [&str; 21] = [
    "name",
    "path",
    "remote_path",
    "last_scan_at",
    "last_scan_started_at",
    "full_scan_in_progress",
    "updated_at",
    "created_at",
    "total_songs",
    "total_albums",
    "total_artists",
    "total_folders",
    "total_files",
    "total_missing_files",
    "total_size",
    "total_duration",
    "default_new_users",
    "pid_album",
    "pid_track",
    "scanned_pid_album",
    "scanned_pid_track",
];

/// The INSERT with `id` bound first when given; `values` pair one-to-one with
/// [`INSERT_COLUMNS`].
#[must_use]
pub fn insert_sql(id: Option<i64>, values: Vec<Value>) -> Sql {
    let mut columns: Vec<&str> = Vec::with_capacity(INSERT_COLUMNS.len() + 1);
    let mut args = Vec::with_capacity(values.len() + 1);
    if let Some(id) = id {
        columns.push("id");
        args.push(Value::Integer(id));
    }
    columns.extend(INSERT_COLUMNS);
    args.extend(values);
    let placeholders = vec!["?"; args.len()].join(", ");
    let cols = columns.join(", ");
    (
        format!("INSERT INTO {TABLE} ({cols}) VALUES ({placeholders})"),
        args,
    )
}

/// `Put`'s admin assignment (`:105-111`): every admin user gains every
/// library, idempotently. A `Sql` with no bound args — all identifiers;
/// `ON CONFLICT … DO NOTHING` keeps re-`Put`s from duplicating rows.
#[must_use]
pub fn assign_admins_sql() -> Sql {
    (
        "INSERT INTO user_library (user_id, library_id)\nSELECT u.id, l.id\nFROM user u\nCROSS JOIN library l\nWHERE u.is_admin = true\nON CONFLICT (user_id, library_id) DO NOTHING;".to_string(),
        Vec::new(),
    )
}

/// `StoreMusicFolder` (`:117-133`): re-point library 1 at the configured
/// music folder — `Set("path", …).Set("updated_at", …)` in call order, gated
/// by `NotEq{"path": …}` so an unchanged config writes nothing (the
/// `rowsAffected > 0` cache update is the caller's).
#[must_use]
pub fn store_music_folder_sql(path: &str, updated_at: i64) -> Sql {
    (
        format!(
            "UPDATE {TABLE} SET path = ?, updated_at = ? \
             WHERE id = ? AND path <> ?"
        ),
        vec![
            Value::from(path),
            Value::Integer(updated_at),
            Value::Integer(DEFAULT_LIBRARY_ID),
            Value::from(path),
        ],
    )
}

/// `AddArtist` (`:135-145`): the library↔artist link, idempotent on its
/// unique key.
#[must_use]
pub fn add_artist_sql(id: i64, artist_id: &str) -> Sql {
    (
        format!(
            "INSERT INTO {LIBRARY_ARTIST_TABLE} (library_id, artist_id) VALUES (?, ?) \
             on conflict(library_id, artist_id) do nothing"
        ),
        vec![Value::Integer(id), Value::from(artist_id)],
    )
}

/// `ScanBegin` (`:147-153`): stamp the scan start and its mode.
#[must_use]
pub fn scan_begin_sql(id: i64, now: i64, full_scan: bool) -> Sql {
    (
        format!(
            "UPDATE {TABLE} SET last_scan_started_at = ?, full_scan_in_progress = ? WHERE id = ?"
        ),
        vec![
            Value::Integer(now),
            Value::Bool(full_scan),
            Value::Integer(id),
        ],
    )
}

/// `ScanEnd` (`:155-168`): stamp the finish, clear the flag and reset the
/// start stamp to the zero time.
#[must_use]
pub fn scan_end_sql(id: i64, now: i64) -> Sql {
    (
        format!(
            "UPDATE {TABLE} SET last_scan_at = ?, full_scan_in_progress = ?, \
             last_scan_started_at = ? WHERE id = ?"
        ),
        vec![
            Value::Integer(now),
            Value::Bool(false),
            Value::Integer(ZERO_TIME),
            Value::Integer(id),
        ],
    )
}

/// `SetScannedPID` (`:170-178`): record the PID specs the finished scan used.
#[must_use]
pub fn set_scanned_pid_sql(id: i64, pid_album: &str, pid_track: &str) -> Sql {
    (
        format!("UPDATE {TABLE} SET scanned_pid_album = ?, scanned_pid_track = ? WHERE id = ?"),
        vec![
            Value::from(pid_album),
            Value::from(pid_track),
            Value::Integer(id),
        ],
    )
}

/// `ScanInProgress` (`:180-184`): `NotEq{"last_scan_started_at":
/// time.Time{}}` — the zero-time constant must match the one [`scan_end_sql`]
/// writes, or a finished scan would read as in-progress.
#[must_use]
pub fn scan_in_progress_sql() -> Sql {
    (
        format!("SELECT * FROM {TABLE} WHERE last_scan_started_at <> ?"),
        vec![Value::Integer(ZERO_TIME)],
    )
}

/// `RefreshStats` (`:183-240`): the eight aggregate probes, in
/// `run.Parallel`'s declaration order (`:199-236`). The `Eq` maps render
/// their two keys alphabetically (`library_id` < `missing`), and the folder
/// count's `And` nests the `Gt` clause last (`:221-225`). Each probe's row is
/// a scalar `count`/`sum` the caller sums into [`refresh_stats_update_sql`].
#[must_use]
pub fn refresh_stats_probes_sql(id: i64) -> [Sql; 8] {
    let lib = Value::Integer(id);
    [
        (
            "SELECT count(*) as count FROM media_file WHERE library_id = ? AND missing = ?".to_string(),
            vec![lib.clone(), Value::Bool(false)],
        ),
        (
            "SELECT count(*) as count FROM album WHERE library_id = ? AND missing = ?".to_string(),
            vec![lib.clone(), Value::Bool(false)],
        ),
        (
            "SELECT count(*) as count FROM library_artist la \
             INNER JOIN artist a ON la.artist_id = a.id \
             WHERE la.library_id = ? AND a.missing = ?"
                .to_string(),
            vec![lib.clone(), Value::Bool(false)],
        ),
        (
            "SELECT count(*) as count FROM folder WHERE library_id = ? AND missing = ? AND num_audio_files > ?".to_string(),
            vec![lib.clone(), Value::Bool(false), Value::Integer(0)],
        ),
        (
            "SELECT ifnull(sum(num_audio_files + num_playlists + json_array_length(image_files)),0) as count \
             FROM folder WHERE library_id = ? AND missing = ?"
                .to_string(),
            vec![lib.clone(), Value::Bool(false)],
        ),
        (
            "SELECT count(*) as count FROM media_file WHERE library_id = ? AND missing = ?".to_string(),
            vec![lib.clone(), Value::Bool(true)],
        ),
        (
            "SELECT ifnull(sum(size),0) as sum FROM album WHERE library_id = ? AND missing = ?".to_string(),
            vec![lib.clone(), Value::Bool(false)],
        ),
        (
            "SELECT ifnull(sum(duration),0) as sum FROM album WHERE library_id = ? AND missing = ?".to_string(),
            vec![lib, Value::Bool(false)],
        ),
    ]
}

/// `RefreshStats`'s totals update (`:229-238`): the eight probe results in
/// probe order, then `updated_at` — Go's `.Set` call order.
#[must_use]
/// The eight probe results plus the id, in [`refresh_stats_probes_sql`] order.
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshStatsTotals {
    /// `library.id`.
    pub id: i64,
    /// `total_songs` (probe 0).
    pub songs: i64,
    /// `total_albums` (probe 1).
    pub albums: i64,
    /// `total_artists` (probe 2).
    pub artists: i64,
    /// `total_folders` (probe 3).
    pub folders: i64,
    /// `total_files` (probe 4).
    pub files: i64,
    /// `total_missing_files` (probe 5).
    pub missing: i64,
    /// `total_size` (probe 6).
    pub size: i64,
    /// `total_duration` (probe 7).
    pub duration: f64,
    /// `updated_at` (Go's `time.Now()`).
    pub updated_at: i64,
}

/// See [`RefreshStatsTotals`].
pub fn refresh_stats_update_sql(totals: &RefreshStatsTotals) -> Sql {
    (
        format!(
            "UPDATE {TABLE} SET total_songs = ?, total_albums = ?, total_artists = ?, \
             total_folders = ?, total_files = ?, total_missing_files = ?, total_size = ?, \
             total_duration = ?, updated_at = ? WHERE id = ?"
        ),
        vec![
            Value::Integer(totals.songs),
            Value::Integer(totals.albums),
            Value::Integer(totals.artists),
            Value::Integer(totals.folders),
            Value::Integer(totals.files),
            Value::Integer(totals.missing),
            Value::Integer(totals.size),
            Value::Real(totals.duration),
            Value::Integer(totals.updated_at),
            Value::Integer(totals.id),
        ],
    )
}

/// `Delete` (`:242-270`): the row delete — the admin/ID-1 validation
/// (`:244-249`) and the artist-orphan/plugin cleanups (`:252-266`) are the
/// caller's (auth, artist/plugin lanes).
#[must_use]
pub fn delete_sql(id: i64) -> Sql {
    (
        format!("DELETE FROM {TABLE} WHERE id = ?"),
        vec![Value::Integer(id)],
    )
}

/// `GetUsersWithLibraryAccess` (`:198-208`): the user rows joined through
/// `user_library`, ordered by name. Squirrel's plain `.Join` renders bare
/// `JOIN` (`squirrel@v1.5.4/select.go:298`).
#[must_use]
pub fn get_users_with_library_access_sql(library_id: i64) -> Sql {
    (
        "SELECT u.* FROM user u \
         JOIN user_library ul ON u.id = ul.user_id \
         WHERE ul.library_id = ? \
         ORDER BY u.name"
            .to_string(),
        vec![Value::Integer(library_id)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Arrange/Act/Assert helpers: every test pins the (sql, args) pair Go's
    // squirrel/dbx layer materializes, against the literal shapes of
    // library_repository.go.

    #[test]
    fn get_and_count_pin_the_base_selects() {
        // Arrange: Get (:35-40) and CountAll (:193-196) over library.
        // Act + Assert:
        assert_eq!(
            get_sql(3),
            (
                "SELECT * FROM library WHERE id = ?".to_string(),
                vec![Value::Integer(3)]
            )
        );
        assert_eq!(
            count_all_sql(),
            (
                "SELECT count(distinct library.id) as count FROM library".to_string(),
                Vec::new()
            )
        );
    }

    #[test]
    fn get_all_renders_pagination_and_caller_order() {
        // Arrange: GetAll (:186-191) under parsed options.
        // Act:
        let (sql, args) = get_all_sql(&["name asc"], 10, 20);
        // Assert: the bounds render as squirrel literals (`select.go:139-147`),
        // so this select binds nothing at all.
        assert_eq!(
            sql,
            "SELECT * FROM library ORDER BY name asc LIMIT 10 OFFSET 20"
        );
        assert!(args.is_empty());
        assert_eq!(
            get_all_sql(&[], 0, 0).0,
            "SELECT * FROM library",
            "non-positive bounds leave the clauses unset (applyOptions)"
        );
    }

    #[test]
    fn put_update_sets_all_columns_in_sorted_key_order() {
        // Arrange: Put's SetMap (:85-87) with no colsToUpdate — squirrel
        // sorts the SET keys (squirrel@v1.5.4/update.go:237).
        let update = LibraryUpdate {
            name: "Renamed",
            path: "/music/updated",
            remote_path: "/remote/original",
            default_new_users: true,
            pid_album: "alb",
            pid_track: "trk",
            updated_at: 1_700_000_000,
        };
        // Act:
        let (sql, args) = put_update_sql(2, &update, &[]);
        // Assert:
        assert_eq!(
            sql,
            "UPDATE library SET default_new_users = ?, name = ?, path = ?, \
             pid_album = ?, pid_track = ?, remote_path = ?, updated_at = ? WHERE id = ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Bool(true),
                Value::Text("Renamed".to_string()),
                Value::Text("/music/updated".to_string()),
                Value::Text("alb".to_string()),
                Value::Text("trk".to_string()),
                Value::Text("/remote/original".to_string()),
                Value::Integer(1_700_000_000),
                Value::Integer(2),
            ]
        );
    }

    #[test]
    fn put_update_honors_cols_to_update_and_always_stamps_updated_at() {
        // Arrange: Put(ctx, lib, "name", "path") (:65-69) — only the
        // requested columns plus the mandatory updated_at (:83).
        let update = LibraryUpdate {
            name: "Renamed",
            path: "/music/original",
            remote_path: "/remote/original",
            default_new_users: true,
            pid_album: "alb",
            pid_track: "trk",
            updated_at: 99,
        };
        // Act:
        let (sql, args) = put_update_sql(2, &update, &["name", "path"]);
        // Assert:
        assert_eq!(
            sql,
            "UPDATE library SET name = ?, path = ?, updated_at = ? WHERE id = ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("Renamed".to_string()),
                Value::Text("/music/original".to_string()),
                Value::Integer(99),
                Value::Integer(2),
            ]
        );
    }

    #[test]
    fn insert_binds_id_first_when_given_and_renders_all_row_columns() {
        // Arrange: dbx Model(l).Insert() (:78, :96-101) — the full row in
        // model.Library declaration order; id first for the ID-999 case.
        // Act:
        let (sql, args) = insert_sql(Some(999), vec![Value::from("x"); 21]);
        // Assert:
        assert!(sql.starts_with(
            "INSERT INTO library (id, name, path, remote_path, last_scan_at, \
             last_scan_started_at, full_scan_in_progress, updated_at, created_at, "
        ));
        assert!(sql.ends_with(&format!(
            "scanned_pid_track) VALUES ({})",
            vec!["?"; 22].join(", ")
        )));
        assert_eq!(args.len(), 22);
        assert_eq!(args[0], Value::Integer(999));
        assert!(insert_sql(None, vec![Value::from("x"); 21])
            .0
            .contains("INSERT INTO library (name, path,"));
    }

    #[test]
    fn assign_admins_is_the_argless_cross_join_upsert() {
        // Arrange: Put's admin assignment (:105-111) — the exact literal.
        // Act:
        let (sql, args) = assign_admins_sql();
        // Assert:
        assert_eq!(
            sql,
            "INSERT INTO user_library (user_id, library_id)\nSELECT u.id, l.id\n\
             FROM user u\nCROSS JOIN library l\nWHERE u.is_admin = true\n\
             ON CONFLICT (user_id, library_id) DO NOTHING;"
        );
        assert!(args.is_empty(), "identifiers only — nothing to bind");
    }

    #[test]
    fn store_music_folder_guards_on_unchanged_path() {
        // Arrange: StoreMusicFolder (:117-133) — Set order is call order.
        // Act:
        let (sql, args) = store_music_folder_sql("/music", 5);
        // Assert:
        assert_eq!(
            sql,
            "UPDATE library SET path = ?, updated_at = ? WHERE id = ? AND path <> ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("/music".to_string()),
                Value::Integer(5),
                Value::Integer(DEFAULT_LIBRARY_ID),
                Value::Text("/music".to_string()),
            ]
        );
    }

    #[test]
    fn scan_lifecycle_and_pid_updates_pin_set_order() {
        // Arrange: ScanBegin/ScanEnd/SetScannedPID (:147-178) — .Set call order.
        // Act + Assert:
        assert_eq!(
            scan_begin_sql(1, 7, true),
            (
                "UPDATE library SET last_scan_started_at = ?, full_scan_in_progress = ? WHERE id = ?".to_string(),
                vec![Value::Integer(7), Value::Bool(true), Value::Integer(1)]
            )
        );
        assert_eq!(
            scan_end_sql(1, 8),
            (
                "UPDATE library SET last_scan_at = ?, full_scan_in_progress = ?, \
                 last_scan_started_at = ? WHERE id = ?"
                    .to_string(),
                vec![
                    Value::Integer(8),
                    Value::Bool(false),
                    Value::Integer(ZERO_TIME),
                    Value::Integer(1)
                ]
            )
        );
        assert_eq!(
            set_scanned_pid_sql(2, "a", "t"),
            (
                "UPDATE library SET scanned_pid_album = ?, scanned_pid_track = ? WHERE id = ?"
                    .to_string(),
                vec![
                    Value::Text("a".to_string()),
                    Value::Text("t".to_string()),
                    Value::Integer(2)
                ]
            )
        );
    }

    #[test]
    fn scan_in_progress_compares_against_the_same_zero_scan_end_writes() {
        // Arrange: ScanInProgress (:180-184) — the NotEq against time.Time{}.
        // The constant must be the one scan_end_sql binds, or a finished
        // scan reads as in-progress.
        let (_, end_args) = scan_end_sql(1, 8);
        // Act:
        let (sql, args) = scan_in_progress_sql();
        // Assert:
        assert_eq!(sql, "SELECT * FROM library WHERE last_scan_started_at <> ?");
        assert_eq!(args[0], end_args[2]);
    }

    #[test]
    fn refresh_stats_probes_pin_the_eight_aggregates() {
        // Arrange: RefreshStats' run.Parallel probes (:199-236) — each
        // two-key Eq map renders alphabetically (library_id, missing).
        // Act:
        let probes = refresh_stats_probes_sql(1);
        // Assert:
        assert_eq!(
            probes[0].0,
            "SELECT count(*) as count FROM media_file WHERE library_id = ? AND missing = ?"
        );
        assert_eq!(probes[0].1, vec![Value::Integer(1), Value::Bool(false)]);
        assert_eq!(
            probes[2].0,
            "SELECT count(*) as count FROM library_artist la \
             INNER JOIN artist a ON la.artist_id = a.id \
             WHERE la.library_id = ? AND a.missing = ?"
        );
        assert_eq!(
            probes[3].0,
            "SELECT count(*) as count FROM folder WHERE library_id = ? AND missing = ? AND num_audio_files > ?"
        );
        assert_eq!(
            probes[4].0,
            "SELECT ifnull(sum(num_audio_files + num_playlists + json_array_length(image_files)),0) as count FROM folder WHERE library_id = ? AND missing = ?"
        );
        assert_eq!(
            probes[5].1,
            vec![Value::Integer(1), Value::Bool(true)],
            "the missing-files probe flips missing to true (:228-230)"
        );
        assert_eq!(
            probes[6].0,
            "SELECT ifnull(sum(size),0) as sum FROM album WHERE library_id = ? AND missing = ?"
        );
        assert_eq!(
            probes[7].0,
            "SELECT ifnull(sum(duration),0) as sum FROM album WHERE library_id = ? AND missing = ?"
        );
    }

    #[test]
    fn refresh_stats_update_sets_totals_in_probe_order() {
        // Arrange: the totals UPDATE (:229-238) — .Set call order, values
        // ordered to the probes.
        // Act:
        let totals = RefreshStatsTotals {
            id: 1,
            songs: 10,
            albums: 2,
            artists: 3,
            folders: 4,
            files: 5,
            missing: 6,
            size: 7,
            duration: 8.5,
            updated_at: 9,
        };
        // Act:
        let (sql, args) = refresh_stats_update_sql(&totals);
        // Assert:
        assert_eq!(
            sql,
            "UPDATE library SET total_songs = ?, total_albums = ?, total_artists = ?, \
             total_folders = ?, total_files = ?, total_missing_files = ?, total_size = ?, \
             total_duration = ?, updated_at = ? WHERE id = ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Integer(10),
                Value::Integer(2),
                Value::Integer(3),
                Value::Integer(4),
                Value::Integer(5),
                Value::Integer(6),
                Value::Integer(7),
                Value::Real(8.5),
                Value::Integer(9),
                Value::Integer(1),
            ]
        );
    }

    #[test]
    fn add_artist_delete_and_users_join_pin_their_shapes() {
        // Arrange: AddArtist (:135-145), Delete (:242-270) and
        // GetUsersWithLibraryAccess (:198-208).
        // Act + Assert:
        assert_eq!(
            add_artist_sql(1, "ar-1"),
            (
                "INSERT INTO library_artist (library_id, artist_id) VALUES (?, ?) \
                 on conflict(library_id, artist_id) do nothing"
                    .to_string(),
                vec![Value::Integer(1), Value::Text("ar-1".to_string())]
            )
        );
        assert_eq!(
            delete_sql(1),
            (
                "DELETE FROM library WHERE id = ?".to_string(),
                vec![Value::Integer(1)]
            )
        );
        assert_eq!(
            get_users_with_library_access_sql(2),
            (
                "SELECT u.* FROM user u JOIN user_library ul ON u.id = ul.user_id \
                 WHERE ul.library_id = ? ORDER BY u.name"
                    .to_string(),
                vec![Value::Integer(2)]
            )
        );
    }
}
