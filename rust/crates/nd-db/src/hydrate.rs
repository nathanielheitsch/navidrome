//! S-D3 shared hydration core: the row → entity-record seam for the eight Go
//! `PostScan` hooks (`docs/rust-port/hydrate-scan-contract.md`, S-D3 brief).
//!
//! ## Two-phase shape (Go's `Scan` → `PostScan`)
//!
//! Go's dbx first scans the raw row columns into the `db*` struct and then
//! runs the entity's `PostScan` hook to decode the JSON shim columns
//! (`persistence/rows.go` machinery, `dbUser.PostScan`
//! `persistence/user_repository.go:36-43`). The port mirrors that split
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
//! the `rules` shim and the LEFT-joined annotation columns).
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
//! (`go-sqlite3@v1.14.52/sqlite3.go:2832-2842`) swallows ANY DATETIME parse
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
//! [`go_zero_time`] above (`sqlite3.go:2832-2842`), and a pointer column gets a
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
//! (`persistence/rows.go:84`). A ported hydrator must therefore treat
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
//! No new prefixes are invented; hooks whose Go code has no wrapper use
//! the codec's own message.

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use rusqlite::types::Type;
use rusqlite::{Error as SqliteError, Result as SqliteResult, Row};

use nd_model::annotation::Annotation;
use nd_model::criteria::unmarshal_criteria_str;
use nd_model::fields::default_field_map;

use crate::folder_repo::{FolderRecord, FolderRow};
use crate::playlist_repo::{PlaylistRecord, PlaylistRow};
use crate::sql_json::unmarshal_image_files;
use crate::user_repo::{UserLibrary, UserRecord, UserRow};

/// Go's `dbUser.PostScan` wrapper, verbatim
/// (`persistence/user_repository.go:38`).
pub const USER_LIBRARIES_PREFIX: &str = "parsing user libraries from db: ";

/// Go's `dbFolder.PostScan` wrapper, verbatim
/// (`persistence/folder_repository.go:42`).
pub const FOLDER_IMAGE_FILES_PREFIX: &str = "parsing folder image files from db: ";

/// Go's zero `time.Time` (`time.Time{}`), the value the pinned driver returns
/// for any DATETIME text it cannot parse
/// (`go-sqlite3@v1.14.52/sqlite3.go:2832-2842`: the `SQLiteTimestampFormats`
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
    /// The seam's failure as a `rusqlite` conversion failure, the shape Go's
    /// `ScanStruct` error takes out of the read (`database/sql` surfaces a
    /// column-conversion failure from `Rows.Scan`): a
    /// `FromSqlConversionFailure` whose payload IS the Go-prefixed message.
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
    /// A TEXT column Go scans into a non-pointer `string`: **NULL errors**
    /// (Go parity), as does an absent column.
    fn get_text(&self, column: &str) -> SqliteResult<String>;

    /// A coalesce'd INTEGER `bool` column (Go's `bool` non-pointer):
    /// **NULL errors**.
    fn get_bool(&self, column: &str) -> SqliteResult<bool>;

    /// A non-pointer `int` column: **NULL errors**.
    fn get_i32(&self, column: &str) -> SqliteResult<i32>;

    /// A non-pointer `int64` column: **NULL errors**.
    fn get_i64(&self, column: &str) -> SqliteResult<i64>;

    /// A non-pointer `real` column: **NULL errors**.
    fn get_f64(&self, column: &str) -> SqliteResult<f64>;

    /// A `*string` column: NULL → `None`.
    fn get_opt_text(&self, column: &str) -> SqliteResult<Option<String>>;

    /// A `*float64` column (the `rg_*` shims, `average_rating`):
    /// NULL → `None`.
    fn get_opt_f64(&self, column: &str) -> SqliteResult<Option<f64>>;

    /// A `*time.Time` column: SQL NULL → `None` (Go's nil pointer,
    /// `database/sql/convert.go:491-493`); text the driver cannot parse →
    /// `Some(`[`go_zero_time`]`)`, a POINTER TO THE ZERO VALUE, because the
    /// driver's parse-failure arm hands back `time.Time{}` with no error
    /// (`go-sqlite3@v1.14.52/sqlite3.go:2832-2842`) and `database/sql` then
    /// allocates a fresh `time.Time` for it (`convert.go:495-496`). That is why
    /// Go's JSON for such a field still renders `0001-01-01T00:00:00Z` instead
    /// of being dropped by `omitempty`.
    fn get_opt_dt(&self, column: &str) -> SqliteResult<Option<DateTime<Utc>>>;

    /// A non-pointer `time.Time` column: **SQL NULL errors** (Go parity —
    /// `convert.go` cannot store `driver.Value` nil into `*time.Time`), but a
    /// non-NULL value the driver cannot parse scans as [`go_zero_time`] with no
    /// error, exactly as Go's DATETIME arm does (`sqlite3.go:2832-2842`). The
    /// DDL's [`crate::time::ZERO_DATE_SENTINEL`] default therefore scans, it does
    /// not fail the read.
    fn get_dt(&self, column: &str) -> SqliteResult<DateTime<Utc>>;

    /// `true` when the row's SELECT emits `column`. The
    /// absent-optional-column rule: joined columns (`annotation`,
    /// `bookmark`, …) may be absent on light queries. Probing uses the
    /// accessor's own missing-column error, so no statement handle is
    /// needed.
    fn has_column(&self, column: &str) -> bool;
}

