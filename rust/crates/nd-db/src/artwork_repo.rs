//! Port of `persistence/artwork_repository.go` (184L) — the image cache
//! (`artwork`) plus per-entity artwork state (`item_artwork`).
//!
//! House conventions as elsewhere: every method returns the materialized
//! `(sql, args)` pair; execution stays with the caller. squirrel's `SetMap`
//! inserts project sorted column names (verified against squirrel@v1.5.4
//! source in the mediafile slice), so the two upserts below list their
//! columns alphabetically, exactly what Go's `Insert().SetMap(values)` emits.
//! `kind.Prefix()` (`model/artwork_id.go:23`) is caller-supplied typed-domain
//! text, values bound.
//!
//! The one deliberate simplification: Go's conflict suffixes are multi-line
//! raw strings; the emitted SQL here is the same statement on one line —
//! whitespace-equivalent to squirrel's join, pinning content not layout.

use crate::sqlrepo::{Repository, SqlError, Value};

/// A materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// The image table (`:27`).
pub const TABLE: &str = "artwork";

/// The entity-state table (`:29`, `:15-17`).
pub const ITEMS_TABLE: &str = "item_artwork";

/// `artworkBatchSize` (`:16-17`): rows per multi-row statement for
/// `DeleteForItems` and `GetInfoForItems`.
pub const BATCH_SIZE: usize = 200;

/// `model.ImageTypePrimary` (`model/artwork.go:43`), the `PutItemArtwork`
/// default (`:123`, `cmp.Or`).
pub const IMAGE_TYPE_PRIMARY: &str = "primary";

/// `artworkOwnerTables` (`:83-89`) as data: kind prefix → owning entity table.
/// Both halves are fixed identifiers from the typed Kind domain.
pub const OWNER_TABLES: [(&str, &str); 5] = [
    ("album", "album"),
    ("artist", "artist"),
    ("playlist", "playlist"),
    ("radio", "radio"),
    ("media_file", "media_file"),
];

/// `GetImage` (`:33-40`): `Select("*").From(artwork).Where(Eq{"hash": hash})`.
#[must_use]
pub fn get_image_sql(hash: &str) -> Sql {
    Repository::new(TABLE)
        .select(vec!["*"])
        .where_eq("hash", hash)
        .to_sql()
        .unwrap()
}

/// `PutImage` (`:42-56`): `Insert(artwork).SetMap(toSQLArgs(*a))` — sorted
/// columns — with the conflict suffix that resets `created_at` so reacquiring
/// an orphan moves its prune-grace window (`:49`, `:50`).
#[must_use]
pub fn put_image_sql(a: &ArtworkRow, now: i64) -> Sql {
    // squirrel SetMap sorts the model's column names (verified in the mediafile
    // slice against squirrel@v1.5.4), so the insert lists them alphabetically.
    (
        format!(
            "INSERT INTO {TABLE} (blur_hash, created_at, dominant_color, hash, height, mime, size_bytes, thumb_hash, width) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (hash) DO UPDATE SET mime=excluded.mime, width=excluded.width, height=excluded.height, size_bytes=excluded.size_bytes, blur_hash=excluded.blur_hash, thumb_hash=excluded.thumb_hash, dominant_color=excluded.dominant_color, created_at=excluded.created_at"
        ),
        vec![
            Value::Text(a.blur_hash.clone()), Value::Integer(now),
            Value::Text(a.dominant_color.clone()), Value::Text(a.hash.clone()),
            Value::Integer(a.height), Value::Text(a.mime.clone()),
            Value::Integer(a.size_bytes), Value::Text(a.thumb_hash.clone()),
            Value::Integer(a.width),
        ],
    )
}

