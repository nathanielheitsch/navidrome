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
//! [`Sql`] pairs. The write path is the record-driven [`put_sql`] (S-D2b):
//! [`PlaylistRecord`] → [`playlist_row`] (the `structs.Map(dbPlaylist)` +
//! `dbPlaylist.PostMapArgs` map — the smart/non-smart `rules`/counter
//! delete lives there) → the shared [`crate::marshal::MarshalledRow`] seam,
//! which reproduces `put`'s upsert and `selectUpdateColumns`'s
//! `created_at`/`birth_time` drops (`sql_base_repository.go:587-642`) from
//! the record — no caller-supplied column arrays for the write path.
//!
//! Not ported, with the reason: `GetCursor` (`:225-235`) and the streaming
//! cursor machinery (`streamByIDs`) need the streaming/execution layer, and the
//! `Tracks(...)` sub-repository binding (`playlist_track_repository.go:38-71`)
//! belongs to that lane's own file; the select shapes they consume are pinned
//! here ([`tracks_query`], [`get_all_ids_sql`]). The artwork hydration
//! (`hydrateArtwork`, `:191-194`) landed with this file's read side
//! ([`PlaylistRepo`]) and delegates to
//! [`crate::artwork_hydration::hydrate_playlist_artwork`]. `rest.Repository`'s
//! `Count`/`Read`/`ReadAll` are `CountAll`/`Get`/`GetAll` over
//! already-parsed options (`:410-420`).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

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

/// `smartPlaylistFilter` (`:76-81`): `Or{Eq{"rules": ""}, Eq{"rules": nil}}`.
/// squirrel renders a nil-valued `Eq` as `rules IS NULL` with **no placeholder**
/// (`squirrel@v1.5.4/expr.go:182-183`, `if val == nil { expr = fmt.Sprintf("%s %s NULL",
/// key, nullOpr) }`, with `nullOpr = "IS"` at `:150` — no arg is appended) and
/// parenthesizes every conjunction group it emits (`expr.go:384-386`). Executed
/// against the pinned squirrel (`/tmp/sd3f_p1/args.go`):
/// `sql = (rules = ? OR rules IS NULL)`, `len(args) == 1`.
///
/// **Defect fixed in this lane:** this builder used to emit
/// `rules = ? OR rules = ?` with a `Value::Null` second arg. SQL three-valued logic
/// makes `rules = NULL` never true, so a playlist whose `rules` cell is NULL — the
/// normal non-smart row written by `put` — was EXCLUDED from Go's `smart` filter.
/// Executed over {rules = "", rules = NULL}: the squirrel-rendered clause selects
/// both rows, the old port text selected only the empty-string row.
#[must_use]
pub fn smart_playlist_filter() -> Sql {
    (
        "(rules = ? OR rules IS NULL)".to_string(),
        vec![Value::Text(String::new())],
    )
}