impl RowGet for Row<'_> {
    fn get_text(&self, column: &str) -> SqliteResult<String> {
        let v: Option<String> = self.get(column)?;
        match v {
            Some(v) => Ok(v),
            // Go: NULL into a non-pointer string errors.
            None => Err(null_required(column, Type::Text)),
        }
    }

    fn get_bool(&self, column: &str) -> SqliteResult<bool> {
        let v: Option<bool> = self.get(column)?;
        match v {
            Some(v) => Ok(v),
            None => Err(null_required(column, Type::Integer)),
        }
    }

    fn get_i32(&self, column: &str) -> SqliteResult<i32> {
        let v: Option<i32> = self.get(column)?;
        match v {
            Some(v) => Ok(v),
            None => Err(null_required(column, Type::Integer)),
        }
    }

    fn get_i64(&self, column: &str) -> SqliteResult<i64> {
        let v: Option<i64> = self.get(column)?;
        match v {
            Some(v) => Ok(v),
            None => Err(null_required(column, Type::Integer)),
        }
    }

    fn get_f64(&self, column: &str) -> SqliteResult<f64> {
        let v: Option<f64> = self.get(column)?;
        match v {
            Some(v) => Ok(v),
            None => Err(null_required(column, Type::Real)),
        }
    }

    fn get_opt_text(&self, column: &str) -> SqliteResult<Option<String>> {
        self.get(column)
    }

    fn get_opt_f64(&self, column: &str) -> SqliteResult<Option<f64>> {
        self.get(column)
    }

    fn get_opt_dt(&self, column: &str) -> SqliteResult<Option<DateTime<Utc>>> {
        // SQL NULL → None (nil pointer); unparseable text → Some(Go's zero),
        // NOT an error and NOT None — the driver never fails a DATETIME parse
        // and `database/sql` allocates a pointer for the value it gets back.
        let raw: Option<String> = self.get(column)?;
        Ok(raw.map(|text| driver_parse_dt(&text)))
    }

    fn get_dt(&self, column: &str) -> SqliteResult<DateTime<Utc>> {
        // Non-pointer Go `time.Time`: SQL NULL errors like every bare getter
        // (`convert.go` cannot store nil into `*time.Time`), and any non-NULL
        // text — the DDL sentinel included — scans, unparseable ones as Go's
        // zero time.
        let raw: Option<String> = self.get(column)?;
        match raw {
            None => Err(null_required(column, Type::Text)),
            Some(text) => Ok(driver_parse_dt(&text)),
        }
    }

    fn has_column(&self, column: &str) -> bool {
        // Row has no `column_index`; the accessor's own missing-column
        // error is the probe. `get_ref` needs no conversion, so a present
        // column never errors here.
        self.get_ref(column).is_ok()
    }
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

