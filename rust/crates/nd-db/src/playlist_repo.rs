//! Port of `persistence/playlist_repository.go` (502L) — the playlist table
//! with its owner/permissions filter, token-based public access, save/load
//! shape and track order bookkeeping.
//!
//! Ownership: Go derives the logged-in user from the ambient `ctx`
//! (`sql_base_repository.go:53-57`); the crate has no ambient context (the
//! `sqlrepo.rs` DI rule), so the port threads the caller's user through
//! `user_filter` — `None` is admin/headless (Go's `And{}` — no restriction),
//! `Some(user_id)` is the `public OR owned` visibility of `userFilter`
//! (`playlist_repository.go:83-91`). The `annotation` join of
//! `selectPlaylist`'s `withAnnotation` (`sql_annotations.go:63-77`) is spelled
//! out — user_id is concatenated into the ON clause exactly as Go splices it
//! there (the crate's `with_annotation` binds it; the playlist select's
//! column list is Go's literal order, kept verbatim).
//!
//! Every Go method builds a squirrel statement and hands it to
//! `executeSQL`/`queryOne`/`queryAll`/`put`/`count`, which materialize through
//! `toSQL()` (`sql_base_repository.go:358-372`) as `(sql, args)`; execution and
//! logging stay with the caller (`store.rs`), so this port stops at the same
//! [`Sql`] pairs. `put`'s upsert (`sql_base_repository.go:613-635` — UPDATE
//! then INSERT, with the id forced into the SET list) and `selectUpdateColumns`'s
//! `created_at`/`birth_time` drops (`:587-604`) are reproduced on the
//! caller-supplied column list.
//!
//! Not ported, with the reason: `GetCursor` (`:225-235`) and the streaming
//! cursor machinery (`streamByIDs`), the artwork hydration
//! (`hydrateArtwork`, `:191-194`, per-row image loads) and the `Tracks(...)`
//! sub-repository binding (`playlist_track_repository.go:38-71` — that lane's
//! own file) need either the streaming/execution layer or the sibling lane's
//! surface; the select shapes they consume are pinned here
//! ([`tracks_query`], [`get_all_ids_query`]). `rest.Repository`'s
//! `Count`/`Read`/`ReadAll` are `CountAll`/`Get`/`GetAll` over
//! already-parsed options (`:410-420`).

use crate::sqlrepo::Value;

/// The `playlist` table (`registerModel(&model.Playlist{})`, `:31`).
pub const TABLE: &str = "playlist";

/// The `playlist_tracks` table the track methods rewrite (`:276`).
pub const TRACKS_TABLE: &str = "playlist_tracks";

/// The `media_file` table `keepAccessible` and `refreshCounters` read
/// (`:313-315`, `:337-339`).
pub const MEDIA_FILE_TABLE: &str = "media_file";

/// The `annotation` table the select joins (`sql_annotations.go:63-77`).
pub const ANNOTATION_TABLE: &str = "annotation";

/// The artwork queue `enqueueCoverRebuild` writes (`:365-372`,
/// `artwork_queue_repository.go:47-50`).
pub const ARTWORK_QUEUE_TABLE: &str = "artwork_queue";

/// `model.KindPlaylistArtwork.Prefix()` (`model/artwork_id.go:31`).
pub const PLAYLIST_ARTWORK_KIND: &str = "pl";

/// `model.ImageTypePrimary` (`model/artwork.go:22`).
pub const IMAGE_TYPE_PRIMARY: &str = "primary";

/// `model.ArtworkPriorityScan` (`model/artwork.go:116`).
pub const ARTWORK_PRIORITY_SCAN: i64 = 50;

/// `slices.Chunk(mediaFileIds, 200)` (`:298`): rows per keepAccessible probe
/// and per multi-row INSERT.
pub const CHUNK_SIZE: usize = 200;

/// `naturalSort`'s NATSORT collation (`helpers.go:89-94`,
/// `db/db.go:22`): the sort mapping `name`/`owner_name` wrap their column in
/// when `conf.Server.EnableNaturalSorting` is on.
pub const NATURAL_COLLATION: &str = "NATSORT";

/// A materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// The effective user for [`user_filter`]: `None` is admin/headless (Go's
/// `userFilter` returns `And{}` for admins, `:85-86`), `Some(id)` a normal
/// user restricted to public or owned playlists.
pub type User = Option<String>;

/// `userFilter` (`:83-91`): the visibility clause — the empty clause (AND
/// identity) for admins, `public = ? OR owner_id = ?` otherwise. Returns
/// `None` for the admin case (no clause is appended, matching `And{}`).
#[must_use]
pub fn user_filter(user: &User) -> Option<Sql> {
    user.as_ref().map(|uid| {
        (
            "public = ? OR owner_id = ?".to_string(),
            vec![Value::Bool(true), Value::Text(uid.clone())],
        )
    })
}

