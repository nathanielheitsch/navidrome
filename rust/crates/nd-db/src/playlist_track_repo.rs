//! Port of `persistence/playlist_track_repository.go` (326L) — the tracks of
//! one playlist, including the position bookkeeping.
//!
//! Go binds the repository to `playlistId` at construction (`Tracks`,
//! `playlist_track_repository.go:38-71`) and reads the logged-in user from
//! the ambient `ctx`; the crate has no ambient context (the `sqlrepo.rs` DI
//! rule), so the port threads `playlist_id`, `user_id` and the library filter
//! explicitly through the functions. `applyLibraryFilter`
//! (`sql_base_repository.go:261-283`) is a raw fragment parameterized by its
//! table alias; the caller decides (admin / full-access users skip it, per
//! `userSeesAllLibraries`, `:287-295`) by passing `Some(user_id)` or `None`.
//!
//! Every Go method builds a squirrel statement and hands it to
//! `executeSQL`/`queryOne`/`queryAll`/`count`, which materialize through
//! `toSQL()` (`sql_base_repository.go:358-372`) as `(sql, args)`; execution and
//! logging stay with the caller (`store.rs`), so this port stops at the same
//! [`Sql`] pairs. Squirrel joins every `.Where()` with AND and renders joins
//! before the WHERE; the builder reproduces that order, so multi-statement
//! sequences (Insert's negate/flip pair, Reorder's four steps, Delete's
//! chunking) keep Go's exact literal SQL — they rely on raw `Expr` updates the
//! port spells out verbatim.
//!
//! `addMediaFileIds`-shaped selectors (`:152-170` — AddAlbums/AddArtists/
//! AddDiscs) select candidate `media_file.id`s the caller then feeds back
//! through `add_tracks`; the participant subquery for `AddArtists`
//! (`ParticipantIDFilter`, `sql_participations.go:28-45`) is spelled out with
//! its role list in the where clause.
//!
//! S-D3e landed the READ side: [`PlaylistTrackRow`] (Go's `dbPlaylistTrack`
//! phase 1) plus [`PlaylistTrackRecord`] and the executed
//! [`PlaylistTrackRepo::read`] / [`PlaylistTrackRepo::get_all`], which run
//! [`read_sql`] and `playlist_repo.rs`'s [`crate::playlist_repo::tracks_query`]
//! through [`crate::execution`] and hydrate the joined `media_file` columns on
//! the shared seam ([`crate::hydrate::row_to_playlist_track`] →
//! [`crate::hydrate::playlist_track_post_scan`]). Playlist-track artwork is Go's
//! `hydratePlaylistTrackArtwork` (`artwork_hydration.go:169-181`), i.e.
//! [`crate::artwork_hydration::hydrate_media_file_artwork`] over the tracks'
//! media files — there is no `pt` artwork kind, so the media-file wrapper is
//! reused rather than a new lookup ([`hydrate_track_artwork`]).
//!
//! Not ported, with the reason: `GetCursor` (`:131-143`) needs the streaming
//! cursor machinery (`queryWithStableResults`) that the crate does not own; its
//! tracks-query shape is already pinned by `playlist_repo.rs`'s `tracks_query`
//! (shared by `loadTracks` at `playlist_repository.go:401` and by `GetCursor` at
//! `playlist_track_repository.go:132`). `crate::playlist_repo::get_all_sql` also
//! cannot express Go's `QueryOptions.Filters` yet — see its docs.
//!
//! `get_all` DOES forward its `model.QueryOptions` into `newSelect`
//! (`playlist_track_repository.go:124`) — unlike artist's `Get`/`GetAll`
//! (`artist_repository.go:255-267`, `:269-277`), which do not. Executed against the
//! real repository, `Sort: "title", Order: "desc", Max: 2, Offset: 1` emits
//! `ORDER BY order_title desc LIMIT 2 OFFSET 1` (the `title` mapping comes from the
//! sortMappings at `playlist_track_repository.go:54-68`) and a filter emits BEFORE
//! the library leg and the playlist id, because `newSelect` applies options and
//! filters first (`sql_base_repository.go:139-147`) and `tracksQuery` appends its
//! own `Where` last (`playlist_repository.go:396`). This port renders none of
//! those legs — see [`crate::playlist_track_repo::PlaylistTrackRepo::get_all`].

use crate::mediafile_repo::MediaFileRecord;
use crate::sqlrepo::{Repository, Value};

/// The `playlist_tracks` table (`:49`, `p.tableName = "playlist_tracks"`).
pub const TABLE: &str = "playlist_tracks";

/// A materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// `model.PlaylistTrack` (`model/playlist.go:226-231`): one row of
/// `playlist_tracks`, whose `id` is the 1-based POSITION rather than the media
/// file's identity. Go's `MediaFile` embed is promoted, so the hydrated media
/// file rides on the same record and a read returns one flat value per row.
///
/// The media file's `id` is NOT the row's `id`: `dbPlaylistTrack.PostScan`
/// overwrites it from `media_file_id` (`playlist_track_repository.go:32`), which
/// is exactly how a track can carry a media file whose id differs from its
/// position column (see [`crate::hydrate::apply_embedded_mediafile_id`]).
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistTrackRecord {
    /// Go `ID` — the position. Scanned from the LAST column named `id` (i.e.
    /// `playlist_tracks.id`) and rendered as Go's `string` even though the
    /// column is INTEGER (see [`crate::hydrate::row_to_playlist_track`]).
    pub id: String,
    /// Go `MediaFileID` — the FK to `media_file.id`, and the source of
    /// [`PlaylistTrackRecord::media`]'s `id`.
    pub media_file_id: String,
    /// Go `PlaylistID`.
    pub playlist_id: String,
    /// Go's promoted `MediaFile` embed: hydrated by the delegated
    /// `dbMediaFile.PostScan`, then id-overwritten.
    pub media: MediaFileRecord,
}

