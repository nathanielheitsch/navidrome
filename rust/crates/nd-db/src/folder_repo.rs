//! Port of `persistence/folder_repository.go` (327L) — the folder repository.
//!
//! Go's repository is a `sqlRepository` over `folder` whose selects join
//! `library` for the caller's path (`selectFolder`, `:58-62`:
//! `folder.*`, `library.path as library_path`, INNER JOIN — a folder row
//! always has its library). Every method builds a squirrel statement and
//! hands it to `queryOne`/`queryAll`/`count`/`exists`/`executeSQL`, which
//! materialize through `toSQL()` (`sql_base_repository.go:358-372`) as
//! `(sql, args)`; execution stays with the caller (`store.rs` seam), so this
//! port stops at the same [`Sql`] pairs and pins their shapes.
//!
//! `PostScan`/`PostMapArgs` (`:25-53`) marshal the `image_files` JSON column
//! (TEXT in the table, `json.Unmarshal` into `model.Folder.ImageFiles`,
//! `"[]"` for a nil slice). Only the SQL shapes pin here; the JSON row codec
//! belongs to the model/execution slice.
//!
//! `GetTouchedWithPlaylists`/`GetAllWithPlaylists` (`:282-297`) wrap Go's
//! `queryWithStableResults` cursor machinery, which is the streaming slice's
//! to port; their WHERE shapes pin here so the caller can hand the pair to
//! its own cursor seam.
//!
//! `GetFolderUpdateInfo` (`:86-117`) batches target paths at
//! [`BATCH_SIZE`] (100: each path adds ~3 conditions and SQLite rejects
//! expression trees deeper than 1000, `:81-83`); a path of `""`/`"."` selects
//! the whole library (`:90-93` → [`get_folder_update_info_all_sql`]).
//!
//! Safety rule (`sqlrepo.rs`): identifiers are caller-supplied names only;
//! every value — paths, ids, LIKE patterns — is bound to a `?` parameter.
//! [`escape_like_prefix`] keeps a LIKE prefix literal even when the path
//! contains `%`/`_`/`\`.

use crate::sqlrepo::{Repository, Value};

/// `r.tableName` (`newFolderRepository`, `folder_repository.go:56`).
pub const FOLDER_TABLE: &str = "folder";

/// One materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// `GetFolderUpdateInfo`'s path batch size (`folder_repository.go:82`).
pub const BATCH_SIZE: usize = 100;

/// `MarkMissing`'s id chunk size (`slices.Chunk(ids, 200)`,
/// `folder_repository.go:265`).
pub const MARK_MISSING_CHUNK_SIZE: usize = 200;

/// `escapeLikePrefix` (`folder_repository.go:251-253`): escapes the SQL LIKE
/// wildcards so a path can be a literal prefix in a `LIKE … ESCAPE '\'`
/// pattern. Go's `strings.NewReplacer` order — backslash first — is
/// load-bearing (an escaped `%` must not double-escape a preceding `\`).
#[must_use]
pub fn escape_like_prefix(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '%' => out.push_str("\\%"),
            '_' => out.push_str("\\_"),
            other => out.push(other),
        }
    }
    out
}

/// `path.Clean` / `filepath.Clean` on slash-form paths
/// (`folder_repository.go:118,215` and `model/folder.go:48-50`): collapse
/// separators, drop `.` segments, resolve `..`, empty → `"."`. Only what Go's
/// clean does to the repository's slash-form relative paths.
fn clean_path(p: &str) -> String {
    if p.is_empty() {
        return ".".to_string();
    }
    let abs = p.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else if !abs {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return if abs {
            "/".to_string()
        } else {
            ".".to_string()
        };
    }
    let joined = parts.join("/");
    if abs {
        format!("/{joined}")
    } else {
        joined
    }
}

