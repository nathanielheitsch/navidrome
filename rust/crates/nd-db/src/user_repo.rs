//! Port of `persistence/user_repository.go` (518L) — the user repository's
//! SQL shaping plus its executed read/write path.
//!
//! Every Go method either builds a squirrel `SelectBuilder` and materializes
//! it through `toSQL()` (`sql_base_repository.go:358-372`) or hands an
//! `Update`/`Insert`/`Delete` statement to `executeSQL`; the shaping ports stop
//! at the same `(sql, args)` pairs, and [`UserRepo`] is the `executeSQL`/
//! `queryOne`/`queryAll` half that runs them. Where the [`Repository`] builder
//! can render the statement (plain `SELECT`s with INNER JOIN/`Eq`) it is used;
//! [`select_user_with_libraries`] needs `LEFT JOIN` + `GROUP BY`, which
//! squirrel renders but the builder does not, so that one select is shaped
//! directly, keeping the identifier safety rule of `sqlrepo.rs` (identifiers
//! are this repository's registered names, values are always parameterized).
//!
//! The heart of the port is [`put_password_update_sql`]: the token-epoch
//! revocation of `user_repository.go:129-144`. It is nd-auth #13's hard
//! dependency — the `token_epoch` bump invalidates every cached auth token
//! for the user, and the `RETURNING token_epoch` leg is what lets the
//! caller's own in-flight token be refreshed instead of logged out.
//!
//! The executed half is [`UserRepo`] — `Get`/`GetAll`/`FindByUsername`/
//! `FindFirstAdmin`/`CountAll`/`Put`/`UpdateLastLoginAt`/`UpdateLastAccessAt`/
//! `Delete`/`SetUserLibraries` run through [`crate::execution`] on a leased
//! connection, and [`UserRow`] is `dbUser`: the raw row plus its
//! `libraries_json`, hydrated by [`UserRow::into_record`] exactly where Go's
//! `PostScan` runs (`:36-43`). Password encryption/decryption stays with
//! nd-auth, so reads hand back the stored ciphertext verbatim.
//!
//! Not ported (different slices): `encryptPassword`/`decryptPassword` and
//! `initPasswordEncryptionKey` (`:453-518`, nd-auth/nd-utils), the `rest`
//! permission wrappers `Count`/`Read`/`ReadAll`/`Save`/`Update`
//! (`:206-252`, nd-api), `validatePasswordChange`/`validateUsernameUnique`/
//! `validateScrobbleFilter` (`:254-318`, nd-api),
//! `cleanupPluginUserReferences` (`:330-337`, plugin slice), and
//! `GetUserLibraries`'s full library-row hydration (`:485-495`, which reads
//! every `library` column — the library repository's model half).

use crate::sqlrepo::{Repository, Value};

/// `r.tableName` for the user repository (`user_repository.go:47`).
pub const USER_TABLE: &str = "user";

/// The join table of the user↔library many-to-many
/// (`user_repository.go:438,451`).
pub const USER_LIBRARY_TABLE: &str = "user_library";

/// One materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// The `libraries_json` select column of `selectUserWithLibraries`
/// (`user_repository.go:66-81`): one JSON array of the user's libraries per
/// row, coalesced to `'[]'` for a user with none (the `FILTER (WHERE
/// library.id IS NOT NULL)` drops the NULL row the LEFT JOIN produces, which
/// would otherwise render a `[null]` array).
#[must_use]
pub fn libraries_json_column() -> String {
    "COALESCE(json_group_array(json_object(\
     'id', library.id, \
     'name', library.name, \
     'path', library.path, \
     'remote_path', library.remote_path, \
     'last_scan_at', library.last_scan_at, \
     'last_scan_started_at', library.last_scan_started_at, \
     'full_scan_in_progress', library.full_scan_in_progress, \
     'updated_at', library.updated_at, \
     'created_at', library.created_at\
     )) FILTER (WHERE library.id IS NOT NULL), '[]') AS libraries_json"
        .to_string()
}

/// `selectUserWithLibraries` (`user_repository.go:62-83`): the base select
/// every read path builds on — `user.*` plus the hydrated libraries JSON, the
/// two LEFT JOINs and the `GROUP BY user.id` that collapses the join's row
/// multiplication. `where_sql`/`where_args` pin the caller's clause
/// (`Eq{"user.id": id}` etc.); `order_by` carries each expression's direction
/// (Go's `buildSortOrder` output); `limit`/`offset` are the `Max`/`Offset`
/// of `model.QueryOptions`, rendered as decimal literals like squirrel.
///
/// Clause order follows squirrel's renderer (`select.go:117-148`): JOINs, WHERE,
/// GROUP BY, ORDER BY, LIMIT, OFFSET — so `GROUP BY` sits *after* the caller's
/// WHERE, not before it (`GROUP BY … WHERE` is a SQLite parse error).
///
/// Deviation (deliberate, cited): SQLite only accepts `OFFSET` with a `LIMIT`
/// (`select 1 offset 1` is a parse error), while squirrel emits a bare
/// `OFFSET n` (`select.go:144-147`) whenever `Max` is unset, so Go's
/// offset-only page fails at the driver (`sql_base_repository.go:151-157`).
/// Instead of reproducing that latent failure, an offset with no limit renders
/// `LIMIT -1` — SQLite's unlimited bound — before `OFFSET n`. The bounds are
/// literals (`select.go:139-147`), so they never join the args list.
#[must_use]
pub fn select_user_with_libraries(
    where_sql: Option<&str>,
    where_args: Vec<Value>,
    order_by: &[&str],
    limit: Option<u64>,
    offset: Option<u64>,
) -> Sql {
    let mut sql = format!(
        "SELECT user.*, {} FROM user \
         LEFT JOIN user_library ul ON user.id = ul.user_id \
         LEFT JOIN library ON ul.library_id = library.id",
        libraries_json_column()
    );
    let args = where_args;
    if let Some(clause) = where_sql {
        sql.push_str(&format!(" WHERE {clause}"));
    }
    sql.push_str(" GROUP BY user.id");
    if !order_by.is_empty() {
        sql.push_str(&format!(" ORDER BY {}", order_by.join(", ")));
    }
    if let Some(max) = limit {
        // squirrel renders the bound as a decimal literal and binds nothing
        // (`select.go:139-142`), so it never joins the args list.
        sql.push_str(&format!(" LIMIT {max}"));
    }
    if let Some(off) = offset {
        if limit.is_none() {
            // Deviation (deliberate, cited): squirrel emits a bare `OFFSET n`
            // when Max is unset (`applyOptions` sets the bounds independently,
            // `sql_base_repository.go:151-156`), which SQLite rejects as a parse
            // error. SQLite is Navidrome's primary engine, so the port emits
            // `LIMIT -1` (its unlimited bound) ahead of the offset.
            sql.push_str(" LIMIT -1");
        }
        sql.push_str(&format!(" OFFSET {off}"));
    }
    (sql, args)
}

