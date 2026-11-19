//! S-D3 shared hydration core: the row → entity-record seam for the eight Go
//! `PostScan` hooks (`docs/rust-port/hydrate-scan-contract.md`, S-D3 brief).
//!
//! ## Two-phase shape (Go's `Scan` → `PostScan`)
//!
//! Go's dbx first scans the raw row columns into the `db*` struct and then
//! runs the entity's `PostScan` hook to decode the JSON shim columns
//! (`dbx@v1.12.0/rows.go:92-100` calls the hook after `Scan`, and
//! `dbUser.PostScan` is `persistence/user_repository.go:36-43`). The port mirrors that split
//! exactly, following the landed `UserRow{record, libraries_json}` →
//! `UserRow::into_record` precedent (`user_repo.rs`):
//!
//! 1. a **raw row** fn — [`row_to_user`] — reads the plain columns dbx scans
//!    and hands back a raw struct ([`crate::user_repo::UserRow`]) with the
//!    JSON shim column still unparsed;
//! 2. a **decode** fn — [`user_post_scan`] — applies the entity's hook over
//!    the raw payload and produces the hydrated record.
//!
//! Later sub-slices (S-D3b/c/d/e: folder, playlist, album, mediafile,
//! artist, playlist_track, scrobble_buffer) add their own raw-row + decode
//! pairs here on top of the shared [`RowGet`] accessor and the landed
//! `sql_json`/`sql_tags`/`sql_participations` codecs. S-D3b landed the folder
//! pair ([`row_to_folder`] → [`folder_post_scan`], which decodes the
//! `image_files` shim and carries the INNER-joined `library_path`) and the
//! playlist pair ([`row_to_playlist`] → [`playlist_post_scan`], which decodes
//! the `rules` shim and the LEFT-joined annotation columns). S-D3d landed the
//! artist pair ([`row_to_artist`] → [`artist_post_scan`], `:43-98`), whose
//! second phase folds the `JSON_GROUP_OBJECT` library-stats alias into
//! `Stats`/`Size`/`AlbumCount`/`SongCount` and decodes `similar_artists`.
//! S-D3e landed the two **delegating** pairs ([`row_to_playlist_track`] →
//! [`playlist_track_post_scan`], [`row_to_scrobble_buffer`] →
//! [`scrobble_buffer_post_scan`]). Neither owns a shim column: both Go structs
//! embed `dbMediaFile` and delegate the entire decode to `dbMediaFile.PostScan`
//! (`playlist_track_repository.go:27-34`, `scrobble_buffer_repository.go:23-30`),
//! then overwrite `MediaFile.ID = MediaFileID` — the single behavior of
//! [`apply_embedded_mediafile_id`]. Phase 1 of both readers shares
//! `mediafile_record_from_row` with [`row_to_mediafile`], so the media-file
//! column set, its read order and its NULL behavior cannot drift between them.
//!
//! ## The embedded-media-file `id` collision (S-D3e's signature behavior)
//!
//! `dbPlaylistTrack` embeds `dbMediaFile` (which embeds `*model.MediaFile`) and
//! `*model.PlaylistTrack`; `dbScrobbleBuffer` likewise embeds
//! `*model.ScrobbleEntry`. A result column named `id` therefore names two
//! candidate fields: `ID` through `dbMediaFile` at reflect path `[0,0,0]`
//! (length 3) and `ID` through the outer entity at `[1,0]` (length 2). dbx keeps
//! the SHALLOWEST path (`dbx@v1.12.0/struct.go:203-207`:
//! `!ok || len(path2) < len(si.nameMap[fi.name].path)`), so the **outer entity's**
//! `ID` wins the `id` mapping — the playlist position, or the scrobble entry id —
//! and `MediaFile.ID` is bound by NOTHING at all. An executed dbx probe confirms
//! it (executed against dbx v1.12.0 over real SQLite): a
//! `dbPlaylistTrack`-shaped mirror over `SELECT mf.*, pt.*` leaves
//! `MediaFile.ID == ""` while `Track.ID == "1"`; over `SELECT mf.id, pt.id` the
//! outer field takes the LAST column's value, and reversing the two columns
//! flips it — when several result columns share a name dbx maps every one of
//! them to the SAME winning field (`rows.go:80-86` builds one ref per column from
//! `dbNameMap`), so the LAST column scanned wins, because `sql.Rows.Scan`
//! converts in emitted order. That is exactly why Go's
//! hooks assign `MediaFile.ID = MediaFileID` (`:32`, `:28`) and why scrobble's
//! select puts `s.*` last (`scrobble_buffer_repository.go:66-67`: "Put `s.*` last
//! or else `m.id` overrides `s.id`").
//!
//! The port reproduces both rules with [`id_field_from_row`], which walks the
//! columns named `id` in emitted order — the same order `sql.Rows.Scan` converts
//! them in — and stops at the first NULL, which is where Go's scan aborts. The SQL
//! statements stay byte-identical to Go's. rusqlite's own name lookup resolves a
//! duplicate to the FIRST index (`rusqlite-0.32.1/src/column.rs:127-139`), so
//! `get_text("id")` alone would take `m.id` here — the trap this helper exists to
//! avoid, pinned by tests in both column orders.
//!
//! Two further Go-only consequences are recorded, not emulated. dbx's `indirect`
//! allocates `dbMediaFile.MediaFile` mid-scan (`struct.go:251-259`), so the
//! embedded pointer is non-nil even when a joined leg came back NULL. It never
//! gets to carry anything, though: `ScanStruct` returns the `Scan` failure BEFORE
//! the PostScanner check (`dbx@v1.12.0/rows.go:88-90` returns `r.Scan(refs...)`'s
//! error; the hook at `:92-102` is reached only once the scan succeeded), so a row
//! with NULL media-file legs never runs the copy and never reaches
//! `MediaFile.ID = MediaFileID`. Executed against the real
//! `scrobbleBufferRepository.Next`, such a row surfaces the `Scan` error itself —
//! `sql: Scan error on column index 0, name "id": converting NULL to string is
//! unsupported` — and no record is produced. In Rust the media-file columns are
//! required, so a NULL leg fails phase 1 the same way: the caller sees no entry
//! plus an error, as pinned at [`playlist_track_post_scan`] and
//! [`scrobble_buffer_post_scan`].
//!
//! A raw row for these entities also carries a **join-only** column that the
//! record keeps but the write map must not: `FolderRecord::library_path`
//! (Go `structs:"-"`, `model/folder.go:21`) and the annotation embed on
//! `PlaylistRecord` (Go `structs:"-"`, `model/playlist.go:21`). Neither is
//! emitted by [`crate::folder_repo::folder_row`] or
//! [`crate::playlist_repo::playlist_row`], so the write SQL is unchanged. Each
//! record also keeps its row's own `id` (`Folder.ID`/`Playlist.ID` are scanned by
//! name), which is what makes a bulk read carry per-row identity; `id` likewise
//! stays out of the write map. Non-pointer timestamp columns use
//! [`RowGet::get_dt`], which errors on NULL *only*: the pinned driver
//! (`go-sqlite3@v1.14.52/sqlite3.go:2831-2838`) swallows ANY DATETIME parse
//! failure — the DDL's `'0000-00-00 00:00:00'` default included — and hands back
//! the zero `time.Time` with no error, so a non-NULL unparseable cell scans as
//! [`go_zero_time`], never as a failure, and [`is_go_zero`] is Go's `IsZero()`.
//! A record that spells "never stamped" as `None` maps Go's zero back to `None`.
//!
//! ## The dbx scan contract (why field names, not `structs` tags, decide)
//!
//! `Rows.ScanStruct` (`dbx@v1.12.0/rows.go:60-85`) maps columns to fields
//! through `DefaultFieldMapFunc` — snake-cased FIELD NAMES — and Navidrome sets
//! no `FieldMapper` (`persistence/persistence.go:71` / `:171`, no override
//! anywhere in the tree). The `structs` tags drive only the WRITE map
//! (`structs.Map`, `persistence/helpers.go:21`); they are invisible to the scan,
//! and the only scan-relevant tag is `db` (`dbx@v1.12.0/struct.go:48`). Two
//! consequences the hydrators must honour:
//!
//! * `Folder.UpdateAt` (`model/folder.go:32`) snake-cases to `update_at`, which
//!   no SELECT emits, so the `updated_at` column is discarded into the throwaway
//!   `sql.NullString` (`rows.go:84`) and Go's `Folder.UpdateAt` stays the zero
//!   time. [`row_to_folder`] therefore hydrates Go's zero
//!   ([`go_zero_time`]), NOT the column — verified against the pinned dbx.
//!   The write side is unaffected: `structs:"updated_at"` still emits
//!   `updated_at`.
//! * `Playlist.OwnerName` (`structs:"-"`, `model/playlist.go:26`) snake-cases to
//!   `owner_name`, which `selectPlaylist` DOES emit (`playlist_repository.go:259`),
//!   so Go scans it — the `structs:"-"` tag does not block the scan. The same
//!   rule covers the `Annotations`/`ItemImage` embeds (`structs:"-"`,
//!   `model/playlist.go:21-22`): anonymous structs are descended into and their
//!   fields mapped by name (`struct.go:200-210`), which is why the annotation
//!   columns are read at all. [`row_to_playlist`] hydrates `owner_name` onto the
//!   record and both stay out of the write map.
//!
//! ## NULL parity (Go `database/sql`)
//!
//! NULL into a **non-pointer** numeric/string/time field **errors**; NULL
//! into a pointer yields nil. [`RowGet`] enforces the same: the bare
//! `get_*` getters error on NULL, the `get_opt_*` getters yield `None`.
//! A timestamp that cannot be parsed is NOT a failure in either case — see
//! [`go_zero_time`] above (`sqlite3.go:2831-2838`), and a pointer column gets a
//! pointer to that zero value, which is why `*time.Time` JSON still renders
//! `0001-01-01T00:00:00Z` rather than being omitted. On the WIRE a stamp is
//! Go's `RFC3339Nano` text ([`to_go_time_text`]), never the SQLite storage
//! spelling — the annotation DTO's strings are model data, not column bytes.
//!
//! ## Absent-column tolerance (the two corrections)
//!
//! Base reads JOIN `annotation` (and the mediafile base read also JOINs
//! `bookmark`), but the *light* queries (e.g. scrobble-buffer's `m.*` on a
//! missing media file) do not emit those columns, and dbx discards
//! unmatched columns into a throwaway `NullString`
//! (`dbx@v1.12.0/rows.go:80-86`, the `else` branch at `:84`). A ported hydrator must therefore treat
//! optional (joined) columns as **absent** on light queries — reading them
//! with `get_opt_*` yields `None`-free access **only when the column is
//! present**; when it is *absent* the accessor must not panic. Use
//! [`RowGet::has_column`] (probing with the accessor's own
//! missing-column error) to branch: absent optional columns decode to
//! their zero/None value.
//! Required (base-table) columns may use the bare getters unconditionally.
//!
//! ## Errors
//!
//! [`HydrateError::Decoded`] carries Go's EXACT error prefix verbatim
//! (e.g. `"parsing user libraries from db: "`, brief §4c). Callers must
//! depend only on the prefix: the suffix is the decode failure, and its
//! wording comes from `serde_json`/`encoding-json`-style rendering, which
//! is allowed to differ from Go's `encoding/json` byte-for-byte.
//! No Go prefix is dropped or renamed. One addition is deliberate:
//! [`crate::sql_json::JsonColumnError`] renders `{column}: {msg}` — executed:
//! `parsing album discs from db: discs: key must be a string at line 1 column 2`
//! where Go says `parsing album discs from db: invalid character 'b' looking for
//! beginning of object key string`. The token names the failing column, which Go's
//! message does not, and it is emitted ONLY by the three `sql_json` codecs
//! (`discs`, `folder_ids`, `image_files`); callers must depend on the prefix only.
//! The two codecs with their own Go inner prefix emit no token at all, via
//! `JsonColumnError::Bare`: `parsing participants:`
//! (`sql_participations.go:61-66`) and `parsing tags:` (`sql_tags.go:26`), both
//! reproduced verbatim, so neither is port noise a caller may strip.
//!
//! Scan-side messages are Go-shaped but deliberately NOT byte-identical. Go's
//! driver returns the bare `convert.go` text and `database/sql` then wraps it as
//! `sql: Scan error on column index %d, name %q: %w` (`sql.go:3441, :3450`);
//! rusqlite surfaces the wrapped payload's own `Display`, so there is no seam in
//! which to inject that wrapper. The four Go-shaped scan payloads in this module
//! — `NullRequired`, `UnsupportedScan`, `GoToInt64` and `GoToFloat64` — therefore
//! name the column themselves: `UnsupportedScan`, `GoToInt64` and `GoToFloat64`
//! reproduce Go's inner message and append an " (column `X`)" suffix carrying the
//! identity Go's wrapper supplied, while `NullRequired` reports the failure Go's
//! `converting NULL to %s is unsupported` arms produce — `database/sql` returns
//! that text from four arms, int/uint/float/string at convert.go:499, :511,
//! :523, :535 — but with its OWN wording (`NULL value in required column …`)
//! plus the column name, since rusqlite's `Type` carries no Go kind to
//! interpolate. `GoToBool` reproduces `driver.Bool`'s rejection text verbatim
//! (`driver/types.go:75` string, `:81` []byte, `:93` int, `:99` uint, `:102`
//! everything else including `float64`), except for a NULL cell, where Go's
//! converter says `couldn't convert <nil> (<nil>) into type bool` and the port
//! keeps the shared NULL payload. Executed tests therefore assert Go's inner
//! message as a CONTAINED substring where Rust reproduces it, and assert
//! `NullRequired` by its own text plus the column name — never by equality with
//! Go's fully wrapped string.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Timelike, Utc};
use rusqlite::types::Type;
use rusqlite::{Error as SqliteError, Result as SqliteResult, Row};

use nd_model::annotation::Annotation;
use nd_model::criteria::unmarshal_criteria_str;
use nd_model::fields::default_field_map;

use crate::album_repo::{participant_names_from_participants, AlbumRecord, AlbumRow};
use crate::artist_repo::{ArtistRecord, ArtistRow, SimilarArtist, TOTAL_ROLE};
use crate::folder_repo::{FolderRecord, FolderRow};
use crate::mediafile_repo::{MediaFileRecord, MediaFileRow};
use crate::playlist_repo::{PlaylistRecord, PlaylistRow};
use crate::playlist_track_repo::{PlaylistTrackRecord, PlaylistTrackRow};
use crate::scrobble_buffer_repo::{ScrobbleBufferRow, ScrobbleEntryRecord};
use crate::sql_json::{unmarshal_discs, unmarshal_image_files, unmarshal_nullable_strings};
use crate::sql_participations::unmarshal_participants;
use crate::sql_participations::Role;
use crate::sql_tags::{tags_to_genres, unmarshal_tags};
use crate::user_repo::{UserLibrary, UserRecord, UserRow};
use serde_json::from_str;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::str::FromStr;

/// Go's `dbUser.PostScan` wrapper, verbatim
/// (`persistence/user_repository.go:38`).
pub const USER_LIBRARIES_PREFIX: &str = "parsing user libraries from db: ";

/// Go's `dbFolder.PostScan` wrapper, verbatim
/// (`persistence/folder_repository.go:42`).
pub const FOLDER_IMAGE_FILES_PREFIX: &str = "parsing folder image files from db: ";

/// Go's `dbAlbum.PostScan` discs wrapper, verbatim
/// (`persistence/album_repository.go:45`).
pub const ALBUM_DISCS_PREFIX: &str = "parsing album discs from db: ";

/// Go's `dbAlbum.PostScan` wrapper for the `participants` and `tags` arms,
/// verbatim (`persistence/album_repository.go:50` and `:55`). Both arms use the
/// same sentence, so one constant covers them.
pub const ALBUM_PREFIX: &str = "parsing album from db: ";

/// Go's `dbAlbum.PostScan` folder_ids wrapper, verbatim
/// (`persistence/album_repository.go:62`).
pub const ALBUM_FOLDER_IDS_PREFIX: &str = "parsing album folder_ids from db: ";

/// Go's `dbMediaFile.PostScan` wrapper, verbatim — the same sentence for both the
/// `participants` and the `tags` arm
/// (`persistence/mediafile_repository.go:57` and `:62`).
pub const MEDIAFILE_PREFIX: &str = "parsing media_file from db: ";

/// Go's `dbArtist.PostScan` stats wrapper, verbatim INCLUDING the trailing
/// space (`fmt.Errorf("parsing artist stats from db: %w", err)`,
/// `persistence/artist_repository.go:49`).
pub const ARTIST_STATS_PREFIX: &str = "parsing artist stats from db: ";

/// Go's `dbArtist.PostScan` similar-artists wrapper, verbatim INCLUDING the
/// trailing space (`fmt.Errorf("parsing similar artists from db: %w", err)`,
/// `persistence/artist_repository.go:89`).
pub const ARTIST_SIMILAR_PREFIX: &str = "parsing similar artists from db: ";

/// Go's zero `time.Time` (`time.Time{}`), the value the pinned driver returns
/// for any DATETIME text it cannot parse
/// (`go-sqlite3@v1.14.52/sqlite3.go:2822-2838`: the `SQLiteTimestampFormats`
/// loop leaves `err` set and the arm assigns `t = time.Time{}` with no error).
///
/// chrono's `NaiveDate::MIN` is chrono's own floor (year **-262143**), not Go's
/// zero, so the year is named explicitly: Go's `time.Time{}` is January 1st of
/// year 1 at midnight, which `encoding/json` renders as `0001-01-01T00:00:00Z`.
#[must_use]
pub fn go_zero_time() -> DateTime<Utc> {
    DateTime::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(1, 1, 1)
            .unwrap_or(NaiveDate::MIN)
            .and_hms_opt(0, 0, 0)
            .unwrap_or_default(),
        Utc,
    )
}

/// `true` for Go's zero `time.Time` — the port's spelling of Go's `t.IsZero()`.
#[must_use]
pub fn is_go_zero(dt: &DateTime<Utc>) -> bool {
    *dt == go_zero_time()
}

/// chrono pattern for Go's `time.RFC3339Nano` in UTC (`T`-separated, `Z`
/// suffix), the layout `encoding/json` renders a `time.Time` with — the shape
/// the annotation timestamp strings on the
/// [`nd_model::annotation::Annotation`] DTO must carry. The nanosecond field is
/// printed in full and trimmed by [`to_go_time_text`], because Go drops trailing
/// zeros and chrono does not.
pub const GO_RFC3339_NANO: &str = "%Y-%m-%dT%H:%M:%S%.9fZ";

// The playlist `rules` decode carries **no** Go wrapper: `dbPlaylist.PostScan`
// (`persistence/playlist_repository.go:29-34`) returns the `json.Unmarshal`
// error bare, so [`playlist_post_scan`] surfaces the codec's own message
// un-prefixed (Go's own `criteria.UnmarshalJSON` has no wrapper either). No
// prefix constant is defined for it, and none must be invented.

/// Failures of the shared hydration seam.
#[derive(Debug, thiserror::Error)]
pub enum HydrateError {
    /// A JSON shim column decoded with an error, carrying Go's EXACT
    /// wrapper prefix verbatim (e.g. `"parsing user libraries from db: …"`,
    /// `USER_LIBRARIES_PREFIX`).
    ///
    /// Depend only on the prefix: the suffix is the underlying decode
    /// failure and its exact wording is `serde_json`'s, not Go's
    /// `encoding/json`'s.
    #[error("{0}")]
    Decoded(String),

    /// The row read itself failed (statement, conversion, NULL into a
    /// required column, missing required column).
    #[error(transparent)]
    Sqlite(#[from] SqliteError),
}

/// The boxed error payload for a decode failure surfaced as a sqlite
/// conversion failure (see [`HydrateError::into_sqlite`]).
#[derive(Debug)]
struct DecodedPayload(String);

impl std::fmt::Display for DecodedPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DecodedPayload {}

impl HydrateError {
    /// The seam's failure as a `rusqlite` conversion failure: a
    /// `FromSqlConversionFailure` whose payload IS the Go-prefixed message.
    ///
    /// This is a REPRESENTATION choice, not Go parity. Go's `ScanStruct` returns a
    /// `PostScan` failure verbatim (`dbx@v1.12.0/rows.go:92-102` — the hook's `err`
    /// is returned unchanged, not wrapped in `sql.ScanError`), so Go's text reaches
    /// the caller unwrapped. The crate has no opaque error slot in its read
    /// `Result`, so the seam's decode failure rides SQLite's conversion-failure
    /// variant to land in the same repo error variant Go's failure would land in;
    /// the message text and its Go prefix are preserved, only the wrapper differs.
    #[must_use]
    pub fn into_sqlite(self) -> SqliteError {
        match self {
            Self::Sqlite(e) => e,
            Self::Decoded(msg) => {
                SqliteError::FromSqlConversionFailure(0, Type::Text, Box::new(DecodedPayload(msg)))
            }
        }
    }
}

/// The shared column accessor over a raw row (brief §4a).
///
/// NULL semantics follow Go (`database/sql/convert.go`): the bare getters
/// **error** on NULL; the `opt_*` getters yield `None`.
///
/// Absent-column rule: joined/optional columns may be missing on light
/// queries (brief §2 corrections). Callers probe with [`Self::has_column`]
/// (or the accessor's own missing-column error) and fall back to the
/// zero/None value — a missing *optional* column is data, not a bug.
/// Missing *required* (base-table) columns surface as `InvalidColumnName`
/// errors.
pub trait RowGet {
    /// A column Go scans into a non-pointer `string`: **SQL NULL errors** (Go
    /// parity), as does an absent column; every other storage class converts
    /// through Go's `asString` (see [`RowGet::get_opt_text`]).
    fn get_text(&self, column: &str) -> SqliteResult<String>;

    /// A coalesce'd INTEGER `bool` column (Go's `bool` non-pointer):
    /// **NULL errors**. A column declared `boolean` follows the driver's own
    /// `val > 0` rule instead (see [`RowGet::get_bool`]).
    fn get_bool(&self, column: &str) -> SqliteResult<bool>;

    /// A non-pointer `int` column: **NULL errors**.
    fn get_i32(&self, column: &str) -> SqliteResult<i32>;

    /// A non-pointer `int64` column: **NULL errors**.
    fn get_i64(&self, column: &str) -> SqliteResult<i64>;

    /// A non-pointer `real` column: **NULL errors**. Non-REAL cells go through
    /// Go's `asString` + `strconv.ParseFloat` (see [`RowGet::get_f64`]).
    fn get_f64(&self, column: &str) -> SqliteResult<f64>;

    /// A `*string` column: SQL NULL → `None`; INTEGER, REAL and BLOB cells are
    /// rendered by Go's `asString`, so a non-TEXT storage class is text rather
    /// than a scan error (`database/sql/convert.go:533-544`).
    fn get_opt_text(&self, column: &str) -> SqliteResult<Option<String>>;

    /// A `*float64` column (the `Rg*` shims, `average_rating`): SQL NULL →
    /// `None`, every other cell through Go's float arm (and its errors) — see
    /// [`RowGet::get_f64`].
    fn get_opt_f64(&self, column: &str) -> SqliteResult<Option<f64>>;

    /// A Go `*int` column (`bit_depth`, `bpm`): SQL NULL → `None` (Go's nil
    /// pointer); every other cell goes through the SAME int64 arm as
    /// [`RowGet::get_i64`] — TEXT `"16"` and a UTF-8 BLOB scan, an unparseable
    /// cell reports Go's error — then narrows to the record's `i32`. rusqlite's
    /// bare `Option<i32>` is INTEGER-only, so it must not be used here.
    fn get_opt_i32(&self, column: &str) -> SqliteResult<Option<i32>>;

    /// A `*time.Time` column: SQL NULL → `None` (Go's nil pointer,
    /// `database/sql/convert.go:491-493`); text the driver cannot parse →
    /// `Some(`[`go_zero_time`]`)`, a POINTER TO THE ZERO VALUE, because the
    /// driver's parse-failure arm hands back `time.Time{}` with no error
    /// (`go-sqlite3@v1.14.52/sqlite3.go:2831-2838`) and `database/sql` then
    /// allocates a fresh `time.Time` for it (`convert.go:495-496`). That is why
    /// Go's JSON for such a field still renders `0001-01-01T00:00:00Z` instead
    /// of being dropped by `omitempty`.
    ///
    /// Both arms are keyed on the column's DECLARED type
    /// (`sqlite3.go:2782`/`:2822`): an INTEGER cell in a declared
    /// `datetime`/`timestamp`/`date` column is NOT a conversion failure either
    /// (see [`driver_integer_dt`]), and TEXT in any OTHER declared type — or in
    /// an expression column, which has no decltype — is a scan error rather than
    /// a parse (see [`RowGet::get_gotime`]).
    fn get_opt_dt(&self, column: &str) -> SqliteResult<Option<DateTime<Utc>>>;

    /// A non-pointer `time.Time` column: **SQL NULL errors** (Go parity —
    /// `convert.go` cannot store `driver.Value` nil into `*time.Time`), but a
    /// non-NULL value the driver cannot parse scans as [`go_zero_time`] with no
    /// error, exactly as Go's DATETIME arm does (`sqlite3.go:2831-2838`). The
    /// DDL's [`crate::time::ZERO_DATE_SENTINEL`] default therefore scans, it does
    /// not fail the read — and only in a DECLARED timestamp column, since the
    /// arm is keyed on the declared type (`sqlite3.go:2822`).
    fn get_dt(&self, column: &str) -> SqliteResult<DateTime<Utc>>;

    /// The same cell as Go's `time.Time` INCLUDING the zone the driver parsed it
    /// with — the shape an annotation timestamp's wire text needs, which a
    /// `DateTime<Utc>` cannot carry (see [`GoTime`]). NULL → `None`; a value the
    /// driver cannot decode → `Some(`[`go_zero_gotime`]`)`; a cell whose driver
    /// type cannot be stored into `time.Time` at all → an error, which is every
    /// REAL/BLOB cell, every INTEGER outside a declared timestamp column, and
    /// every TEXT outside a declared timestamp column — including an expression
    /// column, which has no declared type at all.
    fn get_gotime(&self, column: &str) -> SqliteResult<Option<GoTime>>;

    /// `true` when the row's SELECT emits `column`. The
    /// absent-optional-column rule: joined columns (`annotation`,
    /// `bookmark`, …) may be absent on light queries. Probing uses the
    /// accessor's own missing-column error, so no statement handle is
    /// needed.
    fn has_column(&self, column: &str) -> bool;

    /// A Go non-pointer `string` read from the column at `index` — the same
    /// conversion arms as [`RowGet::get_text`], addressed by position instead of
    /// name. Used for the duplicated-name columns of the S-D3e projections.
    fn get_text_at(&self, index: usize) -> SqliteResult<String>;
}

impl RowGet for Row<'_> {
    fn get_text(&self, column: &str) -> SqliteResult<String> {
        match self.get_opt_text(column)? {
            Some(value) => Ok(value),
            // Go: NULL into a non-pointer string errors.
            None => Err(null_required(column, Type::Text)),
        }
    }

