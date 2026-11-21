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
//! DB-13 adds the epoch-only sibling [`revoke_tokens_in_tx`]: Go has no
//! epoch-only operation (the bump only rides the password UPDATE), so a
//! no-password `Put` cannot revoke anything and a password-carrying `Put`
//! would rewrite unrelated columns and auto-assign libraries. This is an
//! approved Rust API addition, not claimed Go endpoint parity
//! (`docs/rust-port/subplans/db-runtime.md` DB-13 point 8).
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
//! `cleanupPluginUserReferences` (`:330-337`, NOT ported — `D-PLUGIN`), and
//! `GetUserLibraries`'s full library-row hydration (`:485-495`, which reads
//! every `library` column — the library repository's model half).

use crate::runtime_error::DbOperationError;
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
/// The DB-13 epoch-only revocation statement: one guarded atomic increment
/// that touches NOTHING else — no `updated_at`, no `last_access_at`, no
/// `password`, no `is_admin`, and no `user_library` leg (compare
/// [`put_password_update_sql`], whose `Put` caller then auto-assigns
/// libraries at `user_repository.go:162-177`).
///
/// `token_epoch` is bumped by SQLite itself (`token_epoch + 1`), never by a
/// Rust read-modify-write, so concurrent revocations cannot lose an update.
/// BOTH predicates are load-bearing and BOTH run BEFORE any mutation.
///
/// `AND typeof(token_epoch) = 'integer'` rejects a cell whose storage class is
/// not INTEGER. Without it SQLite performs the arithmetic — and INTEGER-affinity
/// conversion — first, so a malformed REAL can become a valid INTEGER and the
/// strict `row.get::<_, i64>` decode reports a SUCCESSFUL revocation for a row
/// that never held an integer epoch. Executed on THIS crate's bundled SQLite
/// (rusqlite 0.32 `bundled`, `rusqlite::version()` = 3.46.0) with the fresh DDL
/// and no type guard: a cell holding `4503599627370495.5` (`typeof` = `'real'`)
/// is rewritten to `4503599627370496` / `'integer'` — rounding across the f64
/// precision boundary, not an i64 overflow — and returned as
/// `Ok(NewTokenEpoch(4503599627370496))`, which a committing caller would
/// publish; a cell holding `1.5` is rewritten to `2.5` before the decode fails.
/// A valid result decode cannot certify the ORIGINAL storage class, so the type
/// check has to be part of the predicate. Malformed rows are reported, never
/// cast, repaired, or classified as a domain miss. An INTEGRAL REAL is refused
/// too: on the fresh DDL INTEGER affinity stores `3.0` as an INTEGER cell, so only
/// a schema that declares `token_epoch` REAL (a damaged or hand-edited DB) can hold
/// one — executed on such a schema, the primitive reports the concrete decode
/// failure and leaves the cell untouched.
///
/// `AND token_epoch < 9223372036854775807` is the upper bound: on the same
/// build, an unguarded `token_epoch + 1` on a row holding `i64::MAX` SUCCEEDS
/// and promotes the cell to REAL (`typeof` = `'real'`, value
/// `9.223372036854776e+18`), corrupting the `token_epoch INTEGER` column
/// declared at `nd-dbschema/src/schema_ddl.rs:57` and handing back a value no
/// `i64` can hold. Either guard turning the statement into a no-row result is
/// what [`revoke_tokens_in_tx`] classifies — as typed `Exhausted` for an INTEGER
/// cell at the ceiling — BEFORE any mutation runs: the cell is never saturated,
/// wrapped, reset, coerced, or promoted.
///
/// The bound is the exact `i64::MAX` decimal literal — SQLite parses it as
/// INTEGER, while a literal above it would become REAL and quietly change
/// the comparison. `id` is the statement's only bound value.
#[must_use]
pub fn increment_token_epoch_sql(id: &str) -> Sql {
    (
        format!(
            "UPDATE {USER_TABLE} SET token_epoch = token_epoch + 1 \
             WHERE id = ? AND typeof(token_epoch) = 'integer' \
             AND token_epoch < {max} RETURNING token_epoch",
            max = i64::MAX
        ),
        vec![Value::from(id)],
    )
}