/// `playlistFilter` (`:69-74`): `Or{substringFilter(name), substringFilter(comment)}`.
/// Each `substringFilter` (`sql_restful.go:119-126`) is an **`And` over the
/// whitespace-split parts**, so the two FIELDS OR while the parts within one field
/// AND. Executed against the pinned squirrel (`/tmp/sd3f_p1/q.go`):
/// `"rah frahm"` ⇒ `((playlist.name LIKE ? AND playlist.name LIKE ?) OR
/// (playlist.comment LIKE ? AND playlist.comment LIKE ?))`, args
/// `[%rah%, %frahm%, %rah%, %frahm%]` — the name legs bind before the comment legs,
/// like Go's `Or{first, second}`.
///
/// **Two defects fixed in this lane.** (1) The builder used to flatten every part of
/// BOTH fields into one `OR`, so a query whose words live in different fields matched
/// a row Go rejects (name has word 1, comment has word 2). (2) An empty query used to
/// return `1=0`. Go's `Or` is not empty in that case — its two legs are EMPTY `And`s,
/// and squirrel renders an empty conjunction as `1=1` (`expr.go`, `sqlTrue`), so
/// `q=` renders `((1=1) OR (1=1))` and matches EVERYTHING; `1=0` is what an empty *Or*
/// renders, which Go never builds here. Executed: `q=""` ⇒ `((1=1) OR (1=1))`, no args.
#[must_use]
pub fn playlist_filter(value: &str) -> Sql {
    let parts = value.split_whitespace().collect::<Vec<_>>();
    if parts.is_empty() {
        // Two empty And{} legs: squirrel renders each as the conjunction identity.
        return ("((1=1) OR (1=1))".to_string(), vec![]);
    }
    let mut args = Vec::new();
    let mut legs = Vec::new();
    for column in ["playlist.name", "playlist.comment"] {
        let mut and_leg = Vec::new();
        for part in &parts {
            args.push(Value::Text(format!("%{part}%")));
            and_leg.push(format!("{column} LIKE ?"));
        }
        legs.push(format!("({})", and_leg.join(" AND ")));
    }
    (format!("({})", legs.join(" OR ")), args)
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

/// `Delete` (`:106-108` via `delete`, `sql_base_repository.go:644-647`):
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
///
/// The visibility clause is parenthesized because `userFilter` returns an `Or`
/// (`playlist_repository.go:88-91`) and squirrel parenthesizes every conjunct
/// group it renders (`squirrel@v1.5.4/expr.go:384-386`; `And` and `Or` are both
/// `conj.join`, `:391-401`). Executed against the pinned squirrel, a non-admin
/// `Get` renders `WHERE (playlist.id = ? AND (public = ? OR owner_id = ?))`.
/// Unparenthesized, SQL binds `AND` tighter than `OR`, so the predicate becomes
/// `(id AND public) OR owner_id`: the id no longer restricts the owner branch,
/// and a non-admin asking for an invisible id gets some OTHER visible row
/// instead of NotFound. Executed in SQLite over {adm-private, adm-public,
/// lim-private}: `WHERE id = ? AND public = ? OR owner_id = ?` bound to
/// `adm-private` returns `lim-private`, and `nobody` also returns `lim-private`;
/// the parenthesized form returns no rows for both, which is what the real Go
/// repository reports (`data not found`, nil pointer). Same wrapping the
/// sibling builders already use (`exists_sql`, `delete_sql`, `count_all_sql`,
/// `get_all_ids_sql`, `get_playlists_sql`).
#[must_use]
pub fn get_sql(id: &str, user_id: &str, user: &User) -> Sql {
    let mut clauses = vec![(
        "playlist.id = ?".to_string(),
        vec![Value::Text(id.to_string())],
    )];
    if let Some((filter_sql, filter_args)) = user_filter(user) {
        clauses.push((format!("({filter_sql})"), filter_args));
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
///
/// `GetAll` hands `userFilter` straight to `Where` (`:197`), so it is the whole
/// predicate and needs no parentheses for CORRECTNESS — but the clause is wrapped
/// here anyway so the rendered text matches squirrel's `Or` output
/// (`WHERE (public = ? OR owner_id = ?)`, executed against squirrel v1.5.4) and so
/// a future second conjunct cannot inherit [`get_sql`]'s precedence hazard.
#[must_use]
pub fn get_all_sql(user_id: &str, user: &User, order_by: &[String]) -> Sql {
    let clauses = user_filter(user)
        .map(|(filter_sql, filter_args)| (format!("({filter_sql})"), filter_args))
        .map(|clause| vec![clause])
        .unwrap_or_default();
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
/// selectPlaylist shape joined to `playlist_tracks` and filtered by the media file id
/// plus visibility, sorted by the `name` mapping.
///
/// Two text-level differences from Go's traced statement, both recorded:
/// * Leg order. Go roots the statement at `playlist` (`selectPlaylist`, `:257-269`) and
///   appends `Join("playlist_tracks on …")` (`:239`), tracing
///   `FROM playlist JOIN user … LEFT JOIN annotation … JOIN playlist_tracks …`.
///   The port roots at `playlist_tracks` and joins `playlist` (`:240`). Every leg is an
///   INNER JOIN, so the row set, the column set and the bind order are identical; only
///   the printed `FROM`/`JOIN` order differs, and SQLite's planner does not care.
/// * Sort direction. `name_sort` is the caller's rendered mapping — Go's
///   `buildSortOrder` (`sql_base_repository.go:199-228`) appends a direction, so the
///   traced leg is `ORDER BY playlist.name asc`, and `applyOptions` emits ORDER BY only
///   when `Sort != ""` (`:157-158`). `GetPlaylists` always passes `Sort: "name"`
///   (`:238`), so the port's unconditional leg matches Go for its only call site; a
///   caller wanting the traced text supplies `"playlist.name asc"`. The port-wide
///   "`apply_pagination` renders no sort leg" residual (already recorded for
///   `get_all_sql`) is why the direction is caller-supplied rather than derived.
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

/// `GetWithTracks`'s track select (`:160-162`): the shared [`tracks_query`]
/// with the missing-track filter and the position order.
#[must_use]
pub fn load_tracks_sql(playlist_id: &str, user_id: &str, library_filter: Option<&Sql>) -> Sql {
    tracks_query(playlist_id, user_id, library_filter)
}

/// `tracksQuery` (`:374-397`): the track hydration select shared by
/// `loadTracks` and `GetCursor` — annotation columns, media file and library
/// joins, the library filter, and the playlist restriction. The annotation
/// LEFT JOIN embeds the user id exactly as Go concatenates it (`:390-393`);
/// the builder models only INNER JOIN, so the whole select is spelled out.
#[must_use]
pub fn tracks_query(playlist_id: &str, user_id: &str, library_filter: Option<&Sql>) -> Sql {
    // tracksQuery applies `applyLibraryFilter` FIRST (`playlist_repository.go:375`)
    // and appends its own `Where(Eq{"playlist_id": id})` LAST (`:396`), so the
    // filter clause precedes the playlist restriction and its bound arg binds
    // first. `playlistTrackRepository.Read` is the opposite — it applies the
    // filter LAST (`playlist_track_repository.go:117`), which is why
    // [`crate::playlist_track_repo::read_sql`] binds the filter arg last.
    let library_clause = match library_filter {
        Some((sql, _)) => format!(
            "WHERE {} AND playlist_id = ?",
            sql.replace("media_file.library_id", "f.library_id")
        ),
        None => "WHERE playlist_id = ?".to_string(),
    };
    // The annotation LEFT JOIN embeds the user id exactly as Go concatenates it
    // (`:390-393`), so it binds nothing. squirrel renders joins before WHERE
    // (`select.go:92-118`), and its `Where` clauses bind in append order, so the
    // filter arg comes before the playlist id.
    let mut args: Vec<Value> = Vec::new();
    if let Some((_, lib_args)) = library_filter {
        args.extend(lib_args.iter().cloned());
    }
    args.push(Value::Text(playlist_id.to_string()));
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
             {library_clause}"
        ),
        args,
    )
}

/// `GetWithTracks`'s missing filter + order (`:161-162`): the two clauses
/// `loadTracks` receives over the shared query — `missing = false` and the
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

/// `model.Playlist`'s persisted row (`model/playlist.go:20-45`) — the 16
/// non-id persisted playlist columns (the contract's 17 minus `id`,
/// which the seam forbids in the map
/// (`crate::marshal::MarshalError::IdInMap`); the id is appended last by
/// [`put_sql`]. Field → column: `Name`→`name`, `Comment`→`comment`,
/// `Duration`→`duration`, `Size`→`size`, `SongCount`→`song_count`,
/// `OwnerID`→`owner_id` (NOT NULL, references `user`), `Public`→`public`,
/// `Path`→`path`, `Sync`→`sync`, `UploadedImage`→`uploaded_image`,
/// `ExternalImageURL`→`external_image_url`, `CreatedAt`→`created_at`,
/// `UpdatedAt`→`updated_at`, `ImportedHash`→`imported_hash`,
/// `Rules`→`rules`, `EvaluatedAt`→`evaluated_at`. Non-persisted:
/// `OwnerName`/`Tracks` (`structs:"-"`), the `Annotations`/`ItemImage`
/// embeds.
///
/// Timestamps are `DateTime<Utc>` bound through `crate::time` (explicit
/// `'%F %T%.f%:z'` TEXT; the DDL carries no defaults for the playlist
/// time columns, so the bind is the only source). `created_at` is `None`
/// for a never-stamped row; [`put_sql`] stamps it like Go's `Put`
/// (`playlist_repository.go:118-121`) when the record is new.
///
/// `rules` is the typed criteria; Go's `json.Marshal(Rules)` output
/// (`nd_model::criteria::marshal_criteria`) is what the PostMapArgs arm
/// stores. `evaluated_at` is `Option` (Go `*time.Time`): `None` binds NULL.
///
/// There is deliberately no `Default`: a write record must name every persisted
/// column, so the hydration-owned fields can only arrive through the read path.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistRecord {
    /// The row's `id` (Go `Playlist.ID`, `structs:"id"`). Read-path identity:
    /// [`crate::hydrate::row_to_playlist`] fills it from the row so a bulk read
    /// keeps every row's own id. [`playlist_row`] does NOT emit it — the write
    /// legs take the id as their own argument like Go's `put(ctx, id, …)`
    /// (`sql_base_repository.go:618`).
    pub id: String,
    /// `name` (Go `Name`).
    pub name: String,
    /// `comment` (Go `Comment`).
    pub comment: String,
    /// `duration` (Go `Duration`, `float32` at `model/playlist.go:27`). The
    /// DDL column is `real` either way (`schema_ddl.rs:66`); `f64` matches the
    /// column affinity and differs from Go only at the float32-rounding
    /// boundary — pre-existing, not introduced here.
    pub duration: f64,
    /// `size` (Go `Size`).
    pub size: i64,
    /// `song_count` (Go `SongCount`).
    pub song_count: i64,
    /// `owner_id` (Go `OwnerID`) — required, FK to `user`.
    pub owner_id: String,
    /// `public` (Go `Public`).
    pub public: bool,
    /// `path` (Go `Path`).
    pub path: String,
    /// `sync` (Go `Sync`).
    pub sync: bool,
    /// `uploaded_image` (Go `UploadedImage`).
    pub uploaded_image: String,
    /// `external_image_url` (Go `ExternalImageURL`).
    pub external_image_url: String,
    /// `created_at` (Go `CreatedAt`); `None` = never stamped, stamped to
    /// `now` by [`put_sql`] for a new record.
    pub created_at: Option<DateTime<Utc>>,
    /// `updated_at` (Go `UpdatedAt`) — re-stamped to `now` by [`put_sql`].
    pub updated_at: DateTime<Utc>,
    /// `imported_hash` (Go `ImportedHash`).
    pub imported_hash: String,
    /// `rules` (Go `Rules *criteria.Criteria`); `Some` makes the record a
    /// smart playlist (PostMapArgs stores the JSON and drops the
    /// evaluation-owned counters), `None` stores no rules column at all.
    pub rules: Option<nd_model::Criteria>,
    /// `evaluated_at` (Go `EvaluatedAt *time.Time`); `None` binds NULL.
    pub evaluated_at: Option<DateTime<Utc>>,
    /// The per-user annotation the select's `annotation` LEFT JOIN contributes
    /// (`sql_annotations.go:63-88`), **derived, not persisted**: Go's
    /// `Playlist.Annotations` embed is `structs:"-"` (`model/playlist.go:21`),
    /// so [`playlist_row`] does NOT emit any of it and the write SQL is
    /// unchanged. [`crate::hydrate::row_to_playlist`] fills it from the six
    /// joined columns plus the base `average_rating`, with zero values where a
    /// light query omits the joined columns. Its timestamp strings carry Go's
    /// RFC3339Nano wire form ([`crate::hydrate::to_go_time_text`]), NOT the
    /// SQLite storage spelling from [`crate::time::to_text`].
    pub annotation: nd_model::annotation::Annotation,
    /// The promoted `ItemImage` embed (`model/playlist.go:22`, `structs:"-"`): the
    /// playlist's own artwork state, **read-path only**. Filled by the SECOND
    /// layer ([`crate::artwork_hydration::hydrate_playlist_artwork`],
    /// `playlist_repository.go:191-194`) — never by
    /// [`crate::hydrate::row_to_playlist`], whose projection has no artwork column,
    /// so it stays zero (= UNRESOLVED) through phases 1 and 2. Zero is NOT the
    /// known-absent state (`image_absent == true`); see
    /// [`crate::artwork_hydration::ItemImage`]. Never emitted by [`playlist_row`],
    /// so the write map keeps its column count unchanged.
    pub image: crate::artwork_hydration::ItemImage,
    /// Go's `OwnerName` (`structs:"-"` at `model/playlist.go:26`), hydrated from
    /// the `user.user_name as owner_name` alias (`:259`). Read-path-only: dbx's
    /// `ScanStruct` maps by FIELD NAME (`dbx@v1.12.0/rows.go:60-85`), so the
    /// alias IS scanned into Go's `OwnerName`; the `structs:"-"` tag only keeps
    /// it out of the write map, which is why [`playlist_row`] still does not
    /// emit it. A projection without the `user` JOIN leaves Go's zero value
    /// (empty string).
    pub owner_name: String,
}

/// Go's `Playlist.IsSmartPlaylist()` (`playlist.go:46-48`): `Rules != nil
/// && Rules.Expression != nil` — the port's nil stand-in is the closed
/// enum's empty `All` group (`nd_model::criteria::Criteria::default`).
///
/// **Known limit, relevant when S-D3 hydration lands:** an empty `All` group
/// is classified non-smart, which would also make PostMapArgs delete its
/// `rules` column (`playlist_repository.go:49`). This is unreachable from
/// parsed criteria — `Criteria.UnmarshalJSON` (`model/criteria/criteria.go:134-140`)
/// rejects `all`/`any` with zero rules outright, so Go never holds a
/// non-nil Criteria whose top-level group is empty. S-D3's rules decoder must
/// reproduce that rejection rather than inventing an empty `All` where Go
/// would have had nil.
#[must_use]
pub fn is_smart_playlist(rules: &Option<nd_model::Criteria>) -> bool {
    match rules {
        Some(c) => !matches!(&c.expression, nd_model::Expression::All(items) if items.is_empty()),
        None => false,
    }
}

/// The counters `dbPlaylist.PostMapArgs` deletes for a smart playlist
/// (`playlist_repository.go:43-45`): owned by `refreshCounters`/evaluation,
/// never by a save.
pub const SMART_OWNED_COUNTER_COLS: [&str; 3] = ["song_count", "duration", "size"];

/// `structs.Map(dbPlaylist)` + `dbPlaylist.PostMapArgs`
/// (`playlist_repository.go:41-51`) as one sorted
/// [`crate::marshal::ColumnMap`]: exactly the 17 persisted playlist
/// columns (without `id`), with the PostMapArgs deletes applied —
/// **smart** (`is_smart_playlist`): `rules` overwritten with the
/// `marshal_criteria` JSON and `song_count`/`duration`/`size` removed
/// (`:38-45`); **non-smart**: `rules` removed (`:49`).
///
/// `id` never appears in the map — the seam rejects it. A smart playlist whose
/// criteria cannot be marshalled aborts construction, matching Go's failed-put
/// control flow; Rust's diagnostic text omits Go's full MarshalerError chain.
///
/// # Errors
/// [`crate::marshal::MarshalError::Criteria`] when a criteria leaf is invalid.
pub fn playlist_row(
    p: &PlaylistRecord,
) -> Result<crate::marshal::ColumnMap, crate::marshal::MarshalError> {
    let mut map = BTreeMap::new();
    map.insert("comment".to_string(), Value::Text(p.comment.clone()));
    map.insert(
        "created_at".to_string(),
        crate::time::optional_value(p.created_at.as_ref()),
    );
    map.insert("duration".to_string(), Value::Real(p.duration));
    map.insert(
        "evaluated_at".to_string(),
        crate::time::optional_value(p.evaluated_at.as_ref()),
    );
    map.insert(
        "external_image_url".to_string(),
        Value::Text(p.external_image_url.clone()),
    );
    map.insert(
        "imported_hash".to_string(),
        Value::Text(p.imported_hash.clone()),
    );
    map.insert("name".to_string(), Value::Text(p.name.clone()));
    map.insert("owner_id".to_string(), Value::Text(p.owner_id.clone()));
    map.insert("path".to_string(), Value::Text(p.path.clone()));
    map.insert("public".to_string(), Value::Bool(p.public));
    // Go's map carries `rules` before PostMapArgs; the value is replaced
    // (smart) or removed (non-smart) below, exactly as Go's delete/assign.
    map.insert("rules".to_string(), Value::Null);
    map.insert("size".to_string(), Value::Integer(p.size));
    map.insert("song_count".to_string(), Value::Integer(p.song_count));
    map.insert("sync".to_string(), Value::Bool(p.sync));
    map.insert("updated_at".to_string(), crate::time::value(&p.updated_at));
    map.insert(
        "uploaded_image".to_string(),
        Value::Text(p.uploaded_image.clone()),
    );
    let map = crate::marshal::ColumnMap::new(map);
    let map = if is_smart_playlist(&p.rules) {
        // PostMapArgs smart arm (`:38-45`): rules → JSON, counters deleted.
        let mut m = map.map().clone();
        m.insert(
            "rules".to_string(),
            Value::Text(
                nd_model::criteria::marshal_criteria(p.rules.as_ref().unwrap())
                    .map_err(crate::marshal::MarshalError::Criteria)?,
            ),
        );
        for c in SMART_OWNED_COUNTER_COLS {
            m.remove(c);
        }
        crate::marshal::ColumnMap::new(m)
    } else {
        // PostMapArgs non-smart arm (`:49`): rules deleted.
        let mut m = map.map().clone();
        m.remove("rules");
        crate::marshal::ColumnMap::new(m)
    };
    Ok(map)
}

/// `dbPlaylist` raw row (phase 1 of the two-phase hydration seam,
/// `persistence/playlist_repository.go:24-27`): the columns `selectPlaylist`'
/// `playlist.*` + owner name + the annotation columns scan into, with the
/// `rules` shim **still unparsed** — Go's `dbPlaylist.Rules` is a
/// `sql.NullString` (`:26`), hence [`Option<String>`] here: SQL NULL and a
/// dropped alias both mean "no rules column". Phase 2 is
/// [`crate::hydrate::playlist_post_scan`], which decodes it onto the record.
///
/// The joined annotation columns are read straight onto the record's derived
/// field (Go's promoted `Playlist.Annotations`, `structs:"-"`), so they are not
/// duplicated on this raw struct; `owner_name` is hydrated from the joined alias.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistRow {
    /// The playlist columns scanned from the row, `rules` still empty.
    pub record: PlaylistRecord,
    /// The raw `rules` column: `None` for SQL NULL (Go's `sql.NullString`
    /// with `Valid == false`), `Some("")` for the empty-string cell that
    /// Go's `PostScan` guard (`:30`) skips.
    pub rules: Option<String>,
}