/// `naturalSort` (`helpers.go:89-94`): `(col collate NATSORT)` when natural
/// sorting is enabled, the bare column otherwise. The sort mapping
/// `name → naturalSort("playlist.name")`, `owner_name → naturalSort("owner_name")`
/// (`:62-65`); identifiers are the repository's fixed registrations, never
/// user text (slice-2 security contract).
#[must_use]
pub fn natural_sort(col: &str, enable_natural_sorting: bool) -> String {
    if enable_natural_sorting {
        format!("({col} collate {NATURAL_COLLATION})")
    } else {
        col.to_string()
    }
}

/// `smartPlaylistFilter` (`:76-81`): the `smart` REST filter — the empty
/// string or the `rules` IS NULL disjunction.
#[must_use]
pub fn smart_playlist_filter() -> Sql {
    (
        "rules = ? OR rules = ?".to_string(),
        vec![Value::Text(String::new()), Value::Null],
    )
}

/// `playlistFilter` (`:69-74`): the `q` REST filter — substring matches over
/// the playlist name and comment OR-joined.
#[must_use]
pub fn playlist_filter(value: &str) -> Sql {
    let parts = value.split_whitespace().collect::<Vec<_>>();
    if parts.is_empty() {
        // squirrel's empty Or renders 1=0, matching nothing.
        return ("1=0".to_string(), vec![]);
    }
    let mut args = Vec::new();
    let mut clauses = Vec::new();
    // Go: Or{substringFilter(name), substringFilter(comment)} — each is an
    // And over the field-split parts, so the two fields OR.
    for part in &parts {
        args.push(Value::Text(format!("%{part}%")));
        clauses.push("playlist.name LIKE ?".to_string());
    }
    for part in &parts {
        args.push(Value::Text(format!("%{part}%")));
        clauses.push("playlist.comment LIKE ?".to_string());
    }
    (clauses.join(" OR "), args)
}

/// `selectPlaylist` (`:257-260`): `playlist.*`, the owner name and the
/// annotation columns, joined to `user` and the caller's annotation LEFT JOIN.
/// The builder models only INNER JOIN, so the annotation join is spliced into
/// the FROM position like Go's column list order — the whole select is
/// rendered as one literal, the only variable parts being the user id and
/// the WHERE clauses.
#[must_use]
pub fn select_playlist_sql(user_id: &str, where_clauses: &[Sql], order_by: &[String]) -> Sql {
    let mut sql = format!(
        "SELECT {TABLE}.*, user.user_name as owner_name, \
         coalesce(starred, 0) as starred, coalesce(rating, 0) as rating, starred_at, \
         play_date, rated_at, coalesce(play_count, 0) as play_count, {TABLE}.average_rating \
         FROM {TABLE} JOIN user ON user.id = owner_id \
         LEFT JOIN {ANNOTATION_TABLE} ON ({ANNOTATION_TABLE}.item_id = {TABLE}.id AND \
         {ANNOTATION_TABLE}.item_type = ? AND {ANNOTATION_TABLE}.user_id = ?)"
    );
    let mut args = vec![
        Value::Text(TABLE.to_string()),
        Value::Text(user_id.to_string()),
    ];
    if !where_clauses.is_empty() {
        let mut clauses = Vec::new();
        for (clause_sql, clause_args) in where_clauses {
            clauses.push(clause_sql.clone());
            args.extend(clause_args.iter().cloned());
        }
        sql.push_str(&format!(" WHERE {}", clauses.join(" AND ")));
    }
    if !order_by.is_empty() {
        sql.push_str(&format!(" ORDER BY {}", order_by.join(", ")));
    }
    (sql, args)
}

/// `Exists` (`:102-104` via `exists`, `sql_base_repository.go:488-493`):
/// `SELECT count(*) as exist FROM playlist WHERE id = ? [visibility]`.
#[must_use]
pub fn exists_sql(id: &str, user: &User) -> Sql {
    let mut sql = format!("SELECT count(*) as exist FROM {TABLE} WHERE id = ?");
    let mut args = vec![Value::Text(id.to_string())];
    if let Some((filter_sql, filter_args)) = user_filter(user) {
        sql.push_str(&format!(" AND ({filter_sql})"));
        args.extend(filter_args);
    }
    (sql, args)
}

/// `Delete` (`:106-108` via `delete`, `sql_base_repository.go:649-655`):
/// `DELETE FROM playlist WHERE id IN (…) [visibility]` — chunked by the
/// caller against SQLITE_MAX_VARIABLE_NUMBER.
#[must_use]
pub fn delete_sql(ids: &[String], user: &User) -> Sql {
    let mut args: Vec<Value> = ids.iter().map(|id| Value::Text(id.clone())).collect();
    let mut sql = format!(
        "DELETE FROM {TABLE} WHERE id IN ({})",
        vec!["?"; ids.len()].join(", ")
    );
    if let Some((filter_sql, filter_args)) = user_filter(user) {
        sql.push_str(&format!(" AND ({filter_sql})"));
        args.extend(filter_args);
    }
    (sql, args)
}