    fn get_bool(&self, column: &str) -> SqliteResult<bool> {
        // Go scans a non-pointer `bool` through `driver.Bool.ConvertValue`
        // (`database/sql/convert.go:441-446` → `driver/types.go:68-102`), NOT
        // through rusqlite's INTEGER-only cast. One branch precedes it: the
        // driver's INTEGER arm for a column declared `boolean`
        // (`go-sqlite3@v1.14.52/sqlite3.go:2797-2798`, `dest[i] = val > 0` after
        // lowercasing the decltype) hands back a Go `bool`, which the converter
        // returns unchanged — so `2` is TRUE and `-3` FALSE there (`val > 0`),
        // while the same values in a column declared `bool` (not one of the
        // driver's names) are rejected as `couldn't convert 2 into type bool`
        // (verified against the pinned driver, both decltypes). A TEXT/BLOB cell
        // is not the INTEGER arm, so it still goes through `strconv.ParseBool`;
        // an INTEGER elsewhere is accepted only as 0 or 1, and a REAL errors.
        match self.get_ref(column)? {
            // Go's converter reports `couldn't convert <nil> (<nil>) into type
            // bool`. The port keeps the shared NULL payload for every required
            // getter, per the module-level note on scan wording.
            rusqlite::types::ValueRef::Null => Err(null_required(column, Type::Integer)),
            rusqlite::types::ValueRef::Integer(value) => match driver_bool_decl(self, column) {
                // `dest[i] = val > 0`, so a negative cell is FALSE, not TRUE.
                true => Ok(value > 0),
                false => match value {
                    0 => Ok(false),
                    1 => Ok(true),
                    other => Err(go_bool_error(column, format!("{other}"))),
                },
            },
            rusqlite::types::ValueRef::Real(value) => Err(go_bool_error(
                column,
                format!("{} (float64)", crate::criteria_sql::go_fmt_float64(value)),
            )),
            rusqlite::types::ValueRef::Text(bytes) | rusqlite::types::ValueRef::Blob(bytes) => {
                go_parse_bool(column, bytes)
            }
        }
    }

    fn get_i32(&self, column: &str) -> SqliteResult<i32> {
        // Same arms as `get_i64`; Go's `int` is 64-bit so a wider cell scans
        // there and is truncated by the model's 32-bit field, while the port's
        // `i32` field reports out-of-range (documented residual).
        go_scan_int64(self, column).and_then(narrow_i32)
    }

    fn get_i64(&self, column: &str) -> SqliteResult<i64> {
        go_scan_int64(self, column)
    }

    fn get_f64(&self, column: &str) -> SqliteResult<f64> {
        // Go's float arm (`convert.go:521-532`): NULL errors, a REAL scans
        // directly, and INTEGER/TEXT/BLOB are rendered by `asString` and run
        // through `strconv.ParseFloat` — so TEXT `"0.5"` succeeds, TEXT `"1e400"`
        // reports `value out of range`, and a UTF-8 BLOB such as `"1.5"` scans
        // because `asString` turns `[]uint8` into a Go `string`.
        match self.get_ref(column)? {
            rusqlite::types::ValueRef::Null => Err(null_required(column, Type::Real)),
            rusqlite::types::ValueRef::Integer(value) => {
                go_parse_float64(column, "int64", value.to_string().as_bytes())
            }
            rusqlite::types::ValueRef::Real(value) => Ok(value),
            rusqlite::types::ValueRef::Text(bytes) => go_parse_float64(column, "string", bytes),
            rusqlite::types::ValueRef::Blob(bytes) => go_parse_float64(column, "[]uint8", bytes),
        }
    }

    fn get_opt_text(&self, column: &str) -> SqliteResult<Option<String>> {
        // Go's `reflect.String` arm (`convert.go:533-544`) accepts EVERY
        // non-NULL storage class: it renders the driver value through `asString`
        // (`convert.go:557-578`) and stores it, so INTEGER `7` becomes `"7"`, a
        // REAL becomes its `%g` spelling, and a BLOB becomes a Go `string`
        // holding the RAW bytes. rusqlite's `FromSql for String` is TEXT-only
        // (`rusqlite-0.32.1/src/types/from_sql.rs:172-177`), so the arms are
        // spelled out here instead of delegating to `self.get`.
        match self.get_ref(column)? {
            // Go: NULL into a non-pointer `string` errors; a pointer field
            // stays nil.
            rusqlite::types::ValueRef::Null => Ok(None),
            rusqlite::types::ValueRef::Integer(value) => Ok(Some(value.to_string())),
            rusqlite::types::ValueRef::Real(value) => {
                Ok(Some(crate::criteria_sql::go_fmt_float64(value)))
            }
            // Go decodes malformed UTF-8 one byte at a time before JSON
            // marshaling (encoding/json/encode.go:1037-1044), so the per-byte
            // replacement here yields byte-identical wire text even though Go's
            // in-memory `string` keeps the raw bytes.
            rusqlite::types::ValueRef::Text(bytes) | rusqlite::types::ValueRef::Blob(bytes) => {
                Ok(Some(go_string_from_bytes(bytes)))
            }
        }
    }

    fn get_opt_f64(&self, column: &str) -> SqliteResult<Option<f64>> {
        // Go's `*float64` dest (`convert.go:491-493`): SQL NULL leaves the
        // pointer nil, and a non-NULL cell goes through the same float arm as
        // [`RowGet::get_f64`] — including its failures. Verified against the
        // pinned driver: a BLOB holding invalid UTF-8 and TEXT `"1e400"` both
        // return an error with a `*float64` dest — invalid syntax and value out
        // of range — and dbx propagates it (`dbx@v1.12.0/rows.go:88-91`), so
        // failures are NOT swallowed.
        match self.get_ref(column)? {
            rusqlite::types::ValueRef::Null => Ok(None),
            _ => self.get_f64(column).map(Some),
        }
    }

    fn get_opt_i32(&self, column: &str) -> SqliteResult<Option<i32>> {
        // Go's `*int` dest: SQL NULL leaves the pointer nil (`convert.go:491-493`),
        // and a non-NULL cell runs the same int arm as `get_i64`
        // (`convert.go:497-508`) — so TEXT `"16"` and a BLOB holding `"16"` scan
        // as 16, while `"abc"` reports Go's conversion error rather than rusqlite's
        // `InvalidColumnType`. The record field is `i32`, so a cell outside that
        // range is out-of-range like `get_i32` (documented residual).
        match self.get_ref(column)? {
            rusqlite::types::ValueRef::Null => Ok(None),
            _ => go_scan_int64(self, column).and_then(narrow_i32).map(Some),
        }
    }

    fn get_opt_dt(&self, column: &str) -> SqliteResult<Option<DateTime<Utc>>> {
        // SQL NULL → None (nil pointer); unparseable text → Some(Go's zero),
        // NOT an error and NOT None — the driver never fails a DATETIME parse
        // and `database/sql` allocates a pointer for the value it gets back.
        Ok(self.get_gotime(column)?.map(|t| t.to_utc()))
    }

    fn get_dt(&self, column: &str) -> SqliteResult<DateTime<Utc>> {
        // Non-pointer Go `time.Time`: SQL NULL errors like every bare getter
        // (`convert.go` cannot store nil into `*time.Time`), and any non-NULL
        // value — the DDL sentinel included — scans, undecodable TEXT as Go's
        // zero time.
        match self.get_gotime(column)? {
            None => Err(null_required(column, Type::Text)),
            Some(t) => Ok(t.to_utc()),
        }
    }

    fn get_gotime(&self, column: &str) -> SqliteResult<Option<GoTime>> {
        // The driver switches on the STORAGE type first (`sqlite3.go:2778`,
        // `:2813`) and only then on the column's DECLARED type, so both have to be
        // inspected: an INTEGER in a `datetime` column is a timestamp, an INTEGER
        // in a plain `INTEGER` column is not.
        match self.get_ref(column)? {
            // `SQLITE_NULL` → `dest[i] = nil` (`:2812-2813`): a nil pointer for the
            // `opt` form, which the bare getter turns into its error.
            rusqlite::types::ValueRef::Null => Ok(None),
            rusqlite::types::ValueRef::Integer(number) => {
                if is_timestamp_decltype(column_decl_type(self, column).as_deref()) {
                    Ok(Some(driver_integer_dt(number)))
                } else {
                    // `database/sql` cannot store the driver's `int64` into a
                    // `time.Time`, so the scan fails rather than guessing.
                    Err(unsupported_scan(column, "int64"))
                }
            }
            rusqlite::types::ValueRef::Real(_) => Err(unsupported_scan(column, "float64")),
            rusqlite::types::ValueRef::Text(bytes) => {
                // The TEXT arm is likewise gated on the DECLARED type
                // (`sqlite3.go:2821-2841`): only `timestamp`/`datetime`/`date` run the
                // nine-format parse and hand back a `time.Time` (zero on parse
                // failure, no error, `:2831-2838`). Every OTHER declared type —
                // `TEXT`, `varchar(255)`, `datetime(3)`, and an expression column
                // such as `SELECT '…' AS dt` or `CAST(dt AS TEXT)`, which has no
                // decltype at all — falls to `default` (`:2839-2840`) and the driver
                // hands back a `string`, which `database/sql` cannot store into
                // `time.Time`: a scan error, not a silent parse
                // (`database/sql/convert.go`, verified against the pinned driver
                // on Go 1.27.4).
                if !is_timestamp_decltype(column_decl_type(self, column).as_deref()) {
                    return Err(unsupported_scan(column, "string"));
                }
                // C.GoStringN in the pinned driver preserves arbitrary bytes in
                // a Go string; time.ParseInLocation rejects invalid UTF-8 as a
                // parse error, and this DATETIME arm turns every parse error into
                // time.Time{} without returning a scan error.
                let Ok(text) = std::str::from_utf8(bytes) else {
                    return Ok(Some(go_zero_gotime()));
                };
                Ok(Some(driver_parse_text_dt(text)))
            }
            rusqlite::types::ValueRef::Blob(_) => Err(unsupported_scan(column, "[]uint8")),
        }
    }

    fn has_column(&self, column: &str) -> bool {
        // Row has no `column_index`; the accessor's own missing-column
        // error is the probe. `get_ref` needs no conversion, so a present
        // column never errors here.
        self.get_ref(column).is_ok()
    }

    fn get_text_at(&self, index: usize) -> SqliteResult<String> {
        // Same conversion arms as [`RowGet::get_text`], addressed by position. The
        // column NAME is still resolved so a failure carries Go's column identity
        // exactly like the name-addressed accessor does.
        let stmt = self.as_ref();
        match self.get_ref(index)? {
            // Go: NULL into a non-pointer string errors.
            rusqlite::types::ValueRef::Null => Err(null_required_at(
                stmt.column_name(index).unwrap_or("<unknown>"),
                index,
                Type::Text,
            )),
            rusqlite::types::ValueRef::Integer(value) => Ok(value.to_string()),
            rusqlite::types::ValueRef::Real(value) => {
                Ok(crate::criteria_sql::go_fmt_float64(value))
            }
            rusqlite::types::ValueRef::Text(bytes) | rusqlite::types::ValueRef::Blob(bytes) => {
                Ok(go_string_from_bytes(bytes))
            }
        }
    }
}

/// The index-addressed twin of [`null_required`]: the same Go-parity NULL
/// failure for a column the caller resolved by INDEX because its name is
/// duplicated in the projection (see [`id_field_from_row`], which walks every
/// column of that name rather than resolving one index — dbx binds ALL of them
/// to the same field, so a single-index read would miss the NULL that aborts
/// Go's scan). It keeps both halves of Go's identity — the name and the
/// position — because `sql: Scan error on column index N, name "X"` carries both.
fn null_required_at(column: &str, index: usize, ty: Type) -> SqliteError {
    SqliteError::FromSqlConversionFailure(
        0,
        ty,
        Box::new(NullRequired(format!("{column} (index {index})"))),
    )
}

/// The Go-parity message for a NULL cell scanned into a Go non-pointer `string`:
/// `database/sql/convert.go:535` reports exactly this sentence, and it is what
/// Go's S-D3e reads surface when a LEFT JOIN finds no `media_file` row (executed
/// against the real repositories: `sql: Scan error on column index 0, name "id":
/// converting NULL to string is unsupported`).
pub const NULL_TO_STRING: &str = "converting NULL to string is unsupported";

/// Payload of that failure. Like this module's other Go-shaped scan payloads it
/// carries the column identity Go's `sql: Scan error on column index N, name "X"`
/// wrapper supplied, because rusqlite surfaces only the payload's `Display` (see
/// the module-level note on scan wording).
#[derive(Debug)]
struct GoNullString {
    column: String,
    index: usize,
}

impl std::fmt::Display for GoNullString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{NULL_TO_STRING} (column `{}`, index {})",
            self.column, self.index
        )
    }
}

impl std::error::Error for GoNullString {}

/// Read the field dbx binds for a DUPLICATED column name — here `id` — the way
/// `sql.Rows.Scan` fills it. dbx maps EVERY result column of that name to the same
/// winning field (`dbx@v1.12.0/rows.go:80-86` looks each name up in one `dbNameMap`),
/// and `Scan` converts columns in emitted order, so:
///
/// * the LAST non-NULL cell of that name is the value that survives;
/// * the FIRST NULL cell aborts the whole scan with Go's
///   [`NULL_TO_STRING`] — even when a later column of the same name is non-NULL,
///   because conversion stops at the failing cell. That is exactly Go's
///   LEFT-JOIN-miss behavior, executed against the real
///   `scrobbleBufferRepository.Next`:
///   `sql: Scan error on column index 0, name "id": converting NULL to string is
///   unsupported` (index 0 is `m.id`, the first column of `m.*`, and the entry's own
///   `s.id` is never reached);
/// * a projection with NO column of that name leaves Go's zero value — dbx builds a
///   ref only for names its field map knows, and an unmatched column is scanned into
///   a throwaway `sql.NullString` (`rows.go:80-86`). Empty, not an error.
///
/// # Errors
/// Go's NULL-into-`string` payload naming the first NULL cell of that name.
pub fn id_field_from_row(row: &Row<'_>) -> SqliteResult<String> {
    let stmt = row.as_ref();
    let mut value = String::new();
    for index in 0..stmt.column_count() {
        if stmt.column_name(index)? != "id" {
            continue;
        }
        match row.get_ref(index)? {
            rusqlite::types::ValueRef::Null => {
                return Err(SqliteError::FromSqlConversionFailure(
                    0,
                    Type::Text,
                    Box::new(GoNullString {
                        column: stmt.column_name(index)?.to_string(),
                        index,
                    }),
                ));
            }
            // Go's `reflect.String` arm renders any non-NULL cell through `asString`
            // (`database/sql/convert.go:533-544`, `:557-578`), so an INTEGER position
            // column becomes its decimal spelling. `get_text_at` reproduces that.
            _ => value = row.get_text_at(index)?,
        }
    }
    Ok(value)
}

/// Convert a Go string's raw bytes to Rust text using encoding/json's per-invalid-byte
/// replacement behavior. `String::from_utf8_lossy` groups some truncated sequences.
fn go_string_from_bytes(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut offset = 0;
    while offset < bytes.len() {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(valid) => {
                out.push_str(valid);
                break;
            }
            Err(error) => {
                let valid_end = offset + error.valid_up_to();
                out.push_str(
                    std::str::from_utf8(&bytes[offset..valid_end])
                        .expect("valid_up_to identifies valid UTF-8"),
                );
                out.push('\u{FFFD}');
                offset = valid_end + 1;
            }
        }
    }
    out
}

/// Go's `%q`/`strconv.Quote` over a driver value's own bytes — the spelling
/// `database/sql` puts in `converting driver.Value type %T (%q) …`
/// (`convert.go:505`, and the same form at `:517` and `:529`). `asString` returns a
/// `string`/`[]uint8` unchanged (`:557-562`), so TEXT and BLOB cells quote over the
/// SAME raw bytes and a TEXT cell with invalid UTF-8 quotes identically to a BLOB.
/// Two things make this its own function rather than `escape_debug`:
/// * a byte that starts an invalid UTF-8 sequence is emitted as `\xNN`, keeping
///   the raw byte (`strconv/quote.go:42-46`), where Rust's `escape_debug`
///   substitutes `\u{fffd}`;
/// * printability is Go's `strconv.IsPrint` (`quote.go:518-559`), whose Latin-1
///   window `¡`–`ÿ` is printable except the soft hyphen `U+00AD`.
///
/// Above Latin-1 the oracle is Rust's `char::escape_debug` (see
/// [`go_is_print`]). This text only ever surfaces inside a scan-error message.
fn go_quote_bytes(bytes: &[u8]) -> String {
    let mut out = String::from("\"");
    let mut offset = 0;
    while offset < bytes.len() {
        let rest = &bytes[offset..];
        // Go decodes one rune at a time and emits `\xNN` for a width-1 invalid
        // byte; the valid prefix before that byte is emitted as runes.
        let valid_up_to = match std::str::from_utf8(rest) {
            Ok(valid) => valid.len(),
            Err(error) => error.valid_up_to(),
        };
        for ch in std::str::from_utf8(&rest[..valid_up_to])
            .unwrap_or_default()
            .chars()
        {
            go_append_escaped_rune(&mut out, ch);
        }
        offset += valid_up_to;
        if offset < bytes.len() {
            let byte = rest[valid_up_to];
            out.push_str("\\x");
            out.push(go_lower_hex(byte >> 4));
            out.push(go_lower_hex(byte & 0x0f));
            offset += 1;
        }
    }
    out.push('"');
    out
}

/// One lowercase hex digit, as `strconv.lowerhex`.
fn go_lower_hex(nibble: u8) -> char {
    (b"0123456789abcdef"[usize::from(nibble & 0x0f)]) as char
}

/// `strconv.appendEscapedRune` (`quote.go:64-115`) for `Quote` — neither
/// `ASCIIonly` nor `graphicOnly`.
fn go_append_escaped_rune(out: &mut String, ch: char) {
    if ch == '"' || ch == '\\' {
        out.push('\\');
        out.push(ch);
        return;
    }
    let code = u32::from(ch);
    if go_is_print(code) {
        out.push(ch);
        return;
    }
    match ch {
        '\u{07}' => out.push_str("\\a"),
        '\u{08}' => out.push_str("\\b"),
        '\u{0c}' => out.push_str("\\f"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        '\u{0b}' => out.push_str("\\v"),
        _ if code < 0x20 || code == 0x7f => {
            out.push_str("\\x");
            out.push(go_lower_hex(u8::try_from(code >> 4).unwrap_or(0)));
            out.push(go_lower_hex(u8::try_from(code & 0x0f).unwrap_or(0)));
        }
        _ if code < 0x10000 => {
            out.push_str("\\u");
            for shift in [12u32, 8, 4, 0] {
                out.push(go_lower_hex(u8::try_from(code >> shift & 0xf).unwrap_or(0)));
            }
        }
        _ => {
            out.push_str("\\U");
            for shift in [28u32, 24, 20, 16, 12, 8, 4, 0] {
                out.push(go_lower_hex(u8::try_from(code >> shift & 0xf).unwrap_or(0)));
            }
        }
    }
}

/// `strconv.IsPrint` (`quote.go:518-559`) for the Latin-1 fast paths exactly as
/// Go spells them, and Rust's `char::escape_debug` as the oracle above that — a
/// rune is printable here iff `escape_debug` emits it unchanged.
///
/// Known divergence, immaterial to where this text appears: above Latin-1 the
/// oracle is Rust's `char::escape_debug`, and the two disagree on exactly the
/// Unicode **mark** categories. Measured over the whole scalar-value range,
/// `escape_debug` escapes 954,591 code points; Go's `IsPrint` accepts 2,140 of
/// those (2,133 of them `unicode.IsMark` — combining marks such as `U+0300`/
/// `U+0301`, plus 7 other printable marks), and Go emits each raw while this
/// renders `\uNNNN`. In the other direction the divergence is empty: Go rejects
/// everything else `escape_debug` escapes. Only a TEXT/BLOB/REAL cell's
/// scan-error message goes through this, so the difference is cosmetic in a
/// diagnostic string rather than a stored or marshalled value; the executed
/// comparisons below pin the invalid-UTF-8, Latin-1 and CJK/emoji cases Go
/// itself reports, which agree byte-for-byte.
fn go_is_print(code: u32) -> bool {
    if code <= 0xff {
        return (0x20..=0x7e).contains(&code) || ((0xa1..=0xff).contains(&code) && code != 0xad);
    }
    let value = char::from_u32(code).unwrap_or('\u{FFFD}');
    value.escape_debug().next() == Some(value) && value.escape_debug().count() == 1
}
/// The boxed error payload for NULL into a required column.
#[derive(Debug)]
struct NullRequired(String);

impl std::fmt::Display for NullRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "NULL value in required column `{}` (Go parity: NULL into non-pointer errors)",
            self.0
        )
    }
}

impl std::error::Error for NullRequired {}

/// The Go-parity NULL failure for a non-pointer column.
fn null_required(column: &str, ty: Type) -> SqliteError {
    SqliteError::FromSqlConversionFailure(0, ty, Box::new(NullRequired(column.to_string())))
}

/// The Go-parity failure for a cell whose driver value cannot be stored into a
/// Go `time.Time` (`database/sql/convert.go`: "unsupported Scan, storing
/// driver.Value type … into type *time.Time"). Reached when a REAL/BLOB cell, or
/// an INTEGER in a column whose DECLARED type is not one of the driver's three
/// timestamp names, is scanned into a time field.
fn unsupported_scan(column: &str, driver_type: &str) -> SqliteError {
    SqliteError::FromSqlConversionFailure(
        0,
        Type::Text,
        Box::new(UnsupportedScan {
            column: column.to_string(),
            driver_type: driver_type.to_string(),
        }),
    )
}

#[derive(Debug)]
struct UnsupportedScan {
    column: String,
    driver_type: String,
}

impl std::fmt::Display for UnsupportedScan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unsupported Scan, storing driver.Value type {} into type *time.Time (column `{}`)",
            self.driver_type, self.column
        )
    }
}

impl std::error::Error for UnsupportedScan {}

/// An instant together with the zone Go's driver gave it — the port's stand-in
/// for a `time.Time` whose `loc` is a `time.FixedZone`.
///
/// The pinned driver decodes TEXT timestamps with
/// `time.ParseInLocation(format, s, time.UTC)` (`sqlite3.go:2827`), and a layout
/// ending in `-07:00` makes Go's parser attach the VALUE's own offset
/// (`time/format.go`'s `parse`: the `zoneOffset != -1` arm calls
/// `t.setLoc(FixedZone(zoneName, zoneOffset))` when no local zone matches).
/// The instant is the offset-subtracted one, but the zone survives, and
/// `encoding/json` renders a `time.Time`/`*time.Time` with it — `RFC3339Nano`
/// prints the offset and reserves `Z` for a zero offset. `DateTime<Utc>` alone
/// cannot carry that, so the annotation DTO's wire text needs this pair, not a
/// UTC-normalised instant.
///
/// Offsets are plain seconds rather than chrono's `FixedOffset` because Go's
/// accepted range is wider: its offset arm allows hour ≤ 24 and minute ≤ 60
/// (`>` rather than `>=`, "as some people do write offsets of 24 hours"), so
/// `|offset| ≤ 90060`, while `FixedOffset::east_opt` stops at 86399.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoTime {
    /// The instant as UTC calendar fields (what `t.UTC()` shows).
    pub utc: NaiveDateTime,
    /// The zone offset in seconds; `0` is Go's UTC location, rendered `Z`.
    pub offset_secs: i32,
}

impl GoTime {
    /// The instant as `DateTime<Utc>` — what a record's timestamp field holds.
    #[must_use]
    pub fn to_utc(&self) -> DateTime<Utc> {
        DateTime::from_naive_utc_and_offset(self.utc, Utc)
    }

    /// Go's `time.Time.IsZero()` (`t.wall == 0 && t.ext == 0`): the *instant* is
    /// year 1 January midnight, whatever the zone. So `0001-01-01 03:00:00+03:00`
    /// IS Go's zero — its instant is year-1 midnight UTC — while still rendering
    /// `+03:00`; `0001-01-01 00:00:00-03:00` is not zero.
    #[must_use]
    pub fn is_go_zero(&self) -> bool {
        self.utc == go_zero_time().naive_utc()
    }

    /// Go's `time.Format(time.RFC3339Nano)` — the text `encoding/json` gives a
    /// `time.Time`: `T`-separated, fractional seconds with trailing zeros trimmed
    /// and no fraction at all when zero, and the zone spelled `Z` for a zero
    /// offset or `±HH:MM` otherwise.
    ///
    /// Go's strict RFC3339 formatter rejects offsets whose hour is outside
    /// `[0, 23]` (`timezone hour outside of range [0,23]`) and years outside
    /// `[0, 9999]` (`year outside of range [0,9999]`); `json.Marshal` then fails
    /// for the entire enclosing object. The driver can parse `+24:60`, and its
    /// INTEGER path can produce years beyond Chrono's calendar range. This DTO
    /// stores timestamps as `String` and has no marshal-error channel, so the
    /// port normalizes invalid offsets to UTC and Chrono-bounded integer times
    /// saturate. These are documented API-boundary divergences, not scan errors.
    #[must_use]
    pub fn to_rfc3339_nano(&self) -> String {
        // Go's check looks at the printed zone HOUR DIGITS and rejects `>= 24`,
        // so ±18:30 renders normally and only the magnitude of the hour matters.
        if self.offset_secs.abs() / 3600 > 23 {
            return utc_rfc3339_nano(self.to_utc());
        }
        let zone = if self.offset_secs == 0 {
            "Z".to_string()
        } else {
            let sign = if self.offset_secs < 0 { '-' } else { '+' };
            let abs = self.offset_secs.abs();
            format!("{sign}{:02}:{:02}", abs / 3600, (abs % 3600) / 60)
        };
        // Go formats `t.locabs()` — the WALL reading in the value's own zone, i.e.
        // the instant plus its offset — while the nanoseconds are zone-
        // independent. So `2024-02-02 03:00:00+03:00` renders `…T03:00:00+03:00`,
        // NOT the UTC fields.
        let Some(wall) = self.wall() else {
            return utc_rfc3339_nano(self.to_utc());
        };
        format!(
            "{}{}{}",
            wall.format("%Y-%m-%dT%H:%M:%S"),
            go_fraction(&wall),
            zone
        )
    }

    /// The wall-clock reading in this value's zone — Go's `t.locabs()`: the UTC
    /// instant shifted by the offset. `None` when the shift leaves chrono's
    /// calendar range.
    #[must_use]
    fn wall(&self) -> Option<NaiveDateTime> {
        let delta = chrono::Duration::try_seconds(i64::from(self.offset_secs))?;
        self.utc.checked_add_signed(delta)
    }
}

/// The zero-time fallback the driver's failure arms assign, as a [`GoTime`]:
/// year 1 midnight with a UTC zone, so the wire text is `0001-01-01T00:00:00Z`.
#[must_use]
fn go_zero_gotime() -> GoTime {
    GoTime {
        utc: go_zero_time().naive_utc(),
        offset_secs: 0,
    }
}

/// Go's `RFC3339Nano` text for a `DateTime<Utc>`: the shape `encoding/json`
/// renders a UTC `time.Time` with, and the shape a value with an unrenderable
/// zone is normalised to (see [`GoTime::to_rfc3339_nano`]).
#[must_use]
fn utc_rfc3339_nano(dt: DateTime<Utc>) -> String {
    // Go prints the fraction with trailing zeros trimmed and none at all when
    // it is zero; chrono's `%.f` keeps the zeros, so render nine digits and
    // trim here.
    let text = dt.format(GO_RFC3339_NANO).to_string();
    let Some(dot) = text.find('.') else {
        return text;
    };
    let (head, fraction_and_z) = text.split_at(dot);
    let fraction = fraction_and_z.trim_end_matches('Z').trim_end_matches('0');
    if fraction == "." || fraction.is_empty() {
        format!("{head}Z")
    } else {
        format!("{head}{fraction}Z")
    }
}

/// Go's `RFC3339Nano` fractional second for a naive instant: `.N…` with trailing
/// zeros trimmed, absent when the nanoseconds are zero.
#[must_use]
fn go_fraction(dt: &NaiveDateTime) -> String {
    let nanos = dt.nanosecond() % 1_000_000_000;
    if nanos == 0 {
        return String::new();
    }
    format!(".{}", format!("{nanos:09}").trim_end_matches('0'))
}

/// `true` when a column's declared type makes the pinned driver treat its cells
/// as timestamps: `sqlite3.go:2782` and `:2822` both switch on `decltype[i]`
/// matching EXACTLY one of `columnTimestamp`/`columnDatetime`/`columnDate`
/// (`:345-348`), with the decltype lower-cased at `:2645`. Matching is exact, not
/// a substring: a declared `datetime(3)` is none of them, so its cells keep their
/// storage type — verified against the pinned driver, where an INTEGER in a
/// `datetime(3)` column scans into `time.Time` as an `unsupported Scan` error.
#[must_use]
fn is_timestamp_decltype(decl_type: Option<&str>) -> bool {
    matches!(
        decl_type,
        Some("timestamp") | Some("datetime") | Some("date")
    )
}

