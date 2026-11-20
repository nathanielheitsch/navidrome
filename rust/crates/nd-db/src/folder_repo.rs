//! Port of `persistence/folder_repository.go` (327L) — the folder repository.
//!
//! Go's repository is a `sqlRepository` over `folder` whose selects join
//! `library` for the caller's path (`selectFolder`, `:74-78`:
//! `folder.*`, `library.path as library_path`, INNER JOIN — a folder row
//! always has its library). Every method builds a squirrel statement and
//! hands it to `queryOne`/`queryAll`/`count`/`exists`/`executeSQL`, which
//! materialize through `toSQL()` (`sql_base_repository.go:358-372`) as
//! `(sql, args)`. S-D3g2 wires the READ side onto that pair through
//! [`crate::execution`] ([`FolderRepo`]): the builders below stay pinned AND the
//! reads execute against SQLite.
//!
//! `PostScan`/`PostMapArgs` (`:25-58`) marshal the `image_files` JSON column
//! (TEXT in the table, `json.Unmarshal` into `model.Folder.ImageFiles`,
//! `"[]"` for a nil slice). The write path is the record-driven
//! [`put_sql`] (S-D2b): [`FolderRecord`] → [`folder_row`] (the
//! `structs.Map(dbFolder)` + `dbFolder.PostMapArgs` map) → the shared
//! [`crate::marshal::MarshalledRow`] seam — no caller-supplied column
//! arrays for the write path.
//!
//! `GetTouchedWithPlaylists`/`GetAllWithPlaylists` (`:276-299`) return a Go
//! `FolderCursor` over `queryWithStableResults`
//! (`sql_base_repository.go:411-439`), which yields `(Folder, error)` per row and
//! STOPS at the first scan failure. nd-db has no streaming seam — `execution` is
//! `query_optional`/`query_all` only, and the album, mediafile and playlist-track
//! lanes each leave their cursor to a later slice — so [`FolderRepo`] ports the
//! FULL-DRAIN observable as [`CursorDrain`]: the rows scanned before the first
//! failure plus that failure, which is what a drained Go cursor yields (executed,
//! `tests/runtime_hydrate_folder_artwork_queue.rs`). The lazy half — a caller that
//! stops mid-cursor and Go's failed-row value yielded BESIDE a scan/PostScan error
//! (`wrapCursor`, `:392-406`) — is NOT ported. A raw Scan error may leave a zero
//! `dbFolder`, but a PostScan error returns the already-scanned row (for example
//! ID `broken`); do not describe both failure kinds as yielding a zero `Folder`.
//!
//! `GetFolderUpdateInfo` (`:105-132`) batches target paths at
//! [`BATCH_SIZE`] (`slices.Chunk` at `:120-123`; the comment at `:118-119` says
//! each path adds ~3 conditions and SQLite rejects expression trees deeper
//! than 1000); a path of `""`/`"."` selects the whole library (`:111-116` →
//! [`get_folder_update_info_all_sql`]). The map is keyed by FOLDER ID
//! (`m[f.ID]`, `:199`), not by path.
//!
//! Two `getFolderUpdateInfoBatch` divergences from squirrel were FIXED in
//! S-D3g2, each backed by an executed SQLite before/after row set (recorded in
//! `docs/rust-port/hydrate-contract.md` §S-D3g2): the id list and the per-path OR
//! were ANDed where Go nests them (`Or{Eq{"id": folderIDs}, pathConditions}`,
//! `:169`), and the descendant arm was rendered as an `ESCAPE '\'` pattern where
//! Go emits a plain `LIKE` (`Like{"path": cleanPath + "/%"}`, `:164`).
//! [`has_audio_outside_folders_sql`] gained the same two classes of fix (see it).
//!
//! Safety rule (`sqlrepo.rs`): identifiers are caller-supplied names only;
//! every value — paths, ids, LIKE patterns — is bound to a `?` parameter.
//! [`escape_like_prefix`] stays load-bearing for the two call sites that spell
//! `ESCAPE '\'` themselves — `folder_subtree_filter` (`:222`) and
//! `HasAudioOutsideFolders` (`:244`) — and is deliberately NOT used by
//! [`get_folder_update_info_batch_sql`], whose arm is Go's unescaped `LIKE`.
//!
use std::collections::BTreeMap;

use crate::runtime_error::DbOperationError;
use crate::sqlrepo::{Repository, SqlError, Value};
use chrono::{DateTime, Utc};

/// `r.tableName` (`newFolderRepository`, `folder_repository.go:67-72`).
pub const FOLDER_TABLE: &str = "folder";

/// One materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// `GetFolderUpdateInfo`'s path batch size (`folder_repository.go:118-120`).
pub const BATCH_SIZE: usize = 100;

/// `MarkMissing`'s id chunk size (`slices.Chunk(ids, 200)`,
/// `folder_repository.go:263`.
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
/// `folder_repository.go:88` (`GetByPath`) and directly at `:159`, `:219`): strip the
/// library root prefix and a leading separator, clean, then hash the SINGLE key
/// `"<libID>:<cleanPath>"` (`key := fmt.Sprintf("%d:%s", lib.ID, path)`, `:50-51`).
///
/// The single-argument form is load-bearing and was a defect until S-D3g2:
/// `id.NewHash` appends the U+200B separator after EVERY argument
/// (`model/id/id.go:39-42`), so hashing the two arguments `libID` and `path`
/// produces `"7" + U+200B + "music" + U+200B` while Go hashes
/// `"7:music" + U+200B` — a different MD5 input, therefore a different id for
/// EVERY folder. Executed against the pinned Go repository (`go run -tags
/// "netgo sqlite_fts5" .`, `/tmp/sd3g2_review/go-final.out`): Go
/// `FolderID({ID:1,Path:"/music-A"}, "/music-A/rock")` is
/// `15ABnojgvDJ2bhfDjr0Ixl` and the two-argument form is
/// `7fv8RsKXA9IsKcrbFnXCjv`, i.e. exactly Go's `NewHash("1", "rock")`. A port
/// id can never match a Go-written row, so `GetByPath` reports NotFound on a real
/// database and `get_folder_update_info` omits the exact target. Fixed by using
/// [`nd_id::new_hash1`] on the colon-joined key; the constants are pinned in
/// `tests/runtime_hydrate_folder_artwork_queue.rs` rather than re-derived from this
/// helper, which previously hid the defect by seeding and asserting the same
/// wrong value.
#[must_use]
pub fn folder_id(lib_id: i64, lib_path: &str, path: &str) -> String {
    let rel = path.strip_prefix(lib_path).unwrap_or(path);
    let rel = rel.strip_prefix('/').unwrap_or(rel);
    let cleaned = clean_path(rel);
    nd_id::new_hash1(&format!("{lib_id}:{cleaned}"))
}

/// `selectFolder` (`folder_repository.go:74-78`): the base select —
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

/// `Get` (`folder_repository.go:80-85`): the base select by id.
#[must_use]
pub fn get_sql(id: &str, library_filter: Option<Sql>) -> Sql {
    select_folder(library_filter)
        .where_eq("folder.id", id)
        .to_sql()
        .unwrap_or_default()
}

/// `GetAll` (`folder_repository.go:92-97`): the base select with the
/// caller-supplied order-by expressions (already direction-carrying, per
/// `buildSortOrder` output) and `applyOptions`' Max/Offset
/// (`sql_base_repository.go:149-156`, non-positive drops the clause).
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

/// `setSortMappings` for this repository: NONE. `folderRepository` never calls it
/// (`grep -n setSortMappings persistence/folder_repository.go` is empty), so
/// `sortMapping` falls back to `toSnakeCase` for every field
/// (`sql_base_repository.go:165-172`, `:187-197`). Kept as an explicit constant so
/// the empty map is a pinned fact rather than an accident: it is why a caller sort
/// of `numAudioFiles` reaches SQLite as `num_audio_files` (executed, oracle
/// `/tmp/sd3g2_oracle9`, captured with `go run -tags "netgo sqlite_fts5" .` at
/// `log.LevelTrace` into `/tmp/g2_folder_queue_trace.txt`; the tag is required or
/// migrations fail with `no such module: fts5`).
pub const SORT_MAPPINGS: [(&str, &str); 0] = [];

/// `lookupSortMapping` over this repository's (empty) map
/// (`sql_base_repository.go:187-197`).
#[must_use]
pub fn sort_mapping(sort: &str) -> Option<&'static str> {
    SORT_MAPPINGS
        .iter()
        .find(|(key, _)| *key == sort || *key == crate::sql_restful::to_snake_case(sort))
        .map(|(_, value)| *value)
}