/// `CountAll` (`user_repository.go:86-88`): `r.count(ctx, Select(), qo...)`
/// drops the options and keeps the DISTINCT aggregate
/// (`sql_base_repository.go:580-590`).
#[must_use]
pub fn count_all_sql() -> Sql {
    Repository::new(USER_TABLE)
        .count(USER_TABLE)
        .to_sql()
        .expect("the bare user count always renders")
}

/// Count users matching a username, using the column's declared collation.
#[must_use]
pub fn count_by_username_sql(user_name: &str) -> Sql {
    (
        "SELECT count(distinct user.id) as count FROM user WHERE user_name = ? ORDER BY user.id"
            .into(),
        vec![Value::from(user_name)],
    )
}

/// `Get` (`user_repository.go:90-97`): the libraries select for one id.
#[must_use]
pub fn get_user_sql(id: &str) -> Sql {
    select_user_with_libraries(Some("user.id = ?"), vec![Value::from(id)], &[], None, None)
}

/// `GetAll` (`user_repository.go:99-105`): the libraries select under the
/// caller's `model.QueryOptions` (`order_by` already carries directions;
/// `limit`/`offset` are `Max`/`Offset`).
#[must_use]
pub fn get_all_users_sql(order_by: &[&str], limit: Option<u64>, offset: Option<u64>) -> Sql {
    select_user_with_libraries(None, Vec::new(), order_by, limit, offset)
}

/// `FindFirstAdmin` (`user_repository.go:146-152`): one admin, taken in
/// ascending `updated_at` order — `QueryOptions{Sort: "updated_at", Max: 1}`
/// plus `Eq{"user.is_admin": true}`. Go passes no `Order`, so `buildSortOrder`
/// (`sql_base_repository.go:199-229`) appends `asc` and `LIMIT 1` selects the
/// EARLIEST-updated admin, not the newest. Callers only need an admin to act
/// as (`core/auth/auth.go:157`, `scanner/phase_4_playlists.go:56`), so the
/// order is behaviour-neutral there; the port keeps it verbatim.
#[must_use]
pub fn find_first_admin_sql() -> Sql {
    select_user_with_libraries(
        Some("user.is_admin = ?"),
        vec![Value::Bool(true)],
        &["updated_at"],
        Some(1),
        None,
    )
}

/// `FindByUsername` (`user_repository.go:154-160`): the case-insensitive
/// username lookup (`model/user.go:65` requires it) via
/// `user.user_name = ? COLLATE NOCASE` — the same collation the schema pins on
/// the column (`user.user_name varchar(255) unique`, nd-dbschema).
#[must_use]
pub fn find_by_username_sql(username: &str) -> Sql {
    select_user_with_libraries(
        Some("user.user_name = ? COLLATE NOCASE"),
        vec![Value::from(username)],
        &[],
        None,
        None,
    )
}

/// The `toSQLArgs(*u)` payload of `Put` (`user_repository.go:106-121`): the
/// `model.User` row columns in the struct's declaration order
/// (`model/user.go:13-35`), which is the order the ports pin for squirrel's
/// map-iteration-order `SetMap`/`Insert` renders. `password` is the encrypted
/// `u.NewPassword` (`structs:"password,omitempty"` — omitted when unset, so
/// a password-less `Put` never rewrites the column); `current_password` is
/// never part of the row (Go deletes it, `user_repository.go:120`).
#[derive(Debug, Clone, PartialEq)]
pub struct UserPutValues<'a> {
    /// `user.id` (Go generates one when empty, `:108-110`).
    pub id: &'a str,
    /// `user.user_name`.
    pub user_name: &'a str,
    /// `user.name`.
    pub name: &'a str,
    /// `user.email`.
    pub email: &'a str,
    /// `user.password` — the encrypted `NewPassword`; `None` omits the column.
    pub password: Option<&'a str>,
    /// `user.is_admin`.
    pub is_admin: bool,
    /// `user.last_login_at` (`*time.Time`, NULL when `None`).
    pub last_login_at: Option<&'a str>,
    /// `user.last_access_at` (`*time.Time`, NULL when `None`).
    pub last_access_at: Option<&'a str>,
    /// `user.created_at` — only the INSERT leg's fresh stamp is used; the
    /// UPDATE keeps whatever the caller carried.
    pub created_at: &'a str,
    /// `user.updated_at` — Go stamps `time.Now()` before rendering
    /// (`user_repository.go:111`).
    pub updated_at: &'a str,
    /// `user.scrobble_filter`.
    pub scrobble_filter: &'a str,
}

impl UserPutValues<'_> {
    /// The bare column names (the SET leg renders them as `col = ?`) with
    /// their bound values, in the declaration order above. `password` joins
    /// only when [`Self::password`] is set.
    fn names_and_args(&self) -> (Vec<&str>, Vec<Value>) {
        let mut columns = vec![
            "id",
            "user_name",
            "name",
            "email",
            "is_admin",
            "last_login_at",
            "last_access_at",
            "created_at",
            "updated_at",
            "scrobble_filter",
        ];
        let mut args = vec![
            Value::from(self.id),
            Value::from(self.user_name),
            Value::from(self.name),
            Value::from(self.email),
            Value::Bool(self.is_admin),
            opt_time(self.last_login_at),
            opt_time(self.last_access_at),
            Value::from(self.created_at),
            Value::from(self.updated_at),
            Value::from(self.scrobble_filter),
        ];
        if let Some(password) = self.password {
            // Squirrel renders the omitempty `password` after `email`, the
            // struct's declaration slot (model/user.go:19 vs :23).
            columns.insert(4, "password");
            args.insert(4, Value::from(password));
        }
        (columns, args)
    }
}

