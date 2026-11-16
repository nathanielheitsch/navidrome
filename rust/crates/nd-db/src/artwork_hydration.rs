//! S-D3c — port of `persistence/artwork_hydration.go` (176L): the SECOND
//! hydration layer, a batched `item_artwork` lookup that fills the entities'
//! embedded `ItemImage`.
//!
//! It is deliberately its own module and NOT part of `hydrate.rs`: the PostScan
//! contract has exactly eight pure-column row hooks and none of them queries
//! (`docs/rust-port/hydrate-scan-contract.md:54` — "artwork hydration is a
//! *different* batched read and should be its own sub-slice, **not** folded into
//! `hydrate.rs`", and `:213-214`, sub-slice S-D3f). PostScan decoding stays in
//! [`crate::hydrate`]; this layer runs after it, on the hydrated records.
//!
//! DTO rule: [`ItemImage`] and [`ItemArtworkInfo`] are nd-db-local. The
//! crate defines no nd-model artwork DTOs
//! (`docs/rust-port/hydrate-contract.md` §6 item 4), and both Go types are
//! `structs:"-"` (`model/artwork.go:29-40`, `:70-78`), so they are read-path
//! state that never appears in `album_row`/`mediafile_row`: the write maps stay
//! at 48 and 66 keys, which `docs/rust-port/next-work.md:775-779` pins and
//! `runtime_hydrate_album_mediafile.rs::repo_hydrated_records_re_render_the_same_write_maps`
//! executes.
//!
//! Logging split: Go's `hydrateItemImages` swallows a failed lookup, logs
//! `log.Error` and returns an EMPTY map so the page still renders
//! (`artwork_hydration.go:62-66`). nd-db has no logging dependency
//! (`sqlrepo.rs:13-17` — "the crate has no logging dependency") and no ambient
//! ctx, so the port swallows the same failure but EXPOSES it as an error-text
//! `Option<String>` the caller can hand to its logger — the exact precedent of
//! [`crate::share_repo::ShareRepo::load`], which returns a flag for Go's
//! swallowed `log.Warn` (`share_repo.rs:436-472`).
//!
//! Configuration split: Go reads `conf.Server.EnableMediaFileCoverArt` directly
//! (`artwork_hydration.go:99,111`, default `true` at `conf/configuration.go:1003`).
//! nd-db does not read config, so the port takes it as a PARAMETER, following
//! the crate's caller-supplied convention (`album_repo::get_all_sql`'s
//! `normalized_album_play_count`).
//!
//! Not ported here (streaming lane, `artwork_hydration.go:19-54,138-161`):
//! `streamByIDs`, `chunkOptions`, `hydrateCursor` and
//! `hydratePlaylistTrackArtwork`. `album_repo::cursor_chunk_sql` exists for
//! that lane, and playlist-track hydration reuses
//! [`hydrate_media_file_artwork`] once that lane has media files to fill.
//!
//! Two chunk sizes exist in Go and only one belongs here: `artworkBatchSize` =
//! 200 (`artwork_repository.go:16`, used by `GetInfoForItems` at `:161`) is
//! [`crate::artwork_repo::BATCH_SIZE`] and drives [`hydrate_item_images`];
//! `artworkChunkSize` = `artworkBatchSize * 3` = 600 (`artwork_hydration.go:17`)
//! belongs to `streamByIDs` alone and is deliberately NOT exposed by this module
//! until the streaming lane needs it.
//!
//! Id lists are NOT deduplicated: Go collects ids in slice order with duplicates
//! retained (`artwork_hydration.go:81-84`), so a page with repeated ids issues
//! the same IN-list. Deduping would change query shape (chunk count and IN-list
//! contents) while leaving results identical, so the port matches Go exactly.

use std::collections::HashMap;

use rusqlite::Connection;
use rusqlite::Row;

use crate::album_repo::AlbumRecord;
use crate::artwork_repo;
use crate::mediafile_repo::MediaFileRecord;