/// The declared type of a row's column, lower-cased exactly as the driver's
/// `declTypes` does (`sqlite3.go:2638-2650`).
///
/// rusqlite exposes the same `sqlite3_column_decltype` through the statement
/// behind the row (`Row::as_ref` → `Statement::columns()`, feature
/// `column_decltype`). An expression (`SELECT CAST(1 AS INTEGER)`) has no
/// declared type — `None` — and the driver's switch then takes `default`, so the
/// value keeps its storage type.
fn column_decl_type(row: &Row<'_>, column: &str) -> Option<String> {
    // rusqlite's `Row: AsRef<Statement>` (`row.rs:349-353`) is the same
    // `sqlite3_column_decltype` the driver reads, and `columns()` is gated on the
    // `column_decltype` feature (`column.rs:145-159`).
    let stmt = row.as_ref();
    let index = stmt.column_index(column).ok()?;
    let columns = stmt.columns();
    let decl = columns.get(index)?.decl_type()?;
    Some(decl.to_lowercase())
}

/// The pinned driver's bool decltype: an INTEGER cell in a column whose
/// lowercased declared type is exactly `boolean` is handed back as a Go `bool`
/// (`sqlite3.go:2797-2798`, `dest[i] = val > 0`), which `driver.Bool` then
/// accepts unchanged. `bool`/`BOOL` are NOT driver names, so they keep the
/// strict 0/1 rule.
fn driver_bool_decl(row: &Row<'_>, column: &str) -> bool {
    column_decl_type(row, column).as_deref() == Some("boolean")
}

/// The pinned driver's TEXT DATETIME decode (`sqlite3.go:2821-2842`) as Go's own
/// grammar, shared by [`RowGet::get_dt`], [`RowGet::get_opt_dt`] and the
/// annotation wire-text reader:
///
/// * `strings.TrimSuffix(s, "Z")` (`:2823`) removes **exactly one** trailing `Z`,
///   so `…+03:00Z` only loses the last character and still matches format 1, while
///   `…ZZ` matches nothing;
/// * `SQLiteTimestampFormats` (`:331-343`) are tried in order, each with
///   `time.ParseInLocation(format, s, time.UTC)` — the first success wins;
/// * **any** failure assigns `time.Time{}` and reports NO error (`:2831-2838`):
///   the DDL's `'0000-00-00 00:00:00'` default and outright garbage land there
///   (a NAIVE `CURRENT_TIMESTAMP` value such as `2024-02-02 03:00:00` does NOT —
///   it matches layout 3 and is a real stamp parsed as UTC);
/// * the connection location (`:2836-2838`) is UTC here — no `_loc` DSN parameter
///   is set anywhere in Navidrome — which does NOT normalise away a zone a value
///   was parsed with, hence [`GoTime`] rather than a bare `DateTime<Utc>`.
///
/// [`go_parse_timestamp`] spells the grammar because chrono's patterns are a
/// different language: chrono's `%#z` accepts a bare `+03`, `%:z` accepts `+0300`,
/// neither accepts Go's comma fractional seconds, and `%F` accepts other
/// separators and underflows `0000-00-00` into a plausible date.
///
/// So an unparseable cell is Go's zero time — never an error, never "absent".
#[must_use]
pub fn driver_parse_text_dt(text: &str) -> GoTime {
    for format in SQLITE_TIMESTAMP_FORMATS {
        if let Some(parsed) = go_parse_timestamp(text, *format) {
            return parsed;
        }
    }
    // Go's `err != nil` arm: `t = time.Time{}`, and no error is reported.
    go_zero_gotime()
}

/// The driver's nine layouts in its own order (`SQLiteTimestampFormats`,
/// `sqlite3.go:331-343`), described rather than spelled as Go's reference
/// layout: the only variables are the date/time separator, whether seconds are
/// present, whether an offset is required, and whether the layout ends at the
/// date.
const SQLITE_TIMESTAMP_FORMATS: &[GoFormat] = &[
    GoFormat {
        separator: ' ',
        seconds: true,
        offset: Some(ZoneStyle::Colon),
        date_only: false,
    },
    GoFormat {
        separator: 'T',
        seconds: true,
        offset: Some(ZoneStyle::Colon),
        date_only: false,
    },
    GoFormat {
        separator: ' ',
        seconds: true,
        offset: None,
        date_only: false,
    },
    GoFormat {
        separator: 'T',
        seconds: true,
        offset: None,
        date_only: false,
    },
    GoFormat {
        separator: ' ',
        seconds: true,
        offset: None,
        date_only: false,
    },
    GoFormat {
        separator: 'T',
        seconds: true,
        offset: None,
        date_only: false,
    },
    GoFormat {
        separator: ' ',
        seconds: false,
        offset: None,
        date_only: false,
    },
    GoFormat {
        separator: 'T',
        seconds: false,
        offset: None,
        date_only: false,
    },
    GoFormat {
        separator: ' ',
        seconds: false,
        offset: None,
        date_only: true,
    },
];

/// One `SQLiteTimestampFormats` entry.
#[derive(Debug, Clone, Copy)]
struct GoFormat {
    /// The literal between `2006-01-02` and `15:04:05` (`' '` or `'T'`).
    separator: char,
    /// Whether the layout carries `:05` after the minutes.
    seconds: bool,
    /// Which zone the layout requires (`None` = none of it).
    offset: Option<ZoneStyle>,
    /// Whether the layout ends at the date.
    date_only: bool,
}

/// The zone half of a layout. Only `-07:00` appears in `SQLiteTimestampFormats`;
/// the variants Go's parser folds together (`stdNumColonTZ` and friends) are
/// kept as an enum so the grammar table stays honest about what it accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZoneStyle {
    /// `-07:00` — sign, two digits, colon, two digits.
    Colon,
}

/// Go's `time.Parse` for one of the nine layouts, as far as those layouts can be
/// described (`time/format.go`'s `parse`, `getnum`, `parseNanoseconds`). The
/// rules that a chrono pattern would get wrong are all here, each verified
/// against the pinned Go toolchain:
///
/// * `2006-01-02` — `stdLongYear` wants four digits, and `stdZeroMonth`/
///   `stdZeroDay` are `fixed`, so exactly two digits each (a single-digit month
///   or day is an error); month `1..12`, day validated leap-year-aware;
/// * `15:04:05` — the HOUR is `stdHour`, whose `getnum(value, false)` accepts a
///   ONE-digit hour (`2024-02-02 3:00:00` parses); minute and second are
///   `stdZeroMinute`/`stdZeroSecond` — `fixed`, two digits each. Hour `< 24`,
///   minute and second `< 60`;
/// * `.999999999` — `stdFracSecond9`: optional, accepts a comma exactly like a
///   period (`commaOrPeriod`), takes any number of digits ("even more than asked
///   for"), and `parseNanoseconds` truncates to ten CHARACTERS (the separator
///   counted) before scaling, so `.999999999999` is `.999999999` and `,5` is `.5`;
/// * `-07:00` — exactly six characters with `':'` at index 3; a bare `+03`, a
///   `+0300` or a one-digit `+1:30` is `errBad`. Ranges are `hour ≤ 24`,
///   `minute ≤ 60`, so `+24:60` parses as a 25-hour offset;
/// * Go's `skip` folds a layout space into a run of spaces, so
///   `2024-02-02  03:00:00` parses while `2024-02-02\t03:00:00` does not;
/// * leftover text after the layout is Go's `extra text` error;
/// * a layout with seconds still consumes a fraction the value carries —
///   `stdZeroSecond`'s "do we have a fractional second but no fractional second
///   in the layout?" arm — which is why the seconds layouts all accept `.5`.
#[must_use]
fn go_parse_timestamp(text: &str, format: GoFormat) -> Option<GoTime> {
    // `strings.TrimSuffix(s, "Z")`, one final `Z`, applied per attempt exactly as
    // the driver does it once before the loop (identical result either way, kept
    // here so each layout sees the same input the driver hands Go).
    let value = text.strip_suffix('Z').unwrap_or(text);
    let mut rest = value;
    let date = go_fixed_date(rest)?;
    rest = &rest[10..];

    if format.date_only {
        // Layout 9 ends at the date; anything left over is Go's `extra text`.
        if !rest.is_empty() {
            return None;
        }
        return Some(GoTime {
            utc: date.and_hms_opt(0, 0, 0)?,
            offset_secs: 0,
        });
    }

    // The separator literal. Go's `skip` only folds spaces, and the `T` layouts
    // contain no space, so `2024-02-02 03:00` cannot match layout 7 but does
    // match layout 6 — the order of the table is what decides which one wins.
    if format.separator == 'T' {
        if rest.is_empty() || rest.as_bytes()[0] != b'T' {
            return None;
        }
        rest = &rest[1..];
    } else {
        if rest.is_empty() || rest.as_bytes()[0] != b' ' {
            return None;
        }
        rest = go_cutspace(rest);
    }

    let (hour, mut rest) = go_num(rest, false)?;
    if hour >= 24 {
        return None;
    }
    if rest.is_empty() || rest.as_bytes()[0] != b':' {
        return None;
    }
    rest = &rest[1..];
    let minute = go_fixed_num(rest, 2)?;
    if minute >= 60 {
        return None;
    }
    rest = &rest[2..];

    let mut second: u32 = 0;
    let mut nanos: u32 = 0;
    if format.seconds {
        if rest.is_empty() || rest.as_bytes()[0] != b':' {
            return None;
        }
        rest = &rest[1..];
        second = go_fixed_num(rest, 2)?;
        if second >= 60 {
            return None;
        }
        rest = &rest[2..];
        let (remainder, fraction) = go_fraction_digits(rest);
        nanos = fraction;
        rest = remainder;
    }

    let mut offset_secs: i32 = 0;
    if format.offset == Some(ZoneStyle::Colon) {
        let (offset, remainder) = go_num_colon_tz(rest)?;
        offset_secs = offset;
        rest = remainder;
    }
    // Go's `parse` ends with `if len(value) != 0 { … extra text … }`.
    if !rest.is_empty() {
        return None;
    }

    let naive = date.and_hms_nano_opt(hour, minute, second, nanos)?;
    // Parsed with the UTC location, so the offset is subtracted from the wall
    // reading to get the instant, while the offset itself stays the zone.
    let utc = go_instant(naive, offset_secs)?;
    Some(GoTime { utc, offset_secs })
}

/// Go's `cutspace` (`time/format.go`): strip a run of leading spaces. Go's
/// `skip` applies it to BOTH the layout's and the value's whitespace run, which
/// is why a layout space matches several value spaces — and why a tab matches
/// nothing, since `cutspace` only knows `' '`.
#[must_use]
fn go_cutspace(text: &str) -> &str {
    text.trim_start_matches(' ')
}

/// `2006-01-02`: four digits, `-`, two-digit month `1..12`, `-`, two-digit day
/// valid for that month. The caller advances exactly ten bytes.
///
/// Every field goes through [`go_fixed_num`], i.e. Go's `getnum(…, fixed = true)`
/// — NOT a numeric parse. `str::parse::<u32>` accepts a leading `+`, so
/// `2024-+2-02 03:00:00` would otherwise decode month `+2` and be accepted;
/// Go's `stdZeroMonth`/`stdZeroDay` are `fixed` and `isDigit`-bound, so a sign
/// in either slot is `errBad` and the cell lands on Go's zero time.
#[must_use]
fn go_fixed_date(text: &str) -> Option<NaiveDate> {
    let head = text.get(..10)?;
    let bytes = head.as_bytes();
    if !bytes[..4].iter().all(|b| b.is_ascii_digit()) || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: i32 = head[..4].parse().ok()?;
    let month = go_fixed_num(&head[5..7], 2)?;
    let day = go_fixed_num(&head[8..10], 2)?;
    // Go validates `day < 1 || day > daysIn(month, year)` after the whole parse;
    // chrono's `from_ymd_opt` does the same check, leap-year-aware.
    #[allow(clippy::manual_range_contains)] // Go's `m < 1 || m > 12`, format.go parseDate
    if month < 1 || month > 12 {
        return None;
    }
    NaiveDate::from_ymd_opt(year, month, day)
}

/// Go's `getnum(s, fixed = false)`: one digit, or two when both are digits.
/// `isDigit` bounds-checks, so empty input is `errBad`.
#[must_use]
fn go_num(text: &str, fixed: bool) -> Option<(u32, &str)> {
    let bytes = text.as_bytes();
    if !bytes.first().is_some_and(|b| b.is_ascii_digit()) {
        return None;
    }
    if bytes.get(1).is_some_and(|b| b.is_ascii_digit()) {
        // Both digits present: `int(s[0]-'0')*10 + int(s[1]-'0')`, two consumed,
        // whichever `fixed` was asked for.
        let pair = std::str::from_utf8(&bytes[..2]).ok()?.parse::<u32>().ok()?;
        return Some((pair, &text[2..]));
    }
    if fixed {
        return None;
    }
    Some((u32::from(bytes[0] - b'0'), &text[1..]))
}

/// Go's `getnum(s, fixed = true)`: exactly `width` ASCII digits or `errBad`.
/// Leading zeros are allowed (`03` → 3).
///
/// Deliberately not a numeric parse: `str::parse` accepts a leading `+`/`-` and
/// Go's `getnum` rejects anything that is not a digit at every position.
#[must_use]
fn go_fixed_num(text: &str, width: usize) -> Option<u32> {
    let head = text.get(..width)?;
    if !head.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    head.parse().ok()
}

/// [`go_fixed_num`] over a byte slice, for the offset arm whose slices can land
/// inside a multi-byte character (see [`go_num_colon_tz`]).
#[must_use]
fn go_fixed_num_bytes(bytes: &[u8], width: usize) -> Option<u32> {
    let head = bytes.get(..width)?;
    if !head.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    std::str::from_utf8(head).ok()?.parse().ok()
}
/// Go's `stdFracSecond9` arm plus `parseNanoseconds`: a `.` or `,` followed by at
/// least one digit, any number of digits, truncated to ten characters and scaled
/// to nanoseconds. A separator with no digit after it means "fractional second
/// omitted" — a SUCCESS, not an error — which is why `2024-02-02 03:00:00.` fails
/// only because the leftover `.` then trips the `extra text` check.
#[must_use]
fn go_fraction_digits(text: &str) -> (&str, u32) {
    let bytes = text.as_bytes();
    if bytes.len() < 2 || (bytes[0] != b'.' && bytes[0] != b',') || !bytes[1].is_ascii_digit() {
        return (text, 0);
    }
    let mut end = 1;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    // `if nbytes > 10 { value = value[:10]; nbytes = 10 }` — `nbytes` COUNTS THE
    // SEPARATOR — then `atoi(value[1:nbytes])` and `scaleDigits = 10 - nbytes`
    // multiplications. So `.5` (nbytes 2) is 5 * 10^8 = `.5`, and a 12-digit
    // fraction is truncated to ten characters with no scaling at all.
    let nbytes = end.min(10);
    let digits = &text[1..nbytes];
    let parsed: u32 = digits.parse().unwrap_or(0);
    let scale: u32 = 10u32.pow(u32::try_from(9 - digits.len()).unwrap_or(0));
    (&text[end..], parsed.saturating_mul(scale))
}

/// Go's `stdNumColonTZ` arm (`-07:00`): sign, two digits, `':'`, two digits. The
/// ranges are `hr > 24` and `mm > 60` — strict `>` — so `+24:00` and `+24:60`
/// parse (the latter as a 25-hour offset) while `+25:00` does not; a trailing
/// `Z` (`+03:00Z`) is the trimmed one and never reaches here.
///
/// The arm works on BYTES rather than `&str` slices: Go indexes `value[0]`/
/// `value[3]` and slices `value[1:3]`/`value[4:6]` over raw bytes, so a
/// non-ASCII rune sitting at the colon (the shapes `…03:€` and `é1:30`) is
/// simply not two digits — `errBad`, and the cell falls through to Go's zero
/// time. Slicing a `&str` mid-character would panic instead.
///
/// The remainder is only returned once both digit pairs are validated ASCII, so
/// byte 6 is guaranteed to be a char boundary.
#[must_use]
fn go_num_colon_tz(text: &str) -> Option<(i32, &str)> {
    let bytes = text.as_bytes();
    if bytes.len() < 6 || bytes[3] != b':' {
        return None;
    }
    let sign = bytes[0];
    let hour = go_fixed_num_bytes(&bytes[1..3], 2)?;
    let minute = go_fixed_num_bytes(&bytes[4..6], 2)?;
    if hour > 24 || minute > 60 {
        return None;
    }
    let offset = i32::try_from(hour * 3600 + minute * 60).ok()?;
    let offset = match sign {
        b'+' => offset,
        b'-' => -offset,
        _ => return None,
    };
    Some((offset, &text[6..]))
}
/// The UTC instant of a wall reading taken in a fixed zone: Go's
/// `Date(…, UTC)` then `t.addSec(-zoneOffset)`.
///
/// Go's seconds arithmetic is unbounded `int64`; chrono's calendar stops at year
/// ±262143, so a reading whose instant lands outside chrono saturates at chrono's
/// bound rather than failing the read (module docs' parity-gap note).
#[must_use]
fn go_instant(naive: NaiveDateTime, offset_secs: i32) -> Option<NaiveDateTime> {
    let delta = chrono::Duration::try_seconds(i64::from(offset_secs))?;
    let bound = if offset_secs > 0 {
        NaiveDateTime::MIN
    } else {
        NaiveDateTime::MAX
    };
    Some(naive.checked_sub_signed(delta).unwrap_or(bound))
}

/// The pinned driver's INTEGER-cell timestamp decode (`sqlite3.go:2781-2797`): a
/// cell stored as `SQLITE_INTEGER` in a declared `timestamp`/`datetime`/`date`
/// column becomes a `time.Time`, not a conversion error.
///
/// * magnitude strictly `> 1e12` or `< -1e12` means milliseconds — Go's own
///   comment says "too large to be a reasonable timestamp in seconds" — and the
///   value is multiplied by `time.Millisecond` (1e6) into nanoseconds;
/// * everything else (including exactly ±1e12) is read as SECONDS;
/// * the multiplication is plain Go `int64`, so it WRAPS:
///   `9223372036854775807 * 1e6 == -1000000`, i.e. `1969-12-31T23:59:59.999Z`,
///   verified against the pinned driver, and `time.Unix(0, nsec)` then normalises
///   a negative remainder by borrowing a whole second;
/// * the result is `.UTC()`, so the zone is UTC and the wire text ends in `Z` —
///   the integer path carries no offset, unlike the TEXT path.
#[must_use]
#[allow(clippy::manual_range_contains)] // Go's `val > 1e12 || val < -1e12`, sqlite3.go:2787
pub fn driver_integer_dt(value: i64) -> GoTime {
    if value > 1_000_000_000_000 || value < -1_000_000_000_000 {
        // Go: `val *= int64(time.Millisecond)` — 1e6, plain wrapping int64 — then
        // `time.Unix(0, val)`, which normalises a negative remainder.
        let nanos = value.wrapping_mul(1_000_000);
        let (sec, nanos) = go_unix_parts(nanos);
        GoTime {
            utc: unix_instant(sec, nanos),
            offset_secs: 0,
        }
    } else {
        // Go: `time.Unix(val, 0)` — the value is SECONDS.
        GoTime {
            utc: unix_instant(value, 0),
            offset_secs: 0,
        }
    }
}

/// Go's `time.Unix(0, nsec)` normalisation (`time.Unix`'s leading arm): truncate
/// toward zero, then borrow a second when the remainder went negative.
#[must_use]
#[allow(clippy::manual_range_contains)] // Go's `nsec < 0 || nsec >= 1e9`, time.Unix
fn go_unix_parts(mut nsec: i64) -> (i64, u32) {
    let mut sec: i64 = 0;
    if nsec < 0 || nsec >= 1_000_000_000 {
        let n = nsec / 1_000_000_000;
        sec = sec.wrapping_add(n);
        nsec -= n * 1_000_000_000;
        if nsec < 0 {
            nsec += 1_000_000_000;
            sec -= 1;
        }
    }
    (sec, nsec as u32)
}

/// The epoch-second + nanosecond instant as chrono calendar fields, saturating at
/// chrono's bounds where Go's `time.Time` has none.
#[must_use]
fn unix_instant(sec: i64, nanos: u32) -> NaiveDateTime {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1)
        .unwrap_or(NaiveDate::MIN)
        .and_hms_opt(0, 0, 0)
        .unwrap_or_default();
    let Some(delta) = chrono::Duration::try_seconds(sec) else {
        return if sec > 0 {
            NaiveDateTime::MAX
        } else {
            NaiveDateTime::MIN
        };
    };
    match epoch.checked_add_signed(delta) {
        Some(base) => base
            .checked_add_signed(chrono::Duration::nanoseconds(i64::from(nanos)))
            .unwrap_or(base),
        None => {
            if sec > 0 {
                NaiveDateTime::MAX
            } else {
                NaiveDateTime::MIN
            }
        }
    }
}

/// `dbUser` row mapping (phase 1 of the two-phase seam): the user columns
/// dbx scans (`persistence/user_repository.go:30-34`) plus the raw
/// `libraries_json` SELECT-alias, still unparsed.
///
/// `libraries_json` is a non-pointer `string` in Go's `dbUser`
/// (`persistence/user_repository.go:33`), so it is read with
/// [`RowGet::get_text`]: the select's `COALESCE(…, '[]')` makes it non-NULL,
/// and a NULL (only possible if the alias were dropped) is a scan error in
/// Go too (NULL into a non-pointer string, `database/sql/convert.go`).
///
/// # Errors
/// A missing or NULL *required* user column fails, per Go parity.
pub fn row_to_user(row: &Row<'_>) -> SqliteResult<UserRow> {
    let libraries_json = row.get_text("libraries_json")?;
    Ok(UserRow {
        record: UserRecord {
            id: row.get_text("id")?,
            user_name: row.get_text("user_name")?,
            name: row.get_text("name")?,
            email: row.get_text("email")?,
            is_admin: row.get_bool("is_admin")?,
            last_login_at: row.get_opt_text("last_login_at")?,
            last_access_at: row.get_opt_text("last_access_at")?,
            created_at: row.get_text("created_at")?,
            updated_at: row.get_text("updated_at")?,
            scrobble_filter: row.get_text("scrobble_filter")?,
            password: row.get_text("password")?,
            token_epoch: row.get_i64("token_epoch")?,
            libraries: Vec::new(),
        },
        libraries_json,
    })
}

/// `dbUser.PostScan` (phase 2 of the two-phase seam,
/// `persistence/user_repository.go:36-43`): decode the raw `libraries_json`
/// onto the record.
///
/// Empty payload (NULL coalesced away) yields an empty library set like
/// Go's guard; the select's `COALESCE(…, '[]')` means a no-libraries user
/// reads the literal `"[]"`, which parses to the same empty (non-nil) set.
/// A non-empty payload that fails to parse is [`HydrateError::Decoded`]
/// with Go's exact prefix.
///
/// # Errors
/// [`HydrateError::Decoded`] carrying `USER_LIBRARIES_PREFIX`.
pub fn user_post_scan(raw: UserRow) -> Result<UserRecord, HydrateError> {
    let mut record = raw.record;
    record.libraries = parse_libraries_json(&raw.libraries_json)
        .map_err(|err| HydrateError::Decoded(format!("{USER_LIBRARIES_PREFIX}{err}")))?;
    Ok(record)
}

/// `PostScan`'s decode (`user_repository.go:37-41`), shared by
/// [`user_post_scan`]. Empty input is the skip-and-return-nil branch;
/// otherwise the JSON array of the select's `libraries_json`.
///
/// # Errors
/// The `serde_json` failure Go's `json.Unmarshal` would return.
pub fn parse_libraries_json(json: &str) -> Result<Vec<UserLibrary>, serde_json::Error> {
    if json.is_empty() {
        return Ok(Vec::new());
    }
    // Go decodes each array element independently: an element that is `null`
    // leaves the destination struct untouched (zero-valued) with NO error,
    // verified against the stdlib (`[{0 "" ""}], err = nil`). serde_json instead
    // rejects `null` as a type mismatch, so null elements are mapped to the
    // default here. The outer shape still has to be an array: Go rejects a
    // top-level object (`json: cannot unmarshal object into Go value of type
    // Libraries`) and serde_json rejects it too (`invalid type: map, expected a
    // sequence`) — the same accept/reject decision with different wording, per
    // the message-scope note in `hydrate-contract.md`.
    let raw: Vec<Option<UserLibrary>> = serde_json::from_str(json)?;
    Ok(raw
        .into_iter()
        .map(|element| element.unwrap_or_default())
        .collect())
}

// ---------------------------------------------------------------------------
// S-D3b: folder + playlist — the PostScan hooks of
// `persistence/folder_repository.go:38-46` and
// `persistence/playlist_repository.go:29-34`.
// ---------------------------------------------------------------------------

/// `dbFolder` row mapping (phase 1 of the two-phase seam): the columns
/// `selectFolder` scans (`persistence/folder_repository.go:74-78` —
/// `folder.*` plus the INNER-joined `library.path as library_path`), with the
/// `image_files` shim **still unparsed**.
///
/// Go's `dbFolder.ImageFiles` is a non-pointer `string` (`:26`), so the shim is
/// read with [`RowGet::get_text`]; the DDL declares it `not null default '[]'`
/// (`schema_ddl.rs:61`), so a NULL is a scan error in Go too.
///
/// Column notes — the dbx field-name mapping decides these, NOT the `structs`
/// tags (`dbx@v1.12.0/rows.go:60-85` scans through `DefaultFieldMapFunc`, and
/// Navidrome sets no `FieldMapper`): Go's field is **`UpdateAt`**, which
/// snake-cases to `update_at`, so the selected `updated_at` column has NO scan
/// target and is discarded into the throwaway `NullString` (`rows.go:84`).
/// `Folder.UpdateAt` therefore stays Go's zero `time.Time` on every read —
/// verified against the pinned dbx, where a folder row holding
/// `'2024-02-02 00:00:00+00:00'` in `updated_at` scans back `isZero() == true`.
/// [`crate::folder_repo::folder_row`] still WRITES `updated_at` (its `structs`
/// tag, `model/folder.go:32`, governs `structs.Map`), so the asymmetry is Go's,
/// not a port bug: this seam hydrates [`go_zero_time`] and the write leg keeps
/// the column.
///
/// `images_updated_at`/`updated_at`/`created_at` are non-pointer `time.Time` →
/// [`RowGet::get_dt`] (SQL NULL fails the scan as in Go; the DDL's
/// `'0000-00-00 00:00:00'` default scans as Go's zero time, since the driver
/// swallows the parse failure); the record spells Go's zero `CreatedAt` as
/// `None` so `is_go_zero` on a scanned value maps back to Go's `IsZero()`.
///
/// `library_path` is the INNER-joined `library.path` alias — the ONLY library
/// column the select emits (there is no `library_name` in `selectFolder`, contrary
/// to the contract's S-D3b line). Go scans it into `Folder.LibraryPath`,
/// `structs:"-"` (`model/folder.go:21`), so it lands on the record and is never
/// emitted by [`crate::folder_repo::folder_row`] — the write SQL is unchanged.
///
/// # Errors
/// A missing or NULL *required* folder column fails, per Go parity.
pub fn row_to_folder(row: &Row<'_>) -> SqliteResult<FolderRow> {
    let image_files = row.get_text("image_files")?;
    Ok(FolderRow {
        record: FolderRecord {
            library_id: row.get_i64("library_id")?,
            path: row.get_text("path")?,
            name: row.get_text("name")?,
            parent_id: row.get_text("parent_id")?,
            num_audio_files: row.get_i64("num_audio_files")?,
            num_playlists: row.get_i64("num_playlists")?,
            // Decoded by phase 2 from the raw shim column.
            image_files: None,
            images_updated_at: row.get_dt("images_updated_at")?,
            hash: row.get_text("hash")?,
            missing: row.get_bool("missing")?,
            // Go's `UpdateAt` has no `update_at` column to scan from, so the
            // selected `updated_at` is discarded and the field stays zero.
            updated_at: go_zero_time(),
            // Go's non-pointer `CreatedAt`: SQL NULL errors, an unparseable
            // default scans as Go's zero — which the record spells `None`.
            // Same helper as the album/mediafile pair (`go_zero_or_none`).
            created_at: go_zero_or_none(row, "created_at")?,
            // Join-only (`structs:"-"`): read onto the record, never written.
            library_path: row.get_text("library_path")?,
            // The row's own primary key (`Folder.ID` scans `id` normally).
            id: row.get_text("id")?,
        },
        image_files,
    })
}

