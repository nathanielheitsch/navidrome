//! DB-11 — the executed **typed scanner reads**, their resource/cancellation
//! contract, and the API-safe scoped point read.
//!
//! Ported behavior, cited to the Go source it mirrors:
//!
//! * the streaming primitives of `persistence/sql_base_repository.go` —
//!   `queryWithStableResults` (`:411-439`: ONE prepared statement on a pool
//!   connection, `defer rows.Close()`, a scan error yielded BESIDE the row at
//!   `:428-431` via `wrapCursor` `:392-406`, a driver error as zero-value +
//!   error at `:434-437`, clean exhaustion = no further yield) and
//!   `streamByIDs` (`persistence/artwork_hydration.go:19-34`: id pre-pass then a
//!   re-query per chunk of 600, non-resumable). `D-CURSOR` (`docs/rust-port/decisions.md`,
//!   PARENT DECIDED 2026-11-20) fixes the two Rust kinds this module exposes:
//!   [`VisitOutcome`]-driven single-statement visitors ([`visit_media_in_folder`]
//!   and friends) and the id-chunked [`IdChunkedCursor`].
//! * scope resolution mirrors `applyLibraryFilter`
//!   (`sql_base_repository.go:261-283`), `userSeesAllLibraries` (`:288-299`) and
//!   `visibleLibraryIDs` (`:302-313`) — see [`resolve_scope`] for the arms.
//!   `D-SCOPE` (`decisions.md:348-376`) requires a DB-rendered scope and never
//!   raw caller SQL. On this module's PUBLIC surface that is carried by the opaque
//!   authorities [`ScannerScope`] (system: a library id only) and [`UserScope`]
//!   (authenticated: resolved on a leased connection) rather than by
//!   `Option<Sql>` directly — the DB-11 review ruling, which tightens D-SCOPE's
//!   wording without changing its intent. Clause text is rendered internally
//!   (`#[doc(hidden)]` seams) and never reaches a caller.
//!
//! # Resource contract (the decision this module exists to make executable)
//!
//! **DB owns early-stop, cancellation and lease release; SC owns backpressure and
//! phase abort policy.** Concretely:
//!
//! * Every visitor prepares, queries and drops INSIDE the call: the
//!   [`rusqlite::Statement`] and [`rusqlite::Rows`] are locals of
//!   the private visitor driver in this module, so they are dropped on every exit
//!   path — completion,
//!   [`Control::Stop`], cancellation and error. Nothing a callback receives
//!   outlives the callback, so a caller cannot retain a `Row`, a `Statement` or
//!   the lease.
//! * The callback receives an **owned** record only and no connection, so it
//!   cannot retain a `Row` or a `Statement`. It does NOT get a structural
//!   prohibition on re-entrance: a `FnMut` closure can capture the `SQLStore` and
//!   call `with_connection*` itself, and `pool.rs::checkout` BLOCKS, so that
//!   nested checkout deadlocks on a pool whose handles are all leased (size 1
//!   deadlocks immediately). Callers MUST NOT check out the store or write to the
//!   database from inside a visitor callback — the lease is held across the whole
//!   read precisely so the statement stays open, and this module cannot enforce
//!   the rule, only state it. See
//!   [`visit_media_in_folder`] for the prohibition at the call site.
//! * `PostScan` runs per row inside the loop (`hydrate::row_to_mediafile` +
//!   `MediaFileRow::into_record`), matching Go's per-row `ScanStruct`
//!   (`dbx@v1.12.0/rows.go:177-190`), so a decode failure on an early row wins
//!   over a later one and the read stops there.
//! * Cancellation is `nd_utils::context::Cancellation::is_cancelled`
//!   (`nd-utils/src/context.rs:31`), checked per row after fetching it: a
//!   cancelled flag yields `Ok(`[`VisitOutcome::Stopped`]`)` with the row NOT
//!   decoded, and `Ok(`[`VisitOutcome::Completed`]`)` when the flag is set only
//!   after the last row. SC-side backpressure (a bounded channel, `blocking_send`
//!   inside `spawn_blocking`, receiver-drop) is SC's job; this module only
//!   guarantees that a callback returning [`Control::Stop`] stops the statement.
//! * The three-way stop taxonomy of `D-CURSOR(3)` is
//!   [`CursorError::Sqlite`] (query/scan failure), [`CursorError::Closed`]
//!   (post-completion use of an [`IdChunkedCursor`]) and `Ok` completion.
//!   Folding a SQLite error into end-of-stream is the one forbidden conflation,
//!   so a decode failure NEVER arrives as `Completed`.
//! * Dropping an awaiting future does NOT stop SQLite: `store.rs:313` documents
//!   that started blocking work runs to completion. A visitor is synchronous and
//!   its stop signal is [`Control::Stop`] or the cancellation flag — never a drop.
//!
//! # What is deliberately NOT here
//!
//! * **Artwork hydration.** Go's `GetCursor` (`mediafile_repository.go:309-316`)
//!   does not hydrate (`persistence/artwork_hydration_test.go:633` pins that),
//!   and neither do `GetMissingAndMatching` (`:494-517`), `FindByPaths`
//!   (`:365-404`) or `FindRecentFilesBy*` (`:525-569`). These reads build on
//!   [`crate::mediafile_repo::select_media_file`] and never on
//!   [`crate::mediafile_repo::MediaFileRepo::get_all`], which always hydrates.
//! * **The REST `mediaFileFilter` whitelist** (`mediafile_repository.go:117-131`).
//!   Scanner reads are filter-by-construction: the predicates below are
//!   DB-authored typed clauses, not client filter keys.
//! * **`SEEDEDRAND` random-sort paging** — `D-CURSOR(5)` says random is not a
//!   cursor; that render path belongs to the REST/API lane.
//! * **Folder cursor draining.** [`crate::folder_repo::CursorDrain`] keeps its
//!   declared accepted deviation (`D-CURSOR(6)`: prefix dropped beside the
//!   failure); this module does not re-implement or repair it.
//! * **Phase-4 playlist import readiness.** [`media_by_paths`] is the typed
//!   read `cmd/missing.go:139` and `core/playlists/parse_m3u.go:97` need; the
//!   import orchestration is SC/API's.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Utc};
use rusqlite::{params_from_iter, Connection, Row};

use crate::album_repo::{self, AlbumRecord, AlbumRow};
use crate::artist_repo;
use crate::folder_repo;
use crate::hydrate::{self, HydrateError};
use crate::library_repo::LibraryRepo;
use crate::mediafile_repo::{self, MediaFileRecord, MediaFileRepo, MediaFileRow};
use crate::sql_search::Sql;
use crate::sqlrepo::Value;
use crate::user_repo::{UserError, UserRepo};

