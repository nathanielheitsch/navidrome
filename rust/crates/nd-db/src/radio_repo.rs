//! Port of `persistence/radio_repository.go` (143L) — the internet-radio
//! station table.
//!
//! Scope: the row select shapes (`Get`/`GetAll`, `:58-71`), `CountAll` and
//! `Exists` (`:43-56`), the permission-gated delete (`:51-57`, one
//! `deleteByID` per id — a zero-row delete is Go's `model.ErrNotFound`),
//! the `Put` upsert (`:90-116` via `put`, `sql_base_repository.go:613-641`:
//! UPDATE first, INSERT when no row matched or the id was empty) and the
//! artwork-resolution enqueue that never fails the save (`:107-113`).
//!
//! `Save`/`Update` (`:118-141`) are `Put` wrappers: `Update` forces the
//! entity's `ID` and passes the client's column list; `Save` is a bare `Put`.
//! They are the caller composing [`put_sql`], not separate SQL shapes.
//!
//! Ownership: Go gates writes on `isPermitted` (`:27-30` — admin only,
//! `rest.ErrPermissionDenied`); the crate has no ambient context (the
//! `sqlrepo.rs` DI rule), so the caller checks admin before calling —
//! nothing in the SQL layer can stand in for the gate. Reads are unfiltered:
//! radios are visible to every user (`:48`, no ownership/library filter in
//! `newSelect`), which is also why `registerModel`'s only filter mapping is
//! `containsFilter("name")` (`:20-22`).
//!
//! Timestamps: Go binds `time.Time` (go-sqlite3 datetime text); the port uses
//! epoch-seconds integers like `share_repo.rs`, and the caller owns the
//! clock — `Put`'s `UpdatedAt = time.Now()` / fresh-row `CreatedAt` /
//! `id.NewRandom()` minting (`:96-100`) are reproduced by [`put_sql`] taking
//! `now` and minting through [`nd_id::new_random`].
//!
//! Not ported, with the reason: `hydrateArtwork` (`:73-76`) is the shared
//! batched `item_artwork` lookup of `artwork_hydration.go:77-94` — that
//! select belongs to the artwork repository lane (`artwork_repo.rs`), and
//! every repo documents the same hand-off. `uploaded_image` is a
//! migration-added column in Go's schema; `nd-dbschema`'s fresh DDL must
//! carry it for the INSERT to bind (residual dependency on that lane).

use crate::sql_restful::{contains_filter, RestRepository};
use crate::sqlrepo::Value;
use nd_id::new_random;

/// `r.tableName` (`radio_repository.go:16` via `registerModel(&model.Radio{})`,
/// `sql_base_repository.go:104-110`).
pub const TABLE: &str = "radio";

/// The artwork queue `Put` enqueues into (`:107-113`,
/// `artwork_queue_repository.go:47-50`).
pub const ARTWORK_QUEUE_TABLE: &str = "artwork_queue";

/// `model.KindRadioArtwork.Prefix()` (`model/artwork_id.go:33`).
pub const RADIO_ARTWORK_KIND: &str = "ra";

/// `model.ImageTypePrimary` (`model/artwork.go:22`).
pub const IMAGE_TYPE_PRIMARY: &str = "primary";

/// `model.ArtworkPriorityBump` (`model/artwork.go:117`): the enqueue priority
/// so a new radio's cover resolves proactively (`:108-109`).
pub const ARTWORK_PRIORITY_BUMP: i64 = 100;

/// A materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// One `SET column = value` pair of the `put` upsert (`:96-101` via
/// `toSQLArgs`/`PostMapArgs`): identifiers are `model.Radio`'s fixed
/// snake_case `structs:` tags, values are bound.
#[derive(Debug, Clone, PartialEq)]
pub struct Col {
    /// The column name — never user text.
    pub column: String,
    /// Its bound value.
    pub value: Value,
}

impl Col {
    /// A text SET pair.
    #[must_use]
    pub fn text(column: &str, value: &str) -> Self {
        Self {
            column: column.to_string(),
            value: Value::Text(value.to_string()),
        }
    }

    /// An integer SET pair (the epoch timestamps).
    #[must_use]
    pub fn integer(column: &str, value: i64) -> Self {
        Self {
            column: column.to_string(),
            value: Value::Integer(value),
        }
    }
}

