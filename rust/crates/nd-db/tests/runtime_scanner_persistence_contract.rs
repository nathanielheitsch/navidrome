//! DB-21 (P1-DB) — integrated DB-side persistence acceptance for the phase-1 scanner
//! contract, the frozen P1 API surface, and the consumer-handoff ledger.
//!
//! Scope is **DB-side only** (parent ruling 1; `subplans/db-runtime.md:895` PARENT DECIDED
//! 2026-11-21). Three deliverables: (a) an executed regression proving the landed phase-1
//! surfaces compose through the REAL pool, (b) a compilation-level API-surface freeze, (c) a
//! consumer-handoff ledger pinning every DB-external acceptance as **U**.
//! `production_full_finalize_db_contract` (full exit) is out of this package, and
//! DB-21(P1-DB) is NOT #27A progress.
//!
//! Integrated, not unit-level: every write goes through `nd_db::scanner_writes::persist_folder_delta`
//! (DB-14, `scanner/phase_1_folders.go:374-475` `persistFolder` + `:478-506` `persistAlbum`)
//! and every read through `nd_db::scanner_queries` (DB-11), on a `SQLStore` wrapping a real
//! `Pool` of production-shaped handles over a file DB from the fresh DDL
//! (`nd_dbschema::schema::apply_fresh_schema`, `_foreign_keys=on`). The only raw SQL is
//! (i) seeds for tables with no production write API (`user`, `user_library`, `annotation`,
//! `item_artwork`) and (ii) the controlled fault-injection triggers subplan field 7 allows.
//!
//! Read-back discipline: assertions read stored ROWS, junction sets, side tables and FTS
//! `MATCH` counts — never rendered mutation SQL.
//!
//! Oracle labels (`decisions.md` C4): no executable Go oracle exists for these methods in
//! this checkout (the `persistence/*_test.go` suites are Ginkgo against a Go DB), so
//! expectations are pinned to Go source lines and labeled EXECUTED-RUST / SOURCE-VERIFIED.
//!
//! Non-vacuity discipline (DB-21 review round 2): every pass-2 assertion below reads a STORED
//! value that one specific arm wrote, and each is paired with either a `assert_ne!` against the
//! value the arm did NOT write, or a CONTROL pass that runs the batch WITHOUT the input that
//! drives the arm (`(7)` in `production_db_phase1_contract_first_and_second_pass`, the new-tag
//! probe, and the zero-grant negative control in `production_read_scopes_and_queue_fts_match`).
//! Album `created_at` values are DISTINCT per album, so the CopyAttributes arm's effect — the
//! destination holding the SOURCE's stamp — is the only value that satisfies its assertions.

use std::collections::BTreeMap;
use std::convert::Infallible;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use nd_db::album_repo::{
    participant_names_from_participants, put_album_in_tx, AlbumRecord, AlbumWriteError,
    CopyAlbumCreatedAtOutcome,
};
use nd_db::artist_repo::{ArtistColumn, ArtistRecord};
use nd_db::artwork_queue_repo::{
    enqueue_if_missing_in_tx, enqueue_in_tx, ArtworkQueueItem, EnqueueMode,
};
use nd_db::artwork_repo::delete_for_items_in_tx;
use nd_db::folder_repo::{mark_folders_missing_in_tx, put_folder_in_tx, FolderRecord};
use nd_db::library_repo::{
    add_artist_in_tx, put_in_tx, scan_begin_in_tx, scan_end_in_tx, set_scanned_pid_in_tx,
    LibraryColumn, LibraryRecord, LibraryWriteError,
};
use nd_db::mediafile_repo::{
    mark_media_missing_by_folder_in_tx, mark_media_missing_in_tx, put_mediafile_in_tx,
    MediaFileRecord, MediaFileWriteResult, MediaIdentity,
};
use nd_db::pool::{Pool, PoolConfig, MIN_POOL_SIZE};
use nd_db::property_repo::{delete_in_tx, put_in_tx as put_property_in_tx};
use nd_db::scanner_queries::{
    album_ids_by_folders, artist_scope_clause, folder_scope_clause, get_visible_media,
    media_by_paths, membership_clause, resolve_scope, resolve_user,
    sole_album_artist_ids_in_subtrees, split_paths, visit_media_in_album, visit_media_in_filter,
    visit_media_in_folder, visit_missing_and_matching, visit_touched_albums, Control, CursorError,
    IdChunkedCursor, MediaPhaseFilter, ScannerScope, ScopeAlias, ScopeDecision, TimeBound,
    UserScope, VisibleMediaError, VisitOutcome, ALBUM_IDS_FOLDER_CHUNK_SIZE, HEADLESS_USER_ID,
    PATH_CHUNK_SIZE, SUBTREE_PATH_CHUNK_SIZE,
};
use nd_db::scanner_writes::{
    persist_folder_delta, scan_artwork_item, AlbumTransition, CommittedFolderOutcome,
    FolderRunError, FolderStamps, FolderWriteBatch, FolderWriteStage, MissingMedia, ScopeRejection,
    SCAN_ARTWORK_IMAGE_TYPE, SCAN_ARTWORK_PRIORITY,
};
use nd_db::sql_annotations::{
    annotation_count_sql, average_rating_expr_sql, reassign_annotation_in_tx,
    update_avg_rating_in_tx, AnnotationEntityKind,
};
use nd_db::sql_participations::{Participant, Participants, Role};
use nd_db::sql_tags::DbTags;
use nd_db::sqlrepo::Value;
use nd_db::store::{is_busy, try_with_tx, try_with_tx_retry, SQLStore};
use nd_db::tag_repo::{add_in_tx, Tag};
use nd_db::user_repo::revoke_tokens_in_tx;
use nd_db::{time, DbOperationError, DbRuntimeError, TransactionStage};
use nd_dbschema::schema::{apply_fresh_schema, DbOpenOptions, JournalMode, Synchronous};
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use rusqlite::Error as SqliteError;
/// The primary configured library.
const LIB_MAIN: i64 = 1;
/// A second configured library, crossed by batch 3 and used as the containment decoy.
const LIB_OTHER: i64 = 2;

// ---------------------------------------------------------------------------
// Deliverable (b): the P1 surface freeze.
//
// Each `const _: fn(...) = path` is a COMPILE-TIME assertion that the named item exists at
// the stated path with the stated parameter and return types; a signature drift breaks the
// COMPILATION of this test file, not merely a behaviour assertion. `pub` visibility is part
// of what is pinned — an item that loses `pub` stops type-checking here.
//
// The generic visitor/cursor callbacks cannot be ascribed as function pointers (rustc
// rejects `impl Trait` and bounded `for<F: FnMut>` in a `fn` pointer type), so those four
// surfaces are pinned by MONOMORPHIZED CALLS in the read test below, which fixes their
// concrete parameter and return types just as strictly.
//
// What CANNOT be compile-pinned from an integration test, recorded in the U-ledger instead:
//   * `scanner_writes::persist_folder_delta_in_tx` — `pub(crate)` by `D-BATCH`, so the crate
//     boundary rejects an ascription here (ledger entry `db_internal_contract`).
//   * `#[doc(hidden)]` seams (`UserScope::rendered_clause`, `membership_clause`,
//     `artist_scope_clause`, `media_cursor_sql`, `IdChunkedCursor::new`) — executable here
//     but deliberately not part of the documented consumer surface.
// ---------------------------------------------------------------------------

/// Signature pin for `persist_folder_delta`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig01 = fn(&SQLStore, &FolderWriteBatch) -> Result<CommittedFolderOutcome, FolderRunError>;
const _: Sig01 = persist_folder_delta;
/// Signature pin for `scan_artwork_item`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig02 = fn(&str, &str, i64) -> ArtworkQueueItem;
const _: Sig02 = scan_artwork_item;
/// Signature pin for `put_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig03 =
    fn(
        &rusqlite::Transaction<'_>,
        &LibraryRecord,
        &DateTime<Utc>,
        &[LibraryColumn],
    ) -> Result<nd_db::library_repo::LibraryWriteResult, DbOperationError<LibraryWriteError>>;
const _: Sig03 = put_in_tx;
/// Signature pin for `scan_begin_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig29 = fn(
    &rusqlite::Transaction<'_>,
    i64,
    &DateTime<Utc>,
    bool,
) -> Result<(), nd_db::library_repo::LibraryScanError>;
const _: Sig29 = scan_begin_in_tx;
/// Signature pin for `scan_end_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig30 = fn(
    &rusqlite::Transaction<'_>,
    i64,
    &DateTime<Utc>,
) -> Result<(), nd_db::library_repo::LibraryScanError>;
const _: Sig30 = scan_end_in_tx;
/// Signature pin for `set_scanned_pid_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig31 = fn(
    &rusqlite::Transaction<'_>,
    i64,
    &str,
    &str,
) -> Result<(), nd_db::library_repo::LibraryScanError>;
const _: Sig31 = set_scanned_pid_in_tx;
/// Signature pin for `add_artist_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig32 =
    fn(&rusqlite::Transaction<'_>, i64, &str) -> Result<(), nd_db::library_repo::LibraryScanError>;
const _: Sig32 = add_artist_in_tx;
/// Signature pin for `put_folder_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig33 = fn(
    &rusqlite::Transaction<'_>,
    &FolderRecord,
    &DateTime<Utc>,
    &[&str],
) -> Result<
    nd_db::folder_repo::FolderWriteResult,
    DbOperationError<nd_db::folder_repo::FolderWriteError>,
>;
const _: Sig33 = put_folder_in_tx;
/// Signature pin for `mark_folders_missing_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig34 = fn(
    &rusqlite::Transaction<'_>,
    bool,
    &[String],
    &DateTime<Utc>,
) -> Result<usize, nd_db::folder_repo::FolderMarkError>;
const _: Sig34 = mark_folders_missing_in_tx;
/// Signature pin for `mark_media_missing_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig35 = fn(
    &rusqlite::Transaction<'_>,
    bool,
    &[String],
    &DateTime<Utc>,
) -> Result<usize, nd_db::mediafile_repo::MediaMarkError>;
const _: Sig35 = mark_media_missing_in_tx;
/// Signature pin for `mark_media_missing_by_folder_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig36 = fn(
    &rusqlite::Transaction<'_>,
    bool,
    &[String],
    &DateTime<Utc>,
) -> Result<usize, nd_db::mediafile_repo::MediaMarkError>;
const _: Sig36 = mark_media_missing_by_folder_in_tx;
/// Signature pin for `nd_db::album_repo::touch_albums_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig37 = fn(
    &rusqlite::Transaction<'_>,
    &[String],
    &DateTime<Utc>,
) -> Result<usize, nd_db::album_repo::AlbumTouchError>;
const _: Sig37 = nd_db::album_repo::touch_albums_in_tx;
/// Signature pin for `nd_db::album_repo::touch_albums_with_missing_folders_globally_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig38 = fn(
    &rusqlite::Transaction<'_>,
    &DateTime<Utc>,
) -> Result<usize, nd_db::album_repo::AlbumTouchError>;
const _: Sig38 = nd_db::album_repo::touch_albums_with_missing_folders_globally_in_tx;
/// Signature pin for `nd_db::artist_repo::put_artist_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig39 = fn(
    &rusqlite::Transaction<'_>,
    &ArtistRecord,
    &DateTime<Utc>,
    &[ArtistColumn],
) -> Result<
    nd_db::artist_repo::ArtistWriteResult,
    DbOperationError<nd_db::artist_repo::ArtistWriteError>,
>;
const _: Sig39 = nd_db::artist_repo::put_artist_in_tx;
/// Signature pin for `put_album_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig40 = fn(
    &rusqlite::Transaction<'_>,
    &AlbumRecord,
    &DateTime<Utc>,
) -> Result<nd_db::album_repo::AlbumWriteResult, DbOperationError<AlbumWriteError>>;
const _: Sig40 = put_album_in_tx;
/// Signature pin for `nd_db::album_repo::copy_album_created_at_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig41 = fn(
    &rusqlite::Transaction<'_>,
    &str,
    &str,
) -> Result<CopyAlbumCreatedAtOutcome, DbOperationError<Infallible>>;
const _: Sig41 = nd_db::album_repo::copy_album_created_at_in_tx;
/// Signature pin for `put_mediafile_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig04 = fn(
    &rusqlite::Transaction<'_>,
    &MediaIdentity,
    &MediaFileRecord,
    &DateTime<Utc>,
    bool,
) -> Result<
    MediaFileWriteResult,
    DbOperationError<nd_db::mediafile_repo::MediaFileWriteError>,
>;
const _: Sig04 = put_mediafile_in_tx;
/// Signature pin for `add_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig05 =
    fn(&rusqlite::Transaction<'_>, i64, &[Tag]) -> Result<(), nd_db::tag_repo::TagWriteError>;
const _: Sig05 = add_in_tx;
/// Signature pin for `put_property_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig42 = fn(
    &rusqlite::Transaction<'_>,
    &str,
    &str,
) -> Result<(), nd_db::property_repo::PropertyWriteError>;
const _: Sig42 = put_property_in_tx;
/// Signature pin for `delete_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig43 =
    fn(&rusqlite::Transaction<'_>, &str) -> Result<(), nd_db::property_repo::PropertyWriteError>;
const _: Sig43 = delete_in_tx;
/// Signature pin for `delete_for_items_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig44 = fn(
    &rusqlite::Transaction<'_>,
    &str,
    &[String],
) -> Result<usize, nd_db::artwork_repo::ArtworkDeleteFailure>;
const _: Sig44 = delete_for_items_in_tx;
/// Signature pin for `enqueue_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig45 = fn(
    &rusqlite::Transaction<'_>,
    &[ArtworkQueueItem],
    EnqueueMode,
    &DateTime<Utc>,
) -> Result<(), nd_db::artwork_queue_repo::EnqueueFailure>;
const _: Sig45 = enqueue_in_tx;
/// Signature pin for `enqueue_if_missing_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig46 = fn(
    &rusqlite::Transaction<'_>,
    &[ArtworkQueueItem],
    &DateTime<Utc>,
) -> Result<(), nd_db::artwork_queue_repo::EnqueueFailure>;
const _: Sig46 = enqueue_if_missing_in_tx;
/// Signature pin for `revoke_tokens_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig47 =
    fn(
        &rusqlite::Transaction<'_>,
        &str,
    )
        -> Result<nd_db::user_repo::NewTokenEpoch, DbOperationError<nd_db::user_repo::RevokeError>>;
const _: Sig47 = revoke_tokens_in_tx;
/// Signature pin for `update_avg_rating_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig48 = fn(
    &rusqlite::Transaction<'_>,
    AnnotationEntityKind,
    &str,
) -> Result<(), nd_db::sql_annotations::AnnotationWriteError>;
const _: Sig48 = update_avg_rating_in_tx;
/// Signature pin for `reassign_annotation_in_tx`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig49 = fn(
    &rusqlite::Transaction<'_>,
    AnnotationEntityKind,
    &str,
    &str,
) -> Result<(), nd_db::sql_annotations::AnnotationWriteError>;
const _: Sig49 = reassign_annotation_in_tx;

/// DB-02 canonical time and DB-03 pool/open-options handles. The DB-00 FREE seam functions
/// (`store::try_with_tx`:140, `store::try_with_tx_retry`:223) take their body as a by-value
/// generic `impl Fn(&Transaction<'_>)` whose elided inner lifetime is quantified more
/// broadly than any `fn` pointer type can name (rustc E0308), so they are pinned by the
/// EXECUTED, explicitly-monomorphized calls in `p1_surface_freeze_compiles` instead —
/// which fixes their parameter and return types at least as strictly as an ascription.
/// Signature pin for `SQLStore::try_with_tx_retry_sync`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig06 = fn(
    &SQLStore,
    fn(&rusqlite::Transaction<'_>) -> Result<u8, DbOperationError<Infallible>>,
) -> Result<u8, DbRuntimeError<Infallible>>;
const _: Sig06 = SQLStore::try_with_tx_retry_sync;
/// Signature pin for `SQLStore::try_with_tx_sync`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig07 = fn(
    &SQLStore,
    fn(&rusqlite::Transaction<'_>) -> Result<u8, DbOperationError<Infallible>>,
) -> Result<u8, DbRuntimeError<Infallible>>;
const _: Sig07 = SQLStore::try_with_tx_sync;
/// Signature pin for `is_busy`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig08 = fn(&SqliteError) -> bool;
const _: Sig08 = is_busy;
/// Signature pin for `time::value`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig09 = fn(&DateTime<Utc>) -> Value;
const _: Sig09 = time::value;
/// Signature pin for `time::to_text`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig10 = fn(&DateTime<Utc>) -> String;
const _: Sig10 = time::to_text;
const _: fn() -> DateTime<Utc> = time::zero;
/// Signature pin for `time::parse`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig11 = fn(&str) -> Option<DateTime<Utc>>;
const _: Sig11 = time::parse;
const _: fn(&PoolConfig) -> usize = |config| config.size;
const _: fn(&DbOpenOptions) -> u64 = |options| options.busy_timeout;
/// Signature pin for `JournalMode::sqlite_name`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig12 = fn(JournalMode) -> &'static str;
const _: Sig12 = JournalMode::sqlite_name;
const _: fn(Synchronous) -> i64 = |s| match s {
    Synchronous::Off => 0,
    Synchronous::Normal => 1,
    Synchronous::Full => 2,
    Synchronous::Extra => 3,
};

/// DB-11 public surface that is not generic.
const _: for<'a, 'b, 'c> fn(
    &'a Connection,
    &'b ScannerScope,
    &'c [String],
) -> IdChunkedCursor<'a, String> = album_ids_by_folders;
/// Signature pin for `resolve_user`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig50 =
    fn(&Connection, String, ScopeAlias) -> Result<UserScope, nd_db::scanner_queries::ScopeError>;
const _: Sig50 = resolve_user;
/// Signature pin for `resolve_scope`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig51 =
    fn(
        &Connection,
        ScopeAlias,
        &str,
    ) -> Result<nd_db::scanner_queries::ResolvedScope, nd_db::scanner_queries::ScopeError>;
const _: Sig51 = resolve_scope;
/// Signature pin for `get_visible_media`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig13 = fn(&Connection, &str, &str, bool) -> Result<MediaFileRecord, VisibleMediaError>;
const _: Sig13 = get_visible_media;
/// Signature pin for `ScannerScope::library_id`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig14 = fn(&ScannerScope) -> i64;
const _: Sig14 = ScannerScope::library_id;
/// Signature pin for `ScannerScope::for_library`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig15 = fn(i64) -> ScannerScope;
const _: Sig15 = ScannerScope::for_library;
/// Signature pin for `UserScope::for_user`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig16 = fn(String) -> UserScope;
const _: Sig16 = UserScope::for_user;
/// Signature pin for `UserScope::unrestricted`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig17 = fn(String) -> UserScope;
const _: Sig17 = UserScope::unrestricted;
/// Signature pin for `UserScope::decision`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig18 = fn(&UserScope) -> ScopeDecision;
const _: Sig18 = UserScope::decision;
/// Signature pin for `UserScope::alias`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig19 = fn(&UserScope) -> ScopeAlias;
const _: Sig19 = UserScope::alias;
/// Signature pin for `UserScope::user_id`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig20 = fn(&UserScope) -> &str;
const _: Sig20 = UserScope::user_id;
/// Signature pin for `ScopeAlias::table`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig21 = fn(ScopeAlias) -> &'static str;
const _: Sig21 = ScopeAlias::table;
/// `IdChunkedCursor::remaining_chunks`/`is_complete` take `&self` with an unused inner
/// lifetime, so no `fn` pointer type names them; they are pinned by executed calls in
/// `production_read_scopes_and_queue_fts_match`.
/// Signature pin for `split_paths`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig22 = fn(&[String]) -> (Vec<(i64, Vec<String>)>, Vec<String>);
const _: Sig22 = split_paths;
/// Signature pin for `AnnotationEntityKind::table`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig23 = fn(AnnotationEntityKind) -> &'static str;
const _: Sig23 = AnnotationEntityKind::table;
/// Signature pin for `FolderWriteBatch::validate`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig24 = fn(&FolderWriteBatch) -> Result<(), ScopeRejection>;
const _: Sig24 = FolderWriteBatch::validate;
/// Signature pin for `FolderWriteBatch::queue_items`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig25 = fn(&FolderWriteBatch) -> Vec<ArtworkQueueItem>;
const _: Sig25 = FolderWriteBatch::queue_items;
/// Signature pin for `FolderWriteBatch::album_id_map`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig26 = fn(&FolderWriteBatch) -> BTreeMap<String, String>;
const _: Sig26 = FolderWriteBatch::album_id_map;
/// Signature pin for `FolderWriteStage::is_fatal`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig27 = fn(FolderWriteStage) -> bool;
const _: Sig27 = FolderWriteStage::is_fatal;
/// Signature pin for `FolderWriteStage::as_str`: naming the `fn` pointer type in an alias keeps the
/// ascription readable and satisfies clippy `type_complexity` without a lint
/// attribute. A signature drift still breaks this file's compilation.
type Sig28 = fn(FolderWriteStage) -> &'static str;
const _: Sig28 = FolderWriteStage::as_str;
// `TimeBound::contains` (`scanner_queries.rs:1199`) takes `&self` on a generic whose inner
// lifetime is unconstrained by the method, so no `fn` pointer type names it (rustc E0308 in
// both directions). It is pinned by an EXECUTED call in `p1_surface_freeze_compiles`.