/// Go's `invalidUserId` (`sql_base_repository.go:51`): the identity a headless
/// process gets from `loggedUser` (`:51-57`) when no user is in the context.
///
/// Re-exported from [`crate::artist_repo::INVALID_USER_ID`] so there is one
/// constant for it in the crate.
pub const HEADLESS_USER_ID: &str = artist_repo::INVALID_USER_ID;

/// Whether a visitor keeps reading after the callback received a row.
///
/// Go's equivalent is the `yield` return of `queryWithStableResults`
/// (`sql_base_repository.go:427`: `if !yield(row, err) || err != nil { return }`)
/// — the consumer decides, and the statement closes immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Deliver the next row.
    Continue,
    /// Stop now: the statement and rows are dropped before this call returns.
    Stop,
}

/// How a visitor ended. `visited` counts the records actually delivered to the
/// callback, so a stopped read reports a truthful prefix length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisitOutcome {
    /// The statement exhausted normally (Go's clean end of iteration — NOT
    /// "stop, keep progress"; per `D-CURSOR(3)` a caller that wants to abort on
    /// completion is SC's phase policy, not this type's).
    Completed {
        /// Records delivered to the callback before the statement exhausted.
        visited: usize,
    },
    /// The callback returned [`Control::Stop`], or the cancellation flag was set
    /// when a row was fetched. Rows fetched but not delivered are NOT decoded.
    Stopped {
        /// Records delivered to the callback before the stop; the row that
        /// triggered the stop is NOT counted.
        visited: usize,
    },
}

/// Failure of a visitor, per the `D-CURSOR(3)` taxonomy.
#[derive(Debug, thiserror::Error)]
pub enum CursorError {
    /// Prepare, bind, execution or row-decode failure. Go yields this BESIDE the
    /// row (`sql_base_repository.go:428-431`) or as zero-value + error
    /// (`:434-437`); the port has no value-beside-error in a `Result`, so the
    /// prefix rows are simply not reported (the same accepted shape as
    /// [`crate::folder_repo::CursorDrain], `D-CURSOR(6)`).
    ///
    /// A `PostScan` decode failure arrives here too, as
    /// [`HydrateError::into_sqlite`]'s column-conversion error carrying Go's
    /// prefix (e.g. `parsing media_file from db: `).
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// The cursor is past its last chunk (`D-CURSOR(3)`'s post-completion use).
    /// Distinct from [`CursorError::Sqlite`] so a caller can tell "you used it
    /// after it finished" from "the database failed".
    #[error("cursor is closed: all chunks have been visited")]
    Closed,
}

/// The scanner predicate a media-file visitor applies, as a typed clause.
///
/// Go's phase 1 passes `squirrel.Eq{"folder_id": entry.id}`
/// (`scanner/phase_1_folders.go:231-233`) and phase 3 `Eq{"album_id": album.ID}`
/// (`scanner/phase_3_refresh_albums.go:79`); both are DB-authored here, so no
/// client-supplied column name can reach the statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaPhaseFilter<'a> {
    /// `media_file.folder_id = ?` — Go's phase-1 per-folder read.
    InFolder {
        /// The folder primary key.
        folder_id: &'a str,
    },
    /// `media_file.album_id = ?` — Go's phase-3 per-album read.
    InAlbum {
        /// The album primary key.
        album_id: &'a str,
    },
}

impl MediaPhaseFilter<'_> {
    /// Render the predicate with its bound value.
    #[must_use]
    pub fn clause(&self) -> Sql {
        match self {
            Self::InFolder { folder_id } => (
                format!("{}.folder_id = ?", mediafile_repo::TABLE),
                vec![Value::from(*folder_id)],
            ),
            Self::InAlbum { album_id } => (
                format!("{}.album_id = ?", mediafile_repo::TABLE),
                vec![Value::from(*album_id)],
            ),
        }
    }
}

/// Which of Go's two recent-match queries to run
/// (`scanner/phase_2_missing_tracks.go:231-266` tries MBZ first, then properties).
#[derive(Debug, Clone, PartialEq)]
pub enum RecentMatcher<'a> {
    /// `FindRecentFilesByMBZTrackID` (`mediafile_repository.go:512-532`).
    Mbz(mediafile_repo::RecentFileByMbz<'a>),
    /// `FindRecentFilesByProperties` (`mediafile_repository.go:534-563`).
    Properties(mediafile_repo::RecentFileByProperties<'a>),
}

impl RecentMatcher<'_> {
    /// Render the matcher's statement through the existing builders.
    #[must_use]
    pub fn statement(&self) -> Sql {
        match self {
            Self::Mbz(m) => mediafile_repo::find_recent_files_by_mbz_track_id_sql(m),
            Self::Properties(m) => mediafile_repo::find_recent_files_by_properties_sql(m),
        }
    }
}

/// Join a clause onto a base statement with squirrel's `WhereParts` rule
/// (`select.go:112-117` renders the accumulated parts with `" AND "`): a base
/// that already opened a `WHERE` gets ` AND <clause>` and never a second
/// ` WHERE `, which SQLite rejects. The `where_open` FLAG rather than a search of
/// the rendered text keeps a scope subquery's own `WHERE` from being mistaken for
/// an open outer clause — the same rule [`crate::mediafile_repo::get_sql`] states
/// (`mediafile_repo.rs:399-426`).
fn push_where(mut statement: Sql, clause: &Sql, where_open: bool) -> Sql {
    statement
        .0
        .push_str(if where_open { " AND " } else { " WHERE " });
    statement.0.push_str(&clause.0);
    statement.1.extend(clause.1.iter().cloned());
    statement
}