/// `FolderID` (`model/folder.go:46-52`, reached via `model.NewFolder` at
/// `folder_repository.go:71`): strip the library root prefix and a leading
/// separator, clean, then hash `"<libID>:<path>"`.
#[must_use]
pub fn folder_id(lib_id: i64, lib_path: &str, path: &str) -> String {
    let rel = path.strip_prefix(lib_path).unwrap_or(path);
    let rel = rel.strip_prefix('/').unwrap_or(rel);
    let cleaned = clean_path(rel);
    nd_id::new_hash(&[&lib_id.to_string(), &cleaned])
}

/// `selectFolder` (`folder_repository.go:58-62`): the base select —
/// `folder.*` plus `library.path as library_path`, INNER-joined — then the
/// library-access filter. `library_filter` is the caller-resolved
/// `applyLibraryFilter` clause (`sql_base_repository.go:261-282`;
/// `None` = admin/no user, Go skips it).
#[must_use]
pub fn select_folder(library_filter: Option<Sql>) -> Repository {
    let sq = Repository::new(FOLDER_TABLE)
        .select(["folder.*", "library.path as library_path"])
        .inner_join("library", "library.id = folder.library_id");
    match library_filter {
        Some((sql, args)) => sq.where_raw(sql, args),
        None => sq,
    }
}

/// `Get` (`folder_repository.go:64-67`): the base select by id.
#[must_use]
pub fn get_sql(id: &str, library_filter: Option<Sql>) -> Sql {
    select_folder(library_filter)
        .where_eq("folder.id", id)
        .to_sql()
        .unwrap_or_default()
}

/// `GetAll` (`folder_repository.go:74-77`): the base select with the
/// caller-supplied order-by expressions (already direction-carrying, per
/// `buildSortOrder` output) and `applyOptions`' Max/Offset
/// (`sql_base_repository.go:105-108`, non-positive drops the clause).
#[must_use]
pub fn get_all_sql(
    library_filter: Option<Sql>,
    order_by: Vec<String>,
    max: i64,
    offset: i64,
) -> Sql {
    let sq = select_folder(library_filter);
    let sq = if order_by.is_empty() {
        sq
    } else {
        sq.order_by(order_by)
    };
    let sq = if max > 0 {
        sq.limit(u64::try_from(max).unwrap_or(u64::MAX))
    } else {
        sq
    };
    if offset > 0 {
        sq.offset(u64::try_from(offset).unwrap_or(u64::MAX))
    } else {
        sq
    }
    .to_sql()
    .unwrap_or_default()
}

/// `CountAll` (`folder_repository.go:79-82`): `newSelect` + `count(*)` +
/// `applyLibraryFilter`. Go's folder repository counts `count(*)` (not the
/// base `count(distinct id)` shape), because every folder row is one id.
#[must_use]
pub fn count_all_sql(library_filter: Option<Sql>) -> Sql {
    let sq = Repository::new(FOLDER_TABLE).select(["count(*)"]);
    match library_filter {
        Some((sql, args)) => sq.where_raw(sql, args),
        None => sq,
    }
    .to_sql()
    .unwrap_or_default()
}

/// `queryFolderUpdateInfo`'s select (`folder_repository.go:167-169`), shared
/// by [`get_folder_update_info_all_sql`] and
/// [`get_folder_update_info_batch_sql`].
const UPDATE_INFO_COLUMNS: &str = "id, updated_at, hash, image_files, images_updated_at";

/// `getFolderUpdateInfoAll` (`folder_repository.go:123-131`): the
/// update-info columns for every non-missing folder of the library.
/// `image_files` is TEXT; its JSON decode (`:172-177`) is the caller's.
#[must_use]
pub fn get_folder_update_info_all_sql(lib_id: i64) -> Sql {
    (
        format!(
            "SELECT {UPDATE_INFO_COLUMNS} FROM {FOLDER_TABLE} \
             WHERE library_id = ? AND missing = ?"
        ),
        vec![Value::Integer(lib_id), Value::Bool(false)],
    )
}