impl PlaylistTrackRecord {
    /// `PlaylistTracks.MediaFiles()` (`model/playlist.go:235-241`), which is
    /// also the projection step of `hydratePlaylistTrackArtwork`
    /// (`artwork_hydration.go:173-176`): the media files of a batch of tracks in
    /// track order — the slice Go hands to `hydrateMediaFileArtwork`.
    #[must_use]
    pub fn media_files(tracks: &[PlaylistTrackRecord]) -> Vec<MediaFileRecord> {
        tracks.iter().map(|t| t.media.clone()).collect()
    }

    /// The write-back step (`artwork_hydration.go:179-181`): replace each
    /// track's media file with the hydrated one at the SAME index. Positional,
    /// not id-keyed — two tracks can point at the same media file.
    pub fn set_media_files(tracks: &mut [PlaylistTrackRecord], mfs: Vec<MediaFileRecord>) {
        assert_eq!(
            tracks.len(),
            mfs.len(),
            "hydratePlaylistTrackArtwork builds the slice from the tracks, so lengths match"
        );
        for (track, media) in tracks.iter_mut().zip(mfs) {
            track.media = media;
        }
    }
}

/// `dbPlaylistTrack` (`playlist_track_repository.go:22-25`) — phase 1 of the
/// scan: the joined `media_file` columns with the two JSON shims still unparsed
/// (inherited from [`crate::mediafile_repo::MediaFileRow`]), plus the track's own identity columns.
///
/// `dbPlaylistTrack` adds NO `structs:"-"` shim of its own — unlike
/// [`crate::artist_repo::ArtistRow`] or [`crate::mediafile_repo::MediaFileRow`], which own shims — so
/// there is nothing here beyond the media-file raw record and the three columns
/// `playlist_tracks` contributes. Phase 2 is
/// [`crate::hydrate::playlist_track_post_scan`], reachable through
/// [`PlaylistTrackRow::into_record`] like [`crate::mediafile_repo::MediaFileRow::into_record`].
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistTrackRow {
    /// `dbMediaFile`'s record, shims still empty (Go's embedded `dbMediaFile`).
    pub record: MediaFileRecord,
    /// The raw `participants` TEXT column, unparsed (the inherited
    /// `dbMediaFile.Participants`, `mediafile_repository.go:31`). Decoded
    /// UNGUARDED, and a failure keeps Go's **media-file** prefix — the track hook
    /// adds none of its own (`playlist_track_repository.go:27-30`).
    pub participants: String,
    /// The raw `tags` TEXT column, unparsed (the inherited
    /// `dbMediaFile.Tags`, `:32`).
    pub tags: String,
    /// `playlist_tracks.id`, the position. Read from the LAST column named `id`,
    /// because both Go selects also emit the media file's `id` and dbx binds a
    /// duplicated field name from the LAST column scanned (see
    /// [`crate::hydrate::row_to_playlist_track`]).
    pub position: String,
    /// `playlist_tracks.media_file_id`.
    pub media_file_id: String,
    /// `playlist_tracks.playlist_id`.
    pub playlist_id: String,
}

impl PlaylistTrackRow {
    /// `dbPlaylistTrack.PostScan` (`playlist_track_repository.go:27-34`): thin
    /// delegation onto [`crate::hydrate::playlist_track_post_scan`], matching
    /// [`crate::mediafile_repo::MediaFileRow::into_record`]'s shape.
    ///
    /// # Errors
    /// [`crate::hydrate::HydrateError::Decoded`] for a bad `participants`/`tags`
    /// cell (carrying Go's media-file prefix), or a transparent SQLite error.
    pub fn into_record(self) -> Result<PlaylistTrackRecord, crate::hydrate::HydrateError> {
        crate::hydrate::playlist_track_post_scan(self)
    }
}

/// `deleteChunkSize` (`:220`): rows per DELETE statement, matching
/// SQLITE_MAX_VARIABLE_NUMBER.
pub const DELETE_CHUNK_SIZE: usize = 200;

/// `slices.Chunk(mediaFileIds, 200)` of `addTracks`
/// (`playlist_repository.go:301`): rows per multi-row INSERT.
pub const INSERT_CHUNK_SIZE: usize = 200;

/// The user filter fragment of `applyLibraryFilter`
/// (`sql_base_repository.go:261-283`): `None` is the admin/full-access skip
/// (`userSeesAllLibraries`, `:287-295`), `Some(user_id)` the subquery form.
///
/// Alias contract: Go always passes the alias explicitly (the
/// `applyLibraryFilter(ctx, sq, "f")` and `("mf")` call sites at
/// `playlist_track_repository.go:94`, `:117`, `:150`, `:162` and
/// `playlist_repository.go:375`), while this form hardcodes the unaliased
/// `media_file.library_id`. Every builder in this module joins `media_file` under an
/// alias (`f`, or `mf` in [`get_album_ids_query`]), so handing THIS fragment to them is
/// a parse error rather than a wider filter. Executed in SQLite against the
/// `INNER JOIN media_file f ON f.id = media_file_id` shape with this clause appended,
/// the statement fails with `no such column: media_file.library_id` while the aliased
/// form returns the row. Callers of the builder-shaped queries must pass
/// [`library_filter_on`] naming the join's alias. [`read_sql`] splices the fragment
/// verbatim as well, so it carries the same requirement - its executed callers pass
/// `library_filter_on(…, "f")`; only [`crate::playlist_repo::tracks_query`] rewrites
/// this fragment's prefix to `f.library_id` itself.
#[must_use]
pub fn library_filter(user_id: Option<&str>) -> Option<Sql> {
    user_id.map(|uid| {
        (
            "media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)".to_string(),
            vec![Value::Text(uid.to_string())],
        )
    })
}

/// The library filter bound against a table alias other than `media_file`
/// (the `applyLibraryFilter(ctx, sq, "f")`/`("mf")` call sites at
/// `playlist_track_repository.go:94`, `:117`, `:150`, `:162`, and
/// `playlist_repository.go:375`). This is the form the builder-shaped queries in
/// this module require; see [`library_filter`].
#[must_use]
pub fn library_filter_on(user_id: Option<&str>, table: &str) -> Option<Sql> {
    user_id.map(|uid| {
        (
            format!(
                "{table}.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
            ),
            vec![Value::Text(uid.to_string())],
        )
    })
}