/// `model.Artwork` (`model/artwork.go:8-18`); `CreatedAt` is stamped by the
/// caller with the last-acquisition-write time (`:44`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArtworkRow {
    /// XXH3-64 of the image bytes (`model/artwork.go:8`).
    pub hash: String,
    /// e.g. `image/jpeg`.
    pub mime: String,
    /// Pixels.
    pub width: i64,
    /// Pixels.
    pub height: i64,
    /// Image byte size.
    pub size_bytes: i64,
    /// Jellyfin blur hash.
    pub blur_hash: String,
    /// Thumbnail thumb hash.
    pub thumb_hash: String,
    /// `#rrggbb` placeholder (`model/artwork.go:14`).
    pub dominant_color: String,
}

/// `GetMimeByHash` (`:58-72`): the two-column scan; the caller folds rows
/// into Go's hash→mime map.
#[must_use]
pub fn get_mime_by_hash_sql() -> Sql {
    (format!("SELECT hash, mime FROM {TABLE}"), Vec::new())
}

/// `PurgeOrphans` (`:74-80`): `Delete(artwork).Where(And{Lt{created_at},
/// Expr("hash NOT IN (SELECT hash FROM item_artwork WHERE hash <> '')")})`
/// — `createdBefore` (epoch seconds) binds, the subquery fragment is fixed.
#[must_use]
pub fn purge_orphans_sql(created_before: i64) -> Sql {
    (
        format!(
            "DELETE FROM {TABLE} WHERE created_at < ? AND hash NOT IN (SELECT hash FROM {ITEMS_TABLE} WHERE hash <> '')"
        ),
        vec![Value::Integer(created_before)],
    )
}

/// `purgeDangling` (`:92-106`) against `items`: one delete per kind —
/// `WHERE item_kind = ? AND item_id NOT IN (SELECT id FROM <entity table>)`.
/// `entityTable` comes from [`OWNER_TABLES`], a fixed schema identifier.
#[must_use]
pub fn purge_dangling_sql(kind: &str, entity_table: &str) -> Sql {
    (
        format!(
            "DELETE FROM {ITEMS_TABLE} WHERE item_kind = ? AND item_id NOT IN (SELECT id FROM {entity_table})"
        ),
        vec![Value::Text(kind.to_string())],
    )
}

/// `GetItemArtwork` (`:112-120`): the three-key Eq select.
#[must_use]
pub fn get_item_artwork_sql(kind: &str, id: &str, image_type: &str) -> Sql {
    let sq = Repository::new(ITEMS_TABLE)
        .select(vec!["*"])
        .where_eq("item_kind", kind)
        .where_eq("item_id", id)
        .where_eq("image_type", image_type);
    sq.to_sql().unwrap()
}

/// `PutItemArtwork` (`:122-139`): the caller applies the defaults Go does —
/// `image_type = cmp.Or(image_type, primary)` (`:123`) and
/// `attempted_at = updated_at` when unset (`:125-128`) — then this emits
/// `Insert(item_artwork).SetMap(values)` (sorted columns) with the recorded
/// outcome's conflict suffix (`:133-136`).
#[must_use]
pub fn put_item_artwork_sql(ia: &ItemArtworkRow, now: i64) -> Sql {
    let attempted = if ia.attempted_at == 0 {
        now
    } else {
        ia.attempted_at
    };
    (
        format!(
            "INSERT INTO {ITEMS_TABLE} (attempted_at, hash, image_type, item_id, item_kind, last_failure, ref_mtime, source, source_path, trace, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (item_kind, item_id, image_type) DO UPDATE SET hash=excluded.hash, source=excluded.source, source_path=excluded.source_path, ref_mtime=excluded.ref_mtime, trace=excluded.trace, last_failure=excluded.last_failure, attempted_at=excluded.attempted_at, updated_at=excluded.updated_at",
        ),
        vec![
            Value::Integer(attempted), Value::Text(ia.hash.clone()),
            Value::Text(ia.image_type.clone()), Value::Text(ia.item_id.clone()),
            Value::Text(ia.item_kind.clone()), Value::Text(ia.last_failure.clone()),
            Value::Integer(ia.ref_mtime), Value::Text(ia.source.clone()),
            Value::Text(ia.source_path.clone()), Value::Text(ia.trace.clone()),
            Value::Integer(now),
        ],
    )
}

