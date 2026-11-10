//! Port of `persistence/transcoding_repository.go` (110L) — the transcoding
//! configuration repository.
//!
//! Every Go method builds a squirrel statement and hands it to
//! `queryOne`/`queryAll`/`put`/`deleteByID`, which materialize through
//! `toSQL()` (`sql_base_repository.go:358-372`) as `(sql, args)`; execution and
//! logging stay with `store.rs`/the caller, so the port stops at the same
//! `(sql, args)` pairs (see [`Sql`]). The `loggedUser(ctx).IsAdmin` guards
//! (`rest.ErrPermissionDenied`) become [`is_admin`] parameters on the mutating
//! methods, mirroring how sibling slices pass the auth context in.
//!
//! Ported here: `Get`/`FindByFormat`'s Eq selects (`:23-24`, `:39-40`),
//! `CountAll`/`Count` via the base `count()` shape
//! (`sql_base_repository.go:566-576`), `Read`/`ReadAll`'s admin-only `command`
//! masking (`:55-59`, `:72-79`), `Put`/`Save`/`Update` via the base `put()`
//! upsert (`sql_base_repository.go:613-635`) and `Delete` via `deleteByID`
//! (`:649-660`).
//!
//! Not ported, with the reason: `parseRestOptions` over a live request context
//! is owned by the auth-aware slice (see `sql_restful.rs`); the REST filter
//! paths take the caller's already-parsed `QueryOptions`.

use crate::sql_restful::QueryOptions;
use crate::sqlrepo::{Repository, Value};

/// The `transcoding` table (`registerModel(&model.Transcoding{})`, `:18`).
pub const TABLE: &str = "transcoding";

/// `model.Transcoding` (`model/transcoding.go`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Transcoding {
    /// Go `ID`.
    pub id: String,
    /// Go `Name`.
    pub name: String,
    /// Go `TargetFormat`.
    pub target_format: String,
    /// Admin-only in REST reads (`:57`/`:76`) — masked by
    /// [`mask_command`] for non-admins.
    pub command: String,
    /// Go `DefaultBitRate`.
    pub default_bit_rate: i64,
}

/// A materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// `model.Transcodings`.
pub type Transcodings = Vec<Transcoding>;

/// `rest.ErrPermissionDenied` (`:43`, `:64`, `:83`, `:90`): the mutating
/// methods' non-admin failure, spelled as data rather than an error enum the
/// crate does not own.
pub const ERR_PERMISSION_DENIED: &str = "permission denied";

/// The non-admin `command` mask: Go blanks `res.Command` (`:57`) / every
/// `res[i].Command` (`:76`) rather than dropping the row.
#[must_use]
pub fn mask_command(mut t: Transcoding, is_admin: bool) -> Transcoding {
    if !is_admin {
        t.command = String::new();
    }
    t
}

/// `ReadAll`'s loop shape (`:75-78`): mask every row for non-admins.
#[must_use]
pub fn mask_commands(list: Transcodings, is_admin: bool) -> Transcodings {
    list.into_iter()
        .map(|t| mask_command(t, is_admin))
        .collect()
}

/// `Get` (`:23-27`): `SELECT * FROM transcoding WHERE id = ?`.
#[must_use]
pub fn get_sql(id: &str) -> Sql {
    let (sql, args) = Repository::new(TABLE)
        .where_eq("id", id)
        .to_sql()
        .expect("the get select materializes");
    (sql, args)
}

/// `FindByFormat` (`:38-41`): `SELECT * FROM transcoding WHERE target_format = ?`.
#[must_use]
pub fn find_by_format_sql(format: &str) -> Sql {
    let (sql, args) = Repository::new(TABLE)
        .where_eq("target_format", format)
        .to_sql()
        .expect("the find-by-format select materializes");
    (sql, args)
}

/// `CountAll`/`Count` (`:30-31`, `:61-62`) via the base `count()`
/// (`sql_base_repository.go:566-576`): `count(distinct <table>.id) as count`,
/// pagination and order dropped, `options.Filters` kept.
#[must_use]
pub fn count_sql(options: Option<&QueryOptions>) -> Sql {
    let mut sq = Repository::new(TABLE);
    if let Some(options) = options {
        sq = crate::sql_search::apply_filters(sq, options);
    }
    let (sql, args) = sq
        .count(TABLE)
        .to_sql()
        .expect("the count query materializes");
    (sql, args)
}