/// Drive `statement` on `conn`, handing each decoded record to `f`.
///
/// The prepared statement and the rows are locals, so every return path drops
/// them; `rusqlite::Rows::drop` resets the statement, which is what releases the
/// read snapshot. `map` runs per row (Go's per-row `ScanStruct`), and the
/// cancellation flag is checked after a row is fetched but before it is decoded,
/// so a cancelled read neither decodes nor delivers that row.
///
/// # Errors
/// [`CursorError::Sqlite`] on a prepare/bind/execution/decode failure — never
/// folded into completion.
fn visit_rows<T>(
    conn: &Connection,
    statement: &Sql,
    cancel: &nd_utils::context::Cancellation,
    map: impl Fn(&Row<'_>) -> rusqlite::Result<T>,
    mut f: impl FnMut(T) -> Control,
) -> Result<VisitOutcome, CursorError> {
    let mut prepared = conn.prepare(&statement.0).map_err(CursorError::Sqlite)?;
    let mut rows = prepared
        .query(params_from_iter(statement.1.iter()))
        .map_err(CursorError::Sqlite)?;
    let mut visited = 0usize;
    loop {
        match rows.next() {
            Ok(None) => return Ok(VisitOutcome::Completed { visited }),
            Err(err) => return Err(CursorError::Sqlite(err)),
            Ok(Some(row)) => {
                if cancel.is_cancelled() {
                    return Ok(VisitOutcome::Stopped { visited });
                }
                let record = map(row).map_err(CursorError::Sqlite)?;
                visited += 1;
                if f(record) == Control::Stop {
                    return Ok(VisitOutcome::Stopped { visited });
                }
            }
        }
    }
}

/// `dbMediaFile` phase 1 + phase 2 (`row_to_mediafile` →
/// [`MediaFileRow::into_record`]), surfaced as the read's own sqlite error.
fn media_record(row: &Row<'_>) -> rusqlite::Result<MediaFileRecord> {
    hydrate::row_to_mediafile(row)
        .map_err(HydrateError::Sqlite)
        .and_then(MediaFileRow::into_record)
        .map_err(HydrateError::into_sqlite)
}

/// `dbAlbum` phase 1 + phase 2, same exit shape.
fn album_record(row: &Row<'_>) -> rusqlite::Result<AlbumRecord> {
    hydrate::row_to_album(row)
        .map_err(HydrateError::Sqlite)
        .and_then(AlbumRow::into_record)
        .map_err(HydrateError::into_sqlite)
}

/// An id-only projection row. NULL fails the scan, which is Go's behavior too:
/// `queryAllSlice` into `[]string` rejects
/// `converting NULL to string is unsupported`.
fn id_value(row: &Row<'_>) -> rusqlite::Result<String> {
    row.get::<_, String>(0)
}

/// `MediaFile.GetCursor` (`mediafile_repository.go:309-316`) with one of the
/// phase predicates above: `selectMediaFile` under a resolved [`UserScope`] and
/// the typed filter, no artwork hydration.
///
/// `scope` is the opaque authority [`UserScope::resolve`] produced, NOT rendered
/// SQL: only a resolved authority can narrow this statement, and the clause text
/// is decided here from [`ScopeAlias::MediaFile`]'s mapping.
///
/// Args bind in clause order: scope's user id first when the scope clause is the
/// base's first `WHERE`, then the predicate's value. `user_id` is `Some` only
/// for a logged-in non-headless user (Go's `withAnnotation`/`withBookmark` skip,
/// `sql_annotations.go:63-65`, `sql_bookmarks.go:22-28`).
#[doc(hidden)]
#[must_use]
pub fn media_cursor_sql(
    filter: MediaPhaseFilter<'_>,
    scope: Option<&UserScope>,
    user_id: Option<&str>,
) -> Sql {
    debug_assert!(
        scope.is_none_or(|scope| scope.alias == ScopeAlias::MediaFile),
        "a media-file cursor read must carry a scope resolved with ScopeAlias::MediaFile"
    );
    let clause = scope.and_then(UserScope::clause_sql);
    let base = mediafile_repo::select_media_file(user_id, clause.clone());
    // `select_media_file` opens a WHERE exactly when a scope clause was given
    // (`mediafile_repo.rs:288-322`).
    push_where(base, &filter.clause(), clause.is_some())
}

/// `GetCursor` filtered to one folder (`scanner/phase_1_folders.go:231-233`),
/// visited row by row.
///
/// # Errors
/// [`CursorError::Sqlite`] on a statement or decode failure.
/// The callback must not check out the store or write to the database: the lease
/// is held across the whole read, so a nested `with_connection*` from inside the
/// callback blocks (`pool.rs::checkout`) and deadlocks once every handle is
/// leased. This is a caller obligation the signature cannot enforce.
pub fn visit_media_in_folder(
    conn: &Connection,
    folder_id: &str,
    scope: Option<&UserScope>,
    cancel: &nd_utils::context::Cancellation,
    f: impl FnMut(MediaFileRecord) -> Control,
) -> Result<VisitOutcome, CursorError> {
    visit_media_in_filter(
        conn,
        MediaPhaseFilter::InFolder { folder_id },
        scope,
        cancel,
        f,
    )
}

/// The same read filtered to one album (`scanner/phase_3_refresh_albums.go:79`).
///
/// Deviation, declared rather than hidden: Go's phase 3 calls `GetAll` there,
/// which DOES hydrate artwork (`mediafile_repository.go:244-246`), while
/// `GetCursor` with the same filter does not. This is the `GetCursor` shape, so
/// `image`/`album_image` come back unresolved; the album aggregate the scanner
/// derives from these rows reads no artwork field, and SC must not expect one.
///
/// # Errors
/// [`CursorError::Sqlite`] on a statement or decode failure.
pub fn visit_media_in_album(
    conn: &Connection,
    album_id: &str,
    scope: Option<&UserScope>,
    cancel: &nd_utils::context::Cancellation,
    f: impl FnMut(MediaFileRecord) -> Control,
) -> Result<VisitOutcome, CursorError> {
    visit_media_in_filter(
        conn,
        MediaPhaseFilter::InAlbum { album_id },
        scope,
        cancel,
        f,
    )
}

/// Shared body of the two phase visitors (the logged-in user is headless here:
/// scanner reads run in Go's headless context per `D-SCOPE`, so no annotation
/// or bookmark join is rendered).
pub fn visit_media_in_filter(
    conn: &Connection,
    filter: MediaPhaseFilter<'_>,
    scope: Option<&UserScope>,
    cancel: &nd_utils::context::Cancellation,
    f: impl FnMut(MediaFileRecord) -> Control,
) -> Result<VisitOutcome, CursorError> {
    let statement = media_cursor_sql(filter, scope, None);
    visit_rows(conn, &statement, cancel, media_record, f)
}

/// `GetMissingAndMatching` (`mediafile_repository.go:485-510`, consumed by
/// `scanner/phase_2_missing_tracks.go:74-100`), visited in the statement's own
/// `ORDER BY pid` order.
///
/// Library-ID-scoped by [`ScannerScope`] only: Go does NOT apply
/// `applyLibraryFilter` here (`:485-510` builds the library bound in the pid
/// subquery alone), so the authority carries a library id and nothing else — no
/// user id, no grant list, no clause text can reach this read.
///
/// # Errors
/// [`CursorError::Sqlite`] on a statement or decode failure.
pub fn visit_missing_and_matching(
    conn: &Connection,
    scope: &ScannerScope,
    cancel: &nd_utils::context::Cancellation,
    f: impl FnMut(MediaFileRecord) -> Control,
) -> Result<VisitOutcome, CursorError> {
    let statement = mediafile_repo::get_missing_and_matching_sql(scope.library_id());
    visit_rows(conn, &statement, cancel, media_record, f)
}

/// `FindRecentFilesByMBZTrackID` / `FindRecentFilesByProperties`
/// (`mediafile_repository.go:512-563`, called per missing row by
/// `scanner/phase_2_missing_tracks.go:235/254`), visited in `created_at DESC`
/// order. `since` is the missing row's `created_at` and binds strictly (`>`).
/// The library bound lives inside [`RecentMatcher`] — the existing typed input
/// structs carry a typed `library_id`, so this read takes no scope parameter at
/// all and no caller-supplied clause.
///
/// # Errors
/// [`CursorError::Sqlite`] on a statement or decode failure.
pub fn visit_recent_candidates(
    conn: &Connection,
    matcher: &RecentMatcher<'_>,
    cancel: &nd_utils::context::Cancellation,
    f: impl FnMut(MediaFileRecord) -> Control,
) -> Result<VisitOutcome, CursorError> {
    let statement = matcher.statement();
    visit_rows(conn, &statement, cancel, media_record, f)
}

/// `GetTouchedAlbums` (`album_repository.go:392-403`, consumed by
/// `scanner/phase_3_refresh_albums.go:49-59`), visited as hydrated album records.
///
/// Library-ID-scoped and run in Go's HEADLESS scanner context: `GetTouchedAlbums`
/// (`:392-403`) calls `selectAlbum`, which DOES apply `applyLibraryFilter`
/// (`album_repository.go:227-231`) — so it is "no user filter" only because the
/// scanner's context has no logged-in user and `loggedUser` synthesizes
/// `invalidUserId` (`sql_base_repository.go:51-57`), the arm that omits the
/// clause. Consequently this read takes NO `user_id` and NO scope parameter: the
/// system context is the only one that produces Go's statement, and a caller
/// cannot smuggle a user authority into a scanner read.
///
/// `normalized_album_play_count` is Go's
/// `conf.Server.AlbumPlayCountMode` supplied by the caller, as elsewhere in
/// [`crate::album_repo`].
///
/// # Errors
/// [`CursorError::Sqlite`] on a statement or decode failure.
pub fn visit_touched_albums(
    conn: &Connection,
    scope: &ScannerScope,
    normalized_album_play_count: bool,
    cancel: &nd_utils::context::Cancellation,
    f: impl FnMut(AlbumRecord) -> Control,
) -> Result<VisitOutcome, CursorError> {
    let statement = album_repo::get_touched_albums_sql(
        None,
        normalized_album_play_count,
        None,
        scope.library_id(),
    );
    visit_rows(conn, &statement, cancel, album_record, f)
}

/// One id-chunked reader (`streamByIDs`, `artwork_hydration.go:19-34`): it keeps
/// the SAME leased connection across all chunks — `D-CURSOR(1)`'s `IdChunked`
/// kind, which intentionally allows drift between the pre-pass and the chunk
/// re-queries — and reports [`CursorError::Closed`] once the last chunk has been
/// visited.
///
/// The connection is borrowed, not owned: the caller holds the lease
/// (`SQLStore::with_connection` / `with_connection_sync`) for the cursor's whole
/// life, so the cursor can never re-check out the pool.
pub struct IdChunkedCursor<'c, T> {
    conn: &'c Connection,
    statements: Vec<Sql>,
    next: usize,
    complete: bool,
    /// Set by the first [`CursorError::Sqlite`]: per `D-CURSOR(1)` this kind is a
    /// NON-resumable primitive and scanner phases treat a cursor error as fatal,
    /// so the cursor closes at the failure instead of offering the next chunk.
    failed: bool,
    map: fn(&Row<'_>) -> rusqlite::Result<T>,
    /// Key extractor for cross-chunk dedup (`None` = report every row, which is
    /// what Go's `GetAlbumIDsByFolder` does — it appends per-chunk ids without
    /// dedup, `mediafile_repository.go:315`).
    dedup: Option<fn(&T) -> &str>,
    seen: HashSet<String>,
}

impl<T> IdChunkedCursor<'_, T> {
    /// Close the cursor at a chunk failure: the error is reported once and every
    /// later use reports [`CursorError::Closed`].
    fn fail(&mut self, err: rusqlite::Error) -> Result<VisitOutcome, CursorError> {
        self.failed = true;
        self.complete = true;
        Err(CursorError::Sqlite(err))
    }
}