/// Doc-asserted constants a consumer reads as part of the contract.
/// `model.ArtworkPriorityScan` (`model/artwork.go:116`) is **50**. Sized-array ascriptions are
/// compile-time pins: a drift in either constant breaks this file's COMPILATION, not a runtime
/// assertion, so a silent 50 -> 20 change cannot ship past this lane.
const _: [u8; SCAN_ARTWORK_PRIORITY as usize] = [0u8; 50];
const _: [u8; nd_db::playlist_repo::ARTWORK_PRIORITY_SCAN as usize] = [0u8; 50];
/// `artworkChunkSize` (`persistence/artwork_hydration.go:17`, `artworkBatchSize * 3` with
/// `artworkBatchSize = 200` at `persistence/artwork_repository.go:16`) is **600**; mirrored by
/// `artist_repo::CURSOR_CHUNK_SIZE` (`artist_repo.rs:155-162`, corrected from 300 by
/// `D-CURSOR(4)`).
const _: [u8; nd_db::artist_repo::CURSOR_CHUNK_SIZE] = [0u8; 600];
/// The queue's own `slices.Chunk` size (`artwork_queue_repo::CHUNK_SIZE`, 100), and the
/// folder/album-id chunk (`mediafile_repo::CHUNK_SIZE`, 200 = Go's `artworkBatchSize`,
/// `persistence/artwork_repository.go:16`) that `ALBUM_IDS_FOLDER_CHUNK_SIZE` delegates.
const _: [u8; nd_db::artwork_queue_repo::CHUNK_SIZE] = [0u8; 100];
const _: [u8; ALBUM_IDS_FOLDER_CHUNK_SIZE] = [0u8; 200];
const _: i64 = SCAN_ARTWORK_PRIORITY;
const _: &str = SCAN_ARTWORK_IMAGE_TYPE;
const _: &str = nd_db::scanner_writes::QUEUE_EXCLUDED_ALBUM_NAME;
const _: [ArtistColumn; 7] = nd_db::scanner_writes::ARTIST_UPDATE_COLUMNS;
const _: usize = ALBUM_IDS_FOLDER_CHUNK_SIZE;
const _: usize = PATH_CHUNK_SIZE;
const _: usize = SUBTREE_PATH_CHUNK_SIZE;
const _: &str = HEADLESS_USER_ID;
const _: i64 = nd_db::playlist_repo::ARTWORK_PRIORITY_SCAN;
const _: usize = MIN_POOL_SIZE;
const _: [&str; 2] = nd_db::scanner_writes::QUEUE_EXCLUDED_ARTIST_NAMES;

// ---------------------------------------------------------------------------
// Deliverable (c): consumer-handoff ledger. Every DB-external acceptance is U.
// ---------------------------------------------------------------------------

/// The acceptance state. `Unmet` is the only state this lane may assert; moving an entry
/// off U is a parent/docs-lane decision, never a test-side inference (`decisions.md` C3/C4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerAcceptance {
    /// Not met: no executed consumer evidence exists for this acceptance.
    Unmet,
}

impl ConsumerAcceptance {
    /// The checklist token the subplan's acceptance states use for unmet.
    #[must_use]
    pub const fn status_str(self) -> &'static str {
        match self {
            Self::Unmet => "U",
        }
    }
}

/// One acceptance owed by a lane outside `nd-db`.
#[derive(Debug, Clone, Copy)]
pub struct ConsumerHandoff {
    /// Stable acceptance id (never renamed — subplan field 10).
    pub id: &'static str,
    /// The acceptance state.
    pub status: ConsumerAcceptance,
    /// The exact gate that must land before this can move off U.
    pub pending_gate: &'static str,
    /// The owning lane.
    pub owner: &'static str,
}

/// The P1-DB handoff ledger. `production_full_finalize_db_contract` is carried with status
/// U because the FULL exit is OUT of this package, not because it is met.
pub const CONSUMER_HANDOFF: [ConsumerHandoff; 5] = [
    ConsumerHandoff {
        id: "SC.mapping-bridge",
        status: ConsumerAcceptance::Unmet,
        pending_gate: "SC-01 ratification: the nd-mapping bridge is not ratified, and the \
                       distinct MediaFile DTO (map_mediafile.rs:24, mdfile.rs:37,131-165) still \
                       has storage fields absent until SC supplies them",
        owner: "SC (mapping bridge)",
    },
    ConsumerHandoff {
        id: "SC.phase1-fixture",
        status: ConsumerAcceptance::Unmet,
        pending_gate: "SC-01 ratification + an nd-scanner manifest delta adding an nd-db \
                       dependency (manifest authority is SC-01's; nd-scanner declares no nd-db \
                       dependency, has no scanner entrypoint and no tag-to-DB ingest)",
        owner: "SC (phase-1 fixture + manifest)",
    },
    ConsumerHandoff {
        id: "BOOT.configured-library-caller",
        status: ConsumerAcceptance::Unmet,
        pending_gate: "BOOT-01 is the IMPLEMENTING package (boot-api.md:151-167, \
                       `nd-cli/src/{bootstrap.rs,state.rs}` + `nd-cli/tests/bootstrap.rs`: \
                       'Fresh DB, configured library, secrets and transactional setup'), which \
                       itself is BLOCKED on its prerequisites BOOT-00 (approve \
                       construction/dependency/launch deltas — boot-api.md:127, DECISION \
                       REQUIRED), EXT-DB-CONNECTION and EXT-DB-LIBRARY. BOOT-00 approves \
                       construction/dependency deltas only; it is NOT delivered caller \
                       evidence. Today nd-cli `main.rs` is an empty main, so no boot path \
                       creates the configured library through the DB-05 typed put.",
        owner: "BOOT",
    },
    ConsumerHandoff {
        id: "production_full_finalize_db_contract",
        status: ConsumerAcceptance::Unmet,
        pending_gate: "DB-16..DB-20 landed + SC.full-phase-contract + \
                       SC.playlist-import-contract (the FULL-SCANNER-DB exit, explicitly out of \
                       this P1-DB package)",
        owner: "DB spine (DB-16..DB-20), then DB-21(FULL-SCANNER-DB)",
    },
    ConsumerHandoff {
        id: "db_internal_contract",
        status: ConsumerAcceptance::Unmet,
        pending_gate: "not a consumer gate: scanner_writes::persist_folder_delta_in_tx is \
                       pub(crate) by D-BATCH and CANNOT be compile-pinned from an integration \
                       test; pinning it needs a crate-internal test or a recorded D-BATCH \
                       widening when SC-05 consumes it",
        owner: "DB (D-BATCH widening / SC-05 consumer)",
    },
];

/// The single pin line this lane's status axis is allowed to carry.
const LEDGER_PIN_LINE: &str = "DB-21(P1-DB)'s DB-side exit is delivered by THIS file only; \
    every acceptance above is U until its named gate lands. DB-21(P1-DB) is NOT #27A progress, \
    and no DB-external acceptance may be reported green from this lane.";

// ---------------------------------------------------------------------------
// Deferred renderer-prepare gate (parent decision 2, dated 2026-11-21): NOT implemented.
//
// `subplans/db-runtime.md:628-632` recommends one executed gate that prepares every
// renderer's statement against the migrated schema and asserts
// `parameter_count() == args.len()`. A naive all-statements gate FALSE-FAILS on five known
// classes, so the gate is deliberately absent. When DB-16/DB-17 land, scope it by excluding
// exactly these classes and their owning lanes:
//
// 1. `playlist_repo::refresh_counters_sql` (:476) — INTENTIONALLY PARTIAL shape: it returns a
//    `(select, update)` pair whose UPDATE args are completed by the caller from the SELECT's
//    aggregate row, so `args.len() != parameter_count()` by design. Owning lanes:
//    **DB-04..DB-17** — the subplan says to wait for the mutation renderers to stabilize, or
//    the gate fails on this intentionally-partial shape.
// 2. `share_repo::update_sql` (:248) with an EMPTY `cols` list — renders
//    `UPDATE share SET  WHERE id = ?`, which SQLite rejects as a PARSE/SYNTAX error
//    (`near "WHERE": syntax error`, rusqlite `SqliteFailure(Error::SqliteBuild, …)`), because
//    Go's `filterUpdateValues` always re-adds `id` (`persistence/sql_base_repository.go:612-616`,
//    `selectUpdateColumns` :593-609). Owning lane: **DB-16**, whose brief asserts an empty column
//    list must never reach this renderer. Gate-scoping TRAP: a prepare gate that accepts BOTH
//    `no such column:` and `syntax error` conceals which category this is — it is a RENDERER
//    defect (an empty SET list), not a schema defect, and the assertion below pins exactly the
//    syntax-error category.
// 3. `plugin_repo` — every renderer fails with `no such table: plugin` because the table is
//    deliberately dropped (**D-PLUGIN**, `decisions.md`; `schema-contract.md:93-96`). Owning
//    lane: **DB-16**, whose brief carries the deletion of `plugin_repo.rs` and the `lib.rs:35`
//    export. This lane owns neither file and deletes nothing.
// 4. `sql_annotations::annotation_count_sql` (:622) — emits `count(distinct annotation.id)`
//    while `annotation` has NO `id` column (`schema_ddl.rs:59`), so it can never prepare.
//    Known and pinned at **DB-15** (parent decision: keep-and-redocument, with the exact
//    `no such column: annotation.id` failure asserted rather than silently shipped).
// 5. Clause FRAGMENTS — `artist_scope_clause` (:630), `membership_clause`,
//    `media_cursor_sql`'s predicate and `album_repo::select_album`'s raw filters are
//    conjunctions joined by the caller, not standalone statements; preparing one alone is
//    meaningless. Exclude them by SHAPE (fragment vs statement), not by a name whitelist that
//    drifts as lanes land.
//
// Executed in `deferred_renderer_prepare_gate_classes_are_executed` so a future gate cannot
// claim any of these classes is schema-supported.
const DEFERRED_GATE_CLASSES: [&str; 5] = [
    "refresh_counters_sql partial shape (owning lanes DB-04..DB-17)",
    "share::update_sql empty cols -> syntax error (owning lane DB-16)",
    "plugin_repo no such table: plugin (D-PLUGIN; deletion in DB-16's brief)",
    "annotation_count_sql no such column: annotation.id (owning lane DB-15)",
    "clause fragments artist_scope_clause/membership_clause/media_cursor_sql (exclude by shape)",
];

// ---------------------------------------------------------------------------
// Fixture plumbing
// ---------------------------------------------------------------------------

/// One distinct caller-stamped instant per Go `time.Now()` site (D-BATCH forbids clock reads
/// inside the retry closure, so SC — here, the test — supplies every stamp).
#[must_use]
fn at(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, s)
            .unwrap(),
    )
}

/// One distinct instant per Go `time.Now()` site: the minute separates batches, the seconds
/// separate the sites within one batch.
#[must_use]
fn stamps(minute: u32) -> FolderStamps {
    FolderStamps {
        folder_put: at(2019, 1, 2, 0, minute, 1),
        artist_put: at(2019, 1, 2, 0, minute, 2),
        album_put: at(2019, 1, 2, 0, minute, 3),
        mediafile_put: at(2019, 1, 2, 0, minute, 4),
        mark_missing: at(2019, 1, 2, 0, minute, 5),
        album_touch: at(2019, 1, 2, 0, minute, 6),
        artwork: at(2019, 1, 2, 0, minute, 7),
    }
}

/// An older instant for pre-seeded rows, distinct from every batch stamp.
#[must_use]
fn seed_stamp() -> DateTime<Utc> {
    at(2011, 5, 6, 7, 8, 9)
}

/// A DISTINCT, realistic (non-year-one) `album.created_at` per album id. Every album in the
/// corpus gets its own value, so Go's CopyAttributes arm (`album_repository.go:331-357`,
/// ported at `album_repo.rs:1566` `copy_album_created_at_in_tx`) is the ONLY thing that can
/// put another album's stamp on a destination row: delete that arm and the assertion below
/// fails. Distinctness is asserted by `created_at_values_are_distinct`.
#[must_use]
fn created_of(id: &str) -> DateTime<Utc> {
    let sum: usize = id.as_bytes().iter().map(|b| *b as usize).sum();
    at(
        2013,
        1 + (sum % 12) as u32,
        1 + (sum % 27) as u32,
        1 + (sum % 23) as u32,
        (sum % 60) as u32,
        (sum % 60) as u32,
    )
}

/// Go's default DSN shape minus the pragmas `open_with_options` sets per handle.
fn dsn(path: &std::path::Path) -> String {
    format!("file:{}?mode=rwc&_foreign_keys=on", path.display())
}

/// One isolated on-disk DB per invocation, freshly migrated, pooled at Go's floor of 4
/// handles (`db/db.go:62` `SetMaxOpenConns(max(4, NumCPU))`), and EVERY handle carries the
/// same explicit `DbOpenOptions` — the DB-03 every-handle registration this freeze must show.
fn fresh_store(label: &str) -> (SQLStore, std::path::PathBuf) {
    let path = temp_path(label);
    let config = PoolConfig {
        path: dsn(&path),
        size: MIN_POOL_SIZE,
        options: Some(DbOpenOptions {
            journal_mode: Some(JournalMode::Wal),
            busy_timeout: 15_000,
            synchronous: Synchronous::Normal,
            foreign_keys: Some(true),
            cache_size: None,
        }),
    };
    let (pool, first) = Pool::new(&config).unwrap();
    apply_fresh_schema(&first).unwrap();
    pool.put_back(first).expect("return configured handle");
    assert_eq!(pool.size(), MIN_POOL_SIZE);
    (SQLStore::new(pool), path)
}

/// Same production-shaped handle setup as [`fresh_store`], but with an explicit pool size so a
/// test can watch a lease leave and return to the idle queue.
fn fresh_store_sized(label: &str, size: usize) -> (SQLStore, std::path::PathBuf) {
    let path = temp_path(label);
    let config = PoolConfig {
        path: dsn(&path),
        size,
        options: Some(DbOpenOptions {
            journal_mode: Some(JournalMode::Wal),
            busy_timeout: 15_000,
            synchronous: Synchronous::Normal,
            foreign_keys: Some(true),
            cache_size: None,
        }),
    };
    let (pool, first) = Pool::new(&config).unwrap();
    apply_fresh_schema(&first).unwrap();
    pool.put_back(first).expect("return configured handle");
    assert_eq!(pool.size(), size);
    (SQLStore::new(pool), path)
}

fn temp_path(label: &str) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        &format!("nd-db-db21-{label}-"),
        ".db",
    ));
    if path.exists() {
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("cannot clear {path:?}: {e}"));
    }
    path
}