/// Applies a library filter fragment to a builder as a WHERE clause
/// (`applyLibraryFilter`'s `sq.Where(Expr(...))`, `:281-282`). The fragment is
/// spliced in verbatim with no alias rewriting, so it must already name the
/// alias `sq` joins `media_file` under - see [`library_filter`].
#[must_use]
fn with_library_filter(sq: Repository, filter: Option<&Sql>) -> Repository {
    match filter {
        Some((sql, args)) => sq.where_raw(sql.clone(), args.clone()),
        None => sq,
    }
}

/// `CountAll` (`:73-78`): inner-join count of the playlist's tracks, library
/// filter applied. `count`'s base shape (`sql_base_repository.go:566-576`)
/// comes from the caller's `count` helpers on the rendered select.
#[must_use]
pub fn count_all_query(playlist_id: &str, library_filter: Option<&Sql>) -> Repository {
    let sq = Repository::new(TABLE)
        .inner_join("media_file f", "f.id = media_file_id")
        .where_eq("playlist_id", playlist_id);
    with_library_filter(sq, library_filter)
}

/// `Count` (`:80-86`): the LEFT JOIN counts missing tracks too — the REST
/// count must agree with the filtered listing, orphan rows included. The
/// builder emits `INNER JOIN`; the caller renders the LEFT form by dropping
/// the builder's join and adding a left-join fragment, which the crate's
/// `Repository` does not model (see [`crate::sqlrepo`]).
#[must_use]
pub fn count_query(playlist_id: &str, library_filter: Option<&Sql>) -> Repository {
    let sq = Repository::new(TABLE)
        .inner_join("media_file f", "f.id = media_file_id")
        .where_eq("playlist_id", playlist_id);
    with_library_filter(sq, library_filter)
}

/// `Read` (`:98-121`): one track row with the logged-in user's annotation
/// columns, restricted to the playlist and the 1-based position `id`.
/// The annotation LEFT JOIN embeds `user_id` exactly as Go string-concatenates
/// it (`:101-104`); the port keeps Go's literal SQL, so the user id stays an
/// interpolated fixed-format fragment rather than a bound arg.
#[must_use]
pub fn read_sql(playlist_id: &str, id: &str, user_id: &str, library_filter: Option<&Sql>) -> Sql {
    // The builder models only INNER JOIN; the LEFT JOIN is spelled out like
    // Go's literal on-clause, with the user id interpolated exactly as Go
    // concatenates it (`:101-104`).
    let mut args = vec![
        Value::Text(playlist_id.to_string()),
        Value::Text(id.to_string()),
    ];
    let library_clause = match library_filter {
        Some((sql, lib_args)) => {
            args.extend(lib_args.iter().cloned());
            format!(" AND {sql}")
        }
        None => String::new(),
    };
    (
        format!(
            "SELECT coalesce(starred, 0) as starred, coalesce(play_count, 0) as play_count, \
             coalesce(rating, 0) as rating, starred_at, play_date, rated_at, f.*, playlist_tracks.* \
             FROM playlist_tracks \
             LEFT JOIN annotation ON (annotation.item_id = media_file_id AND \
             annotation.item_type = 'media_file' AND annotation.user_id = '{user_id}') \
             INNER JOIN media_file f ON f.id = media_file_id \
             WHERE playlist_id = ? AND playlist_tracks.id = ?{library_clause}"
        ),
        args,
    )
}

/// `GetMediaFileIDs` (`:130-139`): the song ids in playlist order, for
/// callers that need every id but no track data.
#[must_use]
pub fn get_media_file_ids_query(playlist_id: &str, library_filter: Option<&Sql>) -> Repository {
    let sq = Repository::new(TABLE)
        .select(["media_file_id"])
        .inner_join("media_file f", "f.id = media_file_id")
        .where_eq("playlist_id", playlist_id);
    with_library_filter(sq, library_filter)
}

/// `GetAlbumIDs` (`:141-150`): the distinct albums of the playlist's tracks.
#[must_use]
pub fn get_album_ids_query(playlist_id: &str, library_filter: Option<&Sql>) -> Repository {
    let sq = Repository::new(TABLE)
        .select(["distinct mf.album_id"])
        .inner_join("media_file mf", "mf.id = media_file_id")
        .where_eq("playlist_id", playlist_id);
    with_library_filter(sq, library_filter)
}

/// The `max(id)` probe of `Add` (`:160-163`) and `Reorder` (`:238-240`):
/// `SELECT max(id) as max FROM playlist_tracks WHERE playlist_id = ?`.
#[must_use]
pub fn max_position_sql(playlist_id: &str) -> Sql {
    let (sql, args) = Repository::new(TABLE)
        .select(["max(id) as max"])
        .where_eq("playlist_id", playlist_id)
        .to_sql()
        .expect("the max select materializes");
    (sql, args)
}

/// `addTracks` (`playlist_repository.go:291-316`): the chunked multi-row
/// INSERTs appending `media_file_ids` from `starting_pos`, one statement per
/// `INSERT_CHUNK_SIZE` ids. Values are bound in row order (playlist_id,
/// media_file_id, position). The caller pre-filters through
/// `keep_accessible_sql` (`playlist_repo.rs`) exactly as Go does.
#[must_use]
pub fn add_tracks_sql(playlist_id: &str, starting_pos: i64, media_file_ids: &[String]) -> Vec<Sql> {
    media_file_ids
        .chunks(INSERT_CHUNK_SIZE)
        .enumerate()
        .map(|(chunk_idx, chunk)| {
            let mut rows = String::new();
            let mut args = Vec::new();
            for (i, id) in chunk.iter().enumerate() {
                if i > 0 {
                    rows.push_str(", ");
                }
                rows.push_str("(?, ?, ?)");
                let pos = starting_pos + (chunk_idx * INSERT_CHUNK_SIZE + i) as i64;
                args.push(Value::Text(playlist_id.to_string()));
                args.push(Value::Text(id.clone()));
                args.push(Value::Integer(pos));
            }
            (
                format!("INSERT INTO {TABLE} (playlist_id, media_file_id, id) VALUES {rows}"),
                args,
            )
        })
        .collect()
}