/// `model.ItemArtwork` (`model/artwork.go:47+`): the per-entity state row;
/// `hash == ""` means known absent (`:49`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ItemArtworkRow {
    /// `Kind.Prefix()` typed-domain text.
    pub item_kind: String,
    /// Entity id.
    pub item_id: String,
    /// Image slot kind.
    pub image_type: String,
    /// Empty means known-absent (`model/artwork.go:49`).
    pub hash: String,
    /// Resolution source; `!failed` is the gave-up pseudo-source.
    pub source: String,
    /// Backing file (image or audio), "" otherwise.
    pub source_path: String,
    /// SourcePath's mtime at resolution, unix ns (0 without a path).
    pub ref_mtime: i64,
    /// JSON encoded walk.
    pub trace: String,
    /// The exhausted-retry walk (`PutLastFailure`).
    pub last_failure: String,
    /// Epoch seconds, nullable per schema but always set on insert (`:44`);
    /// 0 marks "unset" so `put_item_artwork_sql` substitutes `now`.
    pub attempted_at: i64,
}

/// `PutLastFailure` (`:143-148`): update-only (a fresh row would write an
/// empty hash, which reads as settled absent — Go's comment `:141-142`).
#[must_use]
pub fn put_last_failure_sql(kind: &str, id: &str, image_type: &str, trace: &str) -> Sql {
    (
        format!(
            "UPDATE {ITEMS_TABLE} SET last_failure = ? WHERE item_kind = ? AND item_id = ? AND image_type = ?"
        ),
        vec![
            Value::Text(trace.to_string()), Value::Text(kind.to_string()),
            Value::Text(id.to_string()), Value::Text(image_type.to_string()),
        ],
    )
}

/// `DeleteForItems` (`:150-157`): chunked `Eq{item_kind, item_id: chunk}`
/// deletes (`slices.Chunk(ids, artworkBatchSize)`).
#[must_use]
pub fn delete_for_items_sql(kind: &str, ids: &[String]) -> Vec<Sql> {
    ids.chunks(BATCH_SIZE)
        .map(|chunk| {
            Repository::new(ITEMS_TABLE)
                .where_eq("item_kind", kind)
                .where_in(
                    "item_id",
                    chunk
                        .iter()
                        .map(|id| Value::Text(id.clone()))
                        .collect::<Vec<_>>(),
                )
                .to_sql()
                .unwrap()
        })
        .collect::<Vec<_>>()
}