/// The existence read [`revoke_tokens_in_tx`] uses to classify a no-row
/// increment: a bare single-row `SELECT` on the primary key with no JOIN and
/// no `GROUP BY`, so it cannot multiply rows and does not pay for the
/// `libraries_json` hydration the read paths use.
///
/// Deliberately carries NEITHER of the increment's guards and applies no CAST:
/// it reports what the row actually holds, which is exactly why a malformed
/// cell fails the caller's strict `i64` decode and stays a concrete SQLite error
/// instead of being inferred to be an exhausted counter or a missing user.
#[must_use]
pub fn select_token_epoch_sql(id: &str) -> Sql {
    (
        format!("SELECT token_epoch FROM {USER_TABLE} WHERE id = ?"),
        vec![Value::from(id)],
    )
}

/// The epoch [`revoke_tokens_in_tx`] produced: a newtype over `i64` rather
/// than a bare integer, so a caller cannot confuse an epoch read with a
/// revocation result and the ADVISORY-until-commit contract below has one
/// obvious carrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NewTokenEpoch(i64);

impl NewTokenEpoch {
    /// The post-bump `user.token_epoch` the guarded statement RETURNed.
    ///
    /// This value is ADVISORY until the caller's transaction commits: the
    /// primitive owns no commit, so an epoch carried out of a rolled-back or
    /// failed transaction was never a stored epoch.
    #[must_use]
    pub fn value(self) -> i64 {
        self.0
    }
}

impl std::fmt::Display for NewTokenEpoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Domain failures of [`revoke_tokens_in_tx`].
///
/// The two no-row outcomes of the guarded increment are deliberately
/// DISTINCT variants: conflating them would make a user whose counter has
/// reached the i64 ceiling indistinguishable from a user who does not exist,
/// and the service/API layer (`BOOT.epoch-revoke` / `API-03`) decides
/// separately what each means to a request. This primitive reports neither
/// an HTTP status nor an authorization verdict.
#[derive(Debug, thiserror::Error)]
pub enum RevokeError {
    /// No `user` row carries `id`, so nothing was revoked and nothing was
    /// inserted. Classified by the existence read in the caller's own
    /// transaction, so a rolled-back revoke publishes nothing.
    #[error("record not found: user {id}")]
    NotFound {
        /// The id the caller asked to revoke.
        id: String,
    },

    /// The row exists, holds an INTEGER `token_epoch`, and that integer is
    /// `i64::MAX`, so the upper-bound guard refused the increment and the cell
    /// remains an intact INTEGER. No revocation happened, so no token was
    /// invalidated. A non-INTEGER cell is never reported here — it fails the
    /// classification decode and stays a SQLite error.
    #[error("token_epoch for user {id} is at the i64 maximum and cannot be incremented")]
    Exhausted {
        /// The id whose counter is spent.
        id: String,
    },
}