/// `Get`/`findBy` (`:148-150`, `:175-189`): the selectPlaylist shape with the
/// id and visibility clauses.
#[must_use]
pub fn get_sql(id: &str, user_id: &str, user: &User) -> Sql {
    let mut clauses = vec![(
        "playlist.id = ?".to_string(),
        vec![Value::Text(id.to_string())],
    )];
    if let Some(filter) = user_filter(user) {
        clauses.push(filter);
    }
    select_playlist_sql(user_id, &clauses, &[])
}

/// `FindByPath` (`:171-173`): `findBy` with the path Eq — no visibility
/// filter, exactly as Go calls it (scanner-side, headless).
#[must_use]
pub fn find_by_path_sql(path: &str, user_id: &str) -> Sql {
    select_playlist_sql(
        user_id,
        &[("path = ?".to_string(), vec![Value::Text(path.to_string())])],
        &[],
    )
}

/// `GetAll` (`:196-210`): the selectPlaylist shape with the visibility
/// clause and the caller's sort mapping.
#[must_use]
pub fn get_all_sql(user_id: &str, user: &User, order_by: &[String]) -> Sql {
    let clauses = user_filter(user).map(|f| vec![f]).unwrap_or_default();
    select_playlist_sql(user_id, &clauses, order_by)
}

/// `CountAll` (`:94-100`): the base `count` shape
/// (`sql_base_repository.go:566-576`) over the visibility clause —
/// `count(distinct playlist.id) as count` with the base order-by reset.
#[must_use]
pub fn count_all_sql(user: &User) -> Sql {
    let mut sql = format!("SELECT count(distinct {TABLE}.id) as count FROM {TABLE}");
    let mut args = Vec::new();
    if let Some((filter_sql, filter_args)) = user_filter(user) {
        sql.push_str(&format!(" WHERE ({filter_sql})"));
        args.extend(filter_args);
    }
    sql.push_str(&format!(" ORDER BY {TABLE}.id"));
    (sql, args)
}

/// `getAllIDs` (`:212-222`): the id projection over the user subquery join —
/// the `user` table's name/created_at columns would make a playlist's own
/// ORDER BY ambiguous, so Go joins a projection. Sort clauses come from the
/// caller's mapping; the column list is fixed.
#[must_use]
pub fn get_all_ids_sql(user: &User, order_by: &[String]) -> Sql {
    let mut sql = concat!(
        "SELECT playlist.id, user.user_name as owner_name ",
        "FROM playlist JOIN (select id, user_name from user) user ON user.id = owner_id",
    )
    .to_string();
    let mut args = Vec::new();
    if let Some((filter_sql, filter_args)) = user_filter(user) {
        sql.push_str(&format!(" WHERE ({filter_sql})"));
        args.extend(filter_args);
    }
    if !order_by.is_empty() {
        sql.push_str(&format!(" ORDER BY {}", order_by.join(", ")));
    }
    (sql, args)
}

/// `GetPlaylists` (`:237-255`): the playlists containing a media file — the
/// selectPlaylist shape joined to `playlist_tracks` and filtered by the
/// media file id plus visibility, sorted by the `name` mapping.
#[must_use]
pub fn get_playlists_sql(media_file_id: &str, user_id: &str, user: &User, name_sort: &str) -> Sql {
    let mut sql = format!(
        "SELECT {TABLE}.*, user.user_name as owner_name, \
         coalesce(starred, 0) as starred, coalesce(rating, 0) as rating, starred_at, \
         play_date, rated_at, coalesce(play_count, 0) as play_count, {TABLE}.average_rating \
         FROM playlist_tracks JOIN user ON user.id = owner_id \
         JOIN playlist ON playlist.id = playlist_tracks.playlist_id \
         LEFT JOIN annotation ON (annotation.item_id = playlist.id AND \
         annotation.item_type = ? AND annotation.user_id = ?) \
         WHERE playlist_tracks.media_file_id = ?"
    );
    let mut args = vec![
        Value::Text(TABLE.to_string()),
        Value::Text(user_id.to_string()),
        Value::Text(media_file_id.to_string()),
    ];
    if let Some((filter_sql, filter_args)) = user_filter(user) {
        sql.push_str(&format!(" AND ({filter_sql})"));
        args.extend(filter_args);
    }
    sql.push_str(&format!(" ORDER BY {name_sort}"));
    (sql, args)
}

/// `GetWithTracks`'s track select (`:154-159`): the shared [`tracks_query`]
/// with the missing-track filter and the position order.
#[must_use]
pub fn load_tracks_sql(playlist_id: &str, user_id: &str, library_filter: Option<&Sql>) -> Sql {
    tracks_query(playlist_id, user_id, library_filter)
}