fn cleanup(store: SQLStore, path: std::path::PathBuf) {
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// One raw block on a pooled lease. Never called inside a transaction (that would re-check
/// out the same pool and block, per the pool's single-lease rule).
fn lease<T>(store: &SQLStore, call: impl FnOnce(&Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("lease failed: {e}"))
}

/// A raw fixture statement: seeds for tables with no production write API, and the
/// controlled fault-injection triggers subplan field 7 permits.
fn fixture(store: &SQLStore, sql: &str, args: &[SqlValue]) {
    lease(store, |conn| {
        conn.execute(sql, rusqlite::params_from_iter(args.iter()))
            .unwrap_or_else(|e| panic!("{sql} --{args:?}--: {e}"));
    });
}

fn text(value: impl std::fmt::Display) -> SqlValue {
    SqlValue::Text(value.to_string())
}
fn integer(value: i64) -> SqlValue {
    SqlValue::Integer(value)
}

/// One scalar probe on a FRESH lease (state after a commit or a rollback).
fn scalar<T>(store: &SQLStore, sql: &str) -> T
where
    T: rusqlite::types::FromSql,
{
    lease(store, |conn| {
        conn.query_row(sql, [], |row| row.get(0))
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    })
}

/// Cell text of one row, for stamp assertions. REAL columns need [`real_cell`]: rusqlite
/// rejects a Real cell read as String, so a TEXT-shaped probe is not universal.
fn cell(store: &SQLStore, table: &str, column: &str, id: &str) -> Option<String> {
    lease(store, |conn| {
        conn.query_row(
            &format!("SELECT {column} FROM {table} WHERE id = ?"),
            [text(id)],
            |row| row.get::<_, Option<String>>(0),
        )
        .unwrap_or_else(|e| panic!("{table}.{column} for {id}: {e}"))
    })
}

/// Cell value of a REAL column (`album.average_rating` is `REAL NOT NULL DEFAULT 0`,
/// `schema_ddl.rs:58`).
fn real_cell(store: &SQLStore, table: &str, column: &str, id: &str) -> f64 {
    lease(store, |conn| {
        conn.query_row(
            &format!("SELECT {column} FROM {table} WHERE id = ?"),
            [text(id)],
            |row| row.get::<_, f64>(0),
        )
        .unwrap_or_else(|e| panic!("{table}.{column} for {id}: {e}"))
    })
}

/// A set-valued probe, sorted: junction/tag/queue assertions are set equality, never
/// "some rows exist".
fn set_of(store: &SQLStore, sql: &str) -> Vec<String> {
    lease(store, |conn| {
        let mut statement = conn.prepare(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
        let mut out: Vec<String> = rows
            .map(|r| r.unwrap_or_else(|e| panic!("{sql}: {e}")))
            .collect();
        out.sort();
        out
    })
}

/// `MATCH` hit count against a contentless FTS table (`content=''` makes the row count and
/// the MATCH answers the only observable).
fn fts_match(store: &SQLStore, table: &str, query: &str) -> i64 {
    lease(store, |conn| {
        conn.query_row(
            &format!("SELECT count(*) FROM {table} WHERE {table} MATCH ?"),
            [text(query)],
            |row| row.get(0),
        )
        .unwrap_or_else(|e| panic!("{table} MATCH {query}: {e}"))
    })
}

/// `MATCH` hits resolved back to entity ids through the contentless table's rowid join.
fn fts_ids(store: &SQLStore, table: &str, base: &str, query: &str) -> Vec<String> {
    lease(store, |conn| {
        let mut statement = conn
            .prepare(&format!(
                "SELECT b.id FROM {table} f JOIN {base} b ON b.rowid = f.rowid WHERE {table} MATCH ? ORDER BY b.id"
            ))
            .unwrap_or_else(|e| panic!("{table} join for {query}: {e}"));
        let rows = statement
            .query_map([text(query)], |row| row.get::<_, String>(0))
            .unwrap_or_else(|e| panic!("{table} join for {query}: {e}"));
        rows.map(|r| r.unwrap_or_else(|e| panic!("{table} join: {e}")))
            .collect()
    })
}

/// Base-row rowids, so a re-scan is pinned to the SAME row positions (an FTS ghost would
/// show as a MATCH hit whose rowid no longer joins).
fn rowids(store: &SQLStore, table: &str, ids: &[&str]) -> Vec<i64> {
    lease(store, |conn| {
        ids.iter()
            .map(|id| {
                conn.query_row(
                    &format!("SELECT rowid FROM {table} WHERE id = ?"),
                    [text(*id)],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or_else(|e| panic!("{table} rowid for {id}: {e}"))
            })
            .collect()
    })
}

/// Two configured libraries through the PRODUCTION DB-05 write (`put_in_tx`).
fn seed_libraries(store: &SQLStore) {
    for (id, tag) in [(LIB_MAIN, "main"), (LIB_OTHER, "other")] {
        let record = LibraryRecord {
            id,
            name: format!("lib-{tag}-{id}"),
            path: format!("/lib-{tag}-{id}"),
            remote_path: String::new(),
            last_scan_at: seed_stamp(),
            // An UNSCANNED library carries Go's year-one zero here, which is exactly what
            // `scan_in_progress_sql`'s `last_scan_started_at <> ?` predicate compares
            // against (`library_repository.go:190-194`). Seeding it non-zero would make
            // every library read as in-progress.
            last_scan_started_at: time::zero(),
            full_scan_in_progress: false,
            updated_at: seed_stamp(),
            created_at: seed_stamp(),
            total_songs: 0,
            total_albums: 0,
            total_artists: 0,
            total_folders: 0,
            total_files: 0,
            total_missing_files: 0,
            total_size: 0,
            total_duration: 0.0,
            default_new_users: false,
            pid_album: String::new(),
            pid_track: String::new(),
            scanned_pid_album: String::new(),
            scanned_pid_track: String::new(),
        };
        store
            .try_with_tx_sync::<nd_db::library_repo::LibraryWriteResult, LibraryWriteError>(|tx| {
                put_in_tx(tx, &record, &seed_stamp(), &[] as &[LibraryColumn])
            })
            .unwrap_or_else(|e| panic!("seed_libraries({id}) failed: {e}"));
    }
}

/// `user` + `user_library` rows. No production write API exists for either in this lane
/// (`user_repo` has no `put_in_tx`; the user surface belongs to SC/BOOT), so these are
/// fixture seeds — the same precedent as `runtime_scanner_queries.rs:165-197`.
fn seed_user(store: &SQLStore, id: &str, is_admin: bool) {
    fixture(
        store,
        "INSERT INTO \"user\" (id, user_name, name, email, password, is_admin, created_at, \
         updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            text(id),
            text(id),
            text(id),
            text(format!("{id}@x.y")),
            text("CIPHER"),
            integer(i64::from(is_admin)),
            text(time::to_text(&seed_stamp())),
            text(time::to_text(&seed_stamp())),
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

/// An annotation row. `annotation` has no production write API in this lane (DB-15 owns the
/// MOVES, not the inserts), so seeding one is the fixture; `rating` is explicit so the
/// average-rating assertion has a real population and `rating > 0` holds.
fn seed_annotation(
    store: &SQLStore,
    user_id: &str,
    item_id: &str,
    item_type: &str,
    play_count: i64,
    rating: i64,
) {
    fixture(
        store,
        "INSERT INTO annotation (user_id, item_id, item_type, play_count, rating, starred) \
         VALUES (?, ?, ?, ?, ?, 1)",
        &[
            text(user_id),
            text(item_id),
            text(item_type),
            integer(play_count),
            integer(rating),
        ],
    );
}

/// An `item_artwork` row with a resolved hash (`hash <> ''`). Deleting these is what the
/// warn-only invalidation arm must do, and a decoy of the SAME kind proves the arm is scoped
/// to the batch's ids.
fn seed_item_artwork(store: &SQLStore, kind: &str, id: &str) {
    fixture(
        store,
        "INSERT INTO item_artwork (item_kind, item_id, image_type, hash, source, source_path, \
         ref_mtime, attempted_at, updated_at, trace, last_failure) \
         VALUES (?, ?, 'primary', 'hash-1', 'cover', '', 0, ?, ?, '[]', '[]')",
        &[
            text(kind),
            text(id),
            text(time::to_text(&seed_stamp())),
            text(time::to_text(&seed_stamp())),
        ],
    );
}

fn participants(primary: &str, second: &str) -> Participants {
    let mut participants = Participants::new();
    participants.insert(
        Role::from("albumartist"),
        vec![Participant {
            id: primary.to_string(),
            name: "Main Act".to_string(),
            sub_role: String::new(),
        }],
    );
    participants.insert(
        Role::from("composer"),
        vec![Participant {
            id: second.to_string(),
            name: "Second Act".to_string(),
            sub_role: String::new(),
        }],
    );
    participants
}

/// Genre (the only indexed tag name, `sql_tags::INDEXED_TAG_NAMES`) plus a JSON-only name.
fn fixture_tags() -> DbTags {
    let mut tags = DbTags::new();
    tags.insert("genre".to_string(), vec!["Rock".to_string()]);
    tags.insert("albumversion".to_string(), vec!["Deluxe".to_string()]);
    tags
}

/// An FTS-safe single token: `unicode61` splits on `-`, so an unquoted `mf-1` query would
/// parse as column syntax rather than as a term.
#[must_use]
fn token(id: &str) -> String {
    id.replace('-', "")
}

fn artist_record(id: &str, name: &str) -> ArtistRecord {
    ArtistRecord {
        id: id.to_string(),
        name: name.to_string(),
        sort_artist_name: format!("sort-{id}"),
        order_artist_name: format!("order-{id}"),
        mbz_artist_id: format!("mbz-{id}"),
        similar_artists: "[]".to_string(),
        ..ArtistRecord::default()
    }
}

/// An album record. `created_at` is a DISTINCT real (non-year-one) stamp supplied by the
/// caller: the UPDATE leg never writes `created_at` (`marshal.rs` UPDATE_DROP), so the only
/// way a destination album can end up holding ANOTHER album's stamp is Go's CopyAttributes
/// arm (`album_repository.go:331-357`, ported as `copy_album_created_at_in_tx`). Distinct
/// per-album values are what make that arm's effect — and its absence — observable.
fn album_record(
    id: &str,
    library_id: i64,
    primary: &str,
    second: &str,
    folder_id: &str,
    created: DateTime<Utc>,
) -> AlbumRecord {
    let parties = participants(primary, second);
    let mut discs = BTreeMap::new();
    discs.insert(1, format!("disc-one-{id}"));
    AlbumRecord {
        id: id.to_string(),
        library_id,
        name: format!("Albumid{}", token(id)),
        embed_art_path: format!("embed-{id}"),
        album_artist: "Main Act".to_string(),
        album_artist_id: primary.to_string(),
        min_year: 1979,
        max_year: 1981,
        date: format!("date-{id}"),
        min_original_year: 1978,
        max_original_year: 1977,
        original_date: format!("orig-{id}"),
        release_date: format!("rel-{id}"),
        compilation: false,
        comment: format!("cmt-{id}"),
        song_count: 1,
        duration: 60.5,
        size: 4096,
        discs: Some(discs),
        sort_album_name: format!("sort-{id}"),
        sort_album_artist_name: format!("sortaa-{id}"),
        order_album_name: format!("order-{id}"),
        order_album_artist_name: format!("orderaa-{id}"),
        catalog_num: format!("cat-{id}"),
        mbz_album_id: format!("mbz-{id}"),
        mbz_album_artist_id: format!("mbza-{id}"),
        mbz_album_type: "album".to_string(),
        mbz_album_comment: format!("mbzc-{id}"),
        mbz_release_group_id: format!("mbzrg-{id}"),
        folder_ids: Some(vec![folder_id.to_string()]),
        explicit_status: format!("exp-{id}"),
        rg_album_gain: Some(-8.25),
        rg_album_peak: Some(0.75),
        description: format!("desc-{id}"),
        small_image_url: format!("small-{id}"),
        medium_image_url: format!("medium-{id}"),
        large_image_url: format!("large-{id}"),
        external_url: format!("ext-{id}"),
        external_info_updated_at: Some(seed_stamp()),
        genre: "Rock".to_string(),
        tags: fixture_tags(),
        participant_names: participant_names_from_participants(&parties),
        missing: false,
        imported_at: seed_stamp(),
        created_at: Some(created),
        updated_at: Some(seed_stamp()),
        library_path: String::new(),
        library_name: String::new(),
        annotation: nd_model::annotation::Annotation::default(),
        genres: Vec::new(),
        image: nd_db::artwork_hydration::ItemImage::default(),
        participants: parties,
    }
}

/// A media record. `path`+`library_id` are what Go's `Put` builds its match identity from
/// (`mediafile_repository.go:189`), and `title` carries an FTS-safe term per record.
fn media_record(
    id: &str,
    album_id: &str,
    folder_id: &str,
    path: &str,
    library_id: i64,
    album_artist_id: &str,
) -> MediaFileRecord {
    let parties = participants(album_artist_id, "ar-second");
    MediaFileRecord {
        id: id.to_string(),
        album: format!("Albumid{}", token(album_id)),
        album_artist: "Main Act".to_string(),
        album_artist_id: album_artist_id.to_string(),
        album_id: album_id.to_string(),
        artist: "Main Act".to_string(),
        artist_id: album_artist_id.to_string(),
        birth_time: seed_stamp(),
        bit_depth: None,
        bit_rate: 320,
        bpm: None,
        catalog_num: format!("cat-{id}"),
        channels: 2,
        codec: format!("codec-{id}"),
        comment: format!("cmt-{id}"),
        compilation: false,
        created_at: Some(seed_stamp()),
        date: format!("date-{id}"),
        disc_number: 1,
        disc_subtitle: format!("dsub-{id}"),
        duration: 60.25,
        explicit_status: format!("exp-{id}"),
        folder_id: folder_id.to_string(),
        genre: "Rock".to_string(),
        has_cover_art: false,
        library_id,
        lyrics: format!("lyr-{id}"),
        mbz_recording_id: format!("mr-{id}"),
        mbz_release_track_id: format!("mrt-{id}"),
        mbz_album_id: format!("mal-{id}"),
        mbz_release_group_id: format!("mrg-{id}"),
        mbz_artist_id: format!("mar-{id}"),
        mbz_album_artist_id: format!("maa-{id}"),
        mbz_album_type: "album".to_string(),
        mbz_album_comment: format!("mac-{id}"),
        missing: false,
        order_album_artist_name: format!("oaa-{id}"),
        order_album_name: format!("oal-{id}"),
        order_artist_name: format!("oar-{id}"),
        order_title: format!("oti-{id}"),
        original_date: format!("orig-{id}"),
        original_year: 1978,
        path: path.to_string(),
        pid: format!("pid-{id}"),
        probe_data: format!(r#"{{"codec":"mp3-{id}"}}"#),
        release_date: format!("rel-{id}"),
        release_year: 1980,
        rg_album_gain: Some(0.5),
        rg_album_peak: Some(0.9),
        rg_track_gain: Some(-0.3),
        rg_track_peak: Some(0.7),
        sample_rate: 44_100,
        size: 2048,
        sort_album_artist_name: format!("saa-{id}"),
        sort_album_name: format!("sal-{id}"),
        sort_artist_name: format!("sar-{id}"),
        sort_title: format!("sti-{id}"),
        suffix: "mp3".to_string(),
        tags: fixture_tags(),
        title: format!("Zbrfldr{}", token(id)),
        track_number: 1,
        year: 1980,
        participant_names: participant_names_from_participants(&parties),
        updated_at: Some(seed_stamp()),
        library_path: String::new(),
        library_name: String::new(),
        annotation: nd_model::annotation::Annotation::default(),
        bookmark_position: 0,
        genres: Vec::new(),
        image: nd_db::artwork_hydration::ItemImage::default(),
        album_image: nd_db::artwork_hydration::ItemImage::default(),
        participants: parties,
    }
}

fn folder_record(id: &str, library_id: i64, path: &str, num_audio_files: i64) -> FolderRecord {
    FolderRecord {
        id: id.to_string(),
        library_id,
        path: path.to_string(),
        name: path.to_string(),
        parent_id: String::new(),
        num_audio_files,
        num_playlists: 0,
        image_files: Some(vec![format!("cover-{id}.jpg")]),
        images_updated_at: seed_stamp(),
        hash: format!("hash-{id}"),
        missing: false,
        updated_at: seed_stamp(),
        created_at: Some(seed_stamp()),
        library_path: String::new(),
    }
}

/// Commit one album through the production DB-09 write (a row the delta must UPDATE and the
/// transition must inherit from).
fn seed_album(store: &SQLStore, record: &AlbumRecord) {
    store
        .try_with_tx_sync::<nd_db::album_repo::AlbumWriteResult, AlbumWriteError>(|tx| {
            put_album_in_tx(tx, record, &seed_stamp())
        })
        .unwrap_or_else(|e| panic!("seed_album({}) failed: {e}", record.id));
}

/// Run a delta and panic on any failure — the shape most read-back assertions need.
fn run_ok(store: &SQLStore, batch: &FolderWriteBatch) -> CommittedFolderOutcome {
    persist_folder_delta(store, batch)
        .unwrap_or_else(|e| panic!("persist_folder_delta({}) failed: {e}", batch.folder.id))
}

/// The whole observable corpus state, so a rollback test compares against a real baseline
/// rather than against "no rows".
fn state(store: &SQLStore) -> Vec<(String, i64)> {
    [
        "folder",
        "tag",
        "library_tag",
        "artist",
        "library_artist",
        "album",
        "album_artists",
        "album_tags",
        "media_file",
        "media_file_artists",
        "media_file_tags",
        "artwork_queue",
        "item_artwork",
        "annotation",
    ]
    .iter()
    .map(|t| {
        (
            t.to_string(),
            scalar::<i64>(store, &format!("SELECT count(*) FROM {t}")),
        )
    })
    .collect()
}

// ---------------------------------------------------------------------------
// The phase-1 corpus: four folder deltas, crossed libraries, three album-ID
// transitions (two with a live source row, one silently-ignored missing source),
// both full_scan_mode values, and one match-arm (empty-id) record.
// ---------------------------------------------------------------------------

/// Batch 1: folder `fo-1`, album `al-1` (transition `al-old` -> `al-1`), tracks `mf-1`/`mf-2`,
/// artists `ar-main`/`ar-second` plus the two names Go excludes from the queue (`:412`).
/// `full_scan_mode = false` → `Enqueue` (Reset mode, `:467`).
fn batch_one(minute: u32) -> FolderWriteBatch {
    FolderWriteBatch {
        library_id: LIB_MAIN,
        folder: folder_record("fo-1", LIB_MAIN, "album-one", 2),
        tags: vec![
            Tag {
                id: nd_id::new_tag_id("genre", "Rock"),
                tag_name: "genre".to_string(),
                tag_value: "Rock".to_string(),
                ..Tag::default()
            },
            Tag {
                id: nd_id::new_tag_id("albumversion", "Deluxe"),
                tag_name: "albumversion".to_string(),
                tag_value: "Deluxe".to_string(),
                ..Tag::default()
            },
        ],
        artists: vec![
            artist_record("ar-main", "Main Act"),
            artist_record("ar-second", "Second Act"),
            artist_record("ar-unknown", nd_conf::consts::UNKNOWN_ARTIST),
            artist_record("ar-va", nd_conf::consts::VARIOUS_ARTISTS),
        ],
        albums: vec![album_record(
            "al-1",
            LIB_MAIN,
            "ar-main",
            "ar-second",
            "fo-1",
            created_of("al-1"),
        )],
        media: vec![
            media_record(
                "mf-1",
                "al-1",
                "fo-1",
                "album-one/track-01.mp3",
                LIB_MAIN,
                "ar-main",
            ),
            media_record(
                "mf-2",
                "al-1",
                "fo-1",
                "album-one/track-02.mp3",
                LIB_MAIN,
                "ar-main",
            ),
        ],
        missing: Vec::new(),
        album_transitions: vec![AlbumTransition {
            new_id: "al-1".to_string(),
            prev_id: "al-old".to_string(),
        }],
        full_scan_mode: false,
        append_subtitle: false,
        stamps: stamps(minute),
    }
}

/// Batch 2: folder `fo-2`, album `al-2` (transition `al-old2` -> `al-2`), track `mf-3` plus a
/// record with an EMPTY id, which takes Go's `putByMatch` probe arm
/// (`sql_base_repository.go:581-588`) and then the generated-id arm (`:635-638`).
/// `full_scan_mode = true` → `EnqueueIfMissing` (`:467-469`). `missing` carries
/// `mf-gone`/`al-2`, so MarkMissing (`:446`) and Touch (`:450-456`) run.
fn batch_two(minute: u32) -> FolderWriteBatch {
    let mut probe = media_record(
        "",
        "al-2",
        "fo-2",
        "album-two/track-03.mp3",
        LIB_MAIN,
        "ar-second",
    );
    probe.id = String::new();
    probe.title = "Zbrfldrprobe".to_string();
    probe.pid = "pid-probe".to_string();
    FolderWriteBatch {
        library_id: LIB_MAIN,
        folder: folder_record("fo-2", LIB_MAIN, "album-two", 2),
        tags: vec![Tag {
            id: nd_id::new_tag_id("genre", "Rock"),
            tag_name: "genre".to_string(),
            tag_value: "Rock".to_string(),
            ..Tag::default()
        }],
        artists: vec![artist_record("ar-second", "Second Act")],
        albums: vec![album_record(
            "al-2",
            LIB_MAIN,
            "ar-second",
            "ar-main",
            "fo-2",
            created_of("al-2"),
        )],
        media: vec![
            media_record(
                "mf-3",
                "al-2",
                "fo-2",
                "album-two/track-02.mp3",
                LIB_MAIN,
                "ar-second",
            ),
            probe,
        ],
        missing: vec![MissingMedia {
            id: "mf-gone".to_string(),
            album_id: "al-2".to_string(),
        }],
        album_transitions: vec![AlbumTransition {
            new_id: "al-2".to_string(),
            prev_id: "al-old2".to_string(),
        }],
        full_scan_mode: true,
        append_subtitle: true,
        stamps: stamps(minute),
    }
}

/// Batch 3: the same surface crossed into `LIB_OTHER`, proving multi-library composition
/// through the real FKs (`library_artist`, `album.library_id`, `media_file.library_id`).
fn batch_three(minute: u32) -> FolderWriteBatch {
    FolderWriteBatch {
        library_id: LIB_OTHER,
        folder: folder_record("fo-9", LIB_OTHER, "other/album-nine", 1),
        tags: vec![Tag {
            id: nd_id::new_tag_id("genre", "Jazz"),
            tag_name: "genre".to_string(),
            tag_value: "Jazz".to_string(),
            ..Tag::default()
        }],
        artists: vec![artist_record("ar-nine", "Nine Act")],
        albums: vec![album_record(
            "al-9",
            LIB_OTHER,
            "ar-nine",
            "ar-nine",
            "fo-9",
            created_of("al-9"),
        )],
        media: vec![media_record(
            "mf-9",
            "al-9",
            "fo-9",
            "other/album-nine/track-01.mp3",
            LIB_OTHER,
            "ar-nine",
        )],
        missing: Vec::new(),
        album_transitions: Vec::new(),
        full_scan_mode: false,
        append_subtitle: false,
        stamps: stamps(minute),
    }
}

/// Batch 4: a transition whose previous album row does NOT exist (`al-missing` -> `al-3`),
/// pinning the SILENTLY-ignored source-not-found arm (`:496-500`) — a case that must NOT
/// produce a warning — inside the same composed run.
fn batch_four(minute: u32) -> FolderWriteBatch {
    FolderWriteBatch {
        library_id: LIB_MAIN,
        folder: folder_record("fo-3", LIB_MAIN, "album-three", 1),
        tags: Vec::new(),
        artists: vec![artist_record("ar-main", "Main Act")],
        albums: vec![album_record(
            "al-3",
            LIB_MAIN,
            "ar-main",
            "ar-main",
            "fo-3",
            created_of("al-3"),
        )],
        media: vec![media_record(
            "mf-4",
            "al-3",
            "fo-3",
            "album-three/track-01.mp3",
            LIB_MAIN,
            "ar-main",
        )],
        missing: Vec::new(),
        album_transitions: vec![AlbumTransition {
            new_id: "al-3".to_string(),
            prev_id: "al-missing".to_string(),
        }],
        full_scan_mode: true,
        append_subtitle: false,
        stamps: stamps(minute),
    }
}

/// The four deltas in Go's per-folder order.
fn corpus(minute: u32) -> Vec<FolderWriteBatch> {
    vec![
        batch_one(minute),
        batch_two(minute),
        batch_three(minute),
        batch_four(minute),
    ]
}

/// The rows the transitions need in order to inherit something: `al-old` and `al-old2` exist
/// as albums (written through the production DB-09 write) and carry annotations, plus a
/// decoy annotation and `item_artwork` rows including a decoy of the same kind.
fn seed_transition_sources(store: &SQLStore) {
    seed_album(
        store,
        &album_record(
            "al-old",
            LIB_MAIN,
            "ar-main",
            "ar-second",
            "fo-old",
            created_of("al-old"),
        ),
    );
    seed_album(
        store,
        &album_record(
            "al-old2",
            LIB_MAIN,
            "ar-second",
            "ar-main",
            "fo-old2",
            created_of("al-old2"),
        ),
    );
    seed_user(store, "u-fan", false);
    seed_annotation(store, "u-fan", "al-old", "album", 4, 8);
    seed_annotation(store, "u-fan", "al-old2", "album", 2, 5);
    seed_annotation(store, "u-fan", "al-decoy", "album", 9, 9);
    seed_item_artwork(store, nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, "mf-1");
    seed_item_artwork(store, nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, "mf-9");
    seed_item_artwork(store, nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, "mf-decoy");
}

/// Run the corpus and return the four outcomes in order.
fn run_corpus(store: &SQLStore, minute: u32) -> Vec<CommittedFolderOutcome> {
    corpus(minute)
        .iter()
        .map(|batch| run_ok(store, batch))
        .collect()
}

// ---------------------------------------------------------------------------
// Re-scan dirty state (the seven arms pass 2 must consume)
// ---------------------------------------------------------------------------

/// Rows the re-scan pass consumes as DIRTY STATE, committed against pass 1's results. Each
/// arm is read back against its ACTUAL stored effect below, so dropping that arm from
/// `persist_folder_delta` fails an assertion rather than passing silently.
fn seed_dirty_state(store: &SQLStore) -> DirtyFacts {
    // (i) A track re-imported with a fresh EMPTY id: Go builds `Eq{path, library_id}` from
    // the record (`mediafile_repository.go:189`) and `putByMatch` probes it
    // (`sql_base_repository.go:581-588`), so the COMMITTED id must be reused, not minted.
    let mut probe = media_record(
        "mf-1",
        "al-1",
        "fo-1",
        "album-one/track-01.mp3",
        LIB_MAIN,
        "ar-main",
    );
    probe.id = String::new();
    let reimport = run_ok(
        store,
        &FolderWriteBatch {
            library_id: LIB_MAIN,
            folder: folder_record("fo-1", LIB_MAIN, "album-one", 2),
            tags: Vec::new(),
            artists: Vec::new(),
            albums: Vec::new(),
            media: vec![probe],
            missing: Vec::new(),
            album_transitions: Vec::new(),
            full_scan_mode: true,
            append_subtitle: false,
            stamps: stamps(33),
        },
    );
    assert_eq!(
        reimport.inserted_media_ids,
        Vec::<String>::new(),
        "the probe found the row"
    );
    let reimport_id = reimport.media_ids[0].clone();

    // (ii) mf-2 is a live pass-1 row, and pass 2 hands it to MarkMissing (`:446`). Its
    // stored flag and stamp are pinned HERE so the arm's real row mutation is observable —
    // the earlier shape only ever carried an id with no row, which proved the zero-chunk
    // early return and nothing else.
    let mf2_rowid = rowids(store, "media_file", &["mf-2"])[0];
    let mf2_before = lease(store, |conn| {
        conn.query_row(
            "SELECT missing, updated_at FROM media_file WHERE id = 'mf-2'",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .unwrap_or_else(|e| panic!("mf-2 before pass 2: {e}"))
    });
    assert_eq!(mf2_before.0, 0, "mf-2 is live before the missing pass");
    assert_eq!(mf2_before.1, time::to_text(&seed_stamp()));

    // (iii) Touch's target stamp, pinned before pass 2 (Touch writes `imported_at`,
    // album_repository.go:365).
    let al1_imported = cell(store, "album", "imported_at", "al-1").expect("al-1 imported_at");

    // (iv) Two spent-backoff queue rows: al-2 is re-enqueued by the EnqueueIfMissing arm
    // (full_scan_mode = true) and must keep its lifecycle; al-1 is re-enqueued by the Enqueue
    // arm (full_scan_mode = false) and must have it reset.
    dirty_queue_row(store, nd_id::ARTWORK_KIND_ALBUM_PREFIX, "al-2");
    dirty_queue_row(store, nd_id::ARTWORK_KIND_ALBUM_PREFIX, "al-1");
    let dirty_al2 = queue_row(store, nd_id::ARTWORK_KIND_ALBUM_PREFIX, "al-2");
    let dirty_al1 = queue_row(store, nd_id::ARTWORK_KIND_ALBUM_PREFIX, "al-1");

    // (v) Tag dictionary/library counts before an identical-content re-add.
    let tag_rows = scalar::<i64>(store, "SELECT count(*) FROM tag");
    let library_tag_rows = scalar::<i64>(store, "SELECT count(*) FROM library_tag");

    // (vi) A CONFLICTING annotation for the same user on the transition DESTINATION. Pass 2
    // reassigns al-old -> al-1 again; `update or ignore` must KEEP the existing target row and
    // leave the source row where it is.
    seed_annotation(store, "u-fan", "al-old", "album", 7, 3);
    fixture(
        store,
        "UPDATE annotation SET play_count = 111, rating = 1 WHERE user_id = 'u-fan' AND item_id = 'al-1'",
        &[],
    );
    let al1_annotation = lease(store, |conn| {
        conn.query_row(
            "SELECT play_count, rating FROM annotation WHERE user_id = 'u-fan' AND item_id = 'al-1'",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .unwrap_or_else(|e| panic!("al-1 annotation: {e}"))
    });

    // (vii) An item_artwork row for a track pass 2 re-imports, so the invalidation arm has a
    // REAL row to delete.
    seed_item_artwork(store, nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, "mf-1");

    DirtyFacts {
        reimport_id,
        mf2_rowid,
        mf2_before,
        al1_imported,
        dirty_al2,
        dirty_al1,
        tag_rows,
        library_tag_rows,
        al1_annotation,
    }
}

/// The pass-1 facts the dirty-state corpus establishes for pass 2 to act on.
struct DirtyFacts {
    reimport_id: String,
    mf2_rowid: i64,
    mf2_before: (i64, String),
    al1_imported: String,
    dirty_al2: (i64, i64, String, String, String),
    dirty_al1: (i64, i64, String, String, String),
    tag_rows: i64,
    library_tag_rows: i64,
    al1_annotation: (i64, i64),
}

/// Push a queue row into a spent-backoff state: high priority, exhausted attempts, a stale
/// retry gate, an old enqueued_at and a non-empty trace.
fn dirty_queue_row(store: &SQLStore, kind: &str, id: &str) {
    fixture(
        store,
        "UPDATE artwork_queue SET priority = 90, attempts = 4, retry_at = ?, enqueued_at = ?, \
         trace = ? WHERE item_kind = ? AND item_id = ? AND image_type = 'primary'",
        &[
            text(time::to_text(&at(2001, 1, 1, 0, 0, 0))),
            text(time::to_text(&at(2001, 1, 1, 0, 0, 0))),
            text(r#"["spent-backoff"]"#),
            text(kind),
            text(id),
        ],
    );
}

/// One queue row's lifecycle tuple (priority, attempts, retry_at, enqueued_at, trace).
fn queue_row(store: &SQLStore, kind: &str, id: &str) -> (i64, i64, String, String, String) {
    lease(store, |conn| {
        conn.query_row(
            "SELECT priority, attempts, retry_at, enqueued_at, trace FROM artwork_queue \
             WHERE item_kind = ? AND item_id = ? AND image_type = 'primary'",
            [text(kind), text(id)],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .unwrap_or_else(|e| panic!("queue row {kind}/{id}: {e}"))
    })
}

/// The corpus with `mf-2` carried as a missing track: Go's `entry.missingTracks` drives BOTH
/// MarkMissing (`:446`) and Touch (`:450-456`), so a realistic re-scan hands the same id to
/// both arms, and `album_id` is what the Touch leg derives (`:450-453`).
fn corpus_with_missing(minute: u32) -> Vec<FolderWriteBatch> {
    let mut batches = corpus(minute);
    batches[0].missing.push(MissingMedia {
        id: "mf-2".to_string(),
        album_id: "al-1".to_string(),
    });
    batches
}

/// Per-table row-count deltas between two [`state`] snapshots, zero deltas dropped.
fn state_delta(before: &[(String, i64)], after: &[(String, i64)]) -> Vec<(String, i64)> {
    after
        .iter()
        .map(|(table, count)| {
            let before_count = before
                .iter()
                .find(|(t, _)| t == table)
                .map(|(_, c)| *c)
                .unwrap_or(0);
            (table.clone(), count - before_count)
        })
        .filter(|(_, d)| *d != 0)
        .collect()
}

#[test]
fn production_db_phase1_contract_first_and_second_pass() {
    let (db, path) = fresh_store("twopass");
    seed_libraries(&db);
    seed_transition_sources(&db);

    // ---------------- FIRST PASS ----------------
    let first = run_ok(&db, &batch_one(10));
    assert!(
        first.folder_inserted,
        "the folder Put takes the INSERT leg first"
    );
    assert_eq!(first.album_ids, vec!["al-1".to_string()]);
    assert_eq!(
        first.artist_ids,
        vec![
            "ar-main".to_string(),
            "ar-second".to_string(),
            "ar-unknown".to_string(),
            "ar-va".to_string()
        ]
    );
    assert_eq!(
        first.media_ids,
        vec!["mf-1".to_string(), "mf-2".to_string()]
    );
    assert_eq!(first.inserted_media_ids, first.media_ids);
    assert_eq!(
        first.consumed_transitions,
        vec![AlbumTransition {
            new_id: "al-1".to_string(),
            prev_id: "al-old".to_string()
        }]
    );
    assert!(first.warnings.is_empty(), "clean run: {:?}", first.warnings);
    // Two of the four artists are excluded by Go's queue-name filters (:412/:423).
    assert_eq!(first.queued_item_count, 3, "ar-main, ar-second, al-1");
    assert!(
        !first.enqueued_if_missing,
        "full_scan_mode=false runs Enqueue"
    );
    assert_eq!(
        first.artwork_rows_deleted, 1,
        "only mf-1 had a seeded item_artwork row"
    );

    let second = run_ok(&db, &batch_two(11));
    assert_eq!(second.album_ids, vec!["al-2".to_string()]);
    assert_eq!(
        second.missing_rows_marked, 0,
        "mf-gone does not exist, so Go's zero-row \
        chunk early-returns Ok(0) (mediafile_repository.go:463-467)"
    );
    assert_eq!(second.touched_album_ids, vec!["al-2".to_string()]);
    assert!(
        second.enqueued_if_missing,
        "full_scan_mode=true runs EnqueueIfMissing"
    );
    assert!(
        second.warnings.is_empty(),
        "clean run: {:?}",
        second.warnings
    );
    let probe_id = second
        .media_ids
        .iter()
        .find(|id| id.as_str() != "mf-3")
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "the empty-id record must report a committed id: {:?}",
                second.media_ids
            )
        });
    assert!(
        !probe_id.is_empty(),
        "Go's random arm mints a non-empty id inside the tx"
    );
    assert_eq!(
        second.inserted_media_ids.len(),
        2,
        "both rows are inserts on the first pass"
    );

    let third = run_ok(&db, &batch_three(12));
    assert_eq!(third.folder_id, "fo-9");
    assert_eq!(third.media_ids, vec!["mf-9".to_string()]);
    assert_eq!(third.artwork_rows_deleted, 1);

    let fourth = run_ok(&db, &batch_four(13));
    assert!(
        fourth.warnings.is_empty(),
        "a missing copy source is SILENTLY ignored (:497-500), not a warning: {:?}",
        fourth.warnings
    );
    assert_eq!(
        fourth.consumed_transitions,
        vec![AlbumTransition {
            new_id: "al-3".to_string(),
            prev_id: "al-missing".to_string()
        }]
    );

    // ---- read back FIRST-PASS facts the second pass must preserve exactly ----
    let state_after_first: Vec<(String, i64)> = state(&db);
    let al1_rowid = rowids(&db, "album", &["al-1"])[0];
    let mf1_rowid = rowids(&db, "media_file", &["mf-1"])[0];
    let al1_rating = real_cell(&db, "album", "average_rating", "al-1");
    let al2_rating = real_cell(&db, "album", "average_rating", "al-2");
    let fo1_updated = cell(&db, "folder", "updated_at", "fo-1").expect("fo-1 updated_at");
    let al1_created = cell(&db, "album", "created_at", "al-1").expect("al-1 created_at");
    let al3_created = cell(&db, "album", "created_at", "al-3").expect("al-3 created_at");
    let al9_created = cell(&db, "album", "created_at", "al-9").expect("al-9 created_at");
    let al_old_created = cell(&db, "album", "created_at", "al-old").expect("al-old created_at");
    let al_old2_created = cell(&db, "album", "created_at", "al-old2").expect("al-old2 created_at");
    let tag_set = set_of(&db, "SELECT tag_name || '=' || tag_value FROM tag");
    let queue_set = set_of(&db, "SELECT item_kind || '/' || item_id FROM artwork_queue");
    let album_artist_set = set_of(
        &db,
        "SELECT album_id || '#' || artist_id FROM album_artists",
    );
    let media_tag_set = set_of(
        &db,
        "SELECT media_file_id || '#' || tag_id FROM media_file_tags",
    );
    let library_artist_set = set_of(
        &db,
        "SELECT library_id || '#' || artist_id FROM library_artist",
    );
    let annotation_set = set_of(
        &db,
        "SELECT user_id || '#' || item_id || '#' || item_type FROM annotation",
    );

    // `ReassignAnnotation` (`sql_annotations.go:185-197`, rendered at
    // `sql_annotations.rs:391-396` as `update or ignore … set item_id = ? where item_type = ?
    // and item_id = ?`) MOVES the rows: the source ids hold nothing afterwards. The decoy is
    // untouched, and `al-missing` has no rows to move, so batch 4's transition consumes
    // without creating an annotation row.
    assert_eq!(
        annotation_set,
        vec![
            "u-fan#al-1#album".to_string(),
            "u-fan#al-2#album".to_string(),
            "u-fan#al-decoy#album".to_string(),
        ],
        "the move re-keys al-old/al-old2 onto al-1/al-2 and leaves no source rows"
    );
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM annotation WHERE item_id IN ('al-old','al-old2')"
        ),
        0,
        "non-vacuity: a renderer that appended instead of moving would leave these rows"
    );
    assert_eq!(
        al1_rating, 8.0,
        "avg-rating recalc on the target (sql_annotations.go:195-196)"
    );
    assert_eq!(al2_rating, 5.0, "same, second transition");
    // CopyAttributes (`album_repository.go:331-357`, ported as
    // `copy_album_created_at_in_tx`): the destination's `created_at` becomes the SOURCE's
    // distinct value. Every album in this corpus is seeded with a DISTINCT
    // non-year-one `created_at` (`created_of`), so deleting the CopyAttributes arm would
    // leave al-1 on created_of("al-1") and this assertion would fail.
    assert_eq!(
        al1_created,
        time::to_text(&created_of("al-old")),
        "al-1 inherited al-old's distinct stamp (CopyAttributes)"
    );
    assert_ne!(
        al1_created,
        time::to_text(&created_of("al-1")),
        "non-vacuity: al-1 must NOT still hold its own seeded stamp"
    );
    assert_eq!(
        cell(&db, "album", "created_at", "al-2"),
        Some(time::to_text(&created_of("al-old2"))),
        "al-2 inherited al-old2's distinct stamp"
    );
    assert_ne!(
        cell(&db, "album", "created_at", "al-2"),
        Some(time::to_text(&created_of("al-2")))
    );
    // Source rows keep their own stamps: CopyAttributes never writes the source.
    assert_eq!(al_old_created, time::to_text(&created_of("al-old")));
    assert_eq!(al_old2_created, time::to_text(&created_of("al-old2")));
    // A transition whose source row does not exist is SourceNotFound: the target keeps its
    // OWN distinct stamp (`:497-500` silently ignores it, no warning).
    assert_eq!(
        al3_created,
        time::to_text(&created_of("al-3")),
        "a missing copy source copies nothing: al-3 keeps its own distinct stamp"
    );
    assert_eq!(al9_created, time::to_text(&created_of("al-9")));
    assert!(
        !queue_set.contains(&"mf/mf-decoy".to_string()),
        "queue items come from the batch's \
        artists/albums only, never from pre-seeded artwork rows"
    );
    assert_eq!(
        tag_set,
        vec![
            "albumversion=Deluxe".to_string(),
            "genre=Jazz".to_string(),
            "genre=Rock".to_string(),
        ],
        "tag dictionary rows are exactly the three values the four batches added"
    );
    assert_eq!(
        library_artist_set,
        vec![
            "1#ar-main".to_string(),
            "1#ar-second".to_string(),
            "1#ar-unknown".to_string(),
            "1#ar-va".to_string(),
            "2#ar-nine".to_string(),
        ],
        "DB-05 AddArtist linked every batch artist to ITS OWN library"
    );
    assert!(!library_artist_set.is_empty());
    assert!(!album_artist_set.is_empty());
    assert!(!media_tag_set.is_empty());
    // ---------------- DIRTY STATE, committed against pass 1 ----------------
    let dirty = seed_dirty_state(&db);
    assert_eq!(
        dirty.reimport_id, "mf-1",
        "arm (i): the Match arm resolved the empty-id record to the COMMITTED id of the row \
         holding that path+library, and inserted nothing"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file"),
        6,
        "arm (i) non-vacuity: the empty-id re-import added no row; a probe that minted an id \
         would make this 7"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT missing FROM media_file WHERE id = 'mf-1'"),
        0,
        "the re-imported row is still a live row"
    );
    // Baseline the dirty state itself: pass 2 must not change any row COUNT, only the
    // flags/stamps its arms own.
    let counts_before_second = state(&db);
    let annotation_set_before_second = set_of(
        &db,
        "SELECT user_id || '#' || item_id || '#' || item_type FROM annotation",
    );
    assert!(
        annotation_set_before_second
            .iter()
            .any(|key| key == "u-fan#al-old#album"),
        "arm (vi): the conflicting transition left a live SOURCE row, which is exactly the \
         dirty state the OR IGNORE arm has to face"
    );
    assert_eq!(
        dirty.dirty_al2,
        (
            90,
            4,
            time::to_text(&at(2001, 1, 1, 0, 0, 0)),
            time::to_text(&at(2001, 1, 1, 0, 0, 0)),
            r#"["spent-backoff"]"#.to_string()
        ),
        "arm (iv): the queue row really is in a spent-backoff state"
    );

    // ---------------- SECOND PASS (the Go re-scan case) ----------------
    // The realistic re-scan shape: the disappeared track comes back as `entry.missingTracks`,
    // which drives BOTH MarkMissing (:446) and Touch (:450-456).
    let again = corpus_with_missing(14)
        .iter()
        .map(|batch| run_ok(&db, batch))
        .collect::<Vec<CommittedFolderOutcome>>();
    let pass2_1 = &again[0];
    let pass2_2 = &again[1];
    let pass2_3 = &again[2];
    let pass2_4 = &again[3];

    // ---- arm (ii) MarkMissing: the STORED row flag and stamp, not a reported count ----
    assert_ne!(
        dirty.mf2_before,
        (1, time::to_text(&stamps(14).mark_missing)),
        "the flag and stamp really changed from the pinned pre-pass values"
    );
    assert_eq!(pass2_1.missing_rows_marked, 1, "one real row was marked");
    assert_eq!(
        scalar::<i64>(&db, "SELECT missing FROM media_file WHERE id = 'mf-2'"),
        1,
        "MarkMissing (:446) wrote missing = 1 on the existing row"
    );
    assert_eq!(
        cell(&db, "media_file", "updated_at", "mf-2"),
        Some(time::to_text(&stamps(14).mark_missing)),
        "MarkMissing stamps updated_at with the caller's mark_missing stamp"
    );
    assert_eq!(
        rowids(&db, "media_file", &["mf-2"])[0],
        dirty.mf2_rowid,
        "the flag is written on the SAME row — no delete/re-insert"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file WHERE id = 'mf-2'"),
        1,
        "non-vacuity: MarkMissing updated the existing row rather than adding another"
    );

    // ---- arm (iii) Touch: the STORED imported_at, not just the reported id set ----
    assert_eq!(pass2_1.touched_album_ids, vec!["al-1".to_string()]);
    assert_eq!(
        cell(&db, "album", "imported_at", "al-1"),
        Some(time::to_text(&stamps(14).album_touch)),
        "Touch (:456 / album_repository.go:365) advanced al-1.imported_at to the batch stamp"
    );
    assert_ne!(
        cell(&db, "album", "imported_at", "al-1"),
        Some(dirty.al1_imported.clone()),
        "non-vacuity: a Touch arm that only REPORTED ids would leave the dirty stamp in place"
    );

    // ---- arm (iv) enqueue arms: PreserveBackoff (DO NOTHING) vs Reset, on stored rows ----
    assert_eq!(
        queue_row(&db, nd_id::ARTWORK_KIND_ALBUM_PREFIX, "al-2"),
        dirty.dirty_al2,
        "EnqueueIfMissing (full_scan_mode = true, :467) skipped the queued row: priority, \
         attempts, retry_at, enqueued_at and trace all survive"
    );
    let reset_al1 = queue_row(&db, nd_id::ARTWORK_KIND_ALBUM_PREFIX, "al-1");
    assert_eq!(
        reset_al1.0, 90,
        "Reset keeps MAX(priority, excluded.priority)"
    );
    assert_eq!(reset_al1.1, 0, "Reset resets attempts (:73)");
    assert_eq!(
        reset_al1.2,
        time::to_text(&stamps(14).artwork),
        "Reset rewrites retry_at"
    );
    assert_eq!(
        reset_al1.3,
        time::to_text(&stamps(14).artwork),
        "Reset rewrites enqueued_at"
    );
    assert_eq!(reset_al1.4, "[]", "Reset clears trace literally (:73)");
    assert_ne!(
        reset_al1, dirty.dirty_al1,
        "the Reset arm really changed the row's lifecycle"
    );

    // ---- arm (v) identical tag re-add: no duplicate dictionary or link rows ----
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM tag"),
        dirty.tag_rows,
        "tag dictionary \
         rows are unchanged by a re-add of identical content (`on conflict (id) do nothing`)"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM library_tag"),
        dirty.library_tag_rows,
        "library_tag rows are unchanged (`ON CONFLICT (tag_id, library_id)`, tag_repo.rs:246)"
    );

    // ---- arm (vi) conflicting destination annotation: OR IGNORE keeps the target ----
    assert_eq!(
        lease(&db, |conn| {
            conn.query_row(
                "SELECT play_count, rating FROM annotation WHERE user_id = 'u-fan' AND item_id = 'al-1'",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap_or_else(|e| panic!("al-1 annotation: {e}"))
        }),
        dirty.al1_annotation,
        "the pre-existing target row was KEPT (update or ignore), so its values are untouched"
    );
    assert_eq!(
        lease(&db, |conn| {
            conn.query_row(
                "SELECT play_count FROM annotation WHERE user_id = 'u-fan' AND item_id = 'al-old'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or_else(|e| panic!("the ignored source row must still exist: {e}"))
        }),
        7,
        "the source row STAYED put: the move was ignored, not applied and not replaced"
    );

    // ---- arm (vii) artwork invalidation: the seeded row is really gone ----
    assert_eq!(
        pass2_1.artwork_rows_deleted, 1,
        "the seeded mf-1 item_artwork row was deleted"
    );
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM item_artwork WHERE item_kind = 'mf' AND item_id = 'mf-1'"
        ),
        0,
        "the row is gone from the table, not merely reported as deleted"
    );
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM item_artwork WHERE item_id = 'mf-decoy'"
        ),
        1,
        "the same-kind decoy survives: the delete is scoped to the batch's ids"
    );
    // Queue state stays consistent with what the writes produced.
    assert_eq!(
        set_of(&db, "SELECT item_kind || '/' || item_id FROM artwork_queue"),
        queue_set,
        "queue membership is unchanged by the re-scan"
    );

    // (1) committed IDs are REUSED. Non-vacuity: this fails if the first pass's committed
    // ids were not reused, because a fresh insert would report a NEW id and `inserted`.
    assert_eq!(
        pass2_1.media_ids,
        vec!["mf-1".to_string(), "mf-2".to_string()]
    );
    assert!(
        pass2_1.inserted_media_ids.is_empty(),
        "a re-scan must insert nothing: {:?}",
        pass2_1.inserted_media_ids
    );
    assert!(
        !pass2_1.folder_inserted,
        "the folder Put now takes the UPDATE leg"
    );
    assert_eq!(
        pass2_2.media_ids,
        vec!["mf-3".to_string(), probe_id.clone()],
        "the empty-id record lands on the id the probe found, not on a fresh row"
    );
    assert!(pass2_2.inserted_media_ids.is_empty());
    assert_eq!(pass2_3.media_ids, vec!["mf-9".to_string()]);
    assert!(pass2_4.inserted_media_ids.is_empty());

    // (2) transitions re-consumed correctly: the album-ID map is a fresh per-attempt clone
    // (Go's `maps.Clone`, :378), so a repeat run consumes again, and the second move is a
    // no-op on rows that already carry the new id.
    assert_eq!(
        pass2_1.consumed_transitions,
        vec![AlbumTransition {
            new_id: "al-1".to_string(),
            prev_id: "al-old".to_string()
        }]
    );
    assert_eq!(
        pass2_2.consumed_transitions,
        vec![AlbumTransition {
            new_id: "al-2".to_string(),
            prev_id: "al-old2".to_string()
        }]
    );
    assert_eq!(
        set_of(
            &db,
            "SELECT user_id || '#' || item_id || '#' || item_type FROM annotation"
        ),
        annotation_set_before_second,
        "a repeated pass duplicates, relocates or drops no annotation row — including the \
         source row the OR IGNORE arm left behind"
    );
    // The recalculation leg runs even though the MOVE leg was ignored
    // (`sql_annotations.go:195-196`): al-1's cached average is recomputed from the rows it
    // actually holds now — its own kept row (rating 1), the al-old conflict row having stayed
    // behind. So the value moves from pass 1's 8.0 to 1.0, which is only reachable if BOTH
    // legs ran: a dropped recalc would leave 8.0, and an `or replace`/merge shortcut would
    // overwrite al-1's own row and yield a different average.
    assert_eq!(
        real_cell(&db, "album", "average_rating", "al-1"),
        1.0,
        "recalc ran over the KEPT target row (rating 1), not pass 1's 8.0"
    );
    assert_ne!(
        real_cell(&db, "album", "average_rating", "al-1"),
        al1_rating,
        "non-vacuity: pass 1's cached value is no longer stored"
    );
    assert_eq!(
        real_cell(&db, "album", "average_rating", "al-2"),
        al2_rating,
        "al-2's recalc is over the same single kept row, so its cached value is unchanged"
    );
    assert_eq!(
        real_cell(&db, "album", "average_rating", "al-old"),
        0.0,
        "the SOURCE's cached average is never recomputed: Go recalculates only the target \
         (`sql_annotations.go:195-196`), so al-old still carries its seeded 0.0 even though it \
         now holds a rating-3 annotation row. A shortcut that recalculated both sides would \
         read 3.0 here."
    );

    // (3) no duplicate queue/tag/junction rows, and the same member SET.
    assert_eq!(
        set_of(&db, "SELECT item_kind || '/' || item_id FROM artwork_queue"),
        queue_set,
        "Reset resets the queue row's LIFECYCLE, not its membership"
    );
    assert_eq!(
        set_of(&db, "SELECT tag_name || '=' || tag_value FROM tag"),
        tag_set
    );
    assert_eq!(
        set_of(
            &db,
            "SELECT album_id || '#' || artist_id FROM album_artists"
        ),
        album_artist_set
    );
    assert_eq!(
        set_of(
            &db,
            "SELECT media_file_id || '#' || tag_id FROM media_file_tags"
        ),
        media_tag_set
    );
    assert_eq!(
        set_of(
            &db,
            "SELECT library_id || '#' || artist_id FROM library_artist"
        ),
        library_artist_set
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file"),
        6,
        "2 (fo-1) + 2 (fo-2, incl. the probe-minted row) + 1 (fo-9) + 1 (fo-3); a re-scan \
         never adds a seventh"
    );

    // (4) FTS counts stable, pinned to the SAME rowids (a ghost entry would be a MATCH hit
    // whose rowid no longer joins a base row).
    assert_eq!(rowids(&db, "album", &["al-1"])[0], al1_rowid);
    assert_eq!(rowids(&db, "media_file", &["mf-1"])[0], mf1_rowid);
    assert_eq!(fts_match(&db, "media_file_fts", "Zbrfldrmf1"), 1);
    assert_eq!(fts_match(&db, "media_file_fts", "Zbrfldrmf2"), 1);
    assert_eq!(fts_match(&db, "media_file_fts", "Zbrfldrmf3"), 1);
    assert_eq!(fts_match(&db, "media_file_fts", "Zbrfldrmf9"), 1);
    assert_eq!(
        fts_ids(&db, "album_fts", "album", "Albumidal1"),
        vec!["al-1".to_string()]
    );
    assert_eq!(
        fts_ids(&db, "artist_fts", "artist", "\"Main Act\""),
        vec!["ar-main".to_string()]
    );

    // (5) stamps advanced ONLY where Go advances them.
    assert_ne!(
        cell(&db, "folder", "updated_at", "fo-1"),
        Some(fo1_updated),
        "Folder Put stamps updated_at (:390 via folder_repository.go:255-259)"
    );
    assert_eq!(
        cell(&db, "media_file", "updated_at", "mf-1").as_deref(),
        Some(time::to_text(&seed_stamp()).as_str()),
        "media_file.updated_at is never touched by Put (mediafile_repository.go:184-190)"
    );
    assert_eq!(
        cell(&db, "album", "created_at", "al-1"),
        Some(al1_created.clone()),
        "created_at is dropped from the UPDATE set (sql_base_repository.go:598-603)"
    );
    assert_eq!(
        al1_created,
        time::to_text(&created_of("al-old")),
        "after BOTH passes al-1 still holds al-old's DISTINCT stamp: CopyAttributes is the \
         only writer of that value here, so the arm cannot be dropped or replaced by a \
         self-copy without failing this assertion"
    );
    assert_eq!(
        cell(&db, "album", "created_at", "al-2"),
        Some(time::to_text(&created_of("al-old2"))),
        "same for al-2, after both passes"
    );
    assert_eq!(
        cell(&db, "album", "created_at", "al-3"),
        Some(al3_created),
        "al-3's source row never exists, so its own distinct stamp survives both passes"
    );
    assert_eq!(
        cell(&db, "album", "created_at", "al-9"),
        Some(al9_created),
        "al-9 has no transition at all: untouched"
    );
    assert_eq!(
        cell(&db, "album", "created_at", "al-old"),
        Some(al_old_created),
        "CopyAttributes never writes the SOURCE's own created_at"
    );
    assert_eq!(
        cell(&db, "album", "created_at", "al-old2"),
        Some(al_old2_created)
    );
    assert_eq!(
        cell(&db, "album", "imported_at", "al-3"),
        Some(time::to_text(&stamps(14).album_put)),
        "imported_at IS the batch stamp (album_repository.go:210)"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file WHERE id = 'mf-gone'"),
        0,
        "a missing entry with no row marks nothing (Go's zero-chunk early return)"
    );

    // (6) Per-table row-count deltas between the dirty baseline and pass 2. A re-scan adds no
    // row anywhere; the ONLY legitimate removal is arm (vii)'s artwork invalidation of the
    // seeded mf-1 item_artwork row. Pinning the delta as an exact set is strictly stronger
    // than a tuple comparison: a duplicated tag/junction/queue row or a lost annotation would
    // show up as an extra entry here.
    let counts_after_second = state(&db);
    assert_eq!(
        state_delta(&counts_before_second, &counts_after_second),
        vec![("item_artwork".to_string(), -1)],
        "the only row-count change in pass 2 is the invalidation arm's deletion of the seeded \
         mf-1 artwork row: no other table grew or shrank"
    );
    assert_eq!(
        state_delta(&state_after_first, &counts_after_second),
        vec![("annotation".to_string(), 1)],
        "same delta measured from the PASS-1 snapshot: across the dirty corpus plus pass 2 the \
         only net change is arm (vi)'s re-added source annotation row. The artwork row arm (vii) \
         deleted was itself added by the dirty corpus, so it nets to zero; arm (i) added no media \
         row (id reused), and arms (iv)/(v) added no queue or tag row."
    );
    // The delta helper itself is not vacuous: a real add shows up against the same baseline.
    let state_after_second = state(&db);
    fixture(
        &db,
        "INSERT INTO annotation (user_id, item_id, item_type, play_count) VALUES \
         ('u-fan', 'al-delta', 'album', 1)",
        &[],
    );
    assert_eq!(
        state_delta(&state_after_second, &state(&db)),
        vec![("annotation".to_string(), 1)],
        "state_delta detects an added row"
    );
    fixture(
        &db,
        "DELETE FROM annotation WHERE item_id = 'al-delta'",
        &[],
    );
    assert_eq!(
        state_delta(&state_after_second, &state(&db)),
        Vec::<(String, i64)>::new(),
        "and returns to zero once it is removed"
    );

    // ---- (7) CONTROL PASS: an arm that did NOT run cannot advance the stored value ----
    // batch_three is the one corpus delta that touches NEITHER mf-2 nor al-1 (library B, folder
    // fo-9, album al-9, no `missing` entries). MarkMissing and Touch are driven ONLY by
    // `entry.missingTracks` (`:446` / `:450-456`), so running it must leave both pinned values
    // exactly where pass 2 left them — while its own album write DOES move al-9.imported_at.
    // That contrast is what proves the values read above came from the two arms and not from
    // some generic batch write.
    let control = run_ok(&db, &batch_three(15));
    assert_eq!(control.folder_id, "fo-9");
    assert!(control.missing_rows_marked == 0 && control.touched_album_ids.is_empty());
    assert_eq!(
        cell(&db, "album", "imported_at", "al-9"),
        Some(time::to_text(&stamps(15).album_put)),
        "the control batch DID write its own album's stamp, so it is not a no-op batch"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT missing FROM media_file WHERE id = 'mf-2'"),
        1,
        "mf-2 is still flagged: only the MarkMissing arm writes `missing`"
    );
    assert_eq!(
        cell(&db, "media_file", "updated_at", "mf-2"),
        Some(time::to_text(&stamps(14).mark_missing)),
        "and its updated_at is STILL pass 2's mark_missing stamp, not the control batch's stamp"
    );
    assert_eq!(
        cell(&db, "album", "imported_at", "al-1"),
        Some(time::to_text(&stamps(14).album_touch)),
        "al-1.imported_at is STILL pass 2's touch stamp: Touch only runs for the albums of \
         missing tracks, so the arm's own stamp is what the earlier assertion read"
    );

    // ---- Arm (v) detection control: identical content adds nothing, a NEW value does ----
    let blues_id = nd_id::new_tag_id("genre", "Blues");
    let mut probe_batch = batch_one(16);
    probe_batch.tags = vec![Tag {
        id: blues_id.clone(),
        tag_name: "genre".to_string(),
        tag_value: "Blues".to_string(),
        ..Tag::default()
    }];
    probe_batch.media = Vec::new();
    probe_batch.album_transitions = Vec::new();
    run_ok(&db, &probe_batch);
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM tag"),
        dirty.tag_rows + 1,
        "a genuinely NEW tag value does add a dictionary row, so the identical-content equality \
         above was about the conflict arm, not about tag writes being absent"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM library_tag"),
        dirty.library_tag_rows + 1,
        "and a library_tag row for the new (tag, library) pair"
    );
    fixture(
        &db,
        "DELETE FROM library_tag WHERE tag_id = ?",
        &[text(&blues_id)],
    );
    fixture(&db, "DELETE FROM tag WHERE id = ?", &[text(&blues_id)]);
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM tag"),
        dirty.tag_rows,
        "counts return to the pinned baseline once the control row is removed"
    );

    cleanup(db, path);
}