impl PlaylistRow {
    /// `dbPlaylist.PostScan` (`playlist_repository.go:29-34`): thin delegation
    /// onto [`crate::hydrate::playlist_post_scan`], surfaced the way Go's
    /// `ScanStruct` error leaves the read.
    ///
    /// # Errors
    /// [`crate::hydrate::HydrateError::Decoded`] carrying the criteria codec's
    /// own message (Go wraps it with **no** prefix).
    pub fn into_record(self) -> Result<PlaylistRecord, crate::hydrate::HydrateError> {
        crate::hydrate::playlist_post_scan(self)
    }
}

/// `Put`/`Update` (`playlist_repository.go:110-146`, `:431-437` via `put`,
/// `sql_base_repository.go:618-642`), record-driven (S-D2b): the record is
/// the only write input. [`put_sql`] stamps the row like Go's `Put`
/// (`:118-123`) — `created_at` to `now` when the record carries none
/// (Go `isNew`), `updated_at` to `now` — renders both legs through the
/// shared [`crate::marshal::MarshalledRow`] from the [`playlist_row`]
/// map. The UPDATE leg is the `selectUpdateColumns`-filtered set (no
/// `created_at`; `id` binds last via `WHERE id = ?`), the INSERT leg the
/// full sorted columns with `id` last in both lists
/// (`:637-641`). Go's `put` runs the UPDATE first and only INSERTs on a
/// 0-row match; the caller picks the leg.
///
/// `cols_to_update`: optional `colsToUpdate` (`:595-601`); empty applies
/// no restriction. The entries are `toSnakeCase`-normalized by the seam
/// ([`crate::marshal::to_snake_case`]), so camelCase names such as
/// `"Public"`/`"UpdatedAt"` behave like Go's — Go's `Update` appends
/// `"updatedAt"` to the caller's cols (`:435`); do the same with `now`
/// here. An update set empty after the `created_at`/`birth_time` drop is
/// [`crate::marshal::MarshalError::EmptyUpdateSet`], not degenerate SQL.
///
/// The single source of truth for the drop list is the seam's
/// `selectUpdateColumns` port — the old local `NEVER_UPDATE_COLS`/
/// `filter_update_cols` duplication is gone.
///
/// # Errors
///
/// Returns [`crate::marshal::MarshalError`] when the map carries `id`
/// (impossible from [`playlist_row`]) or the UPDATE set is empty.
///
/// [`PutPlaylistSql::update`]: the `r.put` update leg (filtered columns,
/// `id` last in args).
/// [`PutPlaylistSql::insert`]: the `r.put` insert leg (sorted columns,
/// `id` last in both).
#[derive(Debug, Clone, PartialEq)]
pub struct PutPlaylistSql {
    /// `Update(playlist).SetMap(...).Where(Eq{"id": id})` — the set pairs
    /// bind before `id`.
    pub update: Sql,
    /// `Insert(playlist).SetMap(values)` — the sorted column list, then
    /// `id`, matching the VALUES tuple (`sql_base_repository.go:637-641`).
    pub insert: Sql,
}