/// `ReadAll` (`:67-80`): `SELECT *` with `parseRestOptions`' pagination and
/// filters applied. `Read` (`:54-59`) is `get_sql` + [`mask_command`].
#[must_use]
pub fn read_all_sql(options: Option<&QueryOptions>) -> Sql {
    let (sql, args) = crate::sql_search::new_select(TABLE, options)
        .to_sql()
        .expect("the read-all select materializes");
    (sql, args)
}

/// The model's column set, `registerModel(&model.Transcoding{})`'s
/// `toSQLArgs` output in struct-declaration order (`model/transcoding.go`).
const COLUMNS: [&str; 5] = ["id", "name", "target_format", "command", "default_bit_rate"];

/// `put`'s `values` (`sql_base_repository.go:613-635` via `toSQLArgs`): the
/// column-value pairs in declaration order.
fn set_values(t: &Transcoding) -> (String, Vec<Value>) {
    (
        COLUMNS.join(", ").to_string(),
        vec![
            Value::from(t.id.clone()),
            Value::from(t.name.clone()),
            Value::from(t.target_format.clone()),
            Value::from(t.command.clone()),
            Value::Integer(t.default_bit_rate),
        ],
    )
}

/// `Put`/`Save`/`Update` (`:41-45`, `:82-85`, `:87-93`) via `put` — the
/// update-then-insert upsert pair. `Update` pins `t.ID = id` first (`:91`),
/// which the caller does before calling. `Put`/`Save`/`Update` return
/// [`ERR_PERMISSION_DENIED`] for non-admins (`:43`, `:64`, `:90`).
///
/// `# Errors`
/// [`ERR_PERMISSION_DENIED`] when `is_admin` is false.
pub fn put_sql(
    is_admin: bool,
    t: &Transcoding,
    cols_to_update: &[&str],
) -> Result<(Option<Sql>, Sql), &'static str> {
    if !is_admin {
        return Err(ERR_PERMISSION_DENIED);
    }
    // filterUpdateValues (sql_base_repository.go:601-607): selectUpdateColumns
    // keeps only requested cols (all when none given), then pins the id.
    let (_, values) = set_values(t);
    let mut set_args = Vec::new();
    let mut set_cols = Vec::new();
    for (col, value) in COLUMNS.iter().zip(values.iter()) {
        if cols_to_update.is_empty() || cols_to_update.iter().any(|c| c == col) {
            set_cols.push((*col).to_string());
            set_args.push(value.clone());
        }
    }
    let update = if t.id.is_empty() {
        None
    } else {
        let assignments = set_cols
            .iter()
            .zip(set_args.iter())
            .map(|(c, _)| format!("{c} = ?"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut args = set_args.clone();
        args.push(Value::from(t.id.clone()));
        Some((
            format!("UPDATE {TABLE} SET {assignments} WHERE id = ?"),
            args,
        ))
    };
    // Insert leg: the model's full column set (Go SetMap, all values).
    let (cols, vals) = set_values(t);
    let placeholders = vec!["?"; COLUMNS.len()].join(", ");
    let insert = (
        format!("INSERT INTO {TABLE} ({cols}) VALUES ({placeholders})"),
        vals,
    );
    Ok((update, insert))
}

/// `Delete` (`:95-103`) via `deleteByID` (`sql_base_repository.go:649-660`):
/// one delete per id; a missing row is Go's `model.ErrNotFound`, spelled as
/// data like [`ERR_PERMISSION_DENIED`].
pub const ERR_NOT_FOUND: &str = "not found";

/// `Delete` (`:95-103`): one delete per id; non-admins get
/// [`ERR_PERMISSION_DENIED`] (`:97`).
///
/// `# Errors`
/// [`ERR_PERMISSION_DENIED`] when `is_admin` is false.
pub fn delete_sql(is_admin: bool, ids: &[&str]) -> Result<Vec<Sql>, &'static str> {
    if !is_admin {
        return Err(ERR_PERMISSION_DENIED);
    }
    Ok(ids
        .iter()
        .map(|id| {
            (
                format!("DELETE FROM {TABLE} WHERE id = ?"),
                vec![Value::from(*id)],
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_and_find_by_format_bind_identity_values() {
        // Arrange/Act: Get (:23-24) and FindByFormat (:39-40).
        let get = get_sql("t1");
        let find = find_by_format_sql("mp3");
        // Assert:
        assert_eq!(get.0, "SELECT * FROM transcoding WHERE id = ?");
        assert_eq!(get.1, vec![Value::Text("t1".to_string())]);
        assert_eq!(find.0, "SELECT * FROM transcoding WHERE target_format = ?");
        assert_eq!(find.1, vec![Value::Text("mp3".to_string())]);
    }

    #[test]
    fn count_drops_pagination_and_keeps_filters() {
        // Arrange: base count() (:30-31 via sql_base_repository.go:566-576)
        // with one filter.
        let options = QueryOptions {
            filters: Some(vec![crate::sql_restful::eq_filter("target_format", "mp3")]),
            ..QueryOptions::default()
        };
        // Act:
        let (sql, args) = count_sql(Some(&options));
        // Assert:
        assert_eq!(
            sql,
            "SELECT count(distinct transcoding.id) as count FROM transcoding WHERE target_format = ?"
        );
        assert_eq!(args, vec![Value::Text("mp3".to_string())]);
    }

    #[test]
    fn read_all_selects_star_with_pagination() {
        // Arrange: ReadAll (:69-71) with parsed options.
        let options = QueryOptions {
            sort: "name".to_string(),
            order: "asc".to_string(),
            max: 20,
            ..QueryOptions::default()
        };
        // Act:
        let (sql, args) = read_all_sql(Some(&options));
        // Assert:
        assert_eq!(sql, "SELECT * FROM transcoding LIMIT 20");
        // Assert: squirrel renders the bound as a literal (`select.go:139-142`).
        assert!(args.is_empty());
    }

    #[test]
    fn mask_command_blanks_command_only_for_non_admins() {
        // Arrange: Read (:55-59) / ReadAll (:75-78) masking.
        let t = Transcoding {
            command: "ffmpeg %s".to_string(),
            ..Transcoding::default()
        };
        // Act/Assert:
        assert_eq!(mask_command(t.clone(), true).command, "ffmpeg %s");
        assert_eq!(mask_command(t.clone(), false).command, "");
        let masked = mask_commands(vec![t], false);
        assert_eq!(masked[0].command, "");
    }

    #[test]
    fn put_requires_admin_and_emits_update_then_insert() {
        // Arrange: Put (:41-45) over put()'s upsert (:613-635).
        let t = Transcoding {
            id: "t1".to_string(),
            name: "mp3 audio".to_string(),
            target_format: "mp3".to_string(),
            command: "ffmpeg -i %s".to_string(),
            default_bit_rate: 160,
        };
        // Act:
        let denied = put_sql(false, &t, &[]);
        let (update, insert) = put_sql(true, &t, &[]).expect("admin put renders");
        // Assert: non-admin is refused before any statement is built.
        assert_eq!(denied, Err(ERR_PERMISSION_DENIED));
        let (sql, args) = update.expect("an id-bearing put updates first");
        assert_eq!(
            sql,
            "UPDATE transcoding SET id = ?, name = ?, target_format = ?, command = ?, default_bit_rate = ? WHERE id = ?"
        );
        assert_eq!(args.len(), 6);
        let (sql, args) = insert;
        assert_eq!(
            sql,
            "INSERT INTO transcoding (id, name, target_format, command, default_bit_rate) VALUES (?, ?, ?, ?, ?)"
        );
        assert_eq!(args[4], Value::Integer(160));
    }

    #[test]
    fn put_update_respects_cols_to_update() {
        // Arrange: Update's colsToUpdate (:87-93) narrowing the SET list.
        let t = Transcoding {
            id: "t1".to_string(),
            name: "mp3 audio".to_string(),
            target_format: "opus".to_string(),
            command: "c".to_string(),
            default_bit_rate: 96,
        };
        // Act:
        let (update, _) = put_sql(true, &t, &["default_bit_rate"]).expect("admin put renders");
        // Assert: only the requested column (and the WHERE id) bind.
        let (sql, args) = update.expect("the update leg renders");
        assert_eq!(
            sql,
            "UPDATE transcoding SET default_bit_rate = ? WHERE id = ?"
        );
        assert_eq!(
            args,
            vec![Value::Integer(96), Value::Text("t1".to_string())]
        );
    }

    #[test]
    fn delete_requires_admin_and_deletes_each_id() {
        // Arrange: Delete (:95-103) via deleteByID (:649-660).
        // Act:
        let denied = delete_sql(false, &["t1"]);
        let stmts = delete_sql(true, &["t1", "t2"]).expect("admin delete renders");
        // Assert:
        assert_eq!(denied, Err(ERR_PERMISSION_DENIED));
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].0, "DELETE FROM transcoding WHERE id = ?");
        assert_eq!(stmts[0].1, vec![Value::Text("t1".to_string())]);
        assert_eq!(stmts[1].1, vec![Value::Text("t2".to_string())]);
    }
}