/// `tracksQuery` (`:374-397`): the track hydration select shared by
/// `loadTracks` and `GetCursor` — annotation columns, media file and library
/// joins, the library filter, and the playlist restriction. The annotation
/// LEFT JOIN embeds the user id exactly as Go concatenates it (`:389-393`);
/// the builder models only INNER JOIN, so the whole select is spelled out.
#[must_use]
pub fn tracks_query(playlist_id: &str, user_id: &str, library_filter: Option<&Sql>) -> Sql {
    // The annotation LEFT JOIN embeds the user id exactly as Go concatenates
    // it (:389-393); the library filter is the caller's applyLibraryFilter.
    let mut args = vec![Value::Text(playlist_id.to_string())];
    let library_clause = match library_filter {
        Some((sql, lib_args)) => {
            args.extend(lib_args.iter().cloned());
            format!(
                " AND {}",
                sql.replace("media_file.library_id", "f.library_id")
            )
        }
        None => String::new(),
    };
    (
        format!(
            "SELECT coalesce(starred, 0) as starred, starred_at, \
             coalesce(play_count, 0) as play_count, play_date, coalesce(rating, 0) as rating, \
             rated_at, f.*, playlist_tracks.*, library.path as library_path, \
             library.name as library_name FROM playlist_tracks \
             LEFT JOIN annotation ON (annotation.item_id = media_file_id AND \
             annotation.item_type = 'media_file' AND annotation.user_id = '{user_id}') \
             INNER JOIN media_file f ON f.id = media_file_id \
             INNER JOIN library ON f.library_id = library.id \
             WHERE playlist_id = ?{library_clause}"
        ),
        args,
    )
}

/// `GetWithTracks`'s missing filter + order (`:155-157`): the two clauses
/// `loadTracks` pins over the shared query — `missing = false` and the
/// position order `playlist_tracks.id`.
pub const LOAD_TRACKS_WHERE: &str = "missing = false";
/// The position order of the load select (`:157`).
pub const LOAD_TRACKS_ORDER: &str = "playlist_tracks.id";

/// `updateTracks`/`updatePlaylist` (`:263-283`): the full-replace pair —
/// the delete of the old rows, then `add_tracks_sql`'s chunked inserts from
/// position 1. The inserts come from
/// [`crate::playlist_track_repo::add_tracks_sql`].
#[must_use]
pub fn update_playlist_sql(playlist_id: &str) -> Sql {
    (
        format!("DELETE FROM {TRACKS_TABLE} WHERE playlist_id = ?"),
        vec![Value::Text(playlist_id.to_string())],
    )
}

/// `keepAccessible` (`:310-327`): the chunked existence probe — one
/// `SELECT id FROM media_file WHERE id IN (…)` per CHUNK_SIZE unique ids,
/// restricted by the caller's library filter. The caller intersects the
/// results to drop inaccessible ids while preserving the input order and
/// duplicates (Go `slice.Filter`, `:325-327`).
#[must_use]
pub fn keep_accessible_sql(unique_ids: &[String], library_filter: Option<&Sql>) -> Vec<Sql> {
    unique_ids
        .chunks(CHUNK_SIZE)
        .map(|chunk| {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let mut args: Vec<Value> = chunk.iter().map(|id| Value::Text(id.clone())).collect();
            let mut sql = format!("SELECT id FROM {MEDIA_FILE_TABLE} WHERE id IN ({placeholders})");
            if let Some((filter_sql, filter_args)) = library_filter {
                sql.push_str(&format!(" AND {filter_sql}"));
                args.extend(filter_args.iter().cloned());
            }
            (sql, args)
        })
        .collect()
}

/// `refreshCounters` (`:329-363`): the aggregate select plus the update that
/// writes the totals — Go reads `(duration, size, count)`, writes the three
/// SET columns plus `updated_at`, and hands the same timestamp back for
/// `evaluated_at` (smart_playlist_repo.rs's stamp). The caller owns the
/// timestamp value (Go `time.Now()`).
#[must_use]
pub fn refresh_counters_sql(playlist_id: &str, now: &str) -> (Sql, Sql) {
    let select = (
        format!(
            "SELECT coalesce(sum(duration), 0) as duration, coalesce(sum(size), 0) as size, \
             count(*) as count FROM {MEDIA_FILE_TABLE} \
             JOIN playlist_tracks f ON f.media_file_id = media_file.id \
             WHERE playlist_id = ?"
        ),
        vec![Value::Text(playlist_id.to_string())],
    );
    let update = (
        format!(
            "UPDATE {TABLE} SET duration = ?, size = ?, song_count = ?, updated_at = ? WHERE id = ?"
        ),
        // The aggregate results bind first (the caller substitutes the read
        // row's values), the timestamp and id close the list. The caller
        // fills duration/size/count; this shape pins the arg order.
        vec![
            Value::Text(now.to_string()),
            Value::Text(playlist_id.to_string()),
        ],
    );
    (select, update)
}