impl<'c, T> IdChunkedCursor<'c, T> {
    /// Build a cursor over `statements`, already chunk-split by the caller.
    /// `dedup` is a key extractor applied across chunks; rows whose key was seen
    /// are skipped (and not counted as visited).
    ///
    /// `#[doc(hidden)]`: it accepts raw chunk statements, so it is the internal
    /// construction seam. Callers build a cursor through the typed readers
    /// ([`album_ids_by_folders`], [`sole_album_artist_ids_in_subtrees`]), which are
    /// the only way to obtain one from typed inputs.
    #[doc(hidden)]
    #[must_use]
    pub fn new(
        conn: &'c Connection,
        statements: Vec<Sql>,
        map: fn(&Row<'_>) -> rusqlite::Result<T>,
        dedup: Option<fn(&T) -> &str>,
    ) -> Self {
        Self {
            conn,
            statements,
            next: 0,
            complete: false,
            failed: false,
            map,
            dedup,
            seen: HashSet::new(),
        }
    }

    /// Chunks still to be visited.
    #[must_use]
    pub fn remaining_chunks(&self) -> usize {
        self.statements.len() - self.next
    }

    /// Whether the cursor has finished — normally, or because a chunk failed. A
    /// failed cursor is closed and reports [`CursorError::Closed`] on further use.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// Visit the next chunk, delivering records to `f`. `visited` counts records
    /// delivered by THIS chunk only, so a caller summing chunks adds the
    /// per-chunk numbers.
    ///
    /// A chunk failure is TERMINAL (`D-CURSOR(1)`): the cursor closes at the first
    /// error, so this method never resumes with a later chunk and never re-runs the
    /// failed one.
    ///
    /// # Errors
    /// [`CursorError::Closed`] after the last chunk OR after any failed chunk,
    /// [`CursorError::Sqlite`] on a prepare/bind/execution/decode failure (the
    /// FIRST failure is the one reported; later calls report `Closed`).
    pub fn visit_chunk(
        &mut self,
        cancel: &nd_utils::context::Cancellation,
        mut f: impl FnMut(T) -> Control,
    ) -> Result<VisitOutcome, CursorError> {
        if self.complete || self.failed {
            return Err(CursorError::Closed);
        }
        if self.next >= self.statements.len() {
            self.complete = true;
            return Ok(VisitOutcome::Completed { visited: 0 });
        }
        let statement = self.statements[self.next].clone();
        self.next += 1;
        if self.next >= self.statements.len() {
            self.complete = true;
        }
        let mut prepared = match self.conn.prepare(&statement.0) {
            Ok(prepared) => prepared,
            Err(err) => return self.fail(err),
        };
        let mut rows = match prepared.query(params_from_iter(statement.1.iter())) {
            Ok(rows) => rows,
            Err(err) => return self.fail(err),
        };
        let mut visited = 0usize;
        loop {
            match rows.next() {
                Ok(None) => return Ok(VisitOutcome::Completed { visited }),
                Err(err) => return self.fail(err),
                Ok(Some(row)) => {
                    if cancel.is_cancelled() {
                        return Ok(VisitOutcome::Stopped { visited });
                    }
                    let record = match (self.map)(row) {
                        Ok(record) => record,
                        Err(err) => return self.fail(err),
                    };
                    if let Some(key_of) = self.dedup {
                        if !self.seen.insert(key_of(&record).to_string()) {
                            continue;
                        }
                    }
                    visited += 1;
                    if f(record) == Control::Stop {
                        return Ok(VisitOutcome::Stopped { visited });
                    }
                }
            }
        }
    }
}

/// Chunk size of `GetAlbumIDsByFolder` (`slices.Chunk(folderIDs, 200)`,
/// `mediafile_repository.go:297`).
pub const ALBUM_IDS_FOLDER_CHUNK_SIZE: usize = mediafile_repo::CHUNK_SIZE;

/// `GetAlbumIDsByFolder` (`mediafile_repository.go:293-316`): distinct
/// `album_id` of non-missing tracks in each folder-chunk (or a direct child of
/// it) of the given library.
///
/// Id-chunked kind: one leased connection across all chunks, duplicate ids
/// ACROSS chunks reported as Go reports them (appended, not deduped — the
/// scanner's album queue upserts by PK). Duplicate ids WITHIN a chunk are
/// already collapsed by the statement's `SELECT distinct`.
#[must_use]
pub fn album_ids_by_folders<'c>(
    conn: &'c Connection,
    scope: &ScannerScope,
    folder_ids: &[String],
) -> IdChunkedCursor<'c, String> {
    IdChunkedCursor::new(
        conn,
        mediafile_repo::get_album_ids_by_folder_sql(scope.library_id(), folder_ids),
        id_value,
        None,
    )
}