/// See [`PutPlaylistSql`].
pub fn put_sql(
    id: &str,
    p: &PlaylistRecord,
    now: DateTime<Utc>,
    cols_to_update: &[&str],
) -> Result<PutPlaylistSql, crate::marshal::MarshalError> {
    let stamped = PlaylistRecord {
        // Go: `if isNew { pls.CreatedAt = time.Now() }` (:118-121) — a
        // record carrying created_at keeps its stamp.
        created_at: p.created_at.or(Some(now)),
        updated_at: now,
        ..p.clone()
    };
    let map = playlist_row(&stamped)?;
    let row = crate::marshal::MarshalledRow::new(TABLE, id, &map, cols_to_update)?;
    Ok(PutPlaylistSql {
        update: (row.update_sql, row.update_args),
        insert: (row.insert_sql, row.insert_args),
    })
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

/// Failures of the executed playlist reads ([`PlaylistRepo`]).
///
/// The variant set follows the sibling `ArtistRepo::ArtistError`
/// (`artist_repo.rs:1498-1521`) and `PlaylistTrackRepo::PlaylistTrackError`
/// (`playlist_track_repo.rs:570-600`): a statement/hydration failure and the
/// not-found case, nothing else.
#[derive(Debug, thiserror::Error)]
pub enum PlaylistError {
    /// The SQLite statement failed, or a row's hydration failed. Every read here is
    /// `queryAll`, so a `PostScan` failure surfaces from dbx's slice loop — the hook
    /// runs after a successful `Scan` and its error aborts the loop
    /// (`dbx@v1.12.0/rows.go:177-190`, aborting at `:185-188`; a scan failure aborts
    /// at `:169-171`), never from the single-row `ScanStruct` hook block at `:92-102`.
    /// That is why a decode failure arrives here too — via
    /// [`crate::hydrate::HydrateError::into_sqlite`].
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// No row matched — Go's `model.ErrNotFound` (`model/errors.go:10` →
    /// `rest.ErrNotFound`, `data not found`, `deluan/rest@v1.0.1/errors.go:14`).
    ///
    /// On this repository it comes ONLY from `findBy`'s row count
    /// (`playlist_repository.go:182-184`: `if len(pls) == 0 { return nil,
    /// model.ErrNotFound }`), which `Get` (`:148-150`) and `FindByPath`
    /// (`:171-173`) both reach. Executed: `Get("pl-nobody")` returns
    /// `err="data not found"` with a NIL pointer.
    ///
    /// No read here reaches `queryOne`, so `sql.ErrNoRows` is not this code path:
    /// dbx's `All` loops `Next` then `Close` and makes an empty NON-nil slice for a
    /// zero-row result (`dbx@v1.12.0/query.go:278-291`, `rows.go:122-125`), and
    /// `sql.ErrNoRows` is returned only by `Rows.one`/`Rows.row`
    /// (`rows.go:237-244`, `:281-294`). Go's `queryAll` ErrNoRows→ErrNotFound arm
    /// (`sql_base_repository.go:451-454`) is therefore DEAD on every path in this
    /// file, which is exactly why [`PlaylistRepo::get_all`] and
    /// [`PlaylistRepo::get_playlists`] must NEVER report this variant — and why
    /// Go's `GetPlaylists` `errors.Is(err, model.ErrNotFound)` arm
    /// (`playlist_repository.go:244-246`) can never fire.
    #[error("data not found")]
    NotFound,
}

/// `playlistRepository` (`playlist_repository.go:20-22`) bound to one pooled
/// connection — the executed READ side of the playlist repository.
///
/// Go binds a `dbx.Builder` and reaches the DB through the ambient ctx; this crate
/// has no ambient context (the `sqlrepo.rs` DI freeze), so the caller threads a
/// lease from [`crate::store::SQLStore::with_connection_sync`] (a transaction
/// derefs to a `&Connection`) and every statement runs through
/// [`crate::execution`] — the port of `executeSQL`/`queryOne`/`queryAll`
/// (`sql_base_repository.go:336-355,375-388,441-457`). Rows are hydrated through
/// the shared seam exactly as the album/artist reads do —
/// [`crate::hydrate::row_to_playlist`] → [`PlaylistRow::into_record`] — with
/// [`crate::hydrate::HydrateError::into_sqlite`] as the exit shape, and artwork
/// hydration is the second layer, [`crate::artwork_hydration::hydrate_playlist_artwork`]
/// (`hydrateArtwork`, `:191-194`).
///
/// `user_id` is Go's `loggedUser(ctx).ID` for `selectPlaylist`'s `withAnnotation`
/// LEFT JOIN (`sql_annotations.go:63-77`); `user` is the visibility input of
/// [`user_filter`] — `None` is admin/headless (Go returns the `And{}` identity, which
/// the traced SQL renders as `AND (1=1)`; the port omits the clause entirely, the
/// documented residual). Both are needed separately because the annotation join is
/// emitted for an ADMIN too: the traced admin `Get` binds `annotation.user_id = 'adm'`.
#[derive(Debug)]
pub struct PlaylistRepo<'a> {
    conn: &'a rusqlite::Connection,
}

