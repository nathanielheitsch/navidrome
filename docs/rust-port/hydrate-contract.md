# Hydration / marshal contract for slice S-D (from lane S-C, 2026-11-11)

Produced read-only by Paseo agent `5c478af0-9a4e-4c66-85aa-8971c3977838` on
`pi/bifrost/openrouter/coding`, and spot-checked by the parent against the tree:
hook counts (8 `PostScan` / 5 `PostMapArgs`), the value-vs-pointer embed
difference (`dbPlaylist` embeds `model.Playlist` by value at
`playlist_repository.go:25` and `dbPlaylistTrack` embeds `dbMediaFile` by value
at `playlist_track_repository.go:23`, while `dbAlbum` embeds `*model.Album`),
the `playlist_track` ID overwrite at `playlist_track_repository.go:32`, and
`marshal_similar_artists` hand-escaping via `{:?}` at `artist_repo.rs:502-516`
are all verified.

**Parent-resolved open item.** The brief left one claim UNVERIFIED: whether
`newSelect` joins `annotation` for album/artist/mediafile base reads. It does
not. `newSelect` (`sql_base_repository.go:139-147`) is
`Select().From(r.tableName)` plus options/filters only. Annotation joins are
explicit and per-query (`album_repository.go:416`, `artist_repository.go:446`,
gated in `mediafile_repository.go:152`). Consequence for S-D: base reads must
not expect annotation columns, and only the queries that do join annotations
need the `coalesce(...)` guards that avoid Go's NULL-into-non-pointer error
(`playlist_track_repository.go:106-108` is the precedent).

The remaining UNVERIFIED items in section 7 are still open and must be
re-derived by whoever builds S-D, not assumed.

## 1. Hook inventory (grep-verified: `grep -rn "func.*PostScan" persistence/` = 8, `func.*PostMapArgs` = 5)

**PostScan (8):**

| Model | file:line | What it does |
|---|---|---|
| `dbAlbum` | `persistence/album_repository.go:41-69` | Decodes 4 JSON columns into the embedded `model.Album` + copies 2 shim fields. `Discs` (guard `a.Discs != ""`, :43) → `a.Album.Discs` (`map[int]string`), wrap `"parsing album discs from db: %w"`. `Participants` (no guard — `unmarshalParticipants` accepts empty string) → `a.Album.Participants`, wrap `"parsing album from db: %w"` (:50). `Tags` (guard `a.Tags != ""`, :52) → `a.Album.Tags`, same wrap, then derives `a.Genre, a.Genres = a.Album.Tags.ToGenres()` (:57). `FolderIDs` (guard `a.FolderIDs != ""`, :59) → `a.Album.FolderIDs []string`, wrap `"parsing album folder_ids from db: %w"`. Unconditional: `Album.RGAlbumGain/Peak = shim` (:66-67). |
| `dbArtist` | `persistence/artist_repository.go:43-98` | Always inits `Artist.Stats = make(...)` (:44). `LibraryStatsJSON` (guard `!= ""`, :46) → unmarshal `map[string]map[string]map[string]int64`, wrap `"parsing artist stats from db: %w"`; sums into `Stats` per role (`m`/`a`/`s` keys) and adds the `"total"` entry into `Artist.Size/SongCount/AlbumCount` (:63-67); `RoleInvalid` keys skipped (:70). `SimilarArtists` (guard `== ""` early-return at :84 after `SimilarArtists = nil` :83) → `[]dbSimilarArtist{id,name}`, wrap `"parsing similar artists from db: %w"`, mapped to `model.Artist{ID,Name}`. |
| `dbMediaFile` | `persistence/mediafile_repository.go:49-67` | Unconditional shim copies `RGTrackGain/Peak`, `RGAlbumGain/Peak` (:50-53). `Participants` (no guard) → `MediaFile.Participants`, wrap `"parsing media_file from db: %w"` (:57). `Tags` (guard `!= ""`, :59) → `MediaFile.Tags`, same wrap, then `m.Genre, m.Genres = m.MediaFile.Tags.ToGenres()` (:64). |
| `dbFolder` | `persistence/folder_repository.go:38-46` | `ImageFiles` (guard `!= ""`, :40) → `Folder.ImageFiles []string`, wrap `"parsing folder image files from db: %w"`. |
| `dbPlaylist` | `persistence/playlist_repository.go:29-34` | `Rules` (a `sql.NullString` struct field, `dbPlaylist` at :24-27) — guard `p.Rules.String != ""` (:30), `json.Unmarshal` into `p.Playlist.Rules` (`*criteria.Criteria`). **No error wrapping** — returns the raw `json.Unmarshal` error. |
| `dbPlaylistTrack` | `persistence/playlist_track_repository.go:27-34` | Delegates to `dbMediaFile.PostScan()` (:28), then `PlaylistTrack.MediaFile = *t.dbMediaFile.MediaFile` (:31) and **the overwrite: `PlaylistTrack.MediaFile.ID = t.MediaFileID`** (:32), because the row's own `id` is the track position. |
| `dbScrobbleBuffer` | `persistence/scrobble_buffer_repository.go:23-30` | Same pattern: delegates to `dbMediaFile.PostScan()` (:24), copies into `ScrobbleEntry.MediaFile` (:27), **overwrites `MediaFile.ID = t.MediaFileID`** (:28). Note `Next` then re-fetches participants separately (`getParticipants`, `scrobble_buffer_repository.go:81`) — the joined `m.*` gives a row without tag/participant columns, so that PostScan delegation runs over absent JSON columns (discarded, see §2). |
| `dbUser` | `persistence/user_repository.go:36-43` | `LibrariesJSON` (guard `!= ""`, :37) → `u.User.Libraries`, wrap `"parsing user libraries from db: %w"`. Confirms §1b's claim that user is **not** hook-free. |