/// `enqueueCoverRebuild` (`:365-372` via `artworkQueueRepository.Enqueue`,
/// `artwork_queue_repository.go:47-50`): the upsert that re-resolves the
/// generated 2x2 grid. `retry_at`/`enqueued_at` bind the same caller-owned
/// `now`. Only enqueued when the track set changes (the grid samples albums
/// at random — a rename must not rebuild it, `:389-391`).
#[must_use]
pub fn enqueue_cover_rebuild_sql(playlist_id: &str, now: &str) -> Sql {
    (
        format!(
            "INSERT INTO {ARTWORK_QUEUE_TABLE} \
             (item_kind, item_id, image_type, priority, attempts, retry_at, enqueued_at) \
             VALUES (?, ?, ?, ?, 0, ?, ?) \
             ON CONFLICT (item_kind, item_id, image_type) DO UPDATE SET \
             priority = MAX(priority, excluded.priority), retry_at = excluded.retry_at, \
             attempts = 0, enqueued_at = excluded.enqueued_at, trace = '[]'"
        ),
        vec![
            Value::Text(PLAYLIST_ARTWORK_KIND.to_string()),
            Value::Text(playlist_id.to_string()),
            Value::Text(IMAGE_TYPE_PRIMARY.to_string()),
            Value::Integer(ARTWORK_PRIORITY_SCAN),
            Value::Text(now.to_string()),
            Value::Text(now.to_string()),
        ],
    )
}

/// One `SET column = value` pair of the `put` upsert
/// (`sql_base_repository.go:613-635`): identifiers are the model's fixed
/// snake_case column names, values are bound.
#[derive(Debug, Clone, PartialEq)]
pub struct Col {
    /// The column name — never user text.
    pub column: String,
    /// Its bound value.
    pub value: Value,
}

impl Col {
    /// A text SET pair.
    #[must_use]
    pub fn text(column: &str, value: &str) -> Self {
        Self {
            column: column.to_string(),
            value: Value::Text(value.to_string()),
        }
    }
}

/// `dbPlaylist.PostMapArgs` (`:41-55`): the smart-playlist SET shaping —
/// `rules` is the marshaled criteria JSON, and the evaluation-owned counters
/// (`song_count`, `duration`, `size`) are dropped so a save never overwrites
/// what `refreshCounters` owns. Returns the column list for [`put_sql`].
#[must_use]
pub fn put_smart_cols(rules_json: String) -> Vec<Col> {
    vec![Col {
        column: "rules".to_string(),
        value: Value::Text(rules_json),
    }]
}

/// The columns `PostMapArgs` drops for a smart playlist (`:49-51`) — never
/// SET by a save; `refreshCounters`/evaluation owns them.
pub const SMART_OWNED_COUNTER_COLS: [&str; 3] = ["song_count", "duration", "size"];

/// The columns `selectUpdateColumns` always drops on update
/// (`sql_base_repository.go:601-604`).
pub const NEVER_UPDATE_COLS: [&str; 2] = ["created_at", "birth_time"];

/// `Put` (`:110-146` via `put`, `sql_base_repository.go:613-635`): the
/// upsert pair — an UPDATE against the row id first, and when it affects no
/// row, the INSERT. `selectUpdateColumns` (`:587-604`) filters the SET list:
/// only the requested `cols_to_update` (or all of them when none are), minus
/// `created_at`/`birth_time`; the update re-forces `id` into the SET list
/// (`filterUpdateValues`, `:606-609`). The caller owns the id generation for
/// a fresh row (Go `id2.NewRandom()`, `:627`).
///
/// The track follow-ups (Go runs `updatePlaylist`/`addTracks` +
/// `enqueueCoverRebuild` + `refreshCounters` after the row write, `:130-146`)
/// come from [`update_playlist_sql`],
/// [`crate::playlist_track_repo::add_tracks_sql`],
/// [`enqueue_cover_rebuild_sql`] and [`refresh_counters_sql`].
#[must_use]
pub fn put_sql(id: &str, cols: &[Col], all_cols: &[Col], cols_to_update: &[&str]) -> (Sql, Sql) {
    let update_cols = filter_update_cols(all_cols, cols_to_update);
    let set = update_cols
        .iter()
        .map(|c| format!("{} = ?", c.column))
        .collect::<Vec<_>>()
        .join(", ");
    let mut update_args: Vec<Value> = update_cols.iter().map(|c| c.value.clone()).collect();
    update_args.push(Value::Text(id.to_string()));
    let update = (
        format!("UPDATE {TABLE} SET {set} WHERE id = ?"),
        update_args,
    );
    // Go's `put` adds `id` to the values map before inserting — either the
    // caller's id or a freshly minted one (`sql_base_repository.go:637-641`).
    // `INSERT INTO t SET …` is MySQL syntax that SQLite rejects ("near \"SET\":
    // syntax error"); squirrel renders a sorted column list plus a positional
    // VALUES tuple (`squirrel@v1.5.4/insert.go:271-283`), so the column list
    // and the args must be emitted together.
    let mut insert_cols: Vec<String> = cols.iter().map(|c| c.column.clone()).collect();
    let mut insert_args: Vec<Value> = cols.iter().map(|c| c.value.clone()).collect();
    insert_cols.push("id".to_string());
    insert_args.push(Value::Text(id.to_string()));
    let placeholders = vec!["?"; insert_args.len()].join(", ");
    let insert = (
        format!(
            "INSERT INTO {TABLE} ({}) VALUES ({placeholders})",
            insert_cols.join(", ")
        ),
        insert_args,
    );
    (update, insert)
}