/// Assert the executor's Body-stage shape with the concrete driver error retained.
fn assert_body_failure(err: &FolderRunError) {
    match err {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(
                *stage,
                TransactionStage::Body,
                "the executor owns the stage"
            );
            assert!(!is_busy(source), "a trigger failure is not retried");
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }
}

/// The scan-lifecycle row, as one comparable tuple.
/// The full observable scan-lifecycle row of library 1: `full_scan_in_progress`,
/// `last_scan_at`, `last_scan_started_at`, then both scanned-PID overrides.
///
/// `last_scan_started_at` is IN the compared tuple rather than checked for
/// zero/non-zero: `scan_begin_sql` (`library_repo.rs:786-797`) is the only writer that sets it
/// to a real stamp and `scan_end_sql` (`:802-815`) resets it to the canonical year-one zero, so
/// any change to its stored value — including a nonzero-to-nonzero one — escapes detection only
/// if the column is left out of the tuple.
fn library_scan_row(store: &SQLStore) -> (i64, String, String, String, String) {
    lease(store, |conn| {
        conn.query_row(
            "SELECT full_scan_in_progress, last_scan_at, last_scan_started_at, \
             scanned_pid_album, scanned_pid_track FROM library WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .unwrap_or_else(|e| panic!("{e}"))
    })
}

/// `ScanInProgress`'s count read (`library_repository.go:190-194`), executed through the
/// production renderer so the year-one TEXT zero predicate is the DB-02 seam's own.
fn scan_in_progress_count(store: &SQLStore) -> i64 {
    let statement = nd_db::library_repo::scan_in_progress_count_sql();
    lease(store, |conn| {
        conn.query_row(
            &statement.0,
            rusqlite::params_from_iter(statement.1.iter()),
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or_else(|e| panic!("{e}"))
    })
}

#[test]
fn production_batch_failure_does_not_advance_library_completion() {
    let (db, path) = fresh_store("fatal-arm");
    seed_libraries(&db);

    // Go's scan lifecycle on the DB-05 seam, BEFORE any folder delta.
    let begin = at(2020, 1, 1, 0, 0, 0);
    db.try_with_tx_sync::<(), Infallible>(|tx| scan_begin_in_tx(tx, LIB_MAIN, &begin, true))
        .unwrap_or_else(|e| panic!("scan_begin_in_tx: {e}"));
    assert_eq!(
        scan_in_progress_count(&db),
        1,
        "the library really is scan-in-progress"
    );
    let baseline = state(&db);
    let lifecycle_before = library_scan_row(&db);
    assert_eq!(lifecycle_before.0, 1, "full_scan_in_progress is set");
    assert_eq!(
        lifecycle_before.2,
        time::to_text(&begin),
        "ScanBegin wrote a NONZERO last_scan_started_at, so the failure path has a real stamp \
         to preserve and the tuple comparison cannot pass on a zero-vs-zero coincidence"
    );

    // A FATAL arm: the Album Put is poisoned by a fixture trigger (subplan field 7 permits
    // controlled fault-injection setup). Album Put is fatal per the D-BATCH table (:481), so
    // the whole folder transaction rolls back.
    fixture(
        &db,
        "CREATE TRIGGER fixture_poison BEFORE INSERT ON album FOR EACH ROW \
         WHEN NEW.id = 'al-1' BEGIN SELECT RAISE(ABORT, 'fixture: album put'); END",
        &[],
    );
    let err =
        persist_folder_delta(&db, &batch_one(20)).expect_err("the poisoned Album Put must fail");
    assert_body_failure(&err);

    // The library scan lifecycle is UNADVANCED for that folder: all five columns compared,
    // including last_scan_started_at.
    assert_eq!(
        library_scan_row(&db),
        lifecycle_before,
        "a failed folder batch must not advance the scan lifecycle: full_scan_in_progress, \
         last_scan_at, last_scan_started_at and both scanned PID columns are unchanged"
    );
    assert_eq!(
        scan_in_progress_count(&db),
        1,
        "the scan still reads in-progress"
    );
    assert_eq!(
        state(&db),
        baseline,
        "nothing from the rolled-back delta survived"
    );
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM folder"), 0);

    // The pool is un-poisoned: a fresh lease and a fresh transaction both work.
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM album"), 0);
    db.try_with_tx_sync::<(), Infallible>(|tx| {
        set_scanned_pid_in_tx(tx, LIB_MAIN, "%{ album }%", "%{ track }%")
    })
    .unwrap_or_else(|e| panic!("set_scanned_pid_in_tx after a failure: {e}"));

    // The SAME batch commits cleanly once the trigger is gone ("nothing committed" cannot
    // mean "nothing ever ran").
    fixture(&db, "DROP TRIGGER fixture_poison", &[]);
    let outcome = run_ok(&db, &batch_one(21));
    assert_eq!(outcome.folder_id, "fo-1");
    assert_eq!(
        outcome.media_ids,
        vec!["mf-1".to_string(), "mf-2".to_string()]
    );
    assert_eq!(outcome.album_ids, vec!["al-1".to_string()]);

    // Completion advances, and `ScanInProgress` reads idle through the canonical zero.
    let finish = at(2020, 1, 2, 0, 0, 0);
    db.try_with_tx_sync::<(), Infallible>(|tx| scan_end_in_tx(tx, LIB_MAIN, &finish))
        .unwrap_or_else(|e| panic!("scan_end_in_tx: {e}"));
    assert_eq!(scan_in_progress_count(&db), 0, "a finished scan reads idle");
    let lifecycle_after = library_scan_row(&db);
    assert_eq!(
        lifecycle_after.0, 0,
        "full_scan_in_progress cleared by ScanEnd"
    );
    assert_eq!(
        lifecycle_after.1,
        time::to_text(&finish),
        "last_scan_at is ScanEnd's stamp"
    );
    assert_eq!(
        lifecycle_after.2,
        time::to_text(&time::zero()),
        "ScanEnd resets last_scan_started_at to the canonical year-one zero (:805-806), which is \
        what makes ScanInProgress read idle"
    );
    assert_ne!(
        lifecycle_after.2, lifecycle_before.2,
        "non-vacuity: the start stamp really moved off ScanBegin's value"
    );
    assert_eq!(lifecycle_after.3, "%{ album }%");
    assert_eq!(lifecycle_after.4, "%{ track }%");
    cleanup(db, path);

    // ---- warn-only outcomes do NOT block completion ----
    let (db2, path2) = fresh_store("warn-only");
    seed_libraries(&db2);
    // A resolvable item_artwork row is REQUIRED for the invalidation arm to run at all:
    // `delete_for_items_in_tx` on an empty chunk early-returns Ok(0), so without a row the
    // warn-only path would be vacuous.
    seed_item_artwork(&db2, nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, "mf-1");
    db2.try_with_tx_sync::<(), Infallible>(|tx| scan_begin_in_tx(tx, LIB_MAIN, &begin, true))
        .unwrap_or_else(|e| panic!("scan_begin_in_tx: {e}"));
    fixture(
        &db2,
        "CREATE TRIGGER fixture_queue_insert_fails BEFORE INSERT ON artwork_queue \
         FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'fixture: artwork enqueue fails'); END",
        &[],
    );
    fixture(
        &db2,
        "CREATE TRIGGER fixture_artwork_delete_fails BEFORE DELETE ON item_artwork \
         FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'fixture: artwork invalidate fails'); END",
        &[],
    );
    let warned = run_ok(&db2, &batch_one(30));
    assert!(
        !warned.warnings.is_empty(),
        "both warn-only arms must degrade"
    );
    assert_eq!(
        warned.artwork_rows_deleted, 0,
        "the invalidation warning cost the delete"
    );
    let stages: Vec<&'static str> = warned.warnings.iter().map(|w| w.stage_str()).collect();
    assert_eq!(
        stages,
        vec!["artwork_invalidate", "enqueue"],
        "Go's execution order (:439, :470)"
    );
    assert!(!warned.warnings.iter().any(|w| w.stage.is_fatal()));
    // The transaction COMMITTED, so the folder and its tracks exist and the scan may advance.
    assert_eq!(scalar::<i64>(&db2, "SELECT count(*) FROM folder"), 1);
    assert_eq!(scalar::<i64>(&db2, "SELECT count(*) FROM media_file"), 2);
    assert_eq!(
        scalar::<i64>(&db2, "SELECT count(*) FROM artwork_queue"),
        0,
        "the enqueue warning really cost the queue rows (not a silent success)"
    );
    db2.try_with_tx_sync::<(), Infallible>(|tx| scan_end_in_tx(tx, LIB_MAIN, &finish))
        .unwrap_or_else(|e| {
            panic!("a warnings-carrying CommittedFolderOutcome must still count as advanced: {e}")
        });
    assert_eq!(
        scan_in_progress_count(&db2),
        0,
        "warnings do not block completion"
    );

    // The same batch succeeds fully once the triggers are dropped: warn-only never means
    // permanently degraded.
    fixture(&db2, "DROP TRIGGER fixture_queue_insert_fails", &[]);
    fixture(&db2, "DROP TRIGGER fixture_artwork_delete_fails", &[]);
    let clean = run_ok(&db2, &batch_one(31));
    assert!(clean.warnings.is_empty(), "clean run: {:?}", clean.warnings);
    assert_eq!(scalar::<i64>(&db2, "SELECT count(*) FROM artwork_queue"), 3);
    cleanup(db2, path2);
}

// ---------------------------------------------------------------------------
// Test 3: the DB-11 read surface over the phase-1 corpus, through the real pool.
// ---------------------------------------------------------------------------

/// Execute a folder-scoped read over the **`f` alias** (`ScopeAlias::FolderAlias`) with a
/// resolved scope clause injected into it, and return the folder ids it yields.
///
/// The statement shape is the landed one: `get_album_ids_by_folder_sql`
/// (`mediafile_repo.rs:2110-2120`) nests `SELECT f.id FROM folder f`, so `f` is the alias the
/// production album-ids read actually uses. `clause` is the resolved clause TEXT
/// (`UserScope::rendered_clause`, `#[doc(hidden)]` — a DB-owned contract seam, not public API);
/// `None` is the admin/headless/AllLibraries arm, which omits the predicate entirely.
fn folder_ids_over_alias(
    store: &SQLStore,
    clause: Option<&(String, String, Vec<Value>)>,
    extra_args: &[SqlValue],
) -> Vec<String> {
    lease(store, |conn| {
        let mut sql = String::from("SELECT f.id FROM folder f WHERE f.missing = ?");
        let mut args: Vec<SqlValue> = vec![SqlValue::Integer(0)];
        args.extend(extra_args.iter().cloned());
        if let Some((_, clause_sql, clause_args)) = clause {
            sql.push_str(" AND ");
            sql.push_str(clause_sql);
            for value in clause_args {
                args.push(match value {
                    Value::Text(v) => SqlValue::Text(v.clone()),
                    Value::Integer(v) => SqlValue::Integer(*v),
                    Value::Bool(v) => SqlValue::Integer(i64::from(*v)),
                    Value::Real(v) => SqlValue::Real(*v),
                    Value::Null => SqlValue::Null,
                });
            }
        }
        sql.push_str(" ORDER BY f.id");
        let mut stmt = conn.prepare(&sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        stmt.query_map(rusqlite::params_from_iter(args.iter()), |row| {
            row.get::<_, String>(0)
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect::<Vec<String>>()
    })
}

/// The same clause executed in the production ALBUM-IDS shape: the folder-alias subquery of
/// `GetAlbumIDsByFolder`, i.e. `… AND <clause>` inside `SELECT f.id FROM folder f`.
fn album_ids_over_folder_alias(
    store: &SQLStore,
    clause: Option<&(String, String, Vec<Value>)>,
) -> Vec<String> {
    lease(store, |conn| {
        let mut sql = String::from(
            "SELECT distinct m.album_id FROM media_file m WHERE m.missing = ? AND m.folder_id \
             IN (SELECT f.id FROM folder f WHERE f.missing = ?",
        );
        let mut args: Vec<SqlValue> = vec![SqlValue::Integer(0), SqlValue::Integer(0)];
        if let Some((_, clause_sql, clause_args)) = clause {
            sql.push_str(" AND ");
            sql.push_str(clause_sql);
            sql.push(')');
            for value in clause_args {
                args.push(match value {
                    Value::Text(v) => SqlValue::Text(v.clone()),
                    Value::Integer(v) => SqlValue::Integer(*v),
                    Value::Bool(v) => SqlValue::Integer(i64::from(*v)),
                    Value::Real(v) => SqlValue::Real(*v),
                    Value::Null => SqlValue::Null,
                });
            }
        } else {
            sql.push(')');
        }
        sql.push_str(" ORDER BY m.album_id");
        let mut stmt = conn.prepare(&sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        stmt.query_map(rusqlite::params_from_iter(args.iter()), |row| {
            row.get::<_, String>(0)
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect::<Vec<String>>()
    })
}

/// The corpus folders, per library.
const MAIN_FOLDERS: [&str; 3] = ["fo-1", "fo-2", "fo-3"];

#[test]
fn production_read_scopes_and_queue_fts_match() {
    let (db, path) = fresh_store("read-scopes");
    seed_libraries(&db);
    run_corpus(&db, 40);
    let probe_id = lease(&db, |conn| {
        conn.query_row(
            "SELECT id FROM media_file WHERE path = 'album-two/track-03.mp3'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|e| panic!("probe row: {e}"))
    });
    assert!(!probe_id.is_empty());
    let cancel = nd_utils::context::Cancellation::new();

    // ---- ScannerScope (system authority) drives the chunked id readers ----
    let albums_for = |library_id: i64, folders: &[&str]| -> Vec<String> {
        lease(&db, |conn| {
            let scope = ScannerScope::for_library(library_id);
            assert_eq!(scope.library_id(), library_id);
            let folder_ids: Vec<String> = folders.iter().map(|f| (*f).to_string()).collect();
            let mut cursor = album_ids_by_folders(conn, &scope, &folder_ids);
            let mut out: Vec<String> = Vec::new();
            while !cursor.is_complete() {
                let outcome = cursor
                    .visit_chunk(&cancel, |id| {
                        out.push(id);
                        Control::Continue
                    })
                    .unwrap_or_else(|e| panic!("album_ids_by_folders chunk: {e:?}"));
                assert!(
                    matches!(outcome, VisitOutcome::Completed { .. }),
                    "{outcome:?}"
                );
            }
            assert!(cursor.is_complete());
            assert_eq!(cursor.remaining_chunks(), 0);
            out.sort();
            out
        })
    };
    assert_eq!(
        albums_for(LIB_MAIN, &MAIN_FOLDERS),
        vec!["al-1".to_string(), "al-2".to_string(), "al-3".to_string()],
        "the system scope over LIB_MAIN's folders returns exactly its three albums"
    );
    assert_eq!(
        albums_for(LIB_MAIN, &["fo-9"]),
        Vec::<String>::new(),
        "the scope excludes another library's folder: no cross-library leakage"
    );
    assert_eq!(albums_for(LIB_OTHER, &["fo-9"]), vec!["al-9".to_string()]);

    // ---- UserScope arms over the same folder reads ----
    seed_user(&db, "u-zero", false);
    seed_user(&db, "u-all", false);
    seed_user(&db, "u-admin", true);
    grant(&db, "u-all", LIB_MAIN);
    grant(&db, "u-all", LIB_OTHER);

    let visible_count = |user_id: &str| -> usize {
        lease(&db, |conn| {
            let scope = resolve_user(conn, user_id.to_string(), ScopeAlias::MediaFile)
                .unwrap_or_else(|e| panic!("resolve_user({user_id}): {e}"));
            let mut n = 0usize;
            for folder_id in ["fo-1", "fo-2", "fo-3", "fo-9"] {
                let outcome = visit_media_in_folder(conn, folder_id, Some(&scope), &cancel, |_| {
                    n += 1;
                    Control::Continue
                })
                .unwrap_or_else(|e| panic!("scoped folder read {folder_id}: {e:?}"));
                assert!(
                    matches!(outcome, VisitOutcome::Completed { .. }),
                    "{outcome:?}"
                );
            }
            n
        })
    };

    // NEGATIVE CONTROL: a zero-grant user over a NON-EMPTY library sees nothing.
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM media_file"), 6);
    assert_eq!(
        visible_count("u-zero"),
        0,
        "zero grants ⇒ Denied arm ⇒ nothing"
    );
    assert_eq!(
        visible_count("u-all"),
        6,
        "all grants ⇒ AllLibraries arm ⇒ whole corpus"
    );
    assert_eq!(visible_count("u-admin"), 6, "admin ⇒ clause omitted");
    assert_eq!(visible_count(nd_db::scanner_queries::HEADLESS_USER_ID), 6);

    let zero = lease(&db, |conn| {
        resolve_user(conn, "u-zero".to_string(), ScopeAlias::MediaFile).unwrap()
    });
    assert_eq!(zero.decision(), ScopeDecision::Denied);
    assert!(
        zero.rendered_clause().is_some(),
        "the Denied arm carries the clause"
    );
    assert_eq!(zero.alias(), ScopeAlias::MediaFile);
    assert_eq!(zero.alias().table(), "media_file");
    assert_eq!(zero.user_id(), "u-zero");
    // Revoking one grant moves u-all from AllLibraries to Denied: the arms are resolution
    // outcomes, not caller-settable flags.
    revoke(&db, "u-all", LIB_OTHER);
    assert_eq!(
        visible_count("u-all"),
        5,
        "one revoked grant tightens the read immediately"
    );
    let demoted = lease(&db, |conn| {
        resolve_user(conn, "u-all".to_string(), ScopeAlias::MediaFile).unwrap()
    });
    assert_eq!(demoted.decision(), ScopeDecision::Denied);
    assert!(demoted.rendered_clause().is_some());
    grant(&db, "u-all", LIB_OTHER);
    assert_eq!(visible_count("u-all"), 6);
    let all = lease(&db, |conn| {
        resolve_user(conn, "u-all".to_string(), ScopeAlias::MediaFile).unwrap()
    });
    assert_eq!(all.decision(), ScopeDecision::AllLibraries);
    assert!(
        all.rendered_clause().is_none(),
        "AllLibraries omits the clause"
    );
    let admin = lease(&db, |conn| {
        resolve_user(conn, "u-admin".to_string(), ScopeAlias::MediaFile).unwrap()
    });
    assert_eq!(admin.decision(), ScopeDecision::Admin);
    assert!(admin.rendered_clause().is_none());
    let headless = lease(&db, |conn| {
        resolve_user(
            conn,
            nd_db::scanner_queries::HEADLESS_USER_ID.to_string(),
            ScopeAlias::MediaFile,
        )
        .unwrap()
    });
    assert_eq!(headless.decision(), ScopeDecision::Headless);
    assert!(headless.rendered_clause().is_none());
    assert_eq!(ScopeAlias::FolderAlias.table(), "f");

    // ---- The `f` alias EXECUTED, not type-checked (the DB-11 handoff's API-visible pin) ----
    // `get_visible_media` resolves `ScopeAlias::MediaFile` internally
    // (`scanner_queries.rs:1177`), so no public reader can hand us the folder alias. The alias
    // IS live in the landed album-ids statement (`mediafile_repo.rs:2118`, `SELECT f.id FROM
    // folder f`), so the contract is exercised by executing the resolved clause inside that
    // exact aliased shape. Corpus folders: fo-1/fo-2/fo-3 in LIB_MAIN, fo-9 in LIB_OTHER.
    let granted_b_only = lease(&db, |conn| {
        resolve_user(conn, "u-zero".to_string(), ScopeAlias::FolderAlias)
            .unwrap_or_else(|e| panic!("resolve_user(u-zero, FolderAlias): {e}"))
    });
    assert_eq!(granted_b_only.alias(), ScopeAlias::FolderAlias);
    assert_eq!(
        granted_b_only.user_id(),
        "u-zero",
        "the authority remembers the identity it was resolved for"
    );
    let zero_clause = granted_b_only
        .rendered_clause()
        .expect("a zero-grant user gets a deny clause, never an unrestricted read");
    // `rendered_clause` is `(alias table, clause text, args)`, so .0 is the alias itself and
    // .1 the predicate.
    assert_eq!(
        zero_clause.0, "f",
        "the authority carries the alias it was resolved for"
    );
    assert!(
        zero_clause.1.starts_with("f.library_id IN"),
        "the clause is qualified on the alias it was resolved for: {}",
        zero_clause.1
    );
    assert_eq!(
        folder_ids_over_alias(&db, Some(&zero_clause), &[]),
        Vec::<String>::new(),
        "NEGATIVE CONTROL: a zero-grant user's f-alias read returns NOTHING against a \
         non-empty folder table (4 rows seeded)"
    );
    assert_eq!(
        album_ids_over_folder_alias(&db, Some(&zero_clause)),
        Vec::<String>::new(),
        "the same clause in the production album-ids shape also yields nothing"
    );

    // A user granted ONLY library B (zero grants for A) sees exactly B's folder set through the
    // alias — the shape API-02's folder-scope plumbing hands to the album-ids read.
    grant(&db, "u-zero", LIB_OTHER);
    let b_clause = lease(&db, |conn| {
        resolve_user(conn, "u-zero".to_string(), ScopeAlias::FolderAlias)
            .unwrap_or_else(|e| panic!("resolve_user(u-zero, FolderAlias): {e}"))
    })
    .rendered_clause()
    .expect("a granted non-admin gets a clause");
    assert_eq!(
        folder_ids_over_alias(&db, Some(&b_clause), &[]),
        vec!["fo-9".to_string()],
        "the f-alias clause over a folder read returns exactly library B's folder"
    );
    assert_eq!(
        album_ids_over_folder_alias(&db, Some(&b_clause)),
        vec!["al-9".to_string()],
        "executed through the album-ids shape it yields exactly B's album"
    );

    // All-grant arms omit the predicate and see the FULL set on the same statement.
    for (user_id, arm) in [
        ("u-admin", ScopeDecision::Admin),
        ("u-all", ScopeDecision::AllLibraries),
        (HEADLESS_USER_ID, ScopeDecision::Headless),
    ] {
        let scope = lease(&db, |conn| {
            resolve_user(conn, user_id.to_string(), ScopeAlias::FolderAlias)
                .unwrap_or_else(|e| panic!("resolve_user({user_id}, FolderAlias): {e}"))
        });
        assert_eq!(scope.decision(), arm, "{user_id}");
        assert!(
            scope.rendered_clause().is_none(),
            "{user_id} must omit the clause"
        );
        assert_eq!(
            folder_ids_over_alias(&db, None, &[]),
            vec![
                "fo-1".to_string(),
                "fo-2".to_string(),
                "fo-3".to_string(),
                "fo-9".to_string()
            ],
            "{user_id}: the unrestricted arm reads the whole folder table through the alias"
        );
        assert_eq!(
            album_ids_over_folder_alias(&db, None),
            vec![
                "al-1".to_string(),
                "al-2".to_string(),
                "al-3".to_string(),
                "al-9".to_string()
            ],
            "{user_id}: album-ids over the alias with no clause is the full corpus"
        );
    }

    // The two folder-side renderers agree, differing only in qualification:
    // `folder_scope_clause` (`scanner_queries.rs:1215`, `#[doc(hidden)]` seam) renders the
    // `folder.` form, `membership_clause(FolderAlias, …)` the `f.` form. Both are executed.
    let folder_table_clause =
        folder_scope_clause(Some("u-zero")).expect("a granted user gets a folder-side clause");
    let alias_clause = membership_clause(ScopeAlias::FolderAlias, "u-zero");
    assert!(
        folder_table_clause.0.starts_with("folder.library_id IN"),
        "{}",
        folder_table_clause.0
    );
    assert!(
        alias_clause.0.starts_with("f.library_id IN"),
        "{}",
        alias_clause.0
    );
    assert_eq!(
        folder_table_clause
            .0
            .replace("folder.library_id", "f.library_id"),
        alias_clause.0,
        "one renderer, two qualifications — the predicate text is otherwise identical"
    );
    let executed_folder_table = lease(&db, |conn| {
        let sql = format!(
            "SELECT folder.id FROM folder WHERE {} ORDER BY folder.id",
            folder_table_clause.0
        );
        let mut stmt = conn.prepare(&sql).unwrap();
        stmt.query_map(
            rusqlite::params_from_iter(folder_table_clause.1.iter().map(|v| match v {
                Value::Text(t) => SqlValue::Text(t.clone()),
                Value::Integer(i) => SqlValue::Integer(*i),
                Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
                Value::Real(f) => SqlValue::Real(*f),
                Value::Null => SqlValue::Null,
            })),
            |row| row.get::<_, String>(0),
        )
        .unwrap()
        .map(|r| r.unwrap())
        .collect::<Vec<String>>()
    });
    assert_eq!(
        executed_folder_table,
        folder_ids_over_alias(&db, Some(&b_clause), &[]),
        "the `folder.` and `f.` arms return the same set when executed"
    );
    // ---- get_visible_media: the point read over the three alias shapes ----
    // The alias shapes are `media_file` (the entity's own table, which this read uses),
    // `f` (the folder alias of a folder-joined read, executed separately above) and the
    // headless/admin None-clause arm, exercised here against one row.
    let visible_for = |id: &str, user_id: &str| -> Result<MediaFileRecord, VisibleMediaError> {
        lease(&db, |conn| get_visible_media(conn, id, user_id, false))
    };
    let row = visible_for("mf-1", "u-all").unwrap_or_else(|e| panic!("mf-1 for u-all: {e}"));
    assert_eq!(row.path, "album-one/track-01.mp3");
    assert_eq!(row.library_id, LIB_MAIN);
    assert_eq!(row.album_id, "al-1");
    assert_eq!(row.id, "mf-1");
    assert_eq!(
        visible_for("mf-9", "u-admin").unwrap().library_id,
        LIB_OTHER
    );
    assert_eq!(
        visible_for("mf-9", nd_db::scanner_queries::HEADLESS_USER_ID)
            .unwrap()
            .id,
        "mf-9",
        "headless: clause omitted AND annotation/bookmark joins skipped"
    );
    assert!(
        matches!(
            visible_for("mf-1", "u-zero"),
            Err(VisibleMediaError::NotFound)
        ),
        "a zero-grant user gets NotFound, not an error"
    );
    seed_user(&db, "u-only-main", false);
    grant(&db, "u-only-main", LIB_MAIN);
    assert!(
        matches!(
            visible_for("mf-9", "u-only-main"),
            Err(VisibleMediaError::NotFound)
        ),
        "a LIB_MAIN-only user cannot read a LIB_OTHER row (Denied arm's clause excludes it)"
    );
    assert!(visible_for("mf-1", "u-only-main").is_ok());
    assert!(matches!(
        visible_for("mf-absent", "u-all"),
        Err(VisibleMediaError::NotFound)
    ));
    // The probe-minted row is readable by its committed id, proving the point read covers
    // rows whose id was generated inside the write transaction.
    assert_eq!(visible_for(&probe_id, "u-all").unwrap().id, probe_id);

    // ---- IdChunkedCursor + single-statement visitor: stable PID order ----
    let pid_order = lease(&db, |conn| {
        let mut out: Vec<(String, String)> = Vec::new();
        let outcome = visit_media_in_folder(conn, "fo-2", None, &cancel, |record| {
            out.push((record.pid.clone(), record.id.clone()));
            Control::Continue
        })
        .unwrap_or_else(|e| panic!("fo-2 read: {e:?}"));
        match outcome {
            VisitOutcome::Completed { visited } => assert_eq!(visited, 2),
            other => panic!("{other:?}"),
        }
        out
    });
    let expected_pid_order = lease(&db, |conn| {
        conn.prepare("SELECT pid, id FROM media_file WHERE folder_id = 'fo-2' ORDER BY pid")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect::<Vec<(String, String)>>()
    });
    assert_eq!(
        pid_order, expected_pid_order,
        "the visitor's order IS the statement's ORDER BY pid"
    );
    assert_eq!(pid_order.len(), 2);

    // Control::Stop truncates the read after the FIRST delivered record and reports a
    // truthful prefix length: `visited` counts records actually handed to the callback
    // (`scanner_queries.rs:272-275`), so stopping on row 1 reports 1, not 0 and not 2.
    let stopped = lease(&db, |conn| {
        visit_media_in_folder(conn, "fo-1", None, &cancel, |record| {
            assert_eq!(record.folder_id, "fo-1");
            Control::Stop
        })
        .unwrap_or_else(|e| panic!("stop read: {e:?}"))
    });
    assert!(
        matches!(stopped, VisitOutcome::Stopped { visited: 1 }),
        "{stopped:?}"
    );

    // A cancelled flag stops the read at the first FETCHED row, before decoding it: the
    // check sits between `rows.next()` and `map(row)` (:268-271), so nothing is delivered.
    let cancelled = nd_utils::context::Cancellation::new();
    cancelled.cancel();
    let outcome = lease(&db, |conn| {
        visit_media_in_folder(conn, "fo-1", None, &cancelled, |_| Control::Continue)
    });
    assert!(
        matches!(outcome, Ok(VisitOutcome::Stopped { visited: 0 })),
        "{outcome:?}"
    );

    // A closed cursor reports Closed, not Sqlite.
    lease(&db, |conn| {
        let scope = ScannerScope::for_library(LIB_MAIN);
        let mut cursor = album_ids_by_folders(conn, &scope, &["fo-1".to_string()]);
        let _ = cursor.visit_chunk(&cancel, |_| Control::Continue).unwrap();
        assert!(cursor.is_complete());
        assert!(matches!(
            cursor.visit_chunk(&cancel, |_| Control::Continue),
            Err(CursorError::Closed)
        ));
    });

    // split_paths: Go's `FindByPaths` prefix rule, library groups ascending.
    let paths: Vec<String> = vec![
        "2:other/album-nine/track-01.mp3".to_string(),
        "album-one/track-01.mp3".to_string(),
        "1:album-one/track-02.mp3".to_string(),
    ];
    let (groups, unqualified) = split_paths(&paths);
    assert_eq!(groups.len(), 2, "one group per numeric prefix, ascending");
    assert_eq!(groups[0].0, 1);
    assert_eq!(groups[1].0, 2);
    assert_eq!(
        unqualified.len(),
        3,
        "every path is ALSO searched unqualified"
    );
    let by_path = lease(&db, |conn| {
        let scope = resolve_user(conn, "u-admin".to_string(), ScopeAlias::MediaFile).unwrap();
        let mut hits: Vec<String> = Vec::new();
        let outcome = media_by_paths(conn, &paths, Some(&scope), &cancel, |record| {
            hits.push(record.id.clone());
            Control::Continue
        })
        .unwrap_or_else(|e| panic!("media_by_paths: {e:?}"));
        assert!(
            matches!(outcome, VisitOutcome::Completed { visited: 3 }),
            "{outcome:?}"
        );
        hits.sort();
        hits
    });
    assert_eq!(
        by_path,
        vec!["mf-1".to_string(), "mf-2".to_string(), "mf-9".to_string()],
        "path lookup resolves qualified AND unqualified forms to the same rows, deduped"
    );
    let empty = lease(&db, |conn| {
        media_by_paths(conn, &[], None, &cancel, |_| Control::Continue)
            .unwrap_or_else(|e| panic!("empty path batch: {e:?}"))
    });
    assert!(
        matches!(empty, VisitOutcome::Completed { visited: 0 }),
        "Go's early return issues no SQL"
    );
    // ---- artwork_queue state: exactly what the four batches produced ----
    // Each batch enqueues its non-excluded artists (loop order: artists first) then its
    // albums (scanner/phase_1_folders.go:412-414, :423-425). Batch 1's excluded pair
    // (UnknownArtist / VariousArtists) must be ABSENT — that is the non-vacuity check.
    let queue = lease(&db, |conn| {
        let mut stmt = conn
            .prepare(
                "SELECT item_kind || '/' || item_id FROM artwork_queue ORDER BY item_kind, item_id",
            )
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect::<Vec<String>>()
    });
    assert_eq!(
        queue,
        vec![
            "al/al-1".to_string(),
            "al/al-2".to_string(),
            "al/al-3".to_string(),
            "al/al-9".to_string(),
            "ar/ar-main".to_string(),
            "ar/ar-nine".to_string(),
            "ar/ar-second".to_string(),
        ],
        "queue membership is the batch artists/albums minus Go's excluded names"
    );
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM artwork_queue"), 7);
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM artwork_queue WHERE item_id IN ('ar-unknown','ar-va')"
        ),
        0,
        "Go's queue-name filters (:412) really excluded the two placeholder artists"
    );
    assert_eq!(
        scalar::<i64>(
            &db,
            &format!(
                "SELECT count(*) FROM artwork_queue WHERE priority <> {SCAN_ARTWORK_PRIORITY}"
            )
        ),
        0,
        "every scan-enqueued row carries SCAN_ARTWORK_PRIORITY          (playlist_repo::ARTWORK_PRIORITY_SCAN)"
    );
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM artwork_queue WHERE image_type <> 'primary' OR attempts <> 0"
        ),
        0,
        "scanArtworkItem sets ImageTypePrimary and the renderer binds attempts = 0"
    );
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM artwork_queue WHERE enqueued_at <> retry_at"
        ),
        0,
        "the renderer binds `now` to both enqueued_at and retry_at (:150-155)"
    );

    // Enqueue arms: Reset resets the LIFECYCLE of an existing row; EnqueueIfMissing must not
    // overwrite a higher priority (Go's MAX(priority, excluded.priority), :78).
    // Drive the row the corpus already enqueued into a state the conflict arms can be
    // observed FROM: a HIGHER priority, a spent retry budget, a stale lifecycle and a
    // non-empty trace (`artwork_queue` has no last_failure column — `schema_ddl.rs:50`).
    fixture(
        &db,
        "UPDATE artwork_queue SET priority = 90, attempts = 4, retry_at = ?, \
                  enqueued_at = ?, trace = ? WHERE item_kind = 'al' AND item_id = 'al-1'",
        &[
            text(time::to_text(&at(2001, 1, 1, 0, 0, 0))),
            text(time::to_text(&at(2001, 1, 1, 0, 0, 0))),
            text("[\"old\"]"),
        ],
    );
    db.try_with_tx_sync::<(), Infallible>(|tx| {
        let items = vec![scan_artwork_item(
            nd_id::ARTWORK_KIND_ALBUM_PREFIX,
            "al-1",
            SCAN_ARTWORK_PRIORITY,
        )];
        enqueue_in_tx(tx, &items, EnqueueMode::Reset, &at(2030, 6, 7, 8, 9, 10))
    })
    .unwrap_or_else(|e| panic!("Reset enqueue: {e}"));
    assert_eq!(
        real_priority(&db, "al-1"),
        90.0,
        "Reset keeps MAX(priority, excluded.priority) = MAX(90, SCAN_ARTWORK_PRIORITY)"
    );
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM artwork_queue WHERE item_id = 'al-1' AND attempts = 0"
        ),
        1,
        "Reset resets attempts and trace"
    );
    assert_eq!(
        lease(&db, |conn| {
            conn.query_row(
                "SELECT trace FROM artwork_queue \
                 WHERE item_kind = 'al' AND item_id = 'al-1' AND image_type = 'primary'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap_or_else(|e| panic!("queue trace: {e}"))
        }),
        "[]",
        "the Reset conflict arm writes trace = '[]' literally (:73)"
    );

    db.try_with_tx_sync::<(), Infallible>(|tx| {
        let items = vec![
            scan_artwork_item(
                nd_id::ARTWORK_KIND_ALBUM_PREFIX,
                "al-1",
                SCAN_ARTWORK_PRIORITY,
            ),
            scan_artwork_item(
                nd_id::ARTWORK_KIND_ARTIST_PREFIX,
                "ar-new",
                SCAN_ARTWORK_PRIORITY,
            ),
        ];
        enqueue_if_missing_in_tx(tx, &items, &at(2031, 1, 1, 0, 0, 0))
    })
    .unwrap_or_else(|e| panic!("EnqueueIfMissing: {e}"));
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM artwork_queue"),
        8,
        "EnqueueIfMissing skipped al-1 (already queued) and added ar-new"
    );
    assert_eq!(
        real_priority(&db, "al-1"),
        90.0,
        "the DO NOTHING arm cannot touch an existing row"
    );

    // Chunking: CHUNK_SIZE (100) bounds one statement's VALUES list, so 101 items must run
    // as two statements and still land all rows.
    let many: Vec<ArtworkQueueItem> = (0..101)
        .map(|i| scan_artwork_item(nd_id::ARTWORK_KIND_ARTIST_PREFIX, &format!("chunk-{i}"), 1))
        .collect();
    db.try_with_tx_sync::<(), Infallible>(|tx| {
        enqueue_in_tx(tx, &many, EnqueueMode::Reset, &at(2032, 1, 1, 0, 0, 0))
    })
    .unwrap_or_else(|e| panic!("chunked enqueue: {e}"));
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM artwork_queue WHERE item_id LIKE 'chunk-%'"
        ),
        101
    );
    assert_eq!(nd_db::artwork_queue_repo::CHUNK_SIZE, 100);

    // ---- FTS: MATCH + counts, joined to base rowids ----
    assert_eq!(fts_match(&db, "media_file_fts", "Zbrfldrmf1"), 1);
    assert_eq!(
        fts_ids(&db, "media_file_fts", "media_file", "Zbrfldrmf1"),
        vec!["mf-1".to_string()]
    );
    assert_eq!(
        fts_match(&db, "media_file_fts", "Zbrfldrprobe"),
        1,
        "the row whose id was minted inside the write transaction is indexed too"
    );
    // Participant/genre terms reach the FTS columns through the search_participants /
    // tags-derived columns the triggers index.
    assert_eq!(
        fts_ids(&db, "artist_fts", "artist", "\"Main Act\""),
        vec!["ar-main".to_string()],
        "artist name reaches artist_fts.name through the AFTER INSERT trigger"
    );
    assert_eq!(fts_match(&db, "artist_fts", "\"Second Act\""), 1);
    assert_eq!(
        fts_ids(&db, "album_fts", "album", "Albumidal1"),
        vec!["al-1".to_string()]
    );
    assert_eq!(fts_match(&db, "album_fts", "Albumidal9"), 1);
    // Every corpus album carries the same `albumversion` tag value, so the trigger's
    // json_each-derived `album_version` column indexes it on all four.
    assert_eq!(
        fts_match(&db, "album_fts", "Deluxe"),
        4,
        "the albumversion value reaches album_fts.album_version via json_each in the trigger"
    );
    assert_eq!(
        fts_ids(&db, "album_fts", "album", "Deluxe"),
        vec![
            "al-1".to_string(),
            "al-2".to_string(),
            "al-3".to_string(),
            "al-9".to_string()
        ]
    );
    // A term that exists nowhere: proves MATCH is actually evaluating, not counting rows.
    assert_eq!(fts_match(&db, "media_file_fts", "Zbrfldrnothing"), 0);
    assert_eq!(fts_match(&db, "album_fts", "nosuchalbumterm"), 0);
    // Rowid join covers every base row, so no ghost entry exists for a row that is gone.
    let joined = lease(&db, |conn| {
        conn.query_row(
            "SELECT count(*) FROM media_file_fts f JOIN media_file b ON b.rowid = f.rowid",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    });
    assert_eq!(joined, 6, "every FTS entry joins exactly one live base row");

    cleanup(db, path);
}