**PostMapArgs (5):**

| Model | file:line | What it does to the args map |
|---|---|---|
| `dbAlbum` | `persistence/album_repository.go:71-97` | Sets `full_text` (Name, SortAlbumName, AlbumArtist, `Participants.AllNames()`, `maps.Values(Discs)`, `Tags[TagAlbumVersion]`, `Tags[TagCatalogNumber]` through `formatFullText`, :72-78), `search_participants` (space-joined `AllNames()`, :79), `search_normalized` (`str.NormalizeForFTS(Name, AlbumArtist)`, :80), `tags` (`marshalTags`), `participants` (`marshalParticipants`), `folder_ids` (JSON of `FolderIDs`, wrap `"marshalling album folder_ids: %w"`), `discs` (JSON of `Discs`, wrap `"marshalling album discs: %w"`). |
| `dbArtist` | `persistence/artist_repository.go:100-121` | Sets `similar_artists` (JSON `[{id,name}]`, Go's marshal error ignored :105), `full_text` (`formatFullText(Name, SortArtistName)`, :109), `search_normalized` (`NormalizeForFTS(Name)`, :110). **Deletes** `sort_artist_name` / `mbz_artist_id` from the map when absent or `""` (:114-119) — so an empty value never overwrites the stored one. |
| `dbFolder` | `persistence/folder_repository.go:48-59` | `image_files`: `"[]"` when `ImageFiles == nil` (:49-50) else JSON marshal (wrap `"marshalling image files: %w"`). |
| `dbMediaFile` | `persistence/mediafile_repository.go:69-80` | `full_text` (`FullTitle()`, Album, Artist, AlbumArtist, SortTitle, SortAlbumName, SortArtistName, SortAlbumArtistName, DiscSubtitle, + `AllNames()`, :70-74), `search_participants`, `search_normalized` (`NormalizeForFTS(FullTitle(), Album, Artist, AlbumArtist)`, :76), `tags`, `participants`. |
| `dbPlaylist` | `persistence/playlist_repository.go:36-51` | If `IsSmartPlaylist()`: sets `rules` (JSON of `Rules`, wrap `"invalid criteria expression: %w"` :41) and **deletes** `song_count`, `duration`, `size` (:44-46); else **deletes** `rules` (:49). |

Hook-free models (no invented hydration allowed): library, tag, genre, radio, share, scrobble, transcoding, property, artwork, artwork_queue, plugin, user_props, smart_playlist. `smart_playlist_repository.go:15-23` is correct: it's a `playlistRepository` method set, no repo type.

**Write funnel entry points** (for context): `toSQLArgs` is called from `sql_base_repository.go:507` (`updateOwned`), `:619` (`put`), `artwork_repository.go:45,129`, `player_repository.go:148`, `user_repository.go:123`.

## 2. Column → field map for the six hook-bearing entities

### dbx mechanics — vendor present, verified (not upstream-from-memory)

Vendored at `~/go/pkg/mod/github.com/pocketbase/dbx@v1.12.0` (`go.mod`: `github.com/pocketbase/dbx v1.12.0`). Verified:
- `db` tag is the column source: `DbTag = "db"` (`struct.go:48`); non-anonymous fields with no `db` tag fall back to `mapper(field.Name)` = `DefaultFieldMapFunc` (`struct.go:160-206`, `struct.go:69-71`, `db.go:90` — default, unchanged in Navidrome).
- `DefaultFieldMapFunc` regex is `([^A-Z_])([A-Z])` → `_` before an uppercase letter (e.g. `RGAlbumGain` → `rgalbum_gain` — I ran it: verified output). This is why the `RgAlbumGain` shims exist; the real column `rg_album_gain` only matches a field literally named `RgAlbumGain`.
- Recurses into non-scanner structs: `isNestedStruct` (`struct.go:222-227`) — struct and not `sql.Scanner`, except `time.Time`. So db structs recurse into the flattened model (`dbAlbum` → `Album` → embedded `Annotations`/`ItemImage`).
- Shallow-wins shadowing: `struct.go:203-206` — a deeper field registered first can be replaced by a shallower one (`len(path2) < len(existing.path)`).
- Nil embedded pointers initialized: `indirect` at `struct.go:249-257` does `reflect.New` on nil. Note `getField` (`struct.go:152-157`) calls `indirect` on **every intermediate** — that's how `m.MediaFile.Participants = ...` works in a PostScan on a zero-value `dbMediaFile`.
- Unknown columns discarded into `sql.NullString`: `rows.go:82-84` (and the slice path `rows.go:164-166`). So extra joined columns (e.g. `library_path`, `libraries_json`'s siblings, annotation joins) that have no matching field silently vanish.
- `PostScan` invoked once after `rows.Scan`: `rows.go:91-100`.
- Read call sites: `queryOne` → `One` at `sql_base_repository.go:381`; `queryWithStableResults` → `ScanStruct` per row at `:429`; `queryAll` → `All` at `:450`; `queryAllSlice` → `Column` at `:466`.

### Per entity

**album** — db struct `dbAlbum` (`album_repository.go:29-39`): `*model.Album structs:",flatten"` + shims `Discs/Participants/Tags/FolderIDs string structs:"-"` + `RgAlbumGain/RgAlbumPeak *float64 structs:"-"` (`album_repository.go:31-38`; comment at :35-36 is the authoritative explanation).
- `model.Album` (`model/album.go:15-73`): row columns = `structs` tags (no `db` tags in the model; the db mapper's snake_case fallback matches the same names, verified they coincide for all tagged fields except the RG names, which the shims handle). Embedded non-row structs: `Annotations` (`model/annotation.go:8-16` → `play_count`, `play_date`, `rating`, `rated_at`, `starred`, `starred_at`, `average_rating`), `ItemImage` (`model/artwork.go:29-40` → **all fields `structs:"-"` — none are row columns**). Shadowing: none of Album's own fields are shadowed (embedded names are disjoint). Note `Discs`/`Tags`/`Participants`/`FolderIDs` exist as both model fields (`album.go:40,66-67,51`) **and** flat shim fields in `dbAlbum` — the shallow `dbAlbum` string fields win (`struct.go:203-206`), so the column values land in the shims, not in the model's `Discs Discs`/`Tags Tags`/`Participants Participants` fields; PostScan copies them across.
- Write side: `structs.Map` flattens the model's `Tags`/`Participants`/`Discs`/`FolderIDs` into `tags`/`participants`/`discs`/`folder_ids` keys, then PostMapArgs **overwrites** those four keys with the JSON-serialized versions; the shims are `structs:"-"` so they never enter the map.

**artist** — `dbArtist` (`artist_repository.go:32-36`): `*model.Artist flatten` + `SimilarArtists`, `LibraryStatsJSON` (both `structs:"-"`). `model.Artist` (`model/artist.go:14-47`): row columns per `structs` tags; `Stats`, `Size`, `AlbumCount`, `SongCount` are `structs:"-"` (`artist.go:27-30`) — **they only come from the `library_stats_json` SELECT alias** (`artist_repository.go:199-203` region: `JSON_GROUP_OBJECT(library_artist.library_id, JSONB(library_artist.stats)) as library_stats_json`), hydrated by PostScan. `SimilarArtists` is `structs:"similar_artists"` in the model (`artist.go:38`) **and** `structs:"-"` in the shim — shallow shim wins, model field never scanned directly. Note the model field tag makes it a *column* name candidate but the shim shadowing suppresses it.

**mediafile** — `dbMediaFile` (`mediafile_repository.go:29-39`): `*model.MediaFile flatten` + `Participants`, `Tags` shims + 4 `*float64` RG shims (`:35-38`; comment :33-34). `model.MediaFile` (`model/mediafile.go:27-107`): embedded `Annotations` (`:28`), `Bookmarkable` (`:29`, `model/bookmark.go:8-10` → `bookmark_position` is `structs:"-"` — no row column), `ItemImage` (`:30`, no columns), `AlbumImage` (`:34`, `structs:"-"`, hydrated elsewhere). Same shim-shadowing as album for `tags`/`participants`; RG columns via shims. `AlbumArtistID`/`ArtistID` are real columns (`mediafile.go:45,48`).

**folder** — `dbFolder` (`folder_repository.go:25-28`): `*model.Folder flatten` + `ImageFiles string structs:"-"` shadowing `Folder.ImageFiles []string` (`model/folder.go:18`). **Caveat:** `model/folder.go:25` tags `UpdateAt time.Time structs:"updated_at"` — the Go field is named `UpdateAt` (typo-ish) mapping to column `updated_at`. Also `folder_repository.go:75` selects `library.path as library_path` — no matching field, discarded per `rows.go:84`.

**playlist** — `dbPlaylist` (`playlist_repository.go:24-27`): `model.Playlist structs:",flatten"` **by value, not pointer** + `Rules sql.NullString structs:"-"` shadowing the model's `Rules *criteria.Criteria structs:"rules"` (`model/playlist.go:43`). Because the shadow is a non-scanner struct, dbx recurses? No — `sql.NullString` **is** a `sql.Scanner`, so it's a leaf; the shallow field wins and the column lands in `Rules.NullString`; PostScan unmarshals. `model.Playlist` row columns per `playlist.go:24-45`; `OwnerName`, `Tracks` are `structs:"-"`.

**playlist_track** — `dbPlaylistTrack` (`playlist_track_repository.go:22-25`): `dbMediaFile` (embedded **value**, non-pointer) + `*model.PlaylistTrack flatten`. `model.PlaylistTrack` (`model/playlist.go:226-231`): `ID`, `MediaFileID`, `PlaylistID` + embedded `MediaFile` **by value**. Shadowing is extensive: every media_file column maps to `dbPlaylistTrack.MediaFile.<field>` (depth 2) and nothing shallower shadows them; `id` maps to the track position (depth-1 `PlaylistTrack.ID` beats depth-2 `MediaFile.ID` — the shallow-wins rule is exactly why `:32` then overwrites the media file id). The Read select at `:100-116` emits `f.*` **before** `playlist_tracks.*`, plus coalesced annotation columns.

## 3. NULL semantics

Go delegates to `database/sql.convertAssignRows` (`$GOROOT/src/database/sql/convert.go`, Go 1.24): NULL → non-pointer numeric **errors** (`"converting NULL to %s is unsupported"`, `convert.go:499,511,523`); NULL → string errors at `:535`; NULL → pointer field `dv.SetZero()` = nil pointer (`convert.go:495-498`); NULL → non-pointer `time.Time` falls through the kind switch and errors at `:546` (`"unsupported Scan, storing driver.Value type ... into type ..."`). So: **every non-pointer column of these entities must be NOT NULL in practice, or the query errors; NULL in a pointer column = nil.**

Pointer fields per entity (model file:line):
- **album**: `RGAlbumGain *float64` `model/album.go:53`, `RGAlbumPeak *float64` :54, `ExternalInfoUpdatedAt *time.Time` :62. (Doc §1b's "album.go:53-54,62" — **confirmed**.)
- **artist**: `ExternalInfoUpdatedAt` `model/artist.go:39`, `CreatedAt *time.Time` :45, `UpdatedAt *time.Time` :46. (§1b "39,45-46" — **confirmed**.)
- **mediafile**: `BitDepth *int` `model/mediafile.go:67`, `BPM *int` :84, `RGAlbumGain/Peak` :95-96, `RGTrackGain/Peak` :97-98. (§1b "67,84,95-98" — **confirmed**.)
- **playlist**: `Rules *criteria.Criteria` `model/playlist.go:43`, `EvaluatedAt *time.Time` :44. (§1b "44" — **confirmed**.)
- **user**: `LastLoginAt`/`LastAccessAt *time.Time` `model/user.go:16-17` (**confirmed**).
- **share**: `ExpiresAt`/`LastVisitedAt *time.Time` `model/share.go:19-20` (**confirmed**).
- **annotations (embedded in album, artist, mediafile, playlist)**: `PlayDate`/`RatedAt`/`StarredAt *time.Time` `model/annotation.go:10,12,14` (**confirmed**). Note `rating`/`starred`/`play_count` are non-pointer — Go's track reads coalesce them (`playlist_track_repository.go:106-108` `coalesce(starred,0) as starred` etc.), and playlist_tracks' annotation join is a LEFT JOIN, so **any select that joins annotations for these entities must coalesce or it NULL-errors on rows without annotations** (annotation rows are per-user; admin/no-user selects rely on defaults). The album/artist/mediafile base selects do not join annotation — UNVERIFIED whether `newSelect` does (see §7).
- `model/artwork.go:66-67` (`attempted_at`/`updated_at` non-pointer with the explicit "cannot scan NULL" comment) — **confirmed**, context only.

Rust consequence: the mapper must read NULL into `Option<T>` for exactly the fields above and `NOT NULL` (or coalesced) columns as bare values — matching Go means NULL on a non-nullable column should be an **error**, not a silent zero.

## 4. Write funnel → Rust target

Go: `toSQLArgs` (`persistence/helpers.go:20-42`) — `structs.Map(rec)` (:21), one per-value switch: `*time.Time` non-nil → deref to `time.Time` (:24-27), **nil stays nil** (binds NULL); `driver.Valuer` → `Value()` with error propagation (:28-33); then `PostMapArgs` on the whole map if implemented (:36-40). Two consumers: `updateOwned` (`sql_base_repository.go:507`) and `put` (`:619`), both feeding squirrel `SetMap`.

squirrel: `SetMap` (`squirrel@v1.5.4/insert.go:271-283`, vendored in module cache — verified) sorts the column names (`sort.Strings(cols)`, :278) and emits values positionally against that sorted order. Go `put` appends `id` to the values map (`sql_base_repository.go:637-641`); `filterUpdateValues` re-adds `id` for the update leg (`sql_base_repository.go:606-609`); `selectUpdateColumns` always drops `created_at`/`birth_time` and filters to `colsToUpdate` (`:587-604`).

**The invariant established in `8c89aeb9`: columns and args are one unit, emitted together; `id` is appended to both (last in both), never prepended.** Already-correct Rust precedents: `album_repo.rs:493-528` `put_legs` (insert cols `.chain(["id"])` :509-512, args `.push(id)` :516-517, with the doc comment pinning `squirrel@v1.5.4/insert.go:271-283` and `sql_base_repository.go:637-641`); `mediafile_repo.rs:541-571` (same shape, `:556-566`); `folder_repo.rs:334-366` (`:347-356`); `playlist_repo.rs:502-545` (`:532-543`, plus its doc note that `INSERT … SET` is the rejected MySQL form); `artist_repo.rs:600` calls `put_cols` (`:524-591`, which **itself implements the artist PostMapArgs** — similar_artists marshal, full_text/search_normalized, and the empty `sort_artist_name`/`mbz_artist_id` retain at :583-586) and `:610-630` builds the legs with the same append-id rule. Pinned by executed tests in `tests/runtime_insert_alignment.rs:127,161,188,227`.

Rust target for S-D: `entity -> (Vec<String> columns, Vec<Value> values)` where columns are the sorted column names (sorted *after* applying the entity's PostMapArgs — Go sorts the final map keys, so derived/deleted keys participate in the sort; e.g. artist's deletions remove keys before sorting), values positional, and the repo layer appends `id` last to both, exactly as `put_legs` does. Note `Value` is `crate::sqlrepo::Value` (`sqlrepo.rs`), with `time::optional_col` (`time.rs:132`) producing `(column, Value::Null)` for nil `Option<DateTime<Utc>>` — the seam's nil-time handling already has a home.

## 5. Which existing pub fns consume the seam (call-site map, no edits proposed)

**album** — `album_repo.rs:459` `pub fn put_sql(id: &str, columns: &[String], values: Vec<Value>, participants_json: &str, indexed_tag_ids: Vec<String>) -> PutAlbumSql`; `album_repo.rs:530` `pub fn update_external_info_sql(id, columns, values)`. Both take caller-marshalled columns/values; the seam replaces the caller side. `participants_json`/`indexed_tag_ids` are S-A outputs (`sql_participations` marshaller + `sql_tags.rs:129` `marshal_tags` + `crate::sql_tags::indexed_tag_ids`).

**artist** — already half-wired to a record: `artist_repo.rs:524` `pub fn put_cols(artist: &ArtistRecord) -> Vec<(String, Value)>` and `:600` `pub fn put_sql(artist: &ArtistRecord, now: DateTime<Utc>, cols_to_update: &[&str])`. S-D's seam should generalize/subsume `put_cols` (it is the one entity already doing its own PostMapArgs, including the deletions). Note `ArtistRecord` (`artist_repo.rs:136`) carries `similar_artists: String` pre-marshalled (:157-159) — `marshal_similar_artists` at `:502`.

**mediafile** — `mediafile_repo.rs:498` `pub fn put_sql(id, columns, values, participants_json: &str, indexed_tag_ids: Vec<String>) -> PutMediaFileSql`; the `FtsColumns` DTO at `:459-468` is what S-B's feeders produce. `put_match_sql` (`:441-452`) is unaffected (probe only).

**folder** — `folder_repo.rs:334` `pub fn put_sql(id: &str, columns: &[String], values: Vec<Value>) -> PutFolderSql`. The image_files JSON marshal (Go `folder_repository.go:48-59`, `"[]"` for nil) has no Rust home yet — seam's PostMapArgs hook slot.

**playlist** — `playlist_repo.rs:473` `pub fn put_smart_cols(rules_json: String) -> Vec<Col>` and `:502` `pub fn put_sql(id, cols: &[Col], all_cols: &[Col], cols_to_update: &[&str])`. The `Col` type (:448-460) is playlist's local `(String, Value)`. PostMapArgs behaviour (drop `rules` vs drop counters `SMART_OWNED_COUNTER_COLS` :486) currently happens in the **caller** assembling `cols` — seam's hook slot.

**playlist_track** — write path is positional (`insert_sql` at `playlist_track_repo.rs:241`, `add_tracks_sql`), no per-row entity marshal; the seam only touches its **read** side (hydration with the ID overwrite).

**Read side (hydration) precedents** — the three hand-written mappers §1 lists, re-verified: `user_repo.rs:857` `row_to_user` (+ `UserRow` :542, `into_record` :549, call site :674), `player_repo.rs:1073` `row_to_player`, `share_repo.rs:477` `row_to_share` (call site :319). S-D's row→entity mapper is the shared generalization of these; `user_repo`'s `UserRow → UserRecord` split is the exact precedent for "raw row + hook payload → hydrated record" (mirrors `dbUser`/`dbScrobbleBuffer`).

## 6. Seam shape recommendation

Keep it as plain functions over `rusqlite::Row` in a new `nd-db/src/hydrate.rs` (S-D owns that file + one `lib.rs` module line per §1c), no generics ceremony required. Concretely:

1. **Row reader**: a column-oriented accessor, e.g.
   ```rust
   pub trait RowGet { // or a struct wrapping &rusqlite::Row
       fn get_text(&self, col: &str) -> rusqlite::Result<Option<String>>;
       fn get_int(&self, col: &str) -> rusqlite::Result<i64>;      // NULL → error (Go parity, convert.go:499)
       fn get_i32 / get_f32 / get_f64 / get_bool ...
       fn get_opt_text / get_opt_i64 / get_opt_f64(&self, col) -> ...Result<Option<T>>; // pointer fields
       fn get_opt_dt(&self, col) -> ...Result<Option<DateTime<Utc>>>;  // route through time::optional_sentinel (time.rs:118) for imported_at-style sentinels
   }
   ```
   NULL-into-non-optional must **error**, matching Go; only the §3 pointer set gets `_opt`.
2. **Per-entity hydrator**, one per hook-bearing entity, each: map columns→fields per §2 (shim fields read from their column, unknown columns simply never read — Go discards them via `rows.go:84`, so absence is a no-op), then call that entity's **PostScan hook as a closure**:
   ```rust
   pub struct Hooks<T> { pub post_scan: fn(&mut T) -> Result<(), HydrateError> /* per-entity */ }
   ```
   with the hooks in `hydrate.rs` (or entity modules) as `pub fn album_post_scan(row, album: &mut AlbumRecord) -> ...` etc., doing exactly what the Go hooks do — including the two *delegating* hooks: `playlist_track_post_scan` and `scrobble_buffer_post_scan` both call the **shared mediafile hook first**, then apply the overwrite:
   ```rust
   track.media_file.id = track.media_file_id;  // playlist_track_repository.go:32
   entry.media_file.id = entry.media_file_id;  // scrobble_buffer_repository.go:28
   ```
   The overwrite is just data movement *after* the delegation, so it needs no special mechanism — a two-step hydrator (`hydrate_mediafile` then overwrite) is faithful and testable. The hooks must preserve Go's guards exactly: `!= ""` before unmarshal for `discs`/`tags`/`folder_ids`/`image_files`/`libraries_json`/`rules`/`library_stats_json`/`similar_artists`; **no** guard for `participants` (Go calls `unmarshalParticipants("")` and it succeeds).
3. **Marshaller**, per entity:
   ```rust
   pub fn album_row(e: &AlbumRecord) -> (Vec<String>, Vec<Value>)  // returns sorted cols, positional values, id NOT included
   ```
   implementing PostMapArgs inside: compute derived columns, marshal JSON columns, apply the deletions (artist's `sort_artist_name`/`mbz_artist_id`; playlist's `rules`-vs-counters), then sort the keys (Go sorts the **final** map; use a `BTreeMap` before converting to pairs so the ordering matches `sort.Strings`). The repo's `put_legs` then appends `id` last to both — untouched.
4. **DTOs local to nd-db**: confirmed precedent — `UserRecord` (`user_repo.rs:498`), `PlayerRecord` (`player_repo.rs:207`), `RadioRecord` (`radio_repo.rs:124`), `Tag` (`tag_repo.rs:41`), `Scrobble` (`scrobble_repo.rs:31`), `Transcoding` (`transcoding_repo.rs:31`), `Genre` (`genre_repo.rs:28`), `ScrobbleEntry` (`scrobble_buffer_repo.rs:31`) all live in nd-db; `nd-model` (issue #06's crate) contains none of these and must not gain them. The new records (`AlbumRecord`, `ArtistRecord`-generalized, `MediaFileRecord`, `FolderRecord`, `PlaylistRecord`, `PlaylistTrackRecord`) go in `hydrate.rs` or the entity repo modules.
5. **JSON marshalling inside the hooks** must reuse: `sql_tags::marshal_tags` (`sql_tags.rs:129`, faithful, currently unused in production) for album/mediafile `tags`; S-A's participant marshaller for `participants`; a new discs marshaler for `map[int]string` that **stringifies keys before sorting** (Go `json.Marshal(map[int]string)` sorts keys as strings — §1b trap 2; a `BTreeMap<i32, _>` emits `1,2,10` where Go emits `1,10,2`); a plain `[]string` marshal for `folder_ids`/`image_files` (`"[]"` for empty).

## 7. Risks, ordering, and §1b/1c cross-check

**JSON traps (both confirmed relevant to S-D):**
- Integer-key ordering: album `discs` JSON is the live instance. Go verified in §1b (`{"1","10","2"}` string order); the Rust discs marshaler must stringify-then-sort.
- Participant ordering: `AllNames()` (`model/participants.go:174-185`) iterates `map[Role]ParticipantList` (type at `:94`) — non-deterministic role order, deduped by `slice.Unique` preserving first occurrence. Safe for `full_text` (sorted downstream by `SanitizeStrings`) but `search_participants` bytes depend on iteration order → **persist no inherited ordering** (same rule as #16, §7-item 7 of the doc). `marshalParticipants` (`sql_participations.go:50-65`) likewise marshals the map — role key order is sorted by encoding/json (map keys sort as strings: `albumartist` < `artist` < `composer`...), but **within a role the list order is whatever `model.Participants` held**. S-A's output (a `&[(String,String)]`-free, order-explicit representation) is what the seam consumes; the seam must not reorder.
- Escaping: decision already made in §1b (match shape + round-trip, not Go's escaped bytes) — `marshal_similar_artists` (`artist_repo.rs:502-516`) already hand-escapes with `{:?}`, which does *not* match Go's `<`,`>`,`&`,U+2028/2029 escaping; per the documented decision that's fine, but the seam should use **one** JSON encoder for all hook JSON rather than per-call-site hand-escaping. UNVERIFIED whether the S-A participant golden was byte-compared or parsed (doc says parsed).

**S-A / S-B output the seam must consume:**
- S-A (`nd-db/src/sql_participations.rs` + `tests/participants_json.rs` + `testdata/go_participants/`): a `marshalParticipants`/`unmarshalParticipants` pair. Current state verified: `sql_participations.rs` has only filter/subquery fns (`participant_id_column` :40, `participant_ids_subquery` :48, `with_participant_roles` :66, `participant_id_filter` :87, `not_participant_id_filter` :94) — no marshaler yet, so **S-A is not landed in this checkout** (HEAD `336f69ba` is docs-only since `12e270f7`). The seam's `participants` column reads/writes must be written *against* S-A's API once it lands (S-D is explicitly ordered after S-A+S-B per §1c).
- S-B (new `nd-db/src/sql_feeders.rs` + `tests/search_feeders.rs`): `full_text`/`search_participants`/`search_normalized` producers able to express album's 7 input groups (`album_repository.go:72-80`) and mediafile's (`mediafile_repository.go:70-76`), replacing artist-only `format_full_text`/`search_normalized` (`artist_repo.rs:419,425` — §1b's correction of `:412,418` is **confirmed**: 419 and 425 are the fns, 412 an attribute). S-D's mediafile/album marshallers take feeder output as `FtsColumns`-shaped input.

**Contradictions / confirmations vs next-work.md §1b-§1c:**
- §1b "8 PostScan / 5 PostMapArgs" — **confirmed**, exact sets above; the `user_repository.go:36-42` hook is real (doc says :36-42; actual fn spans :36-43, off-by-one on the closing brace, immaterial).
- §1b `scrobble_buffer_repository.go:23` delegation + ID overwrite — **confirmed** (:23-30; overwrite at :28).
- §1b `toSQLArgs` `helpers.go:20-42` — **confirmed** exactly.
- §1b `dbx@v1.12.0/rows.go:65-104, struct.go:160-206,249-257` — **confirmed** against the vendored module (db tag `struct.go:48`; build/shadow `160-206` incl. shadow at 203-206; `indirect` 249-257; ScanStruct 65-101 incl. NullString discard 82-84 and PostScan 91-100). The doc's line 104 upper bound is loose (fn ends at :101) — harmless.
- §1b `sql_base_repository.go:411/429/441/460` read funnel — **confirmed** (`:411` `queryWithStableResults`, `:429` `ScanStruct`, `:441` `queryAll`, `:460` `queryAllSlice`; the `All` call is at :450, `Column` at :466, matching §1's `:429,450,466`).
- §1b pointer-field line refs — **all confirmed** (album 53-54,62; mediafile 67,84,95-98; artist 39,45-46; playlist 44; user 16-17; share 19-20; annotation 10-14).
- §1b "integer-key ordering verified `{"1","10","2"}`" and "participant ordering unstable" — I did not re-run the Go programs; taken from the doc's stated empirical verification. **UNVERIFIED-by-me, doc-asserted.**
- §1b "dbx discards unknown columns" — confirmed, with one doc-level nuance worth pinning in S-D: discards are by *field-name map*, so a **joined query with an extra aliased column** (e.g. `library_path`, `libraries_json` has a home, `owner_name` doesn't for playlist) silently drops only the columns with no matching field.
- §1c slice table matches the tree: `sql_participations.rs` exists with no marshaler (S-A not landed), no `sql_feeders.rs` (S-B not landed), no `hydrate.rs` (S-D pending) — **consistent**.
- One thing §1b does *not* state that S-D needs: **`dbPlaylist` embeds `model.Playlist` by value and `dbPlaylistTrack` embeds `dbMediaFile` by value** (`playlist_repository.go:25`, `playlist_track_repository.go:23`) — the "initialize nil embedded pointers" rule doesn't help these; PostScan mutates through the value embed, and the Rust hydrator must construct the nested record before running the hook. Also `album`/`artist`/`mediafile`/`folder`/`user` embed **pointers** (`*model.Album` etc.) that dbx allocates lazily — the Rust equivalent is "construct, then hook", same order.
- **UNVERIFIED**: whether `newSelect` (the shared SELECT builder, `sql_base_repository.go`) joins `annotation` for album/artist/mediafile base reads — the track read does explicitly (`playlist_track_repository.go:101-116`) and coalesces, but I did not trace every album/artist/mediafile select's annotation join; S-D must enumerate the actual SELECT column sets per repo when writing hydrators, not assume `*`.
- **UNVERIFIED**: exact `new_tag_id`/`nd_id` call the tag hook uses — `sql_tags.rs:129` uses `nd_id::new_tag_id` per its own doc comment (:126-128); I did not re-derive tag ids.

**VERIFIED vs UNVERIFIED tally:**
- VERIFIED: all 13 hook locations and bodies; dbx vendored source lines (rows.go:65-101, struct.go:48,152-206,222-257,69-71); DefaultFieldMapFunc behaviour (executed); `toSQLArgs` (helpers.go:20-42); squirrel SetMap (insert.go:271-283, executed-by-read); `put`/`selectUpdateColumns`/`filterUpdateValues` (sql_base_repository.go:587-641); convert.go NULL semantics (489-546); all model pointer-field line refs; shadowing/shim mechanisms (album/artist/mediafile/folder/playlist/playlist_track); all Rust put_sql/put_legs line refs and the `8c89aeb9` append-id invariant (incl. tests/runtime_insert_alignment.rs:127-227); `time::optional_col`/`optional_sentinel` (time.rs:118,132); DTO-local-to-nd-db precedent (all 8 structs); S-A/S-B not-yet-landed state at HEAD `336f69ba`; `format_full_text`/`search_normalized` at artist_repo.rs:419,425; §1b/1c consistency.
- UNVERIFIED (flagged, not inferred): §1b's two empirical Go JSON orderings (integer-key and participant-order traps — doc-asserted, not re-run); per-repo annotation-join coverage in base SELECTs; `nd_id::new_tag_id` id derivation; whether S-A's golden comparison is byte- or parse-based.