/// Chunk size of `GetSoleAlbumArtistIDsInSubtrees`' path chunks
/// (`subtreePathChunkSize`, `folder_repository.go:204`, mirrored by
/// [`crate::album_repo::get_sole_album_artist_ids_in_subtrees_sql`]).
pub const SUBTREE_PATH_CHUNK_SIZE: usize = 100;

/// `GetSoleAlbumArtistIDsInSubtrees` (`album_repository.go:285-304`): the single
/// `albumartist[0].id` of albums whose `folder_ids` touch the given
/// library-relative subtree paths. Id-chunked, same-connection, duplicates across
/// chunks preserved (Go's `:285-304` appends per chunk; the artwork queue upserts
/// by PK, `scanner/phase_3_refresh_albums.go:218`).
#[must_use]
pub fn sole_album_artist_ids_in_subtrees<'c>(
    conn: &'c Connection,
    scope: &ScannerScope,
    paths: &[String],
) -> IdChunkedCursor<'c, String> {
    IdChunkedCursor::new(
        conn,
        album_repo::get_sole_album_artist_ids_in_subtrees_sql(scope.library_id(), paths),
        id_value,
        None,
    )
}

/// Chunk size of [`media_by_paths`]' path chunks.
///
/// Declared deviation: Go's `FindByPaths` issues ONE statement for the whole
/// batch (`mediafile_repository.go:365-404` has no chunker). The port chunks at
/// [`mediafile_repo::CHUNK_SIZE`] (200) so the IN-lists and the OR-tree stay
/// bounded (the same limit class that pins `subtreePathChunkSize` at 100,
/// `folder_repository.go:202-204`). For a set read this is result-equivalent —
/// the union over chunks is the same row set — and [`media_by_paths`] dedupes by
/// media-file id, which the single-statement form would have collapsed via its
/// one `WHERE`. Cross-chunk drift is allowed, as for every `IdChunked` read.
pub const PATH_CHUNK_SIZE: usize = mediafile_repo::CHUNK_SIZE;

/// Split paths the way Go's `FindByPaths` does (`mediafile_repository.go:371-380`):
/// a numeric prefix before `:` qualifies a library, but every path is ALSO
/// searched unqualified because the prefix is ambiguous. Library groups come out
/// in ascending library-id order (Go's `slices.Sorted(maps.Keys(…))`, `:383`).
#[must_use]
pub fn split_paths(paths: &[String]) -> (Vec<(i64, Vec<String>)>, Vec<String>) {
    let mut by_library: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    let mut unqualified: Vec<String> = Vec::with_capacity(paths.len());
    for path in paths {
        if let Some((prefix, rest)) = path.split_once(':') {
            if let Ok(library_id) = prefix.parse::<i64>() {
                by_library
                    .entry(library_id)
                    .or_default()
                    .push(rest.to_string());
            }
        }
        unqualified.push(path.clone());
    }
    (by_library.into_iter().collect(), unqualified)
}