// ---------------------------------------------------------------------------
// Test 7: Go-literal constant pins, the unknown-album exclusion EXECUTED, and the
// IdChunkedCursor terminal arms (Stop / clean-exhaustion Closed / lease release).
// ---------------------------------------------------------------------------

#[test]
fn go_constants_unknown_album_exclusion_and_cursor_terminal_arms() {
    // ---- (a) The Go literals, as VALUES not just types ----
    // `model.ArtworkPriorityScan` = 50 (`model/artwork.go:116`); `artworkChunkSize` =
    // `artworkBatchSize * 3` = 600 (`persistence/artwork_hydration.go:17`,
    // `persistence/artwork_repository.go:16`). The compile-time array ascriptions at the top of
    // this file already break compilation on drift; these assert the same values as data so the
    // report can cite them.
    assert_eq!(
        SCAN_ARTWORK_PRIORITY, 50,
        "model/artwork.go:116 ArtworkPriorityScan"
    );
    assert_eq!(
        nd_db::playlist_repo::ARTWORK_PRIORITY_SCAN,
        50,
        "the single source of truth the renderer binds"
    );
    assert_eq!(
        nd_db::artist_repo::CURSOR_CHUNK_SIZE,
        600,
        "artworkChunkSize (artwork_hydration.go:17), corrected from 300 by D-CURSOR(4)"
    );
    assert_eq!(
        ALBUM_IDS_FOLDER_CHUNK_SIZE, 200,
        "slices.Chunk(folderIDs, 200), \
        mediafile_repository.go:297"
    );
    assert_eq!(nd_db::artwork_queue_repo::CHUNK_SIZE, 100);

    // ---- (b) The unknown-album exclusion, EXECUTED through persist_folder_delta ----
    // The smallest case where the filter actually filters: one album named
    // `QUEUE_EXCLUDED_ALBUM_NAME` (`scanner_writes.rs:78` = `consts.UNKNOWN_ALBUM`,
    // `phase_1_folders.go:423-425`) alongside one normal album, no artists.
    let (db, path) = fresh_store("exclusion");
    seed_libraries(&db);
    let mut batch = batch_one(52);
    batch.artists = Vec::new();
    batch.albums = vec![
        album_record(
            "al-unk",
            LIB_MAIN,
            "ar-main",
            "ar-main",
            "fo-1",
            created_of("al-unk"),
        ),
        album_record(
            "al-ok",
            LIB_MAIN,
            "ar-main",
            "ar-main",
            "fo-1",
            created_of("al-ok"),
        ),
    ];
    batch.albums[0].name = nd_db::scanner_writes::QUEUE_EXCLUDED_ALBUM_NAME.to_string();
    batch.media = Vec::new();
    batch.album_transitions = Vec::new();
    batch.tags = Vec::new();
    batch.full_scan_mode = true;
    let outcome = run_ok(&db, &batch);
    assert_eq!(
        outcome.album_ids,
        vec!["al-unk".to_string(), "al-ok".to_string()],
        "both albums are WRITTEN — the filter is a queue filter, not a persistence filter"
    );
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM artwork_queue WHERE item_id = 'al-unk'"
        ),
        0,
        "the unknown-named album is absent from the queue: the exclusion really filtered it"
    );
    assert_eq!(
        set_of(&db, "SELECT item_kind || '/' || item_id FROM artwork_queue"),
        vec!["al/al-ok".to_string()],
        "and the normal album is the only queued item"
    );
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM artwork_queue WHERE priority = 50"
        ),
        1,
        "the queued row carries Go's literal scan priority 50"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM album WHERE id = 'al-unk'"),
        1,
        "non-vacuity: the excluded row exists, so its queue absence is the filter's doing"
    );

    // ---- (c) IdChunkedCursor terminal arms ----
    // 601 tracks in ONE folder, 601 distinct album ids, so one statement yields 601 records and
    // stopping at 300 is mid-CHUNK (the chunk unit is the folder-id list, not the row count).
    let rows: Vec<String> = (0..601)
        .map(|i| format!("('mf-c{i}', 'al-c{i}', 'fo-1', 0, 'lib-c{i}')"))
        .collect();
    let mut sql =
        String::from("INSERT INTO media_file (id, album_id, folder_id, missing, title) VALUES ");
    sql.push_str(&rows.join(", "));
    fixture(&db, &sql, &[]);
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM media_file WHERE folder_id = 'fo-1'"
        ),
        601
    );

    let cancel = nd_utils::context::Cancellation::new();
    let folder_ids: Vec<String> = vec!["fo-1".to_string()];
    let stopped_mid_chunk = lease(&db, |conn| {
        let scope = ScannerScope::for_library(LIB_MAIN);
        let mut cursor = album_ids_by_folders(conn, &scope, &folder_ids);
        assert_eq!(
            cursor.remaining_chunks(),
            1,
            "one folder id = one statement"
        );
        assert!(!cursor.is_complete());
        let mut seen = 0usize;
        let outcome = cursor
            .visit_chunk(&cancel, |_| {
                seen += 1;
                if seen == 300 {
                    Control::Stop
                } else {
                    Control::Continue
                }
            })
            .unwrap_or_else(|e| panic!("stop mid-chunk: {e:?}"));
        assert!(
            matches!(outcome, VisitOutcome::Stopped { visited: 300 }),
            "{outcome:?}"
        );
        assert_eq!(seen, 300, "exactly 300 records were delivered");
        // The stopped chunk WAS the last statement, so the cursor is terminal now.
        assert!(
            cursor.is_complete(),
            "the last chunk was consumed, so the cursor closed"
        );
        assert_eq!(cursor.remaining_chunks(), 0);
        let err = cursor.visit_chunk(&cancel, |_| Control::Continue);
        assert!(
            matches!(err, Err(CursorError::Closed)),
            "a stopped-at-last-chunk cursor reports Closed, not a zero-visited Completed: {err:?}"
        );
        seen
    });
    assert_eq!(stopped_mid_chunk, 300);

    // The terminal-contract nuance, pinned: a Stop in a NON-final chunk does NOT close the
    // cursor — the IdChunked kind (D-CURSOR(1)) is non-resumable only on ERROR, and `complete`
    // is set by chunk exhaustion (`scanner_queries.rs:566-576`). 201 folder ids = 2 chunks
    // (`ALBUM_IDS_FOLDER_CHUNK_SIZE` = 200), so stopping in chunk 1 must still offer chunk 2.
    let extra: Vec<String> = (0..201)
        .map(|i| {
            format!(
                "('fo-e{i}', {LIB_MAIN}, 'path-e{i}', 'name-e{i}', 0, '', 0, '[]', \
                 '0000-00-00 00:00:00')"
            )
        })
        .collect();
    let mut insert = String::from(
        "INSERT INTO folder (id, library_id, path, name, num_audio_files, parent_id, \
         num_playlists, image_files, images_updated_at) VALUES ",
    );
    insert.push_str(&extra.join(", "));
    fixture(&db, &insert, &[]);
    // One track in the FIRST chunk's first folder, so chunk 1 delivers a record to stop on.
    fixture(
        &db,
        "INSERT INTO media_file (id, album_id, folder_id, missing, title) VALUES \
         ('mf-e0', 'al-e0', 'fo-e0', 0, 'e0')",
        &[],
    );
    let many_folder_ids: Vec<String> = (0..201)
        .map(|i| format!("fo-e{i}"))
        .chain(std::iter::once("fo-1".to_string()))
        .collect();
    lease(&db, |conn| {
        let scope = ScannerScope::for_library(LIB_MAIN);
        let mut cursor = album_ids_by_folders(conn, &scope, &many_folder_ids);
        assert_eq!(
            cursor.remaining_chunks(),
            2,
            "202 ids (201 generated + fo-1) chunk as 200 + 2"
        );
        let first = cursor
            .visit_chunk(&cancel, |id| {
                assert_eq!(id, "al-e0", "chunk 1 covers fo-e0..fo-e199 only");
                Control::Stop
            })
            .unwrap_or_else(|e| panic!("stop in chunk 1: {e:?}"));
        assert!(
            matches!(first, VisitOutcome::Stopped { visited: 1 }),
            "{first:?}"
        );
        assert!(
            !cursor.is_complete(),
            "a Stop mid-stream does not close the cursor"
        );
        assert_eq!(cursor.remaining_chunks(), 1);
        let second = cursor
            .visit_chunk(&cancel, |_| Control::Continue)
            .unwrap_or_else(|e| panic!("chunk 2 after a Stop: {e:?}"));
        assert!(
            matches!(second, VisitOutcome::Completed { visited: 601 }),
            "chunk 2 is fo-e200 + fo-1, i.e. the 601 album ids of the stopped-at-300 folder: \
             {second:?}"
        );
        assert!(cursor.is_complete());
        assert_eq!(cursor.remaining_chunks(), 0);
        // (ii) Closed AFTER CLEAN EXHAUSTION, distinct from the error path below.
        assert!(
            matches!(
                cursor.visit_chunk(&cancel, |_| Control::Continue),
                Err(CursorError::Closed)
            ),
            "clean exhaustion also reports Closed"
        );
    });

    // A chunk FAILURE closes the cursor too: Sqlite once, then Closed forever (`:505-510`,
    // `fail()`). Built through the `#[doc(hidden)]` construction seam with two chunk statements,
    // the FIRST one unpreparable, so the non-resumability is observable: the second (valid)
    // statement is never offered.
    lease(&db, |conn| {
        let mut cursor: IdChunkedCursor<'_, String> = IdChunkedCursor::new(
            conn,
            vec![
                (
                    "SELECT id FROM no_such_table_where_cursor_fails".to_string(),
                    Vec::new(),
                ),
                (
                    "SELECT id FROM media_file WHERE folder_id = 'fo-1' AND missing = 0"
                        .to_string(),
                    Vec::new(),
                ),
            ],
            |row| row.get::<_, String>(0),
            None,
        );
        assert_eq!(cursor.remaining_chunks(), 2);
        let err = cursor.visit_chunk(&cancel, |_| Control::Continue);
        assert!(
            matches!(err, Err(CursorError::Sqlite(_))),
            "the first failure is Sqlite: {err:?}"
        );
        assert!(cursor.is_complete(), "a failed cursor closes immediately");
        assert_eq!(
            cursor.remaining_chunks(),
            1,
            "the failed chunk is never re-offered"
        );
        assert!(
            matches!(
                cursor.visit_chunk(&cancel, |_| Control::Continue),
                Err(CursorError::Closed)
            ),
            "every later use reports Closed, so the valid second chunk never runs"
        );
    });
    cleanup(db, path);

    // ---- (d) The lease is released when the cursor drops ----
    // Pool pinned to 2 so the leased/idle split is exact: while the cursor runs, idle is 1;
    // after the enclosing lease drops (cursor's `&Connection` borrow ends), idle is 2 again.
    let (db2, path2) = fresh_store_sized("cursor-lease", 2);
    seed_libraries(&db2);
    run_corpus(&db2, 53);
    assert_eq!(
        db2.idle_count().unwrap(),
        2,
        "both handles idle before the read"
    );
    let visited = db2
        .with_connection_sync(|conn| {
            assert_eq!(db2.idle_count().unwrap(), 1, "the lease took one handle");
            let scope = ScannerScope::for_library(LIB_MAIN);
            let mut cursor =
                album_ids_by_folders(conn, &scope, &["fo-1".to_string(), "fo-2".to_string()]);
            let mut n = 0usize;
            while !cursor.is_complete() {
                match cursor.visit_chunk(&cancel, |_| {
                    n += 1;
                    Control::Continue
                }) {
                    Ok(VisitOutcome::Completed { .. }) => {}
                    Ok(other) => panic!("unexpected outcome {other:?}"),
                    Err(CursorError::Sqlite(err)) => return Err(err),
                    Err(other) => panic!("unexpected cursor error {other:?}"),
                }
            }
            Ok(n)
        })
        .unwrap_or_else(|e| panic!("leased cursor read: {e}"));
    assert_eq!(
        visited, 2,
        "fo-1 + fo-2 in ONE 200-id chunk: their two distinct album ids, delivered once"
    );
    assert_eq!(
        db2.idle_count().unwrap(),
        2,
        "dropping the cursor (and with it the lease) returned the handle to the idle queue"
    );
    cleanup(db2, path2);
}