impl<'a> PlaylistRepo<'a> {
    /// `NewPlaylistRepository` (`:53-66`): bind to `conn` (the pooled lease). Go's
    /// registrations (the filter mappings and sort mappings it registers) are the
    /// caller's inputs to the statement builders, not per-connection state.
    #[must_use]
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    /// `Get` (`:148-150`) → `findBy` (`:175-189`): [`get_sql`] (the selectPlaylist
    /// shape narrowed to one id plus visibility), hydrated through both layers.
    ///
    /// Go's NotFound comes from the row COUNT after `queryAll`, not from a
    /// driver-level no-rows condition — see [`PlaylistError::NotFound`] — and it is
    /// decided BEFORE hydration, so a zero-row read issues no artwork lookup at all
    /// (`:182-184` returns before `:187`). Go returns `nil` beside that error, so
    /// the port's `Err` carries the same information.
    ///
    /// An artwork-lookup failure is NOT an error here: Go's `hydrateItems` logs and
    /// returns an EMPTY map (`artwork_hydration.go:58-68`), so the playlist comes
    /// back with a zero (unresolved) [`crate::artwork_hydration::ItemImage`]. That
    /// failure is deliberately dropped here — Go's `findBy` has nowhere to report
    /// it. Use [`PlaylistRepo::get_all`] when the caller needs it surfaced.
    ///
    /// # Errors
    /// [`PlaylistError::NotFound`] when the id matches nothing — **including when the
    /// visibility clause excludes it for a non-admin**, which requires [`get_sql`] to
    /// parenthesize the `Or` clause (see that function); without the parentheses the
    /// id stops restricting the owner branch and a different row comes back instead.
    /// Executed against the real Go repository: `Get` on an admin-owned private
    /// playlist as a non-admin gives `data not found` with a nil pointer, and a
    /// missing id gives the same. [`PlaylistError::Sqlite`] on a statement or
    /// hydration failure.
    pub fn get(
        &self,
        id: &str,
        user_id: &str,
        user: &User,
    ) -> Result<PlaylistRecord, PlaylistError> {
        let statement = get_sql(id, user_id, user);
        let records = self.read_hydrated(&statement)?;
        // Go hydrates the whole (0-or-1 element) slice only AFTER the len check
        // (`:182-187`), then takes `&list[0]`.
        if records.is_empty() {
            return Err(PlaylistError::NotFound);
        }
        let mut outcome = self.hydrate_artwork(records);
        // Go's swallow-and-log path: the page still renders (`artwork_hydration.go:62-66`).
        let _ = outcome.artwork_error;
        Ok(outcome.items.remove(0))
    }

    /// `FindByPath` (`:171-173`) → the same `findBy` with the `path` Eq. Executed
    /// against the real repository: the traced statement is
    /// `… WHERE path = {:p2}` — NO visibility clause, unlike `Get` — so a
    /// non-admin sees every playlist's file by path (scanner-side, headless). The
    /// NotFound rule is identical to [`PlaylistRepo::get`].
    ///
    /// # Errors
    /// [`PlaylistError::NotFound`] for a path with no row;
    /// [`PlaylistError::Sqlite`] on a statement or hydration failure.
    pub fn find_by_path(&self, path: &str, user_id: &str) -> Result<PlaylistRecord, PlaylistError> {
        let statement = find_by_path_sql(path, user_id);
        let mut records = self.read_hydrated(&statement)?;
        if records.is_empty() {
            return Err(PlaylistError::NotFound);
        }
        // Go hydrates ONLY the first row: `list := model.Playlists{pls[0].Playlist}`
        // (`:186`) before calling hydrateArtwork, so a duplicate-path table issues
        // `IN (1 id)` in Go and `IN (n ids)` here. Taking the row first keeps the
        // artwork IN-list identical; the returned record is the same either way
        // (`remove(0)` ≡ `pls[0]`). `playlist` has no unique index on `path`
        // (`nd-dbschema` index list), so duplicates are reachable.
        records.truncate(1);
        let mut outcome = self.hydrate_artwork(records);
        let _ = outcome.artwork_error;
        Ok(outcome.items.remove(0))
    }

    /// `GetAll` (`:196-210`): [`get_all_sql`] under the caller's visibility and
    /// sort-mapping legs, then artwork hydration.
    ///
    /// `order_by` is the caller's rendered sort mapping — Go maps `name` and
    /// `owner_name` through `naturalSort` (`:62-65`), and with natural sorting off
    /// the traced statement renders `ORDER BY playlist.name`. Passing `&[]` renders
    /// no ORDER BY at all, which is Go's no-options behavior.
    ///
    /// An empty result is an empty `Vec`, never [`PlaylistError::NotFound`]: Go
    /// returns `playlists` (a `make`d non-nil slice) with `err` from the `queryAll`
    /// call unchanged (`:199-209`).
    ///
    /// `artwork_error` carries Go's swallowed `log.Error`
    /// (`artwork_hydration.go:62-66`): the playlists come back either way, each with
    /// a zero (unresolved) image — executed against the real repository with
    /// `item_artwork` dropped, where every read still returned its rows.
    ///
    /// # Errors
    /// [`PlaylistError::Sqlite`] when the statement fails or a row will not hydrate.
    pub fn get_all(
        &self,
        user_id: &str,
        user: &User,
        order_by: &[String],
    ) -> Result<crate::artwork_hydration::ReadOutcome<PlaylistRecord>, PlaylistError> {
        let statement = get_all_sql(user_id, user, order_by);
        let records = self.read_hydrated(&statement)?;
        Ok(self.hydrate_artwork(records))
    }