/// `FindByPaths` (`mediafile_repository.go:365-404`), typed and scoped: the
/// caller's raw path batch is split with [`split_paths`] and every chunk runs
/// under a resolved [`UserScope`] (`None` = Go's admin/headless context, which is
/// what `cmd/missing.go:139` and `core/playlists/parse_m3u.go:97` get today). The
/// clause is rendered here from [`ScopeAlias::MediaFile`], so no caller can supply
/// clause text.
///
/// An empty batch renders no statement at all and visits nothing, matching Go's
/// early return (`:393-395`); no SQL is issued in that case.
///
/// # Errors
/// [`CursorError::Sqlite`] on a statement or decode failure.
pub fn media_by_paths(
    conn: &Connection,
    paths: &[String],
    scope: Option<&UserScope>,
    cancel: &nd_utils::context::Cancellation,
    mut f: impl FnMut(MediaFileRecord) -> Control,
) -> Result<VisitOutcome, CursorError> {
    if paths.is_empty() {
        return Ok(VisitOutcome::Completed { visited: 0 });
    }
    debug_assert!(
        scope.is_none_or(|scope| scope.alias == ScopeAlias::MediaFile),
        "a media-file path read must carry a scope resolved with ScopeAlias::MediaFile"
    );
    let clause = scope.and_then(UserScope::clause_sql);
    let mut visited = 0usize;
    // Dedup across chunks by media-file id (see PATH_CHUNK_SIZE): the row set of
    // the chunked union is the same as Go's single statement, and a path that is
    // both qualified and unqualified must still be delivered exactly once.
    let mut seen: HashSet<String> = HashSet::new();
    for chunk in paths.chunks(PATH_CHUNK_SIZE) {
        let (by_library, unqualified) = split_paths(chunk);
        let Some(statement) =
            mediafile_repo::find_by_paths_scoped_sql(&by_library, &unqualified, clause.clone())
        else {
            continue;
        };
        let mut prepared = conn.prepare(&statement.0).map_err(CursorError::Sqlite)?;
        let mut rows = prepared
            .query(params_from_iter(statement.1.iter()))
            .map_err(CursorError::Sqlite)?;
        loop {
            match rows.next() {
                Ok(None) => break,
                Err(err) => return Err(CursorError::Sqlite(err)),
                Ok(Some(row)) => {
                    if cancel.is_cancelled() {
                        return Ok(VisitOutcome::Stopped { visited });
                    }
                    let record = media_record(row).map_err(CursorError::Sqlite)?;
                    if !seen.insert(record.id.clone()) {
                        continue;
                    }
                    visited += 1;
                    if f(record) == Control::Stop {
                        return Ok(VisitOutcome::Stopped { visited });
                    }
                }
            }
        }
    }
    Ok(VisitOutcome::Completed { visited })
}

/// Why a scope came out the way it did, so callers and tests can pin the arm
/// rather than only the rendered SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeDecision {
    /// `user.is_admin` — Go's `applyLibraryFilter` skip (`:264-266`).
    Admin,
    /// Headless (`ID == "-1"`) — the same skip (`:264-266`).
    Headless,
    /// Non-admin granted every library AND the `library` table is non-empty
    /// (`userSeesAllLibraries`, `:288-299`): the clause would exclude nothing.
    AllLibraries,
    /// The deny clause applies — including a non-admin with ZERO grants against
    /// a non-empty `library` table, and the empty-`library`-table case (`:295-298`:
    /// `res.Count == 0` ⇒ `false` ⇒ clause). A scope of this arm NEVER means
    /// unrestricted.
    Denied,
}

/// Which table an API read scopes ON, as a CLOSED set of the aliases actually
/// used. Go's `applyLibraryFilter` takes the table name from the repository
/// (`sql_base_repository.go:277-281`), so the port mirrors that with a variant
/// per alias rather than a caller-supplied string: no arbitrary table name can
/// reach the clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeAlias {
    /// `media_file` — the entity's own table (`mediafile_repo::TABLE`).
    MediaFile,
    /// `f` — the folder alias of a folder-joined read (`folder f`). Owned by
    /// API-02's alias plumbing; pinned as a shape here.
    FolderAlias,
}

impl ScopeAlias {
    /// The qualified table name this alias renders as.
    #[must_use]
    pub fn table(self) -> &'static str {
        match self {
            Self::MediaFile => mediafile_repo::TABLE,
            Self::FolderAlias => "f",
        }
    }
}

/// The scope resolved for one request: an OPAQUE read authority. Its fields are
/// private, so the only way to obtain a narrowed read is to resolve one, and the
/// rendered clause text never reaches a caller.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedScope {
    /// The arm above.
    pub decision: ScopeDecision,
    /// The user's granted library count, for logging/tests (`0` for admin and
    /// headless, whose visible set is the whole table instead).
    pub granted: usize,
    /// The alias the clause is qualified on (chosen by the caller, not by the
    /// identity).
    pub alias: ScopeAlias,
    /// `Some` is the membership clause on `alias`'s table; `None` is Go's
    /// omitted-clause case. Private: it never leaves this module as a value.
    clause: Option<Sql>,
}

impl ResolvedScope {
    /// The opaque read authority this resolution produced, bound to the identity
    /// it was resolved for. This is the only way out of a `ResolvedScope`, and it
    /// hands out no clause text.
    #[must_use]
    pub fn into_user_scope(self, user_id: String) -> UserScope {
        UserScope {
            user_id,
            decision: self.decision,
            alias: self.alias,
            clause: self
                .clause
                .map(|(sql, args)| (self.alias.table().to_string(), sql, args)),
        }
    }
}

/// Opaque authority of a SYSTEM (scanner/headless) read: a library id, not a user
/// and not SQL text. Every system-only reader takes `&ScannerScope`, so a scanner
/// read cannot be widened by a caller-supplied user id, grant list or clause.
///
/// The library id is the ONLY thing it carries, and the only constructor takes a
/// typed `i64`, so there is no path from arbitrary SQL into a system read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScannerScope {
    library_id: i64,
}

impl ScannerScope {
    /// The system authority for reads scoped to one library.
    #[must_use]
    pub fn for_library(library_id: i64) -> Self {
        Self { library_id }
    }

    /// The library the read is scoped to. Readers use this; it is the whole
    /// authority's content.
    #[must_use]
    pub fn library_id(&self) -> i64 {
        self.library_id
    }
}

/// Opaque authority of an AUTHENTICATED-USER read: the identity plus the arm its
/// resolution took plus the DB-rendered clause on one [`ScopeAlias`]. The clause
/// is private, so a caller can neither construct one from text nor read it back —
/// only [`crate::scanner_queries`]'s builders render it into a statement.
///
/// `UserScope::unrestricted()` is the ONLY unrestricted form and it is reachable
/// only from an [`ScopeDecision::Admin`] or [`ScopeDecision::Headless`] resolution
/// (or explicitly, for a caller that has decided it is the admin/headless context,
/// exactly like Go's `applyLibraryFilter` skip arms).
#[derive(Debug, Clone, PartialEq)]
pub struct UserScope {
    user_id: String,
    decision: ScopeDecision,
    /// The alias the clause is qualified on.
    alias: ScopeAlias,
    /// `(table, clause, args)` — `None` is Go's omitted-clause case. Private.
    clause: Option<(String, String, Vec<Value>)>,
}