/// Priority of a queue row (INTEGER column).
fn real_priority(store: &SQLStore, id: &str) -> f64 {
    lease(store, |conn| {
        conn.query_row(
            "SELECT priority FROM artwork_queue WHERE item_id = ?",
            [text(id)],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or_else(|e| panic!("priority for {id}: {e}"))
    }) as f64
}

// ---------------------------------------------------------------------------
// Deliverable (b) execution half: the surfaces whose signatures cannot be
// ascribed as `fn` pointers are pinned here by MONOMORPHIZED calls, which fix
// their concrete parameter and return types at least as strictly.
// ---------------------------------------------------------------------------

#[test]
fn p1_surface_freeze_compiles() {
    let (db, path) = fresh_store("surface-freeze");
    seed_libraries(&db);
    // A resolvable item_artwork row, so the delete primitive below has something to delete
    // (an empty result would make the assertion vacuous).
    seed_item_artwork(&db, nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, "mf-seed");
    seed_user(&db, "u-zero", false);
    run_corpus(&db, 55);
    let cancel = nd_utils::context::Cancellation::new();

    // DB-00 seam, explicitly monomorphized so the parameter and return types are fixed.
    let seam: Result<u8, DbRuntimeError<Infallible>> =
        db.try_with_tx_sync::<u8, Infallible>(|tx| {
            Ok(tx
                .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                .unwrap() as u8)
        });
    // `SELECT 1` inside the transaction returns 1, so the block's own value crosses the
    // seam boundary intact (commit-on-success, `persistence.go:176-180`).
    assert_eq!(seam.unwrap(), 1);
    // `try_with_tx_retry_sync` supplies the production delay internally
    // (`store.rs:429-437`, `production_delay`), so its signature takes only the block.
    let seam_retry: Result<u8, DbRuntimeError<Infallible>> = db
        .try_with_tx_retry_sync::<u8, Infallible>(|tx| {
            Ok(tx
                .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                .unwrap() as u8)
        });
    assert!(seam_retry.is_ok());
    let retry_delay: &dyn Fn(u32) -> std::time::Duration = &|_| std::time::Duration::from_millis(1);
    let mut conn = Connection::open_in_memory().unwrap();
    let free: Result<u8, DbRuntimeError<Infallible>> = try_with_tx(&mut conn, |tx| {
        Ok(tx
            .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .unwrap() as u8)
    });
    assert!(free.is_ok());
    let free_retry: Result<u8, DbRuntimeError<Infallible>> =
        try_with_tx_retry(&mut conn, retry_delay, |tx| {
            Ok(tx
                .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                .unwrap() as u8)
        });
    assert!(free_retry.is_ok());
    // DB-00's busy predicate: only the SQLITE_BUSY primary code is retryable
    // (`db/db.go:128-140`), so an SQLITE_ERROR failure must read as not-busy.
    assert!(!is_busy(&SqliteError::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
        Some("not busy".to_string())
    )));
    assert!(is_busy(&SqliteError::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
        Some("busy".to_string())
    )));
    assert!(
        !is_busy(&SqliteError::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_LOCKED),
            Some("locked".to_string())
        )),
        "SQLITE_LOCKED is NOT retryable (persistence.go:218)"
    );

    // DB-02 canonical stamp, and the zero time that Go's year-one convention stores.
    assert_eq!(
        time::value(&seed_stamp()),
        Value::Text(time::to_text(&seed_stamp()))
    );
    assert_eq!(time::to_text(&time::zero()), "0001-01-01 00:00:00+00:00");

    // TimeBound::contains (the method no `fn` pointer type can name).
    let from = at(2020, 1, 1, 0, 0, 0);
    let until = at(2020, 2, 1, 0, 0, 0);
    let bound = TimeBound {
        from: &from,
        until: &until,
    };
    assert!(bound.contains(&at(2020, 1, 15, 0, 0, 0)));
    assert!(
        bound.contains(&from),
        "lower bound is INCLUSIVE (`from <= t`)"
    );
    assert!(
        !bound.contains(&until),
        "upper bound is EXCLUSIVE (`t < until`)"
    );
    assert!(
        !bound.contains(&at(2019, 12, 31, 0, 0, 0)),
        "below the bound is outside it"
    );

    // The generic visitors, monomorphized with a concrete `FnMut(MediaFileRecord) -> Control`.
    lease(&db, |conn| {
        let scope = ScannerScope::for_library(LIB_MAIN);
        assert_eq!(scope.library_id(), LIB_MAIN);
        let outcome: VisitOutcome = visit_media_in_folder(conn, "fo-1", None, &cancel, |record| {
            assert_eq!(record.folder_id, "fo-1");
            Control::Continue
        })
        .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(matches!(outcome, VisitOutcome::Completed { visited: 2 }));

        let outcome = visit_media_in_album(conn, "al-1", None, &cancel, |record| {
            assert_eq!(record.album_id, "al-1");
            Control::Continue
        })
        .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(matches!(outcome, VisitOutcome::Completed { visited: 2 }));

        let outcome = visit_media_in_filter(
            conn,
            MediaPhaseFilter::InFolder { folder_id: "fo-2" },
            None,
            &cancel,
            |record| {
                assert_eq!(record.folder_id, "fo-2");
                Control::Continue
            },
        )
        .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(matches!(outcome, VisitOutcome::Completed { visited: 2 }));

        // DB-11's other visitors over the corpus, so their signatures are exercised too.
        let outcome = visit_missing_and_matching(conn, &scope, &cancel, |record| {
            assert_eq!(record.library_id, LIB_MAIN);
            Control::Continue
        })
        .unwrap_or_else(|e| panic!("visit_missing_and_matching: {e:?}"));
        assert!(
            matches!(outcome, VisitOutcome::Completed { visited: 0 }),
            "no missing row exists in a clean corpus: the read is still well-typed"
        );

        let outcome = visit_touched_albums(conn, &scope, false, &cancel, |record| {
            assert_eq!(record.library_id, LIB_MAIN);
            Control::Continue
        })
        .unwrap_or_else(|e| panic!("visit_touched_albums: {e:?}"));
        assert!(
            matches!(outcome, VisitOutcome::Completed { .. }),
            "{outcome:?}"
        );

        // The subtree cursor reader (`album_repository.go:285-304`), path-chunked.
        let mut cursor = sole_album_artist_ids_in_subtrees(conn, &scope, &[String::new()]);
        let mut ids: Vec<String> = Vec::new();
        while !cursor.is_complete() {
            cursor
                .visit_chunk(&cancel, |id| {
                    ids.push(id);
                    Control::Continue
                })
                .unwrap_or_else(|e| panic!("subtree chunk: {e:?}"));
        }

        // The doc(hidden) seams: executable here, deliberately outside the documented surface.
        let (groups, unqualified) = split_paths(&["3:one".to_string()]);
        assert_eq!(groups[0].0, 3);
        assert_eq!(unqualified.len(), 1);
        let clause = membership_clause(ScopeAlias::MediaFile, "u-zero");
        assert!(clause.0.contains("user_library"));
        let artist_clause = artist_scope_clause(&[LIB_MAIN]);
        assert!(artist_clause.0.contains("library_artist"));
        let cursor = album_ids_by_folders(conn, &scope, &["fo-1".to_string()]);
        assert_eq!(cursor.remaining_chunks(), 1);
    });
    // Chunk sizes: both media-file readers delegate to `mediafile_repo::CHUNK_SIZE`
    // (`mediafile_repo.rs:1922` = 200); the subtree reader is Go's own
    // `subtreePathChunkSize` (`folder_repository.go:204`) at 100.
    assert_eq!(
        ALBUM_IDS_FOLDER_CHUNK_SIZE,
        nd_db::mediafile_repo::CHUNK_SIZE
    );
    assert_eq!(PATH_CHUNK_SIZE, nd_db::mediafile_repo::CHUNK_SIZE);
    assert_eq!(nd_db::mediafile_repo::CHUNK_SIZE, 200);
    assert_eq!(SUBTREE_PATH_CHUNK_SIZE, 100);
    assert_eq!(nd_db::artwork_queue_repo::CHUNK_SIZE, 100);

    // DB-12/DB-13/DB-14/DB-15 in-tx primitives, each on a real transaction.
    db.try_with_tx_sync::<(), Infallible>(|tx| {
        let n = delete_for_items_in_tx(
            tx,
            nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX,
            &["mf-seed".to_string()],
        )
        .unwrap_or_else(|e| panic!("delete_for_items_in_tx: {e}"));
        assert_eq!(
            n, 1,
            "the primitive deletes the seeded row and reports the count"
        );
        let items = vec![scan_artwork_item(
            nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX,
            "mf-x",
            1,
        )];
        enqueue_in_tx(tx, &items, EnqueueMode::Reset, &seed_stamp())
            .unwrap_or_else(|e| panic!("enqueue_in_tx: {e}"));
        enqueue_if_missing_in_tx(tx, &items, &seed_stamp())
            .unwrap_or_else(|e| panic!("enqueue_if_missing_in_tx: {e}"));
        let epoch = revoke_tokens_in_tx(tx, "u-zero")
            .unwrap_or_else(|e| panic!("revoke_tokens_in_tx: {e}"));
        assert_eq!(
            epoch.value(),
            1,
            "the guarded increment returns the post-bump token epoch"
        );
        reassign_annotation_in_tx(tx, AnnotationEntityKind::Album, "al-decoy", "al-1")
            .unwrap_or_else(|e| panic!("reassign_annotation_in_tx: {e}"));
        update_avg_rating_in_tx(tx, AnnotationEntityKind::Album, "al-1")
            .unwrap_or_else(|e| panic!("update_avg_rating_in_tx: {e}"));
        Ok(())
    })
    .unwrap_or_else(|e| panic!("surface freeze transaction: {e}"));
    assert_eq!(
        fts_match(&db, "media_file_fts", "Zbrfldrmf1"),
        1,
        "deleting an item_artwork row does not touch the media_file row or its FTS entry"
    );
    cleanup(db, path);
}