/// `getFolderUpdateInfoBatch` (`folder_repository.go:134-161`) for one batch
/// of paths (`GetFolderUpdateInfo` chunks at [`BATCH_SIZE`], `:82-89`):
/// non-missing folders of the library whose id is one of the target folder
/// ids OR whose path equals / starts with a target path. Go's `And`/`Or`
/// render parenthesized, so the `id IN` group and the per-path OR group wrap;
/// args bind in fragment order (library_id, missing, ids, then per path:
/// equality, LIKE prefix). `clean_path` mirrors `path.Clean(TrimPrefix(p,
/// "/"))` (`:141-143`) and the ids come from [`folder_id`]
/// (`model.FolderID`, `:145`).
#[must_use]
pub fn get_folder_update_info_batch_sql(lib_id: i64, target_paths: &[String]) -> Sql {
    let mut id_args: Vec<Value> = Vec::with_capacity(target_paths.len());
    let mut or_clauses: Vec<String> = Vec::with_capacity(target_paths.len() * 2);
    let mut or_args: Vec<Value> = Vec::with_capacity(target_paths.len() * 2);
    for p in target_paths {
        let clean = clean_path(p.strip_prefix('/').unwrap_or(p));
        id_args.push(Value::from(folder_id(lib_id, "", &clean)));
        or_clauses.push("path = ?".to_string());
        or_args.push(Value::from(clean.as_str()));
        or_clauses.push("path LIKE ? ESCAPE '\\'".to_string());
        or_args.push(Value::from(escape_like_prefix(&clean) + "/%"));
    }
    let ids_placeholders = vec!["?"; id_args.len()].join(", ");
    let mut args = vec![Value::Integer(lib_id), Value::Bool(false)];
    args.extend(id_args);
    args.extend(or_args);
    (
        format!(
            "SELECT {UPDATE_INFO_COLUMNS} FROM {FOLDER_TABLE} \
             WHERE library_id = ? AND missing = ? AND (id IN ({ids_placeholders})) \
             AND ({})",
            or_clauses.join(" OR ")
        ),
        args,
    )
}

/// `folderSubtreeFilter` (`folder_repository.go:210-227`): the folders at the
/// given library-relative paths plus their descendants — the
/// `folder.library_id` / `folder.missing` conditions plus a flat OR over
/// each path's `folder.id`, `folder.path` and the escaped
/// `folder.path LIKE … ESCAPE '\'` prefix (squirrel renders a flat `Or{…}`
/// with one ` OR ` between every clause, no inner grouping). A path that
/// cleans to `"."` selects the whole library and drops the path conditions
/// (`:215-217`). The fragment is caller-bound (Go composes it inside
/// `Exists(...)` joins, `album_repository.go:291-293`).
#[must_use]
pub fn folder_subtree_filter(lib_id: i64, paths: &[String]) -> Sql {
    let mut args: Vec<Value> = vec![Value::Integer(lib_id), Value::Bool(false)];
    let mut or_clauses: Vec<String> = Vec::new();
    for p in paths {
        let clean = clean_path(p.strip_prefix('/').unwrap_or(p));
        if clean == "." {
            return (
                "(folder.library_id = ? AND folder.missing = ?)".to_string(),
                args,
            );
        }
        or_clauses.push("folder.id = ?".to_string());
        or_clauses.push("folder.path = ?".to_string());
        or_clauses.push("folder.path LIKE ? ESCAPE '\\'".to_string());
        args.push(Value::from(folder_id(lib_id, "", &clean)));
        args.push(Value::from(clean.as_str()));
        args.push(Value::from(escape_like_prefix(&clean) + "/%"));
    }
    (
        format!(
            "(folder.library_id = ? AND folder.missing = ? AND ({}))",
            or_clauses.join(" OR ")
        ),
        args,
    )
}