/// `model.ItemImage` (`model/artwork.go:29-40`): the per-entity artwork state
/// hydrated at query time and never persisted (every field is `structs:"-"`).
///
/// The zero value is Go's zero value too, and it means UNRESOLVED: no entry in
/// the batched lookup leaves the embedded struct untouched
/// (`applyItemImage`, `artwork_hydration.go:71-75`), so `image_hash` stays
/// empty and `image_absent` stays false — distinct from the known-absent entry
/// (`hash == ""`), which sets `image_absent = true` via
/// [`ItemArtworkInfo::image`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemImage {
    /// `ImageHash` (`model/artwork.go:30`): the image's content hash; empty
    /// when absent or unresolved.
    pub image_hash: String,
    /// `ImageAbsent` (`:31`): known-absent, i.e. serving definitively yields a
    /// placeholder. Never set for an unresolved item.
    pub image_absent: bool,
    /// `BlurHash` (`:33`): Jellyfin's blur hash (off native JSON — its mappers
    /// read the field directly).
    pub blur_hash: String,
    /// `ThumbHash` (`:34`).
    pub thumb_hash: String,
    /// `DominantColor` (`:36`): `#rrggbb` placeholder.
    pub dominant_color: String,
    /// `ImageWidth` (`:38`): Go `int` → `i32` (Go's int is 64-bit on the
    /// supported platforms but the column's `INTEGER` affinity and the
    /// pixel-domain value make `i32` the faithful width).
    pub image_width: i32,
    /// `ImageHeight` (`:39`), same spelling as [`ItemImage::image_width`].
    pub image_height: i32,
}

/// `model.ItemArtworkInfo` (`model/artwork.go:70-78`): the list-hydration
/// projection, one row of `item_artwork ia LEFT JOIN artwork a`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemArtworkInfo {
    /// `ItemID` — the map key Go indexes by
    /// (`artwork_repository.go:178-180`).
    pub item_id: String,
    /// `Hash`: `""` means known absent (`model/artwork.go:79-80`).
    pub hash: String,
    /// `COALESCE(a.blur_hash, '')`.
    pub blur_hash: String,
    /// `COALESCE(a.thumb_hash, '')`.
    pub thumb_hash: String,
    /// `COALESCE(a.dominant_color, '')`.
    pub dominant_color: String,
    /// `COALESCE(a.width, 0)`.
    pub width: i32,
    /// `COALESCE(a.height, 0)`.
    pub height: i32,
}

impl ItemArtworkInfo {
    /// `ItemArtworkInfo.Absent()` (`model/artwork.go:81`): resolved with no
    /// image. An UNRESOLVED item is not absent — it has no entry at all.
    #[must_use]
    pub fn absent(&self) -> bool {
        self.hash.is_empty()
    }

    /// `ItemArtworkInfo.Image()` (`model/artwork.go:84-93`): the entity-facing
    /// projection. Note the two derived fields: `image_absent` comes from
    /// [`Self::absent`], so a found hash never sets it.
    #[must_use]
    pub fn image(&self) -> ItemImage {
        ItemImage {
            image_hash: self.hash.clone(),
            image_absent: self.absent(),
            blur_hash: self.blur_hash.clone(),
            thumb_hash: self.thumb_hash.clone(),
            dominant_color: self.dominant_color.clone(),
            image_width: self.width,
            image_height: self.height,
        }
    }
}

/// The result of a read that hydrates artwork onto its rows.
///
/// `artwork_error` is Go's swallowed `log.Error`
/// (`artwork_hydration.go:62-66`) surfaced as text: the read still returns its
/// rows (with every [`ItemImage`] left zero/unresolved, exactly as Go's empty
/// map leaves them) and the caller decides whether to log. `None` means the
/// lookup succeeded. When a read issues two lookups — media_file's album kind
/// and media_file kind — both failures are reported, joined with `"; "` in
/// Go's call order; Go logs them as two separate entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOutcome<T> {
    /// The hydrated rows, in statement order.
    pub items: Vec<T>,
    /// Go's swallowed artwork-lookup failure, as text (`None` when it succeeded).
    pub artwork_error: Option<String>,
}