/// `dbFolder.PostScan` (phase 2, `folder_repository.go:38-46`): decode the raw
/// `image_files` column onto the record.
///
/// The empty shim is Go's skip branch (`:40`): no decode runs and `ImageFiles`
/// stays nil (`None`). A non-empty payload that fails to parse is
/// [`HydrateError::Decoded`] carrying Go's EXACT prefix.
///
/// # Errors
/// [`HydrateError::Decoded`] carrying [`FOLDER_IMAGE_FILES_PREFIX`].
pub fn folder_post_scan(raw: FolderRow) -> Result<FolderRecord, HydrateError> {
    let mut record = raw.record;
    record.image_files = parse_folder_image_files(&raw.image_files)
        .map_err(|err| HydrateError::Decoded(format!("{FOLDER_IMAGE_FILES_PREFIX}{err}")))?;
    Ok(record)
}

/// `PostScan`'s decode (`folder_repository.go:40-44`), shared by
/// [`folder_post_scan`]: empty input is the skip-and-return-nil branch, the
/// landed [`crate::sql_json::unmarshal_image_files`] does the rest (its
/// nil-vs-empty distinction — JSON `null` → `None`, `[]` → `Some(empty)` — is
/// Go's `encoding/json` behaviour).
///
/// # Errors
/// The codec failure Go's `json.Unmarshal` would return (before wrapping).
pub fn parse_folder_image_files(
    json: &str,
) -> Result<Option<Vec<String>>, crate::sql_json::JsonColumnError> {
    if json.is_empty() {
        return Ok(None);
    }
    unmarshal_image_files(json)
}

/// `dbPlaylist` row mapping (phase 1): the columns `selectPlaylist` emits
/// (`persistence/playlist_repository.go:257-260` through
/// `sql_annotations.go:63-91`) — `playlist.*`, the six annotation columns and
/// the base `average_rating` — with the `rules` shim **still unparsed**.
///
/// `rules` is Go's `sql.NullString` shim (`:26`), so it is read with
/// [`RowGet::get_opt_text`]: SQL NULL → `None`, the empty cell → `Some("")`;
/// phase 2 leaves both as `Rules == None`, like Go's guard.
///
/// The annotation columns come from a LEFT JOIN, so they are *optional*: a
/// light query omits all six (dbx discards unmatched columns into a throwaway
/// `NullString`, `rows.go:84`). Each is probed with [`RowGet::has_column`] and
/// falls back to its zero value; the base `average_rating` (`playlist.average_rating`,
/// `NOT NULL DEFAULT 0`) is probed the same way so a projection without it
/// decodes to 0.0.
///
/// `owner_name` (`user.user_name as owner_name`, `:259`) is HYDRATED onto
/// `PlaylistRecord::owner_name`: Go's `Playlist.OwnerName` is `structs:"-"`
/// (`model/playlist.go:26`), but that tag governs only the WRITE map —
/// `ScanStruct` maps by field NAME (`dbx@v1.12.0/rows.go:60-85`), and
/// `OwnerName` snake-cases to the emitted `owner_name`, so Go scans it. Verified
/// against the pinned dbx: the join yields `OwnerName == "jdoe"`. It stays out
/// of [`crate::playlist_repo::playlist_row`] (`structs:"-"`), so the write SQL is
/// unchanged.
///
/// `created_at`/`updated_at` are non-pointer `time.Time` in Go
/// (`model/playlist.go:38-39`) → [`RowGet::get_dt`]; SQL NULL fails the scan, an
/// unparseable cell scans as Go's zero. Go's zero `CreatedAt` is the record's
/// `None`.
/// `evaluated_at` is `*time.Time` → [`RowGet::get_opt_dt`] (NULL → `None`,
/// unparseable → `Some(`[`go_zero_time`]`)`, because `database/sql` allocates a
/// pointer for the zero value the driver returns).
///
/// # Errors
/// A missing or NULL *required* playlist column fails, per Go parity.
pub fn row_to_playlist(row: &Row<'_>) -> SqliteResult<PlaylistRow> {
    let rules = row.get_opt_text("rules")?;
    Ok(PlaylistRow {
        record: PlaylistRecord {
            name: row.get_text("name")?,
            comment: row.get_text("comment")?,
            duration: row.get_f64("duration")?,
            size: row.get_i64("size")?,
            song_count: row.get_i64("song_count")?,
            owner_id: row.get_text("owner_id")?,
            public: row.get_bool("public")?,
            path: row.get_text("path")?,
            sync: row.get_bool("sync")?,
            uploaded_image: row.get_text("uploaded_image")?,
            external_image_url: row.get_text("external_image_url")?,
            created_at: go_zero_or_none(row, "created_at")?,
            updated_at: row.get_dt("updated_at")?,
            imported_hash: row.get_text("imported_hash")?,
            // Decoded by phase 2 from the raw shim column.
            rules: None,
            evaluated_at: row.get_opt_dt("evaluated_at")?,
            annotation: row_to_annotation(row)?,
            // The promoted `ItemImage` embed is filled by the SECOND layer
            // (`artwork_hydration::hydrate_playlist_artwork`, Go's `hydrateArtwork`
            // at `playlist_repository.go:191-194`); `selectPlaylist` emits no
            // artwork column, so phase 1 leaves Go's zero value = UNRESOLVED.
            image: crate::artwork_hydration::ItemImage::default(),
            // `OwnerName` is scanned by field name despite `structs:"-"`.
            owner_name: owner_name_column(row)?,
            // The row's own primary key (`Playlist.ID` scans `id` normally).
            id: row.get_text("id")?,
        },
        rules,
    })
}

/// `radioRepository.Get`'s row mapping (`radio_repository.go:56-66`): there is
/// deliberately **one phase, not two**. Go has no `dbRadio` wrapper struct, no
/// `PostScan` hook and no shim column —
/// `grep -n "dbRadio\|PostScan" persistence/radio_repository.go` returns nothing —
/// because `queryOne`/`queryAll` `ScanStruct` straight into `model.Radio`
/// (`dbx@v1.12.0/rows.go:60-85`). So this reader yields a
/// [`crate::radio_repo::RadioRecord`] directly: there is no raw-row twin and no
/// phase-2 hook to invent, and nothing here normalises a column after the scan.
///
/// `SELECT *` emits the table's own columns in DDL order, so every field is read
/// by name: `id`, `stream_url`, `name`, `home_page_url`, `uploaded_image` are
/// non-pointer Go `string` ([`RowGet::get_text`] — a NULL cell fails the scan,
/// `database/sql/convert.go:535`), and `created_at`/`updated_at` are NON-pointer
/// Go `time.Time` (`model/radio.go:20-21`) read with [`RowGet::get_dt`], which
/// likewise ERRORS on NULL. Executed against the real repository over a row with
/// NULL `created_at`: `Get` returns
/// `sql: Scan error on column index 4, name "created_at": unsupported Scan,
/// storing driver.Value type <nil> into type *time.Time` beside a NON-nil pointer
/// to a zero Radio (Go's `return &res, err`, `:61`). `GetAll` over the same row
/// returns the same error beside the rows already scanned — dbx appends each row
/// after a successful scan and aborts the loop at
/// (`dbx@v1.12.0/rows.go:169-171`, appending at `:192`), and
/// `radio_repository.go:71-73` returns `res` together with `err`. Executed over
/// {ok1, ok2, null}: `len == 2` with the error; with the NULL row first `len == 0`,
/// which is the shape an earlier oracle saw. The fresh DDL declares both columns as
/// bare `datetime` with no DEFAULT, so NULL is reachable. The port reports that
/// failure as [`crate::radio_repo::RadioError::Sqlite`] — NOT NotFound and not a
/// zero timestamp — and drops the partial slice, since `Result` has no
/// value-beside-error (see [`crate::radio_repo::RadioRepo::get_all`]). The artwork
/// layer never runs for such a row because Go returns before `hydrateArtwork`
/// (`:60-62`).
///
/// The `structs:"-"` embed is the read-path field: `image` stays zero here and the
/// second layer
/// ([`crate::artwork_hydration::hydrate_radio_artwork`]) fills it. Radio has no
/// `Annotations` embed (`model/radio.go:12` is `ItemImage`) and its `SELECT *`
/// joins nothing, so a radio carries no annotation state at all.
///
/// # Errors
/// A NULL or unscannable *required* radio column fails, per Go parity.
pub fn row_to_radio(row: &Row<'_>) -> SqliteResult<crate::radio_repo::RadioRecord> {
    Ok(crate::radio_repo::RadioRecord {
        id: row.get_text("id")?,
        stream_url: row.get_text("stream_url")?,
        name: row.get_text("name")?,
        home_page_url: row.get_text("home_page_url")?,
        uploaded_image: row.get_text("uploaded_image")?,
        created_at: row.get_dt("created_at")?,
        updated_at: row.get_dt("updated_at")?,
        // `ItemImage` (`structs:"-"`): the second layer's job, zero = unresolved.
        image: crate::artwork_hydration::ItemImage::default(),
    })
}

/// The annotation-column reader the contract lists as S-D3's gap (a)
/// (`hydrate-scan-contract.md`: "no shared annotation-column reader; `nd-db`
/// has no `Annotations` record yet"), folded into
/// [`nd_model::annotation::Annotation`] and stored on the record as a derived,
/// non-persisted field.
///
/// The three `coalesce`d columns (`starred`, `rating`, `play_count`) are
/// non-pointer and non-NULL *when the select emits them*; the three times
/// (`starred_at`, `play_date`, `rated_at`) are `*time.Time` and NULL for a user
/// with no annotation row. Absent columns decode to zero/`None` rather than
/// erroring — the light-query rule from the module docs.
///
/// nd-model's annotation DTO keeps timestamps as serialisable model strings
/// (`nd-model/src/annotation.rs:5-7`: timestamp parsing is the DB lane's job),
/// and Go's `encoding/json` renders a `*time.Time` as RFC3339Nano
/// (`database/sql/convert.go:329` uses the same layout, and `model.Annotations`
/// carries the pointers straight to JSON). A present stamp is therefore rendered
/// with [`to_go_time_text`] — `T`-separated, `Z` for UTC — NOT the SQLite
/// storage spelling from [`crate::time::to_text`], which is a DB convention and
/// never a wire format.
///
/// # Errors
/// Propagates a [`RowGet`] failure on a *present* column (NULL into a
/// non-pointer one, or a REAL `play_count` cell whose Go `%g` spelling is not an
/// integer — see `annotation_play_count`).
pub fn row_to_annotation(row: &Row<'_>) -> SqliteResult<Annotation> {
    Ok(Annotation {
        play_count: if row.has_column("play_count") {
            annotation_play_count(row, "play_count")?
        } else {
            0
        },
        play_date: optional_dt_text(row, "play_date")?,
        rating: if row.has_column("rating") {
            row.get_i32("rating")?
        } else {
            0
        },
        rated_at: optional_dt_text(row, "rated_at")?,
        starred: if row.has_column("starred") {
            row.get_bool("starred")?
        } else {
            false
        },
        starred_at: optional_dt_text(row, "starred_at")?,
        average_rating: if row.has_column("average_rating") {
            row.get_f64("average_rating")?
        } else {
            0.0
        },
    })
}

/// Go's int64 scan for a non-pointer `int64` column — [`go_scan_int64`] — kept
/// as its own entry point because the **normalized** album play count is the
/// case that motivated it:
/// `sql_annotations.go:80-83` renders
/// `round(coalesce(round(cast(play_count as float) / coalesce(album.song_count, 1), 1), 0)) as play_count`,
/// an expression column with no declared type, so SQLite stores it as REAL
/// (`typeof()` = `real`) even when the value is integral. Go does NOT truncate:
/// `asString` renders a `float64` with `strconv.FormatFloat(f, 'g', -1, 64)`
/// (`convert.go:570-571`) and `strconv.ParseInt(s, 10, 64)` decides the outcome
/// (`convert.go:499-505`). Probes against the pinned go-sqlite3 driver confirm both
/// arms: `round(7.0/3.0)` = 2 scans as `pc = 2, err = nil`; `1e+06` fails with
/// `converting driver.Value type float64 ("1e+06") to a int64: invalid syntax`;
/// `4.1152263e+07` fails with the same shape carrying `"4.1152263e+07"`. A
/// fractional cell (`2.3333`) fails identically, because `%g` keeps the decimal point.
///
/// So a REAL cell is rendered with [`crate::criteria_sql::go_fmt_float64`] — the
/// crate's existing Go-`%g` renderer, reused rather than duplicated — and parsed as
/// an integer. A spelling `ParseInt` rejects (exponent notation, a fraction, a
/// non-finite value, or a magnitude outside `int64`) becomes Go's conversion error,
/// carrying the column name. This deliberately diverges from a
/// truncate/round-toward-integer suggestion:
/// Go surfaces the failure rather than guessing, and the failure stays visible to
/// the caller instead of silently corrupting `play_count`.
///
/// # Errors
/// Go's NULL-into-non-pointer failure for a NULL cell, and Go's
/// `converting driver.Value type float64 ("<%g spelling>") to a int64:
/// invalid syntax` failure when a REAL cell's `%g` spelling is not an `int64`
/// — including an infinite cell, which Go spells `+Inf`/`-Inf` and rejects the
/// same way.
fn annotation_play_count(row: &Row<'_>, column: &str) -> SqliteResult<i64> {
    go_scan_int64(row, column)
}

/// Narrow a Go-parity int64 scan into the record's 32-bit field. Go's `int` is
/// 64-bit, so a wider cell scans there and is truncated by the model's field; the
/// port reports out-of-range instead (documented residual in
/// `hydrate-contract.md`), which keeps a silent truncation from becoming data.
///
/// # Errors
/// `SqliteError::IntegralValueOutOfRange` for a cell outside `i32`.
fn narrow_i32(value: i64) -> SqliteResult<i32> {
    i32::try_from(value).map_err(|_| SqliteError::IntegralValueOutOfRange(0, value))
}

/// Go's int64 scan for a non-pointer `int64`/`int` column
/// (`database/sql/convert.go:497-508`): NULL errors; an INTEGER cell is the
/// driver's `int64` and stores directly; every other driver value is rendered by
/// `asString` (`:557-578`) and run through `strconv.ParseInt`. That is why TEXT
/// `"7"` scans as 7, TEXT `"True"` fails, and a REAL cell is judged by its `%g`
/// spelling rather than truncated.
///
/// # Errors
/// Go's NULL-into-non-pointer failure, or Go's
/// `converting driver.Value type <T> ("<asString>") to a int64: <reason>`.
fn go_scan_int64(row: &Row<'_>, column: &str) -> SqliteResult<i64> {
    match row.get_ref(column)? {
        rusqlite::types::ValueRef::Null => Err(null_required(column, Type::Integer)),
        rusqlite::types::ValueRef::Integer(value) => Ok(value),
        rusqlite::types::ValueRef::Real(value) => go_parse_int64(
            column,
            "float64",
            &crate::criteria_sql::go_fmt_float64(value),
            go_quote_bytes(crate::criteria_sql::go_fmt_float64(value).as_bytes()),
        ),
        // `asString` returns a driver `string`/`[]uint8` unchanged
        // (`convert.go:557-562`). The driver's type name for a BLOB is `[]uint8`
        // — verified against the pinned driver — and Go quotes the RAW bytes
        // (escaping undecodable ones as `\xNN`), so the BLOB arm uses
        // [`go_quote_bytes`] rather than a lossy decode.
        rusqlite::types::ValueRef::Text(bytes) => go_parse_int64(
            column,
            "string",
            &go_string_from_bytes(bytes),
            go_quote_bytes(bytes),
        ),
        // A BLOB is a driver `[]uint8`, quoted over the same raw bytes.
        rusqlite::types::ValueRef::Blob(bytes) => {
            let text = go_string_from_bytes(bytes);
            go_parse_int64(column, "[]uint8", &text, go_quote_bytes(bytes))
        }
    }
}

/// Go's float64 scan (`database/sql/convert.go:521-532`): NULL errors, a REAL
/// cell stores directly, and INTEGER/TEXT/BLOB go through `asString` +
/// `strconv.ParseFloat`, so TEXT `"0.5"` scans and `"1e400"` reports
/// `value out of range`. The parse always sees the `asString` text; only the
/// failure message quotes it with `%q` over the value's raw bytes.
///
/// # Errors
/// Go's NULL failure, or `converting driver.Value type <T> (<quoted>) to a
/// float64`, where the reason is Go's inner `strconv` error text.
fn go_parse_float64(column: &str, driver_type: &str, bytes: &[u8]) -> SqliteResult<f64> {
    let text = go_string_from_bytes(bytes);
    let reason = match text.parse::<f64>() {
        Ok(value) => {
            if value.is_finite() {
                return Ok(value);
            }
            // A non-finite result is either Go's own special spelling or an
            // overflow beyond float64, where `ParseFloat` returns `±Inf` WITH
            // `ErrRange` — Rust returns the infinity silently, so the two are
            // told apart from the text, not from the value.
            match go_float_special(&text) {
                GoFloatSpecial::Infinity(v) => return Ok(v),
                GoFloatSpecial::NaN => return Ok(f64::NAN),
                GoFloatSpecial::SignedNaN => "invalid syntax",
                GoFloatSpecial::NotSpecial => "value out of range",
            }
        }
        Err(_) => "invalid syntax",
    };
    // `%q` runs over the driver value's own bytes, so invalid UTF-8 keeps its
    // `\xNN` spelling for a TEXT cell exactly as it does for a BLOB.
    let quoted = go_quote_bytes(bytes);
    Err(SqliteError::FromSqlConversionFailure(
        0,
        Type::Real,
        Box::new(GoToFloat64 {
            column: column.to_string(),
            driver_type: driver_type.to_string(),
            quoted,
            reason,
        }),
    ))
}

/// Go's `special` (`internal/strconv/atof.go:49-76`), the case-insensitive
/// infinity and NaN spellings `strconv.ParseFloat` recognises *before* trying a
/// decimal. Only a complete `inf`/`infinity` (never a longer prefix such as
/// `infin`) and an UNSIGNED `nan` are special: the sign branch falls through to
/// `inf` alone, so `+nan` is a syntax error even though Rust parses it.
enum GoFloatSpecial {
    Infinity(f64),
    NaN,
    /// `±nan`, which Go rejects.
    SignedNaN,
    /// No special spelling, so a non-finite parse is an overflow.
    NotSpecial,
}

fn go_float_special(text: &str) -> GoFloatSpecial {
    let signed = matches!(text.as_bytes().first(), Some(b'+') | Some(b'-'));
    let sign = if text.starts_with('-') { -1.0 } else { 1.0 };
    let rest = if signed { &text[1..] } else { text };
    let lower = rest.to_ascii_lowercase();
    if lower == "inf" || lower == "infinity" {
        return GoFloatSpecial::Infinity(sign * f64::INFINITY);
    }
    if lower == "nan" {
        return if signed {
            GoFloatSpecial::SignedNaN
        } else {
            GoFloatSpecial::NaN
        };
    }
    GoFloatSpecial::NotSpecial
}

/// `strconv.ParseBool`'s accepted spellings — `1`, `t`, `T`, `TRUE`, `true`,
/// `True`, `0`, `f`, `F`, `FALSE`, `false`, `False` — which Rust's
/// `bool::from_str` accepts as a subset (`true`/`false`) and Go rejects nothing
/// else, so the set is spelled out rather than delegated to `parse::<bool>`.
/// Anything else is `driver.Bool`'s rejection.
///
/// # Errors
/// Go's `sql/driver: couldn't convert "<text>" into type bool`, carried with the
/// column name.
fn go_parse_bool(column: &str, bytes: &[u8]) -> SqliteResult<bool> {
    // `strconv.ParseBool` over the driver's `string`/`[]byte` (`driver/types.go:71-83`),
    // whose accepted spellings are exactly the twelve below — six true and six
    // false (verified: `TrUe`, `yes`, `2`, `1.0`, `""` and `" t "` all fail). Both
    // arms report Go's `%q` of the raw bytes, so a BLOB and the same TEXT spell
    // identically in the message.
    match bytes {
        b"1" | b"t" | b"T" | b"TRUE" | b"true" | b"True" => Ok(true),
        b"0" | b"f" | b"F" | b"FALSE" | b"false" | b"False" => Ok(false),
        _ => Err(go_bool_error(column, go_quote_bytes(bytes))),
    }
}

/// `driver.Bool`'s failure text (`driver/types.go:75, :81, :93, :99, :102`),
/// carried with the column name so a bulk read stays diagnosable. `described` is
/// Go's own `%q`/`%d`/`%v` rendering of the offending driver value.
fn go_bool_error(column: &str, described: String) -> SqliteError {
    SqliteError::FromSqlConversionFailure(
        0,
        Type::Integer,
        Box::new(GoToBool {
            column: column.to_string(),
            described,
        }),
    )
}

/// The `strconv.ParseInt(asString(v), 10, 64)` step of Go's int64 scan arm
/// (`database/sql/convert.go:497-505`). On failure Go renders
/// `converting driver.Value type %T (%q) to a %s: %v` with the **`%q` of `s`**,
/// the `asString` text (not of the driver value), and `strconvErr`
/// (`convert.go:550-555`) unwraps the `*strconv.NumError` to its inner `Err` —
/// `invalid syntax` or `value out of range`. A non-numeric `float64` spelling
/// such as `-Inf` is rejected as `invalid syntax`, which is what `ParseInt`
/// reports for a first character that is not a digit or sign.
///
/// # Errors
/// Go's conversion failure carrying the driver type, the quoted spelling that
/// failed (already `strconv.Quote`-formatted, so `Display` adds no quotes) and
/// the column name.
fn go_parse_int64(
    column: &str,
    driver_type: &str,
    parse_text: &str,
    quoted: String,
) -> SqliteResult<i64> {
    let reason = match parse_text.parse::<i64>() {
        Ok(value) => return Ok(value),
        Err(err) => match err.kind() {
            std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow => {
                "value out of range"
            }
            _ => "invalid syntax",
        },
    };
    Err(SqliteError::FromSqlConversionFailure(
        0,
        Type::Integer,
        Box::new(GoToInt64 {
            column: column.to_string(),
            driver_type: driver_type.to_string(),
            quoted,
            reason,
        }),
    ))
}

/// The boxed Go-parity payload for a driver value whose decimal spelling is not an
/// `int64` (`database/sql/convert.go:505`, same form at `:517`). `quoted` is already
/// `strconv.Quote`-formatted, exactly like Go's `%q` of the `asString` text.
#[derive(Debug)]
struct GoToInt64 {
    column: String,
    driver_type: String,
    quoted: String,
    reason: &'static str,
}

impl std::fmt::Display for GoToInt64 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "converting driver.Value type {} ({}) to a int64: {} (column `{}`)",
            self.driver_type, self.quoted, self.reason, self.column
        )
    }
}

impl std::error::Error for GoToInt64 {}

/// The boxed Go-parity payload for a driver value whose `asString` spelling is
/// not a `float64` (`database/sql/convert.go:529`).
#[derive(Debug)]
struct GoToFloat64 {
    column: String,
    driver_type: String,
    quoted: String,
    reason: &'static str,
}

impl std::fmt::Display for GoToFloat64 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "converting driver.Value type {} ({}) to a float64: {} (column `{}`)",
            self.driver_type, self.quoted, self.reason, self.column
        )
    }
}

impl std::error::Error for GoToFloat64 {}

/// The boxed Go-parity payload for `driver.Bool`'s rejection
/// (`database/sql/driver/types.go:75, :93, :99, :102`).
#[derive(Debug)]
struct GoToBool {
    column: String,
    described: String,
}

impl std::fmt::Display for GoToBool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "sql/driver: couldn't convert {} into type bool (column `{}`)",
            self.described, self.column
        )
    }
}

impl std::error::Error for GoToBool {}
/// A joined `*time.Time` column as the model's wire string: absent → `None`,
/// SQL NULL → `None` (Go's nil pointer), an undecodable cell →
/// `Some(0001-01-01T00:00:00Z)` (a pointer to Go's zero time), a real stamp →
/// Go's RFC3339Nano text WITH THE ZONE THE DRIVER PARSED, which is why this reads
/// [`RowGet::get_gotime`] rather than the UTC-normalised [`RowGet::get_opt_dt`]:
/// `2024-02-02 03:00:00+03:00` in a `datetime` column marshals as
/// `2024-02-02T03:00:00+03:00`, not as `2024-02-01T21:00:00Z`.
fn optional_dt_text(row: &Row<'_>, column: &str) -> SqliteResult<Option<String>> {
    if !row.has_column(column) {
        return Ok(None);
    }
    Ok(row.get_gotime(column)?.map(|t| t.to_rfc3339_nano()))
}

/// Go's `encoding/json` text for a `time.Time` — `time.RFC3339Nano` in UTC:
/// `T`-separated, `Z` suffix, fractional seconds present only when non-zero and
/// with trailing zeros trimmed (Go trims them; chrono's `%.f` does the same).
/// A UTC stamp is the zero value rendered as `0001-01-01T00:00:00Z`, which is
/// what a `*time.Time` pointing at `time.Time{}` marshals to.
#[must_use]
pub fn to_go_time_text(dt: &DateTime<Utc>) -> String {
    // Go's RFC3339Nano prints the fractional second with trailing zeros trimmed
    // and no fraction at all when it is zero; chrono's `%.f` keeps the zeros, so
    // render the full nanosecond field and trim it here.
    let text = dt.format(GO_RFC3339_NANO).to_string();
    let Some(dot) = text.find('.') else {
        return text;
    };
    let (head, fraction_and_z) = text.split_at(dot);
    let fraction = fraction_and_z.trim_end_matches('Z').trim_end_matches('0');
    if fraction == "." || fraction.is_empty() {
        format!("{head}Z")
    } else {
        format!("{head}{fraction}Z")
    }
}

/// The `owner_name` alias, tolerant of projections that do not JOIN `user`
/// (`select_playlist_sql` always emits it, a caller-side projection may not).
/// Go's `ScanStruct` gives an unmatched column a throwaway `NullString`
/// (`rows.go:81-86`), so an absent alias leaves `OwnerName` at its zero value —
/// the empty string — rather than failing.
///
/// # Errors
/// Propagates Go's NULL-into-non-pointer-string failure when the alias is
/// present but NULL.
fn owner_name_column(row: &Row<'_>) -> SqliteResult<String> {
    if !row.has_column("owner_name") {
        return Ok(String::new());
    }
    row.get_text("owner_name")
}

/// `dbPlaylist.PostScan` (phase 2, `playlist_repository.go:29-34`): decode the
/// raw `rules` column onto the record.
///
/// Go returns the decode error **un-wrapped** (`:31` — no `fmt.Errorf`), so the
/// payload here is the criteria codec's own message with no prefix invented;
/// [`crate::hydrate`]'s module docs record that this hook has no folder-style
/// prefix constant.
///
/// # Errors
/// [`HydrateError::Decoded`] carrying the codec's raw message.
pub fn playlist_post_scan(raw: PlaylistRow) -> Result<PlaylistRecord, HydrateError> {
    let mut record = raw.record;
    record.rules = parse_playlist_rules(raw.rules.as_deref())
        .map_err(|err| HydrateError::Decoded(err.to_string()))?;
    Ok(record)
}