/// `Add` (`:158-165`): append after the current `max` position. Returns the
/// insert statements for the accessible ids; the caller reads the max first
/// and passes `max + 1` as `starting_pos`.
#[must_use]
pub fn add_sql(playlist_id: &str, starting_pos: i64, media_file_ids: &[String]) -> Vec<Sql> {
    add_tracks_sql(playlist_id, starting_pos, media_file_ids)
}

/// The outcome of [`insert_sql`]: the shift statements plus the insert
/// statements, in Go's execution order (`Insert`, `:110-136`).
#[derive(Debug, Clone, PartialEq)]
pub struct InsertSql {
    /// Step 1: negate while shifting, so no intermediate row hits the unique
    /// (playlist_id, id) index — `UPDATE … SET id = -(id + ?) …`.
    pub shift: Sql,
    /// Step 2: flip the shifted range back to positive (`res == 0` means the
    /// position was past the end — the caller falls back to `add_sql`, Go
    /// `:123-125`).
    pub unshift: Sql,
    /// Step 3: the inserts at `pos`, one statement per chunk.
    pub inserts: Vec<Sql>,
}

/// `Insert` (`:110-136`): add tracks before the 1-based position `pos`,
/// shifting the following entries down; a position past the end appends (the
/// `unshift` statement affects no row then, and the caller detects it by the
/// affected-row count). Callers must run it in a transaction.
#[must_use]
pub fn insert_sql(playlist_id: &str, media_file_ids: &[String], pos: i64) -> InsertSql {
    let pos = pos.max(1);
    let n = media_file_ids.len() as i64;
    InsertSql {
        shift: (
            format!("UPDATE {TABLE} SET id = -(id + ?) WHERE playlist_id = ? AND id >= ?"),
            vec![
                Value::Integer(n),
                Value::Text(playlist_id.to_string()),
                Value::Integer(pos),
            ],
        ),
        unshift: (
            format!("UPDATE {TABLE} SET id = -id WHERE playlist_id = ? AND id < 0"),
            vec![Value::Text(playlist_id.to_string())],
        ),
        inserts: add_tracks_sql(playlist_id, pos, media_file_ids),
    }
}

/// The outcome of [`delete_sql`]: the chunked deletes plus the renumber pair
/// (`Delete`, `:222-230`).
#[derive(Debug, Clone, PartialEq)]
pub struct DeleteSql {
    /// One DELETE per `DELETE_CHUNK_SIZE` position ids.
    pub deletes: Vec<Sql>,
    /// `playlist_repo.rs`'s `renumber` statements, run afterwards.
    pub renumber: Vec<Sql>,
}

/// `Delete` (`:222-230`): remove positions in `DELETE_CHUNK_SIZE` chunks, then
/// renumber. `renumber` is the shared pair of
/// `playlist_repo::renumber_sql`.
#[must_use]
pub fn delete_sql(playlist_id: &str, ids: &[String], renumber: Vec<Sql>) -> DeleteSql {
    let deletes = ids
        .chunks(DELETE_CHUNK_SIZE)
        .map(|chunk| {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let mut args = vec![Value::Text(playlist_id.to_string())];
            args.extend(chunk.iter().map(|id| Value::Text(id.clone())));
            (
                format!("DELETE FROM {TABLE} WHERE playlist_id = ? AND id IN ({placeholders})"),
                args,
            )
        })
        .collect();
    DeleteSql { deletes, renumber }
}

/// `DeleteAll` (`:232-238`): clear the playlist, then renumber.
#[must_use]
pub fn delete_all_sql(playlist_id: &str, renumber: Vec<Sql>) -> DeleteSql {
    DeleteSql {
        deletes: vec![(
            format!("DELETE FROM {TABLE} WHERE playlist_id = ?"),
            vec![Value::Text(playlist_id.to_string())],
        )],
        renumber,
    }
}

/// The outcome of [`reorder_sql`]: `Err(pos)` carries the out-of-range source
/// position (Go returns `model.ErrNotFound` after the `max` probe, `:245-247`)
/// with no statements; `Ok` carries the four steps.
#[derive(Debug, Clone, PartialEq)]
pub enum ReorderSql {
    /// `pos` outside `[1, last]` — no statements run.
    OutOfRange,
    /// The steps, in Go's execution order.
    Steps {
        /// Step 1: move the source out of the way (sentinel `-999999`).
        step1: Sql,
        /// Step 2: shift the affected range using negative values.
        step2: Sql,
        /// Step 3: flip the shifted range back to positive.
        step3: Sql,
        /// Step 4: place the source at its new position.
        step4: Sql,
    },
}

/// `Reorder` (`:236-284`): move the track at `pos` to `new_pos`, shifting the
/// others. `new_pos` is clamped to `[1, last]` like Go (`:248`). `pos` is
/// checked against `last` — the caller reads it from [`max_position_sql`].
#[must_use]
pub fn reorder_sql(playlist_id: &str, pos: i64, new_pos: i64, last: i64) -> ReorderSql {
    if pos < 1 || pos > last {
        return ReorderSql::OutOfRange;
    }
    let new_pos = new_pos.clamp(1, last);
    if pos == new_pos {
        return ReorderSql::OutOfRange;
    }
    let pid = Value::Text(playlist_id.to_string());
    let step1 = (
        format!("UPDATE {TABLE} SET id = -999999 WHERE playlist_id = ? AND id = ?"),
        vec![pid.clone(), Value::Integer(pos)],
    );
    let step2 = if pos < new_pos {
        (
            format!(
                "UPDATE {TABLE} SET id = -(id - 1) WHERE playlist_id = ? AND id > ? AND id <= ?"
            ),
            vec![pid.clone(), Value::Integer(pos), Value::Integer(new_pos)],
        )
    } else {
        (
            format!(
                "UPDATE {TABLE} SET id = -(id + 1) WHERE playlist_id = ? AND id >= ? AND id < ?"
            ),
            vec![pid.clone(), Value::Integer(new_pos), Value::Integer(pos)],
        )
    };
    let step3 = (
        format!("UPDATE {TABLE} SET id = -id WHERE playlist_id = ? AND id < 0 AND id != -999999"),
        vec![pid.clone()],
    );
    let step4 = (
        format!("UPDATE {TABLE} SET id = ? WHERE playlist_id = ? AND id = -999999"),
        vec![Value::Integer(new_pos), pid],
    );
    ReorderSql::Steps {
        step1,
        step2,
        step3,
        step4,
    }
}