/// `*time.Time` → bound value: NULL when unset, text otherwise.
fn opt_time(stamp: Option<&str>) -> Value {
    stamp.map(Value::from).unwrap_or(Value::Null)
}

/// The password-less `Put` UPDATE (`user_repository.go:122-128,133-139`):
/// `Update(user).Where(Eq{"id": u.ID}).SetMap(values)`, executed and its
/// affected-row count deciding `isNewUser` (0 rows → INSERT fallback).
#[must_use]
pub fn put_update_sql(values: &UserPutValues<'_>) -> Sql {
    let (cols, mut args) = values.names_and_args();
    args.push(Value::from(values.id));
    (
        format!(
            "UPDATE {USER_TABLE} SET {} WHERE id = ?",
            cols.iter()
                .map(|c| format!("{c} = ?"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        args,
    )
}

/// THE TOKEN-EPOCH REVOCATION — `user_repository.go:129-144`.
///
/// This is nd-auth #13's hard dependency: when a password changes, the same
/// single UPDATE also bumps `user.token_epoch = token_epoch + 1`, so every
/// auth token issued before the change — which embeds the old epoch — fails
/// its revalidation and the user's cached sessions are invalidated. Go's
/// comment on why the bump *rides* the UPDATE instead of following it: as two
/// statements they can interleave with a concurrent change and leave a
/// session valid that the other change should have revoked
/// (`user_repository.go:123-125`). The `RETURNING token_epoch` suffix hands
/// back the *new* epoch so the caller can refresh their own in-flight token
/// ([`refresh_inflight_token`]); "no row" (`ErrNotFound`) is the `isNewUser`
/// signal for the INSERT fallback.
#[must_use]
pub fn put_password_update_sql(values: &UserPutValues<'_>) -> Sql {
    let (cols, mut args) = values.names_and_args();
    args.push(Value::from(values.id));
    let set = cols
        .iter()
        .map(|c| format!("{c} = ?"))
        .collect::<Vec<_>>()
        .join(", ");
    (
        format!(
            "UPDATE {USER_TABLE} SET {set}, token_epoch = token_epoch + 1 \
             WHERE id = ? RETURNING token_epoch"
        ),
        args,
    )
}

/// The `isNewUser` INSERT fallback of `Put` (`user_repository.go:140-147`):
/// `Insert(user).SetMap(values)` after `values["created_at"] = time.Now()`
/// stamps the row's real creation time — the caller passes that fresh stamp,
/// not [`UserPutValues::created_at`], because the struct's `CreatedAt` is the
/// zero time for a brand-new user.
#[must_use]
pub fn put_insert_sql(values: &UserPutValues<'_>, created_at: &str) -> Sql {
    let stamped = UserPutValues {
        created_at,
        ..values.clone()
    };
    let (cols, args) = stamped.names_and_args();
    let placeholders = vec!["?"; args.len()].join(", ");
    (
        format!(
            "INSERT INTO {USER_TABLE} ({}) VALUES ({placeholders})",
            cols.join(", ")
        ),
        args,
    )
}

/// The admin library auto-assign of `Put` (`user_repository.go:150-156`):
/// every library, in one `INSERT OR IGNORE` so re-running `Put` for an admin
/// cannot duplicate rows.
#[must_use]
pub fn assign_all_libraries_sql(user_id: &str) -> Sql {
    (
        format!(
            "INSERT OR IGNORE INTO {USER_LIBRARY_TABLE} (user_id, library_id) SELECT ?, id FROM library"
        ),
        vec![Value::from(user_id)],
    )
}

/// The new-regular-user default-library assign of `Put`
/// (`user_repository.go:157-165`): only libraries flagged
/// `default_new_users = true`, and only on first creation (`isNewUser`).
#[must_use]
pub fn assign_default_libraries_sql(user_id: &str) -> Sql {
    (
        format!(
            "INSERT OR IGNORE INTO {USER_LIBRARY_TABLE} (user_id, library_id) SELECT ?, id FROM library WHERE default_new_users = true"
        ),
        vec![Value::from(user_id)],
    )
}

/// The in-flight-token refresh condition of `Put` (`user_repository.go:160-163`):
/// only the caller's own password change refreshes their token — an admin
/// resetting another user must keep their own epoch. The caller compares
/// `loggedUser(ctx).ID == u.ID` on the Go side; here the boolean is the input.
/// nd-auth #13 consumes the returned epoch to mint the refreshed token.
#[must_use]
pub fn refresh_inflight_token(
    password_changed: bool,
    is_new_user: bool,
    caller_is_user: bool,
) -> bool {
    password_changed && !is_new_user && caller_is_user
}

/// `UpdateLastLoginAt` (`user_repository.go:167-170`).
#[must_use]
pub fn update_last_login_at_sql(id: &str, now: &str) -> Sql {
    (
        format!("UPDATE {USER_TABLE} SET last_login_at = ? WHERE id = ?"),
        vec![Value::from(now), Value::from(id)],
    )
}

/// `UpdateLastAccessAt` (`user_repository.go:172-176`).
#[must_use]
pub fn update_last_access_at_sql(id: &str, now: &str) -> Sql {
    (
        format!("UPDATE {USER_TABLE} SET last_access_at = ? WHERE id = ?"),
        vec![Value::from(now), Value::from(id)],
    )
}

/// `deleteByID` for `Delete` (`user_repository.go:329-332` via
/// `sql_base_repository.go`): one id per statement, executed in a loop.
/// `cleanupPluginUserReferences` belongs to the plugin slice and is absent.
#[must_use]
pub fn delete_user_sql(id: &str) -> Sql {
    (
        format!("DELETE FROM {USER_TABLE} WHERE id = ?"),
        vec![Value::from(id)],
    )
}

/// `GetUserLibraries` (`user_repository.go:438-448`): the libraries of one
/// user, name-ordered. Go's `Select("l.*").From("library l").Join(...)`
/// is an INNER JOIN, so a user with no associations gets no rows.
#[must_use]
pub fn get_user_libraries_sql(user_id: &str) -> Sql {
    Repository::new("library l")
        .select(["l.*"])
        .join("user_library ul", "l.id = ul.library_id", Vec::new())
        .where_eq("ul.user_id", user_id)
        .order_by(["l.name"])
        .to_sql()
        .expect("the library select always renders")
}

/// The DELETE leg of `SetUserLibraries` (`user_repository.go:451-453`):
/// drop every existing association before inserting the new set.
#[must_use]
pub fn clear_user_libraries_sql(user_id: &str) -> Sql {
    (
        format!("DELETE FROM {USER_LIBRARY_TABLE} WHERE user_id = ?"),
        vec![Value::from(user_id)],
    )
}

/// The INSERT leg of `SetUserLibraries` (`user_repository.go:455-462`): one
/// multi-row `VALUES` statement for the whole new set. Go skips the INSERT
/// for an empty set (the DELETE above has already emptied the user), so
/// `None` mirrors that skip.
#[must_use]
pub fn insert_user_libraries_sql(user_id: &str, library_ids: &[i64]) -> Option<Sql> {
    if library_ids.is_empty() {
        return None;
    }
    let mut sql = format!("INSERT INTO {USER_LIBRARY_TABLE} (user_id, library_id) VALUES ");
    let mut args = Vec::new();
    for (index, library_id) in library_ids.iter().enumerate() {
        if index > 0 {
            sql.push_str(", ");
        }
        sql.push_str("(?, ?)");
        args.push(Value::from(user_id));
        args.push(Value::Integer(*library_id));
    }
    Some((sql, args))
}
/// One library of a user's association set, the subset `dbUser.PostScan`
/// (`user_repository.go:36-43`) actually populates.
///
/// Go unmarshals `libraries_json` into `model.Library`, whose json tags are
/// camelCase (`model/library.go:14-35`: `remotePath`, `lastScanAt`,
/// `fullScanInProgress`, …) while `json_object` emits the column names
/// (`user_repository.go:66-81`: `remote_path`, `last_scan_at`,
/// `full_scan_in_progress`, …). Only the keys that agree — `id`, `name`,
/// `path` — reach the model; the rest stay zero in Go (verified against the
/// real dbx v1.12.0 scanner). The port carries exactly those three instead
/// of inventing a richer record; the full library row is the library
/// repository's model half.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct UserLibrary {
    /// `library.id`.
    pub id: i64,
    /// `library.name`.
    pub name: String,
    /// `library.path`.
    pub path: String,
}

/// The hydrated user: the `model.User` fields the row read produces
/// (`model/user.go:12-35`).
///
/// Timestamps stay the stored datetime TEXT — date parsing sits at the
/// `nd-utils`/`nd-model` boundary and the repository stops at the row.
/// `password` is the stored ciphertext: Go scans it the same way and only
/// `FindByUsernameWithPassword` decrypts it (`user_repository.go:209-216`,
/// `:464-472`), so decryption is nd-auth's call, never the repository's.
/// `token_epoch` is the revocation counter nd-auth revalidates
/// (`core/auth/auth.go:85,147`); Go scans it because `user.*` carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRecord {
    /// `user.id`.
    pub id: String,
    /// `user.user_name`.
    pub user_name: String,
    /// `user.name`.
    pub name: String,
    /// `user.email`.
    pub email: String,
    /// `user.is_admin`.
    pub is_admin: bool,
    /// `user.last_login_at` — `None` for NULL (Go's `*time.Time`).
    pub last_login_at: Option<String>,
    /// `user.last_access_at` — `None` for NULL.
    pub last_access_at: Option<String>,
    /// `user.created_at`.
    pub created_at: String,
    /// `user.updated_at`.
    pub updated_at: String,
    /// `user.scrobble_filter`.
    pub scrobble_filter: String,
    /// `user.password` — stored ciphertext, untouched (see struct doc).
    pub password: String,
    /// `user.token_epoch`.
    pub token_epoch: i64,
    /// `user.Libraries`, hydrated from the select's `libraries_json`.
    pub libraries: Vec<UserLibrary>,
}

/// The row the read paths scan: the user columns plus `libraries_json`, still
/// raw — the exact `dbUser` shape (`persistence/user_repository.go:30-34`).
///
/// dbx maps columns to fields by the `db` tag and falls back to
/// `DefaultFieldMapFunc` on the field name (`struct.go:65-71,171-190`);
/// `model.User` has no `db` tags, so `Password` maps to `password` and
/// `TokenEpoch` to `token_epoch`, and both are scanned — `struct.go:167-197`
/// skips only unexported fields and `db:"-"`, neither of which `model.User`
/// has. Confirmed against the real dbx v1.12.0 scanner. Only
/// `NewPassword`/`CurrentPassword` miss: the select has no column named
/// `password` for `NewPassword` to match (that column is scanned by
/// `Password`) and no `current_password` column exists at all, and an
/// unmatched column is scanned into a throwaway `sql.NullString`
/// (`rows.go:80-86`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRow {
    /// The record read from the user columns, `libraries` still empty.
    pub record: UserRecord,
    /// The select's `libraries_json` column, unparsed.
    pub libraries_json: String,
}