/// `PostScan`'s decode (`playlist_repository.go:30-32`) through the landed
/// [`nd_model::criteria::unmarshal_criteria_str`] with Go's default field map —
/// Go's path is `json.Unmarshal(data, &p.Playlist.Rules)` →
/// `Criteria.UnmarshalJSON` → `unmarshalCriteriaStr(DefaultFieldMap, …)`
/// (`model/criteria/criteria.go:113-121`).
///
/// The three ways Go ends up with `Rules == nil` all decode to `None`: SQL NULL
/// (`NullString.Valid == false` ⇒ `String == ""`), the empty cell (the guard at
/// `:30`), and a literal JSON `null` (`json.Unmarshal` into a
/// `**criteria.Criteria` leaves the pointer nil with no error). Anything else
/// is decoded; a payload Go rejects (`all`+`any` conflict, no rules, bad slot)
/// is the codec's error. Because the codec reproduces Go's rejection of an empty
/// top-level group (`criteria.go:134-140`), a successful decode is always
/// smart per [`crate::playlist_repo::is_smart_playlist`] — the documented
/// empty-`All` stand-in cannot be produced by this decoder.
///
/// # Errors
/// [`nd_model::CriteriaError`] — the failure Go's `json.Unmarshal` returns bare.
pub fn parse_playlist_rules(
    json: Option<&str>,
) -> Result<Option<nd_model::Criteria>, nd_model::CriteriaError> {
    let payload = match json {
        None => return Ok(None),
        Some(text) => text,
    };
    // Go's scanner accepts only the four JSON whitespace bytes (SP, TAB, CR, LF);
    // Unicode `trim` would incorrectly accept NBSP/VT/etc. around literal null.
    let json_trimmed = payload.trim_matches(|ch| matches!(ch, ' ' | '\t' | '\r' | '\n'));
    if payload.is_empty() || json_trimmed == "null" {
        return Ok(None);
    }
    unmarshal_criteria_str(&default_field_map(), payload.to_string()).map(Some)
}
// ---------------------------------------------------------------------------
// S-D3b: album + mediafile — the PostScan hooks of
// `persistence/album_repository.go:41-69` and
// `persistence/mediafile_repository.go:49-67`.
// ---------------------------------------------------------------------------

/// `dbAlbum` row mapping (phase 1): the columns `selectAlbum` emits
/// (`persistence/album_repository.go:227-232` — `album.*`, the LEFT-joined
/// `library.path as library_path`/`library.name as library_name`, and
/// `withAnnotation`'s six columns plus `album.average_rating`
/// (`sql_annotations.go:63-91`)), with the four JSON shim columns **still
/// unparsed** on the returned [`crate::album_repo::AlbumRow`].
///
/// Shim reads: all four are Go's non-pointer `string`
/// (`album_repository.go:31-34`), so [`RowGet::get_text`] — SQL NULL fails the scan,
/// the same failure Go's `converting NULL to string is unsupported` reports (Rust's
/// payload names the column rather than reproducing that wording; see the module
/// `## Errors` note). The DDL gives each shim a NOT NULL default
/// (`schema_ddl.rs:58`), so an empty string is the
/// "shim column not scanned" case, which phase 2's guards treat as the skip
/// branch.
///
/// Absent-column tolerance: the annotation columns come from a LEFT JOIN that the
/// anonymous (no-user) base read does not emit at all, so [`row_to_annotation`] probes each with
/// [`RowGet::has_column`] and decodes an absent one to its zero. The two library
/// display columns are likewise LEFT-joined aliases: an absent alias leaves Go's
/// zero (empty string) — dbx hands an unmatched column a throwaway `NullString`
/// (`dbx@v1.12.0/rows.go:80-86`) — while a PRESENT alias holding NULL errors, like
/// Go's non-pointer `string` (`model/album.go:21-22`). See `join_text`.
///
/// `rg_album_gain`/`rg_album_peak` are read straight from their columns: Go's
/// `RgAlbumGain`/`RgAlbumPeak` shim fields exist only because dbx's field-map cannot
/// render `RGAlbumGain` to `rg_album_gain` (`album_repository.go:37-38`), and the
/// `*float64` pointer means NULL → `None` — the same shape as
/// [`RowGet::get_opt_f64`]. The unconditional copies at `:66-67` therefore have
/// nothing to do here (see [`album_post_scan`]).
///
/// `created_at`/`updated_at` are Go non-pointer `time.Time` →
/// [`RowGet::get_dt`] (SQL NULL fails the scan, an unparseable cell — the DDL's
/// `'0000-00-00 00:00:00'` — scans as Go's zero), and this record spells Go's zero
/// as `None`. `imported_at` is NOT NULL and the record keeps the scanned value as-is
/// (`DateTime<Utc>`, no `None` spelling). `external_info_updated_at` is
/// `*time.Time` → [`RowGet::get_opt_dt`] (NULL → `None`, unparseable →
/// `Some(`[`go_zero_time`]`)` — a pointer to Go's zero).
///
/// # Errors
/// A missing or NULL *required* album column fails, per Go parity. The four shim
/// columns are the exception: they are read through `join_text`, so an absent
/// one is Go's zero value rather than an error.
pub fn row_to_album(row: &Row<'_>) -> SqliteResult<AlbumRow> {
    Ok(AlbumRow {
        discs: join_text(row, "discs")?,
        participants: join_text(row, "participants")?,
        tags: join_text(row, "tags")?,
        folder_ids: join_text(row, "folder_ids")?,
        record: AlbumRecord {
            // The row's own primary key (`Album.ID` scans `id` by field name).
            id: row.get_text("id")?,
            library_id: row.get_i64("library_id")?,
            name: row.get_text("name")?,
            embed_art_path: row.get_text("embed_art_path")?,
            album_artist: row.get_text("album_artist")?,
            album_artist_id: row.get_text("album_artist_id")?,
            min_year: row.get_i32("min_year")?,
            max_year: row.get_i32("max_year")?,
            date: row.get_text("date")?,
            min_original_year: row.get_i32("min_original_year")?,
            max_original_year: row.get_i32("max_original_year")?,
            original_date: row.get_text("original_date")?,
            release_date: row.get_text("release_date")?,
            compilation: row.get_bool("compilation")?,
            comment: row.get_text("comment")?,
            song_count: row.get_i32("song_count")?,
            duration: row.get_f64("duration")?,
            size: row.get_i64("size")?,
            // Decoded by phase 2 from the raw shim column.
            discs: None,
            sort_album_name: row.get_text("sort_album_name")?,
            sort_album_artist_name: row.get_text("sort_album_artist_name")?,
            order_album_name: row.get_text("order_album_name")?,
            order_album_artist_name: row.get_text("order_album_artist_name")?,
            catalog_num: row.get_text("catalog_num")?,
            mbz_album_id: row.get_text("mbz_album_id")?,
            mbz_album_artist_id: row.get_text("mbz_album_artist_id")?,
            mbz_album_type: row.get_text("mbz_album_type")?,
            mbz_album_comment: row.get_text("mbz_album_comment")?,
            mbz_release_group_id: row.get_text("mbz_release_group_id")?,
            // Decoded by phase 2 from the raw shim column.
            folder_ids: None,
            explicit_status: row.get_text("explicit_status")?,
            // `*float64` read from Go's `Rg*` shim column; spelling the real column
            // name directly is exactly what that shim exists to achieve.
            rg_album_gain: row.get_opt_f64("rg_album_gain")?,
            rg_album_peak: row.get_opt_f64("rg_album_peak")?,
            description: row.get_text("description")?,
            small_image_url: row.get_text("small_image_url")?,
            medium_image_url: row.get_text("medium_image_url")?,
            large_image_url: row.get_text("large_image_url")?,
            external_url: row.get_text("external_url")?,
            external_info_updated_at: row.get_opt_dt("external_info_updated_at")?,
            // The COLUMN value. Phase 2 overwrites it only when the tags shim is
            // non-empty (`album_repository.go:52-57`: the guard, then its `ToGenres`
            // assignment), so an empty shim leaves
            // whatever the column says.
            genre: row.get_text("genre")?,
            // Decoded by phase 2 from the raw shim column.
            tags: BTreeMap::new(),
            // Decoded by phase 2 from the raw shim column.
            participants: BTreeMap::new(),
            // Derived from the decoded participants in phase 2.
            participant_names: Vec::new(),
            missing: row.get_bool("missing")?,
            imported_at: row.get_dt("imported_at")?,
            created_at: go_zero_or_none(row, "created_at")?,
            updated_at: go_zero_or_none(row, "updated_at")?,
            // Join-only (`structs:"-"`): read onto the record, never written.
            library_path: join_text(row, "library_path")?,
            library_name: join_text(row, "library_name")?,
            annotation: row_to_annotation(row)?,
            // Derived by phase 2 from the decoded tags.
            genres: Vec::new(),
            // S-D3c: Go's `dbAlbum.PostScan` reads NO artwork column — the embedded
            // ItemImage (`model/album.go:17`) is filled by the SECOND hydration layer
            // (`persistence/artwork_hydration.go:79-90`, ported in
            // `crate::artwork_hydration`), so this phase leaves it at Go's zero value,
            // which means UNRESOLVED rather than known-absent.
            image: crate::artwork_hydration::ItemImage::default(),
        },
    })
}

/// A Go non-pointer `time.Time` column, read into the record's
/// `Option<DateTime<Utc>>`. Introduced for the album/mediafile reads and reused by
/// the pre-existing folder/playlist `created_at` readers — six columns across four
/// tables, each `Option<DateTime<Utc>>` on its record. Two arms, deliberately
/// different:
///
/// * **SQL NULL errors the read** (`get_dt`), which IS Go parity: Go scans the same
///   cell into a non-pointer `time.Time` and `database/sql` fails it.
/// * **A cell that scans as Go's zero time becomes `None`, not an error and not a
///   zero stamp.** This is a record-shape divergence, recorded in
///   `hydrate-contract.md`'s residual list: Go's non-pointer `time.Time` holds
///   year-1 midnight (`IsZero() == true`, `err == nil`), and these records have no
///   zero-time value to hold — `None` is their only "never stamped" spelling.
///   `is_go_zero`/`go_zero_time` keep Go's semantics explicit where a record CAN
///   represent zero (`folder.updated_at`, annotation stamps).
///
/// A NULL cell normally means a foreign or legacy row: the mediafile, folder and
/// playlist writers stamp `created_at` when the record holds `None`
/// (`mediafile_repo::put_sql` at `mediafile_repository.go:185-187`, which Go's `Put`
/// mirrors, and `folder_repo::put_sql`/`playlist_repo::put_sql` follow the same
/// precedent), so those writes never store NULL. Album's `Put` stamps only
/// `imported_at` (`album_repository.go:208-219`), so a NULL `album.created_at` is
/// reachable in principle — but Go reads the same cell into a non-pointer
/// `time.Time` and fails there too, which is the behavior this reproduces. Erroring
/// on NULL is therefore Go parity, not a Rust-only hazard — Go's driver reports
/// `sql: Scan error on column index 0, name "created_at": unsupported Scan, storing
/// driver.Value type <nil> into type *time.Time` (verified probe), and the executed
/// put→hydrate round-trips in `tests/runtime_hydrate_album_mediafile.rs` show the
/// write and read paths agree for both entities.
///
/// # Errors
/// Propagates the NULL-into-non-pointer scan failure.
fn go_zero_or_none(row: &Row<'_>, column: &str) -> SqliteResult<Option<DateTime<Utc>>> {
    row.get_dt(column)
        .map(|dt| if is_go_zero(&dt) { None } else { Some(dt) })
}

/// A TEXT column dbx + Go read into a **non-pointer `string`**: absent → Go's zero
/// value (empty string, because the column gets a throwaway `sql.NullString` and is
/// discarded — `dbx@v1.12.0/rows.go:80-86`); present-and-NULL → Go's scan failure;
/// present → the text.
///
/// LEFT-joined display aliases (`library_path`, `library_name`, `owner_name`) are
/// the plain case: the alias is only emitted by the join, so a light query leaves
/// Go's zero value.
///
/// The JSON **shim** columns (`discs`, `participants`, `tags`, `folder_ids`) use the
/// same accessor for a different reason: dbx binds a reference only for a column its
/// field map names (`dbx@v1.12.0/rows.go:80-86`), so a projection that omits a shim
/// leaves the Go field at its zero value, `""`. What that zero value then does depends
/// on the hook: the GUARDED shims take `PostScan`'s `!= ""` skip branch
/// (`album_repository.go:43`/`:52`/`:59`, `mediafile_repository.go:59`), but the
/// UNGUARDED `participants` decode does NOT skip — `unmarshalParticipants("")` fails
/// with Go's media-file/album prefix (`album_repository.go:48`,
/// `mediafile_repository.go:55-58`), so an omitted `participants` column is an ERROR,
/// not an empty map. Pinned by
/// `runtime_hydrate_playlist_track_scrobble.rs::a_projection_without_the_participants_shim_still_fails_the_unguarded_decode`.
/// The light projections (`getAllIDs`, the rowid pick) never reach a hook at all: they
/// scan a plain `[]string`/`[]int64` through `queryAllSlice` (`sql_base_repository.go:460-466`)
/// → dbx `Query.Column` (`query.go:308-316`) → `Rows.column` (`rows.go:200-233`), which scans
/// each row into a bare element and never calls `ScanStruct`, so there is no `PostScanner`
/// call. A present shim cell keeps Go's non-pointer `string` behavior: SQL NULL aborts the
/// read.
///
/// # Errors
/// Propagates Go's NULL-into-non-pointer-string failure when the column is present
/// but NULL.
fn join_text(row: &Row<'_>, column: &str) -> SqliteResult<String> {
    if !row.has_column(column) {
        return Ok(String::new());
    }
    row.get_text(column)
}

/// `dbAlbum.PostScan` (phase 2, `persistence/album_repository.go:41-69`), in Go's
/// exact statement order and with Go's guards:
///
/// 1. `discs` — guarded non-empty (`:43`), decoded by
///    [`crate::sql_json::unmarshal_discs`], wrapped with [`ALBUM_DISCS_PREFIX`] (`:45`).
/// 2. `participants` — **UNGUARDED** (`:48`): even the empty string goes through
///    `unmarshalParticipants`, which rejects it, and the failure carries
///    [`ALBUM_PREFIX`] (`:50`). Also fills the record's FTS participant-name input
///    from the same decoded map
///    ([`crate::album_repo::participant_names_from_participants`]).
/// 3. `tags` — guarded non-empty (`:52`), decoded by
///    [`crate::sql_tags::unmarshal_tags`] then [`crate::sql_tags::tags_to_genres`],
///    which OVERWRITES `genre` and `genres` (`:57`). Wrapped with [`ALBUM_PREFIX`] (`:55`).
/// 4. `folder_ids` — guarded non-empty (`:59`), decoded by
///    [`crate::sql_json::unmarshal_nullable_strings`], wrapped with
///    [`ALBUM_FOLDER_IDS_PREFIX`] (`:62`).
/// 5. The two unconditional `RG*` copies (`:66-67`). **No shim to copy in Rust:**
///    Go's `RgAlbumGain`/`RgAlbumPeak` shim fields exist only because dbx's default
///    field-map cannot render `RGAlbumGain` to `rg_album_gain`
///    (`album_repository.go:37-38`); [`row_to_album`] reads `rg_album_gain`/
///    `rg_album_peak` straight onto the record's real fields, so these assignments are
///    identity no-ops. Documented rather than emulated with a fake shim.
///
/// # Errors
/// [`HydrateError::Decoded`] carrying one of Go's EXACT prefixes above.
pub fn album_post_scan(raw: AlbumRow) -> Result<AlbumRecord, HydrateError> {
    let mut record = raw.record;

    // 1. discs — Go's `if a.Discs != ""` guard (`:43`).
    if !raw.discs.is_empty() {
        record.discs = unmarshal_discs(&raw.discs)
            .map_err(|err| HydrateError::Decoded(format!("{ALBUM_DISCS_PREFIX}{err}")))?;
    }

    // 2. participants — no guard in Go (`:48`), so the empty string is decoded too.
    let (participants, err) = unmarshal_participants(&raw.participants);
    if !err.is_empty() {
        return Err(HydrateError::Decoded(format!("{ALBUM_PREFIX}{err}")));
    }
    record.participant_names = participant_names_from_participants(&participants);
    record.participants = participants;

    // 3. tags — guarded (`:52`); the decode also derives genre/genres (`:57`).
    if !raw.tags.is_empty() {
        let (tags, err) = unmarshal_tags(raw.tags.clone());
        if !err.is_empty() {
            return Err(HydrateError::Decoded(format!("{ALBUM_PREFIX}{err}")));
        }
        record.tags = tags;
        let (genre, genres) = tags_to_genres(&record.tags);
        record.genre = genre;
        record.genres = genres;
    }

    // 4. folder_ids — guarded (`:59`).
    if !raw.folder_ids.is_empty() {
        record.folder_ids = unmarshal_nullable_strings(&raw.folder_ids)
            .map_err(|err| HydrateError::Decoded(format!("{ALBUM_FOLDER_IDS_PREFIX}{err}")))?;
    }

    // 5. `a.Album.RGAlbumGain = a.RgAlbumGain`/`…Peak…` (`:66-67`): identity in Rust,
    // since row_to_album reads rg_* straight onto the real fields. Nothing to assign.
    Ok(record)
}

/// `dbMediaFile` row mapping (phase 1): the columns `selectMediaFile` emits
/// (`persistence/mediafile_repository.go:204-210` — `media_file.*`, the two
/// LEFT-joined library aliases, `withAnnotation`'s columns plus
/// `media_file.average_rating`, and — for a logged-in user ONLY —
/// `coalesce(position, 0) as bookmark_position` from `withBookmark`
/// (`sql_bookmarks.go:18-28`)), with the two JSON shim columns **still unparsed**.
///
/// `bookmark_position` is read behind [`RowGet::has_column`]: the anonymous (no-user)
/// base read omits the column entirely (`sql_bookmarks.go:20-22` returns the query
/// unchanged), and dbx discards unmatched columns (`dbx@v1.12.0/rows.go:80-86`), so
/// absence decodes to Go's zero (`0`), not an error. A projection that also drops
/// base-table columns (the id/rowid-only ones) is not hydratable on either side.
/// The coalesce means a present column is never NULL.
///
/// `participants`/`tags` are Go's non-pointer `string` shims
/// (`mediafile_repository.go:31-32`) → [`RowGet::get_text`], so NULL aborts the scan
/// for the same reason Go's `converting NULL to string is unsupported` does.
///
/// `bit_depth`/`bpm` are Go `*int` (`model/mediafile.go:67,84`) → NULL yields `None`;
/// every other plain numeric/string column is non-pointer and errors on NULL.
/// `birth_time` is Go's non-pointer `time.Time` and NOT NULL in the DDL →
/// [`RowGet::get_dt`]; `created_at`/`updated_at` are nullable and spell Go's zero as
/// `None`, exactly like the album pair.
///
/// # Errors
/// A missing or NULL *required* media_file column fails, per Go parity.
pub fn row_to_mediafile(row: &Row<'_>) -> SqliteResult<MediaFileRow> {
    Ok(MediaFileRow {
        participants: join_text(row, "participants")?,
        tags: join_text(row, "tags")?,
        // `None` reads the record's `id` from the row, as Go scans `MediaFile.ID`
        // by field name. S-D3e's readers pass an id override instead — see
        // [`apply_embedded_mediafile_id`].
        record: mediafile_record_from_row(row, None)?,
    })
}

/// The media-file column set shared by [`row_to_mediafile`] and the two S-D3e
/// delegating readers, so the projection, read ORDER and NULL behavior have one
/// source of truth and cannot drift between a `media_file` read and a track or
/// scrobble read over the same joined columns.
///
/// `entry_id` is the outer entity's id — the value dbx binds for the DUPLICATED
/// `id` name, i.e. the playlist position or the scrobble entry id. Passing it is
/// what keeps the raw record faithful to Go: Go's `MediaFile.ID` is bound by
/// nothing and therefore starts as the empty `string`, and the only assignment it
/// ever receives is [`apply_embedded_mediafile_id`] in the hook. The seed is
/// deliberately NOT the media file's own `id`, which Go never reads, and NOT
/// `None`, which would read the FIRST `id` column (`m.id`/`f.id`) — a cell Go
/// binds to neither field. `None` is [`row_to_mediafile`]'s case, where `id`
/// genuinely is the media file's primary key.
///
/// # Errors
/// A missing or NULL *required* media_file column fails, per Go parity.
fn mediafile_record_from_row(
    row: &Row<'_>,
    entry_id: Option<&str>,
) -> SqliteResult<MediaFileRecord> {
    Ok(MediaFileRecord {
        // Either the row's own primary key (`MediaFile.ID` scans `id` by field
        // name), or the outer entity's id — the cell that actually wins the
        // duplicated name, and which the hook replaces with `media_file_id`.
        id: match entry_id {
            Some(value) => value.to_string(),
            None => row.get_text("id")?,
        },
        album: row.get_text("album")?,
        album_artist: row.get_text("album_artist")?,
        album_artist_id: row.get_text("album_artist_id")?,
        album_id: row.get_text("album_id")?,
        artist: row.get_text("artist")?,
        artist_id: row.get_text("artist_id")?,
        birth_time: row.get_dt("birth_time")?,
        // Go `*int` → NULL is the nil pointer.
        bit_depth: row.get_opt_i32("bit_depth")?,
        bit_rate: row.get_i32("bit_rate")?,
        bpm: row.get_opt_i32("bpm")?,
        catalog_num: row.get_text("catalog_num")?,
        channels: row.get_i32("channels")?,
        codec: row.get_text("codec")?,
        comment: row.get_text("comment")?,
        compilation: row.get_bool("compilation")?,
        created_at: go_zero_or_none(row, "created_at")?,
        date: row.get_text("date")?,
        disc_number: row.get_i32("disc_number")?,
        disc_subtitle: row.get_text("disc_subtitle")?,
        duration: row.get_f64("duration")?,
        explicit_status: row.get_text("explicit_status")?,
        folder_id: row.get_text("folder_id")?,
        // The COLUMN value; phase 2 overwrites it only when tags is non-empty.
        genre: row.get_text("genre")?,
        has_cover_art: row.get_bool("has_cover_art")?,
        library_id: row.get_i64("library_id")?,
        lyrics: row.get_text("lyrics")?,
        mbz_recording_id: row.get_text("mbz_recording_id")?,
        mbz_release_track_id: row.get_text("mbz_release_track_id")?,
        mbz_album_id: row.get_text("mbz_album_id")?,
        mbz_release_group_id: row.get_text("mbz_release_group_id")?,
        mbz_artist_id: row.get_text("mbz_artist_id")?,
        mbz_album_artist_id: row.get_text("mbz_album_artist_id")?,
        mbz_album_type: row.get_text("mbz_album_type")?,
        mbz_album_comment: row.get_text("mbz_album_comment")?,
        missing: row.get_bool("missing")?,
        order_album_artist_name: row.get_text("order_album_artist_name")?,
        order_album_name: row.get_text("order_album_name")?,
        order_artist_name: row.get_text("order_artist_name")?,
        order_title: row.get_text("order_title")?,
        original_date: row.get_text("original_date")?,
        original_year: row.get_i32("original_year")?,
        path: row.get_text("path")?,
        pid: row.get_text("pid")?,
        probe_data: row.get_text("probe_data")?,
        release_date: row.get_text("release_date")?,
        release_year: row.get_i32("release_year")?,
        // `*float64` read from Go's `Rg*` shim columns — the copies at
        // `mediafile_repository.go:50-53` are identity no-ops here.
        rg_album_gain: row.get_opt_f64("rg_album_gain")?,
        rg_album_peak: row.get_opt_f64("rg_album_peak")?,
        rg_track_gain: row.get_opt_f64("rg_track_gain")?,
        rg_track_peak: row.get_opt_f64("rg_track_peak")?,
        sample_rate: row.get_i32("sample_rate")?,
        size: row.get_i64("size")?,
        sort_album_artist_name: row.get_text("sort_album_artist_name")?,
        sort_album_name: row.get_text("sort_album_name")?,
        sort_artist_name: row.get_text("sort_artist_name")?,
        sort_title: row.get_text("sort_title")?,
        suffix: row.get_text("suffix")?,
        // Decoded by phase 2 from the raw shim column.
        tags: BTreeMap::new(),
        title: row.get_text("title")?,
        track_number: row.get_i32("track_number")?,
        year: row.get_i32("year")?,
        // Decoded by phase 2 from the raw shim column.
        participants: BTreeMap::new(),
        // Derived from the decoded participants in phase 2.
        participant_names: Vec::new(),
        updated_at: go_zero_or_none(row, "updated_at")?,
        // Join-only (`structs:"-"`): read onto the record, never written.
        library_path: join_text(row, "library_path")?,
        library_name: join_text(row, "library_name")?,
        annotation: row_to_annotation(row)?,
        // `withBookmark` emits the column only for a logged-in user; an absent
        // column is Go's zero value, exactly as dbx discarding it leaves it.
        bookmark_position: if row.has_column("bookmark_position") {
            row.get_i64("bookmark_position")?
        } else {
            0
        },
        // Derived by phase 2 from the decoded tags.
        genres: Vec::new(),
        // S-D3c: same rule as the album — PostScan reads no artwork column. Go's
        // `ItemImage` embed and `MediaFile.AlbumImage` (`model/mediafile.go:30,34`)
        // are filled by `crate::artwork_hydration::hydrate_media_file_artwork`, so
        // this phase leaves both at Go's zero value (= UNRESOLVED, not absent).
        image: crate::artwork_hydration::ItemImage::default(),
        album_image: crate::artwork_hydration::ItemImage::default(),
    })
}

/// `dbMediaFile.PostScan` (phase 2, `persistence/mediafile_repository.go:49-67`), in
/// Go's exact statement order:
///
/// 1. The four unconditional `RG*` copies (`:50-53`). **No shim to copy in Rust** —
///    the same identity no-op as the album hook's `:66-67`, because
///    [`row_to_mediafile`] reads `rg_track_gain`/`rg_track_peak`/`rg_album_gain`/
///    `rg_album_peak` straight onto the record's real fields. Documented rather than
///    emulated with a fake shim.
/// 2. `participants` — **UNGUARDED** (`:55`): the empty string is decoded and
///    rejected, and the failure carries [`MEDIAFILE_PREFIX`] (`:57`). Also fills the
///    record's FTS participant-name input from the same decoded map.
/// 3. `tags` — guarded non-empty (`:59`), decoded then
///    [`crate::sql_tags::tags_to_genres`], which OVERWRITES `genre`/`genres` (`:64`),
///    wrapped with [`MEDIAFILE_PREFIX`] (`:62`).
///
/// # Errors
/// [`HydrateError::Decoded`] carrying [`MEDIAFILE_PREFIX`].
pub fn mediafile_post_scan(raw: MediaFileRow) -> Result<MediaFileRecord, HydrateError> {
    let mut record = raw.record;

    // 1. `m.RGTrackGain = m.RgTrackGain` etc. (`:50-53`): identity in Rust —
    // row_to_mediafile reads rg_* straight onto the real fields. Nothing to assign.

    // 2. participants — no guard in Go (`:55`).
    let (participants, err) = unmarshal_participants(&raw.participants);
    if !err.is_empty() {
        return Err(HydrateError::Decoded(format!("{MEDIAFILE_PREFIX}{err}")));
    }
    record.participant_names = participant_names_from_participants(&participants);
    record.participants = participants;

    // 3. tags — guarded (`:59`); the decode also derives genre/genres (`:64`).
    if !raw.tags.is_empty() {
        let (tags, err) = unmarshal_tags(raw.tags.clone());
        if !err.is_empty() {
            return Err(HydrateError::Decoded(format!("{MEDIAFILE_PREFIX}{err}")));
        }
        record.tags = tags;
        let (genre, genres) = tags_to_genres(&record.tags);
        record.genre = genre;
        record.genres = genres;
    }

    Ok(record)
}