/// `model.Radio` minus the `structs:"-"` embeds (`model/radio.go:12-21`):
/// exactly the columns `toSQLArgs` yields, in struct order. `created_at`/
/// `updated_at` are epoch seconds (see module header).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RadioRecord {
    /// `id` — empty for a fresh row; [`put_sql`] mints it.
    pub id: String,
    /// `stream_url`.
    pub stream_url: String,
    /// `name` (unique, NOCASE).
    pub name: String,
    /// `home_page_url`.
    pub home_page_url: String,
    /// `uploaded_image` (migration-added column; see module header).
    pub uploaded_image: String,
    /// `created_at`.
    pub created_at: i64,
    /// `updated_at`.
    pub updated_at: i64,
}

/// `toSQLArgs(&model.Radio{})` (`helpers.go:18-37`): every column, struct
/// order, as SET pairs.
#[must_use]
pub fn put_cols(radio: &RadioRecord) -> Vec<Col> {
    vec![
        Col::text("id", &radio.id),
        Col::text("stream_url", &radio.stream_url),
        Col::text("name", &radio.name),
        Col::text("home_page_url", &radio.home_page_url),
        Col::text("uploaded_image", &radio.uploaded_image),
        Col::integer("created_at", radio.created_at),
        Col::integer("updated_at", radio.updated_at),
    ]
}

/// The columns `selectUpdateColumns` always drops on update
/// (`sql_base_repository.go:601-604`); `birth_time` does not exist on radio.
pub const NEVER_UPDATE_COLS: [&str; 1] = ["created_at"];

/// `registerModel`'s rest-facing half (`:16-22`): the `radio` table with its
/// one filter mapping (`containsFilter("name")`) and the field whitelist
/// (`registerModelWhiteList(&model.Radio{})` + annotation fields,
/// `sql_restful.go:151-154`).
#[must_use]
pub fn rest_repo() -> RestRepository {
    RestRepository::new(TABLE)
        .with_filter_mappings([("name", contains_filter("name"))])
        .with_field_white_list([
            "ID",
            "StreamUrl",
            "Name",
            "HomePageUrl",
            "UploadedImage",
            "CreatedAt",
            "UpdatedAt",
        ])
}

/// `Get`/`Read` (`:58-66,125-127`): `SELECT * … WHERE id = ?` — radios are
/// visible to every user, so no ownership restriction applies (`:48`).
#[must_use]
pub fn get_sql(id: &str) -> Sql {
    (
        format!("SELECT * FROM {TABLE} WHERE id = ?"),
        vec![Value::Text(id.to_string())],
    )
}

/// `GetAll`/`ReadAll` (`:68-71,129-133`): `newSelect(ctx, options...).Columns("*")`
/// — the caller's filters (already parsed, `rest_repo`) and pagination applied.
///
/// # Errors
/// [`crate::sqlrepo::SqlError`] when the builder cannot materialize (it
/// always can here — the table is fixed).
pub fn get_all_sql(
    options: Option<&crate::sql_restful::QueryOptions>,
) -> Result<Sql, crate::sqlrepo::SqlError> {
    let mut sq = crate::sqlrepo::Repository::new(TABLE).select(["*"]);
    if let Some(options) = options {
        sq = crate::sql_search::apply_filters(sq, options);
        sq = crate::sql_search::apply_pagination(sq, options);
    }
    sq.to_sql()
}

/// `CountAll`/`Count` (`:43-45,118-120` via `count`,
/// `sql_base_repository.go:565-575`): `count(distinct radio.id) as count`
/// with the caller's filters kept.
///
/// # Errors
/// [`crate::sqlrepo::SqlError`] when the builder cannot materialize.
pub fn count_all_sql(
    options: Option<&crate::sql_restful::QueryOptions>,
) -> Result<Sql, crate::sqlrepo::SqlError> {
    let mut sq = crate::sqlrepo::Repository::new(TABLE);
    if let Some(options) = options {
        sq = crate::sql_search::apply_filters(sq, options);
    }
    sq.count(TABLE).to_sql()
}

/// `Exists` (`:48-50` via `exists`, `sql_base_repository.go:488-493`):
/// `count(*) as exist` over the id — no ownership filter, per the Go comment.
#[must_use]
pub fn exists_sql(id: &str) -> Sql {
    (
        format!("SELECT count(*) as exist FROM {TABLE} WHERE id = ?"),
        vec![Value::Text(id.to_string())],
    )
}