/// Run one renderer's statement on a live lease and report only whether it PREPARES.
///
/// Used by the deferred-gate inventory below — NOT as a general "every statement must
/// prepare" gate, which is deliberately not implemented (parent decision, dated 2026-11-21;
/// see the five excluded classes above).
fn prepare_probe(store: &SQLStore, sql: &str) -> Result<usize, String> {
    lease(store, |conn| match conn.prepare(sql) {
        Ok(statement) => Ok(statement.parameter_count()),
        Err(err) => Err(err.to_string()),
    })
}

#[test]
fn deferred_renderer_prepare_gate_classes_are_executed() {
    let (db, path) = fresh_store("deferred-gate");
    seed_libraries(&db);

    // Class 3 (D-PLUGIN): every `plugin_repo` renderer fails on a missing table because the
    // table is deliberately dropped. Owning lane DB-16 carries the file deletion.
    let plugin_sql = nd_db::plugin_repo::get_sql("p-1");
    let err =
        prepare_probe(&db, &plugin_sql.0).expect_err("the plugin table does not exist by D-PLUGIN");
    assert!(err.contains("no such table: plugin"), "{err}");

    // Class 4 (DB-15): `annotation_count_sql` references `annotation.id`, which the migrated
    // schema does not have. Known and pinned at DB-15 as keep-and-redocument.
    let count_sql = annotation_count_sql("album", "u-fan", &["al-1"]);
    let err = prepare_probe(&db, &count_sql.0)
        .expect_err("annotation has no id column (schema_ddl.rs:59)");
    assert!(err.contains("no such column: annotation.id"), "{err}");

    // Class 2 (DB-16): an empty column list renders `UPDATE share SET  WHERE id = ?`, which is
    // a SYNTAX/PARSE error — not a `no such column:` schema error. The dual-accept form
    // (`no such column:` OR `syntax error`) used to conceal the wrong explanation, so the exact
    // category is pinned here and the schema-error category is asserted ABSENT.
    let empty = nd_db::share_repo::update_sql("s-1", &[], None);
    assert_eq!(
        empty.0, "UPDATE share SET  WHERE id = ?",
        "the renderer really emits an empty SET list"
    );
    let err = prepare_probe(&db, &empty.0).expect_err("empty SET clause is invalid SQL");
    assert!(err.contains(r#"near "WHERE": syntax error"#), "{err}");
    assert!(
        !err.contains("no such column:"),
        "the failure is a parse error, not a missing-column error: {err}"
    );

    // Class 1 (DB-04..DB-17): `refresh_counters_sql` is INTENTIONALLY PARTIAL — its UPDATE
    // leg's args are completed by the caller from the SELECT's aggregate row, so a naive
    // `args.len() == parameter_count()` assertion fails on it by design.
    let (select, update) =
        nd_db::playlist_repo::refresh_counters_sql("pl-1", "2020-01-01 00:00:00+00:00");
    assert_eq!(prepare_probe(&db, &select.0), Ok(1));
    assert_eq!(
        update.1.len(),
        2,
        "the renderer supplies updated_at + id only"
    );
    assert_eq!(
        prepare_probe(&db, &update.0),
        Ok(5),
        "the UPDATE needs 5 bound values but the renderer provides 2 — the caller completes it \
         from the SELECT row (playlist_repository.go:392-408). A naive prepare gate fails here."
    );

    // The avg-rating expression is a SELECT fragment (class 5): standalone it prepares, but
    // it is consumed inside an UPDATE's SET by update_avg_rating_sql, so a prepare gate would
    // be asserting the wrong thing about it.
    let expr = average_rating_expr_sql();
    assert!(expr.contains("coalesce((select"), "{expr}");
    assert!(expr.contains("rating > 0"), "{expr}");

    // Class 5: clause FRAGMENTS are conjunctions the caller joins, not statements. Exclude by
    // SHAPE, and show why a name whitelist would drift.
    let fragment = artist_scope_clause(&[LIB_MAIN]);
    assert!(
        prepare_probe(&db, &fragment.0).is_err(),
        "a bare clause is not a statement and cannot be prepared alone"
    );
    let joined = format!("SELECT id FROM artist WHERE 1=1 AND {}", fragment.0);
    assert_eq!(
        prepare_probe(&db, &joined),
        Ok(1),
        "joined into a statement by the caller it prepares fine — the failure was shape, not text"
    );

    // The five classes, asserted present in the compile-time inventory so a future gate
    // cannot silently drop one.
    assert_eq!(DEFERRED_GATE_CLASSES.len(), 5);
    assert!(DEFERRED_GATE_CLASSES[2].contains("D-PLUGIN"));
    assert!(DEFERRED_GATE_CLASSES[1].contains("DB-16"));
    assert!(DEFERRED_GATE_CLASSES[0].contains("DB-04..DB-17"));
    cleanup(db, path);
}

#[test]
fn consumer_handoff_ledger_is_all_unmet() {
    // The self-test: the state cannot drift silently, because any entry that is not U fails
    // here, and the status token is the subplan's checklist letter.
    for entry in CONSUMER_HANDOFF.iter() {
        assert_eq!(entry.status, ConsumerAcceptance::Unmet, "{}", entry.id);
        assert_eq!(entry.status.status_str(), "U", "{}", entry.id);
        assert!(
            !entry.pending_gate.is_empty(),
            "{} has no recorded gate",
            entry.id
        );
        assert!(
            !entry.owner.is_empty(),
            "{} has no recorded owner",
            entry.id
        );
    }
    let ids: Vec<&str> = CONSUMER_HANDOFF.iter().map(|e| e.id).collect();
    assert_eq!(
        ids,
        vec![
            "SC.mapping-bridge",
            "SC.phase1-fixture",
            "BOOT.configured-library-caller",
            "production_full_finalize_db_contract",
            "db_internal_contract",
        ]
    );
    assert_eq!(
        CONSUMER_HANDOFF
            .iter()
            .filter(|e| e.status == ConsumerAcceptance::Unmet)
            .count(),
        5
    );
    // The pin line itself is part of the deliverable, so it must be reachable.
    assert!(LEDGER_PIN_LINE.contains("DB-21(P1-DB)"));
    assert!(LEDGER_PIN_LINE.contains("NOT #27A"));

    // Each entry's gate must name the IMPLEMENTING package, not merely an approval gate: a
    // scheduler reading this ledger must not mistake "dependency delta approved" for "caller
    // delivered".
    let boot = CONSUMER_HANDOFF
        .iter()
        .find(|e| e.id == "BOOT.configured-library-caller")
        .expect("BOOT entry present");
    assert!(
        boot.pending_gate
            .contains("BOOT-01 is the IMPLEMENTING package"),
        "{}",
        boot.pending_gate
    );
    assert!(
        boot.pending_gate
            .contains("BOOT-00 approves construction/dependency deltas only"),
        "BOOT-00 must be recorded as a prerequisite, not as the missing caller: {}",
        boot.pending_gate
    );
    let fixture = CONSUMER_HANDOFF
        .iter()
        .find(|e| e.id == "SC.phase1-fixture")
        .expect("SC fixture entry present");
    assert!(
        fixture
            .pending_gate
            .contains("nd-scanner manifest delta adding an nd-db"),
        "{}",
        fixture.pending_gate
    );
    let full = CONSUMER_HANDOFF
        .iter()
        .find(|e| e.id == "production_full_finalize_db_contract")
        .expect("full-exit entry present");
    assert!(
        full.pending_gate.contains("DB-16..DB-20 landed")
            && full.pending_gate.contains("FULL-SCANNER-DB exit")
            && full.pending_gate.contains("this P1-DB package"),
        "{}",
        full.pending_gate
    );
}