/// `dbPlaylistTrack` phase 1 (`persistence/playlist_track_repository.go:22-25`):
/// the `dbMediaFile` column set plus the track's own identity. `dbPlaylistTrack`
/// adds no `structs:"-"` shim of its own, so phase 2 is a pure delegation to
/// [`mediafile_post_scan`] followed by [`apply_embedded_mediafile_id`].
///
/// `position` is the LAST column named `id` — the cell dbx actually binds, because
/// `PlaylistTrack.ID` at path `[1,0]` beats `MediaFile.ID` at `[0,0,0]` and the
/// last of several same-named columns wins (module note above). In both Go selects
/// that is `playlist_tracks.id`. It goes through [`RowGet::get_text_at`] because
/// Go's destination is a `string` while `playlist_tracks.id` is INTEGER: Go's
/// `reflect.String` arm renders any non-NULL cell through `asString`
/// (`database/sql/convert.go:533-544`, `:557-578`), so position `3` becomes `"3"`
/// on both sides — executed against the real repository, where `Read(1)` yields
/// `ID == "1"` and `MediaFileID == "m1"`.
///
/// # Errors
/// A missing or NULL *required* media-file column fails, per Go parity. When a
/// projection's joined `f.*` legs come back NULL (a track whose `media_file` row is
/// gone), [`id_field_from_row`] reports Go's
/// `converting NULL to string is unsupported` naming the first NULL cell — the
/// column Go's `Scan` aborts on. Go's own `Read` hides that behind an INNER JOIN,
/// so its ghost row yields `model.ErrNotFound` instead; see
/// [`crate::playlist_track_repo::PlaylistTrackRepo::read`].
pub fn row_to_playlist_track(row: &Row<'_>) -> SqliteResult<PlaylistTrackRow> {
    // The duplicated-name `id` scan FIRST: in this projection `id` is column 0
    // (`f.*` leads), so a projection with several NULL legs aborts on the same
    // cell Go's `Scan` aborts on. The shim reads follow, keeping
    // row_to_mediafile's precedence for the remaining columns.
    let position = id_field_from_row(row)?;
    let participants = join_text(row, "participants")?;
    let tags = join_text(row, "tags")?;
    let record = mediafile_record_from_row(row, Some(&position))?;
    Ok(PlaylistTrackRow {
        participants,
        tags,
        position,
        media_file_id: row.get_text("media_file_id")?,
        playlist_id: row.get_text("playlist_id")?,
        record,
    })
}

/// `dbPlaylistTrack.PostScan` (`persistence/playlist_track_repository.go:27-34`):
/// delegate to `dbMediaFile.PostScan` verbatim, then overwrite the media file's id
/// with `media_file_id`.
///
/// The delegation is the whole hook, so the participants/tags decode, their guards
/// and [`MEDIAFILE_PREFIX`] are exactly `mediafile_post_scan`'s: a bad `participants`
/// cell reached through a track row reports Go's **media-file** sentence
/// (`parsing media_file from db: …`), not a playlist-track message, because
/// `:27-30` returns the inner error unchanged. The order is Go's too: the media-file
/// decode runs BEFORE the id overwrite.
///
/// # Errors
/// [`HydrateError::Decoded`] carrying [`MEDIAFILE_PREFIX`], or a transparent SQLite
/// error.
pub fn playlist_track_post_scan(
    raw: PlaylistTrackRow,
) -> Result<PlaylistTrackRecord, HydrateError> {
    let mut media = mediafile_post_scan(MediaFileRow {
        record: raw.record,
        participants: raw.participants,
        tags: raw.tags,
    })?;
    // `:32` — `t.PlaylistTrack.MediaFile = *t.dbMediaFile.MediaFile` copies the
    // hydrated media file onto the track (a plain struct move in Rust), and `:33`
    // overwrites its id.
    apply_embedded_mediafile_id(&mut media, &raw.media_file_id);
    Ok(PlaylistTrackRecord {
        id: raw.position,
        media_file_id: raw.media_file_id,
        playlist_id: raw.playlist_id,
        media,
    })
}

/// `dbScrobbleBuffer` phase 1 (`persistence/scrobble_buffer_repository.go:18-21`):
/// the `dbMediaFile` column set plus the entry's own columns.
///
/// `entry_id` is the LAST column named `id`: `ScrobbleEntry.ID` at path `[1,0]` is
/// the field that wins the duplicated name, and Go's `SELECT m.*, s.*` puts `s.*`
/// last precisely so `s.id` — not `m.id` — is the value that lands in it
/// (`scrobble_buffer_repository.go:66-67`: "Put `s.*` last or else `m.id` overrides
/// `s.id`"). [`id_field_from_row`] reproduces that without touching the statement; an
/// executed dbx mirror of the struct over both column orders confirms the flip
/// (`[m.*, s.*]` → entry id `tieA`, `[s.*, m.*]` → the media file's `m1`).
///
/// `play_time`/`enqueue_time` are Go's non-pointer `time.Time` fields: NULL errors,
/// and an unparseable cell in a declared `datetime` column scans as
/// [`go_zero_time`] with no error. An INTEGER cell in that declared column scans
/// through [`driver_integer_dt`] as Go does — executed: an epoch-integer `play_time`
/// comes back as `2020-09-13 12:26:40 +0000 UTC`.
///
/// # Errors
/// A missing or NULL *required* column fails. For `Next`'s LEFT JOIN over a missing
/// `media_file`, Go's `queryOne` fails on the FIRST NULL column of the projection —
/// executed against the real repository, whose logged error is
/// `sql: Scan error on column index 0, name "id": converting NULL to string is
/// unsupported` (index 0 is `m.id`, the first column of `m.*`). It is NOT
/// `model.ErrNotFound`, so `Next` does NOT treat it as an empty queue.
/// [`id_field_from_row`] surfaces Go's own sentence naming that column.
pub fn row_to_scrobble_buffer(row: &Row<'_>) -> SqliteResult<ScrobbleBufferRow> {
    // Same ordering rule as row_to_playlist_track: `m.id` is column 0 of
    // `m.*, s.*`, so the `id` scan is Go's first failure point for a projection
    // whose joined media file is gone.
    let entry_id = id_field_from_row(row)?;
    let participants = join_text(row, "participants")?;
    let tags = join_text(row, "tags")?;
    let record = mediafile_record_from_row(row, Some(&entry_id))?;
    Ok(ScrobbleBufferRow {
        participants,
        tags,
        entry_id,
        service: row.get_text("service")?,
        user_id: row.get_text("user_id")?,
        play_time: row.get_dt("play_time")?,
        enqueue_time: row.get_dt("enqueue_time")?,
        // Go scans `ScrobbleEntry.MediaFileID` from `s.media_file_id` and the hook
        // copies it onto the media file (`:28`); `m.id` is never read for it.
        media_file_id: row.get_text("media_file_id")?,
        // Seeded with the entry id because that is the only `id` cell dbx binds —
        // `MediaFile.ID` is bound by nothing, so its value comes entirely from the
        // hook's overwrite. `None` would read `m.id`, a cell Go binds to neither
        // field.
        record,
    })
}

/// `dbScrobbleBuffer.PostScan` (`persistence/scrobble_buffer_repository.go:23-30`):
/// delegate to `dbMediaFile.PostScan`, then overwrite the media file's id with
/// `media_file_id`, exactly like the playlist-track hook.
///
/// `participants` is the media file's decoded map, because in Go the entry has no
/// participants field of its own: `model.ScrobbleEntry` (`model/scrobble_buffer.go:8-16`)
/// embeds `MediaFile`, so `ScrobbleEntry.Participants` IS `MediaFile.Participants`
/// (executed: `&e.Participants == &e.MediaFile.Participants` is `true` for every entry
/// `Next` returns). `Next` then OVERWRITES that field with
/// `getParticipants`'s result (`scrobble_buffer_repository.go:84`,
/// `sql_participations.go:127-146`), which re-reads the artists and replaces each
/// participant's embedded `Artist` wholesale where its `artist.id IN (…)` select
/// returns a row. That re-read is NOT part of the PostScan contract and this port
/// does NOT perform it: [`crate::scrobble_buffer_repo::ScrobbleBufferRepo::next`]
/// stops at the delegated decode, so its entry carries the alias value BEFORE
/// Go's re-read. See that module's note for the executed replace/keep behavior of
/// the re-read.
///
/// # Errors
/// [`HydrateError::Decoded`] carrying [`MEDIAFILE_PREFIX`], or a transparent SQLite
/// error.
pub fn scrobble_buffer_post_scan(
    raw: ScrobbleBufferRow,
) -> Result<ScrobbleEntryRecord, HydrateError> {
    let mut media = mediafile_post_scan(MediaFileRow {
        record: raw.record,
        participants: raw.participants,
        tags: raw.tags,
    })?;
    apply_embedded_mediafile_id(&mut media, &raw.media_file_id);
    // Go's alias is ONE shared field; Rust has value semantics, so the entry's
    // copy is a clone of the media file's decoded map (identical content, and
    // pinned equal by a test). This is what the caller sees until Go's
    // `getParticipants` re-read replaces it — which this port does not perform.
    let participants = media.participants.clone();
    Ok(ScrobbleEntryRecord {
        id: raw.entry_id,
        service: raw.service,
        user_id: raw.user_id,
        play_time: raw.play_time,
        enqueue_time: raw.enqueue_time,
        media_file_id: raw.media_file_id,
        media,
        participants,
    })
}

/// The single statement both S-D3e hooks share:
/// `t.PlaylistTrack.MediaFile.ID = t.MediaFileID`
/// (`playlist_track_repository.go:32`) and
/// `t.ScrobbleEntry.MediaFile.ID = t.MediaFileID`
/// (`scrobble_buffer_repository.go:28`). The media file's identity comes from the
/// junction column, never from the joined media-file row's own `id`.
///
/// It is unconditional and unguarded, so an empty `media_file_id` legitimately
/// yields a media file with an empty id; there is no validation to add.
pub fn apply_embedded_mediafile_id(media: &mut MediaFileRecord, media_file_id: &str) {
    media.id = media_file_id.to_string();
}

/// `dbArtist` phase 1 (`artist_repository.go:32-36`): the artist columns the
/// stats select emits, with the two `PostScan` inputs left as raw text and the
/// artwork slot left zero.
///
/// Two `artist.*` columns are deliberately NOT read — `full_text` and
/// `search_normalized` have no destination field on `dbArtist`
/// (`model/artist.go:14-47` has neither), so dbx binds them to a throwaway
/// `sql.NullString` and discards them (`dbx@v1.12.0/rows.go:80-86`).
/// [`RowGet::has_column`] guards below are what makes a projection that OMITS
/// them (or a light query that omits the annotation columns) hydrate to Go's
/// zero values instead of failing.
///
/// `similar_artists` is the one shim that needs no extra slot: it is a real
/// column on `model.Artist` (`:38`), so its text lands on
/// [`ArtistRecord::similar_artists`] — where the write map needs it — and
/// phase 2 decodes it into [`ArtistRecord::similar`].
///
/// `image` stays [`crate::artwork_hydration::ItemImage::default()`]: Go's
/// `PostScan` reads no artwork column at all; the embedded `ItemImage`
/// (`model/artist.go:16`) is filled by the SECOND layer
/// (`artwork_hydration.go:79-90`, ported as
/// [`crate::artwork_hydration::hydrate_artist_artwork`]), so zero here means
/// UNRESOLVED, not known-absent.
///
/// # Errors
/// Propagates Go's NULL-into-non-pointer-string failure for a required TEXT
/// column, and the int/bool narrowing failures.
pub fn row_to_artist(row: &Row<'_>) -> SqliteResult<ArtistRow> {
    Ok(ArtistRow {
        library_stats_json: join_text(row, "library_stats_json")?,
        record: ArtistRecord {
            // The row's own primary key (`Artist.ID` scans `id` by field name).
            id: row.get_text("id")?,
            name: row.get_text("name")?,
            sort_artist_name: row.get_text("sort_artist_name")?,
            order_artist_name: row.get_text("order_artist_name")?,
            mbz_artist_id: row.get_text("mbz_artist_id")?,
            biography: row.get_text("biography")?,
            small_image_url: row.get_text("small_image_url")?,
            medium_image_url: row.get_text("medium_image_url")?,
            large_image_url: row.get_text("large_image_url")?,
            external_url: row.get_text("external_url")?,
            // The raw column text; phase 2 decodes it into `similar`.
            similar_artists: join_text(row, "similar_artists")?,
            // Go's `*time.Time`: NULL is a nil pointer, so this one column is
            // optional (`artist.go:39`, DDL `external_info_updated_at datetime`).
            external_info_updated_at: row.get_opt_dt("external_info_updated_at")?,
            missing: row.get_bool("missing")?,
            uploaded_image: row.get_text("uploaded_image")?,
            // Go's `*time.Time` (`artist.go:45-46`) — a nil pointer, NOT Go's
            // zero time — so SQL NULL is `None` and `get_opt_dt` is the right
            // accessor. Unlike the album/mediafile/folder/playlist reads, which
            // embed a NON-pointer `time.Time` and take [`go_zero_or_none`].
            created_at: row.get_opt_dt("created_at")?,
            updated_at: row.get_opt_dt("updated_at")?,
            // ---- hydration-only state, all filled by phase 2 / layer 3 ----
            // Go creates the map unconditionally at `:44`; the fold below fills it.
            stats: BTreeMap::new(),
            size: 0,
            album_count: 0,
            song_count: 0,
            // The promoted `Annotations` embed, read by field name despite
            // `structs:"-"` (`dbx@v1.12.0/struct.go:189-206` descends into
            // anonymous embeds). Reused, not reimplemented.
            annotation: row_to_annotation(row)?,
            image: crate::artwork_hydration::ItemImage::default(),
            similar: Vec::new(),
        },
    })
}

/// The `map<library_id, map<role, map<stat_key, int64>>>` shape Go decodes
/// into (`var rawLibStats map[string]map[string]map[string]int64`,
/// `artist_repository.go:47`). Every level is an `Option`, because Go's
/// decoder accepts a `null` at any nesting level and leaves that level's
/// Go value zero (`err == nil`) while serde's derive rejects it as a type
/// mismatch. Same established style as
/// [`crate::sql_tags::decode_tags`] and
/// [`crate::sql_participations::decode_participants`] — value-level decoding
/// over [`JsonValue`], with Go's `cannot unmarshal …` wording for a
/// value-level rejection and serde's own text for a parse failure.
/// The shape Go decodes the alias into: `map[string]map[string]map[string]int64`
/// — library id → role key → stat key (`m`/`a`/`s`) → count.
type RawLibraryStats = BTreeMap<String, BTreeMap<String, BTreeMap<String, i64>>>;

fn decode_library_stats(json: &str) -> Result<RawLibraryStats, String> {
    let root: JsonValue = match from_str::<JsonValue>(json) {
        Ok(value) => value,
        Err(err) => return Err(err.to_string()),
    };
    match &root {
        // Go: `null` into a map leaves it nil and returns no error, so the
        // `for … range` over a nil map simply does nothing.
        JsonValue::Null => return Ok(BTreeMap::new()),
        JsonValue::Object(_) => {}
        other => {
            return Err(format!(
                "json: cannot unmarshal {} into Go value of type map[string]map[string]map[string]int64",
                json_type_name(other)
            ));
        }
    }
    let mut libs: RawLibraryStats = BTreeMap::new();
    for (library_id, roles) in root.as_object().expect("matched above") {
        let role_map: BTreeMap<String, BTreeMap<String, i64>> = match roles {
            JsonValue::Null => BTreeMap::new(),
            JsonValue::Object(entries) => {
                let mut role_map = BTreeMap::new();
                for (role, stats) in entries {
                    let stat_map: BTreeMap<String, i64> = match stats {
                        JsonValue::Null => BTreeMap::new(),
                        JsonValue::Object(stat_entries) => {
                            let mut stat_map = BTreeMap::new();
                            for (key, value) in stat_entries {
                                // The path Go would report for this value, built once
                                // per entry and shared by both rejection arms —
                                // [`go_into_what`] reads its LAST token.
                                let stat_path = format!(".{library_id}.{role}.{key}");
                                match value {
                                    // Go zero-fills a null int64 with no error.
                                    JsonValue::Null => {
                                        stat_map.insert(key.clone(), 0);
                                    }
                                    JsonValue::Number(number) => {
                                        // Go decodes an integer JSON number into
                                        // int64 and REJECTS a non-integer spelling
                                        // (`1.0`, `1e3`, `1.5`) with an
                                        // UnmarshalTypeError carrying the value.
                                        match number.as_i64() {
                                            Some(v) => {
                                                stat_map.insert(key.clone(), v);
                                            }
                                            None => {
                                                // Go's `UnmarshalTypeError.Value`
                                                // for a literal is `"number " + <the raw
                                                // token>` (`encoding/json/v2_inject.go:128-135`),
                                                // so the sentence reads
                                                // `cannot unmarshal number 1.5 into …`.
                                                // serde renders the NUMBER, not the raw
                                                // literal, so an exponent spelling (`1e3`)
                                                // and an integer wider than `u64` differ in
                                                // inner text only — recorded residual. The
                                                // sentence SHAPE, including the choice of
                                                // the `Go struct field` phrase, is Go's: see
                                                // [`go_into_what`].
                                                return Err(go_type_error(
                                                    "number",
                                                    &format!("number {number}"),
                                                    go_into_what(&stat_path),
                                                    &stat_path,
                                                    "int64",
                                                ));
                                            }
                                        }
                                    }
                                    other => {
                                        return Err(go_type_error(
                                            json_type_name(other),
                                            "",
                                            go_into_what(&stat_path),
                                            &stat_path,
                                            "int64",
                                        ));
                                    }
                                }
                            }
                            stat_map
                        }
                        other => {
                            return Err(go_type_error(
                                json_type_name(other),
                                "",
                                go_into_what(&format!(".{library_id}.{role}")),
                                &format!(".{library_id}.{role}"),
                                "map[string]int64",
                            ));
                        }
                    };
                    role_map.insert(role.clone(), stat_map);
                }
                role_map
            }
            other => {
                // Whether the phrase `Go struct field ` survives here is decided
                // by Go's POSITIONAL heuristic on the path, NOT by the target
                // type being a map: a digit-only library id drops the phrase
                // (executed: `{"1":5}` → `json: cannot unmarshal number into .1
                // of type map[string]map[string]int64`) while a non-digit one
                // keeps it (executed: `{"alpha":5}` → `… into Go struct field
                // .alpha of type map[string]map[string]int64`). See [`go_into_what`].
                let field = format!(".{library_id}");
                return Err(go_type_error(
                    json_type_name(other),
                    "",
                    go_into_what(&field),
                    &field,
                    "map[string]map[string]int64",
                ));
            }
        };
        libs.insert(library_id.clone(), role_map);
    }
    Ok(libs)
}

/// Go's `encoding/json` `UnmarshalTypeError.Error()`
/// (`encoding/json/v2_decode.go:132-152`):
/// `json: cannot unmarshal <value|type> into <intoWhat> <Struct>.<Field> of type <Type>`,
/// where `intoWhat` comes from [`go_into_what`]. The port keeps Go's sentence
/// shape; see the residual note about serde's number-literal inner wording.
fn go_type_error(value: &str, spelled: &str, into_what: &str, field: &str, ty: &str) -> String {
    let shown = if spelled.is_empty() { value } else { spelled };
    if into_what.is_empty() {
        format!("json: cannot unmarshal {shown} into {field} of type {ty}")
    } else {
        format!("json: cannot unmarshal {shown} into {into_what} {field} of type {ty}")
    }
}

/// Go's heuristic for the `intoWhat` phrase — `encoding/json/v2_decode.go:143-147`:
///
/// ```go
/// intoWhat := "Go struct field "
/// i := strings.LastIndexByte(e.Field, '.') + len(".")
/// if len(e.Field[i:]) > 0 && strings.TrimRight(e.Field[i:], "0123456789") == "" {
///     intoWhat = "" // likely a Go slice or array
/// }
/// ```
///
/// The decision is POSITIONAL, not type-driven: it looks only at the last path
/// token, i.e. what follows the final `.` (and at the whole field when it holds
/// no `.` at all). A token that is non-empty and made entirely of ASCII digits
/// is taken for an array index and the phrase is dropped; everything else keeps
/// it — including an empty token, and including a spelling such as `01`, which
/// counts as all-digits because `TrimRight` strips a cutset rather than parses
/// a number.
///
/// Executed against Go 1.27.4, both sides of the rule at every level:
/// * `{"alpha":5}` → `json: cannot unmarshal number into Go struct field .alpha
///   of type map[string]map[string]int64`;
/// * `{"1":5}` → `json: cannot unmarshal number into .1 of type
///   map[string]map[string]int64`;
/// * `{"1":{"alpha":5}}` keeps the phrase, `{"1":{"2":5}}` →
///   `json: cannot unmarshal number into .1.2 of type map[string]int64`;
/// * `{"1":{"albumartist":{"5":true}}}` → `json: cannot unmarshal bool into
///   .1.albumartist.5 of type int64`, while `{"1":{"albumartist":{"m2":true}}}`
///   → `json: cannot unmarshal bool into Go struct field .1.albumartist.m2 of
///   type int64`.
///
/// `decode_library_stats` applies this to all four of its arms. The similar-
/// artists decoder needs no call here: Go IGNORES an unrecognized key, so a
/// digit-only key can never be the LAST token of one of its field sentences
/// (executed: `[{"5":3}]` → `err == nil`, and a numeric key cannot case-fold to
/// `id`/`name`), while its element-level arm's path IS the bare digit index `.0`
/// — for which Go drops the phrase (executed: `[5]` → `json: cannot unmarshal
/// number into .0 of type main.sa`) and that arm already passes an empty
/// `into_what`. Its field arms end in the raw key (`.0.id`, `.0.ID`), which is
/// non-digit, so Go keeps the phrase and that arm keeps it (executed:
/// `[{"id":3}]` → `… into Go struct field .0.id of type string`).
fn go_into_what(field: &str) -> &'static str {
    let token = match field.rfind('.') {
        Some(i) => &field[i + 1..],
        None => field,
    };
    if !token.is_empty() && token.bytes().all(|b| b.is_ascii_digit()) {
        ""
    } else {
        "Go struct field"
    }
}

/// `dbArtist.PostScan` (phase 2, `persistence/artist_repository.go:43-98`),
/// statement by statement with Go's order and guards:
///
/// 1. `:44` — `Stats` is created UNCONDITIONALLY, so an empty shim yields an
///    EMPTY (not defaulted, not absent) map.
/// 2. `:46-50` — the stats decode is guarded ONLY by `!= ""`; a failure carries
///    [`ARTIST_STATS_PREFIX`].
/// 3. `:52-79` — for every library, for every role key: build
///    `ArtistStats{m, a, s}` with ZERO defaults for a missing key or a JSON
///    null, then, ONLY when `key == "total"`, ACCUMULATE onto the artist's own
///    `size`/`song_count`/`album_count` (`:63-67`) with Go's WRAPPING `+=` — see
///    `hydrate.rs::wrapping_int64` / `hydrate.rs::narrow_i32_go` (both private,
///    so named in text, not as intra-doc links) — then parse the role with
///    [`Role::from_str`] (Go's `model.RoleFromString`, `model/participants.go:71-76`)
///    and `continue` on an invalid role — discarding the `invalid role:` text
///    exactly as Go's bare `continue` discards it (`:69-72`) — then accumulate
///    onto that role's entry the same way (`:73-78`).
///    Consequences pinned by `tests/runtime_hydrate_artist.rs`:
///    * `"total"` is added to the totals and NEVER inserted into `stats`,
///      because the `continue` at `:69-72` drops it AFTER the addition.
///    * an unknown role contributes NOTHING — neither to `stats` nor to the
///      totals, since the `+=` block is gated by `key == "total"`.
///    * pre-seeded totals ACCUMULATE, they are not overwritten, and an overflow
///      WRAPS rather than panicking (executed: `runtime_hydrate_artist.rs`
///      `::total_arm_accumulation_wraps_instead_of_panicking`).
/// 4. `:83-96` — `SimilarArtists` is reset to empty first, the decode is
///    guarded by `== ""` returning early, and a failure carries
///    [`ARTIST_SIMILAR_PREFIX`]. The stats loop has ALREADY run at that point,
///    so a similar-artists failure leaves `stats` populated.
///
/// First-error-wins and the row is discarded, so Go's partially-mutated map on
/// a decode failure is not reproduced (the row never reaches the caller).
///
/// # Errors
/// [`HydrateError::Decoded`] carrying [`ARTIST_STATS_PREFIX`] or
/// [`ARTIST_SIMILAR_PREFIX`], verbatim INCLUDING the trailing space.
pub fn artist_post_scan(row: ArtistRow) -> Result<ArtistRecord, HydrateError> {
    let mut record = row.record;

    // 1. `a.Artist.Stats = make(map[model.Role]model.ArtistStats)` (`:44`).
    record.stats = BTreeMap::new();

    // 2. guarded ONLY by `!= ""` (`:46`).
    if !row.library_stats_json.is_empty() {
        let raw = decode_library_stats(&row.library_stats_json)
            .map_err(|err| HydrateError::Decoded(format!("{ARTIST_STATS_PREFIX}{err}")))?;
        for stats in raw.values() {
            // 3. `:54-78`, over Go's (unordered) map iteration. Order cannot
            // matter here: every arm is a wrapping add and the role keys are unique.
            for (key, stat) in stats {
                // `:56-60` — zero defaults for a missing key.
                let song_count = stat.get("m").copied().unwrap_or(0);
                let album_count = stat.get("a").copied().unwrap_or(0);
                let size = stat.get("s").copied().unwrap_or(0);

                // `:63-67` — the literal "total" only, accumulating onto the
                // artist's own fields with Go's WRAPPING `+=`. A plain `+=` would
                // panic here: overflow checks are on in dev/test (see
                // [`wrapping_int64`]).
                if key == TOTAL_ROLE {
                    record.size = wrapping_int64(record.size, size);
                    record.song_count = narrow_i32_go(record.song_count, song_count);
                    record.album_count = narrow_i32_go(record.album_count, album_count);
                }

                // `:69-72` — invalid role: continue, discarding the text.
                let Ok(role) = Role::from_str(key) else {
                    continue;
                };

                // `:73-78` — per-role accumulate, the same wrapping adds.
                let current = record.stats.entry(role).or_default();
                current.song_count = narrow_i32_go(current.song_count, song_count);
                current.album_count = narrow_i32_go(current.album_count, album_count);
                current.size = wrapping_int64(current.size, size);
            }
        }
    }

    // 4. `:83` — reset before the guard, so a skipped decode still clears it.
    record.similar = Vec::new();
    if record.similar_artists.is_empty() {
        return Ok(record);
    }
    let root: JsonValue = match from_str::<JsonValue>(&record.similar_artists) {
        Ok(value) => value,
        Err(err) => {
            return Err(HydrateError::Decoded(format!(
                "{ARTIST_SIMILAR_PREFIX}{}",
                err
            )));
        }
    };
    match &root {
        // Go: `null` into a slice leaves it nil, and the `range` adds nothing.
        JsonValue::Null => {}
        JsonValue::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                match item {
                    // `[null]` yields a zero-valued entry, not a skip.
                    JsonValue::Null => record.similar.push(SimilarArtist::default()),
                    JsonValue::Object(entries) => {
                        // Field by field. Go's decoder walks the JSON object it
                        // reads and reports the FIRST field whose value has the
                        // wrong type; `serde_json::Value`'s map is a `BTreeMap`
                        // (`preserve_order` is NOT enabled in this workspace —
                        // executed probe on the resolved serde_json 1.0.154:
                        // `{"name":3,"id":4,"zzz":5,"Name":6}` iterates as
                        // `["Name","id","name","zzz"]`), so when BOTH fields are
                        // wrong the port names `.id` where Go names whichever key
                        // comes first in the source text. Recorded as a residual;
                        // a single wrong field reports identically on both sides.
                        // A missing key or a JSON `null` zero-fills with no error,
                        // an unknown field is ignored, and a key that matches only
                        // case-insensitively still fills the field — and its error
                        // names the RAW JSON key, not the struct field name
                        // (`decode.go`'s `object`/`fieldMatch` + `struct`'s
                        // `UnmarshalTypeError.Field = key`): executed
                        // `[{"ID":3}]` → `.0.ID`.
                        let mut pair = SimilarArtist::default();
                        for (key, value) in entries.iter() {
                            let slot = if key.eq_ignore_ascii_case("id") {
                                Some(&mut pair.id)
                            } else if key.eq_ignore_ascii_case("name") {
                                Some(&mut pair.name)
                            } else {
                                None
                            };
                            match (slot, value) {
                                (None, _) => continue,
                                // Go's `literalStore` assigns the zero value for a
                                // JSON null into a non-pointer string field.
                                (Some(_), JsonValue::Null) => {}
                                (Some(slot), JsonValue::String(text)) => {
                                    *slot = text.clone();
                                }
                                (Some(_), JsonValue::Number(_)) => {
                                    // Go's number arm reports a bare `number`
                                    // when the destination is a string
                                    // (`decode.go`: the `v.Kind() == String` case
                                    // saves the error WITHOUT `Value`), so the
                                    // literal never appears here — unlike the
                                    // int64 stats arm, which does spell
                                    // `number 1.5` (executed: `[{"id":1.5}]`,
                                    // `[{"id":1e3}]` and
                                    // `[{"id":-9223372036854775808}]` all read
                                    // `cannot unmarshal number into …`).
                                    return Err(HydrateError::Decoded(format!(
                                        "{ARTIST_SIMILAR_PREFIX}{}",
                                        go_type_error(
                                            "number",
                                            "",
                                            "Go struct field",
                                            &format!(".{i}.{key}"),
                                            "string",
                                        )
                                    )));
                                }
                                (Some(_), other) => {
                                    return Err(HydrateError::Decoded(format!(
                                        "{ARTIST_SIMILAR_PREFIX}{}",
                                        go_type_error(
                                            json_type_name(other),
                                            "",
                                            "Go struct field",
                                            &format!(".{i}.{key}"),
                                            "string",
                                        )
                                    )));
                                }
                            }
                        }
                        record.similar.push(pair);
                    }
                    other => {
                        return Err(HydrateError::Decoded(format!(
                            "{ARTIST_SIMILAR_PREFIX}{}",
                            go_type_error(
                                json_type_name(other),
                                "",
                                "",
                                &format!(".{i}"),
                                "nd_db::artist_repo::SimilarArtist",
                            )
                        )));
                    }
                }
            }
        }
        other => {
            return Err(HydrateError::Decoded(format!(
                "{ARTIST_SIMILAR_PREFIX}json: cannot unmarshal {} into Go value of type []nd_db::artist_repo::SimilarArtist",
                json_type_name(other)
            )));
        }
    }
    Ok(record)
}