/// `Delete` (`:51-57` via `deleteByID`, `sql_base_repository.go:594-601`):
/// one DELETE per id, in order; a zero-row delete is the caller's
/// not-found. The admin gate is the caller's (see module header).
#[must_use]
pub fn delete_sql(ids: &[String]) -> Vec<Sql> {
    ids.iter()
        .map(|id| {
            (
                format!("DELETE FROM {TABLE} WHERE id = ?"),
                vec![Value::Text(id.clone())],
            )
        })
        .collect()
}

/// [`put_sql`]'s output: the upsert pair plus the follow-ups Go's `Put` runs.
#[derive(Debug, Clone, PartialEq)]
pub struct PutSql {
    /// The id the row lands under — [`nd_id::new_random`] when the record
    /// came in empty (`:98-99`), else the record's.
    pub id: String,
    /// `put`'s UPDATE leg (`sql_base_repository.go:619-627`); execute first,
    /// and only run [`Self::insert`] when it matched no row.
    pub update: Sql,
    /// `put`'s INSERT leg (`:629-635`).
    pub insert: Sql,
    /// The artwork-resolution enqueue (`:107-113`); never fails the save.
    pub enqueue_artwork: Sql,
}

/// `Put` (`:90-116`): stamp `updated_at` (always) and `created_at` (fresh
/// rows), mint the id when empty, then the `put` upsert — an UPDATE against
/// the id whose SET list is [`select_update_cols`]'s output (the requested
/// columns plus the appended `updated_at`, `:101-103`, always minus
/// `created_at`, `sql_base_repository.go:601-604`), followed by the full-row
/// INSERT — plus the Bump-priority artwork enqueue.
///
/// `cols_to_update`'s names are the fixed snake_case column identifiers
/// (Go's `toSnakeCase` of the struct fields), never user text.
#[must_use]
pub fn put_sql(radio: &RadioRecord, now: i64, cols_to_update: &[&str]) -> PutSql {
    let id = if radio.id.is_empty() {
        new_random()
    } else {
        radio.id.clone()
    };
    let created_at = if radio.id.is_empty() {
        now
    } else {
        radio.created_at
    };
    let stamped = RadioRecord {
        id: id.clone(),
        created_at,
        updated_at: now,
        ..radio.clone()
    };
    let all_cols = put_cols(&stamped);
    let update_cols = select_update_cols(&all_cols, cols_to_update);
    let set = update_cols
        .iter()
        .map(|c| format!("{} = ?", c.column))
        .collect::<Vec<_>>()
        .join(", ");
    let mut update_args: Vec<Value> = update_cols.iter().map(|c| c.value.clone()).collect();
    update_args.push(Value::Text(id.clone()));
    let update = (
        format!("UPDATE {TABLE} SET {set} WHERE id = ?"),
        update_args,
    );
    let insert_set = all_cols
        .iter()
        .map(|c| format!("{} = ?", c.column))
        .collect::<Vec<_>>()
        .join(", ");
    let insert_args: Vec<Value> = all_cols.iter().map(|c| c.value.clone()).collect();
    let insert = (format!("INSERT INTO {TABLE} SET {insert_set}"), insert_args);
    PutSql {
        id: id.clone(),
        update,
        insert,
        enqueue_artwork: enqueue_artwork_sql(&id, now),
    }
}

/// `selectUpdateColumns` + the `UpdatedAt` append (`:101-103`,
/// `sql_base_repository.go:587-609`): all columns when none requested,
/// else the requested ones plus `updated_at`, always minus `created_at`.
#[must_use]
fn select_update_cols(all_cols: &[Col], cols_to_update: &[&str]) -> Vec<Col> {
    let mut wanted: Vec<&str> = cols_to_update.to_vec();
    if !wanted.is_empty() && !wanted.contains(&"updated_at") {
        wanted.push("updated_at");
    }
    all_cols
        .iter()
        .filter(|c| wanted.is_empty() || wanted.contains(&c.column.as_str()))
        .filter(|c| !NEVER_UPDATE_COLS.contains(&c.column.as_str()))
        .cloned()
        .collect()
}