impl UserScope {
    /// The identity a scope was resolved for. Constructing one does NOT grant any
    /// scope: [`UserScope::resolve`] must run on a leased connection before the
    /// authority narrows anything.
    #[must_use]
    pub fn for_user(user_id: String) -> Self {
        Self {
            user_id,
            decision: ScopeDecision::Denied,
            alias: ScopeAlias::MediaFile,
            clause: None,
        }
    }

    /// Go's clause-omitted arms, reached by [`resolve_user`] for an admin or the
    /// headless id. Public so an admin/headless caller (Go's `cmd/missing.go:139`,
    /// `core/playlists/parse_m3u.go:97`) can state that context explicitly without
    /// a user row — it is the ONLY unrestricted constructor and it renders no
    /// clause at all.
    #[must_use]
    pub fn unrestricted(user_id: String) -> Self {
        Self {
            user_id,
            decision: ScopeDecision::Headless,
            alias: ScopeAlias::MediaFile,
            clause: None,
        }
    }

    /// Resolve this identity on a leased connection and return the authority for
    /// `alias`. Same arms as [`resolve_user`] — see it for the semantics.
    ///
    /// # Errors
    /// [`ScopeError`] when a resolution read fails. A failed resolution never
    /// degrades to an unrestricted scope.
    pub fn resolve(&self, conn: &Connection, alias: ScopeAlias) -> Result<Self, ScopeError> {
        resolve_user(conn, self.user_id.clone(), alias)
    }

    /// The arm this authority came from.
    #[must_use]
    pub fn decision(&self) -> ScopeDecision {
        self.decision
    }

    /// The alias this authority's clause is qualified on.
    #[must_use]
    pub fn alias(&self) -> ScopeAlias {
        self.alias
    }

    /// The identity this authority was resolved for.
    #[must_use]
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// The clause for this module's builders ONLY. Not part of the public API:
    /// `#[doc(hidden)]` keeps it out of the documented surface while letting this
    /// crate's tests pin the rendered shape.
    #[doc(hidden)]
    #[must_use]
    pub fn rendered_clause(&self) -> Option<(String, String, Vec<Value>)> {
        self.clause.clone()
    }

    /// Render for this module's builders (`None` = omit the clause). Private: the
    /// clause text is never handed to a caller.
    fn clause_sql(&self) -> Option<Sql> {
        self.clause
            .as_ref()
            .map(|(_, sql, args)| (sql.clone(), args.clone()))
    }
}

/// Resolve `user_id` into a [`UserScope`] for `alias` on a leased connection.
///
/// Arms (Go's `applyLibraryFilter` `sql_base_repository.go:261-283` with
/// `userSeesAllLibraries` `:288-299`): admin ⇒ unrestricted; headless `"-1"` ⇒
/// unrestricted WITHOUT reading a user row (Go's `loggedUser` synthesizes that id,
/// `:51-57`); non-admin whose grant count equals `count(*) FROM library` with that
/// count greater than zero ⇒ unrestricted; EVERYTHING ELSE ⇒ the deny clause,
/// including zero grants against a non-empty library table and an empty library
/// table (`:295-298`), so an empty allowed set denies rather than opens the read.
///
/// Reads only [`crate::user_repo::UserRepo::get`] (which supplies `is_admin` and
/// the hydrated `libraries`) and [`crate::library_repo::LibraryRepo::count_all`];
/// never a caller-supplied `is_admin` bool or library id list. A `user` row that
/// grants nothing is a legitimate state, not an error, and a missing row behaves
/// like one that grants nothing (Go's `loggedUser` returns a synthesized user).
///
/// # Errors
/// [`ScopeError::Sqlite`] when the user read fails, [`ScopeError::LibraryCount`]
/// when the `library` count read fails. A failed resolution NEVER degrades to an
/// unrestricted scope.
pub fn resolve_user(
    conn: &Connection,
    user_id: String,
    alias: ScopeAlias,
) -> Result<UserScope, ScopeError> {
    if user_id == HEADLESS_USER_ID {
        return Ok(UserScope::unrestricted(user_id));
    }
    let resolved = resolve_scope(conn, alias, user_id.as_str())?;
    Ok(resolved.into_user_scope(user_id))
}

/// The `<table>.library_id IN (SELECT ul.library_id FROM user_library ul WHERE
/// ul.user_id = ?)` clause of `applyLibraryFilter`
/// (`sql_base_repository.go:271-283`), with the user id bound.
///
/// Takes [`ScopeAlias`], not a table string: the set of aliasable tables is closed.
/// There is NO `1=1` predicate in Go's user arm — `1=1` appears there only as
/// squirrel's empty-conjunction rendering — so this builder has no unrestricted
/// form: unrestricted is [`UserScope::unrestricted`], decided by resolution.
#[doc(hidden)]
#[must_use]
pub fn membership_clause(alias: ScopeAlias, user_id: &str) -> Sql {
    (
        format!(
            "{}.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)",
            alias.table()
        ),
        vec![Value::from(user_id)],
    )
}

/// The artist scope arm (`artist_repository.go:191-203` + `artistLibraryFilter`
/// `:640-650`): artist has no `library_id` column, so scope is an EXISTS over
/// `library_artist`, and the EMPTY id list renders Go's `Eq{"1": 2}` —
/// [`crate::artist_repo::FALSE_EXPR`] — i.e. match nothing, never unrestricted.
/// Artist scope is keyed by library ids rather than by a table alias, so it stays a
/// separate builder.
#[doc(hidden)]
#[must_use]
pub fn artist_scope_clause(library_ids: &[i64]) -> Sql {
    artist_repo::artist_library_filter(library_ids)
}

