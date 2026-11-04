//! Port of `persistence/user_repository.go` (518L) — the user repository's
//! SQL shaping only.
//!
//! Every Go method either builds a squirrel `SelectBuilder` and materializes
//! it through `toSQL()` (`sql_base_repository.go:358-372`) or hands an
//! `Update`/`Insert`/`Delete` statement to `executeSQL`; execution, `WithTx`,
//! password encryption and the `rest` permission checks stay with
//! `store.rs`/`nd-auth`/later slices, so the ports here stop at the same
//! `(sql, args)` pairs. Where the [`Repository`] builder can render the
//! statement (plain `SELECT`s with INNER JOIN/`Eq`) it is used;
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
//! Not ported (different slices): `dbUser`/`PostScan` JSON hydration (row
//! mapping, nd-model), `encryptPassword`/`decryptPassword` and
//! `initPasswordEncryptionKey` (`:392-518`, nd-auth/nd-utils), the `rest`
//! permission wrappers `Count`/`Read`/`ReadAll`/`Save`/`Update`
//! (`:146-252`, nd-api), `validatePasswordChange`/`validateUsernameUnique`/
//! `validateScrobbleFilter` (`:254-318`, nd-api), and
//! `cleanupPluginUserReferences` (`:330-337`, plugin slice).

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
/// of `model.QueryOptions`, rendered as bound placeholders like squirrel.
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
         LEFT JOIN library ON ul.library_id = library.id \
         GROUP BY user.id",
        libraries_json_column()
    );
    let mut args = where_args;
    if let Some(clause) = where_sql {
        sql.push_str(&format!(" WHERE {clause}"));
    }
    if !order_by.is_empty() {
        sql.push_str(&format!(" ORDER BY {}", order_by.join(", ")));
    }
    if let Some(max) = limit {
        sql.push_str(" LIMIT ?");
        args.push(Value::Integer(i64::try_from(max).unwrap_or(i64::MAX)));
    }
    if let Some(off) = offset {
        sql.push_str(" OFFSET ?");
        args.push(Value::Integer(i64::try_from(off).unwrap_or(i64::MAX)));
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

/// `FindFirstAdmin` (`user_repository.go:146-152`): the most recently updated
/// admin — `QueryOptions{Sort: "updated_at", Max: 1}` plus
/// `Eq{"user.is_admin": true}`.
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
             GROUP BY user.id WHERE user.id = ? ORDER BY updated_at LIMIT ? OFFSET ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Text(USER_ID.to_string()),
                Value::Integer(1),
                Value::Integer(5),
            ]
        );
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
        assert!(get_sql.contains(" GROUP BY user.id WHERE user.id = ?"));
        assert_eq!(get_args, vec![Value::Text(USER_ID.to_string())]);
        assert!(all_sql.ends_with(" GROUP BY user.id ORDER BY name asc LIMIT ?"));
        // Assert: squirrel renders the LIMIT as a bound placeholder.
        assert_eq!(all_args, vec![Value::Integer(10)]);
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
        assert!(sql.contains(" WHERE user.is_admin = ? ORDER BY updated_at LIMIT ?"));
        assert_eq!(args, vec![Value::Bool(true), Value::Integer(1)]);
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