impl UserRow {
    /// `dbUser.PostScan` (`user_repository.go:36-43`): parse `libraries_json`
    /// onto the record. A non-empty payload that fails to parse is an error,
    /// because Go returns the `json.Unmarshal` failure out of `ScanStruct`
    /// rather than handing back a half-hydrated user; an empty string yields
    /// no libraries, like Go's guard.
    ///
    /// # Errors
    /// [`UserError::Sqlite`] carrying the JSON failure — Go's scan error
    /// (`parsing user libraries from db`) surfaces the same way, from the read.
    pub fn into_record(self) -> Result<UserRecord, UserError> {
        let mut record = self.record;
        record.libraries = parse_libraries_json(&self.libraries_json).map_err(|err| {
            UserError::Sqlite(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(err),
            ))
        })?;
        Ok(record)
    }
}

/// `PostScan`'s decode (`user_repository.go:37-41`). Empty input is the
/// skip-and-return-nil branch; otherwise the JSON array of the select's
/// `libraries_json`.
///
/// # Errors
/// The `serde_json` failure Go's `json.Unmarshal` would return.
pub fn parse_libraries_json(json: &str) -> Result<Vec<UserLibrary>, serde_json::Error> {
    if json.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(json)
}

/// Failures surfaced by the executed user repository.
#[derive(Debug, thiserror::Error)]
pub enum UserError {
    /// The SQLite statement failed (read or write), including a
    /// `libraries_json` payload that will not parse.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// No row matched — Go's `model.ErrNotFound` (`model/errors.go:10`),
    /// raised by `queryOne`'s `sql.ErrNoRows` mapping
    /// (`sql_base_repository.go:382-385`) and by `deleteByID`'s zero count
    /// (`:650-659`).
    #[error("record not found")]
    NotFound,
}