/// `selectUpdateColumns` (`sql_base_repository.go:587-604`): keep only the
/// requested columns (all when none requested), drop `created_at` and
/// `birth_time`. The id column is re-added by [`put_sql`]'s caller per
/// `filterUpdateValues` (`:606-609`) — it is never dropped here.
#[must_use]
fn filter_update_cols(all_cols: &[Col], cols_to_update: &[&str]) -> Vec<Col> {
    all_cols
        .iter()
        .filter(|c| cols_to_update.is_empty() || cols_to_update.contains(&c.column.as_str()))
        .filter(|c| !NEVER_UPDATE_COLS.contains(&c.column.as_str()))
        .cloned()
        .collect()
}

/// `removeOrphans` (`:439-473`): the playlists-with-orphans select — groups
/// the playlist_tracks whose media file is missing, per playlist.
#[must_use]
pub fn remove_orphans_select_sql() -> Sql {
    (
        concat!(
            "SELECT playlist_tracks.playlist_id as id, p.name FROM playlist_tracks ",
            "JOIN playlist p ON playlist_tracks.playlist_id = p.id ",
            "LEFT JOIN media_file mf ON playlist_tracks.media_file_id = mf.id ",
            "WHERE mf.id = ? GROUP BY playlist_tracks.playlist_id",
        )
        .to_string(),
        vec![Value::Null],
    )
}

/// `removeOrphans`'s per-playlist delete (`:455-462`): the orphan rows of one
/// playlist — media files that match no track.
#[must_use]
pub fn remove_orphans_delete_sql(playlist_id: &str) -> Sql {
    (
        format!(
            "DELETE FROM {TRACKS_TABLE} WHERE media_file_id NOT IN (SELECT id FROM {MEDIA_FILE_TABLE}) \
             AND playlist_id = ?"
        ),
        vec![Value::Text(playlist_id.to_string())],
    )
}

/// `renumber` (`:475-502`): the two-step resequencing that avoids the UNIQUE
/// (playlist_id, id) constraint — negate all ids, then reassign via a
/// materialized CTE in `id DESC` order (restoring the original order since
/// the ids are negative). Used by `removeOrphans`, `Delete`/`DeleteAll` and
/// `Insert`'s hole-closing, and followed by the caller's
/// `enqueueCoverRebuild` + `refreshCounters` (`:498-500`).
#[must_use]
pub fn renumber_sql(playlist_id: &str) -> Vec<Sql> {
    let pid = Value::Text(playlist_id.to_string());
    vec![
        (
            format!("UPDATE {TRACKS_TABLE} SET id = -id WHERE playlist_id = ? AND id > 0"),
            vec![pid.clone()],
        ),
        (
            concat!(
                "WITH new_ids AS ( ",
                "SELECT rowid as rid, ROW_NUMBER() OVER (ORDER BY id DESC) as new_id ",
                "FROM playlist_tracks WHERE playlist_id = ?) ",
                "UPDATE playlist_tracks SET id = new_ids.new_id ",
                "FROM new_ids ",
                "WHERE playlist_tracks.rowid = new_ids.rid AND playlist_tracks.playlist_id = ?",
            )
            .to_string(),
            vec![pid.clone(), pid],
        ),
    ]
}