/// `HasAudioOutsideFolders` (`folder_repository.go:236-247`): any non-missing
/// folder in `parent`'s subtree, other than the excluded ids, that holds
/// audio files. `parent_path` is Go's
/// `TrimPrefix(path.Join(parent.Path, parent.Name), "/")` (`:240`); the
/// `parent.NumAudioFiles > 0` fast path (`:237-239`) is a caller-side check
/// and produces no SQL. Go's `Eq{"library_id": …, "missing": false}` map
/// iterates randomly; the port pins `library_id` first for a stable shape.
#[must_use]
pub fn has_audio_outside_folders_sql(
    library_id: i64,
    parent_path: &str,
    exclude_folder_ids: &[String],
) -> Sql {
    let mut sql = format!(
        "SELECT 1 FROM {FOLDER_TABLE} WHERE library_id = ? AND missing = ? \
         AND num_audio_files > ? AND "
    );
    let mut args = vec![
        Value::Integer(library_id),
        Value::Bool(false),
        Value::Integer(1),
    ];
    if exclude_folder_ids.is_empty() {
        // Squirrel's empty-Eq renders 1 = 0 (matches nothing).
        sql.push_str("1 = 0");
    } else {
        let placeholders = vec!["?"; exclude_folder_ids.len()].join(", ");
        sql.push_str(&format!("id NOT IN ({placeholders})"));
        args.extend(exclude_folder_ids.iter().map(|id| Value::from(id.as_str())));
    }
    sql.push_str(" AND (path = ? OR path LIKE ? ESCAPE '\\') LIMIT 1");
    args.push(Value::from(parent_path));
    args.push(Value::from(escape_like_prefix(parent_path) + "/%"));
    (sql, args)
}

/// `Put` (`folder_repository.go:255-259`): `r.put(ctx, f.ID, dbFolder)` —
/// the update-then-insert upsert of `sql_base_repository.go:618-642`. The
/// UPDATE matches rows by `id` and the INSERT runs only when it matched
/// nothing; the caller binds [`PutFolderSql::update`] first, then
/// [`PutFolderSql::insert`] when the update's row count is 0.
/// `columns`/`values` pair one column per value (the caller marshalled the
/// `image_files` JSON itself); `created_at` never appears in an update's set
/// map (`selectUpdateColumns`, `sql_base_repository.go:591-604`).
#[derive(Debug, Clone, PartialEq)]
pub struct PutFolderSql {
    /// `Update(folder).SetMap(...).Where(Eq{"id": id})` — the set pairs bind
    /// before `id`.
    pub update: Sql,
    /// `Insert(folder).SetMap(values)` — the set pairs in column order, then
    /// `id`, matching the column list (`sql_base_repository.go:637-641`).
    pub insert: Sql,
}