/// What [`UserRepo::put`] reports: the two facts Go threads through the
/// request context instead of a return value (`user_repository.go:112-186`)
/// — whether the row was created, and the epoch a password change produced
/// for the caller's in-flight token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PutOutcome {
    /// True when the UPDATE matched no row, so the INSERT leg ran
    /// (`isNewUser`, `:133-147`).
    pub is_new_user: bool,
    /// The epoch the password UPDATE's `RETURNING token_epoch` produced;
    /// `None` when the password was untouched (Go leaves `epoch` at zero on
    /// that arm, `:129-144`).
    pub token_epoch: Option<i64>,
    /// Whether the caller's own token must be refreshed with
    /// [`Self::token_epoch`] — the `refresh_inflight_token` condition
    /// (`:180-185`), i.e. Go's `request.SetTokenEpoch` call site.
    pub refresh_inflight_token: bool,
}

/// The executed user repository: `userRepository`
/// (`persistence/user_repository.go:56-68`) bound to one pooled connection.
///
/// Go binds a `dbx.Builder` and reaches the DB through the ambient ctx; the
/// crate has no ambient context (#S1 DI freeze, as in `share_repo.rs`), so the
/// caller threads a lease from
/// [`crate::store::SQLStore::with_connection`] and every statement runs
/// through [`crate::execution`] — the port of
/// `executeSQL`/`queryOne`/`queryAll` (`sql_base_repository.go:336-355,375-388,441-457`).
///
/// Password crypto is deliberately absent: Go encrypts `NewPassword` before
/// rendering (`:453-461`) and decrypts only inside
/// `FindByUsernameWithPassword` (`:209-216`), both nd-auth's
/// `utils.Encrypt`/`Decrypt`. The port takes the already-encrypted payload in
/// [`UserPutValues::password`] and hands reads back the stored ciphertext.
/// Likewise `updated_at` and the INSERT leg's `created_at` are the caller's
/// `time.Now()` stamps.
#[derive(Debug)]
pub struct UserRepo<'a> {
    conn: &'a rusqlite::Connection,
}

impl<'a> UserRepo<'a> {
    /// `NewUserRepository` (`:56-68`): bind to `conn` (the pooled lease).
    #[must_use]
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    /// `Get` (`:90-97`): the libraries select for one id, hydrated.
    ///
    /// # Errors
    /// [`UserError::NotFound`] when no row matches; [`UserError::Sqlite`] on
    /// a statement or hydration failure.
    pub fn get(&self, id: &str) -> Result<UserRecord, UserError> {
        self.read_one(&get_user_sql(id))
    }

