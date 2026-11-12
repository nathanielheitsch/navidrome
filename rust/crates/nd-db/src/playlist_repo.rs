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
//! cursor machinery (`streamByIDs`), the artwork hydration
//! (`hydrateArtwork`, `:191-194`, per-row image loads) and the `Tracks(...)`
//! sub-repository binding (`playlist_track_repository.go:38-71` — that lane's
//! own file) need either the streaming/execution layer or the sibling lane's
//! surface; the select shapes they consume are pinned here
//! ([`tracks_query`], [`get_all_ids_query`]). `rest.Repository`'s
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
/// stores. `evaluated_at` is `Option` (Go `*time.Time`): `None` binds
/// NULL.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistRecord {
    /// `name` (Go `Name`).
    pub name: String,
    /// `comment` (Go `Comment`).
    pub comment: String,
    /// `duration` (Go `Duration`, `float32` at `model/playlist.go:25`). The
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
/// `id` never appears in the map — the seam rejects it.
#[must_use]
pub fn playlist_row(p: &PlaylistRecord) -> crate::marshal::ColumnMap {
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
            Value::Text(nd_model::criteria::marshal_criteria(
                p.rules.as_ref().unwrap(),
            )),
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
    map
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
    let map = playlist_row(&stamped);
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
        let row = playlist_row(&smart_record());
        // Assert: rules is the marshal_criteria JSON; the
        // evaluation-owned counters are absent from the map.
        let rules = row
            .iter()
            .find(|(c, _)| *c == "rules")
            .map(|(_, v)| v.clone());
        assert!(matches!(rules, Some(Value::Text(_))), "rules must be JSON");
        if let Some(Value::Text(text)) = rules {
            let c = smart_record().rules.clone().unwrap();
            assert_eq!(text, nd_model::criteria::marshal_criteria(&c));
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
    fn playlist_row_non_smart_deletes_rules() {
        // Arrange/Act: the PostMapArgs non-smart arm (:49).
        let row = playlist_row(&test_record());
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