/// `GetInfoForItems`' per-chunk hydration (`:159-182`): the COALESCE'd
/// projection over `item_artwork ia LEFT JOIN artwork a ON a.hash = ia.hash`
/// (join spliced at squirrel's position after FROM, args after WHERE —
/// neither join side binds).
pub fn get_info_for_items_sql(kind: &str, ids: &[String]) -> Result<Vec<Sql>, SqlError> {
    let columns: [&str; 7] = [
        "ia.item_id",
        "ia.hash",
        "COALESCE(a.blur_hash, '') as blur_hash",
        "COALESCE(a.thumb_hash, '') as thumb_hash",
        "COALESCE(a.dominant_color, '') as dominant_color",
        "COALESCE(a.width, 0) as width",
        "COALESCE(a.height, 0) as height",
    ];
    let mut stmts = Vec::new();
    for chunk in ids.chunks(BATCH_SIZE) {
        let sq = Repository::new(format!("{ITEMS_TABLE} ia"))
            .select(columns)
            .where_eq("ia.item_kind", kind)
            .where_eq("ia.image_type", IMAGE_TYPE_PRIMARY)
            .where_in(
                "ia.item_id",
                chunk
                    .iter()
                    .map(|id| Value::Text(id.clone()))
                    .collect::<Vec<_>>(),
            );
        let (mut sql, args) = sq.to_sql()?;
        let from = format!("FROM {ITEMS_TABLE} ia");
        let Some(pos) = sql.find(&from) else {
            return Err(SqlError::NoTable);
        };
        sql.replace_range(
            (pos + from.len())..(pos + from.len()),
            " LEFT JOIN artwork a ON a.hash = ia.hash",
        );
        stmts.push((sql, args));
    }
    Ok(stmts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_image_upsert_resets_the_grace_window_on_conflict() {
        // Arrange: an acquired image (model.Artwork).
        let a = ArtworkRow {
            hash: "h1".to_string(),
            ..Default::default()
        };
        // Act:
        let (sql, args) = put_image_sql(&a, 42);
        // Assert: sorted-column SetMap insert, and created_at in the
        // conflict clause (PutImage :50 — unlike a plain upsert).
        assert_eq!(args.len(), 9);
        assert_eq!(args[1], Value::Integer(42));
        assert!(sql.contains("ON CONFLICT (hash) DO UPDATE SET"));
        assert!(sql.contains("created_at=excluded.created_at"));
    }

    #[test]
    fn purge_orphans_keys_on_the_grace_cutoff() {
        // Act: PurgeOrphans (:74-80).
        let (sql, args) = purge_orphans_sql(1000);
        // Assert: the NOT IN subquery is fixed, cutoff binds.
        assert_eq!(
            sql,
            "DELETE FROM artwork WHERE created_at < ? AND hash NOT IN (SELECT hash FROM item_artwork WHERE hash <> '')"
        );
        assert_eq!(args, vec![Value::Integer(1000)]);
    }

    #[test]
    fn dangling_purge_scopes_per_kind_and_owner_table() {
        // Act: PurgeDanglingItems over the media_file owner (:92-106).
        let (sql, args) = purge_dangling_sql("media_file", "media_file");
        // Assert:
        assert_eq!(
            sql,
            "DELETE FROM item_artwork WHERE item_kind = ? AND item_id NOT IN (SELECT id FROM media_file)"
        );
        assert_eq!(args, vec![Value::Text("media_file".to_string())]);
    }

    #[test]
    fn put_item_artwork_stamps_attempted_at_when_unset() {
        // Arrange: an unattempted item (PutItemArtwork :125-128).
        let ia = ItemArtworkRow {
            item_kind: "album".to_string(),
            item_id: "al-1".to_string(),
            ..Default::default()
        };
        // Act:
        let (sql, args) = put_item_artwork_sql(&ia, 55);
        // Assert: attempted_at falls back to updated_at's now, and image_type
        // is the caller's (cmp.Or at :123 is caller-side; empty here pinned).
        assert_eq!(args[0], Value::Integer(55));
        assert_eq!(args.last(), Some(Value::Integer(55)).as_ref());
        assert!(sql.contains("ON CONFLICT (item_kind, item_id, image_type) DO UPDATE SET"));
    }

    #[test]
    fn delete_for_items_chunks_at_the_batch_size() {
        // Arrange: 201 ids (DeleteForItems :151 slices.Chunk).
        let ids = (1..=201).map(|i| format!("it-{i}")).collect::<Vec<_>>();
        // Act:
        let stmts = delete_for_items_sql("album", &ids);
        // Assert:
        assert_eq!(stmts.len(), 2);
        assert_eq!(
            stmts[1].1,
            vec![
                Value::Text("album".to_string()),
                Value::Text("it-201".to_string())
            ]
        );
    }

    #[test]
    fn info_hydration_left_joins_the_image_cache() {
        // Act: GetInfoForItems single chunk (:159-182).
        let stmts = get_info_for_items_sql("album", &["al-1".to_string()]).unwrap();
        // Assert: the LEFT JOIN splices after FROM, before WHERE.
        assert!(stmts[0]
            .0
            .contains("FROM item_artwork ia LEFT JOIN artwork a ON a.hash = ia.hash WHERE"));
        assert_eq!(stmts[0].1.len(), 3);
    }
}