/// The artwork enqueue of `Put` (`:107-113` via `Enqueue`,
/// `artwork_queue_repository.go:47-50`): the Bump-priority upsert that starts
/// a fresh lifecycle on conflict. `retry_at`/`enqueued_at` bind the same
/// caller-owned `now`.
#[must_use]
pub fn enqueue_artwork_sql(radio_id: &str, now: i64) -> Sql {
    (
        format!(
            "INSERT INTO {ARTWORK_QUEUE_TABLE} \
             (item_kind, item_id, image_type, priority, attempts, retry_at, enqueued_at) \
             VALUES (?, ?, ?, ?, 0, ?, ?) \
             ON CONFLICT (item_kind, item_id, image_type) DO UPDATE SET \
             priority = MAX(priority, excluded.priority), retry_at = excluded.retry_at, \
             attempts = 0, enqueued_at = excluded.enqueued_at, trace = '[]'"
        ),
        vec![
            Value::Text(RADIO_ARTWORK_KIND.to_string()),
            Value::Text(radio_id.to_string()),
            Value::Text(IMAGE_TYPE_PRIMARY.to_string()),
            Value::Integer(ARTWORK_PRIORITY_BUMP),
            Value::Integer(now),
            Value::Integer(now),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql_restful::{RestQueryOptions, ANNOTATION_FIELDS};
    use std::collections::BTreeMap;

    const NOW: i64 = 1_736_000_000;

    fn radio(id: &str) -> RadioRecord {
        RadioRecord {
            id: id.to_string(),
            stream_url: "https://example.com:4533/app".to_string(),
            name: "Radio One".to_string(),
            home_page_url: "https://example.com".to_string(),
            uploaded_image: "cover.png".to_string(),
            created_at: NOW - 100,
            updated_at: NOW - 100,
        }
    }

    #[test]
    fn get_pins_the_unfiltered_row_select() {
        // Arrange: Get (radio_repository.go:58-66) — every user sees radios (:48).
        // Act:
        let (sql, args) = get_sql("ra-1");
        // Assert:
        assert_eq!(sql, "SELECT * FROM radio WHERE id = ?");
        assert_eq!(args, vec![Value::Text("ra-1".to_string())]);
    }

    #[test]
    fn get_all_applies_parsed_filters_and_pagination() {
        // Arrange: GetAll (:68-71) over a contains-filtered page, the shape
        // rest_repo's "name" mapping produces (containsFilter, :21).
        let mut filters = BTreeMap::new();
        filters.insert("name".to_string(), "one".to_string());
        let rest = RestQueryOptions {
            sort: String::new(),
            order: String::new(),
            max: 10,
            offset: 20,
            filters,
        };
        let options = rest_repo().parse_rest_options(Some(&rest));
        // Act:
        let (sql, args) = get_all_sql(Some(&options)).expect("the radio select materializes");
        // Assert:
        assert_eq!(
            sql,
            "SELECT * FROM radio WHERE name LIKE ? LIMIT 10 OFFSET 20"
        );
        // Assert: the bounds are squirrel literals (`select.go:139-147`), so
        // only the parsed filter binds.
        assert_eq!(args, vec![Value::Text("%one%".to_string())]);
    }

    #[test]
    fn count_all_and_exists_pin_the_count_shapes() {
        // Arrange: CountAll (:43-45) via count() and Exists (:48-50) via exists().
        // Act:
        let (count_sql, count_args) = count_all_sql(None).expect("count materializes");
        let (exist_sql, exist_args) = exists_sql("ra-2");
        // Assert:
        assert_eq!(
            count_sql,
            "SELECT count(distinct radio.id) as count FROM radio"
        );
        assert!(count_args.is_empty());
        assert_eq!(
            exist_sql,
            "SELECT count(*) as exist FROM radio WHERE id = ?"
        );
        assert_eq!(exist_args, vec![Value::Text("ra-2".to_string())]);
    }

    #[test]
    fn delete_emits_one_statement_per_id_in_order() {
        // Arrange: Delete (:51-57) via deleteByID — the admin gate is the
        // caller's (see module header).
        // Act:
        let stmts = delete_sql(&["ra-1".to_string(), "ra-2".to_string()]);
        // Assert:
        assert_eq!(stmts.len(), 2);
        for (i, (sql, args)) in stmts.iter().enumerate() {
            assert_eq!(sql, "DELETE FROM radio WHERE id = ?");
            assert_eq!(args.as_slice(), [Value::Text(format!("ra-{}", i + 1))]);
        }
    }

    #[test]
    fn put_update_leg_sets_every_column_but_created_at() {
        // Arrange: Put with no colsToUpdate (:90-103) — selectUpdateColumns
        // keeps all, drops created_at (:601-604).
        let record = radio("ra-1");
        // Act:
        let put = put_sql(&record, NOW, &[]);
        let (sql, args) = &put.update;
        // Assert:
        assert_eq!(
            sql.as_str(),
            "UPDATE radio SET id = ?, stream_url = ?, name = ?, home_page_url = ?, \
             uploaded_image = ?, updated_at = ? WHERE id = ?"
        );
        assert_eq!(
            args.as_slice(),
            vec![
                Value::Text("ra-1".to_string()),
                Value::Text("https://example.com:4533/app".to_string()),
                Value::Text("Radio One".to_string()),
                Value::Text("https://example.com".to_string()),
                Value::Text("cover.png".to_string()),
                Value::Integer(NOW),
                Value::Text("ra-1".to_string()),
            ]
        );
    }

    #[test]
    fn put_with_cols_to_update_appends_updated_at_and_keeps_only_requested() {
        // Arrange: Update's column list (:101-103, :135-141): the client's
        // "name" plus the appended UpdatedAt — created_at never overwritten.
        let record = radio("ra-1");
        // Act:
        let put = put_sql(&record, NOW, &["name"]);
        let (sql, args) = &put.update;
        // Assert:
        assert_eq!(
            sql.as_str(),
            "UPDATE radio SET name = ?, updated_at = ? WHERE id = ?"
        );
        assert_eq!(
            args.as_slice(),
            [
                Value::Text("Radio One".to_string()),
                Value::Integer(NOW),
                Value::Text("ra-1".to_string()),
            ]
        );
    }

    #[test]
    fn put_fresh_row_mints_the_id_and_stamps_created_at() {
        // Arrange: Put with an empty id (:96-99) — NewRandom + CreatedAt=now.
        let mut record = radio("");
        record.created_at = 0;
        // Act:
        let put = put_sql(&record, NOW, &[]);
        let (sql, args) = &put.insert;
        // Assert:
        assert_eq!(put.id.len(), 22, "id2.NewRandom() is 22 base62 chars");
        assert!(sql.starts_with("INSERT INTO radio SET id = ?"));
        assert_eq!(args[0], Value::Text(put.id.clone()));
        assert_eq!(args[5], Value::Integer(NOW), "created_at = now");
        assert_eq!(args[6], Value::Integer(NOW), "updated_at = now");
        // The UPDATE leg still targets the minted id (Go's put always tries it).
        assert!(put.update.0.ends_with("WHERE id = ?"));
        assert_eq!(put.update.1.last(), Some(&Value::Text(put.id.clone())));
    }

    #[test]
    fn put_enqueues_artwork_resolution_at_bump_priority() {
        // Arrange: the never-failing enqueue (:107-113), KindRadioArtwork "ra"
        // and ArtworkPriorityBump 100.
        // Act:
        let (sql, args) = enqueue_artwork_sql("ra-9", NOW);
        // Assert:
        assert_eq!(
            sql,
            "INSERT INTO artwork_queue (item_kind, item_id, image_type, priority, attempts, retry_at, enqueued_at) \
             VALUES (?, ?, ?, ?, 0, ?, ?) \
             ON CONFLICT (item_kind, item_id, image_type) DO UPDATE SET \
             priority = MAX(priority, excluded.priority), retry_at = excluded.retry_at, \
             attempts = 0, enqueued_at = excluded.enqueued_at, trace = '[]'"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("ra".to_string()),
                Value::Text("ra-9".to_string()),
                Value::Text("primary".to_string()),
                Value::Integer(100),
                Value::Integer(NOW),
                Value::Integer(NOW),
            ]
        );
    }

    #[test]
    fn rest_repo_whitelists_radio_fields_plus_annotations() {
        // Arrange: registerModel's whitelist (:16-22, sql_restful.go:151-154).
        let repo = rest_repo();
        // Act:
        let (kept, _) = repo.sanitize_sort("Name", "asc");
        let (dropped, _) = repo.sanitize_sort("Bogus", "asc");
        // Assert:
        assert_eq!(kept, "name".to_string());
        assert_eq!(dropped, String::new());
        for field in ANNOTATION_FIELDS {
            let (sort, _) = repo.sanitize_sort(field, "");
            assert_ne!(sort, String::new(), "{field} must be whitelisted");
        }
    }

    #[test]
    fn rest_repo_name_filter_is_a_contains_match() {
        // Arrange: containsFilter("name") (:21).
        let repo = rest_repo();
        let mut filters = BTreeMap::new();
        filters.insert("name".to_string(), "jazz".to_string());
        // Act:
        let options = repo.parse_rest_options(Some(&RestQueryOptions {
            filters,
            ..RestQueryOptions::default()
        }));
        // Assert:
        let (sql, args) = options.where_clause().expect("the filter renders");
        assert_eq!(sql, "name LIKE ?");
        assert_eq!(args, vec![Value::Text("%jazz%".to_string())]);
    }
}
