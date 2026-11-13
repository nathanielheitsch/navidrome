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

use chrono::{DateTime, NaiveDate, NaiveDateTime, Timelike, Utc};
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
    /// error, exactly as Go's DATETIME arm does (`sqlite3.go:2832-2842`). The
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
        // The driver switches on the STORAGE type first (`sqlite3.go:2779`,
        // `:2813`) and only then on the column's DECLARED type, so both have to be
        // inspected: an INTEGER in a `datetime` column is a timestamp, an INTEGER
        // in a plain `INTEGER` column is not.
        match self.get_ref(column)? {
            // `SQLITE_NULL` → `dest[i] = nil` (`:2832`): a nil pointer for the
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
                // (`sqlite3.go:2822`): only `timestamp`/`datetime`/`date` run the
                // nine-format parse and hand back a `time.Time` (zero on parse
                // failure, no error, `:2832-2842`). Every OTHER declared type —
                // `TEXT`, `varchar(255)`, `datetime(3)`, and an expression column
                // such as `SELECT '…' AS dt` or `CAST(dt AS TEXT)`, which has no
                // decltype at all — falls to `default` (`:2841`) and the driver
                // hands back a `string`, which `database/sql` cannot store into
                // `time.Time`: a scan error, not a silent parse
                // (`database/sql/convert.go`, verified against the pinned driver
                // on Go 1.27.4).
                if !is_timestamp_decltype(column_decl_type(self, column).as_deref()) {
                    return Err(unsupported_scan(column, "string"));
                }
                let text = std::str::from_utf8(bytes).map_err(|e| {
                    SqliteError::FromSqlConversionFailure(0, Type::Text, Box::new(e))
                })?;
                Ok(Some(driver_parse_text_dt(text)))
            }
            rusqlite::types::ValueRef::Blob(_) => Err(unsupported_scan(column, "[]byte")),
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
    /// Go's strict RFC3339 formatter REJECTS a zone hour outside `[0, 23]`
    /// (`timezone hour outside of range [0,23]`), which makes Go's
    /// `json.Marshal` of that value fail outright (verified against the pinned
    /// toolchain for `2024-02-02 03:00:00+24:60`, whose offset parses as 25
    /// hours). The failure belongs to marshal time — a read the driver already
    /// succeeded — and the annotation DTO has no error channel, so the port
    /// renders the UTC-normalised text instead. That is the one documented
    /// divergence in this fn.
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
/// `declTypes` does (`sqlite3.go:2638-2649`).
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

/// The pinned driver's TEXT DATETIME decode (`sqlite3.go:2821-2842`) as Go's own
/// grammar, shared by [`RowGet::get_dt`], [`RowGet::get_opt_dt`] and the
/// annotation wire-text reader:
///
/// * `strings.TrimSuffix(s, "Z")` (`:2823`) removes **exactly one** trailing `Z`,
///   so `…+03:00Z` only loses the last character and still matches format 1, while
///   `…ZZ` matches nothing;
/// * `SQLiteTimestampFormats` (`:331-343`) are tried in order, each with
///   `time.ParseInLocation(format, s, time.UTC)` — the first success wins;
/// * **any** failure assigns `time.Time{}` and reports NO error (`:2832-2842`):
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
    // Go's scanner accepts only the four JSON whitespace bytes (SP, TAB, CR, LF);
    // Unicode `trim` would incorrectly accept NBSP/VT/etc. around literal null.
    let json_trimmed = payload.trim_matches(|ch| matches!(ch, ' ' | '\t' | '\r' | '\n'));
    if payload.is_empty() || json_trimmed == "null" {
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
    /// failure arm (`sqlite3.go:2832-2842`).
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