/// DB-13: revoke every token of one user by bumping its epoch, EXECUTED on
/// the CALLER's transaction.
///
/// This is the epoch-only counterpart of the password bump
/// ([`put_password_update_sql`]): it runs [`increment_token_epoch_sql`], a
/// single guarded statement, and hands back the new epoch. It reuses no part
/// of `Put` — a no-password [`UserRepo::put`] leaves `token_epoch` untouched,
/// and a password-carrying `Put` rewrites every other column and
/// auto-assigns libraries (`user_repository.go:149-165`).
///
/// Transaction-only by design: stage assignment (`Body`) and busy retry
/// belong to the DB-00 executor, so there is NO connection-bound autocommit
/// variant, and this never begins, commits, retries, nests a checkout, mints
/// a token, touches a cache, or refreshes a `TokenEpochHolder`. Whether the
/// caller's own session dies is the revoke route's decision, not this
/// primitive's; authorization is trusted caller/API
/// (`db-runtime.md` DB-13 point 4).
/// A pooled lease cannot drive it either: a `&Connection` is not a
/// `Transaction`, which the signature pins structurally.
/// ```compile_fail
/// let (_pool, first) = nd_db::pool::Pool::new(&nd_db::pool::PoolConfig::with_size(
///     "file::memory:",
///     1,
/// ))
/// .unwrap();
/// let conn: &rusqlite::Connection = &first;
/// let _ = nd_db::user_repo::revoke_tokens_in_tx(conn, "u1");
/// ```
///
/// # Errors
/// [`DbOperationError::Domain`]·[`RevokeError::NotFound`] when no row
/// matches `id` (nothing is inserted) and [`RevokeError::Exhausted`] when the
/// row exists and holds an INTEGER `token_epoch` at `i64::MAX` — the two
/// no-row outcomes separated by [`select_token_epoch_sql`] IN THE SAME
/// TRANSACTION, so neither result leaks past a rollback. Every other failure
/// stays on the
/// [`DbOperationError::Sqlite`] arm carrying the concrete `rusqlite::Error`
/// with extended result codes intact (`SQLITE_BUSY` stays visible for the
/// executor's retry classifier) and is NEVER mistaken for a domain miss: a
/// SQL/prepare failure and a `token_epoch` cell that is not an INTEGER — REAL,
/// TEXT, or BLOB — both surface as SQLite errors rather than `NotFound` or
/// `Exhausted`.
///
/// A non-INTEGER cell is rejected by the increment's `typeof` predicate, so
/// there is no mutation to unwind: the row is left exactly as found and the
/// caller may commit its transaction without publishing a revocation. The
/// strict `i64` decode of the classification read — with no CAST and no
/// coercion — is what keeps malformed data on the SQLite arm.
///
/// The returned [`NewTokenEpoch`] is ADVISORY until the caller commits.
pub fn revoke_tokens_in_tx(
    tx: &rusqlite::Transaction<'_>,
    user_id: &str,
) -> Result<NewTokenEpoch, DbOperationError<RevokeError>> {
    // Step 1: the guarded atomic increment. `query_optional` mirrors Go's
    // `queryOne` `sql.ErrNoRows` mapping (`sql_base_repository.go:375-388`);
    // no row here means either the user is absent or the guard refused,
    // which step 2 separates.
    let bumped = crate::execution::query_optional(tx, &increment_token_epoch_sql(user_id), |row| {
        row.get::<_, i64>(0)
    })
    .map_err(DbOperationError::sqlite)?;
    if let Some(epoch) = bumped {
        return Ok(NewTokenEpoch(epoch));
    }

    // Step 2: classify the miss inside the caller's transaction. For an INTEGER
    // cell the two guards are the only reasons for no row, and the upper bound
    // can only be reached at exactly `i64::MAX`, so a present INTEGER row here
    // means an exhausted counter. A cell that is not an INTEGER fails the strict
    // decode below and stays a SQLite error: damaged data is reported, never
    // inferred to be a missing user or an exhausted counter.
    let stored = crate::execution::query_optional(tx, &select_token_epoch_sql(user_id), |row| {
        row.get::<_, i64>(0)
    })
    .map_err(DbOperationError::sqlite)?;
    let id = user_id.to_string();
    match stored {
        None => Err(DbOperationError::Domain(RevokeError::NotFound { id })),
        Some(_epoch) => Err(DbOperationError::Domain(RevokeError::Exhausted { id })),
    }
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
/// `cleanupPluginUserReferences` is NOT ported (`D-PLUGIN`: the `plugin` table is
/// dropped, so a user delete has no plugin-cleanup step).
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
// Go's `encoding/json` never errors on an absent field: the object decoder only
// touches keys it finds (`decode.go:700-743`), so a payload with just `id`
// unmarshals to `{ID:1, Name:"", Path:""}` with `err = nil` (executed), and a
// `null` literal leaves a struct/primitive element untouched
// (`decode.go:897-909`), so `[null]` yields one zero-valued element with
// `err = nil` (executed, over the array loop at `decode.go:542-567`). serde's
// derive makes absent fields an error, hence `#[serde(default)]` here; the null
// element is mapped by `crate::hydrate::parse_libraries_json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct UserLibrary {
    /// `library.id`.
    #[serde(default)]
    pub id: i64,
    /// `library.name`.
    #[serde(default)]
    pub name: String,
    /// `library.path`.
    #[serde(default)]
    pub path: String,
}