/// `addMediaFileIds` (`:152-158`): the candidate-id select
/// `SELECT id FROM media_file WHERE <cond> ORDER BY album_artist, album,
/// release_date, disc_number, track_number` — fixed identifiers, the only
/// variable is the condition.
#[must_use]
fn add_media_file_ids_query(cond: &Sql) -> Repository {
    let (cond_sql, cond_args) = cond;
    Repository::new("media_file")
        .select(["id"])
        .where_raw(cond_sql.clone(), cond_args.clone())
        .order_by([
            "album_artist",
            "album",
            "release_date",
            "disc_number",
            "track_number",
        ])
}

/// The candidate select plus the library filter, materialized.
fn add_media_file_ids_sql(cond: &Sql, library_filter: Option<&Sql>) -> Sql {
    with_library_filter(add_media_file_ids_query(cond), library_filter)
        .to_sql()
        .expect("the candidate select materializes")
}

/// `AddAlbums` (`:168-169`): candidates from `album_id IN (…)`.
#[must_use]
pub fn add_albums_sql(album_ids: &[String], library_filter: Option<&Sql>) -> Sql {
    let values = album_ids
        .iter()
        .map(|id| Value::Text(id.clone()))
        .collect::<Vec<_>>();
    let placeholders = vec!["?"; values.len()].join(", ");
    add_media_file_ids_sql(
        &(format!("album_id IN ({placeholders})"), values),
        library_filter,
    )
}

/// `AddArtists` (`:171-176`): candidates matching album-artist participation
/// (`ParticipantIDFilter`, `sql_participations.go:28-45`) — the participant
/// subquery, not the deprecated `album_artist_id` column.
#[must_use]
pub fn add_artists_sql(artist_ids: &[String], library_filter: Option<&Sql>) -> Sql {
    let ids = artist_ids
        .iter()
        .map(|id| Value::Text(id.clone()))
        .collect::<Vec<_>>();
    let placeholders = vec!["?"; ids.len()].join(", ");
    add_media_file_ids_sql(
        &(
            format!(
                "id IN (SELECT media_file_id FROM media_file_artists WHERE \
                 artist_id IN ({placeholders}) AND role IN ('album_artist'))"
            ),
            ids,
        ),
        library_filter,
    )
}

/// `AddDiscs` (`:178-185`): candidates from (album, release date, disc)
/// triples, OR-joined like Go's `Or{…}` of `And` pairs.
#[must_use]
pub fn add_discs_sql(discs: &[(String, String, i64)], library_filter: Option<&Sql>) -> Sql {
    let mut clauses = Vec::new();
    let mut args = Vec::new();
    for (album_id, release_date, disc_number) in discs {
        args.push(Value::Text(album_id.clone()));
        args.push(Value::Text(release_date.clone()));
        args.push(Value::Integer(*disc_number));
        clauses.push("(album_id = ? AND release_date = ? AND disc_number = ?)".to_string());
    }
    add_media_file_ids_sql(&(clauses.join(" OR "), args), library_filter)
}

/// Failures of the executed playlist-track read side.
#[derive(Debug, thiserror::Error)]
pub enum PlaylistTrackError {
    /// The statement failed, or a row's hydration failed. `Read` is `queryOne`, so a
    /// `PostScan` failure surfaces from dbx's `ScanStruct` hook block
    /// (`dbx@v1.12.0/rows.go:92-102`, returning at `:97-99` after a successful `Scan`
    /// at `:88-90`); [`PlaylistTrackRepo::get_all`] is `queryAll`, whose hook lives in
    /// the row loop at `:177-190` and aborts there at `:185-188`. Either way a decode
    /// failure arrives here as a column-conversion error — via
    /// [`crate::hydrate::HydrateError::into_sqlite`].
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// No row matched — Go's `model.ErrNotFound`
    /// (`model/errors.go:10` → `rest.ErrNotFound`, `data not found`,
    /// `deluan/rest@v1.0.1/errors.go:14`).
    ///
    /// [`PlaylistTrackRepo::read`] reaches it the way Go does: `Read` is a
    /// `queryOne` (`playlist_track_repository.go:119`), and Go's `queryOne` maps
    /// `sql.ErrNoRows` to `model.ErrNotFound`
    /// (`sql_base_repository.go:375-388`, `dbx@v1.12.0/rows.go:237-244`) — unlike
    /// `artistRepository.Get`, which counts rows instead. Executed: `Read("999")`
    /// returns `(nil, data not found)`, and a track whose `media_file` row is gone
    /// also lands here because the INNER JOIN drops the row (executed:
    /// `Read(ghost) → data not found`, while `Count` still counts it).
    ///
    /// [`PlaylistTrackRepo::get_all`] NEVER reports this variant: `GetAll` is
    /// `queryAll`, and dbx's `All` makes an empty non-nil slice for a zero-row
    /// result (`dbx@v1.12.0/query.go:278-291`, `rows.go:122-125`), so Go's
    /// `queryAll` ErrNoRows→ErrNotFound arm (`sql_base_repository.go:451-454`) is
    /// dead code on that path.
    ///
    /// A hydration decode failure is NOT this variant — it is
    /// [`PlaylistTrackError::Sqlite`], as in Go.
    #[error("data not found")]
    NotFound,
}