/// Go's `int64` `+=` at `artist_repository.go:64` and `:74` has no overflow
/// check: a wrapped sum is simply what the field holds afterwards (executed:
/// `2147483647 += 2147483647` → `4294967294`, `int64MAX += 1` → `int64MIN`).
/// Rust's overflow checks are ON in this workspace — `rust/Cargo.toml`'s
/// `[profile.dev]`/`[profile.test]` set only `debug`/`incremental`, so nothing
/// disables them — and a plain `+=` at `i64::MAX` panics with
/// `attempt to add with overflow` (executed against a probe built with those
/// exact profiles). Spell Go's wrap out instead, the same way
/// `nd-model/src/criteria.rs:119`'s `wrapping_mul` does for `EffectiveLimit`.
fn wrapping_int64(current: i64, added: i64) -> i64 {
    current.wrapping_add(added)
}

/// Accumulate a Go `int64` stat onto the port's `i32` count field, the way
/// `artist_repository.go:65-66` and `:75-76` do — with two differences, both
/// deliberate and recorded in `docs/rust-port/hydrate-contract.md`:
///
/// 1. Go's `int` is 64-bit (executed: `int bits=64`, and
///    `int(int64(3_000_000_000))` is `3_000_000_000`, so `int(stat["m"])` at
///    `:57-58` narrows NOTHING). The port's fields are `i32`, so this narrowing
///    is a port-wide model choice Go does not make; the value is clamped rather
///    than truncated away.
/// 2. The add WRAPS like Go's unchecked `+=` instead of panicking. The clamp is
///    applied to each step's SUM (never to an individual addend), so a sum that
///    still fits `i32` is kept exactly — `i32::MAX + i32::MAX` widens to
///    `4294967294` without wrapping and saturates at `i32::MAX`. What it cannot
///    do is carry Go's 64-bit intermediate: once a step clamps to `i32::MAX`, the
///    next add sees that clamped value, so two `i64::MAX` totals leave Go's field
///    at `-2` (`MAX + MAX` wrapped) and this one at `i32::MIN`
///    (both executed; pinned by `runtime_hydrate_artist.rs
///    ::total_arm_accumulation_wraps_instead_of_panicking`). That gap is the `i32`
///    field, not the accumulation.
///
/// The sibling [`narrow_i32`] used by the scan accessors ERRORS with
/// `IntegralValueOutOfRange` on the same input; the two helpers diverge by
/// design and must not be conflated.
#[must_use]
fn narrow_i32_go(current: i32, added: i64) -> i32 {
    let sum = (current as i64).wrapping_add(added);
    i32::try_from(sum).unwrap_or(if sum < 0 { i32::MIN } else { i32::MAX })
}

/// Go's `encoding/json` names JSON types in its error text (`json: cannot
/// unmarshal string into …`); the same vocabulary `sql_tags::json_type_name`
/// and `sql_participations::json_type_name` use, kept crate-local here because
/// both of those are private to their own modules.
fn json_type_name(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "bool",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}

// ---------------------------------------------------------------------------
// S-D3g1: library + the `SELECT u.*` user projection.
// ---------------------------------------------------------------------------

/// `libraryRepository.Get`/`GetAll` row mapping (`library_repository.go:35-40`,
/// `:283-287`): there is deliberately **one phase, not two**. Go has no
/// `dbLibrary` wrapper, no `PostScan` hook and no shim column —
/// `grep -n "dbLibrary\|PostScan" persistence/library_repository.go` and
/// `grep -rn "dbLibrary" persistence/ model/` both come back empty — because
/// `queryOne`/`queryAll` `ScanStruct` straight into `model.Library`
/// (`dbx@v1.12.0/rows.go:60-85`). So this reader yields a
/// [`crate::library_repo::LibraryRecord`] directly: no raw-row twin, no phase-2
/// hook, and nothing normalises a column after the scan.
///
/// `SELECT *` emits the table's own columns, so every field is read by name.
/// NULL parity follows Go's non-pointer fields exactly: the eight `string`/`bool`/
/// numeric columns go through [`RowGet::get_text`]/[`RowGet::get_bool`]/
/// [`RowGet::get_i64`]/[`RowGet::get_f64`] and a NULL cell FAILS the scan, which is
/// executed Go behavior (`model/library.go:15-36` has no pointer field at all):
/// over a row with `total_duration = NULL`, `Get` returns
/// `sql: Scan error on column index 16, name "total_duration": converting NULL to
/// float64 is unsupported` beside a NON-nil pointer, and `GetAll` aborts with the
/// same error keeping only the rows scanned before it — executed over
/// {id=1 NULL, id=2 ok} with `ORDER BY id asc`: `len == 1`, error set. With
/// `remote_path = NULL` on the SECOND row the trace is
/// `sql: Scan error on column index 3, name "remote_path": converting NULL to
/// string is unsupported` (oracle `/tmp/sd3g1_oracle2`). The port reports the
/// failure and drops the partial slice, since `Result` has no value-beside-error
/// (the same decision as [`crate::radio_repo::RadioRepo::get_all`]).
///
/// All five stamps (`last_scan_at`, `last_scan_started_at`, `updated_at`,
/// `created_at` — `model/library.go:19-23` — are non-pointer `time.Time`) go
/// through [`RowGet::get_dt`]: NULL errors, and an unparseable cell scans as
/// [`go_zero_time`] because the driver swallows every DATETIME parse failure
/// (`go-sqlite3@v1.14.52/sqlite3.go:2831-2838`). Crucially, an INTEGER cell in a
/// column DECLARED `datetime` is decoded as a timestamp rather than rejected
/// (see [`driver_integer_dt`]), which is what makes the read side parity for
/// BOTH storage spellings. Executed against the real Go repository over
/// `last_scan_at = 1704067200` and `last_scan_started_at = 0`:
/// `2024-01-01 00:00:00 +0000 UTC` and `1970-01-01 00:00:00 +0000 UTC`, and
/// `IsZero()` on the latter is **false** — the epoch integer is NOT Go's zero
/// time. Scanner writes now bind TEXT through [`crate::time::value`], and the
/// idle predicate compares against [`crate::library_repo::zero_time_value`];
/// the integer decoder remains for legacy/external INTEGER cells.
///
/// `ID` is Go's `int` (`model/library.go:15`), read with [`RowGet::get_i64`] —
/// Go's `int` is 64-bit, so the record's `i64` carries it without narrowing.
/// The six `total_*` counters are Go `int` too, kept as `i64` for the same
/// reason; `total_size` is Go `int64` and `total_duration` Go `float64`.
///
/// # Errors
/// A NULL or unscannable *required* library column fails, per Go parity.
pub fn row_to_library(row: &Row<'_>) -> SqliteResult<crate::library_repo::LibraryRecord> {
    Ok(crate::library_repo::LibraryRecord {
        id: row.get_i64("id")?,
        name: row.get_text("name")?,
        path: row.get_text("path")?,
        remote_path: row.get_text("remote_path")?,
        last_scan_at: row.get_dt("last_scan_at")?,
        last_scan_started_at: row.get_dt("last_scan_started_at")?,
        full_scan_in_progress: row.get_bool("full_scan_in_progress")?,
        updated_at: row.get_dt("updated_at")?,
        created_at: row.get_dt("created_at")?,
        total_songs: row.get_i64("total_songs")?,
        total_albums: row.get_i64("total_albums")?,
        total_artists: row.get_i64("total_artists")?,
        total_folders: row.get_i64("total_folders")?,
        total_files: row.get_i64("total_files")?,
        total_missing_files: row.get_i64("total_missing_files")?,
        total_size: row.get_i64("total_size")?,
        total_duration: row.get_f64("total_duration")?,
        default_new_users: row.get_bool("default_new_users")?,
        pid_album: row.get_text("pid_album")?,
        pid_track: row.get_text("pid_track")?,
        scanned_pid_album: row.get_text("scanned_pid_album")?,
        scanned_pid_track: row.get_text("scanned_pid_track")?,
    })
}

/// The `SELECT u.*` user projection (`library_repository.go:297-307`,
/// `GetUsersWithLibraryAccess`): the SAME twelve columns and the SAME read order
/// as [`row_to_user`], but with **no `libraries_json` alias**, so the second
/// phase never runs.
///
/// Go scans into `dbUser` (`user_repository.go:30-34`), whose `LibrariesJSON`
/// field only gets a scan target when the SELECT emits a column named
/// `libraries_json` (`dbx@v1.12.0/rows.go:80-86` binds a reference per mapped
/// column). `GetUsersWithLibraryAccess` builds a plain `Select("u.*")` with no
/// such alias, so `LibrariesJSON` stays `""`, `dbUser.PostScan` takes its
/// `!= ""` skip arm (`user_repository.go:37-41`), and `User.Libraries` stays
/// NIL. Executed against the real repository: every returned user reported
/// `libs=0`, `lastLoginNil` true wherever `last_login_at` is NULL, and the
/// `structs:"-"` `Password`/`TokenEpoch` columns ARE scanned (`pwd="CIPHER"`,
/// `epoch=7`) — the same field-name mapping rule [`row_to_user`] documents.
///
/// The record therefore carries `libraries: Vec::new()` — Go's nil slice and the
/// empty slice agree for `HasLibraryAccess` (`model/user.go:42-50`, which loops
/// zero times) and for JSON's `omitempty` (both omit the key). It is NOT the
/// hydrated library list: a caller that needs it must use
/// [`crate::user_repo::UserRepo::get`], whose select emits the alias.
///
/// The column reads are deliberately spelled out rather than shared with
/// [`row_to_user`]: that fn reads `libraries_json` FIRST and would fail this
/// projection, and S-D3g1 must not modify an existing row mapper's body. The
/// two lists are field-for-field identical, so the drift risk is confined to
/// this doc; `runtime_hydrate_library_property_tag.rs` pins the executed column
/// set from the live schema.
///
/// # Errors
/// A missing or NULL *required* user column fails, per Go parity.
pub fn row_to_user_without_libraries(row: &Row<'_>) -> SqliteResult<UserRecord> {
    Ok(UserRecord {
        id: row.get_text("id")?,
        user_name: row.get_text("user_name")?,
        name: row.get_text("name")?,
        email: row.get_text("email")?,
        is_admin: row.get_bool("is_admin")?,
        last_login_at: row.get_opt_text("last_login_at")?,
        last_access_at: row.get_opt_text("last_access_at")?,
        created_at: row.get_text("created_at")?,
        updated_at: row.get_text("updated_at")?,
        scrobble_filter: row.get_text("scrobble_filter")?,
        password: row.get_text("password")?,
        token_epoch: row.get_i64("token_epoch")?,
        // No `libraries_json` column exists on this projection, so Go's
        // PostScan never runs and the list stays nil/empty (see above).
        libraries: Vec::new(),
    })
}

/// `tagRepository.GetAll` row mapping (`persistence/tag_repository.go:78-83`):
/// the five columns of `baseTagRepository.newSelect` (`persistence/sql_tags.go:
/// 197-228`) read straight into `model.Tag` (`model/tag.go:17-23`).
///
/// This is deliberately a SINGLE-phase mapper, like [`row_to_library`]: the tag
/// dictionary has no `dbTag` wrapper and no `PostScan` hook
/// (`grep -n "dbTag\|PostScan" persistence/sql_tags.go persistence/tag_repository.go`
/// is empty), so `queryAll`'s `ScanStruct` (`dbx@v1.12.0/rows.go:60-85`) assigns
/// each column by `db`/field name and nothing runs afterwards. There is therefore
/// no raw-row twin and no `…_post_scan` sibling for this read.
///
/// Reading by ALIAS name is what Go does: the two aggregate columns are aliased
/// `as album_count` / `as song_count` (`sql_tags.go:192-193`) and `SongCount` is
/// the Go field the second one fills. Executed row sets (oracle
/// `/tmp/sd3g1_oracle6`, libraries 1 and 2, admin `adm` with access to library 1
/// only):
/// * `GetAll("genre")` → `Pop/0/0`, `Rock/2/5` — the dictionary row with no
///   `library_tag` row at all and the one belonging to the inaccessible library
///   are BOTH absent, because the `user_library` leg is an INNER join;
/// * headless `GetAll("genre")` → `Lonely/0/0`, `OnlyLib2/9/99`, `Pop/0/0`,
///   `Rock/2/5` — with no logged-in user that join is not added at all, so every
///   dictionary row appears (`sql_tags.go:205-210`).
///
/// `COALESCE(…, 0)` makes both aliases non-NULL for any row the aggregate emits,
/// so Go's non-pointer `int` fields never meet a NULL here. The port reads them
/// with [`RowGet::get_i64`]: Go's `int` is 64-bit, so unlike the `i32` model
/// fields elsewhere in this file there is no narrowing step and no
/// `narrow_i32`-style divergence to record.
///
/// # Errors
/// A missing or unscannable alias fails the read. A NULL in `album_count` or
/// `song_count` — unreachable through the aggregate's own `COALESCE`, reachable
/// only if a caller hands this mapper a different projection — fails as Go's
/// scan error does rather than yielding a zero count.
pub fn row_to_tag(row: &Row<'_>) -> SqliteResult<crate::tag_repo::Tag> {
    Ok(crate::tag_repo::Tag {
        id: row.get_text("id")?,
        tag_name: row.get_text("tag_name")?,
        tag_value: row.get_text("tag_value")?,
        album_count: row.get_i64("album_count")?,
        song_count: row.get_i64("song_count")?,
    })
}