/// The pinned driver's DATETIME decode (`go-sqlite3@v1.14.52/sqlite3.go:2821-2842`)
/// as one fn, shared by [`RowGet::get_dt`] and [`RowGet::get_opt_dt`]:
///
/// * trailing `Z` is trimmed (`sqlite3.go:2823`), then `SQLiteTimestampFormats`
///   (`:331-343`) are tried in order in UTC;
/// * **any** failure assigns `time.Time{}` and reports NO error
///   (`:2832-2842`) — the DDL's `'0000-00-00 00:00:00'` default, a naive
///   `CURRENT_TIMESTAMP` value that carries no offset, and outright garbage all
///   land here;
/// * the value is then converted to the connection location (`:2836-2838`),
///   which this port keeps at UTC (no `_loc` DSN parameter is set).
///
/// So an unparseable cell is [`go_zero_time`], never an error and never "absent".
#[must_use]
fn driver_parse_dt(text: &str) -> DateTime<Utc> {
    // The pinned driver's own loop (`sqlite3.go:2821-2842`): trim a trailing `Z`,
    // then try `SQLiteTimestampFormats` (`:331-343`) in order, all in UTC.
    // Go reports an error for an impossible calendar date (`0000-00-00` — the
    // DDL sentinel — included) and the driver's failure arm assigns
    // `time.Time{}` with NO error, so unparseable text is [`go_zero_time`].
    let trimmed = text.strip_suffix('Z').unwrap_or(text);
    // Go's layouts all begin with the literal `2006-01-02`; chrono's `%F` also
    // accepts other separators and underflows `0000-00-00` into a plausible
    // date, so the head is validated first.
    let Some(date) = go_calendar_date(trimmed) else {
        return go_zero_time();
    };
    let rest = trimmed.get(10..).unwrap_or("");
    // Formats 1-2: date, time, optional fraction, and an offset — an instant,
    // converted to UTC (Go parses in UTC and then converts to the connection
    // location, which this port keeps at UTC).
    for format in ["%F %T%.f%:z", "%FT%T%.f%:z", "%F %T%.f%#z", "%FT%T%.f%#z"] {
        if !rest.is_empty() {
            if let Ok(dt) = DateTime::parse_from_str(trimmed, format) {
                return dt.with_timezone(&Utc);
            }
        }
    }
    // Formats 3-8: date + time with no offset, read in UTC exactly as
    // `ParseInLocation(…, time.UTC)`; minute precision fills seconds with zero.
    for format in ["%F %T%.f", "%FT%T%.f", "%F %H:%M", "%FT%H:%M"] {
        if !rest.is_empty() {
            if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, format) {
                return Utc.from_utc_datetime(&naive);
            }
        }
    }
    // Format 9: date only, midnight.
    if rest.is_empty() {
        return Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap_or_default());
    }
    // Go's `err != nil` arm: `t = time.Time{}`, and no error is reported.
    go_zero_time()
}

/// The calendar date Go's `time.Parse` requires at the head of every accepted
/// layout: `YYYY-MM-DD` with real dashes and a valid month/day. `None` means Go
/// reports a parse error, which the driver turns into [`go_zero_time`].
fn go_calendar_date(text: &str) -> Option<NaiveDate> {
    let head = text.get(..10)?;
    let bytes = head.as_bytes();
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let mut parts = head.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    NaiveDate::from_ymd_opt(year, month, day)
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
    serde_json::from_str(json)
}

// ---------------------------------------------------------------------------
// S-D3b: folder + playlist — the PostScan hooks of
// `persistence/folder_repository.go:38-46` and
// `persistence/playlist_repository.go:29-34`.
// ---------------------------------------------------------------------------

/// `dbFolder` row mapping (phase 1 of the two-phase seam): the columns
/// `selectFolder` scans (`persistence/folder_repository.go:58-62` —
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
            created_at: folder_created_at(row)?,
            // Join-only (`structs:"-"`): read onto the record, never written.
            library_path: row.get_text("library_path")?,
            // The row's own primary key (`Folder.ID` scans `id` normally).
            id: row.get_text("id")?,
        },
        image_files,
    })
}