/// `playlistTrackRepository` (`playlist_track_repository.go:12-19`) bound to one
/// pooled connection and one playlist — the executed READ side.
///
/// Go binds `playlistId` at construction (`playlist_repository.go:82-87`) and
/// reads the logged-in user from the ambient ctx; this crate has no ambient
/// context (the `sqlrepo.rs` DI freeze), so the caller threads the playlist id
/// through [`PlaylistTrackRepo::new`] and the user through the read methods.
/// `user` is Go's `loggedUser(ctx).ID`, used for the annotation LEFT JOIN's
/// embedded `annotation.user_id` and — when the user does not see every library
/// (`userSeesAllLibraries`, `sql_base_repository.go:286-299`) — for the library
/// filter. Go applies that filter with the alias `f` in both reads
/// (`playlist_track_repository.go:117`, `playlist_repository.go:375`), so the
/// caller passes [`library_filter_on`] `(Some(user), "f")` and `None` for an
/// admin — NOT this module's unaliased [`library_filter`] default form.
pub struct PlaylistTrackRepo<'a> {
    conn: &'a rusqlite::Connection,
    playlist_id: String,
}

impl<'a> PlaylistTrackRepo<'a> {
    /// `Tracks(ctx, playlistId, …)`'s binding of `p.playlistId`
    /// (`playlist_repository.go:82-87`).
    #[must_use]
    pub fn new(conn: &'a rusqlite::Connection, playlist_id: &str) -> Self {
        Self {
            conn,
            playlist_id: playlist_id.to_string(),
        }
    }

    /// `Read` (`:98-122`): the single track at position `id`, hydrated through
    /// both phases of the seam.
    ///
    /// Go's `Read` does NOT hydrate artwork — it is the only track read that
    /// skips `hydratePlaylistTrackArtwork`, which lives in `loadTracks`
    /// (`playlist_repository.go:406`). Executed: `Read(1)` leaves
    /// `ItemImage`/`AlbumImage` at the zero (unresolved) state while `GetAll` on
    /// the same row yields the resolved hashes. The port matches, so no artwork
    /// lookup runs here and no artwork failure can be reported.
    ///
    /// # Errors
    /// [`PlaylistTrackError::NotFound`] when nothing matches — unknown position,
    /// wrong playlist, or a track excluded by the caller's library filter, which
    /// Go appends LAST (`:120`, executed: `Read(lim, 2)` over a library the user
    /// cannot see returns `(nil, data not found)`).
    /// [`PlaylistTrackError::Sqlite`] for a statement failure or a hydration
    /// failure (bad `participants`/`tags` JSON, or Go's
    /// `converting NULL to string is unsupported` from a projection whose joined
    /// legs are NULL).
    pub fn read(
        &self,
        id: &str,
        user: &str,
        library_filter: Option<&Sql>,
    ) -> Result<PlaylistTrackRecord, PlaylistTrackError> {
        let statement = read_sql(&self.playlist_id, id, user, library_filter);
        let row = crate::execution::query_optional(
            self.conn,
            &statement,
            crate::hydrate::row_to_playlist_track,
        )?;
        // Go returns `(nil, model.ErrNotFound)`; the port's `Option` carries the
        // same information as `None` → NotFound.
        let raw = row.ok_or(PlaylistTrackError::NotFound)?;
        Ok(raw
            .into_record()
            .map_err(crate::hydrate::HydrateError::into_sqlite)?)
    }

    /// `GetAll` (`:123-129`) → `playlistRepository.loadTracks`
    /// (`playlist_repository.go:399-408`): the playlist's tracks in statement
    /// order, hydrated through both phases, then artwork-hydrated by
    /// [`hydrate_track_artwork`] — the wrapper Go calls at `:406`.
    ///
    /// Go forwards its `model.QueryOptions` into `newSelect` here
    /// (`playlist_track_repository.go:124`), so `Sort`/`Order`/`Max`/`Offset` and
    /// `Filters` all reach the tracks select. Executed against the real
    /// repository: no options emits NO `ORDER BY` at all;
    /// `Sort: "title", Order: "desc", Max: 2, Offset: 1` emits
    /// `ORDER BY order_title desc LIMIT 2 OFFSET 1`; a filter emits before
    /// `playlist_id` because `newSelect` applies options first and `tracksQuery`
    /// appends its own `Where` last. This port renders NONE of those legs:
    /// [`crate::playlist_repo::tracks_query`] takes no options, so they have no
    /// effect and rows come back unordered and unlimited. Recorded as a residual
    /// rather than emulated; it also means Go's `optimizePagination`
    /// (`sql_base_repository.go:477-486`) cannot be expressed here, and that
    /// Go's `Offset`-only option (`Max == 0`) is a SQLite syntax error in Go
    /// (`near "OFFSET": syntax error`, executed) while the port returns rows.
    ///
    /// An empty result is an empty `Vec` — never
    /// [`PlaylistTrackError::NotFound`] (see that variant's docs).
    ///
    /// # Errors
    /// [`PlaylistTrackError::Sqlite`] for a statement or hydration failure; a
    /// hydration failure aborts the whole read, as `queryAll` does in Go. The
    /// artwork-lookup failure is REPORTED through
    /// [`crate::artwork_hydration::ReadOutcome`] rather than becoming an error,
    /// because Go's `hydrateItems` logs and returns an empty map
    /// (`artwork_hydration.go:58-68`) and `loadTracks` returns the tracks anyway;
    /// that follows the crate's `artist_repo`/`album_repo` precedent of reporting
    /// it beside the items.
    pub fn get_all(
        &self,
        user: &str,
        library_filter: Option<&Sql>,
    ) -> Result<crate::artwork_hydration::ReadOutcome<PlaylistTrackRecord>, PlaylistTrackError>
    {
        let statement =
            crate::playlist_repo::load_tracks_sql(&self.playlist_id, user, library_filter);
        let rows = crate::execution::query_all(
            self.conn,
            &statement,
            crate::hydrate::row_to_playlist_track,
        )?;
        // Go's `toModels()` (`:46-52`) dereferences the embed dbx already
        // allocated (see [`PlaylistTrackRow`]), so hydration is the whole step.
        let mut items = rows
            .into_iter()
            .map(PlaylistTrackRow::into_record)
            .collect::<Result<Vec<PlaylistTrackRecord>, crate::hydrate::HydrateError>>()
            .map_err(crate::hydrate::HydrateError::into_sqlite)?;
        let artwork_error =
            hydrate_track_artwork(self.conn, &mut items, ENABLE_MEDIA_FILE_COVER_ART);
        Ok(crate::artwork_hydration::ReadOutcome {
            items,
            artwork_error,
        })
    }
}