/// Failure of scope resolution.
#[derive(Debug, thiserror::Error)]
pub enum ScopeError {
    /// The user read failed. Distinct from "no grants": a failed resolution must
    /// never silently become an unrestricted scope.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// The `count(*) FROM library` resolution read failed (statement, scan, or
    /// Go's `toSQL` materialization failure). Reported rather than treated as a
    /// count of zero, which would otherwise turn a failing read into a denied
    /// scope.
    #[error(transparent)]
    LibraryCount(#[from] crate::library_repo::LibraryError),
}

/// Resolve the request scope ONCE on a leased connection, mirroring Go's
/// `applyLibraryFilter` decision order (`sql_base_repository.go:261-283` with
/// `userSeesAllLibraries` `:288-299`):
///
/// 1. admin ⇒ clause omitted;
/// 2. headless `"-1"` ⇒ clause omitted, WITHOUT reading a user row (Go's
///    `loggedUser` synthesizes that id, `:51-57`);
/// 3. non-admin whose grant count equals `count(*) FROM library` with that count
///    greater than zero ⇒ clause omitted;
/// 4. everything ELSE ⇒ the deny clause: zero grants against a non-empty
///    library table, a missing user row, and an empty library table all take this
///    arm (`:295-298`), so an empty allowed set denies rather than opens the read.
///
/// Reads only [`crate::user_repo::UserRepo::get`] (which supplies `is_admin` and
/// the hydrated `libraries`) and [`crate::library_repo::LibraryRepo::count_all`];
/// never a caller-supplied `is_admin` bool or library id list.
///
/// A `user` row that exists but grants nothing is a legitimate state, not an
/// error: it takes the [`ScopeDecision::Denied`] arm, which is what Go renders
/// for a user with no grants (a missing row falls through the same LEFT JOINs).
///
/// # Errors
/// [`ScopeError::Sqlite`] when the user read fails, [`ScopeError::LibraryCount`]
/// when the `library` count read fails. A failed resolution NEVER degrades to an
/// unrestricted scope.
pub fn resolve_scope(
    conn: &Connection,
    alias: ScopeAlias,
    user_id: &str,
) -> Result<ResolvedScope, ScopeError> {
    if user_id == HEADLESS_USER_ID {
        return Ok(ResolvedScope {
            decision: ScopeDecision::Headless,
            granted: 0,
            alias,
            clause: None,
        });
    }
    let user = match UserRepo::new(conn).get(user_id) {
        Ok(user) => user,
        // A `user` row that does not exist behaves like one that grants nothing
        // (is_admin false, no libraries), which takes the Denied arm below.
        Err(UserError::NotFound) => crate::user_repo::UserRecord {
            id: user_id.to_string(),
            user_name: String::new(),
            name: String::new(),
            email: String::new(),
            is_admin: false,
            last_login_at: None,
            last_access_at: None,
            created_at: String::new(),
            updated_at: String::new(),
            scrobble_filter: String::new(),
            password: String::new(),
            token_epoch: 0,
            libraries: Vec::new(),
        },
        Err(UserError::Sqlite(sqlite)) => return Err(ScopeError::Sqlite(sqlite)),
    };
    if user.is_admin {
        return Ok(ResolvedScope {
            decision: ScopeDecision::Admin,
            granted: user.libraries.len(),
            alias,
            clause: None,
        });
    }
    let granted = user.libraries.len();
    let total = LibraryRepo::new(conn)
        .count_all()
        .map_err(ScopeError::LibraryCount)?;
    let decision = if total > 0 && i64::try_from(granted).unwrap_or(i64::MAX) == total {
        ScopeDecision::AllLibraries
    } else {
        ScopeDecision::Denied
    };
    let clause = (decision == ScopeDecision::Denied).then(|| membership_clause(alias, user_id));
    Ok(ResolvedScope {
        decision,
        granted,
        alias,
        clause,
    })
}

/// Failure of [`get_visible_media`], so the API lane can distinguish "not yours"
/// from "database failed".
#[derive(Debug, thiserror::Error)]
pub enum VisibleMediaError {
    /// No visible media file with this id for this user — Go's
    /// `model.ErrNotFound` (`mediafile_repository.go:217-219`), which is also
    /// what a library-filtered-out row produces.
    #[error("record not found")]
    NotFound,

    /// The scope resolution or the read failed.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// The scope resolution failed (`library` count read). Reported as its own
    /// arm so a failing resolution is never mistaken for a denied read.
    #[error(transparent)]
    Scope(#[from] ScopeError),
}

/// The API-safe point read: resolve the caller's scope ONCE on this leased
/// connection, then hand the resulting [`UserScope`]'s clause to the existing
/// [`crate::mediafile_repo::MediaFileRepo::get`] unchanged. The rendered clause is
/// consumed inside this call — no caller can obtain it, and no SQL text enters the
/// signature.
///
/// `cover_flag` is Go's `conf.Server.EnableMediaFileCoverArt`
/// (`conf/configuration.go:1003`) supplied by the caller; the artwork-lookup
/// failure keeps being swallowed the way Go swallows it
/// (`artwork_hydration.go:62-66`), so an unresolved artwork field is not an error.
///
/// `user_id == "-1"` is Go's headless identity: the clause is omitted AND the
/// annotation/bookmark joins are skipped, because `withAnnotation`/`withBookmark`
/// return early for `invalidUserId` (`sql_annotations.go:63-65`). That is the
/// only place the port maps the id to `None`; a real user id is passed through so
/// the same annotation and bookmark columns come back as Go's.
///
/// # Errors
/// [`VisibleMediaError::NotFound`] when the row is absent or outside the scope;
/// [`VisibleMediaError::Sqlite`] on a resolution or statement failure.
pub fn get_visible_media(
    conn: &Connection,
    id: &str,
    user_id: &str,
    cover_flag: bool,
) -> Result<MediaFileRecord, VisibleMediaError> {
    let scope = resolve_user(conn, user_id.to_string(), ScopeAlias::MediaFile)?;
    let repo_user_id = if user_id == HEADLESS_USER_ID {
        None
    } else {
        Some(user_id)
    };
    MediaFileRepo::new(conn)
        .get(id, repo_user_id, scope.clause_sql(), cover_flag)
        .map_err(|err| match err {
            mediafile_repo::MediaFileError::NotFound => VisibleMediaError::NotFound,
            mediafile_repo::MediaFileError::Sqlite(sqlite) => VisibleMediaError::Sqlite(sqlite),
        })
}

/// `GetRandom`-style phase 2 bounds, exposed as chrono types so the scanner lane
/// cannot pass a raw timestamp string. Not used by the reads above (which take
/// `since` from the missing row), but it is the typed seam the subplan asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeBound<'a> {
    /// Inclusive lower bound (`>=`).
    pub from: &'a DateTime<Utc>,
    /// Exclusive upper bound (`<`).
    pub until: &'a DateTime<Utc>,
}

impl TimeBound<'_> {
    /// Whether `candidate` lies inside the bound (`from <= t < until`).
    #[must_use]
    pub fn contains(&self, candidate: &DateTime<Utc>) -> bool {
        candidate >= self.from && candidate < self.until
    }
}

/// The folder-side scope clause (`folder.library_id IN (…)`), delegated to the
/// existing builder so there is one renderer for it. `#[doc(hidden)]`: it renders
/// clause text, so it is an internal seam, not part of the public API surface.
#[doc(hidden)]
#[must_use]
pub fn folder_scope_clause(user_id: Option<&str>) -> Option<Sql> {
    folder_repo::library_filter(user_id)
}