/// `GetWithTracks`'s `includeMissing` parameter (`:152`): the missing filter
/// toggle — `false` applies [`LOAD_TRACKS_WHERE`], `true` keeps every row.
#[must_use]
pub fn load_tracks_include_missing(include_missing: bool) -> Option<&'static str> {
    if include_missing {
        None
    } else {
        Some(LOAD_TRACKS_WHERE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user() -> User {
        Some("u1".to_string())
    }

    #[test]
    fn user_filter_pins_the_public_or_owned_visibility() {
        // Arrange/Act: userFilter (:83-91).
        let admin = user_filter(&None);
        let user = user_filter(&user());
        // Assert:
        assert_eq!(admin, None);
        assert_eq!(
            user,
            Some((
                "public = ? OR owner_id = ?".to_string(),
                vec![Value::Bool(true), Value::Text("u1".to_string()),]
            ))
        );
    }

    #[test]
    fn natural_sort_wraps_only_when_enabled() {
        // Arrange/Act: naturalSort (helpers.go:89-94) over the name mapping.
        let on = natural_sort("playlist.name", true);
        let off = natural_sort("playlist.name", false);
        // Assert:
        assert_eq!(on, "(playlist.name collate NATSORT)");
        assert_eq!(off, "playlist.name");
    }

    #[test]
    fn exists_counts_with_the_visibility_clause() {
        // Arrange/Act: Exists (:102-104).
        let (sql, args) = exists_sql("pl-1", &user());
        // Assert:
        assert_eq!(
            sql,
            "SELECT count(*) as exist FROM playlist WHERE id = ? AND (public = ? OR owner_id = ?)"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("pl-1".to_string()),
                Value::Bool(true),
                Value::Text("u1".to_string()),
            ]
        );
        let (admin_sql, admin_args) = exists_sql("pl-1", &None);
        assert_eq!(
            admin_sql,
            "SELECT count(*) as exist FROM playlist WHERE id = ?"
        );
        assert_eq!(admin_args, vec![Value::Text("pl-1".to_string())]);
    }

    #[test]
    fn delete_binds_ids_then_visibility() {
        // Arrange/Act: Delete (:106-108).
        let (sql, args) = delete_sql(&["pl-1".to_string(), "pl-2".to_string()], &user());
        // Assert:
        assert_eq!(
            sql,
            "DELETE FROM playlist WHERE id IN (?, ?) AND (public = ? OR owner_id = ?)"
        );
        assert_eq!(args.len(), 4);
    }

    #[test]
    fn select_playlist_pins_go_column_and_join_order() {
        // Arrange/Act: selectPlaylist (:257-260) + withAnnotation's columns.
        let (sql, args) = select_playlist_sql(
            "u1",
            &[(
                "playlist.id = ?".to_string(),
                vec![Value::Text("pl-1".to_string())],
            )],
            &[natural_sort("playlist.name", true)],
        );
        // Assert:
        assert_eq!(
            sql,
            "SELECT playlist.*, user.user_name as owner_name, \
             coalesce(starred, 0) as starred, coalesce(rating, 0) as rating, starred_at, \
             play_date, rated_at, coalesce(play_count, 0) as play_count, playlist.average_rating \
             FROM playlist JOIN user ON user.id = owner_id \
             LEFT JOIN annotation ON (annotation.item_id = playlist.id AND \
             annotation.item_type = ? AND annotation.user_id = ?) \
             WHERE playlist.id = ? ORDER BY (playlist.name collate NATSORT)"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("playlist".to_string()),
                Value::Text("u1".to_string()),
                Value::Text("pl-1".to_string()),
            ]
        );
    }

    #[test]
    fn count_all_and_get_all_ids_pin_their_shapes() {
        // Arrange/Act: CountAll (:94-100), getAllIDs (:212-222).
        let (count, cargs) = count_all_sql(&user());
        let (ids, iargs) =
            get_all_ids_sql(&user(), &["(playlist.name collate NATSORT)".to_string()]);
        // Assert:
        assert_eq!(
            count,
            "SELECT count(distinct playlist.id) as count FROM playlist \
             WHERE (public = ? OR owner_id = ?) ORDER BY playlist.id"
        );
        assert_eq!(cargs.len(), 2);
        assert_eq!(
            ids,
            "SELECT playlist.id, user.user_name as owner_name \
             FROM playlist JOIN (select id, user_name from user) user ON user.id = owner_id \
             WHERE (public = ? OR owner_id = ?) ORDER BY (playlist.name collate NATSORT)"
        );
        assert_eq!(iargs.len(), 2);
    }

    #[test]
    fn get_playlists_joins_tracks_and_sorts_by_name() {
        // Arrange/Act: GetPlaylists (:237-255).
        let (sql, args) =
            get_playlists_sql("mf-1", "u1", &user(), "(playlist.name collate NATSORT)");
        // Assert:
        assert!(sql.contains("JOIN playlist ON playlist.id = playlist_tracks.playlist_id"));
        assert!(sql.contains("WHERE playlist_tracks.media_file_id = ?"));
        assert!(sql.ends_with(
            "AND (public = ? OR owner_id = ?) ORDER BY (playlist.name collate NATSORT)"
        ));
        assert_eq!(args[2], Value::Text("mf-1".to_string()));
    }

    #[test]
    fn tracks_query_pins_the_shared_hydration_select() {
        // Arrange/Act: tracksQuery (:374-397) for playlist "pl-1", user "u1".
        let (sql, args) = load_tracks_sql("pl-1", "u1", None);
        // Assert:
        assert_eq!(
            sql,
            "SELECT coalesce(starred, 0) as starred, starred_at, coalesce(play_count, 0) as play_count, \
             play_date, coalesce(rating, 0) as rating, rated_at, f.*, playlist_tracks.*, \
             library.path as library_path, library.name as library_name \
             FROM playlist_tracks LEFT JOIN annotation ON (annotation.item_id = media_file_id AND \
             annotation.item_type = 'media_file' AND annotation.user_id = 'u1') \
             INNER JOIN media_file f ON f.id = media_file_id \
             INNER JOIN library ON f.library_id = library.id \
             WHERE playlist_id = ?"
        );
        assert_eq!(args, vec![Value::Text("pl-1".to_string())]);
    }

    #[test]
    fn keep_accessible_chunks_unique_ids() {
        // Arrange: 201 unique ids (:310-327) — two probe chunks.
        let ids: Vec<String> = (1..=CHUNK_SIZE + 1).map(|i| format!("mf-{i}")).collect();
        // Act:
        let probes = keep_accessible_sql(&ids, None);
        // Assert:
        assert_eq!(probes.len(), 2);
        assert_eq!(
            probes[0].0,
            format!(
                "SELECT id FROM media_file WHERE id IN ({})",
                vec!["?"; CHUNK_SIZE].join(", ")
            )
        );
        assert_eq!(probes[1].1.len(), 1);
    }

    #[test]
    fn refresh_counters_pins_aggregate_and_update_shapes() {
        // Arrange/Act: refreshCounters (:329-363).
        let (select, update) = refresh_counters_sql("pl-1", "2024-01-01 00:00:00+00:00");
        // Assert:
        assert_eq!(
            select.0,
            "SELECT coalesce(sum(duration), 0) as duration, coalesce(sum(size), 0) as size, \
             count(*) as count FROM media_file \
             JOIN playlist_tracks f ON f.media_file_id = media_file.id \
             WHERE playlist_id = ?"
        );
        assert_eq!(
            update.0,
            "UPDATE playlist SET duration = ?, size = ?, song_count = ?, updated_at = ? WHERE id = ?"
        );
        assert_eq!(
            update.1,
            vec![
                Value::Text("2024-01-01 00:00:00+00:00".to_string()),
                Value::Text("pl-1".to_string()),
            ]
        );
    }

    #[test]
    fn enqueue_cover_rebuild_pins_the_upsert() {
        // Arrange/Act: enqueueCoverRebuild (:365-372, artwork_queue_repository.go:47-50).
        let (sql, args) = enqueue_cover_rebuild_sql("pl-1", "2024-01-01 00:00:00+00:00");
        // Assert:
        assert_eq!(
            sql,
            "INSERT INTO artwork_queue \
             (item_kind, item_id, image_type, priority, attempts, retry_at, enqueued_at) \
             VALUES (?, ?, ?, ?, 0, ?, ?) \
             ON CONFLICT (item_kind, item_id, image_type) DO UPDATE SET \
             priority = MAX(priority, excluded.priority), retry_at = excluded.retry_at, \
             attempts = 0, enqueued_at = excluded.enqueued_at, trace = '[]'"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("pl".to_string()),
                Value::Text("pl-1".to_string()),
                Value::Text("primary".to_string()),
                Value::Integer(50),
                Value::Text("2024-01-01 00:00:00+00:00".to_string()),
                Value::Text("2024-01-01 00:00:00+00:00".to_string()),
            ]
        );
    }

    #[test]
    fn put_filters_never_updated_columns_from_the_update() {
        // Arrange: Put's upsert (:110-146 via sql_base_repository.go:587-635).
        let all = vec![
            Col::text("name", "Mix"),
            Col::text("created_at", "old"),
            Col::text("rules", "{}"),
        ];
        // Act: a full update (no cols_to_update).
        let (update, insert) = put_sql("pl-1", &all, &all, &[]);
        // Assert: created_at dropped from the UPDATE, kept in the INSERT.
        assert_eq!(
            update.0,
            "UPDATE playlist SET name = ?, rules = ? WHERE id = ?"
        );
        // The INSERT is a column list + positional VALUES tuple (SQLite has no
        // `INSERT … SET`), with `id` appended to both like Go's `put`
        // (`sql_base_repository.go:637-641`).
        assert_eq!(
            insert.0,
            "INSERT INTO playlist (name, created_at, rules, id) VALUES (?, ?, ?, ?)"
        );
        assert_eq!(
            insert.1,
            vec![
                Value::from("Mix"),
                Value::from("old"),
                Value::from("{}"),
                Value::from("pl-1"),
            ]
        );
        // A partial update keeps only the requested columns, still minus created_at.
        let (partial, _) = put_sql("pl-1", &all, &all, &["created_at"]);
        assert_eq!(partial.0, "UPDATE playlist SET  WHERE id = ?");
    }

    #[test]
    fn remove_orphans_and_renumber_pin_go_literals() {
        // Arrange/Act: removeOrphans (:439-473), renumber (:475-502).
        let (sel, sargs) = remove_orphans_select_sql();
        let del = remove_orphans_delete_sql("pl-1");
        let renum = renumber_sql("pl-1");
        // Assert:
        assert!(sel.contains("LEFT JOIN media_file mf ON playlist_tracks.media_file_id = mf.id"));
        assert_eq!(sargs, vec![Value::Null]);
        assert_eq!(
            del,
            (
                "DELETE FROM playlist_tracks WHERE media_file_id NOT IN (SELECT id FROM media_file) \
                 AND playlist_id = ?".to_string(),
                vec![Value::Text("pl-1".to_string())]
            )
        );
        assert_eq!(
            renum[0].0,
            "UPDATE playlist_tracks SET id = -id WHERE playlist_id = ? AND id > 0"
        );
        assert!(renum[1].0.contains("WITH new_ids AS"));
        assert!(renum[1].0.contains("ROW_NUMBER() OVER (ORDER BY id DESC)"));
        assert_eq!(renum[1].1.len(), 2);
    }
}