/// `hydrateItemImages` (`artwork_hydration.go:58-68`): one batched lookup per
/// kind, in [`crate::artwork_repo::BATCH_SIZE`] chunks
/// (`artwork_repository.go:159-182`).
///
/// `kind` is the kind PREFIX (`artwork_repository.go:169` binds
/// `Eq{"ia.item_kind": kind.Prefix()}` inside the `Where(And{…})` at `:168-172`);
/// pass one of nd-id's `ARTWORK_KIND_*_PREFIX` constants
/// (`nd-id/src/artwork.rs:27,35,43,51,59,67`).
///
/// No ids means no query at all (`:59-61`), so the empty map is returned with
/// `None`. On a statement failure the port returns the EMPTY map — Go returns
/// `nil` from `GetInfoForItems` (`:176`) and `hydrateItemImages` replaces it
/// with an empty map, so rows collected by an earlier chunk are DISCARDED, not
/// partially applied; the failure text is returned instead of logged.
///
/// # Errors
/// The failure is not raised: it is returned as text. A rusqlite error inside
/// the row mapper is reported the same way.
#[must_use]
pub fn hydrate_item_images(
    conn: &Connection,
    kind: &str,
    ids: &[String],
) -> (HashMap<String, ItemArtworkInfo>, Option<String>) {
    if ids.is_empty() {
        return (HashMap::new(), None);
    }
    let statements = match artwork_repo::get_info_for_items_sql(kind, ids) {
        Ok(stmts) => stmts,
        // The builder's only failure mode (`SqlError::NoTable`) is unreachable
        // for a fixed table; report it in the same channel as a sqlite failure.
        Err(err) => return (HashMap::new(), Some(err.to_string())),
    };
    let mut infos = HashMap::new();
    for statement in &statements {
        match crate::execution::query_all(conn, statement, row_to_item_artwork_info) {
            Ok(rows) => {
                for row in rows {
                    infos.insert(row.item_id.clone(), row);
                }
            }
            Err(err) => {
                // Go: `return nil, err` (:175-177), then the caller's empty map.
                return (HashMap::new(), Some(err.to_string()));
            }
        }
    }
    (infos, None)
}

/// The 7 aliased columns of `GetInfoForItems`
/// (`artwork_repository.go:161-167`), scanned into [`ItemArtworkInfo`]. The
/// COALESCE arms mean the joined columns are never NULL, and `item_kind`/
/// `image_type` are WHERE predicates rather than selected columns — so
/// `item_id` and `hash` are the only unguarded scans, both `NOT NULL` in the
/// fresh DDL (`nd-dbschema::schema_ddl`'s `item_artwork` table).
fn row_to_item_artwork_info(row: &Row<'_>) -> rusqlite::Result<ItemArtworkInfo> {
    Ok(ItemArtworkInfo {
        item_id: row.get("item_id")?,
        hash: row.get("hash")?,
        blur_hash: row.get("blur_hash")?,
        thumb_hash: row.get("thumb_hash")?,
        dominant_color: row.get("dominant_color")?,
        width: row.get("width")?,
        height: row.get("height")?,
    })
}

/// `applyItemImage` (`artwork_hydration.go:71-75`): copy an entry onto `img`.
/// A missing entry leaves it ZERO (unresolved) — the whole point of Go's
/// `if info, ok := infos[id]; ok` guard.
pub fn apply_item_image(infos: &HashMap<String, ItemArtworkInfo>, id: &str, img: &mut ItemImage) {
    if let Some(info) = infos.get(id) {
        *img = info.image();
    }
}