/// The hydrated user: the `model.User` fields the row read produces
/// (`model/user.go:12-35`).
///
/// Timestamps stay the stored datetime TEXT — date parsing sits at the
/// `nd-utils`/`nd-model` boundary and the repository stops at the row. This is a
/// documented divergence from Go, which does NOT hand those columns verbatim:
/// `go-sqlite3` converts a `datetime`-declared cell to a `time.Time`
/// (`sqlite3.go:2822-2838`), and `database/sql`'s `case time.Time:` arm renders
/// a `*string` destination with `s.Format(time.RFC3339Nano)`
/// (`convert.go:323-329`) — `asString` is not reached for that destination — so
/// Go's text for such a column carries `2024-01-02T03:04:05Z` where this record
/// carries the stored `2024-01-02 03:04:05+00:00`, an epoch integer becomes
/// `2024-02-06T19:00:00Z`, and unparseable text becomes Go's zero time
/// `0001-01-01T00:00:00Z` (all verified against the pinned driver; see
/// `hydrate-contract.md`'s residual list). Nothing marshals these fields today:
/// the only consumer is nd-auth's login handler, whose serialized `LoginPayload`
/// (`login.rs:67-75`, emitted at `:155-162`) carries
/// `id`/`name`/`username`/`is_admin`/`avatar`/`token` and no timestamp at all, so
/// the divergence is inert. A user-facing DTO must instead read such a column
/// through `RowGet::get_gotime` and render it with `GoTime::to_rfc3339_nano`,
/// which reproduces Go's `FormatTime(RFC3339Nano)` including the zone the driver
/// parsed. Fixing that here would change these fields' types, so it belongs to
/// the lane that adds a user DTO.
///
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
        // Thin delegation onto the S-D3 seam: the same decode, the same Go
        // prefix, surfaced the same way Go's ScanStruct error does — a
        // sqlite conversion failure out of the read.
        crate::hydrate::user_post_scan(self).map_err(|err| UserError::Sqlite(err.into_sqlite()))
    }
}

/// `PostScan`'s decode (`user_repository.go:37-41`): delegates to
/// [`crate::hydrate::parse_libraries_json`], the shared S-D3 seam. Empty
/// input is the skip-and-return-nil branch; otherwise the JSON array of the
/// select's `libraries_json`.
///
/// # Errors
/// The `serde_json` failure Go's `json.Unmarshal` would return.
pub fn parse_libraries_json(json: &str) -> Result<Vec<UserLibrary>, serde_json::Error> {
    crate::hydrate::parse_libraries_json(json)
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
    /// `model.ErrNotFound`. Go's `cleanupPluginUserReferences` is NOT ported
    /// (`D-PLUGIN`) and does not run here.
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
    // Re-pointed at the S-D3 seam: the same reads, through the shared
    // RowGet accessors (identical behaviour, one seam for the eight hooks).
    crate::hydrate::row_to_user(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER_ID: &str = "user-1";
    const USERNAME: &str = "jdoe";
    const NOW: &str = "2024-01-02 03:04:05+00:00";

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
        let (sql, args) = put_insert_sql(&user_values("enc-secret"), "2025-06-07 08:09:10+00:00");
        // Assert:
        assert_eq!(
            sql,
            "INSERT INTO user (id, user_name, name, email, password, is_admin, \
             last_login_at, last_access_at, created_at, updated_at, scrobble_filter) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        );
        assert_eq!(
            args[8],
            Value::Text("2025-06-07 08:09:10+00:00".to_string())
        );
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