    /// `GetPlaylists` (`:237-255`): the playlists containing `media_file_id`,
    /// [`get_playlists_sql`] joined to `playlist_tracks` and sorted by the `name`
    /// mapping, then artwork hydration.
    ///
    /// Go's `errors.Is(err, model.ErrNotFound)` arm (`:244-246`) — return an empty
    /// `Playlists` with a nil error — is DEAD: `queryAll` never produces
    /// `model.ErrNotFound`, because dbx's `All` yields an empty non-nil slice and
    /// the ErrNoRows mapping lives only in `Rows.one`/`Rows.row`
    /// (`dbx@v1.12.0/query.go:278-291`, `rows.go:237-244`,
    /// `sql_base_repository.go:451-454`). The port therefore has no special arm:
    /// a zero-row read is an empty `Vec` and a statement failure is
    /// [`PlaylistError::Sqlite`], which is what Go returns on that path (`:247`).
    ///
    /// # Errors
    /// [`PlaylistError::Sqlite`] when the statement fails or a row will not hydrate.
    /// Never [`PlaylistError::NotFound`] — see above.
    pub fn get_playlists(
        &self,
        media_file_id: &str,
        user_id: &str,
        user: &User,
        name_sort: &str,
    ) -> Result<crate::artwork_hydration::ReadOutcome<PlaylistRecord>, PlaylistError> {
        let statement = get_playlists_sql(media_file_id, user_id, user, name_sort);
        let records = self.read_hydrated(&statement)?;
        Ok(self.hydrate_artwork(records))
    }

    /// Phase 1 + phase 2 (Go's `queryAll` + `toModels()`): the raw rows through the
    /// shared seam, the `rules` shim decoded onto the record. A `PostScan` failure
    /// aborts the whole read, as `queryAll` does in Go.
    fn read_hydrated(&self, statement: &Sql) -> Result<Vec<PlaylistRecord>, PlaylistError> {
        let rows =
            crate::execution::query_all(self.conn, statement, crate::hydrate::row_to_playlist)?;
        // Go's read funnel leaves a PostScan failure as a column-conversion error,
        // so `into_sqlite` is the exit shape and `PlaylistError::Sqlite` wraps it.
        Ok(rows
            .into_iter()
            .map(PlaylistRow::into_record)
            .collect::<Result<Vec<PlaylistRecord>, crate::hydrate::HydrateError>>()
            .map_err(crate::hydrate::HydrateError::into_sqlite)?)
    }