/// `hydrateItems` (`artwork_hydration.go:79-90`): collect the ids, run ONE
/// batched lookup for `kind`, then apply per item.
///
/// Go's `ref func(*T) (string, *model.ItemImage)` cannot be one Rust closure
/// (it would return an immutable and a mutable borrow of the same item at
/// once), so it splits into `id_of` and `image_of`, which is the same
/// information: "an item's id and the ItemImage to fill, which is all that
/// differs between the entity kinds" (Go's comment `:77-78`).
///
/// Ids are collected in slice order WITHOUT dedup, matching `:81-84`.
///
/// The lookup failure is appended to `artwork_error` rather than logged, and
/// leaves every item's image zero — Go's empty-map path.
pub fn hydrate_items<T>(
    conn: &Connection,
    kind: &str,
    items: &mut [T],
    id_of: impl Fn(&T) -> &str,
    image_of: impl Fn(&mut T) -> &mut ItemImage,
    artwork_error: &mut Option<String>,
) {
    let ids: Vec<String> = items.iter().map(|item| id_of(item).to_string()).collect();
    let (infos, error) = hydrate_item_images(conn, kind, &ids);
    record_artwork_error(artwork_error, error);
    for item in items.iter_mut() {
        // The id is cloned before the mutable borrow so the two borrows never
        // overlap (Go copies the string out of `ref` for the same reason).
        let id = id_of(item).to_string();
        apply_item_image(&infos, &id, image_of(item));
    }
}

/// `albumRepository.hydrateArtwork` (`album_repository.go:257-260`): the
/// embedded [`ItemImage`] of every album, from one `al`-kind lookup.
pub fn hydrate_album_artwork(
    conn: &Connection,
    albums: &mut [AlbumRecord],
    artwork_error: &mut Option<String>,
) {
    hydrate_items(
        conn,
        nd_id::ARTWORK_KIND_ALBUM_PREFIX,
        albums,
        |a| a.id.as_str(),
        |a| &mut a.image,
        artwork_error,
    );
}

/// `hydrateMediaFileArtwork` (`artwork_hydration.go:94-135`): the branch matrix
/// reproduced verbatim, in Go's statement order and with every `continue`.
///
/// Inputs: `enable_media_file_cover_art` is Go's
/// `conf.Server.EnableMediaFileCoverArt` (`:99,111`; default `true`,
/// `conf/configuration.go:1003`) supplied by the caller. `albumIDs` is collected
/// for ALL tracks, not only the eligible ones (`:98-103`), so a track with no
/// own art still finds its album's state. Two lookups follow: the `al` kind for
/// the album ids and the `mf` kind for the ELIGIBLE ids only (`:105-106`).
///
/// The matrix, per track, after `album_image` is applied FIRST (`:110`):
/// 1. eligible && own resolved && own not absent → own image, `continue` (`:112-116`).
/// 2. `own_wont_resolve = !eligible || (own_resolved && own.absent())` (`:117`).
/// 3. album entry present and NOT absent: inherit only when `disc_number == 0`
///    and `own_wont_resolve`, then `continue` REGARDLESS (`:120-125`) — a
///    multi-disc track is served disc art, and an eligible-but-unresolved one
///    still extracts its own embedded image (Go's comment `:118-119`).
/// 4. `disc_number > 0` → `continue` (`:128-130`): disc art keeps the track
///    requestable.
/// 5. Otherwise set `image_absent` only when the album entry is present, absent,
///    and `own_wont_resolve` (`:131-133`).
///
/// Returns Go's swallowed lookup failure(s) as text (see [`ReadOutcome`]).
#[must_use]
pub fn hydrate_media_file_artwork(
    conn: &Connection,
    mfs: &mut [MediaFileRecord],
    enable_media_file_cover_art: bool,
) -> Option<String> {
    if mfs.is_empty() {
        return None;
    }
    // :98-103 — albumIDs for every track; eligibleIDs for has_cover_art &&
    // enableMediaFileCoverArt only.
    let album_ids: Vec<String> = mfs.iter().map(|m| m.album_id.clone()).collect();
    let eligible_ids: Vec<String> = mfs
        .iter()
        .filter(|m| m.has_cover_art && enable_media_file_cover_art)
        .map(|m| m.id.clone())
        .collect();
    // :105-106 — two batched lookups, one per kind.
    let (album_infos, album_error) =
        hydrate_item_images(conn, nd_id::ARTWORK_KIND_ALBUM_PREFIX, &album_ids);
    let (own_infos, own_error) =
        hydrate_item_images(conn, nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, &eligible_ids);

    let mut error: Option<String> = None;
    record_artwork_error(&mut error, album_error);
    record_artwork_error(&mut error, own_error);

    for mf in mfs.iter_mut() {
        // :110 — the album's own state is applied before any branch logic, on
        // every path including the own-art-wins `continue`.
        apply_item_image(&album_infos, &mf.album_id, &mut mf.album_image);

        let eligible = mf.has_cover_art && enable_media_file_cover_art;
        // Go's `ownInfo, ownResolved := mfInfos[mf.ID]` (`:113`). mfInfos is
        // fetched for the ELIGIBLE ids only (`:106`), so `Some(own)` already
        // IMPLIES eligible: an ineligible track can never have an entry here.
        let own = own_infos.get(&mf.id);

        // :112-116 — Go's compound `eligible && ownResolved && !ownInfo.Absent()`:
        // the `eligible` guard IS present here (it is the outer `if`), with the
        // resolved and not-absent checks nested inside, which is the same
        // short-circuit order Go's `&&` gives.
        if eligible {
            if let Some(own_info) = own {
                if !own_info.absent() {
                    mf.image = own_info.image();
                    continue;
                }
            }
        }

        // :117 — `ownWontResolve := !eligible || (ownResolved && ownInfo.Absent())`.
        let own_wont_resolve = !eligible || own.map(ItemArtworkInfo::absent).unwrap_or(false);

        // :120-125 — present-and-found album.
        if let Some(album) = album_infos.get(&mf.album_id).filter(|a| !a.absent()) {
            if mf.disc_number == 0 && own_wont_resolve {
                mf.image = album.image();
            }
            continue;
        }

        // :128-130 — disc art: keep the track requestable.
        if mf.disc_number > 0 {
            continue;
        }

        // :131-133 — the definitive-placeholder case only: the album entry must be
        // PRESENT as well as absent, so a track whose album has no state row at all
        // stays unresolved rather than absent.
        if album_infos
            .get(&mf.album_id)
            .is_some_and(|album| album.absent() && own_wont_resolve)
        {
            mf.image.image_absent = true;
        }
    }
    error
}