/// `sortMapping` (`sql_base_repository.go:165-182`): resolve each comma-separated
/// part through [`sort_mapping`], snake-casing the parts it does not know. Commas
/// inside parentheses are not separators (`splitFunc`, `:230-245`).
#[must_use]
pub fn mapped_sort(sort: &str) -> String {
    crate::sql_restful::split_sort_fields(sort, ',')
        .iter()
        .map(|part| {
            let part = part.trim();
            match sort_mapping(part) {
                Some(mapping) => mapping.to_string(),
                None => crate::sql_restful::to_snake_case(part),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `buildSortOrder` (`sql_base_repository.go:199-228`) over THIS repository's
/// mappings. Local rather than reused from another lane's copy, which resolves
/// THAT repository's map — Go's splitter is per-repository for exactly that reason
/// (the same decision is recorded in `library_repo::build_sort_order`).
#[must_use]
pub fn build_sort_order(sort: &str, order: &str) -> String {
    let order = order.trim().to_lowercase();
    let (order, reverse) = if order == "desc" {
        ("desc", "asc")
    } else {
        ("asc", "desc")
    };
    crate::sql_restful::split_sort_fields(&mapped_sort(sort), ',')
        .iter()
        .map(|part| {
            let fields = crate::sql_restful::split_sort_fields(part, ' ');
            let field = fields.first().map(String::as_str).unwrap_or("");
            let direction = match fields.get(1).map(String::as_str) {
                None | Some("asc") => order,
                Some(_) => reverse,
            };
            format!("{field} {direction}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `applyLibraryFilter` (`sql_base_repository.go:261-283`) for this repository:
/// `None` is the skip arm (admin, no logged user, or a non-admin granted every
/// library — `userSeesAllLibraries`, `:287-295`), `Some(user_id)` the subquery.
///
/// Alias contract (`playlist_track_repo.rs:177-191`, the hazard the brief names):
/// Go passes NO table name here (`applyLibraryFilter(ctx, sql)`,
/// `folder_repository.go:78`, `:101`), so it uses `r.tableName` = `folder` and the
/// executed clause names `folder.library_id`. `selectFolder` joins `library`
/// unaliased (`Join("library on library.id = folder.library_id")`, `:76`) and the
/// folder table is never aliased, so this fragment parses against every read below.
/// Executed (oracle `/tmp/sd3g2_oracle9`, saved trace `/tmp/g2_folder_queue_trace.txt`):
/// `… WHERE missing = ? AND folder.library_id IN (SELECT ul.library_id FROM
/// user_library ul WHERE ul.user_id = ?)`.
///
/// Join KIND: the library leg is squirrel's `.Join`, which renders `INNER JOIN`
/// (`squirrel@v1.5.4/select.go:297-299`); the port emits the same KIND and only the
/// bare-`JOIN` text differs (already recorded for [`select_folder`]). `user_library`
/// is a SUBQUERY here, not a join, so the `genre_repo.rs:82` LEFT-vs-INNER residual
/// does not reach any folder read.
#[must_use]
pub fn library_filter(user_id: Option<&str>) -> Option<Sql> {
    user_id.map(|uid| {
        (
            format!(
                "{FOLDER_TABLE}.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
            ),
            vec![Value::Text(uid.to_string())],
        )
    })
}

/// `GetAll` (`folder_repository.go:92-97`) under the caller's parsed options.
///
/// Go's `selectFolder(ctx, opt...)` order is `newSelect` — `applyOptions`
/// (`sql_base_repository.go:149-162`: LIMIT, OFFSET, then the `buildSortOrder` leg)
/// then `applyFilters` (`:247-252`) — with `applyLibraryFilter` LAST (`:78`), so the
/// caller's filter args bind BEFORE the library filter's user id. Executed arg order
/// `p0:false p1:lim` (oracle `/tmp/sd3g2_oracle9`, saved trace
/// `/tmp/g2_folder_queue_trace.txt`).
///
/// # Errors
/// [`SqlError`] if the builder fails to materialize.
pub fn get_all_query_sql(
    library_filter: Option<Sql>,
    options: Option<&crate::sql_restful::QueryOptions>,
) -> Result<Sql, SqlError> {
    let mut sq = select_folder(None);
    if let Some(options) = options {
        sq = crate::sql_search::apply_pagination(sq, options);
        if !options.sort.is_empty() {
            sq = sq.order_by([build_sort_order(&options.sort, &options.order)]);
        }
        sq = crate::sql_search::apply_filters(sq, options);
    }
    if let Some((sql, args)) = library_filter {
        sq = sq.where_raw(sql, args);
    }
    sq.to_sql()
}

/// `CountAll` (`folder_repository.go:99-103`) with NO options: `newSelect(ctx)`,
/// the `count(*)` column, `applyLibraryFilter`, then `count`'s rewrite.
///
/// The projection is `count`'s REWRITE rather than the `count(*)` handed to the
/// builder at `:100` — see [`count_all_query_sql`] for the full shape and its
/// executed traces. Executed (oracle `/tmp/sd3g2_oracle9`, saved trace
/// `/tmp/g2_folder_queue_trace.txt`): admin `SELECT count(distinct folder.id) as
/// count FROM folder ORDER BY folder.id` with no args; non-admin the same plus
/// `WHERE folder.library_id IN (…)`.
#[must_use]
pub fn count_all_sql(library_filter: Option<Sql>) -> Sql {
    let sq = Repository::new(FOLDER_TABLE);
    let sq = match library_filter {
        Some((sql, args)) => sq.where_raw(sql, args),
        None => sq,
    };
    sq.count(FOLDER_TABLE)
        .order_by([format!("{FOLDER_TABLE}.id")])
        .to_sql()
        .unwrap_or_default()
}

/// `CountAll` (`folder_repository.go:99-103`) under the caller's parsed options.
///
/// The projection is `count`'s REWRITE, not the `count(*)` the builder is handed at
/// `:100`: `count` applies `RemoveColumns().Columns("count(distinct folder.id) as
/// count").RemoveOffset().RemoveLimit().OrderBy("folder.id").From("folder")`
/// (`sql_base_repository.go:565-570`), so the caller's page is dropped and its sort
/// leg survives with `folder.id` appended after it (squirrel `OrderBy` appends,
/// `select.go:362-373`). `applyFilters` at `:571` gets no options here — `CountAll`
/// calls `r.count(ctx, query)` with an empty list — so the WHERE is not duplicated.
///
/// Executed (oracle `/tmp/sd3g2_oracle9`, saved trace `/tmp/g2_folder_queue_trace.txt`):
/// `CountAll(lim, Filters: Eq{missing:false}, Max:1, Offset:2, Sort:"name",
/// Order:"desc")` traces `SELECT count(distinct folder.id) as count FROM folder WHERE
/// missing = ? AND folder.library_id IN (…) ORDER BY name desc, folder.id`, and
/// plain `CountAll(adm)` traces `… FROM folder ORDER BY folder.id` with no args.
///
/// **Fixed in S-D3g2:** this leg previously rendered the `count(*)` arm Go discards.
/// That is a projection defect, not a text one — Go's `count` scans the `count`
/// alias, which `SELECT count(*) FROM folder` does not emit. The COUNT itself agrees
/// with Go on a folder table (one row per id, Go's own comment at `:100`), so the
/// executed row sets are identical; what changed is the readable projection and the
/// reset ORDER BY leg.
///
/// # Errors
/// [`SqlError`] if the builder fails to materialize.
pub fn count_all_query_sql(
    library_filter: Option<Sql>,
    options: Option<&crate::sql_restful::QueryOptions>,
) -> Result<Sql, SqlError> {
    let caller_leg = options
        .filter(|o| !o.sort.is_empty())
        .map(|o| build_sort_order(&o.sort, &o.order));
    let mut sq = Repository::new(FOLDER_TABLE);
    if let Some(options) = options {
        // Go's newSelect applies LIMIT/OFFSET and the sort leg before filters
        // (`:141-145`); count then discards the page legs (`:566-567`).
        sq = crate::sql_search::apply_pagination(sq, options);
        if !options.sort.is_empty() {
            sq = sq.order_by([build_sort_order(&options.sort, &options.order)]);
        }
        sq = crate::sql_search::apply_filters(sq, options);
    }
    if let Some((sql, args)) = library_filter {
        sq = sq.where_raw(sql, args);
    }
    sq = sq.count(FOLDER_TABLE);
    let mut legs = Vec::new();
    if let Some(leg) = caller_leg {
        legs.push(leg);
    }
    legs.push(format!("{FOLDER_TABLE}.id"));
    sq.order_by(legs).to_sql()
}

/// `queryFolderUpdateInfo`'s select (`folder_repository.go:179`), shared
/// by [`get_folder_update_info_all_sql`] and
/// [`get_folder_update_info_batch_sql`].
const UPDATE_INFO_COLUMNS: &str = "id, updated_at, hash, image_files, images_updated_at";

/// `getFolderUpdateInfoAll` (`folder_repository.go:135-141`): the
/// update-info columns for every non-missing folder of the library.
/// `image_files` is TEXT; its JSON decode (`:194-198`) is the caller's, and
/// that lane wraps failures as `parsing folder image_files:` (`:196`) — NOT
/// [`crate::hydrate::FOLDER_IMAGE_FILES_PREFIX`], which belongs to
/// `dbFolder.PostScan` (`:38-46`). Executed: a bad cell aborts the WHOLE map
/// (`return nil, err`, `:196-197`) rather than only its own row — see
/// [`FolderRepo::get_folder_update_info`].
///
/// The predicate carries squirrel's conjunction parentheses (`And` is
/// `conj.join`, `squirrel@v1.5.4/expr.go:371-386`), matching Go's executed trace
/// `… WHERE (library_id = ? AND missing = ?)` (oracle `/tmp/sd3g2_oracle2`). No
/// library-access clause is added: `queryFolderUpdateInfo` (`:179`) never calls
/// `applyLibraryFilter`, so a non-admin still gets rows of a library it cannot
/// see (executed, oracle `/tmp/sd3g2_oracle8`).
#[must_use]
pub fn get_folder_update_info_all_sql(lib_id: i64) -> Sql {
    (
        format!(
            "SELECT {UPDATE_INFO_COLUMNS} FROM {FOLDER_TABLE} \
            WHERE (library_id = ? AND missing = ?)"
        ),
        vec![Value::Integer(lib_id), Value::Bool(false)],
    )
}

/// `getFolderUpdateInfoBatch` (`folder_repository.go:144-175`) for one batch of
/// paths (`GetFolderUpdateInfo` chunks at [`BATCH_SIZE`], `:120-123`):
/// non-missing folders of the library whose id is one of the target folder ids
/// OR whose `path` equals or starts with a target path.
///
/// The shape is squirrel's, executed against the pinned library: Go builds
/// `And{Eq{library_id}, Eq{missing}, Or{Eq{"id": folderIDs}, pathConditions}}`
/// (`:145-172`), and `conj.join` wraps EVERY conjunction
/// (`squirrel@v1.5.4/expr.go:371-386`), so the id list and the per-path OR are
/// **nested by OR inside one group** — the executed trace is
/// `… WHERE (library_id = ? AND missing = ? AND (id IN (?, ?) OR (path = ? OR path LIKE ? …)))`
/// (oracle `/tmp/sd3g2_oracle2`).
///
/// **Fixed in S-D3g2 (row-set defect, not text):** the previous form ANDed the
/// two groups (`… AND (id IN (…)) AND (path = ? OR …)`), so a folder whose id IS
/// the target's `FolderID` but whose stored `path` is unrelated dropped out.
/// Executed in SQLite over a fixture folder `target-id-only` (id = the target
/// path's `FolderID`, `path = '/music-A'`): the nested form returns 1 row
/// (`[target-id-only]`, = Go's result) and the flat-AND form returns 0
/// (`[]`) (oracle `/tmp/sd3g2_oracle7`).
///
/// **Also fixed in S-D3g2:** the descendant arm is Go's plain `LIKE` with NO
/// `ESCAPE` clause and the UNescaped pattern (`Like{"path": cleanPath + "/%"}`,
/// `:164`; squirrel's `Like.toSql` renders `%s LIKE ?` with no escape and appends
/// the raw value, `expr.go:214-247`). The previous arm was
/// `path LIKE ? ESCAPE '\'` over `escape_like_prefix(clean) + "/%"`, which is a
/// second row-set defect: executed in SQLite against a stored path containing a
/// literal `%`, Go's `LIKE 'a%b/%'` matches the descendant (`[deep-child]`) while
/// the escaped form matches nothing (`[]`); identically for a literal `_`
/// (`LIKE 'a_b/%'` → `[deep-child]`, `LIKE 'a\_b/%' ESCAPE '\'` → `[]`). The
/// pattern now binds raw, which is also Go's arg order and text (`p4:a%b/%`).
///
/// Args bind in fragment order (library_id, missing, ids, then per path:
/// equality, LIKE prefix). `clean_path` mirrors `path.Clean(TrimPrefix(p, "/"))`
/// (`:154-156`) and the ids come from [`folder_id`] (`model.FolderID`, called at
/// `:159`).
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
        // Go's `Like` arm (`:164`): plain LIKE, no ESCAPE, pattern NOT escaped.
        or_clauses.push("path LIKE ?".to_string());
        or_args.push(Value::from(format!("{clean}/%")));
    }
    let ids_placeholders = vec!["?"; id_args.len()].join(", ");
    let mut args = vec![Value::Integer(lib_id), Value::Bool(false)];
    args.extend(id_args);
    args.extend(or_args);
    (
        format!(
            "SELECT {UPDATE_INFO_COLUMNS} FROM {FOLDER_TABLE} \
            WHERE (library_id = ? AND missing = ? AND \
            (id IN ({ids_placeholders}) OR ({})))",
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
/// cleans to `"."` selects the whole library and returns a FRESH clause: the
/// path conditions AND the args accumulated so far are both dropped, wherever
/// the root path sits in the batch (`:214-216`). The fragment is caller-bound
/// (Go composes it inside
/// `Exists(...)` joins, `album_repository.go:291-293`).
#[must_use]
pub fn folder_subtree_filter(lib_id: i64, paths: &[String]) -> Sql {
    // Go builds `conds` as it goes and RETURNS the fresh
    // `And{Eq{library_id}, Eq{missing}}}` the moment a path cleans to "."
    // (`folder_repository.go:214-216`), so earlier paths' conditions AND args are
    // dropped. Args are therefore appended only AFTER the `.` check, per path:
    // pre-building them would hand the whole-library clause stale extra args and
    // every caller would fail binding with InvalidParameterCount.
    let mut args: Vec<Value> = Vec::new();
    let mut or_clauses: Vec<String> = Vec::new();
    for p in paths {
        let clean = clean_path(p.strip_prefix('/').unwrap_or(p));
        if clean == "." {
            return (
                "(folder.library_id = ? AND folder.missing = ?)".to_string(),
                vec![Value::Integer(lib_id), Value::Bool(false)],
            );
        }
        if args.is_empty() {
            args.push(Value::Integer(lib_id));
            args.push(Value::Bool(false));
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

/// `HasAudioOutsideFolders` (`folder_repository.go:232-247`): any non-missing
/// folder in `parent`'s subtree, other than the excluded ids, that holds audio
/// files. `parent_path` is Go's
/// `TrimPrefix(path.Join(parent.Path, parent.Name), "/")` (`:236`); the
/// `parent.NumAudioFiles > 0` fast path (`:233-235`) is a caller-side check and
/// produces no SQL. Squirrel sorts `Eq` map keys before rendering
/// (`squirrel@v1.5.4/expr.go:161`), so this shape is deterministic: `library_id`
/// precedes `missing`. The port pins that same order explicitly.
///
/// The statement is `exists()`'s shape (`sql_base_repository.go:488-493`):
/// `SELECT count(*) as exist FROM folder WHERE (…)`, the executed trace in oracle
/// `/tmp/sd3g2_oracle8`. The earlier `SELECT 1 … LIMIT 1` form agreed on truthiness
/// but not on the projection Go's `exists` reads.
///
/// Two squirrel defects FIXED in S-D3g2, both row-set-affecting and both executed
/// (oracle `/tmp/sd3g2_oracle8`, folders `l1-a`/`l1-b` holding audio, `l1-m`
/// missing, `l2-a`/`l2-b` in library 2):
/// * `NotEq{"id": excludeFolderIDs}` with an EMPTY list takes squirrel's negated
///   `inEmptyExpr`, which is `sqlTrue` = `(1=1)` (`squirrel@v1.5.4/expr.go:156-158`),
///   NOT a match-nothing arm. The port emitted `1 = 0`, so Go's nil exclusion slice
///   got `false` where Go gets `true` (see the empty-arm comment below).
/// * `Gt{"num_audio_files": 0}` (`:239`) binds the map's value, 0
///   (`squirrel@v1.5.4/expr.go:228-262`). The port bound `1`, so a subtree whose
///   only audio-bearing folder holds exactly 1 file read `false` here and `true`
///   in Go. Executed against `num_audio_files = 1`: bound to 0 the row matches
///   (Go's answer); bound to 1 it does not. Fixed to bind 0.
///
/// The subtree arm IS Go's explicit `Expr(… ESCAPE '\')` (`:244`), so
/// [`escape_like_prefix`] stays correct on this path — unlike the update-info
/// batch, which emits Go's unescaped `LIKE`.
#[must_use]
pub fn has_audio_outside_folders_sql(
    library_id: i64,
    parent_path: &str,
    exclude_folder_ids: &[String],
) -> Sql {
    let mut sql = format!(
        "SELECT count(*) as exist FROM {FOLDER_TABLE} \
        WHERE (library_id = ? AND missing = ? AND num_audio_files > ? AND "
    );
    let mut args = vec![
        Value::Integer(library_id),
        Value::Bool(false),
        Value::Integer(0),
    ];
    if exclude_folder_ids.is_empty() {
        // Squirrel's empty NotEq renders the negated form's `inEmptyExpr`, which
        // is sqlTrue (`squirrel@v1.5.4/expr.go:156-158`): an empty exclusion list
        // excludes nothing. The previous `1 = 0` made Go's nil-slice caller —
        // `core/artwork/folders_album.go:94` — read false where Go reads true.
        sql.push_str("(1=1)");
    } else {
        let placeholders = vec!["?"; exclude_folder_ids.len()].join(", ");
        sql.push_str(&format!("id NOT IN ({placeholders})"));
        args.extend(exclude_folder_ids.iter().map(|id| Value::from(id.as_str())));
    }
    sql.push_str(" AND (path = ? OR path LIKE ? ESCAPE '\\'))");
    args.push(Value::from(parent_path));
    args.push(Value::from(escape_like_prefix(parent_path) + "/%"));
    (sql, args)
}

/// `GetByPath` (`folder_repository.go:87-90`): `model.NewFolder(lib, path).ID`
/// fed to [`get_sql`]. Go builds a whole `Folder` only to read its `ID`
/// (`model/folder.go:54-73` → `FolderID`, `:46-52`): strip the library root prefix
/// and a leading separator, clean, then hash `"<libID>:<cleanPath>"` — the same
/// rule [`folder_id`] implements, so the ids agree exactly. `NewFolder`'s other
/// output (`Path`/`Name`/`ParentID`) is discarded on this path, so the port does
/// not duplicate the split/parent step.
#[must_use]
pub fn get_by_path_sql(
    lib_id: i64,
    lib_path: &str,
    path: &str,
    library_filter: Option<Sql>,
) -> Sql {
    get_sql(&folder_id(lib_id, lib_path, path), library_filter)
}

/// `model.Folder`'s persisted row (`model/folder.go:18-33`) — the 12
/// non-id persisted folder columns (the contract's 13 minus `id`, which
/// the seam forbids in the map
/// (`crate::marshal::MarshalError::IdInMap`); the id is appended last by
/// [`put_sql`]. Field → column: `LibraryID`→`library_id`, `Path`→`path`,
/// `Name`→`name`, `ParentID`→`parent_id`, `NumAudioFiles`→`num_audio_files`,
/// `NumPlaylists`→`num_playlists`, `ImageFiles`→`image_files` (JSON, nil→
/// `"[]"`), `ImagesUpdatedAt`→`images_updated_at`, `Hash`→`hash`,
/// `Missing`→`missing`, **`UpdateAt`→`updated_at`** (Go's field name is
/// `UpdateAt`; the `structs` tag is `updated_at`),
/// `CreatedAt`→`created_at`. Non-persisted: `LibraryPath`
/// (`structs:"-"`, join-only).
///
/// Timestamps are `DateTime<Utc>` bound through `crate::time` (explicit
/// `'%F %T%.f%:z'` TEXT; the DDL's localtime defaults are never relied on).
/// `created_at` is `None` for a never-stamped row (Go zero `time.Time`);
/// [`put_sql`] stamps it like Go's `NewFolder` (`folder.go:65-73`).
///
/// There is deliberately no `Default`: a write record must name every persisted
/// column, so the hydration-owned fields can only arrive through the read path.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderRecord {
    /// The row's `id` (Go `Folder.ID`, `structs:"id"`). Read-path identity:
    /// [`crate::hydrate::row_to_folder`] fills it from the row, so a bulk read
    /// keeps every row's own id. [`folder_row`] does NOT emit it — the id is
    /// appended by the repo's write legs ([`put_sql`] takes it as its own
    /// argument, matching Go's `put(ctx, id, …)`, `sql_base_repository.go:618`).
    pub id: String,
    /// `library_id` (Go `LibraryID`) — FK to `library`.
    pub library_id: i64,
    /// `path` (Go `Path`) — library-relative directory.
    pub path: String,
    /// `name` (Go `Name`).
    pub name: String,
    /// `parent_id` (Go `ParentID`).
    pub parent_id: String,
    /// `num_audio_files` (Go `NumAudioFiles`).
    pub num_audio_files: i64,
    /// `num_playlists` (Go `NumPlaylists`).
    pub num_playlists: i64,
    /// `image_files` (Go `ImageFiles []string`); `None` (Go nil) stores the
    /// literal `"[]"` via `dbFolder.PostMapArgs` (`:48-59`) — never `null`.
    pub image_files: Option<Vec<String>>,
    /// `images_updated_at` (Go `ImagesUpdatedAt`).
    pub images_updated_at: DateTime<Utc>,
    /// `hash` (Go `Hash`).
    pub hash: String,
    /// `missing` (Go `Missing`).
    pub missing: bool,
    /// `updated_at` (Go **`UpdateAt`**, `structs:"updated_at"`).
    ///
    /// Asymmetry inherited from Go: the WRITE side emits `updated_at` (the
    /// `structs` tag governs `structs.Map`), but the READ side cannot hydrate
    /// it — `ScanStruct` maps by field name, `UpdateAt` → `update_at`, and no
    /// SELECT emits `update_at`, so Go's `Folder.UpdateAt` is always the zero
    /// `time.Time` ([`crate::hydrate::go_zero_time`]). Do not "fix" the read by
    /// reading the column: that would diverge from Go.
    pub updated_at: DateTime<Utc>,
    /// `created_at` (Go `CreatedAt`); `None` = never stamped (Go zero
    /// `time.Time`), stamped to `now` by [`put_sql`].
    pub created_at: Option<DateTime<Utc>>,
    /// The select's `library.path as library_path` (`folder_repository.go:75`),
    /// **join-only**: Go's `Folder.LibraryPath` is `structs:"-"`
    /// (`model/folder.go:21`), so [`folder_row`] deliberately does NOT emit it
    /// and the write SQL is unchanged. Read onto the record by
    /// [`crate::hydrate::row_to_folder`]; only the library's `path` is selected
    /// (`library.name` is not).
    pub library_path: String,
}

/// `structs.Map(dbFolder)` + `dbFolder.PostMapArgs` (`folder_repository.go`
/// `:48-59`) as one sorted [`crate::marshal::ColumnMap`]: exactly the 12
/// non-id persisted folder columns (the 13th, `id`, is appended by the
/// repo), with `image_files`
/// PostMapArgs-overwritten by its marshalled JSON (Go's `"[]"` nil arm is
/// byte-identical to `crate::sql_json::marshal_image_files(None)`, so the
/// seam's transformation is the single implementation of both arms).
///
/// `id` never appears in the map — the seam rejects it.
#[must_use]
pub fn folder_row(f: &FolderRecord) -> crate::marshal::ColumnMap {
    let mut map = BTreeMap::new();
    map.insert(
        "created_at".to_string(),
        crate::time::optional_value(f.created_at.as_ref()),
    );
    map.insert("hash".to_string(), Value::Text(f.hash.clone()));
    map.insert(
        "image_files".to_string(),
        Value::Text(crate::sql_json::marshal_image_files(
            f.image_files.as_deref(),
        )),
    );
    map.insert(
        "images_updated_at".to_string(),
        crate::time::value(&f.images_updated_at),
    );
    map.insert("library_id".to_string(), Value::Integer(f.library_id));
    map.insert("missing".to_string(), Value::Bool(f.missing));
    map.insert("name".to_string(), Value::Text(f.name.clone()));
    map.insert(
        "num_audio_files".to_string(),
        Value::Integer(f.num_audio_files),
    );
    map.insert("num_playlists".to_string(), Value::Integer(f.num_playlists));
    map.insert("parent_id".to_string(), Value::Text(f.parent_id.clone()));
    map.insert("path".to_string(), Value::Text(f.path.clone()));
    map.insert("updated_at".to_string(), crate::time::value(&f.updated_at));
    // PostMapArgs overwrites image_files (identity here — same codec Go's
    // :50/:56 arms run); routed through the seam so the folder's
    // PostMapArgs lives in exactly one place.
    crate::marshal::folder_post_map_args(
        crate::marshal::ColumnMap::new(map),
        crate::marshal::FolderDerived {
            image_files: f.image_files.clone(),
        },
    )
}

/// `dbFolder` raw row (phase 1 of the two-phase hydration seam,
/// `persistence/folder_repository.go:25-28`): the columns `selectFolder`'
/// `folder.*` + `library.path as library_path` scan into, with the
/// `image_files` JSON shim **still unparsed** (it is a non-pointer `string`
/// on the raw struct, exactly like Go's `dbFolder.ImageFiles`). Phase 2 is
/// [`crate::hydrate::folder_post_scan`], which decodes it onto the record.
///
/// The `library_path` join column is read straight onto the record (Go
/// scans it into `Folder.LibraryPath`, `structs:"-"`); it is not duplicated
/// here.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderRow {
    /// The folder columns scanned from the row, `image_files` still empty.
    pub record: FolderRecord,
    /// The raw `image_files` TEXT column, unparsed (Go's `dbFolder.ImageFiles`,
    /// `folder_repository.go:27`). Empty means the shim was not scanned: Go's
    /// `PostScan` guard (`:40`) skips the decode and leaves `ImageFiles` nil.
    pub image_files: String,
}

impl FolderRow {
    /// `dbFolder.PostScan` (`folder_repository.go:38-46`): thin delegation onto
    /// [`crate::hydrate::folder_post_scan`], surfaced the way Go's `ScanStruct`
    /// error leaves the read (a sqlite conversion failure carrying Go's prefix).
    ///
    /// # Errors
    /// [`crate::hydrate::HydrateError::Decoded`] carrying
    /// [`crate::hydrate::FOLDER_IMAGE_FILES_PREFIX`] + the decode failure — the
    /// same shape Go's `ScanStruct` error takes out of the read.
    pub fn into_record(self) -> Result<FolderRecord, crate::hydrate::HydrateError> {
        crate::hydrate::folder_post_scan(self)
    }
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
///
/// The record is the only write input (S-D2b — no caller-supplied column
/// arrays): it stamps the row like Go's `NewFolder`/`Put` (`model/folder.go`
/// `:65-73`) — `created_at` to `now` when the record carries none,
/// `updated_at` to `now` — and renders both legs through the shared
/// [`crate::marshal::MarshalledRow`] from the [`folder_row`] map. The
/// UPDATE leg is the `selectUpdateColumns`-filtered set (no `created_at`;
/// `id` binds last via `WHERE id = ?`), the INSERT leg the full sorted
/// columns with `id` last in both lists
/// (`sql_base_repository.go:637-641`). Go's `put` runs the UPDATE first and
/// only INSERTs on a 0-row match; the caller picks the leg.
///
/// `cols_to_update`: optional `colsToUpdate`
/// (`sql_base_repository.go:595-601`); empty applies no restriction. The
/// entries are `toSnakeCase`-normalized by the seam
/// ([`crate::marshal::to_snake_case`]), so camelCase names such as
/// `"UpdatedAt"` behave like Go's. An update set empty after the
/// `created_at`/`birth_time` drop is
/// [`crate::marshal::MarshalError::EmptyUpdateSet`], not degenerate SQL.
///
/// # Errors
///
/// Returns [`crate::marshal::MarshalError`] when the map carries `id`
/// (impossible from [`folder_row`]) or the UPDATE set is empty.
///
/// **Deliberate divergence — `updated_at` is always re-stamped.** Go's
/// `folderRepository.Put` (`folder_repository.go:255-259`) writes the caller's
/// `UpdateAt` unchanged: the stamp happens at entry-construction time
/// (`scanner/phase_1_folders.go:389` → `toFolder` → `model.NewFolder`,
/// `folder_entry.go:94` → `model/folder.go:72`), so a re-scan can commit a
/// `updated_at` slightly older than commit time. This seam takes `now` and
/// always stamps it, keeping the render-time value explicit instead of relying
/// on a caller-side stamp. The drift is bounded by the scan interval, and the
/// only consumer (`GetTouchedWithPlaylists`, `folder_repository.go:276-287`)
/// compares against `library.last_scan_at`. A future wiring slice can pass the
/// caller's `UpdateAt` in as `now`.
pub fn put_sql(
    id: &str,
    f: &FolderRecord,
    now: DateTime<Utc>,
    cols_to_update: &[&str],
) -> Result<PutFolderSql, crate::marshal::MarshalError> {
    let stamped = FolderRecord {
        created_at: f.created_at.or(Some(now)),
        updated_at: now,
        ..f.clone()
    };
    let map = folder_row(&stamped);
    let row = crate::marshal::MarshalledRow::new(FOLDER_TABLE, id, &map, cols_to_update)?;
    Ok(PutFolderSql {
        update: (row.update_sql, row.update_args),
        insert: (row.insert_sql, row.insert_args),
    })
}

/// The S-D2a base-map boundary, retained so the caller-supplied
/// [`crate::marshal::ColumnMap`] seam stays exercisable (S-D2a's
/// `tests/runtime_marshal.rs` drives it): a [`crate::marshal::MarshalledRow`]
/// from the PostMapArgs-applied map (see
/// [`crate::marshal::folder_post_map_args`] — the only derived key is
/// `image_files`, nil→`"[]"`). Same rendering contract as the record-driven
/// [`put_sql`] write path: UPDATE leg = the `selectUpdateColumns`-filtered
/// set (no `created_at`; `id` binds last via `WHERE id = ?`), INSERT leg =
/// the full sorted list with `id` last in both.
///
/// `cols_to_update`: optional `colsToUpdate` (`sql_base_repository.go:595-601`);
/// empty applies no restriction. An update set empty after the
/// `created_at`/`birth_time` drop is a [`crate::marshal::MarshalError`].
#[derive(Debug, Clone, PartialEq)]
pub struct PutFolderMarshalled {
    /// The `r.put` update leg (filtered columns, `id` last in args).
    pub update: Sql,
    /// The `r.put` insert leg (sorted columns, `id` last in both).
    pub insert: Sql,
}

/// See [`PutFolderMarshalled`].
pub fn put_marshalled_sql(
    id: &str,
    map: &crate::marshal::ColumnMap,
    cols_to_update: &[&str],
) -> Result<PutFolderMarshalled, crate::marshal::MarshalError> {
    let row = crate::marshal::MarshalledRow::new(FOLDER_TABLE, id, map, cols_to_update)?;
    Ok(PutFolderMarshalled {
        update: (row.update_sql, row.update_args),
        insert: (row.insert_sql, row.insert_args),
    })
}

/// `MarkMissing` (`folder_repository.go:261-274`): one UPDATE per 200-id
/// chunk (`slices.Chunk(ids, 200)`, `:263`), setting `missing` and
/// `updated_at` (the caller binds the timestamp).
#[must_use]
pub fn mark_missing_sql(missing: bool, ids: &[String], updated_at: &DateTime<Utc>) -> Vec<Sql> {
    ids.chunks(MARK_MISSING_CHUNK_SIZE)
        .map(|chunk| {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let mut args = vec![Value::Bool(missing), crate::time::value(updated_at)];
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

/// Callback error of the executed folder [`put_folder_in_tx`].
///
/// The builder arm is REAL and must stay separate from the SQLite arm: [`put_sql`]
/// returns [`crate::marshal::MarshalError`], so a failure to materialize the UPDATE/INSERT
/// legs is a domain failure, not a SQLite one. An `Infallible` domain type would make
/// `Domain` uninhabited and force a builder failure onto the [`DbOperationError::Sqlite`]
/// arm, which `D-ERROR` forbids: the concrete cause would be lost and `store::is_busy`
/// would be asked to classify something that never was a driver failure.
#[derive(Debug, thiserror::Error)]
pub enum FolderWriteError {
    /// The statement could not be materialized. The only reachable failure from
    /// [`put_sql`] today is [`crate::marshal::MarshalError::EmptyUpdateSet`] — a `cols`
    /// selection that leaves nothing settable after the `created_at`/`birth_time` drop
    /// (e.g. `&["created_at"]`). Go has NO such error: its `filterUpdateValues`
    /// (`sql_base_repository.go:612-616`) re-adds `id`, so Go renders the no-op
    /// `UPDATE folder SET id = ? WHERE id = ?`. The port omits that no-op SET (id binds
    /// only in `WHERE`) and errors instead — the typed error is the port's contract, not
    /// a Go error. See [`crate::marshal::MarshalError::EmptyUpdateSet`].
    #[error(transparent)]
    Builder(#[from] crate::marshal::MarshalError),

    /// [`FolderRecord::id`] is empty, so there is nothing to write. **DOCUMENTED
    /// DEVIATION, not exact Go parity:** Go's `put` fills an empty id with
    /// `id2.NewRandom()` (`sql_base_repository.go:635-638`) and SKIPS the UPDATE leg
    /// entirely, returning the generated id with no error. This port refuses instead,
    /// because folder identity comes from SC ([`folder_id`], `model/folder.go:44-51`) and
    /// a DB-side id generator would be an invented policy nobody asked for. Accepted under
    /// the supplied-SC-ID contract (parent decision); Go has no equivalent error here, so
    /// do NOT describe this arm as Go-exact.
    #[error("folder record has an empty id: the id comes from SC and is never generated here")]
    EmptyId,
}

/// Callback error of the executed folder [`mark_folders_missing_in_tx`], which has NO
/// domain failure to carry: Go's `MarkMissing` returns a bare `error` that can only be
/// a driver failure (`folder_repository.go:268-271`) — no validation, no `NotFound`, no
/// permission check. `Infallible` makes the `Domain` arm structurally uninhabited rather
/// than reserved, as in `library_repo::LibraryScanError` and `tag_repo::TagWriteError`,
/// and keeps every SQLite failure on the [`DbOperationError::Sqlite`] arm so
/// `store::is_busy` still classifies `SQLITE_BUSY` for the executor's retry loop.
pub type FolderMarkError = DbOperationError<std::convert::Infallible>;

/// What a successful [`put_folder_in_tx`] reports.
///
/// Both fields are ADVISORY until the caller commits: a rolled-back Put publishes
/// nothing. Go's `folderRepository` keeps no cache either, so unlike
/// `library_repo::put_in_tx` there is nothing here to invalidate on rollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderWriteResult {
    /// The written row's id: [`FolderRecord::id`], which is always non-empty here.
    /// Go's `put` returns `(newId, err)` (`sql_base_repository.go:618-642`) and
    /// `folderRepository.Put` DISCARDS it (`folder_repository.go:255-259`); the port
    /// reports it because DB-09/10 need it to order junction writes.
    pub id: String,
    /// `true` when the INSERT leg ran rather than the UPDATE leg, i.e. when the UPDATE
    /// matched zero rows (`sql_base_repository.go:630-641`).
    pub inserted: bool,
}

/// `Put` (`folder_repository.go:255-259`) EXECUTED on the CALLER's transaction: Go's
/// `r.put(ctx, dbf.ID, &dbf)` upsert (`sql_base_repository.go:618-642`) in Go's own
/// statement order — UPDATE by `id` FIRST, and the INSERT only when that matched zero
/// rows. A zero-row UPDATE is the insert fallback (`:630-641`, the new record with a
/// predefined id), NOT a miss and never a `NotFound`.
///
/// Identity: the id is the caller's — `record.id` is the UPDATE's `WHERE id = ?` and
/// the INSERT's id column. This function never recomputes it from the path
/// ([`folder_id`] is the authority for `NewFolder`/`GetByPath`, `model/folder.go:44-51`,
/// and Go's `Put` never calls it) and invents no random-id arm. Go fills an empty id
/// with `id2.NewRandom()` (`:635-638`), but a folder id always arrives from SC, so an
/// empty `record.id` is refused as a typed contract violation BEFORE any statement runs
/// rather than silently repaired — DB-side identity generation is not this port's. That
/// refusal is a **port-side deviation from Go** (Go generates an id and succeeds), not a
/// Go error; see [`FolderWriteError::EmptyId`].
///
/// Timestamps: `caller_scan_stamp` is Go's `UpdateAt`, sampled by the caller when it
/// built the entry (`scanner/folder_entry.go:94-103` → `model.NewFolder`,
/// `model/folder.go:65-74`) and NEVER the commit clock — the DB-07 plan step 5
/// (`docs/rust-port/subplans/db-runtime.md:265`) requires the supplied caller stamp.
/// step 5 forbids commit-time stamping here. It reaches [`put_sql`] as its `now`, which
/// stamps `updated_at` on both legs and fills `created_at` ONLY when the record carries
/// none (`[`put_sql`] above, lines 840-857`); `created_at` stays immutable on the UPDATE leg because
/// `selectUpdateColumns` drops it (`sql_base_repository.go:593-610`; the port's
/// `marshal::UPDATE_DROP` list). The record is borrowed and never mutated — the
/// stamps travel as bound values.
///
/// Scope: a caller-trusted primitive. `WHERE id = ?` and nothing else, exactly like
/// Go: no library predicate, no auth check, no ownership guard, so a caller that passes
/// an id belonging to another library HAS it written — future SC typed authority
/// (`docs/rust-port/subplans/db-runtime.md:264`) owns that, not this wrapper.
///
/// The transaction is the caller's: this never begins, commits, retries, or runs in
/// autocommit, and there is no connection-bound variant.
///
/// `cols` is the legacy `&[&str]` seam contract (Go's `Put` passes no `colsToUpdate`
/// today, `:255-259`): entries are `to_snake_case`-normalized by
/// [`crate::marshal::MarshalledRow`], so `"UpdatedAt"` behaves like Go's camelCase entry.
/// An empty selection applies no restriction. No typed folder column enum is introduced
/// here — the shared marshal seam already owns that contract and this lane does not
/// change it.
///
/// # Errors
/// [`DbOperationError::Domain`]·[`FolderWriteError::EmptyId`] for an empty
/// [`FolderRecord::id`], and [`DbOperationError::Domain`]·[`FolderWriteError::Builder`]
/// when the selection resolves to no settable column; [`DbOperationError::Sqlite`] carrying the concrete
/// `rusqlite::Error` (extended code intact) from the UPDATE or the INSERT — the
/// `folder.library_id` FK (`nd-dbschema/src/schema_ddl.rs:61`) included, which Go
/// returns raw and unwrapped (`:257-258` adds no `fmt.Errorf`). There is no UNIQUE
/// constraint on `folder`, so a repeated id is an UPDATE hit rather than a constraint
/// failure; a zero-row UPDATE is NOT an error.
pub fn put_folder_in_tx(
    tx: &rusqlite::Transaction<'_>,
    record: &FolderRecord,
    caller_scan_stamp: &DateTime<Utc>,
    cols: &[&str],
) -> Result<FolderWriteResult, DbOperationError<FolderWriteError>> {
    if record.id.is_empty() {
        // Go's `id == ""` arm generates a random id (`:635-638`). The port refuses
        // instead: folder identity belongs to SC, and inventing a DB-side id would be a
        // policy nobody asked for.
        return Err(DbOperationError::Domain(FolderWriteError::EmptyId));
    }
    let statements = put_sql(&record.id, record, *caller_scan_stamp, cols)
        .map_err(|err| DbOperationError::Domain(FolderWriteError::Builder(err)))?;
    // UPDATE first (:624-633): a row count > 0 ends the Put, so the INSERT never runs.
    if crate::execution::execute(tx, &statements.update).map_err(DbOperationError::sqlite)? > 0 {
        return Ok(FolderWriteResult {
            id: record.id.clone(),
            inserted: false,
        });
    }
    // INSERT fallback (:634-641).
    crate::execution::execute(tx, &statements.insert).map_err(DbOperationError::sqlite)?;
    Ok(FolderWriteResult {
        id: record.id.clone(),
        inserted: true,
    })
}

/// `MarkMissing` (`folder_repository.go:261-274`) EXECUTED on the CALLER's
/// transaction: one UPDATE per [`MARK_MISSING_CHUNK_SIZE`] ids in Go's loop order over
/// the CALLER's id order (`slices.Chunk(ids, 200)`, `:263`), executing exactly what
/// [`mark_missing_sql`] renders rather than re-rendering it here. Empty ids run NO
/// statement at all and return `Ok(0)`.
///
/// Go checks `err` only (`:268-271`) and DISCARDS the row count, so a chunk matching
/// zero rows — every unknown id — is a successful NO-OP and the loop CONTINUES to the
/// next chunk. Contrast [`crate::mediafile_repo::mark_media_missing_in_tx`], whose Go
/// counterpart aborts the loop on a zero-row chunk; that asymmetry is Go's own and is
/// preserved rather than unified. The total is returned for the caller's observability
/// only (nd-db has no logger, so Go's `log.Debug` at `:262` is not ported); no policy
/// rides on it.
///
/// `updated_at` is the caller's stamp, bound where Go binds `Set("updated_at",
/// time.Now())` (`:266`) — the caller owns the clock, this function never reads one.
///
/// Scope: caller-trusted. `WHERE id IN (chunk)` and nothing else, exactly like Go
/// (`:267` is `Where(Eq{"id": chunk})` and there is no library predicate): ids of
/// another library in the input ARE updated, and a decoy id only stays untouched
/// because it was not supplied.
///
/// # Errors
/// [`DbOperationError::Sqlite`] carrying the concrete `rusqlite::Error` from any chunk;
/// [`DbOperationError::Domain`] is structurally uninhabited ([`FolderMarkError`] has no
/// domain arm). A failed chunk fails the whole call, and because the chunks share the
/// caller's transaction the ones that already ran roll back.
pub fn mark_folders_missing_in_tx(
    tx: &rusqlite::Transaction<'_>,
    missing: bool,
    ids: &[String],
    updated_at: &DateTime<Utc>,
) -> Result<usize, FolderMarkError> {
    let mut total = 0usize;
    for statement in mark_missing_sql(missing, ids, updated_at) {
        total += crate::execution::execute(tx, &statement).map_err(DbOperationError::sqlite)?;
    }
    Ok(total)
}

/// The `And{Eq{missing}, Gt{num_playlists}}` arm shared by the two playlist
/// cursors (`folder_repository.go:277-281`, `:290-293`). squirrel renders BOTH
/// columns UNQUALIFIED (`Eq.toSql` emits the map key verbatim,
/// `squirrel@v1.5.4/expr.go:200-203`) and wraps the conjunction in parentheses
/// (`conj.join`, `:371-386`), so the group is `(missing = ? AND num_playlists > ?)`.
/// The library filter is a separate clause and comes FIRST, which is why the
/// executed form is `WHERE folder.library_id IN (…) AND (missing = ? AND …)`
/// (oracle `/tmp/sd3g2_oracle9`, saved trace `/tmp/g2_folder_queue_trace.txt`).
/// The touched arm's `ConcatExpr` joins that SAME group rather than becoming a
/// separate clause, so the executed text is
/// `WHERE (missing = ? AND num_playlists > ? AND folder.updated_at >
/// library.last_scan_at)` (oracle `/tmp/sd3g2_oracle9`, saved trace
/// `/tmp/g2_folder_queue_trace.txt`). The library filter is a clause of
/// `selectFolder` (`:74-78`) and is ANDed BEFORE the group, giving the non-admin
/// `WHERE folder.library_id IN (…) AND (missing = ? AND num_playlists > ?)`.
/// The S-D3g1 rendering qualified both columns and emitted no group at all; that
/// was a text-only divergence — the row set is identical, since no other joined
/// table has `missing` or `num_playlists` — now corrected to Go's text.
fn playlists_where(touched: bool) -> Sql {
    let sql = if touched {
        "(missing = ? AND num_playlists > ? AND folder.updated_at > library.last_scan_at)"
    } else {
        "(missing = ? AND num_playlists > ?)"
    };
    (sql.to_string(), vec![Value::Bool(false), Value::Integer(0)])
}

/// `GetTouchedWithPlaylists` (`folder_repository.go:276-287`): non-missing
/// folders with playlists touched since the library's last scan. The third arm is
/// Go's `ConcatExpr("folder.updated_at > library.last_scan_at")` (`:280`), a raw
/// fragment with no bound value.
///
/// Executed trace (oracle `/tmp/sd3g2_oracle9`, `/tmp/g2_folder_queue_trace.txt`):
/// `… WHERE (missing = ? AND num_playlists > ? AND folder.updated_at >
/// library.last_scan_at)`, and for a non-admin
/// `… WHERE folder.library_id IN (…) AND (missing = ? AND num_playlists > ? AND …)`.
#[must_use]
pub fn get_touched_with_playlists_sql(library_filter: Option<Sql>) -> Sql {
    let (sql, args) = playlists_where(true);
    select_folder(library_filter)
        .where_raw(sql, args)
        .to_sql()
        .unwrap_or_default()
}

/// `GetAllWithPlaylists` (`folder_repository.go:289-299`): non-missing folders
/// with playlists. See [`get_touched_with_playlists_sql`] for the group's shape;
/// the executed trace is `… WHERE (missing = ? AND num_playlists > ?)`.
#[must_use]
pub fn get_all_with_playlists_sql(library_filter: Option<Sql>) -> Sql {
    let (sql, args) = playlists_where(false);
    select_folder(library_filter)
        .where_raw(sql, args)
        .to_sql()
        .unwrap_or_default()
}

/// `purgeEmpty` (`folder_repository.go:305-325`): one DELETE, optionally
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

/// One row of `queryFolderUpdateInfo` (`folder_repository.go:180-186`): the
/// anonymous struct Go's `ScanStruct` fills for the five update-info columns.
///
/// `image_files` is the raw TEXT shim, decoded AFTER the whole page is read
/// (`:193-198`), so a bad cell is not a scan failure — see
/// [`FolderRepo::get_folder_update_info`].
#[derive(Debug, Clone, PartialEq)]
pub struct FolderUpdateInfo {
    /// `updated_at` (Go `UpdatedAt`, a non-pointer `time.Time`).
    pub updated_at: DateTime<Utc>,
    /// `hash` (Go `Hash`).
    pub hash: String,
    /// `image_files` decoded: `None` is Go's nil slice, which the empty shim
    /// leaves untouched (`:193` skips the `json.Unmarshal` entirely).
    pub image_files: Option<Vec<String>>,
    /// `images_updated_at` (Go `ImagesUpdatedAt`).
    pub images_updated_at: DateTime<Utc>,
}

/// What a **drained** folder cursor yields: the rows scanned before the first
/// failure, and that failure if there was one. See the module header for why the
/// port drains instead of streaming.
#[derive(Debug)]
pub struct CursorDrain {
    /// Rows successfully hydrated, in SQLite's returned order. Go's `wrapCursor`
    /// yields each of these (`sql_base_repository.go:397-405`) and stops at the
    /// first error, so this prefix is exactly what a caller that drains to
    /// completion receives.
    pub folders: Vec<FolderRecord>,
    /// The first failure, if any: a scan/`PostScan` error. Go yields it beside the
    /// failed row (`sql_base_repository.go:392-406`); a PostScan error may carry
    /// an already-populated `Folder` (the pinned probe's ID was `broken`). A
    /// driver/statement failure yields only its error (`:434-437`).
    pub error: Option<crate::hydrate::HydrateError>,
}

/// Errors of the executed [`FolderRepo`] reads.
#[derive(Debug, thiserror::Error)]
pub enum FolderError {
    /// The statement failed, or a row could not be scanned/hydrated. A
    /// `dbFolder.PostScan` JSON failure arrives here too, wrapped as Go's
    /// column-conversion error by [`crate::hydrate::HydrateError::into_sqlite`]
    /// (Go's dbx slice loop aborts on the hook error,
    /// `dbx@v1.12.0/rows.go:177-190`), carrying
    /// [`crate::hydrate::FOLDER_IMAGE_FILES_PREFIX`].
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// The statement could not be materialized — Go's `toSQL` failure
    /// (`sql_base_repository.go:358-372`).
    #[error(transparent)]
    Builder(#[from] SqlError),

    /// No row matched — Go's `model.ErrNotFound` (`model/errors.go:10` →
    /// `rest.ErrNotFound`, `data not found`, `deluan/rest@v1.0.1/errors.go:14`),
    /// which `queryOne` maps from `sql.ErrNoRows`
    /// (`sql_base_repository.go:375-388`, `dbx@v1.12.0/rows.go:237-244`).
    ///
    /// Only [`FolderRepo::get`] and [`FolderRepo::get_by_path`] report it: they are
    /// the two `queryOne` reads. Executed (oracle `/tmp/sd3g2_oracle9`, saved output
    /// `/tmp/g2_folder_queue_out.txt`): `Get("nope")` and
    /// `GetByPath(lib1, "/music-A/rock")` (no such folder) both returned
    /// `err="data not found"`. Go returns that error beside a non-nil pointer to a
    /// zero `Folder` (`dbx` allocates the embedded model in `indirect`,
    /// `dbx@v1.12.0/struct.go:251-259`); the port's `Err` carries the same
    /// information minus a pointer no Go caller can legitimately read.
    #[error("data not found")]
    NotFound,
}

/// `folderRepository`'s executed read side (`folder_repository.go:20-24,
/// 80-132`): statements from the builders above, run through
/// [`crate::execution`] on a leased connection, rows hydrated through
/// [`crate::hydrate::row_to_folder`] + [`FolderRow::into_record`] (Go's
/// `ScanStruct` → `PostScan` pair).
///
/// Go's reads take `ctx` for the logged user and the seeded-random reset;
/// `applyLibraryFilter` (`sql_base_repository.go:261-283`) is the ctx-derived part
/// that changes SQL, so the port takes it as an explicit
/// [`library_filter`] argument instead of a context. The seed leg needs nothing
/// here: `resetSeededRandom` (`:297-311`) only rewrites a `Sort` containing
/// `"seed"`, and folder has no sort mapping registered ([`SORT_MAPPINGS`]), so no
/// folder read can produce the `SEEDEDRAND` expression.
pub struct FolderRepo<'a> {
    conn: &'a rusqlite::Connection,
}

impl<'a> FolderRepo<'a> {
    /// `newFolderRepository` (`folder_repository.go:65-72`): bind to `conn` (the
    /// pooled lease). Go's `tableName`/`db` fields are the builders' constants here.
    #[must_use]
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    /// `Get` (`folder_repository.go:80-85`): the base select narrowed to one id,
    /// plus the caller's library filter.
    ///
    /// # Errors
    /// [`FolderError::NotFound`] when nothing matches — including when the library
    /// filter excludes the row for a restricted user (executed: the traced
    /// statement for a non-admin is `… WHERE folder.library_id IN (…) AND
    /// folder.id = ?`). [`FolderError::Sqlite`] on a statement or hydration failure.
    pub fn get(&self, id: &str, library_filter: Option<Sql>) -> Result<FolderRecord, FolderError> {
        let statement = get_sql(id, library_filter);
        self.read_one(&statement)
    }

    /// `GetByPath` (`folder_repository.go:87-90`): `Get` by
    /// `model.NewFolder(lib, path).ID`. Go builds the id through `NewFolder`
    /// (`model/folder.go:55-73`), whose ID step is `FolderID`
    /// (`:44-50` = [`folder_id`]); the extra `dir`/`name`/`ParentID` fields it
    /// computes are discarded by `Get`, which only ever reads the id.
    ///
    /// # Errors
    /// Same as [`FolderRepo::get`].
    pub fn get_by_path(
        &self,
        lib_id: i64,
        lib_path: &str,
        path: &str,
        library_filter: Option<Sql>,
    ) -> Result<FolderRecord, FolderError> {
        let statement = get_by_path_sql(lib_id, lib_path, path, library_filter);
        self.read_one(&statement)
    }

    /// `GetAll` (`folder_repository.go:92-97`) under the caller's parsed options.
    ///
    /// Go returns `res.toModels(), err` (`:96`), i.e. the rows scanned BEFORE a
    /// mid-page failure are handed back beside it. `Result` has no
    /// value-beside-error, so a failure here drops the prefix — the same decision
    /// already recorded for `library_repo::LibraryRepo::get_all`.
    ///
    /// # Errors
    /// [`FolderError::Sqlite`] on a statement or hydration failure; a zero-row
    /// result is an empty `Vec`, never [`FolderError::NotFound`] (Go's `queryAll`
    /// makes an empty non-nil slice, `dbx@v1.12.0/query.go:278-291`).
    /// [`FolderError::Builder`] if the statement fails to materialize.
    pub fn get_all(
        &self,
        library_filter: Option<Sql>,
        options: Option<&crate::sql_restful::QueryOptions>,
    ) -> Result<Vec<FolderRecord>, FolderError> {
        self.read_page(&get_all_query_sql(library_filter, options)?)
    }

    /// `CountAll` (`folder_repository.go:99-103`): `count`'s rewrite of the
    /// builder's `count(*)` (see [`count_all_query_sql`]).
    ///
    /// Go's `count` reads the `count` alias out of one row (`:572-574`); a
    /// `SELECT count(distinct …)` always emits one row, so `unwrap_or_default`
    /// mirrors Go's zero `res.Count` when the row is missing (unreachable in
    /// SQLite, but it keeps the port from inventing a `NotFound`).
    ///
    /// # Errors
    /// [`FolderError::Sqlite`] / [`FolderError::Builder`].
    pub fn count_all(
        &self,
        library_filter: Option<Sql>,
        options: Option<&crate::sql_restful::QueryOptions>,
    ) -> Result<i64, FolderError> {
        let statement = count_all_query_sql(library_filter, options)?;
        Ok(
            crate::execution::query_optional(self.conn, &statement, |row| {
                row.get::<_, i64>("count")
            })?
            .unwrap_or_default(),
        )
    }

    /// `HasAudioOutsideFolders` (`folder_repository.go:231-247`) via
    /// [`has_audio_outside_folders_sql`] — `exists`'s `count(*) as exist`
    /// projection and `Exist > 0` test (`sql_base_repository.go:488-493`).
    ///
    /// Go returns `(exists, err)` and its caller reads the bool beside the error
    /// (`scanner/phase_2_folders.go:250-254`); the port reports `false` beside
    /// `Err`, which is the value Go's zero `res.Exist` would give.
    ///
    /// # Errors
    /// [`FolderError::Sqlite`] on a statement failure.
    pub fn has_audio_outside_folders(
        &self,
        lib_id: i64,
        parent_path: &str,
        exclude_folder_ids: &[String],
    ) -> Result<bool, FolderError> {
        let statement = has_audio_outside_folders_sql(lib_id, parent_path, exclude_folder_ids);
        let exist = crate::execution::query_optional(self.conn, &statement, |row| {
            row.get::<_, i64>("exist")
        })?
        .unwrap_or_default();
        Ok(exist > 0)
    }

    /// `GetFolderUpdateInfo` (`folder_repository.go:105-132`): the whole-library
    /// read when `target_paths` is empty or contains `""`/`"."`, otherwise one
    /// [`get_folder_update_info_batch_sql`] statement per [`BATCH_SIZE`] chunk
    /// (`slices.Chunk`, `:120-123`), merged in order.
    ///
    /// **Key semantics (correcting the brief):** the map is keyed by FOLDER ID,
    /// not by path — `m[f.ID] = info` (`folder_repository.go:199`), over the
    /// selected `id` column. A later row therefore OVERWRITES an earlier one with
    /// the same folder id; two chunks returning the same id means the later chunk
    /// wins, which `BTreeMap::extend` reproduces. Keys arrive as
    /// [`folder_id`] hashes (`model/folder.go:44-50`), never as paths.
    ///
    /// A bad `image_files` cell aborts the ENTIRE map: `return nil, err` inside
    /// the loop (`:194-197`), so Go hands back a NIL map, not the partial one
    /// built so far. The port matches that by returning `Err` (its `BTreeMap` has
    /// no partial). The wrapper is Go's `parsing folder image_files: …` (`:196`) —
    /// NOT `dbFolder.PostScan`'s `parsing folder image files from db:` prefix,
    /// which belongs to the bulk folder reads ([`crate::hydrate::FOLDER_IMAGE_FILES_PREFIX`]).
    ///
    /// # Errors
    /// [`FolderError::Sqlite`] on a statement failure or on the decode failure
    /// above; [`FolderError::Builder`] if a batch statement fails to materialize.
    pub fn get_folder_update_info(
        &self,
        lib_id: i64,
        target_paths: &[String],
    ) -> Result<BTreeMap<String, FolderUpdateInfo>, FolderError> {
        if target_paths.is_empty()
            || target_paths
                .iter()
                .any(|path| path.is_empty() || path == ".")
        {
            return self.query_folder_update_info(&get_folder_update_info_all_sql(lib_id));
        }
        let mut result = BTreeMap::new();
        for chunk in target_paths.chunks(BATCH_SIZE) {
            let batch =
                self.query_folder_update_info(&get_folder_update_info_batch_sql(lib_id, chunk))?;
            result.extend(batch);
        }
        Ok(result)
    }

    /// `queryFolderUpdateInfo` (`folder_repository.go:178-202`): run one
    /// update-info statement and build the id-keyed map.
    fn query_folder_update_info(
        &self,
        statement: &Sql,
    ) -> Result<BTreeMap<String, FolderUpdateInfo>, FolderError> {
        use crate::hydrate::RowGet;
        let rows = crate::execution::query_all(self.conn, statement, |row| {
            Ok((
                row.get_text("id")?,
                row.get_dt("updated_at")?,
                row.get_text("hash")?,
                row.get_text("image_files")?,
                row.get_dt("images_updated_at")?,
            ))
        })?;
        let mut map = BTreeMap::new();
        for (id, updated_at, hash, image_files, images_updated_at) in rows {
            // Go decodes AFTER the whole page is scanned (`:192-199`), and the
            // empty shim skips the decode entirely (`:193`).
            let image_files =
                crate::hydrate::parse_folder_image_files(&image_files).map_err(|err| {
                    FolderError::Sqlite(
                        crate::hydrate::HydrateError::Decoded(format!(
                            "parsing folder image_files: {err}"
                        ))
                        .into_sqlite(),
                    )
                })?;
            map.insert(
                id,
                FolderUpdateInfo {
                    updated_at,
                    hash,
                    image_files,
                    images_updated_at,
                },
            );
        }
        Ok(map)
    }

    /// `GetTouchedWithPlaylists` (`folder_repository.go:276-287`), drained.
    ///
    /// Go's `queryWithStableResults` also runs `optimizePagination` when
    /// `options[0].Offset > 0` (`sql_base_repository.go:412-414`); this cursor is
    /// called with NO options by both callers
    /// (`scanner/phase_4_playlists.go:70-74`, `:72`), so that leg is unreachable
    /// here and deliberately not ported.
    ///
    /// # Errors
    /// The first failure is reported inside [`CursorDrain::error`] rather than as
    /// `Err`, because Go yields it as a cursor item. `Err` only covers a statement
    /// that cannot be prepared at all, which Go reports as the cursor constructor's
    /// own error (`:417-418`).
    pub fn drain_touched_with_playlists(
        &self,
        library_filter: Option<Sql>,
    ) -> Result<CursorDrain, FolderError> {
        self.drain(&get_touched_with_playlists_sql(library_filter))
    }

    /// `GetAllWithPlaylists` (`folder_repository.go:289-299`), drained. Same
    /// cursor caveats as [`Self::drain_touched_with_playlists`].
    pub fn drain_all_with_playlists(
        &self,
        library_filter: Option<Sql>,
    ) -> Result<CursorDrain, FolderError> {
        self.drain(&get_all_with_playlists_sql(library_filter))
    }

    /// Drain one cursor statement: hydrate rows until one fails.
    ///
    /// Go stops at the first row scan/PostScan failure and returns its populated
    /// `Folder` beside the error (`sql_base_repository.go:402,427-433`); it does NOT
    /// necessarily return a zero value. This drain retains successfully hydrated
    /// prefix rows and the first error, but cannot represent Go's failed-row value.
    fn drain(&self, statement: &Sql) -> Result<CursorDrain, FolderError> {
        let mut prepared = self
            .conn
            .prepare(&statement.0)
            .map_err(FolderError::Sqlite)?;
        let mut rows = prepared
            .query(rusqlite::params_from_iter(statement.1.iter()))
            .map_err(FolderError::Sqlite)?;
        let mut folders = Vec::new();
        let mut error = None;
        loop {
            match rows.next() {
                Ok(None) => break,
                Err(err) => {
                    // Go's `rows.Err()` arm yields the error alone
                    // (`sql_base_repository.go:434-437`); a driver failure is
                    // indistinguishable from a scan failure at this seam.
                    error = Some(crate::hydrate::HydrateError::Sqlite(err));
                    break;
                }
                Ok(Some(row)) => {
                    match crate::hydrate::row_to_folder(row)
                        .map_err(crate::hydrate::HydrateError::Sqlite)
                        .and_then(FolderRow::into_record)
                    {
                        Ok(record) => folders.push(record),
                        Err(err) => {
                            error = Some(err);
                            break;
                        }
                    }
                }
            }
        }
        Ok(CursorDrain { folders, error })
    }

    /// `queryOne` over a folder statement: at most one row, `NotFound` when
    /// there is none.
    fn read_one(&self, statement: &Sql) -> Result<FolderRecord, FolderError> {
        let row = crate::execution::query_optional(self.conn, statement, |row| {
            crate::hydrate::row_to_folder(row)
                .map_err(crate::hydrate::HydrateError::Sqlite)
                .and_then(FolderRow::into_record)
                .map_err(crate::hydrate::HydrateError::into_sqlite)
        })?;
        row.ok_or(FolderError::NotFound)
    }

    /// `queryAll` over a folder statement. Hydrate each row (including PostScan)
    /// inside the callback, matching Go's per-row `ScanStruct` order: a failure on
    /// an earlier row wins over a scan failure on a later row.
    fn read_page(&self, statement: &Sql) -> Result<Vec<FolderRecord>, FolderError> {
        Ok(crate::execution::query_all(self.conn, statement, |row| {
            crate::hydrate::row_to_folder(row)
                .map_err(crate::hydrate::HydrateError::Sqlite)
                .and_then(FolderRow::into_record)
                .map_err(crate::hydrate::HydrateError::into_sqlite)
        })?)
    }
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
        assert_eq!(from_lib_root, "3UmURwgxb31J8ABTHgCR9q");
        assert_eq!(from_lib_root, nd_id::new_hash1("7:music"));
    }

    #[test]
    fn select_folder_joins_library_and_applies_filter() {
        // Arrange: selectFolder (:74-78) + applyLibraryFilter.
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
        // Arrange: Get (:80-85).
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
        // Arrange: GetAll (:92-97) via applyOptions (:149-156): order-by
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
    fn count_all_rewrites_to_count_distinct_id_and_resets_order() {
        // Arrange: CountAll (:99-103) hands the builder count(*), but base
        // `count` (:565-570) removes those columns, replaces them with
        // count(distinct folder.id) as count, drops LIMIT/OFFSET and appends
        // ORDER BY folder.id. Executed against the real repository.
        // Act:
        let (sql, args) = count_all_sql(None);
        let (scoped, scoped_args) = count_all_sql(library_filter(Some("lim")));
        // Assert:
        assert_eq!(
            sql,
            "SELECT count(distinct folder.id) as count FROM folder ORDER BY folder.id"
        );
        assert!(args.is_empty());
        assert_eq!(
            scoped,
            "SELECT count(distinct folder.id) as count FROM folder \
             WHERE folder.library_id IN (SELECT ul.library_id FROM user_library ul \
             WHERE ul.user_id = ?) ORDER BY folder.id"
        );
        assert_eq!(scoped_args, vec![Value::Text("lim".to_string())]);
    }

    #[test]
    fn sort_leg_snake_cases_the_caller_field() {
        // Arrange: no sort mappings are registered (:65-72 never calls
        // setSortMappings), so buildSortOrder falls back to toSnakeCase for every
        // part (sql_base_repository.go:165-172, :199-228).
        // Act:
        let camel = build_sort_order("numAudioFiles", "desc");
        let mixed = build_sort_order("name", "ASC");
        let multi = build_sort_order("name, numAudioFiles", "desc");
        let empty_fields = build_sort_order("name,,id", "asc");
        let trailing_comma = build_sort_order("name,", "asc");
        let function_expr = build_sort_order("coalesce(folder.name,folder.path)", "asc");
        // Assert: the executed Go leg for Sort:"numAudioFiles"/Order:"desc" is
        // `ORDER BY num_audio_files desc` (oracle /tmp/sd3g2_oracle9, saved trace
        // /tmp/g2_folder_queue_trace.txt).
        assert_eq!(camel, "num_audio_files desc");
        assert_eq!(mixed, "name asc");
        assert_eq!(multi, "name desc, num_audio_files desc");
        assert_eq!(empty_fields, "name asc, id asc");
        assert_eq!(trailing_comma, "name asc");
        assert_eq!(function_expr, "coalesce(folder.name,folder.path) asc");
        assert!(sort_mapping("name").is_none(), "the map is empty");
    }

    #[test]
    fn update_info_all_pins_non_missing_library_scope() {
        // Arrange: getFolderUpdateInfoAll (:135-141).
        // Act:
        let (sql, args) = get_folder_update_info_all_sql(7);
        // Assert:
        assert_eq!(
            sql,
            "SELECT id, updated_at, hash, image_files, images_updated_at FROM folder \
             WHERE (library_id = ? AND missing = ?)"
        );
        assert_eq!(args, vec![Value::Integer(7), Value::Bool(false)]);
    }

    #[test]
    fn update_info_batch_pins_id_in_group_and_path_or_group() {
        // Arrange: one-batch getFolderUpdateInfoBatch (:144-175): the
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
             WHERE (library_id = ? AND missing = ? AND \
             (id IN (?, ?) OR (path = ? OR path LIKE ? OR path = ? OR path LIKE ?)))"
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
        // GetFolderUpdateInfo's ""/"." whole-library rule (:111-116).
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
            "SELECT count(*) as exist FROM folder \
             WHERE (library_id = ? AND missing = ? AND num_audio_files > ? \
             AND id NOT IN (?, ?) AND (path = ? OR path LIKE ? ESCAPE '\\'))"
        );
        assert_eq!(
            args,
            vec![
                Value::Integer(7),
                Value::Bool(false),
                Value::Integer(0),
                Value::Text("f-1".to_string()),
                Value::Text("f-2".to_string()),
                Value::Text("music".to_string()),
                Value::Text("music/%".to_string()),
            ]
        );
        // Go's NotEq with an empty slice takes squirrel's negated inEmptyExpr,
        // sqlTrue = (1=1) (squirrel@v1.5.4/expr.go:156-158,188): the exclusion
        // excludes NOTHING, so a nil-slice caller sees Go's true, not false.
        assert!(bare_sql.contains("AND (1=1) AND"));
        assert_eq!(bare_args[..3], args[..3]);
    }

    fn test_dt(hms: &str) -> DateTime<Utc> {
        use chrono::TimeZone;
        let parts: Vec<u32> = hms.split(':').map(|v| v.parse().unwrap()).collect();
        chrono::Utc
            .with_ymd_and_hms(2024, 7, 1, parts[0], parts[1], parts[2])
            .unwrap()
    }

    /// A record with a value in every column, each distinct and typed per
    /// column — a value rotation fails one of the assertions below.
    fn test_record() -> FolderRecord {
        FolderRecord {
            // Read-path identity: never in folder_row (the repo appends the id).
            id: "f-42".to_string(),
            library_id: 42,
            path: "music/rock".to_string(),
            name: "Rock".to_string(),
            parent_id: "p-abc".to_string(),
            num_audio_files: 31,
            num_playlists: 5,
            image_files: Some(vec!["a.jpg".to_string(), "b.jpg".to_string()]),
            images_updated_at: test_dt("10:11:12"),
            hash: "H-777".to_string(),
            missing: true,
            updated_at: test_dt("08:09:10"),
            created_at: Some(test_dt("01:02:03")),
            // Join-only, hydration-owned: never in folder_row.
            library_path: String::new(),
        }
    }

    #[test]
    fn folder_row_is_sorted_and_id_free() {
        // Arrange/Act:
        let row = folder_row(&test_record());
        // Assert: the 12 non-id persisted columns (the 13th, `id`, is
        // appended by the repo, never in the map), sorted keys.
        assert_eq!(row.len(), 12);
        assert!(!row.contains("id"));
        let cols: Vec<&str> = row.iter().map(|(c, _)| c).collect();
        let mut sorted = cols.clone();
        sorted.sort();
        assert_eq!(cols, sorted);
    }

    #[test]
    fn put_sql_renders_record_driven_update_and_insert() {
        // Arrange: full record (see test_record) + a distinct `now`.
        let f = test_record();
        let now = test_dt("14:15:16");
        // Act:
        let put = put_sql("f-1", &f, now, &[]).expect("renders");
        // Assert: UPDATE — sorted SET columns, no created_at, no id in
        // SET; the id binds last in WHERE.
        assert_eq!(
            put.update.0,
            "UPDATE folder SET hash = ?, image_files = ?, images_updated_at = ?, \
             library_id = ?, missing = ?, name = ?, num_audio_files = ?, \
             num_playlists = ?, parent_id = ?, path = ?, updated_at = ? WHERE id = ?"
        );
        assert_eq!(
            put.update.1,
            vec![
                Value::Text("H-777".to_string()),
                Value::Text(r#"["a.jpg","b.jpg"]"#.to_string()),
                Value::Text(crate::time::to_text(&f.images_updated_at)),
                Value::Integer(42),
                Value::Bool(true),
                Value::Text("Rock".to_string()),
                Value::Integer(31),
                Value::Integer(5),
                Value::Text("p-abc".to_string()),
                Value::Text("music/rock".to_string()),
                Value::Text(crate::time::to_text(&now)),
                Value::Text("f-1".to_string()),
            ]
        );
        // Assert: INSERT — sorted columns incl. created_at, id appended
        // LAST in both lists (the 8c89aeb9 invariant).
        assert_eq!(
            put.insert.0,
            "INSERT INTO folder (created_at, hash, image_files, images_updated_at, library_id, missing, name, num_audio_files, num_playlists, parent_id, path, updated_at, id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        );
        assert_eq!(
            put.insert.1,
            vec![
                Value::Text(crate::time::to_text(&f.created_at.unwrap())),
                Value::Text("H-777".to_string()),
                Value::Text(r#"["a.jpg","b.jpg"]"#.to_string()),
                Value::Text(crate::time::to_text(&f.images_updated_at)),
                Value::Integer(42),
                Value::Bool(true),
                Value::Text("Rock".to_string()),
                Value::Integer(31),
                Value::Integer(5),
                Value::Text("p-abc".to_string()),
                Value::Text("music/rock".to_string()),
                Value::Text(crate::time::to_text(&now)),
                Value::Text("f-1".to_string()),
            ]
        );
    }

    #[test]
    fn put_sql_image_files_none_binds_empty_json_array() {
        // Arrange: Go nil ImageFiles — PostMapArgs' "[]" arm.
        let mut f = test_record();
        f.image_files = None;
        // Act:
        let put = put_sql("f-1", &f, Utc::now(), &[]).expect("renders");
        // Assert: image_files binds the literal "[]" (never NULL) at its
        // position in both legs.
        assert_eq!(
            put.insert.1[2],
            Value::Text("[]".to_string()),
            "insert position of image_files"
        );
        assert_eq!(put.update.1[1], Value::Text("[]".to_string()));
    }

    #[test]
    fn put_sql_stamps_created_at_and_rejects_empty_update_set() {
        // Arrange: a never-stamped row (Go zero CreatedAt).
        let mut f = test_record();
        f.created_at = None;
        // Act:
        let put = put_sql("f-1", &f, Utc::now(), &[]).expect("renders");
        // Assert: created_at was stamped (TEXT, not NULL) on the insert.
        assert!(matches!(put.insert.1[0], Value::Text(_)));
        // And a created_at-only restriction is the typed error, not a
        // degenerate UPDATE.
        let err = put_sql("f-1", &f, Utc::now(), &["created_at"])
            .expect_err("must be empty after the drop");
        assert_eq!(err, crate::marshal::MarshalError::EmptyUpdateSet);
    }

    #[test]
    fn put_sql_restricted_update_normalizes_camel_case_cols() {
        // Arrange: Go's toSnakeCase'd colsToUpdate ("UpdatedAt" →
        // updated_at).
        let f = test_record();
        // Act:
        let put = put_sql("f-1", &f, Utc::now(), &["UpdatedAt"]).expect("renders");
        // Assert: only the requested column is SET; the id closes the args.
        assert_eq!(
            put.update.0,
            "UPDATE folder SET updated_at = ? WHERE id = ?"
        );
        assert_eq!(put.update.1.len(), 2);
        assert_eq!(put.update.1[1], Value::Text("f-1".to_string()));
        // A full update never SETs created_at.
        let full = put_sql("f-1", &f, Utc::now(), &[]).expect("renders");
        assert!(!full.update.0.contains("created_at = ?"));
    }

    #[test]
    fn mark_missing_chunks_ids_and_sets_updated_at() {
        // Arrange: 201 ids → 2 chunks (:261-274, chunk 200 at :263).
        let ids = (0..201).map(|i| format!("f-{i}")).collect::<Vec<_>>();
        // Act:
        let stmts = mark_missing_sql(true, &ids, &crate::time::zero());
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
        assert_eq!(stmts[0].1[1], crate::time::value(&crate::time::zero()));
        assert_eq!(stmts[0].1.len(), 202);
        assert_eq!(stmts[1].1.len(), 3);
    }

    #[test]
    fn playlist_cursor_wheres_pin_their_conditions() {
        // Arrange: GetTouchedWithPlaylists (:276-287) adds the last_scan_at
        // comparison GetAllWithPlaylists (:289-299) omits.
        // Act:
        let touched = get_touched_with_playlists_sql(None);
        let all = get_all_with_playlists_sql(None);
        // Assert:
        // Go's And wraps the whole group, and the ConcatExpr is a member of it
        // rather than a separate clause (see the builders' executed traces).
        assert_eq!(
            touched.0,
            "SELECT folder.*, library.path as library_path FROM folder \
             INNER JOIN library ON library.id = folder.library_id \
             WHERE (missing = ? AND num_playlists > ? \
             AND folder.updated_at > library.last_scan_at)"
        );
        assert_eq!(touched.1, vec![Value::Bool(false), Value::Integer(0)]);
        assert_eq!(
            all.0,
            "SELECT folder.*, library.path as library_path FROM folder \
             INNER JOIN library ON library.id = folder.library_id \
             WHERE (missing = ? AND num_playlists > ?)"
        );
        assert_eq!(all.1, vec![Value::Bool(false), Value::Integer(0)]);
        // The library filter is ANDed BEFORE the group (selectFolder applies it
        // first, folder_repository.go:74-78).
        let scoped = get_all_with_playlists_sql(library_filter(Some("lim")));
        assert_eq!(
            scoped.0,
            "SELECT folder.*, library.path as library_path FROM folder \
             INNER JOIN library ON library.id = folder.library_id \
             WHERE folder.library_id IN (SELECT ul.library_id FROM user_library ul \
             WHERE ul.user_id = ?) AND (missing = ? AND num_playlists > ?)"
        );
        assert_eq!(
            scoped.1,
            vec![
                Value::Text("lim".to_string()),
                Value::Bool(false),
                Value::Integer(0)
            ]
        );
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