/// `Folder.CreatedAt` as the record spells it: Go's non-pointer `time.Time`
/// scanned from `created_at` — SQL NULL errors the read (`get_dt`), a value the
/// driver cannot parse scans as Go's zero time, and Go's zero means "never
/// stamped", which this record writes as `None`.
///
/// # Errors
/// Propagates the NULL-into-non-pointer scan failure.
fn folder_created_at(row: &Row<'_>) -> SqliteResult<Option<DateTime<Utc>>> {
    row.get_dt("created_at")
        .map(|dt| if is_go_zero(&dt) { None } else { Some(dt) })
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
/// `sql_annotations.go:63-88`) — `playlist.*`, the six annotation columns and
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
            created_at: playlist_created_at(row)?,
            updated_at: row.get_dt("updated_at")?,
            imported_hash: row.get_text("imported_hash")?,
            // Decoded by phase 2 from the raw shim column.
            rules: None,
            evaluated_at: row.get_opt_dt("evaluated_at")?,
            annotation: row_to_annotation(row)?,
            // `OwnerName` is scanned by field name despite `structs:"-"`.
            owner_name: owner_name_column(row)?,
            // The row's own primary key (`Playlist.ID` scans `id` normally).
            id: row.get_text("id")?,
        },
        rules,
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
/// non-pointer one).
pub fn row_to_annotation(row: &Row<'_>) -> SqliteResult<Annotation> {
    Ok(Annotation {
        play_count: if row.has_column("play_count") {
            row.get_i64("play_count")?
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

/// A joined `*time.Time` column as the model's wire string: absent → `None`,
/// SQL NULL → `None` (Go's nil pointer), an unparseable cell →
/// `Some(0001-01-01T00:00:00Z)` (a pointer to Go's zero time), a real stamp →
/// Go's RFC3339Nano text.
fn optional_dt_text(row: &Row<'_>, column: &str) -> SqliteResult<Option<String>> {
    if !row.has_column(column) {
        return Ok(None);
    }
    Ok(row.get_opt_dt(column)?.map(|dt| to_go_time_text(&dt)))
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

/// `Playlist.CreatedAt` as the record spells it: Go's non-pointer `time.Time`
/// scanned from `created_at`. SQL NULL fails the read; Go's zero (an unparseable
/// cell) means "never stamped" and is the record's `None`.
///
/// # Errors
/// Propagates the NULL-into-non-pointer scan failure.
fn playlist_created_at(row: &Row<'_>) -> SqliteResult<Option<DateTime<Utc>>> {
    row.get_dt("created_at")
        .map(|dt| if is_go_zero(&dt) { None } else { Some(dt) })
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
    // Go's guard is on the raw string (`:30`); `json.Unmarshal` also trims
    // whitespace, so a whitespace-only `null` is Go's nil arm too.
    if payload.is_empty() || payload.trim() == "null" {
        return Ok(None);
    }
    unmarshal_criteria_str(&default_field_map(), payload.to_string()).map(Some)
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
    /// ANY unparseable non-NULL text is a pointer to Go's zero time
    /// (`Some(go_zero_time())`) and NOT an error — the sentinel included — and
    /// a parseable cell parses.
    #[test]
    fn opt_dt_is_nil_pointer_on_null_and_pointer_to_go_zero_on_unparseable_text() {
        let conn = Connection::open_in_memory().expect("memory db opens");
        let expected = time::parse("2024-01-02 03:04:05+00:00").unwrap();
        conn.query_row(
            "SELECT NULL AS a, '0000-00-00 00:00:00' AS b, \
             '2024-01-02 03:04:05+00:00' AS c, 'not a date' AS d, \
             '2024-01-02 03:04' AS e, '2024-01-02' AS f, \
             '2024-01-02T03:04:05Z' AS g",
            [],
            |row| {
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
            },
        )
        .expect("get_opt_dt read");
    }

    /// The non-pointer getter: SQL NULL errors (Go cannot store nil into
    /// `*time.Time`), and any non-NULL text scans — unparseable ones as Go's
    /// zero. `is_go_zero` is the port's `IsZero()`, and [`to_go_time_text`] is
    /// the `encoding/json` rendering of the same value.
    #[test]
    fn get_dt_errors_on_null_and_scans_unparseable_text_as_go_zero() {
        let conn = Connection::open_in_memory().expect("memory db opens");
        conn.query_row("SELECT NULL AS t", [], |row| {
            assert!(row.get_dt("t").is_err(), "NULL into non-pointer errors");
            Ok(())
        })
        .expect("NULL read");
        conn.query_row(
            "SELECT '0000-00-00 00:00:00' AS s, 'not a date' AS g, \
             '2024-01-02 03:04:05+00:00' AS c",
            [],
            |row| {
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
            },
        )
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
}