/// `conf.Server.EnableMediaFileCoverArt` (`conf/configuration.go:1003`, default
/// `true`) — Go reads a global; the crate has no config global, so the read side
/// supplies the documented default and callers that model the option pass their
/// own value to [`hydrate_track_artwork`].
pub const ENABLE_MEDIA_FILE_COVER_ART: bool = true;

/// `hydratePlaylistTrackArtwork` (`artwork_hydration.go:169-181`): project the
/// tracks' media files, run the media-file wrapper over them, and write the
/// results back positionally. The empty-batch early return is Go's `:170-172`,
/// and the wrapper is REUSED rather than a new lookup: playlist tracks have no
/// artwork kind of their own (`model/artwork_id.go` defines no `pt`), so the two
/// lookups are the media file's `al` and `mf` kinds
/// (`artwork_hydration.go:106-107`, the album lookup first).
///
/// `enable_media_file_cover_art` is Go's conf flag (see
/// [`ENABLE_MEDIA_FILE_COVER_ART`]).
///
/// Returns Go's swallowed lookup failure text (see
/// [`crate::artwork_hydration::ReadOutcome`]), or `None`.
#[must_use]
pub fn hydrate_track_artwork(
    conn: &rusqlite::Connection,
    tracks: &mut [PlaylistTrackRecord],
    enable_media_file_cover_art: bool,
) -> Option<String> {
    if tracks.is_empty() {
        return None;
    }
    let mut mfs = PlaylistTrackRecord::media_files(tracks);
    let error = crate::artwork_hydration::hydrate_media_file_artwork(
        conn,
        &mut mfs,
        enable_media_file_cover_art,
    );
    PlaylistTrackRecord::set_media_files(tracks, mfs);
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIB: Option<&Sql> = None;

    #[test]
    fn count_all_inner_joins_media_file_and_binds_playlist() {
        // Arrange/Act: CountAll (:73-78).
        let (sql, args) = count_all_query("pl-1", LIB)
            .count(TABLE)
            .to_sql()
            .expect("count materializes");
        // Assert:
        assert_eq!(
            sql,
            "SELECT count(distinct playlist_tracks.id) as count FROM playlist_tracks \
             INNER JOIN media_file f ON f.id = media_file_id WHERE playlist_id = ?"
        );
        assert_eq!(args, vec![Value::Text("pl-1".to_string())]);
    }

    #[test]
    fn read_binds_playlist_and_position_with_the_annotation_join() {
        // Arrange/Act: Read (:98-121), user "u1" embedded like Go's concat.
        let (sql, args) = read_sql("pl-1", "2", "u1", LIB);
        // Assert:
        assert!(sql.starts_with("SELECT coalesce(starred, 0) as starred"));
        assert!(sql.contains("LEFT JOIN annotation ON (annotation.item_id = media_file_id AND"));
        assert!(sql.contains("annotation.user_id = 'u1')"));
        assert!(sql.contains("INNER JOIN media_file f ON f.id = media_file_id"));
        assert_eq!(
            sql,
            "SELECT coalesce(starred, 0) as starred, coalesce(play_count, 0) as play_count, \
             coalesce(rating, 0) as rating, starred_at, play_date, rated_at, f.*, playlist_tracks.* \
             FROM playlist_tracks LEFT JOIN annotation ON (annotation.item_id = media_file_id AND \
             annotation.item_type = 'media_file' AND annotation.user_id = 'u1') \
             INNER JOIN media_file f ON f.id = media_file_id \
             WHERE playlist_id = ? AND playlist_tracks.id = ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("pl-1".to_string()),
                Value::Text("2".to_string())
            ]
        );
    }

    #[test]
    fn max_position_probe_binds_only_the_playlist() {
        // Arrange/Act: Add's/Reorder's max(id) probe (:160-163, :238-240).
        let (sql, args) = max_position_sql("pl-1");
        // Assert:
        assert_eq!(
            sql,
            "SELECT max(id) as max FROM playlist_tracks WHERE playlist_id = ?"
        );
        assert_eq!(args, vec![Value::Text("pl-1".to_string())]);
    }

    #[test]
    fn add_tracks_chunks_rows_of_three_bound_values() {
        // Arrange: addTracks' chunked insert (:291-316), 2 ids from pos 3.
        let ids = vec!["m1".to_string(), "m2".to_string()];
        // Act:
        let stmts = add_tracks_sql("pl-1", 3, &ids);
        // Assert: one statement under the chunk size, positions 3 then 4.
        assert_eq!(stmts.len(), 1);
        assert_eq!(
            stmts[0].0,
            "INSERT INTO playlist_tracks (playlist_id, media_file_id, id) VALUES (?, ?, ?), (?, ?, ?)"
        );
        assert_eq!(
            stmts[0].1,
            vec![
                Value::Text("pl-1".to_string()),
                Value::Text("m1".to_string()),
                Value::Integer(3),
                Value::Text("pl-1".to_string()),
                Value::Text("m2".to_string()),
                Value::Integer(4),
            ]
        );
    }

    #[test]
    fn insert_shifts_negates_and_inserts_in_go_order() {
        // Arrange: Insert (:110-136), 2 ids before position 2.
        let ids = vec!["m1".to_string(), "m2".to_string()];
        // Act:
        let ins = insert_sql("pl-1", &ids, 2);
        // Assert:
        assert_eq!(
            ins.shift.0,
            "UPDATE playlist_tracks SET id = -(id + ?) WHERE playlist_id = ? AND id >= ?"
        );
        assert_eq!(
            ins.shift.1,
            vec![
                Value::Integer(2),
                Value::Text("pl-1".to_string()),
                Value::Integer(2),
            ]
        );
        assert_eq!(
            ins.unshift.0,
            "UPDATE playlist_tracks SET id = -id WHERE playlist_id = ? AND id < 0"
        );
        // The inserts start at the clamped position.
        assert_eq!(ins.inserts[0].1[2], Value::Integer(2));
        // Zero-or-less positions clamp to 1.
        let clamped = insert_sql("pl-1", &ids, 0);
        assert_eq!(clamped.shift.1[2], Value::Integer(1));
    }

    #[test]
    fn delete_chunks_positions_and_carries_the_renumber() {
        // Arrange: Delete (:222-230) with 2 chunk sizes of ids and the
        // renumber statements the caller owns.
        let renumber = vec![("renumber-1".to_string(), vec![])];
        let mut ids = Vec::new();
        for i in 1..=(DELETE_CHUNK_SIZE + 1) as i64 {
            ids.push(i.to_string());
        }
        // Act:
        let del = delete_sql("pl-1", &ids, renumber.clone());
        // Assert: two chunks — 200 ids then the 201st.
        assert_eq!(del.deletes.len(), 2);
        assert_eq!(del.renumber, renumber);
        assert_eq!(
            del.deletes[0].0,
            "DELETE FROM playlist_tracks WHERE playlist_id = ? AND id IN (?, ?, ?)".replace(
                "(?, ?, ?)",
                &format!("({})", vec!["?"; DELETE_CHUNK_SIZE].join(", "))
            )
        );
        assert_eq!(del.deletes[1].1.len(), 2);
    }

    #[test]
    fn reorder_rejects_out_of_range_and_steps_in_order() {
        // Arrange/Act: Reorder (:236-284) moving position 1 to 3 of 3.
        let oob_low = reorder_sql("pl-1", 0, 1, 3);
        let oob_high = reorder_sql("pl-1", 4, 1, 3);
        let noop = reorder_sql("pl-1", 2, 2, 3);
        let ReorderSql::Steps {
            step1,
            step2,
            step3,
            step4,
        } = reorder_sql("pl-1", 1, 3, 3)
        else {
            panic!("a forward move renders steps");
        };
        // Assert:
        assert_eq!(oob_low, ReorderSql::OutOfRange);
        assert_eq!(oob_high, ReorderSql::OutOfRange);
        assert_eq!(noop, ReorderSql::OutOfRange);
        assert_eq!(
            step1.0,
            "UPDATE playlist_tracks SET id = -999999 WHERE playlist_id = ? AND id = ?"
        );
        assert_eq!(
            step2.0,
            "UPDATE playlist_tracks SET id = -(id - 1) WHERE playlist_id = ? AND id > ? AND id <= ?"
        );
        assert_eq!(
            step3.0,
            "UPDATE playlist_tracks SET id = -id WHERE playlist_id = ? AND id < 0 AND id != -999999"
        );
        assert_eq!(
            step4.0,
            "UPDATE playlist_tracks SET id = ? WHERE playlist_id = ? AND id = -999999"
        );
        assert_eq!(
            step4.1,
            vec![Value::Integer(3), Value::Text("pl-1".to_string())]
        );
        // A backward move takes the -(id + 1) branch.
        let ReorderSql::Steps { step2: back, .. } = reorder_sql("pl-1", 3, 1, 3) else {
            panic!("a backward move renders steps");
        };
        assert_eq!(
            back.0,
            "UPDATE playlist_tracks SET id = -(id + 1) WHERE playlist_id = ? AND id >= ? AND id < ?"
        );
        // Clamp: past-the-end positions move to the end.
        let ReorderSql::Steps { step4: clamped, .. } = reorder_sql("pl-1", 1, 9, 3) else {
            panic!("a clamped move renders steps");
        };
        assert_eq!(clamped.1[0], Value::Integer(3));
    }

    #[test]
    fn add_albums_artists_and_discs_pin_the_candidate_selects() {
        // Arrange/Act: addMediaFileIds (:152-185).
        let albums = add_albums_sql(&["al-1".to_string(), "al-2".to_string()], LIB);
        let artists = add_artists_sql(&["ar-1".to_string()], LIB);
        let discs = add_discs_sql(&[("al-1".to_string(), "2020-01-01".to_string(), 2)], LIB);
        // Assert:
        assert_eq!(
            albums.0,
            "SELECT id FROM media_file WHERE album_id IN (?, ?) ORDER BY album_artist, album, \
             release_date, disc_number, track_number"
        );
        assert_eq!(
            artists.0,
            "SELECT id FROM media_file WHERE id IN (SELECT media_file_id FROM media_file_artists \
             WHERE artist_id IN (?) AND role IN ('album_artist')) ORDER BY album_artist, album, \
             release_date, disc_number, track_number"
        );
        assert_eq!(
            discs.0,
            "SELECT id FROM media_file WHERE (album_id = ? AND release_date = ? AND \
             disc_number = ?) ORDER BY album_artist, album, release_date, disc_number, track_number"
        );
    }

    #[test]
    fn library_filter_is_a_parameterized_subquery_or_absent() {
        // Arrange/Act: applyLibraryFilter (:261-283), user "u2".
        let (sql, args) = get_media_file_ids_query("pl-1", library_filter(Some("u2")).as_ref())
            .to_sql()
            .expect("filtered select materializes");
        let plain = get_media_file_ids_query("pl-1", LIB)
            .to_sql()
            .expect("unfiltered select materializes");
        // Assert:
        assert_eq!(
            sql,
            "SELECT media_file_id FROM playlist_tracks INNER JOIN media_file f ON f.id = media_file_id \
             WHERE playlist_id = ? AND media_file.library_id IN \
             (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("pl-1".to_string()),
                Value::Text("u2".to_string()),
            ]
        );
        assert_eq!(plain.1, vec![Value::Text("pl-1".to_string())]);
    }
}