/// The `SELECT * FROM transcoding` row (`transcoding_repository.go:24`, `:39`, `:69`):
/// the four text columns plus Go's non-pointer `int` `DefaultBitRate
/// (`model/transcoding.go:9-14`), read in struct-declaration order. Hook-free —
/// `grep -n "dbTranscoding\|PostScan" persistence/transcoding_repository.go` is empty,
/// so there is no phase 2 (`hydrate-contract.md:218`), and the DTO stays in nd-db
/// (`hydrate-contract.md:324`).
///
/// `default_bit_rate` uses [`RowGet::get_i64`], so a NULL cell is a SCAN error, not
/// NotFound and not a zero bit rate: Go scans into a non-pointer `int` and
/// `database/sql` cannot store a nil driver value into it
/// (`convert.go:521-526`). The fresh DDL declares
/// `default_bit_rate int default 192` with NO `not null`
/// (`nd-dbschema/src/schema_ddl.rs:56`), so a NULL cell is reachable and fails the
/// same way in both languages.
///
/// `command` is read UNMASKED here — masking is the repository's
/// `Read`/`ReadAll` boundary (`transcoding_repository.go:56-58`, `:75-78`), so
/// `Get`/`FindByFormat` hand the stream layer the real command.
///
/// # Errors
/// A missing or NULL transcoding column fails, per Go parity.
pub fn row_to_transcoding(row: &Row<'_>) -> SqliteResult<crate::transcoding_repo::Transcoding> {
    Ok(crate::transcoding_repo::Transcoding {
        id: row.get_text("id")?,
        name: row.get_text("name")?,
        target_format: row.get_text("target_format")?,
        command: row.get_text("command")?,
        default_bit_rate: row.get_i64("default_bit_rate")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time;
    use rusqlite::Connection;

    /// One row with every user column present: the happy path through
    /// both phases.
    fn full_user_row(conn: &Connection) -> UserRow {
        const SQL: &str = "SELECT 'u1' AS id, 'jdoe' AS user_name, 'John' AS name, \
             'j@x.y' AS email, 1 AS is_admin, \
             '2024-01-02 03:04:05+00:00' AS last_login_at, \
             NULL AS last_access_at, \
             '2024-01-01 00:00:00+00:00' AS created_at, \
             '2024-02-01 00:00:00+00:00' AS updated_at, \
             '' AS scrobble_filter, 'CIPHER' AS password, \
             0 AS token_epoch, \
             '[{\"id\":7,\"name\":\"Rock\",\"path\":\"/rock\"}]' AS libraries_json";
        conn.query_row(SQL, [], row_to_user)
            .expect("full row reads")
    }

    /// The 12 user columns without `libraries_json` (the light-query shape).
    const USER_COLS_NO_JSON: &str = "SELECT 'u1' AS id, 'jdoe' AS user_name, 'John' AS name, \
         'j@x.y' AS email, 0 AS is_admin, NULL AS last_login_at, \
         NULL AS last_access_at, '2024-01-01 00:00:00+00:00' AS created_at, \
         '2024-01-01 00:00:00+00:00' AS updated_at, '' AS scrobble_filter, \
         'CIPHER' AS password, 0 AS token_epoch";

    #[test]
    fn libraries_json_matches_gos_zero_fill_and_null_element_rules() {
        // Executed against Go 1.27.4 `encoding/json` with `model.Libraries`
        // (`model/library.go:72`, element `model.Library` json tags `id`/`name`/
        // `path`):
        //   [{"id":1}]                                    -> [{1 "" ""}], err = nil
        //   [null]                                       -> [{0 "" ""}], err = nil
        //   [{"id":1,"name":2}]                          -> err "cannot unmarshal number ... of type string"
        //   {"a":1}                                      -> err "cannot unmarshal object into Go value of type Libraries"
        // serde's derive turns an absent field into an error, so the struct
        // declares `#[serde(default)]` and null elements are mapped to the
        // default above.
        let partial = parse_libraries_json(r#"[{"id":1}]"#)
            .expect("Go leaves missing fields at their zero value");
        assert_eq!(partial[0].id, 1);
        assert_eq!(partial[0].name, "");
        assert_eq!(partial[0].path, "");

        let nulls =
            parse_libraries_json("[null]").expect("Go decodes a null element as the zero value");
        assert_eq!(nulls.len(), 1);
        assert_eq!(nulls[0], UserLibrary::default());

        // Shape and type errors stay errors, like Go's.
        assert!(parse_libraries_json(r#"{"a":1}"#).is_err());
        assert!(parse_libraries_json(r#"[{"id":1,"name":2}]"#).is_err());
        assert!(parse_libraries_json(r#"[{"id":1,"name":"a","path":"p"}]"#).is_ok());
    }

    #[test]
    fn optional_int_columns_follow_gos_pointer_int_arms() {
        // Executed against the pinned driver with `*int` destinations:
        //   TEXT '16' -> 16, BLOB '16' -> 16, REAL 16.0 -> 16, NULL -> nil, all err = nil
        //   TEXT 'abc' -> converting driver.Value type string ("abc") to a int: invalid syntax
        // rusqlite's bare `Option<i32>` rejects Text/Real/Blob outright, so the
        // accessor goes through Go's int arm.
        let conn = Connection::open_in_memory().expect("memory db opens");
        conn.execute_batch(
            "CREATE TABLE t (bd text, bb blob, br real, nn integer);
             INSERT INTO t VALUES ('16', cast(x'3136' as blob), 16.0, NULL);",
        )
        .expect("table");
        conn.query_row("SELECT bd, bb, br, nn FROM t", [], |row| {
            assert_eq!(
                row.get_opt_i32("bd")?,
                Some(16),
                "Go scans TEXT \"16\" into *int"
            );
            assert_eq!(
                row.get_opt_i32("bb")?,
                Some(16),
                "Go scans a []uint8 into *int"
            );
            assert_eq!(
                row.get_opt_i32("br")?,
                Some(16),
                "Go's %g spelling parses as 16"
            );
            assert_eq!(row.get_opt_i32("nn")?, None, "SQL NULL is Go's nil pointer");
            Ok(())
        })
        .expect("row");
        conn.execute("INSERT INTO t VALUES ('abc', NULL, NULL, NULL)", [])
            .expect("insert");
        let msg = conn
            .query_row("SELECT bd FROM t WHERE bd = 'abc'", [], |row| {
                row.get_opt_i32("bd")
            })
            .err()
            .unwrap()
            .to_string();
        assert!(
            msg.contains(
                r#"converting driver.Value type string ("abc") to a int64: invalid syntax"#
            ),
            "{msg}"
        );
        assert!(msg.contains("column `bd`"), "the column is named: {msg}");
    }

    #[test]
    fn boolean_declared_column_with_real_storage_errors_like_go() {
        // SQLite's NUMERIC affinity narrows only an exactly-representable REAL
        // literal to INTEGER on insert: executed through rusqlite (bundled SQLite
        // 3.46.0), a `boolean` column keeps `2.0`/`cast(2 as real)` as INTEGER but
        // `2.5`, `1.5` and `cast(2.5 as real)` as REAL. Executed against the pinned
        // driver with `cast(2.5 as real)` and a `bool` dest, Go reports
        // `sql/driver: couldn't convert 2.5 (float64) into type bool`; `cast(2 as
        // real)` stores as INTEGER and Go's `val > 0` arm returns true.
        let conn = Connection::open_in_memory().expect("memory db opens");
        conn.execute_batch(
            "CREATE TABLE t (b boolean);
             INSERT INTO t VALUES (cast(2.5 as real)), (cast(2 as real));",
        )
        .expect("table");
        let msg = {
            let mut stmt = conn
                .prepare("SELECT b, typeof(b) FROM t")
                .expect("statement");
            let mut rows = stmt.query([]).expect("rows");
            let row = rows.next().expect("row").expect("first row");
            assert_eq!(
                rusqlite::Row::get::<_, String>(row, 1).expect("typeof"),
                "real",
                "the cell really is REAL storage"
            );
            row.get_bool("b").err().unwrap().to_string()
        };
        assert!(
            msg.contains("sql/driver: couldn't convert 2.5 (float64) into type bool"),
            "{msg}"
        );
        conn.query_row("SELECT b FROM t WHERE typeof(b) = 'integer'", [], |row| {
            assert!(
                row.get_bool("b")?,
                "the decltype arm's `val > 0` is reached before driver.Bool"
            );
            Ok(())
        })
        .expect("row");
    }

    #[test]
    fn bool_failure_messages_match_gos_driver_bool() {
        // Executed against the pinned driver on Go 1.27.4 with a column declared
        // `bool`: REAL 1.5 -> `couldn't convert 1.5 (float64) into type bool`,
        // INTEGER 2 -> `couldn't convert 2 into type bool`, BLOB 00 ff ->
        // `couldn't convert "\x00\xff" into type bool`.
        let conn = Connection::open_in_memory().expect("memory db opens");
        conn.execute_batch(
            "CREATE TABLE b (v bool);
             INSERT INTO b VALUES (1.5), (2), (cast(x'00ff' as blob));",
        )
        .expect("table");
        let msg = |index: usize| {
            conn.query_row("SELECT v FROM b LIMIT 1 OFFSET ?", [index], |row| {
                row.get_bool("v")
            })
            .err()
            .unwrap()
            .to_string()
        };
        let real = msg(0);
        let integer = msg(1);
        let blob = msg(2);
        assert!(
            real.contains("couldn't convert 1.5 (float64) into type bool"),
            "{real}"
        );
        assert!(
            integer.contains("couldn't convert 2 into type bool"),
            "{integer}"
        );
        assert!(
            blob.contains("couldn't convert \"\\x00\\xff\" into type bool"),
            "{blob}"
        );
    }

    #[test]
    fn invalid_utf8_in_a_text_cell_is_quoted_like_gos_driver_string() {
        // Go's `%q` runs over the driver value's own bytes, so a TEXT cell holding
        // invalid UTF-8 keeps the `\xNN` spelling rather than a replacement
        // character. Executed on Go 1.27.4 with an `int64` destination:
        //   cast(x'ff' as text)       -> converting driver.Value type string ("\xff") to a int64: invalid syntax
        //   cast(x'6869ff41' as text) -> converting driver.Value type string ("hi\xffA") to a int64: invalid syntax
        let conn = Connection::open_in_memory().expect("memory db opens");
        conn.execute_batch(
            "CREATE TABLE t (s text, n integer);
             INSERT INTO t VALUES (cast(x'ff' as text), 1), (cast(x'6869ff41' as text), 2);",
        )
        .expect("table");
        let msg = |index: usize| {
            conn.query_row("SELECT s FROM t LIMIT 1 OFFSET ?", [index], |row| {
                row.get_i64("s")
            })
            .err()
            .unwrap()
            .to_string()
        };
        let single = msg(0);
        let mixed = msg(1);
        let cell_type = conn
            .query_row("SELECT typeof(cast(x'ff' as text))", [], |row| {
                rusqlite::Row::get::<_, String>(row, 0)
            })
            .expect("typeof");
        assert_eq!(cell_type, "text", "the cell really is TEXT storage");
        assert!(
            single.contains(
                r#"converting driver.Value type string ("\xff") to a int64: invalid syntax"#
            ),
            "{single}"
        );
        assert!(
            mixed.contains(
                r#"converting driver.Value type string ("hi\xffA") to a int64: invalid syntax"#
            ),
            "{mixed}"
        );
        // Go's wrapper supplies the column name, and the port's documented
        // substitute is the ` (column `X`)` suffix — spelled exactly, with the
        // backticks around the name only.
        assert!(single.ends_with(" (column `s`)"), "{single}");
        assert!(mixed.ends_with(" (column `s`)"), "{mixed}");
    }

    #[test]
    fn go_quote_matches_strconv_quote_for_raw_bytes() {
        // Go's `%q` over a driver value: undecodable bytes stay as `\xNN` rather
        // than becoming U+FFFD, and printability is Go's Latin-1 rule (`¡`–`ÿ`
        // printable, soft hyphen not). Compared against the exact Go output
        // captured from `fmt.Sprintf("%q", []byte(...))` on Go 1.27.4, asserted
        // byte-for-byte below. The CJK/emoji rows cover the above-Latin-1 path where
        // `escape_debug` agrees with Go's `IsPrint`; the only disagreement there is
        // the Unicode mark categories, documented on [`go_is_print`].
        assert_eq!(
            go_quote_bytes(&[0x00, 0xc3, 0xf8, 0xff]),
            "\"\\x00\\xc3\\xf8\\xff\""
        );
        assert_eq!(go_quote_bytes(b""), "\"\"");
        assert_eq!(go_quote_bytes(b"0123456789"), "\"0123456789\"");
        assert_eq!(go_quote_bytes("café".as_bytes()), "\"café\"");
        assert_eq!(go_quote_bytes(&[0x01, 0x7f]), "\"\\x01\\x7f\"");
        assert_eq!(go_quote_bytes(b"\t\n\r\"\\x"), "\"\\t\\n\\r\\\"\\\\x\"");
        assert_eq!(go_quote_bytes("€".as_bytes()), "\"€\"");
        assert_eq!(go_quote_bytes("你好".as_bytes()), "\"你好\"");
        assert_eq!(
            go_quote_bytes(b"a\xed\xbf\xbf"),
            "\"a\\xed\\xbf\\xbf\"",
            "a surrogate is three invalid bytes, each its own \\xNN"
        );
        assert_eq!(go_quote_bytes(b"trunc\xc3"), "\"trunc\\xc3\"");
        let mut emoji_bad = "emoji🙂bad".as_bytes().to_vec();
        emoji_bad.push(0xc3); // Go's `"…\xc3"` is the raw byte, not U+00C3
        assert_eq!(go_quote_bytes(&emoji_bad), "\"emoji🙂bad\\xc3\"");
        assert_eq!(
            go_quote_bytes(&[0x7e, 0x80, 0xa0, 0xff]),
            "\"~\\x80\\xa0\\xff\""
        );
        assert_eq!(go_quote_bytes("😀".as_bytes()), "\"😀\"");
        assert_eq!(go_quote_bytes(&[0x1b, 0x07]), "\"\\x1b\\a\"");
        assert_eq!(
            go_quote_bytes("\u{3000}ideographic space".as_bytes()),
            "\"\\u3000ideographic space\"",
            "Go's Ideographic Space is not printable"
        );
        assert_eq!(go_quote_bytes("é\u{00}tail".as_bytes()), "\"é\\x00tail\"");
        assert_eq!(
            go_quote_bytes("\u{00ad}".as_bytes()),
            "\"\\u00ad\"",
            "the soft hyphen is the one Latin-1 non-printable"
        );
        assert_eq!(go_quote_bytes("\u{00a1}".as_bytes()), "\"¡\"");
    }

    #[test]
    fn text_and_float_getters_convert_every_storage_class_like_go() {
        let conn = Connection::open_in_memory().expect("memory db opens");
        conn.execute_batch(
            "CREATE TABLE s (k text, v);
             INSERT INTO s VALUES ('int', 7), ('real', 1.5), ('realint', 2.0),
             ('blobutf8', cast(x'312e35' as blob)), ('blobnul', cast(x'312e3500' as blob)),
             ('bad', cast(x'00c3f8ff' as blob)),
             ('txt', 'hi'), ('nantxt', 'nan'), ('nul', NULL), ('bigreal', 1e400);",
        )
        .expect("table");
        let text = |key: &str| {
            conn.query_row("SELECT v FROM s WHERE k = ?", [key], |row| {
                row.get_opt_text("v")
            })
            .expect("row")
        };
        // Go's `reflect.String` arm renders every non-NULL storage class through
        // `asString`, so these are values, not scan errors.
        assert_eq!(text("int"), Some("7".to_string()));
        assert_eq!(text("real"), Some("1.5".to_string()));
        assert_eq!(
            text("realint"),
            Some("2".to_string()),
            "Go's %g drops the trailing zero, unlike Rust's `{{}}` on a float"
        );
        assert_eq!(text("txt"), Some("hi".to_string()));
        assert_eq!(
            text("bigreal"),
            Some("+Inf".to_string()),
            "an infinite REAL is Go's `+Inf` spelling in text"
        );
        // A BLOB becomes a Go `string` of raw bytes (`convert.go:541-543`), and
        // the per-byte replacement below is what `encoding/json` writes on the way
        // out, so that is the form the port keeps.
        assert_eq!(text("blobutf8"), Some("1.5".to_string()));
        assert_eq!(
            text("blobnul"),
            Some("1.5\u{00}".to_string()),
            "NUL is valid UTF-8, so Go's string keeps it raw"
        );
        // 0x00 is itself valid UTF-8, so it survives; the three bytes after it
        // each start an invalid sequence and get one replacement apiece.
        assert_eq!(
            text("bad"),
            Some("\u{00}\u{FFFD}\u{FFFD}\u{FFFD}".to_string())
        );
        assert_eq!(text("nul"), None, "NULL leaves a pointer field nil");
        assert!(
            conn.query_row("SELECT v FROM s WHERE k = ?", ["nul"], |row| {
                row.get_text("v")
            })
            .is_err(),
            "NULL into a non-pointer string errors"
        );
        // get_f64 reaches TEXT through `asString`, and a UTF-8 BLOB scans because
        // `asString` maps `[]uint8` to a Go `string` (verified: `1.5\0` -> 1.5).
        let f64_of = |key: &str| {
            conn.query_row("SELECT v FROM s WHERE k = ?", [key], |row| row.get_f64("v"))
        };
        assert!(f64_of("txt").is_err(), "`hi` is Go's invalid syntax");
        assert!(
            f64_of("nantxt").unwrap().is_nan(),
            "Go's `special` accepts the bare `nan` spelling"
        );
        assert_eq!(f64_of("blobutf8").unwrap(), 1.5, "Go accepts a UTF-8 BLOB");
        assert!(f64_of("bad").is_err(), "U+FFFD is not a number");
        assert!(
            f64_of("nul").is_err(),
            "NULL into a non-pointer float errors"
        );
        assert_eq!(f64_of("int").unwrap(), 7.0);
        assert_eq!(
            f64_of("bigreal").unwrap(),
            f64::INFINITY,
            "REAL stores directly"
        );
    }

    #[test]
    fn numeric_scan_arms_follow_gos_asstring_and_parses() {
        let conn = Connection::open_in_memory().expect("memory db opens");
        conn.execute_batch(
            "CREATE TABLE n (k text, v);
             INSERT INTO n VALUES ('txt7', '7'), ('txtTrue', 'True'), ('real15', 1.5),
             ('int2', 2), ('blobT', cast(x'54727565' as blob)), ('big', '5000000000'),
             ('txt15', '15.0'), ('txt1e400', '1e400'), ('txtinf', 'Inf'),
             ('txtplusnan', '+nan'), ('txtmax', '9223372036854775808'),
             ('blobbad', cast(x'00c3f8ff' as blob)), ('nul', NULL);",
        )
        .expect("table");
        fn read<T, F>(conn: &Connection, key: &str, get: F) -> rusqlite::Result<T>
        where
            F: FnOnce(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
        {
            conn.query_row("SELECT v FROM n WHERE k = ?", [key], get)
        }
        // TEXT reaches `asString` + `ParseInt`, so "7" scans and "True" fails.
        assert_eq!(read(&conn, "txt7", |r| r.get_i64("v")).unwrap(), 7);
        assert!(read(&conn, "txtTrue", |r| r.get_i64("v")).is_err());
        // A REAL cell is judged by its `%g` spelling; an INTEGER stores directly.
        assert!(read(&conn, "real15", |r| r.get_i64("v")).is_err());
        assert_eq!(read(&conn, "int2", |r| r.get_i64("v")).unwrap(), 2);
        // A BLOB is the driver's `[]uint8`: quoted text is the RAW bytes.
        let blob_err = read(&conn, "blobbad", |r| r.get_i64("v"))
            .unwrap_err()
            .to_string();
        assert!(
            blob_err.contains("[]uint8") && blob_err.contains("\\x00\\xc3\\xf8\\xff"),
            "raw-byte quoting: {blob_err}"
        );
        // A BLOB holding text still parses through `asString`, quoted as `[]uint8`.
        let blob_text = read(&conn, "blobT", |r| r.get_i64("v"))
            .unwrap_err()
            .to_string();
        assert!(
            blob_text.contains("[]uint8 (\"True\")") && blob_text.contains("invalid syntax"),
            "Go's `[]uint8` rejection: {blob_text}"
        );
        // Floats: TEXT parses, overflow is Go's range failure, `Inf` is a value,
        // and `+nan` is a syntax error even though Rust would parse it.
        assert_eq!(read(&conn, "txt15", |r| r.get_f64("v")).unwrap(), 15.0);
        assert!(read(&conn, "txt1e400", |r| r.get_f64("v"))
            .err()
            .unwrap()
            .to_string()
            .contains("value out of range"));
        assert_eq!(
            read(&conn, "txtinf", |r| r.get_f64("v")).unwrap(),
            f64::INFINITY
        );
        assert!(read(&conn, "txtplusnan", |r| r.get_f64("v")).is_err());
        // int64 overflow in TEXT is Go's range failure, not a wrap.
        assert!(read(&conn, "txtmax", |r| r.get_i64("v"))
            .err()
            .unwrap()
            .to_string()
            .contains("value out of range"));
        // `i32` reports out-of-range for a cell Go's 64-bit `int` would accept.
        assert_eq!(
            read(&conn, "big", |r| r.get_i64("v")).unwrap(),
            5_000_000_000
        );
        assert!(read(&conn, "big", |r| r.get_i32("v")).is_err());
        // NULL keeps the shared required-column payload.
        assert!(read(&conn, "nul", |r| r.get_i64("v")).is_err());
        // A `*float64` dest errors on a failed conversion too, and dbx
        // propagates it; only NULL becomes None.
        assert!(read(&conn, "blobbad", |r| r.get_opt_f64("v")).is_err());
        assert_eq!(
            read(&conn, "txt15", |r| r.get_opt_f64("v")).unwrap(),
            Some(15.0)
        );
        assert_eq!(read(&conn, "nul", |r| r.get_opt_f64("v")).unwrap(), None);
        conn.query_row("SELECT 1 AS only", [], |row| {
            assert!(
                row.get_opt_f64("missing").is_err(),
                "absent column surfaces"
            );
            Ok(())
        })
        .expect("row");
    }

    #[test]
    fn two_phase_split_hydrates_libraries() {
        // Arrange:
        let conn = Connection::open_in_memory().expect("memory db opens");
        // Act:
        let raw = full_user_row(&conn);
        let record = user_post_scan(raw).expect("payload parses");
        // Assert:
        assert_eq!(record.id, "u1");
        assert_eq!(record.libraries.len(), 1);
        assert_eq!(record.libraries[0].id, 7);
        assert_eq!(record.libraries[0].name, "Rock");
        assert_eq!(record.libraries[0].path, "/rock");
        // Assert: NULL pointer time is None, non-NULL required text read.
        assert_eq!(
            record.last_login_at.as_deref(),
            Some("2024-01-02 03:04:05+00:00")
        );
        assert_eq!(record.last_access_at, None);
    }

    #[test]
    fn empty_and_null_libraries_decode_to_an_empty_set() {
        // "" is Go's guard-skip branch; the select coalesces no-library
        // users to "[]", which parses to the same empty (non-nil) vec.
        assert!(parse_libraries_json("").unwrap().is_empty());
        let parsed = parse_libraries_json("[]").unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn malformed_libraries_json_yields_decoded_with_gos_prefix() {
        // Arrange: a row whose libraries_json will not parse.
        let conn = Connection::open_in_memory().expect("memory db opens");
        let raw = conn
            .query_row(
                &format!("{USER_COLS_NO_JSON}, '{{\"not\": an array}}' AS libraries_json"),
                [],
                row_to_user,
            )
            .expect("the raw row itself is valid");
        // Act:
        let err = user_post_scan(raw).unwrap_err();
        // Assert: Decoded, with Go's exact prefix up front.
        let HydrateError::Decoded(msg) = err else {
            panic!("expected Decoded, got {err:?}");
        };
        assert!(msg.starts_with(USER_LIBRARIES_PREFIX), "{msg}");
    }

    #[test]
    fn required_getters_error_on_null_and_optional_yield_none() {
        // Arrange: one row where every column is NULL.
        let conn = Connection::open_in_memory().expect("memory db opens");
        let err = conn
            .query_row("SELECT NULL AS id, NULL AS t", [], |row| row.get_text("id"))
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("NULL value in required column `id`"),
            "{err}"
        );
        conn.query_row(
            "SELECT NULL AS f, NULL AS b, NULL AS n, NULL AS d",
            [],
            |row| {
                assert!(row.get_f64("f").is_err());
                assert!(row.get_bool("b").is_err());
                assert!(row.get_i64("n").is_err());
                assert!(row.get_opt_f64("f").unwrap().is_none());
                assert_eq!(row.get_opt_text("f").unwrap(), None);
                Ok(())
            },
        )
        .expect("opt getters tolerate NULL");
    }

    /// The driver's DATETIME semantics, executed (`sqlite3.go:2821-2842` +
    /// `database/sql/convert.go:491-496`): SQL NULL is the nil pointer (`None`),
    /// ANY unparseable non-NULL text in a DECLARED timestamp column is a pointer
    /// to Go's zero time (`Some(go_zero_time())`) and NOT an error — the sentinel
    /// included — and a parseable cell parses.
    ///
    /// The columns are declared `datetime` because the driver's parse arm switches
    /// on the DECLARED type (`:2822`); undeclared text and expression columns are
    /// scan errors instead (see
    /// [`undeclared_text_and_expressions_are_not_converted_to_a_time`]).
    #[test]
    fn opt_dt_is_nil_pointer_on_null_and_pointer_to_go_zero_on_unparseable_text() {
        let conn = Connection::open_in_memory().expect("memory db opens");
        let expected = time::parse("2024-01-02 03:04:05+00:00").unwrap();
        conn.execute_batch(
            "CREATE TABLE probe (a datetime, b datetime, c datetime, d datetime, \
             e datetime, f datetime, g datetime);",
        )
        .expect("probe table");
        conn.execute(
            "INSERT INTO probe VALUES(NULL, '0000-00-00 00:00:00', \
             '2024-01-02 03:04:05+00:00', 'not a date', \
             '2024-01-02 03:04', '2024-01-02', '2024-01-02T03:04:05Z')",
            [],
        )
        .expect("probe row");
        conn.query_row("SELECT * FROM probe", [], |row| {
            assert_eq!(row.get_opt_dt("a")?, None, "SQL NULL is a nil pointer");
            assert_eq!(
                row.get_opt_dt("b")?,
                Some(go_zero_time()),
                "the DDL sentinel scans as a pointer to Go's zero time"
            );
            assert_eq!(row.get_opt_dt("c")?, Some(expected));
            assert_eq!(
                row.get_opt_dt("d")?,
                Some(go_zero_time()),
                "garbage is NOT an error: the driver assigns time.Time{{}}"
            );
            // The driver formats chrono's parser does not try.
            assert_eq!(
                row.get_opt_dt("e")?,
                Some(time::parse("2024-01-02 03:04:00+00:00").unwrap()),
                "minute precision parses (driver format 5)"
            );
            assert_eq!(
                row.get_opt_dt("f")?,
                Some(time::parse("2024-01-02 00:00:00+00:00").unwrap()),
                "date-only parses (driver format 7)"
            );
            assert_eq!(
                row.get_opt_dt("g")?,
                Some(expected),
                "the Z suffix is trimmed, then parsed as UTC"
            );
            Ok(())
        })
        .expect("get_opt_dt read");
    }

    /// The non-pointer getter: SQL NULL errors (Go cannot store nil into
    /// `*time.Time`), and any non-NULL text in a DECLARED timestamp column scans
    /// — unparseable ones as Go's zero. `is_go_zero` is the port's `IsZero()`, and
    /// [`to_go_time_text`] is the `encoding/json` rendering of the same value.
    #[test]
    fn get_dt_errors_on_null_and_scans_unparseable_text_as_go_zero() {
        let conn = Connection::open_in_memory().expect("memory db opens");
        conn.execute_batch("CREATE TABLE probe (t datetime, s datetime, g datetime, c datetime);")
            .expect("probe table");
        conn.execute(
            "INSERT INTO probe VALUES(NULL, '0000-00-00 00:00:00', 'not a date', \
             '2024-01-02 03:04:05+00:00')",
            [],
        )
        .expect("probe row");
        conn.query_row("SELECT * FROM probe", [], |row| {
            assert!(row.get_dt("t").is_err(), "NULL into non-pointer errors");
            Ok(())
        })
        .expect("NULL read");
        conn.query_row("SELECT * FROM probe", [], |row| {
            assert!(
                is_go_zero(&row.get_dt("s")?),
                "the sentinel scans as Go's zero, not an error"
            );
            assert!(
                is_go_zero(&row.get_dt("g")?),
                "garbage scans as Go's zero, not an error"
            );
            assert!(!is_go_zero(&row.get_dt("c")?), "a real stamp is not zero");
            Ok(())
        })
        .expect("get_dt read");

        // The wire form Go's encoding/json emits for a time.Time, zero included.
        assert_eq!(to_go_time_text(&go_zero_time()), "0001-01-01T00:00:00Z");
        assert_eq!(
            to_go_time_text(&time::parse("2024-01-02 03:04:05+00:00").unwrap()),
            "2024-01-02T03:04:05Z",
            "RFC3339Nano with T and Z, not the SQLite storage spelling"
        );
        assert_eq!(
            to_go_time_text(&time::parse("2024-01-02 03:04:05.210+00:00").unwrap()),
            "2024-01-02T03:04:05.21Z",
            "Go trims trailing zeros of the fractional part"
        );
        assert_eq!(
            to_go_time_text(&time::parse("2024-01-02 03:04:05.000+00:00").unwrap()),
            "2024-01-02T03:04:05Z",
            "a zero fractional part prints no dot at all"
        );
    }

    #[test]
    fn a_select_without_libraries_json_errors_not_panics() {
        // The absent-column rule: a light SELECT lacking an optional
        // column must surface a rusqlite error, never a panic.
        let conn = Connection::open_in_memory().expect("memory db opens");
        let err = conn
            .query_row(USER_COLS_NO_JSON, [], row_to_user)
            .unwrap_err();
        assert!(matches!(err, SqliteError::InvalidColumnName(_)), "{err:?}");
        // has_column reports the absence for the tolerant opt-by-name path.
        conn.query_row("SELECT 1 AS id", [], |row| {
            assert!(!row.has_column("libraries_json"));
            assert!(row.has_column("id"));
            Ok(())
        })
        .expect("probe read");
    }

    /// Findings 1 and 5, pure grammar: multi-byte runes inside the hour/offset
    /// slots and a `+` in the month/day slots must be Go's zero time with NO
    /// panic. Go indexes and slices raw bytes (`time/format.go`), so a rune at
    /// the colon is simply not two digits, and `getnum(…, fixed = true)` rejects
    /// a sign — verified against Go 1.27.4 (`time.ParseInLocation` over the nine
    /// `SQLiteTimestampFormats`, all `errBad`).
    #[test]
    fn non_ascii_runes_and_sign_prefixed_date_fields_are_go_zero_without_panic() {
        for text in [
            "2024-02-02 03:00:00\u{00e9}1:30",
            "2024-02-02 03:00:00+03:\u{20ac}",
            "2024-02-02 03:00:00\u{00a0}+03:00",
            "2024-02-02 03:00:00+\u{20ac}3:00",
            "2024-02-02 3:\u{20ac}00:00",
            "2024-+2-02 03:00:00",
            "2024-02-+2 03:00:00",
            "2024-+2-+2 03:00:00",
            "20+4-02-02 03:00:00",
        ] {
            let got = std::panic::catch_unwind(|| driver_parse_text_dt(text));
            let parsed = got.unwrap_or_else(|_| panic!("{text} must not panic"));
            assert!(
                parsed.is_go_zero(),
                "{text} must be Go's zero time, got {}",
                parsed.to_rfc3339_nano()
            );
        }
    }

    /// Finding 5, executed: the same spellings stored in a declared `datetime`
    /// column scan as Go's zero time with no error, exactly as the driver's
    /// failure arm (`sqlite3.go:2831-2838`).
    #[test]
    fn malformed_month_day_signs_scan_as_go_zero_in_a_timestamp_column() {
        let conn = Connection::open_in_memory().expect("memory db opens");
        conn.execute_batch("CREATE TABLE probe (key TEXT, dt datetime)")
            .expect("probe table");
        for (key, value) in [
            ("plus_month", "2024-+2-02 03:00:00"),
            ("plus_day", "2024-02-+2 03:00:00"),
            ("accent_hour", "2024-02-02 03:00:00\u{00e9}1:30"),
            ("euro_offset", "2024-02-02 03:00:00+03:\u{20ac}"),
        ] {
            conn.execute(
                "INSERT INTO probe(key, dt) VALUES(?, ?)",
                rusqlite::params![key, value],
            )
            .unwrap_or_else(|e| panic!("{key} insert: {e}"));
        }
        for key in ["plus_month", "plus_day", "accent_hour", "euro_offset"] {
            let got = conn
                .query_row(
                    "SELECT dt FROM probe WHERE key = ?",
                    rusqlite::params![key],
                    |row| row.get_opt_dt("dt"),
                )
                .unwrap_or_else(|e| panic!("{key}: the driver must not fail a TEXT scan: {e}"));
            assert_eq!(
                got,
                Some(go_zero_time()),
                "{key} is a pointer to Go's zero time"
            );
        }
    }

    /// An invalid UTF-8 TEXT cell remains a TEXT value in SQLite. The pinned Go
    /// driver copies it into a Go string, then its timestamp parser fails and
    /// returns time.Time{} without a scan error (`sqlite3.go:2818-2842`).
    #[test]
    fn invalid_utf8_text_in_declared_datetime_scans_as_go_zero() {
        let conn = Connection::open_in_memory().expect("memory db opens");
        conn.execute_batch(
            "CREATE TABLE probe (dt datetime); \
             INSERT INTO probe VALUES(CAST(X'FF' AS TEXT));",
        )
        .expect("invalid UTF-8 is stored as SQLite TEXT");

        conn.query_row("SELECT dt FROM probe", [], |row| {
            assert_eq!(row.get_opt_dt("dt")?, Some(go_zero_time()));
            assert!(is_go_zero(&row.get_dt("dt")?));
            Ok(())
        })
        .expect("Go's DATETIME parse failure is not a scan failure");
    }

    /// go-sqlite3 returns invalid TEXT bytes as a Go string; encoding/json's
    /// `DecodeRuneInString` replaces one malformed byte at a time.
    #[test]
    fn invalid_utf8_plain_text_scans_as_json_replacement_character() {
        let conn = Connection::open_in_memory().expect("memory db opens");
        conn.execute_batch(
            "CREATE TABLE probe (txt TEXT); \
             INSERT INTO probe VALUES(CAST(X'FF' AS TEXT)); \
             INSERT INTO probe VALUES(CAST(X'F09F80' AS TEXT));",
        )
        .expect("invalid UTF-8 is stored as SQLite TEXT");

        conn.query_row("SELECT txt FROM probe WHERE rowid = 1", [], |row| {
            assert_eq!(row.get_text("txt")?, "\u{FFFD}");
            assert_eq!(row.get_opt_text("txt")?, Some("\u{FFFD}".to_string()));
            Ok(())
        })
        .expect("Go's TEXT scan accepts arbitrary string bytes");
        conn.query_row("SELECT txt FROM probe WHERE rowid = 2", [], |row| {
            assert_eq!(row.get_text("txt")?, "\u{FFFD}\u{FFFD}\u{FFFD}");
            Ok(())
        })
        .expect("Go replaces each byte in an incomplete UTF-8 sequence");
    }

    /// Finding 2, executed: the driver's TEXT parse arm is keyed on the DECLARED
    /// type (`sqlite3.go:2822`), so only `timestamp`/`datetime`/`date` columns
    /// get the conversion. A bare string literal (no decltype), a `CAST(… AS
    /// TEXT)`, and a column declared `TEXT` all hand the driver a `string`, which
    /// `database/sql` cannot store into `time.Time` — a scan error, verified
    /// against the pinned driver on Go 1.27.4.
    #[test]
    fn undeclared_text_and_expressions_are_not_converted_to_a_time() {
        let conn = Connection::open_in_memory().expect("memory db opens");
        conn.execute_batch("CREATE TABLE probe (dt datetime, txt TEXT, weird datetime(3))")
            .expect("probe table");
        conn.execute(
            "INSERT INTO probe VALUES('2024-02-02 03:00:00+03:00', \
             '2024-02-02 03:00:00+03:00', '2024-02-02 03:00:00+03:00')",
            [],
        )
        .expect("probe row");

        // Declared datetime: converted, offset preserved.
        let declared = conn
            .query_row("SELECT dt FROM probe", [], |row| row.get_gotime("dt"))
            .expect("declared datetime converts")
            .expect("non-NULL");
        assert_eq!(declared.to_rfc3339_nano(), "2024-02-02T03:00:00+03:00");

        // Go json.Marshal rejects these years; the String DTO cannot expose
        // that error, so this formatter preserves Chrono's wire spelling.
        for (year, expected) in [
            (0, "0000-01-01T00:00:00Z"),
            (9999, "9999-01-01T00:00:00Z"),
            (10000, "+10000-01-01T00:00:00Z"),
            (-1, "-0001-01-01T00:00:00Z"),
        ] {
            let utc = NaiveDate::from_ymd_opt(year, 1, 1)
                .expect("Chrono supports test year")
                .and_hms_opt(0, 0, 0)
                .expect("midnight is valid");
            let timestamp = GoTime {
                utc,
                offset_secs: 0,
            };
            assert_eq!(timestamp.to_rfc3339_nano(), expected, "year {year}");
        }

        // Declared TEXT: the driver's `default` arm hands back a string.
        for column in ["txt", "weird"] {
            let err = conn
                .query_row(&format!("SELECT {column} FROM probe"), [], |row| {
                    row.get_gotime(column)
                })
                .expect_err("{column} is not a timestamp column, so it must not convert");
            assert!(
                err.to_string().contains("unsupported Scan") && err.to_string().contains("string"),
                "Go's message names the driver type, got {err}"
            );
        }

        // A bare string literal has no declared type at all.
        let literal = conn
            .query_row("SELECT '2024-02-02 03:00:00' AS dt", [], |row| {
                row.get_gotime("dt")
            })
            .expect_err("an expression column has no decltype, so no conversion");
        assert!(
            literal.to_string().contains("unsupported Scan"),
            "got {literal}"
        );

        // `CAST(dt AS TEXT)` likewise loses the declared type (the driver's own
        // `default` arm), and `CAST(… AS INTEGER)` keeps its storage type.
        let cast_text = conn
            .query_row("SELECT CAST(dt AS TEXT) AS dt FROM probe", [], |row| {
                row.get_gotime("dt")
            })
            .expect_err("CAST(… AS TEXT) has no declared type");
        assert!(
            cast_text.to_string().contains("unsupported Scan")
                && cast_text.to_string().contains("string"),
            "got {cast_text}"
        );
        let cast_int = conn
            .query_row("SELECT CAST(dt AS TEXT) AS dt FROM probe", [], |row| {
                row.get_opt_dt("dt")
            })
            .expect_err("the optional getter fails the same way");
        assert!(
            cast_int.to_string().contains("unsupported Scan"),
            "got {cast_int}"
        );
    }

    #[test]
    fn playlist_rules_null_fast_path_uses_only_json_whitespace() {
        // Go encoding/json accepts its four JSON whitespace bytes around null,
        // but rejects Unicode whitespace and other control whitespace.
        for text in ["null", " null ", "\tnull\r\n"] {
            assert_eq!(
                parse_playlist_rules(Some(text)).expect("valid JSON null"),
                None,
                "{text:?} is a nil *Criteria",
            );
        }
        for text in [
            "null\u{00a0}",
            "\u{00a0}null",
            "null\u{000b}",
            "null\u{000c}",
            "null\u{2028}",
        ] {
            assert!(
                parse_playlist_rules(Some(text)).is_err(),
                "{text:?} is not valid JSON whitespace",
            );
        }
    }
}