    /// `GetAll` (`:99-105`): the libraries select under the caller's
    /// `QueryOptions`. Go's `queryAll` yields an empty slice for no rows — not
    /// `ErrNotFound`, because dbx `All` only errors on a real failure, so
    /// `sql.ErrNoRows` never fires there (`sql_base_repository.go:450-453`);
    /// the empty Vec is the faithful port.
    ///
    /// # Errors
    /// [`UserError::Sqlite`] when the statement or a row's hydration fails.
    pub fn get_all(
        &self,
        order_by: &[&str],
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> Result<Vec<UserRecord>, UserError> {
        let statement = get_all_users_sql(order_by, limit, offset);
        let rows = crate::execution::query_all(self.conn, &statement, row_to_user)?;
        rows.into_iter().map(UserRow::into_record).collect()
    }

    /// `FindByUsername` (`:154-160`): the case-insensitive lookup.
    ///
    /// # Errors
    /// [`UserError::NotFound`] when no row matches — the case
    /// `validateUsernameUnique` treats as success (`:318-324`);
    /// [`UserError::Sqlite`] on a statement or hydration failure.
    pub fn find_by_username(&self, username: &str) -> Result<UserRecord, UserError> {
        self.read_one(&find_by_username_sql(username))
    }

    /// `FindFirstAdmin` (`:146-152`): one admin, the earliest-updated one
    /// (see [`find_first_admin_sql`] for why ascending order picks that row).
    ///
    /// # Errors
    /// [`UserError::NotFound`] when the instance has no admin;
    /// [`UserError::Sqlite`] on a statement or hydration failure.
    pub fn find_first_admin(&self) -> Result<UserRecord, UserError> {
        self.read_one(&find_first_admin_sql())
    }

    /// `CountAll` (`:86-88`): the DISTINCT aggregate, options dropped.
    ///
    /// # Errors
    /// [`UserError::Sqlite`] when the statement fails.
    pub fn count_all(&self) -> Result<i64, UserError> {
        let statement = count_all_sql();
        Ok(
            crate::execution::query_optional(self.conn, &statement, |row| row.get::<_, i64>(0))?
                .unwrap_or_default(),
        )
    }

    /// Count users matching the given username.
    ///
    /// # Errors
    /// Returns an error if the statement fails.
    pub fn count_by_username(&self, user_name: &str) -> Result<i64, UserError> {
        let statement = count_by_username_sql(user_name);
        Ok(
            crate::execution::query_optional(self.conn, &statement, |row| row.get::<_, i64>(0))?
                .unwrap_or_default(),
        )
    }

    /// `Put` (`:106-186`): UPDATE, INSERT fallback when it matched no row,
    /// then the library auto-assigns. `values.password` carries the
    /// already-encrypted payload — its presence is the port's equivalent of
    /// Go's `u.NewPassword != ""` test that picks the epoch-bumping arm.
    /// `created_at` is the INSERT leg's fresh stamp (Go re-stamps
    /// `values["created_at"] = time.Now()`, `:141`); the UPDATE keeps
    /// [`UserPutValues::created_at`].
    ///
    /// An empty `values.id` is filled with a random id like Go's
    /// `id.NewRandom()` (`:108-110`). Go mutates the struct in place so the
    /// caller sees the id; the port takes `&UserPutValues`, so a caller that
    /// saves with no id re-reads the row by `user_name`.
    ///
    /// # Errors
    /// [`UserError::Sqlite`] when any statement fails. Go propagates every
    /// `executeSQL` error and treats only `ErrNotFound` as new-user.
    pub fn put(
        &self,
        values: &UserPutValues<'_>,
        created_at: &str,
        caller_is_user: bool,
    ) -> Result<PutOutcome, UserError> {
        // Arrange: Go generates the id in place (`:108-110`). Declared first
        // so it outlives the scoped copy that borrows it.
        let generated: Option<String> = (values.id.is_empty()).then(nd_id::new_random);
        let scoped = match &generated {
            Some(id) => UserPutValues {
                id: id.as_str(),
                ..values.clone()
            },
            None => values.clone(),
        };
        let password_changed = scoped.password.is_some();
        // Act: the two UPDATE arms (`:122-144`). The password arm takes the
        // bumped epoch out of the same statement, and no row (ErrNotFound)
        // means new user (`:135-140`).
        let mut epoch: Option<i64> = None;
        let is_new_user = if password_changed {
            epoch = crate::execution::query_optional(
                self.conn,
                &put_password_update_sql(&scoped),
                |row| row.get::<_, i64>(0),
            )?;
            epoch.is_none()
        } else {
            crate::execution::execute(self.conn, &put_update_sql(&scoped))? == 0
        };
        // Act: the INSERT fallback with the fresh created_at (`:140-147`).
        if is_new_user {
            crate::execution::execute(self.conn, &put_insert_sql(&scoped, created_at))?;
        }
        // Act: the library auto-assigns (`:149-165`). Admins get every library
        // on every Put; a new regular user gets the default-new-users set.
        if scoped.is_admin {
            crate::execution::execute(self.conn, &assign_all_libraries_sql(scoped.id))?;
        } else if is_new_user {
            crate::execution::execute(self.conn, &assign_default_libraries_sql(scoped.id))?;
        }
        // Assert: the in-flight refresh condition (`:180-185`).
        Ok(PutOutcome {
            is_new_user,
            token_epoch: epoch,
            refresh_inflight_token: refresh_inflight_token(
                password_changed,
                is_new_user,
                caller_is_user,
            ),
        })
    }

    /// `UpdateLastLoginAt` (`:167-170`). Go ignores the affected-row count, so
    /// an unknown id is a no-op, not `ErrNotFound`. `now` is the caller's
    /// `time.Now()` stamp.
    ///
    /// # Errors
    /// [`UserError::Sqlite`] when the statement fails.
    pub fn update_last_login_at(&self, id: &str, now: &str) -> Result<(), UserError> {
        crate::execution::execute(self.conn, &update_last_login_at_sql(id, now))?;
        Ok(())
    }

    /// `UpdateLastAccessAt` (`:172-176`), same no-op-on-miss semantics.
    ///
    /// # Errors
    /// [`UserError::Sqlite`] when the statement fails.
    pub fn update_last_access_at(&self, id: &str, now: &str) -> Result<(), UserError> {
        crate::execution::execute(self.conn, &update_last_access_at_sql(id, now))?;
        Ok(())
    }

    /// `Delete` (`:320-327`) through `deleteByID`
    /// (`sql_base_repository.go:650-659`): a zero-row delete is
    /// `model.ErrNotFound`. Go's `cleanupPluginUserReferences` is the plugin
    /// slice and does not run here.
    ///
    /// # Errors
    /// [`UserError::NotFound`] when the id matches no row;
    /// [`UserError::Sqlite`] when the statement fails.
    pub fn delete(&self, id: &str) -> Result<(), UserError> {
        if crate::execution::execute(self.conn, &delete_user_sql(id))? == 0 {
            return Err(UserError::NotFound);
        }
        Ok(())
    }

    /// `SetUserLibraries` (`:497-512`): the DELETE then the multi-row INSERT,
    /// with Go's empty-set skip. Go runs the two statements without a
    /// transaction, so the port does too; a caller that needs them atomic
    /// wraps this in [`crate::store::SQLStore::with_tx`].
    ///
    /// # Errors
    /// [`UserError::Sqlite`] when either statement fails.
    pub fn set_user_libraries(&self, user_id: &str, library_ids: &[i64]) -> Result<(), UserError> {
        crate::execution::execute(self.conn, &clear_user_libraries_sql(user_id))?;
        if let Some(statement) = insert_user_libraries_sql(user_id, library_ids) {
            crate::execution::execute(self.conn, &statement)?;
        }
        Ok(())
    }

    /// `queryOne` over the libraries select (`sql_base_repository.go:375-388`):
    /// one row, hydrated, with `sql.ErrNoRows` mapped to
    /// [`UserError::NotFound`] — and a `PostScan` failure surfacing as a scan
    /// error exactly as it does in Go.
    fn read_one(&self, statement: &Sql) -> Result<UserRecord, UserError> {
        let row = crate::execution::query_optional(self.conn, statement, row_to_user)?
            .ok_or(UserError::NotFound)?;
        row.into_record()
    }
}

/// `dbUser` row mapping: the user columns dbx scans plus
/// `libraries_json` (`persistence/user_repository.go:30-34`). Unmatched
/// columns are Go's throwaway `sql.NullString`, so a raw row inserted without
/// `scrobble_filter` still reads (`persistence/user_repository_test.go:87-99`
/// pins that case; the column's DEFAULT supplies the value here).
fn row_to_user(row: &rusqlite::Row<'_>) -> rusqlite::Result<UserRow> {
    Ok(UserRow {
        record: UserRecord {
            id: row.get("id")?,
            user_name: row.get("user_name")?,
            name: row.get("name")?,
            email: row.get("email")?,
            is_admin: row.get("is_admin")?,
            last_login_at: row.get("last_login_at")?,
            last_access_at: row.get("last_access_at")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
            scrobble_filter: row.get("scrobble_filter")?,
            password: row.get("password")?,
            token_epoch: row.get("token_epoch")?,
            libraries: Vec::new(),
        },
        libraries_json: row.get("libraries_json")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER_ID: &str = "user-1";
    const USERNAME: &str = "jdoe";
    const NOW: &str = "2024-01-02T03:04:05Z";

    fn user_values(password: &'static str) -> UserPutValues<'static> {
        UserPutValues {
            id: USER_ID,
            user_name: USERNAME,
            name: "John Doe",
            email: "jdoe@example.com",
            password: if password.is_empty() {
                None
            } else {
                Some(password)
            },
            is_admin: false,
            last_login_at: None,
            last_access_at: None,
            created_at: NOW,
            updated_at: NOW,
            scrobble_filter: "",
        }
    }

    #[test]
    fn select_user_with_libraries_left_joins_groups_and_coalesces() {
        // Arrange: selectUserWithLibraries' base shape
        // (user_repository.go:62-83): user.* + libraries_json, both LEFT
        // JOINs, GROUP BY user.id, and a WHERE/ORDER/LIMIT/OFFSET stack.
        // Act:
        let (sql, args) = select_user_with_libraries(
            Some("user.id = ?"),
            vec![Value::from(USER_ID)],
            &["updated_at"],
            Some(1),
            Some(5),
        );
        // Assert:
        assert_eq!(
            sql,
            "SELECT user.*, COALESCE(json_group_array(json_object(\
             'id', library.id, \
             'name', library.name, \
             'path', library.path, \
             'remote_path', library.remote_path, \
             'last_scan_at', library.last_scan_at, \
             'last_scan_started_at', library.last_scan_started_at, \
             'full_scan_in_progress', library.full_scan_in_progress, \
             'updated_at', library.updated_at, \
             'created_at', library.created_at\
             )) FILTER (WHERE library.id IS NOT NULL), '[]') AS libraries_json \
             FROM user LEFT JOIN user_library ul ON user.id = ul.user_id \
             LEFT JOIN library ON ul.library_id = library.id \
             WHERE user.id = ? GROUP BY user.id ORDER BY updated_at LIMIT 1 OFFSET 5"
        );
        assert_eq!(args, vec![Value::Text(USER_ID.to_string())]);
    }

    #[test]
    fn get_getall_and_count_pin_their_shapes() {
        // Arrange: Get (user_repository.go:90-97), GetAll (:99-105) and
        // CountAll (:86-88, sql_base_repository.go:580-590).
        // Act:
        let (get_sql, get_args) = get_user_sql(USER_ID);
        let (all_sql, all_args) = get_all_users_sql(&["name asc"], Some(10), None);
        let (count_sql, count_args) = count_all_sql();
        // Assert:
        assert!(get_sql.starts_with("SELECT user.*, COALESCE(json_group_array"));
        assert!(get_sql.contains(" WHERE user.id = ? GROUP BY user.id"));
        assert_eq!(get_args, vec![Value::Text(USER_ID.to_string())]);
        assert!(all_sql.ends_with(" GROUP BY user.id ORDER BY name asc LIMIT 10"));
        // Assert: squirrel renders the bound as a literal, contributing no arg.
        assert_eq!(all_args, Vec::<Value>::new());
        assert_eq!(
            count_sql,
            "SELECT count(distinct user.id) as count FROM user"
        );
        assert!(count_args.is_empty());
    }

    #[test]
    fn find_first_admin_orders_by_updated_at_and_binds_bool() {
        // Arrange: FindFirstAdmin (user_repository.go:146-152):
        // QueryOptions{Sort: "updated_at", Max: 1} + Eq{"user.is_admin": true}.
        // Act:
        let (sql, args) = find_first_admin_sql();
        // Assert:
        assert!(
            sql.contains(" WHERE user.is_admin = ? GROUP BY user.id ORDER BY updated_at LIMIT 1")
        );
        assert_eq!(args, vec![Value::Bool(true)]);
    }

    #[test]
    fn offset_without_limit_renders_the_unlimited_bound() {
        // Arrange: squirrel emits a bare `OFFSET n` when Max is unset
        // (select.go:144-147), which SQLite rejects; the port renders the
        // unlimited bound instead (cited deviation on
        // `select_user_with_libraries`).
        // Act:
        let (sql, args) = get_all_users_sql(&["name asc"], None, Some(20));
        // Assert:
        assert!(sql.ends_with(" GROUP BY user.id ORDER BY name asc LIMIT -1 OFFSET 20"));
        // Assert: the bounds are literals, so nothing is bound.
        assert!(args.is_empty());
    }

    #[test]
    fn find_by_username_collates_nocase() {
        // Arrange: FindByUsername (user_repository.go:154-160) — the mock's
        // case-insensitive keying (tests/mock_user_repo.go:59) is this
        // repository's `COLLATE NOCASE` fragment.
        // Act:
        let (sql, args) = find_by_username_sql("JDoe");
        // Assert:
        assert!(sql.contains(" WHERE user.user_name = ? COLLATE NOCASE"));
        assert_eq!(args, vec![Value::Text("JDoe".to_string())]);
    }

    #[test]
    fn put_update_sets_all_row_columns_and_binds_id_last() {
        // Arrange: Put's password-less UPDATE (user_repository.go:122-128):
        // SetMap over the full row (id included, squirrel sets it too) and
        // no `password` column when NewPassword is empty.
        // Act:
        let (sql, args) = put_update_sql(&user_values(""));
        // Assert:
        assert_eq!(
            sql,
            "UPDATE user SET id = ?, user_name = ?, name = ?, email = ?, is_admin = ?, \
             last_login_at = ?, last_access_at = ?, created_at = ?, updated_at = ?, \
             scrobble_filter = ? WHERE id = ?"
        );
        assert_eq!(args.len(), 11);
        assert_eq!(args[10], Value::Text(USER_ID.to_string()));
        // Assert: unset *time.Time columns bind NULL.
        assert_eq!(args[5], Value::Null);
        assert_eq!(args[6], Value::Null);
    }

    #[test]
    fn put_password_update_bumps_token_epoch_in_the_same_statement() {
        // Arrange: THE epoch revocation (user_repository.go:129-144): the
        // password UPDATE also bumps token_epoch and RETURNs the new epoch —
        // one statement, so no concurrent change can slip between the
        // password write and the epoch bump. nd-auth #13 consumes this.
        // Act:
        let (sql, args) = put_password_update_sql(&user_values("enc-secret"));
        // Assert:
        assert_eq!(
            sql,
            "UPDATE user SET id = ?, user_name = ?, name = ?, email = ?, password = ?, \
             is_admin = ?, last_login_at = ?, last_access_at = ?, created_at = ?, \
             updated_at = ?, scrobble_filter = ?, token_epoch = token_epoch + 1 \
             WHERE id = ? RETURNING token_epoch"
        );
        // Assert: password binds at its struct slot and the WHERE id last.
        assert_eq!(args[4], Value::Text("enc-secret".to_string()));
        assert_eq!(args.last(), Some(&Value::Text(USER_ID.to_string())));
        // Assert: the bump and the RETURNING ride the same statement.
        assert!(sql.contains("token_epoch = token_epoch + 1"));
        assert!(sql.ends_with("RETURNING token_epoch"));
    }

    #[test]
    fn refresh_inflight_token_only_for_own_non_new_password_change() {
        // Arrange: the refresh condition (user_repository.go:160-163) — the
        // mock's Put-password flow (tests/mock_user_repo.go:44-48) is the
        // own-change case.
        // Act / Assert:
        assert!(refresh_inflight_token(true, false, true));
        // Assert: an admin resetting another user keeps their own epoch.
        assert!(!refresh_inflight_token(true, false, false));
        // Assert: a brand-new user has no in-flight token to refresh.
        assert!(!refresh_inflight_token(true, true, true));
        // Assert: no password change, no epoch returned at all.
        assert!(!refresh_inflight_token(false, false, true));
    }

    #[test]
    fn put_insert_stamps_fresh_created_at_and_binds_password() {
        // Arrange: the isNewUser INSERT fallback (user_repository.go:140-147):
        // created_at is the fresh `time.Now()` stamp, not the struct's value.
        // Act:
        let (sql, args) = put_insert_sql(&user_values("enc-secret"), "2025-06-07T08:09:10Z");
        // Assert:
        assert_eq!(
            sql,
            "INSERT INTO user (id, user_name, name, email, password, is_admin, \
             last_login_at, last_access_at, created_at, updated_at, scrobble_filter) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        );
        assert_eq!(args[8], Value::Text("2025-06-07T08:09:10Z".to_string()));
        assert_eq!(args[4], Value::Text("enc-secret".to_string()));
    }

    #[test]
    fn library_assigns_split_admin_all_from_new_user_defaults() {
        // Arrange: Put's library auto-assigns (user_repository.go:149-165).
        // Act:
        let (admin_sql, admin_args) = assign_all_libraries_sql(USER_ID);
        let (default_sql, default_args) = assign_default_libraries_sql(USER_ID);
        // Assert: admins get every library, INSERT OR IGNORE keeps re-Puts idempotent.
        assert_eq!(
            admin_sql,
            "INSERT OR IGNORE INTO user_library (user_id, library_id) SELECT ?, id FROM library"
        );
        assert_eq!(admin_args, vec![Value::Text(USER_ID.to_string())]);
        // Assert: new regular users get only the default_new_users set.
        assert_eq!(
            default_sql,
            "INSERT OR IGNORE INTO user_library (user_id, library_id) SELECT ?, id FROM library WHERE default_new_users = true"
        );
        assert_eq!(default_args, vec![Value::Text(USER_ID.to_string())]);
    }

    #[test]
    fn login_access_and_delete_statements_pin_their_single_column_shapes() {
        // Arrange: UpdateLastLoginAt/UpdateLastAccessAt (user_repository.go:
        // 167-176) and Delete's deleteByID (:329-332, the mock's deleteOne
        // walks ids the same way, tests/mock_user_repo.go:159-166).
        // Act:
        let (login_sql, login_args) = update_last_login_at_sql(USER_ID, NOW);
        let (access_sql, access_args) = update_last_access_at_sql(USER_ID, NOW);
        let (delete_sql, delete_args) = delete_user_sql(USER_ID);
        // Assert:
        assert_eq!(login_sql, "UPDATE user SET last_login_at = ? WHERE id = ?");
        assert_eq!(
            access_sql,
            "UPDATE user SET last_access_at = ? WHERE id = ?"
        );
        assert_eq!(delete_sql, "DELETE FROM user WHERE id = ?");
        assert_eq!(
            login_args,
            vec![
                Value::Text(NOW.to_string()),
                Value::Text(USER_ID.to_string())
            ]
        );
        assert_eq!(
            access_args,
            vec![
                Value::Text(NOW.to_string()),
                Value::Text(USER_ID.to_string())
            ]
        );
        assert_eq!(delete_args, vec![Value::Text(USER_ID.to_string())]);
    }

    #[test]
    fn get_user_libraries_inner_joins_and_orders_by_name() {
        // Arrange: GetUserLibraries (user_repository.go:438-448) — Go's
        // `.Join` is INNER, so an unassociated user yields no rows.
        // Act:
        let (sql, args) = get_user_libraries_sql(USER_ID);
        // Assert:
        assert_eq!(
            sql,
            "SELECT l.* FROM library l INNER JOIN user_library ul ON l.id = ul.library_id \
             WHERE ul.user_id = ? ORDER BY l.name"
        );
        assert_eq!(args, vec![Value::Text(USER_ID.to_string())]);
    }

    #[test]
    fn set_user_libraries_clears_then_bulk_inserts() {
        // Arrange: SetUserLibraries' two legs (user_repository.go:451-462):
        // the DELETE, the multi-row INSERT, and the empty-set skip.
        // Act:
        let (clear_sql, clear_args) = clear_user_libraries_sql(USER_ID);
        let insert = insert_user_libraries_sql(USER_ID, &[1, 2, 3]);
        let empty = insert_user_libraries_sql(USER_ID, &[]);
        // Assert:
        assert_eq!(clear_sql, "DELETE FROM user_library WHERE user_id = ?");
        assert_eq!(clear_args, vec![Value::Text(USER_ID.to_string())]);
        let (insert_sql, insert_args) = insert.expect("a non-empty set renders the INSERT");
        assert_eq!(
            insert_sql,
            "INSERT INTO user_library (user_id, library_id) VALUES (?, ?), (?, ?), (?, ?)"
        );
        assert_eq!(
            insert_args,
            vec![
                Value::Text(USER_ID.to_string()),
                Value::Integer(1),
                Value::Text(USER_ID.to_string()),
                Value::Integer(2),
                Value::Text(USER_ID.to_string()),
                Value::Integer(3),
            ]
        );
        // Assert: an empty set is the DELETE-only path.
        assert!(empty.is_none());
    }
}