/// Accumulate a swallowed lookup failure: Go logs each one separately, so two
/// failures of one read are reported as two texts joined in call order.
fn record_artwork_error(slot: &mut Option<String>, error: Option<String>) {
    let Some(text) = error else { return };
    match slot {
        Some(existing) => {
            existing.push_str("; ");
            existing.push_str(&text);
        }
        None => *slot = Some(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_entry_projects_absent_not_a_hash() {
        // Arrange: the known-absent state (hash == "", model/artwork.go:79-80).
        let info = ItemArtworkInfo {
            item_id: "al-1".to_string(),
            ..Default::default()
        };
        // Act:
        let image = info.image();
        // Assert: absent, no hash, and the COALESCE zeros carried through.
        assert!(info.absent());
        assert_eq!(
            image,
            ItemImage {
                image_absent: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn a_found_entry_carries_every_joined_column() {
        // Arrange: a found hash plus the artwork join's columns.
        let info = ItemArtworkInfo {
            item_id: "al-1".to_string(),
            hash: "abcd".to_string(),
            blur_hash: "blur".to_string(),
            thumb_hash: "thumb".to_string(),
            dominant_color: "#0a0b0c".to_string(),
            width: 600,
            height: 400,
        };
        // Act:
        let image = info.image();
        // Assert: image_absent stays false (Image() derives it from Absent()).
        assert!(!info.absent());
        assert_eq!(
            image,
            ItemImage {
                image_hash: "abcd".to_string(),
                image_absent: false,
                blur_hash: "blur".to_string(),
                thumb_hash: "thumb".to_string(),
                dominant_color: "#0a0b0c".to_string(),
                image_width: 600,
                image_height: 400,
            }
        );
    }

    #[test]
    fn a_missing_entry_leaves_the_image_zero() {
        // Arrange: no entry at all (hydrateItemImages returned an empty map).
        let infos = HashMap::new();
        let mut image = ItemImage::default();
        // Act:
        apply_item_image(&infos, "al-1", &mut image);
        // Assert: unresolved is NOT absent (applyItemImage :71-75).
        assert_eq!(image, ItemImage::default());
        assert!(!image.image_absent);
    }
}