    /// Phase 3 (`hydrateArtwork`, `:191-194`): one batched `pl`-kind lookup over
    /// the hydrated records. Chunking and the no-dedup id list are
    /// [`crate::artwork_hydration`]'s behavior, not this method's.
    fn hydrate_artwork(
        &self,
        records: Vec<PlaylistRecord>,
    ) -> crate::artwork_hydration::ReadOutcome<PlaylistRecord> {
        let mut items = records;
        let mut artwork_error = None;
        crate::artwork_hydration::hydrate_playlist_artwork(
            self.conn,
            &mut items,
            &mut artwork_error,
        );
        crate::artwork_hydration::ReadOutcome {
            items,
            artwork_error,
        }
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
    fn get_sql_parenthesizes_the_or_visibility_clause_for_a_non_admin() {
        // Go builds `And{Eq{"playlist.id": id}, userFilter(ctx)}`
        // (`playlist_repository.go:149`), and squirrel wraps every conjunct group in
        // parentheses (`squirrel@v1.5.4/expr.go:384-386`). Executed against the pinned
        // squirrel, the non-admin statement is
        //   WHERE (playlist.id = ? AND (public = ? OR owner_id = ?)).
        // The port renders the flat conjunction squirrel would produce from the same
        // parts, so the `Or` clause must keep its own parentheses: unparenthesized,
        // SQL binds `AND` tighter than `OR`, so the predicate becomes
        // `(id AND public) OR owner_id` and a non-admin asking for an invisible id
        // gets a DIFFERENT visible row instead of NotFound (see `get_sql`'s comment
        // for the executed rows).
        // Go's `loggedUser(ctx).ID` is the SAME value in both places — the annotation
        // join's `user_id` (`sql_annotations.go:64`) and userFilter's `owner_id`
        // (`playlist_repository.go:90`) — so the test passes it as both args.
        let (sql, args) = get_sql("pl-1", "lim", &Some("lim".to_string()));
        assert_eq!(
            sql,
            "SELECT playlist.*, user.user_name as owner_name, \
             coalesce(starred, 0) as starred, coalesce(rating, 0) as rating, starred_at, \
             play_date, rated_at, coalesce(play_count, 0) as play_count, playlist.average_rating \
             FROM playlist JOIN user ON user.id = owner_id \
             LEFT JOIN annotation ON (annotation.item_id = playlist.id AND \
             annotation.item_type = ? AND annotation.user_id = ?) \
             WHERE playlist.id = ? AND (public = ? OR owner_id = ?)"
        );
        // Bind order is unchanged by the parentheses: annotation kind, annotation user,
        // then the id, then the visibility pair.
        assert_eq!(
            args,
            vec![
                Value::Text("playlist".to_string()),
                Value::Text("lim".to_string()),
                Value::Text("pl-1".to_string()),
                Value::Bool(true),
                Value::Text("lim".to_string()),
            ]
        );
    }

    #[test]
    fn admin_get_sql_keeps_the_identity_visibility_clause_absent() {
        // userFilter returns `And{}` for an admin (`:85-86`), which squirrel renders as
        // the traced `AND (1=1)`; the port omits the clause and binds nothing extra
        // (the documented identity residual).
        let (sql, args) = get_sql("pl-1", "adm", &None);
        assert!(sql.ends_with("WHERE playlist.id = ?"), "{sql}");
        assert_eq!(args.len(), 3, "only the annotation pair and the id bind");
    }

    #[test]
    fn get_all_sql_wraps_the_visibility_clause_for_text_parity() {
        // `GetAll` passes userFilter to `Where` directly (`:197`), so squirrel renders
        // `WHERE (public = ? OR owner_id = ?)` (executed). The port wraps it to match,
        // which also keeps a future conjunct from inheriting `get_sql`'s hazard.
        let (sql, args) = get_all_sql(
            "lim",
            &Some("lim".to_string()),
            &["playlist.name".to_string()],
        );
        assert!(
            sql.contains("WHERE (public = ? OR owner_id = ?) ORDER BY playlist.name"),
            "{sql}"
        );
        assert_eq!(args.len(), 4);
        let (admin_sql, _) = get_all_sql("adm", &None, &[]);
        assert!(
            !admin_sql.contains("WHERE"),
            "an admin sees everything: {admin_sql}"
        );
    }
    #[test]
    fn smart_playlist_filter_renders_is_null_with_one_bound_arg() {
        // Go builds `Or{Eq{"rules": ""}, Eq{"rules": nil}}` (`playlist_repository.go:76-81`).
        // squirrel turns the nil Eq into `rules IS NULL` with NO placeholder and wraps the
        // Or (`squirrel@v1.5.4/expr.go:182-183`, the `if val == nil` arm, plus the
        // conjunction wrap at `:384-386`) - executed against the
        // pinned squirrel in `/tmp/sd3f_p1/args.go`:
        //   sql = (rules = ? OR rules IS NULL), len(args) == 1.
        // The port previously emitted `rules = ? OR rules = ?` with a NULL arg; SQL
        // three-valued logic makes `rules = NULL` never true, which silently dropped the
        // NULL-rules rows Go's filter includes. See the executed row set in
        // `runtime_hydrate_playlist_radio_artwork.rs::smart_filter_clause_includes_a_null_rules_row`.
        let (sql, args) = smart_playlist_filter();
        assert_eq!(sql, "(rules = ? OR rules IS NULL)");
        assert_eq!(args, [Value::Text(String::new())]);
    }

    #[test]
    fn playlist_filter_ands_the_parts_inside_each_field_and_ors_the_fields() {
        // `Or{substringFilter(name), substringFilter(comment)}` (`:69-74`) where each
        // `substringFilter` (`sql_restful.go:119-126`) is an `And` over the split parts.
        // Executed against the pinned squirrel (`/tmp/sd3f_p1/q.go`):
        //   ((playlist.name LIKE ? AND playlist.name LIKE ?)
        //    OR (playlist.comment LIKE ? AND playlist.comment LIKE ?))
        // with args [%rah%, %frahm%, %rah%, %frahm%] - all name legs before all comment
        // legs, matching Go's `Or{first, second}` render order.
        let (sql, args) = playlist_filter("rah frahm");
        assert_eq!(
            sql,
            "((playlist.name LIKE ? AND playlist.name LIKE ?) \
             OR (playlist.comment LIKE ? AND playlist.comment LIKE ?))"
        );
        assert_eq!(
            args,
            [
                Value::Text("%rah%".to_string()),
                Value::Text("%frahm%".to_string()),
                Value::Text("%rah%".to_string()),
                Value::Text("%frahm%".to_string()),
            ]
        );
    }

    #[test]
    fn playlist_filter_with_one_word_is_a_two_field_or() {
        // Executed (`/tmp/sd3f_p1/q.go`, `q one word`):
        //   ((playlist.name LIKE ?) OR (playlist.comment LIKE ?))
        // Each leg keeps its own parentheses because squirrel wraps every conjunction
        // group, so joining this clause with another conjunct cannot leak OR past AND.
        let (sql, args) = playlist_filter("rah");
        assert_eq!(sql, "((playlist.name LIKE ?) OR (playlist.comment LIKE ?))");
        assert_eq!(
            args,
            [
                Value::Text("%rah%".to_string()),
                Value::Text("%rah%".to_string()),
            ]
        );
    }

    #[test]
    fn empty_playlist_query_matches_everything_not_nothing() {
        // Go's `Or` is NOT empty for `q = ""`: its two legs are empty `And`s, and squirrel
        // renders an empty conjunction as `1=1` (`expr.go`, `sqlTrue`). Executed
        // (`/tmp/sd3f_p1/q.go`, `q empty`): `((1=1) OR (1=1))`, no args - it matches every
        // row. `1=0` is what an empty `Or` renders (`sqlFalse`), and Go never builds one
        // here; the port previously returned `1=0` and matched nothing.
        let (sql, args) = playlist_filter("");
        assert_eq!(sql, "((1=1) OR (1=1))");
        assert!(args.is_empty());
        let (whitespace_only, _) = playlist_filter("   ");
        assert_eq!(
            whitespace_only, "((1=1) OR (1=1))",
            "Go uses strings.Fields"
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
    fn tracks_query_binds_the_library_filter_before_the_playlist() {
        // Arrange/Act: tracksQuery applies applyLibraryFilter FIRST (:375) and its
        // own Where(Eq{"playlist_id": id}) LAST (:396).
        let filter = (
            "media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE \
             ul.user_id = ?)"
                .to_string(),
            vec![Value::Text("u1".to_string())],
        );
        let (sql, args) = load_tracks_sql("pl-1", "u1", Some(&filter));
        // Assert: the alias-spelled filter (Go's "f" alias) precedes the playlist
        // clause, and its arg binds first.
        assert!(sql.contains("WHERE f.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?) AND playlist_id = ?"));
        assert!(!sql.contains("media_file.library_id"), "Go's alias is f");
        assert_eq!(
            args,
            vec![
                Value::Text("u1".to_string()),
                Value::Text("pl-1".to_string())
            ]
        );
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

    fn test_dt(hms: &str) -> DateTime<Utc> {
        use chrono::TimeZone;
        let parts: Vec<u32> = hms.split(':').map(|v| v.parse().unwrap()).collect();
        chrono::Utc
            .with_ymd_and_hms(2024, 7, 1, parts[0], parts[1], parts[2])
            .unwrap()
    }

    /// A distinct non-zero value per column so a rotation fails; non-smart
    /// (no rules).
    fn test_record() -> PlaylistRecord {
        PlaylistRecord {
            // Read-path identity: never in playlist_row (the repo appends the id).
            id: "pl-1".to_string(),
            name: "Mix".to_string(),
            comment: "c-mix".to_string(),
            duration: 47.5,
            size: 987654,
            song_count: 33,
            owner_id: "u-1".to_string(),
            public: true,
            path: "p-mix".to_string(),
            sync: true,
            uploaded_image: "up-mix".to_string(),
            external_image_url: "ext-mix".to_string(),
            created_at: Some(test_dt("01:02:03")),
            updated_at: test_dt("08:09:10"),
            imported_hash: "ih-mix".to_string(),
            rules: None,
            evaluated_at: None,
            // Derived, hydration-owned: never in playlist_row.
            annotation: nd_model::annotation::Annotation::default(),
            // Second-layer artwork (`structs:"-"`): never in playlist_row.
            image: crate::artwork_hydration::ItemImage::default(),
            // Read-path-only (`structs:"-"`): hydrated from the `owner_name`
            // alias, never in playlist_row.
            owner_name: "jdoe".to_string(),
        }
    }

    fn smart_record() -> PlaylistRecord {
        let mut p = test_record();
        let mut fields = nd_model::Fields::new();
        fields.insert(
            "artist".to_string(),
            serde_json::Value::String("Frahm".to_string()),
        );
        p.name = "SmartMix".to_string();
        p.comment = "c-smart".to_string();
        p.duration = 55.5;
        p.size = 444444;
        p.song_count = 21;
        p.path = "p-smart".to_string();
        p.sync = false;
        p.uploaded_image = "up-smart".to_string();
        p.external_image_url = "ext-smart".to_string();
        p.imported_hash = "ih-smart".to_string();
        p.evaluated_at = Some(test_dt("06:07:08"));
        p.rules = Some(nd_model::Criteria {
            expression: nd_model::Expression::Is(fields),
            ..nd_model::Criteria::default()
        });
        p
    }

    #[test]
    fn is_smart_playlist_follows_go_predicate() {
        // Arrange/Act/Assert: IsSmartPlaylist (playlist.go:46-48) —
        // Rules != nil && Expression != nil; the port's nil stand-in is
        // the empty All group.
        assert!(!is_smart_playlist(&None));
        assert!(is_smart_playlist(&smart_record().rules));
        let empty = nd_model::Criteria::default();
        assert!(!is_smart_playlist(&Some(empty)));
    }

    #[test]
    fn playlist_row_smart_stores_rules_json_and_drops_counters() {
        // Arrange/Act: the PostMapArgs smart arm (:38-45).
        let row = playlist_row(&smart_record()).expect("smart criteria marshals");
        // Assert: rules is the marshal_criteria JSON; the
        // evaluation-owned counters are absent from the map.
        let rules = row
            .iter()
            .find(|(c, _)| *c == "rules")
            .map(|(_, v)| v.clone());
        assert!(matches!(rules, Some(Value::Text(_))), "rules must be JSON");
        if let Some(Value::Text(text)) = rules {
            let c = smart_record().rules.clone().unwrap();
            assert_eq!(
                text,
                nd_model::criteria::marshal_criteria(&c).expect("criteria marshals")
            );
        }
        for c in SMART_OWNED_COUNTER_COLS {
            assert!(!row.contains(c), "{c} must be deleted for a smart put");
        }
        // And the rest of the columns are present: 16 non-id columns
        // minus the 3 deleted counters = 13.
        assert_eq!(row.len(), 13);
        assert!(!row.contains("id"));
    }

    #[test]
    fn put_rejects_invalid_smart_criteria_instead_of_persisting_null() {
        let mut record = smart_record();
        let rules = record.rules.as_mut().expect("smart playlist has rules");
        let mut fields = nd_model::Fields::new();
        fields.insert("a".to_string(), serde_json::Value::from(1));
        fields.insert("b".to_string(), serde_json::Value::from(2));
        rules.expression = nd_model::Expression::Is(fields);

        let error = put_sql("pl-1", &record, Utc::now(), &[])
            .expect_err("Go's PostMapArgs rejects a two-field leaf");
        assert!(
            matches!(&error, crate::marshal::MarshalError::Criteria(message)
                if message.contains("invalid is expression length 2")),
            "got {error}"
        );
    }

    #[test]
    fn playlist_row_non_smart_deletes_rules() {
        // Arrange/Act: the PostMapArgs non-smart arm (:49).
        let row = playlist_row(&test_record()).expect("non-smart row marshals");
        // Assert: no rules column; the counters ARE present;
        // evaluated_at None binds NULL.
        assert!(!row.contains("rules"));
        for c in SMART_OWNED_COUNTER_COLS {
            assert!(row.contains(c));
        }
        assert_eq!(
            row.len(),
            15,
            "15 non-id columns: the 17 minus id and rules"
        );
        assert!(!row.contains("id"));
        let evaluated: Vec<Value> = row
            .iter()
            .filter(|(c, _)| *c == "evaluated_at")
            .map(|(_, v)| v.clone())
            .collect();
        assert_eq!(evaluated, vec![Value::Null]);
    }

    #[test]
    fn put_sql_renders_record_driven_update_and_insert() {
        // Arrange: full non-smart record + a distinct now.
        let p = test_record();
        let now = test_dt("14:15:16");
        // Act:
        let put = put_sql("pl-1", &p, now, &[]).expect("renders");
        // Assert: UPDATE — sorted SET columns, no created_at, no id in
        // SET, id bound last in WHERE.
        assert_eq!(
            put.update.0,
            "UPDATE playlist SET comment = ?, duration = ?, evaluated_at = ?, \
             external_image_url = ?, imported_hash = ?, name = ?, owner_id = ?, \
             path = ?, public = ?, size = ?, song_count = ?, sync = ?, \
             updated_at = ?, uploaded_image = ? WHERE id = ?"
        );
        assert_eq!(put.update.1.len(), 15);
        assert_eq!(
            put.update.1[5],
            Value::Text("Mix".to_string()),
            "name position"
        );
        assert_eq!(put.update.1[8], Value::Bool(true), "public position");
        assert_eq!(put.update.1[10], Value::Integer(33), "song_count position");
        assert_eq!(
            put.update.1[12],
            Value::Text(crate::time::to_text(&now)),
            "updated_at re-stamped to now"
        );
        assert_eq!(
            put.update.1[14],
            Value::Text("pl-1".to_string()),
            "id binds last"
        );
        // Assert: INSERT — sorted columns with created_at, id appended
        // LAST in both lists.
        assert_eq!(
            put.insert.0,
            "INSERT INTO playlist (comment, created_at, duration, evaluated_at, external_image_url, imported_hash, name, owner_id, path, public, size, song_count, sync, updated_at, uploaded_image, id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        );
        assert_eq!(
            put.insert.1,
            vec![
                Value::Text("c-mix".to_string()),
                Value::Text(crate::time::to_text(&p.created_at.unwrap())),
                Value::Real(47.5),
                Value::Null,
                Value::Text("ext-mix".to_string()),
                Value::Text("ih-mix".to_string()),
                Value::Text("Mix".to_string()),
                Value::Text("u-1".to_string()),
                Value::Text("p-mix".to_string()),
                Value::Bool(true),
                Value::Integer(987654),
                Value::Integer(33),
                Value::Bool(true),
                Value::Text(crate::time::to_text(&now)),
                Value::Text("up-mix".to_string()),
                Value::Text("pl-1".to_string()),
            ]
        );
    }

    #[test]
    fn put_sql_stamps_created_at_when_absent_and_rejects_empty_update_set() {
        // Arrange: a never-stamped row (Go zero CreatedAt, isNew).
        let mut p = test_record();
        p.created_at = None;
        // Act:
        let put = put_sql("pl-1", &p, Utc::now(), &[]).expect("renders");
        // Assert: created_at was stamped (TEXT, not NULL) on the insert.
        assert!(matches!(put.insert.1[1], Value::Text(_)));
        // And a created_at-only restriction is the typed error, not a
        // degenerate UPDATE.
        let err = put_sql("pl-1", &p, Utc::now(), &["created_at"])
            .expect_err("must be empty after the drop");
        assert_eq!(err, crate::marshal::MarshalError::EmptyUpdateSet);
    }

    #[test]
    fn put_sql_restricted_update_normalizes_camel_case_cols() {
        // Arrange: Go's Update appends "updatedAt" to the caller's cols
        // (:435); camelCase names are toSnakeCase'd by the seam.
        let p = test_record();
        // Act:
        let put = put_sql("pl-1", &p, Utc::now(), &["Public", "UpdatedAt"]).expect("renders");
        // Assert: only the normalized requested columns are SET; the id
        // closes the args.
        assert_eq!(
            put.update.0,
            "UPDATE playlist SET public = ?, updated_at = ? WHERE id = ?"
        );
        assert_eq!(put.update.1.len(), 3);
        assert_eq!(put.update.1[2], Value::Text("pl-1".to_string()));
        // A full update never SETs created_at.
        let full = put_sql("pl-1", &p, Utc::now(), &[]).expect("renders");
        assert!(!full.update.0.contains("created_at = ?"));
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
