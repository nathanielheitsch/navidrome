//! Port of `persistence/artwork_repository.go` (184L) — the image cache
//! (`artwork`) plus per-entity artwork state (`item_artwork`).
//!
//! House conventions as elsewhere: every method returns the materialized
//! `(sql, args)` pair; execution stays with the caller. squirrel's `SetMap`
//! inserts project sorted column names (verified against squirrel@v1.5.4
//! source in the mediafile slice), so the two upserts below list their
//! columns alphabetically, exactly what Go's `Insert().SetMap(values)` emits.
//! `kind.Prefix()` (`model/artwork_id.go:23`) is caller-supplied typed-domain
//! text, values bound — and it is the ONLY spelling the `item_kind` column ever
//! holds, so [`OWNER_TABLES`] and every test fixture below keys on the prefix
//! (`"al"`, `"mf"`, …), not the kind name. See [`OWNER_TABLES`] for the
//! statement-by-statement citations.
//!
//! The one deliberate simplification: Go's conflict suffixes are multi-line
//! raw strings; the emitted SQL here is the same statement on one line —
//! whitespace-equivalent to squirrel's join, pinning content not layout.

use chrono::{DateTime, Utc};

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

/// `artworkOwnerTables` (`:83-89`) as data: **kind PREFIX** → owning entity table.
/// Both halves are fixed identifiers from the typed Kind domain.
///
/// The KEY is `Kind.Prefix()`, not `Kind.String()`, because that is what the
/// `item_artwork.item_kind` and `artwork_queue.item_kind` columns actually hold:
/// `purgeDangling` binds `kind.Prefix()` (`persistence/artwork_repository.go:96`),
/// Go's own comment states the prefix is the column's token
/// (`model/artwork_id.go:22` — "Prefix is the short token used in artwork ids and
/// the `item_artwork.item_kind` column"), the kinds are
/// `Kind{"al","album"}`/`Kind{"mf","media_file"}` … (`model/artwork_id.go:30-35`),
/// and every writer binds the prefix too
/// (`core/artwork/artwork.go:233`, `:346`; `core/artwork/housekeeping.go:168`;
/// `persistence/artwork_queue_repository.go:38`, `:66-67`, `:108`, `:133`, `:201`).
/// Reading the column by kind NAME would match nothing. The Rust precedent for the
/// same spelling is already in the crate: `radio_repo.rs:54` uses `"ra"` and
/// `playlist_repo.rs:62` uses `"pl"`.
pub const OWNER_TABLES: [(&str, &str); 5] = [
    (nd_id::ARTWORK_KIND_ALBUM_PREFIX, "album"),
    (nd_id::ARTWORK_KIND_ARTIST_PREFIX, "artist"),
    (nd_id::ARTWORK_KIND_PLAYLIST_PREFIX, "playlist"),
    (nd_id::ARTWORK_KIND_RADIO_PREFIX, "radio"),
    (nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, "media_file"),
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
pub fn put_image_sql(a: &ArtworkRow, now: &DateTime<Utc>) -> Sql {
    // squirrel SetMap sorts the model's column names (verified in the mediafile
    // slice against squirrel@v1.5.4), so the insert lists them alphabetically.
    (
        format!(
            "INSERT INTO {TABLE} (blur_hash, created_at, dominant_color, hash, height, mime, size_bytes, thumb_hash, width) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (hash) DO UPDATE SET mime=excluded.mime, width=excluded.width, height=excluded.height, size_bytes=excluded.size_bytes, blur_hash=excluded.blur_hash, thumb_hash=excluded.thumb_hash, dominant_color=excluded.dominant_color, created_at=excluded.created_at"
        ),
        vec![
            Value::Text(a.blur_hash.clone()), crate::time::value(now),
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

/// `PurgeOrphans` (`:74-80`): delete unreferenced artwork older than the supplied
/// chrono cutoff. Its TEXT bound matches the `created_at` TEXT written by
/// [`put_image_sql`].
#[must_use]
pub fn purge_orphans_sql(created_before: &DateTime<Utc>) -> Sql {
    (
        format!(
            "DELETE FROM {TABLE} WHERE created_at < ? AND hash NOT IN (SELECT hash FROM {ITEMS_TABLE} WHERE hash <> '')"
        ),
        vec![crate::time::value(created_before)],
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
/// `attempted_at = updated_at` when unset or Go-zero (`:125-128`) — then this emits
/// `Insert(item_artwork).SetMap(values)` (sorted columns) with the recorded
/// outcome's conflict suffix (`:133-136`).
#[must_use]
pub fn put_item_artwork_sql(ia: &ItemArtworkRow, now: &DateTime<Utc>) -> Sql {
    let attempted = ia
        .attempted_at
        .as_ref()
        .filter(|attempted| **attempted != crate::time::zero())
        .unwrap_or(now);
    (
        format!(
            "INSERT INTO {ITEMS_TABLE} (attempted_at, hash, image_type, item_id, item_kind, last_failure, ref_mtime, source, source_path, trace, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (item_kind, item_id, image_type) DO UPDATE SET hash=excluded.hash, source=excluded.source, source_path=excluded.source_path, ref_mtime=excluded.ref_mtime, trace=excluded.trace, last_failure=excluded.last_failure, attempted_at=excluded.attempted_at, updated_at=excluded.updated_at",
        ),
        vec![
            crate::time::value(attempted), Value::Text(ia.hash.clone()),
            Value::Text(ia.image_type.clone()), Value::Text(ia.item_id.clone()),
            Value::Text(ia.item_kind.clone()), Value::Text(ia.last_failure.clone()),
            Value::Integer(ia.ref_mtime), Value::Text(ia.source.clone()),
            Value::Text(ia.source_path.clone()), Value::Text(ia.trace.clone()),
            crate::time::value(now),
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
    /// Go's `IsZero()` is equivalent to `None` or the year-one zero; either
    /// means `put_item_artwork_sql` substitutes the supplied `now`.
    pub attempted_at: Option<DateTime<Utc>>,
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
        let (sql, args) = put_image_sql(&a, &crate::time::zero());
        // Assert: sorted-column SetMap insert, and created_at in the
        // conflict clause (PutImage :50 — unlike a plain upsert).
        assert_eq!(args.len(), 9);
        assert_eq!(args[1], crate::time::value(&crate::time::zero()));
        assert!(sql.contains("ON CONFLICT (hash) DO UPDATE SET"));
        assert!(sql.contains("created_at=excluded.created_at"));
    }

    #[test]
    fn purge_orphans_keys_on_the_grace_cutoff() {
        // Act: PurgeOrphans (:74-80).
        let cutoff = DateTime::from_timestamp(1_000, 0).unwrap();
        let (sql, args) = purge_orphans_sql(&cutoff);
        // Assert: the NOT IN subquery is fixed, cutoff binds as timestamp TEXT.
        assert_eq!(
            sql,
            "DELETE FROM artwork WHERE created_at < ? AND hash NOT IN (SELECT hash FROM item_artwork WHERE hash <> '')"
        );
        assert_eq!(args, vec![crate::time::value(&cutoff)]);
    }

    #[test]
    fn dangling_purge_scopes_per_kind_and_owner_table() {
        // Act: PurgeDanglingItems over the media_file owner (:92-106). The kind
        // bind is the PREFIX (`kind.Prefix()`, `:96`), the table is the value from
        // OWNER_TABLES (`:83-89`) — a kind NAME would bind a token no row has.
        let (sql, args) = purge_dangling_sql(nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, "media_file");
        // Assert:
        assert_eq!(
            sql,
            "DELETE FROM item_artwork WHERE item_kind = ? AND item_id NOT IN (SELECT id FROM media_file)"
        );
        assert_eq!(args, vec![Value::Text("mf".to_string())]);
    }

    #[test]
    fn owner_tables_keys_are_the_kind_prefixes_not_the_names() {
        // Assert: every OWNER_TABLES key is a prefix from nd-id, and the pairing
        // matches Go's artworkOwnerTables (`artwork_repository.go:83-89`).
        assert_eq!(
            OWNER_TABLES,
            [
                ("al", "album"),
                ("ar", "artist"),
                ("pl", "playlist"),
                ("ra", "radio"),
                ("mf", "media_file"),
            ]
        );
        for (key, table) in OWNER_TABLES {
            assert!(
                key.len() == 2 && !key.contains('_'),
                "item_kind stores the 2-char prefix, got {key:?} for {table}"
            );
            assert!(key != table, "the key must not be the kind name ({key})");
        }
    }

    #[test]
    fn the_item_kind_bind_is_the_prefix_for_album_and_media_file() {
        // Assert the bind VALUES of the two statements artwork hydration uses,
        // for both kinds. This is the regression guard for the name-vs-prefix
        // defect: a kind name binds a token the column never contains.
        let album = get_info_for_items_sql(nd_id::ARTWORK_KIND_ALBUM_PREFIX, &["al-1".to_string()])
            .expect("renders")
            .remove(0);
        assert_eq!(album.1[0], Value::Text("al".to_string()));
        let track =
            get_info_for_items_sql(nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, &["mf-1".to_string()])
                .expect("renders")
                .remove(0);
        assert_eq!(track.1[0], Value::Text("mf".to_string()));

        // Same for the writers: PutItemArtwork binds item_kind from the row, and
        // DeleteForItems binds the kind.
        let put = put_item_artwork_sql(
            &ItemArtworkRow {
                item_kind: nd_id::ARTWORK_KIND_ALBUM_PREFIX.to_string(),
                item_id: "al-1".to_string(),
                ..Default::default()
            },
            &crate::time::zero(),
        );
        assert_eq!(
            put.1[4],
            Value::Text("al".to_string()),
            "item_kind is the 5th column of the sorted SetMap insert"
        );
        let deleted =
            delete_for_items_sql(nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, &["mf-1".to_string()]);
        assert_eq!(deleted[0].1[0], Value::Text("mf".to_string()));
        // GetItemArtwork's three-key Eq, also prefix-first.
        let got =
            get_item_artwork_sql(nd_id::ARTWORK_KIND_ALBUM_PREFIX, "al-1", IMAGE_TYPE_PRIMARY);
        assert_eq!(got.1[0], Value::Text("al".to_string()));
    }

    #[test]
    fn put_item_artwork_stamps_attempted_at_when_unset() {
        // Arrange: an unattempted item (PutItemArtwork :125-128).
        let ia = ItemArtworkRow {
            item_kind: nd_id::ARTWORK_KIND_ALBUM_PREFIX.to_string(),
            item_id: "al-1".to_string(),
            ..Default::default()
        };
        // Act:
        let (sql, args) = put_item_artwork_sql(&ia, &crate::time::zero());
        // Assert: attempted_at falls back to updated_at's now, and image_type
        // is the caller's (cmp.Or at :123 is caller-side; empty here pinned).
        assert_eq!(args[0], crate::time::value(&crate::time::zero()));
        assert_eq!(args.last(), Some(&crate::time::value(&crate::time::zero())));
        assert!(sql.contains("ON CONFLICT (item_kind, item_id, image_type) DO UPDATE SET"));
    }

    #[test]
    fn delete_for_items_chunks_at_the_batch_size() {
        // Arrange: 201 ids (DeleteForItems :151 slices.Chunk).
        let ids = (1..=201).map(|i| format!("it-{i}")).collect::<Vec<_>>();
        // Act:
        let stmts = delete_for_items_sql(nd_id::ARTWORK_KIND_ALBUM_PREFIX, &ids);
        // Assert:
        assert_eq!(stmts.len(), 2);
        assert_eq!(
            stmts[1].1,
            vec![
                Value::Text("al".to_string()),
                Value::Text("it-201".to_string())
            ]
        );
    }

    #[test]
    fn info_hydration_left_joins_the_image_cache() {
        // Act: GetInfoForItems single chunk (:159-182).
        let stmts = get_info_for_items_sql(nd_id::ARTWORK_KIND_ALBUM_PREFIX, &["al-1".to_string()])
            .unwrap();
        // Assert: the LEFT JOIN splices after FROM, before WHERE.
        assert!(stmts[0]
            .0
            .contains("FROM item_artwork ia LEFT JOIN artwork a ON a.hash = ia.hash WHERE"));
        assert_eq!(stmts[0].1.len(), 3);
    }
}