/// See [`PutFolderSql`].
#[must_use]
pub fn put_sql(id: &str, columns: &[String], values: Vec<Value>) -> PutFolderSql {
    let set_pairs = columns
        .iter()
        .map(|c| format!("{c} = ?"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut update_args = values.clone();
    update_args.push(Value::from(id));
    let insert_cols = columns
        .iter()
        .map(|c| c.as_str())
        .chain(["id"])
        .collect::<Vec<_>>()
        .join(", ");
    // Columns end with `id` (above), so the VALUES tuple must end with it too
    // (`squirrel@v1.5.4/insert.go:271-283`); binding id first rotates values.
    let mut insert_args = values;
    insert_args.push(Value::from(id));
    let placeholders = vec!["?"; insert_args.len()].join(", ");
    PutFolderSql {
        update: (
            format!("UPDATE {FOLDER_TABLE} SET {set_pairs} WHERE id = ?"),
            update_args,
        ),
        insert: (
            format!("INSERT INTO {FOLDER_TABLE} ({insert_cols}) VALUES ({placeholders})"),
            insert_args,
        ),
    }
}

/// `MarkMissing` (`folder_repository.go:261-273`): one UPDATE per 200-id
/// chunk (`slices.Chunk(ids, 200)`, `:265`), setting `missing` and
/// `updated_at` (the caller binds the timestamp).
#[must_use]
pub fn mark_missing_sql(missing: bool, ids: &[String], updated_at: i64) -> Vec<Sql> {
    ids.chunks(MARK_MISSING_CHUNK_SIZE)
        .map(|chunk| {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let mut args = vec![Value::Bool(missing), Value::Integer(updated_at)];
            args.extend(chunk.iter().map(|id| Value::from(id.as_str())));
            (
                format!(
                    "UPDATE {FOLDER_TABLE} \
                     SET missing = ?, updated_at = ? WHERE id IN ({placeholders})"
                ),
                args,
            )
        })
        .collect()
}

/// `GetTouchedWithPlaylists` (`folder_repository.go:282-289`): non-missing
/// folders with playlists touched since the library's last scan. Go wraps the
/// select in `queryWithStableResults` (the streaming slice's port); this pins
/// the select pair it wraps.
#[must_use]
pub fn get_touched_with_playlists_sql(library_filter: Option<Sql>) -> Sql {
    select_folder(library_filter)
        .where_eq("folder.missing", false)
        .where_raw("folder.num_playlists > ?", vec![Value::Integer(0)])
        .where_raw("folder.updated_at > library.last_scan_at", Vec::new())
        .to_sql()
        .unwrap_or_default()
}

/// `GetAllWithPlaylists` (`folder_repository.go:291-297`): non-missing
/// folders with playlists.
#[must_use]
pub fn get_all_with_playlists_sql(library_filter: Option<Sql>) -> Sql {
    select_folder(library_filter)
        .where_eq("folder.missing", false)
        .where_raw("folder.num_playlists > ?", vec![Value::Integer(0)])
        .to_sql()
        .unwrap_or_default()
}

/// `purgeEmpty` (`folder_repository.go:315-333`): one DELETE, optionally
/// restricted to the given library ids. Go's `And{...}` clause order pins
/// as written (an empty `image_files` cell is the literal `"[]"` the
/// PostMapArgs default writes).
#[must_use]
pub fn purge_empty_sql(library_ids: &[i64]) -> Sql {
    let mut sql = format!(
        "DELETE FROM {FOLDER_TABLE} WHERE num_audio_files = ? AND num_playlists = ? \
         AND image_files = ? \
         AND id not in (select parent_id from folder) \
         AND id not in (select folder_id from media_file)"
    );
    let mut args = vec![Value::Integer(0), Value::Integer(0), Value::from("[]")];
    if !library_ids.is_empty() {
        let placeholders = vec!["?"; library_ids.len()].join(", ");
        sql.push_str(&format!(" AND library_id IN ({placeholders})"));
        args.extend(library_ids.iter().map(|id| Value::Integer(*id)));
    }
    (sql, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib_filter() -> Option<Sql> {
        Some((
            "folder.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
                .to_string(),
            vec![Value::from("user1")],
        ))
    }

    #[test]
    fn escape_like_prefix_escapes_go_wildcards() {
        // Arrange: the strings.NewReplacer pairs (:251-253).
        // Act/Assert:
        assert_eq!(escape_like_prefix("a\\b"), "a\\\\b".to_string());
        assert_eq!(escape_like_prefix("a%b"), "a\\%b".to_string());
        assert_eq!(escape_like_prefix("a_b"), "a\\_b".to_string());
        assert_eq!(escape_like_prefix("plain"), "plain".to_string());
    }

    #[test]
    fn clean_path_matches_path_clean() {
        // Arrange: path.Clean shapes on slash-form paths (:118,215).
        // Act/Assert:
        assert_eq!(clean_path(""), ".".to_string());
        assert_eq!(clean_path("music/"), "music".to_string());
        assert_eq!(clean_path("a/./b/../c"), "a/c".to_string());
        assert_eq!(clean_path("/music"), "/music".to_string());
    }

    #[test]
    fn folder_id_hashes_lib_id_and_cleaned_relative_path() {
        // Arrange: FolderID (model/folder.go:46-52): the library root prefix
        // and leading separator strip, then "<libID>:<clean>" hashes.
        // Act:
        let from_lib_root = folder_id(7, "/media", "/media/music");
        let already_rel = folder_id(7, "/media", "music");
        let cleaned = folder_id(7, "/media", "music/");
        // Assert:
        assert_eq!(from_lib_root, already_rel);
        assert_eq!(already_rel, cleaned);
        assert_eq!(from_lib_root, nd_id::new_hash(&["7", "music"]));
    }

    #[test]
    fn select_folder_joins_library_and_applies_filter() {
        // Arrange: selectFolder (:58-62) + applyLibraryFilter.
        // Act:
        let (sql, args) = select_folder(lib_filter()).to_sql().expect("renders");
        // Assert:
        assert_eq!(
            sql,
            "SELECT folder.*, library.path as library_path FROM folder \
             INNER JOIN library ON library.id = folder.library_id \
             WHERE folder.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
        );
        assert_eq!(args, vec![Value::Text("user1".to_string())]);
        let (bare_sql, bare_args) = select_folder(None).to_sql().expect("renders");
        assert_eq!(
            bare_sql,
            "SELECT folder.*, library.path as library_path FROM folder \
             INNER JOIN library ON library.id = folder.library_id"
        );
        assert!(bare_args.is_empty());
    }

    #[test]
    fn get_sql_filters_by_bound_folder_id() {
        // Arrange: Get (:64-67).
        // Act:
        let (sql, args) = get_sql("f-1", None);
        // Assert:
        assert_eq!(
            sql,
            "SELECT folder.*, library.path as library_path FROM folder \
             INNER JOIN library ON library.id = folder.library_id WHERE folder.id = ?"
        );
        assert_eq!(args, vec![Value::Text("f-1".to_string())]);
    }

    #[test]
    fn get_all_applies_order_and_pagination() {
        // Arrange: GetAll (:74-77) via applyOptions (:105-108): order-by
        // expressions arrive direction-carrying; non-positive bounds drop.
        // Act:
        let (sql, args) = get_all_sql(None, vec!["folder.name asc".to_string()], 10, 20);
        let (bare_sql, _) = get_all_sql(None, Vec::new(), 0, 0);
        // Assert: the bounds render as squirrel literals (`select.go:139-147`),
        // so the page contributes no args.
        assert_eq!(
            sql,
            "SELECT folder.*, library.path as library_path FROM folder \
             INNER JOIN library ON library.id = folder.library_id \
             ORDER BY folder.name asc LIMIT 10 OFFSET 20"
        );
        assert!(args.is_empty());
        assert_eq!(
            bare_sql,
            "SELECT folder.*, library.path as library_path FROM folder \
             INNER JOIN library ON library.id = folder.library_id"
        );
    }

    #[test]
    fn count_all_uses_plain_count_star() {
        // Arrange: CountAll (:79-82) keeps count(*), not count(distinct id).
        // Act:
        let (sql, args) = count_all_sql(None);
        // Assert:
        assert_eq!(sql, "SELECT count(*) FROM folder");
        assert!(args.is_empty());
    }

    #[test]
    fn update_info_all_pins_non_missing_library_scope() {
        // Arrange: getFolderUpdateInfoAll (:123-131).
        // Act:
        let (sql, args) = get_folder_update_info_all_sql(7);
        // Assert:
        assert_eq!(
            sql,
            "SELECT id, updated_at, hash, image_files, images_updated_at FROM folder \
             WHERE library_id = ? AND missing = ?"
        );
        assert_eq!(args, vec![Value::Integer(7), Value::Bool(false)]);
    }

    #[test]
    fn update_info_batch_pins_id_in_group_and_path_or_group() {
        // Arrange: one-batch getFolderUpdateInfoBatch (:134-161): the
        // `Or{Eq{"id": folderIDs}, pathConditions}` renders as
        // `(id IN (…)) OR (…)` inside Go's And — squirrel Or wraps each
        // element, Eq-slice renders the IN-list.
        let paths = vec!["music".to_string(), "music/rock".to_string()];
        // Act:
        let (sql, args) = get_folder_update_info_batch_sql(7, &paths);
        // Assert:
        assert_eq!(
            sql,
            "SELECT id, updated_at, hash, image_files, images_updated_at FROM folder \
             WHERE library_id = ? AND missing = ? AND (id IN (?, ?)) \
             AND (path = ? OR path LIKE ? ESCAPE '\\' OR path = ? OR path LIKE ? ESCAPE '\\')"
        );
        assert_eq!(args[..2], [Value::Integer(7), Value::Bool(false)]);
        // Both batch folder ids are FolderID(7, path) hashes.
        assert_eq!(args[2], Value::Text(folder_id(7, "", "music")));
        assert_eq!(args[3], Value::Text(folder_id(7, "", "music/rock")));
        assert_eq!(
            args[4..],
            [
                Value::Text("music".to_string()),
                Value::Text("music/%".to_string()),
                Value::Text("music/rock".to_string()),
                Value::Text("music/rock/%".to_string()),
            ]
        );
    }

    #[test]
    fn subtree_filter_root_path_selects_whole_library() {
        // Arrange: the cleanPath "." early return (:215-217), reached via
        // GetFolderUpdateInfo's ""/"." whole-library rule (:90-93).
        // Act:
        let (sql, args) = folder_subtree_filter(7, &["".to_string(), ".".to_string()]);
        // Assert:
        assert_eq!(sql, "(folder.library_id = ? AND folder.missing = ?)");
        assert_eq!(args, vec![Value::Integer(7), Value::Bool(false)]);
    }

    #[test]
    fn subtree_filter_pins_flat_or_with_escaped_like() {
        // Arrange: folderSubtreeFilter (:210-227) for two paths — a flat OR,
        // one ` OR ` between every clause, LIKE prefix escaped.
        // Act:
        let (sql, args) = folder_subtree_filter(7, &["music".to_string(), "a%b".to_string()]);
        // Assert:
        assert_eq!(
            sql,
            "(folder.library_id = ? AND folder.missing = ? AND \
             (folder.id = ? OR folder.path = ? OR folder.path LIKE ? ESCAPE '\\' \
             OR folder.id = ? OR folder.path = ? OR folder.path LIKE ? ESCAPE '\\'))"
        );
        assert_eq!(
            args,
            vec![
                Value::Integer(7),
                Value::Bool(false),
                Value::Text(folder_id(7, "", "music")),
                Value::Text("music".to_string()),
                Value::Text("music/%".to_string()),
                Value::Text(folder_id(7, "", "a%b")),
                Value::Text("a%b".to_string()),
                Value::Text("a\\%b/%".to_string()),
            ]
        );
    }

    #[test]
    fn has_audio_pins_exclude_not_in_and_subtree_or() {
        // Arrange: HasAudioOutsideFolders (:236-247).
        // Act:
        let (sql, args) =
            has_audio_outside_folders_sql(7, "music", &["f-1".to_string(), "f-2".to_string()]);
        let (bare_sql, bare_args) = has_audio_outside_folders_sql(7, "music", &[]);
        // Assert:
        assert_eq!(
            sql,
            "SELECT 1 FROM folder WHERE library_id = ? AND missing = ? \
             AND num_audio_files > ? AND id NOT IN (?, ?) \
             AND (path = ? OR path LIKE ? ESCAPE '\\') LIMIT 1"
        );
        assert_eq!(
            args,
            vec![
                Value::Integer(7),
                Value::Bool(false),
                Value::Integer(1),
                Value::Text("f-1".to_string()),
                Value::Text("f-2".to_string()),
                Value::Text("music".to_string()),
                Value::Text("music/%".to_string()),
            ]
        );
        // An empty exclusion list matches nothing instead of every folder.
        assert!(bare_sql.contains("AND 1 = 0 AND"));
        assert_eq!(bare_args[..3], args[..3]);
    }

    #[test]
    fn put_sql_pins_update_then_insert_with_go_bind_order() {
        // Arrange: Put → r.put (sql_base_repository.go:618-642): the update
        // binds the set pairs before id; the insert binds id first.
        let columns = vec!["name".to_string(), "image_files".to_string()];
        // Act:
        let put = put_sql(
            "f-1",
            &columns,
            vec![Value::from("Rock"), Value::from("[]")],
        );
        // Assert:
        assert_eq!(
            put.update,
            (
                "UPDATE folder SET name = ?, image_files = ? WHERE id = ?".to_string(),
                vec![
                    Value::Text("Rock".to_string()),
                    Value::Text("[]".to_string()),
                    Value::Text("f-1".to_string()),
                ],
            )
        );
        assert_eq!(
            put.insert,
            (
                "INSERT INTO folder (name, image_files, id) VALUES (?, ?, ?)".to_string(),
                vec![
                    Value::Text("Rock".to_string()),
                    Value::Text("[]".to_string()),
                    Value::Text("f-1".to_string()),
                ],
            )
        );
    }

    #[test]
    fn mark_missing_chunks_ids_and_sets_updated_at() {
        // Arrange: 201 ids → 2 chunks (:261-273, chunk 200).
        let ids = (0..201).map(|i| format!("f-{i}")).collect::<Vec<_>>();
        // Act:
        let stmts = mark_missing_sql(true, &ids, 1_700_000_000);
        // Assert:
        assert_eq!(stmts.len(), 2);
        assert_eq!(
            stmts[0].0,
            format!(
                "UPDATE folder SET missing = ?, updated_at = ? WHERE id IN ({})",
                vec!["?"; 200].join(", ")
            )
        );
        assert_eq!(stmts[0].1[0], Value::Bool(true));
        assert_eq!(stmts[0].1[1], Value::Integer(1_700_000_000));
        assert_eq!(stmts[0].1.len(), 202);
        assert_eq!(stmts[1].1.len(), 3);
    }

    #[test]
    fn playlist_cursor_wheres_pin_their_conditions() {
        // Arrange: GetTouchedWithPlaylists (:282-289) adds the last_scan_at
        // comparison GetAllWithPlaylists (:291-297) omits.
        // Act:
        let touched = get_touched_with_playlists_sql(None);
        let all = get_all_with_playlists_sql(None);
        // Assert:
        assert_eq!(
            touched.0,
            "SELECT folder.*, library.path as library_path FROM folder \
             INNER JOIN library ON library.id = folder.library_id \
             WHERE folder.missing = ? AND folder.num_playlists > ? \
             AND folder.updated_at > library.last_scan_at"
        );
        assert_eq!(touched.1, vec![Value::Bool(false), Value::Integer(0)]);
        assert_eq!(
            all.0,
            "SELECT folder.*, library.path as library_path FROM folder \
             INNER JOIN library ON library.id = folder.library_id \
             WHERE folder.missing = ? AND folder.num_playlists > ?"
        );
        assert_eq!(all.1, vec![Value::Bool(false), Value::Integer(0)]);
    }

    #[test]
    fn purge_empty_pins_deletes_and_optional_library_scope() {
        // Arrange: purgeEmpty (:315-333).
        // Act:
        let bare = purge_empty_sql(&[]);
        let scoped = purge_empty_sql(&[1, 2]);
        // Assert:
        assert_eq!(
            bare.0,
            "DELETE FROM folder WHERE num_audio_files = ? AND num_playlists = ? \
             AND image_files = ? AND id not in (select parent_id from folder) \
             AND id not in (select folder_id from media_file)"
        );
        assert_eq!(
            bare.1,
            vec![
                Value::Integer(0),
                Value::Integer(0),
                Value::Text("[]".to_string()),
            ]
        );
        assert!(scoped.0.ends_with("AND library_id IN (?, ?)"));
        assert_eq!(scoped.1.len(), 5);
    }
}
