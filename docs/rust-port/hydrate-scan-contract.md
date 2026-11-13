# S-D3 hydration brief — row → entity records and PostScan hooks

Produced read-only by Paseo scout `4cc1a1dc-ccfb-4781-ba4a-20ac9cc00064` on
`pi/bifrost/openrouter/coding`, tree-verified at HEAD `60198cec`; the parent
independently confirmed the two corrections below in `album_repository.go:230`,
`mediafile_repository.go:207-208`, `artist_repository.go:214`,
`playlist_repository.go:97/218/260`, `sql_annotations.go:63-91`, and
`sql_bookmarks.go:18-28`. This supersedes the annotation-join claim in
`hydrate-contract.md` §"Parent-resolved open item" and replaces the stale
"S-A is not landed" item in that document's §7.

Two corrections change the plan: **base reads do join `annotation`**, and the
mediafile base read **also joins `bookmark`**. Therefore the shared row mapper
must read exactly the columns each repo's SELECT emits and treat absent optional
columns (annotation, bookmark, artwork) as zero/None — it cannot assume a fixed
column set per entity.

---

# S-D3 brief — row → entity hydration + PostScan hooks (read-only scout)

**Verdict: docs are directionally right but contain two material corrections at HEAD 60198cec that change what the hydrators must decode.** Everything else in `hydrate-contract.md` re-verifies. No edits made.

## ⚠️ Two corrections that change the plan (both verified in source)

1. **Base reads DO join `annotation` unconditionally.** The doc's parent-resolved item (hydrate-contract §top, §7) says "`newSelect` is not the annotation join; base reads must not expect annotation columns." **This is wrong at HEAD.** The *base entity SELECTs* each call `r.withAnnotation(...)` on an unguarded line:
   - `persistence/album_repository.go:230` — `sql = r.withAnnotation(ctx, sql, "album.id")` (inside `selectAlbum`)
   - `persistence/mediafile_repository.go:207` — `sql = r.withAnnotation(ctx, sql, "media_file.id")` (inside `selectMediaFile`)
   - `persistence/artist_repository.go:214` — `return r.withAnnotation(ctx, query, "artist.id")` (inside `selectArtist`)

   `withAnnotation` (`persistence/sql_annotations.go:63-91`) LEFT JOINs `annotation` and appends columns `coalesce(starred,0) as starred`, `coalesce(rating,0) as rating`, `starred_at`, `play_date`, `rated_at`, a `coalesce(play_count,0) as play_count` (or the `AlbumPlayCountModeNormalized` rounded form), and `<table>.average_rating`. **Consequence: the album/mediafile/artist/playlist hydrators must decode the 6 annotation columns on every base read** (the non-pointer ones are `coalesce`d so non-NULL → bare reads; the pointer ones → `Option`). The *cited* `album_repository.go:416`/`artist_repository.go:446` are the `RefreshPlayCounts` UPDATE statement's own `join annotation`, a different site. The doc conflated them.

2. **`mediafile` base read also adds a `bookmark` column.** `withBookmark` (`persistence/sql_bookmarks.go:18-28`, called at `mediafile_repository.go:208`) LEFT JOINs `bookmark` and appends `coalesce(position, 0) as bookmark_position`. That column must be read (into `Bookmarkable.BookmarkPosition`) on every mediafile base read. `mediafile_repository.go` `GetAll` reads it; `getRandomHydrate`/`putByMatch` paths don't — the hydrator must be tolerant of the column's *absence* on the light queries (dbx discards unknown/absent columns into a throwaway `NullString`, `rows.go:84`).

Both corrections mean: **the "shared row mapper" cannot assume a fixed column set per entity** — it must read exactly the columns each repo's SELECT emits and treat absent optional columns (bookmark, annotation, artwork) as zero/None.

---

## 1. Complete `PostScan` inventory (8) — all verified file:line at HEAD

| # | Model | file:line | Column reads | DB/other-repo side effects? |
|---|---|---|---|---|
| 1 | `dbAlbum` | `persistence/album_repository.go:41-69` | `discs`, `participants`, `tags`, `folder_ids` (JSON) + `rg_album_gain`, `rg_album_peak` (float) | **None** — pure column decode |
| 2 | `dbArtist` | `persistence/artist_repository.go:43-98` | `library_stats_json`, `similar_artists` (JSON) | **None** — but reads `library_stats_json`, a *SELECT-alias* column (not a stored column) built at `artist_repository.go:201` via `JSON_GROUP_OBJECT(library_artist.library_id, JSONB(library_artist.stats))` |
| 3 | `dbMediaFile` | `persistence/mediafile_repository.go:49-67` | `participants`, `tags` (JSON) + 4 `rg_*` floats | **None** in PostScan. (The `getParticipants` repo query is a *separate*, PostScan-independent call — see §2c.) |
| 4 | `dbFolder` | `persistence/folder_repository.go:38-46` | `image_files` (JSON) | **None** |
| 5 | `dbPlaylist` | `persistence/playlist_repository.go:29-34` | `rules` (`sql.NullString` shim) | **None** |
| 6 | `dbPlaylistTrack` | `persistence/playlist_track_repository.go:27-34` | delegates to (3), then overwrites | **None** — the `annotation` join on `Read` is coalesced *in SQL*, not in the hook |
| 7 | `dbScrobbleBuffer` | `persistence/scrobble_buffer_repository.go:23-30` | delegates to (3), then overwrites | **None** in PostScan. The separate `getParticipants` re-fetch at `:84` is outside PostScan (see §2c). |
| 8 | `dbUser` | `persistence/user_repository.go:36-43` | `libraries_json` (JSON) | **None** — reads the *SELECT-alias* column built at `user_repository.go:76-84` (`json_group_array(json_object(...)) AS libraries_json`) |

**Pure-column vs DB-dependent split:** All 8 hooks are **pure column decoders** — none of them issues a query or touches another repository. The two genuinely DB-dependent reads are **not** PostScan and must be kept out of the hook:
- `getParticipants` (`persistence/sql_participations.go:127-…`) — an `artistRepository.GetAll` by `Participants.AllIDs()`. Invoked explicitly from `mediafile_repository.go:228` (`GetWithParticipants`) and `scrobble_buffer_repository.go:84` (`Next`), **after** the PostScan delegation has run over the joined `m.*` (which lacks `participants`/`tags` columns → those fields are decoded as absent and then overwritten).
- `hydrateArtwork` / `hydrateItems` (`album_repository.go:253,257`, `mediafile_repository.go:240,244`, `artist_repository.go:265,277`) — a batched `item_artwork` lookup that fills `ItemImage` (`structs:"-"`, never a row column). **This is a *second* hydration layer, separate from PostScan.** S-D3's scope is the PostScan hook contract; artwork hydration is a *different* batched read and should be its own sub-slice, **not** folded into `hydrate.rs`.

There are **no dbx row hooks on slice types** (`dbAlbums`, `dbMediaFiles`, `dbArtists`, `dbPlaylistTracks` — e.g. `persistence/album_repository.go:101 toModels`) — those are plain `toModels()` converters, not `PostScan`; dbx's slice path (`rows.go:164-166`) scans each element and invokes the element's `PostScan`.

## 2. Exact per-hook column read, guards, prefixes, written fields

### 2a. album — `album_repository.go:41-69`
```go
if a.Discs != "" {
    if err = json.Unmarshal([]byte(a.Discs), &a.Album.Discs); err != nil {
        return fmt.Errorf("parsing album discs from db: %w", err)
```
- `discs` (guard `!= ""`) → `Album.Discs` (`map[int]string`); wrap `"parsing album discs from db: %w"`. Empty/NULL → leaves `Discs` at zero (`nil`).
- `participants` (**no guard**) → `a.Album.Participants, err = unmarshalParticipants(a.Participants)`; wrap `"parsing album from db: %w"` (`:50`). `unmarshalParticipants("")` **errors** in Go (`json.Unmarshal` of empty input) — but a base read always has the `participants` column with a `jsonb default '{}'` DDL, so it is never `""`; a `NULL` (only possible on a non-joined leg) is a hard error. Zero-value: `nil` map.
- `tags` (guard `!= ""`) → `unmarshalTags`; wrap `"parsing album from db: %w"`; **then** `a.Genre, a.Genres = a.Album.Tags.ToGenres()` (`:57`). Empty/NULL → `Tags` stays nil, `Genre=""`, `Genres=nil`.
- `folder_ids` (guard `!= ""`) → `FolderIDs []string`; wrap `"parsing album folder_ids from db: %w"`.
- Unconditional `a.Album.RGAlbumGain = a.RgAlbumGain` / `RGAlbumPeak = a.RgAlbumPeak` (`:66-67`) — `*float64` shims → model pointer fields; `NULL` → `nil`.

### 2b. artist — `artist_repository.go:43-98`
- Always `a.Artist.Stats = make(map[model.Role]model.ArtistStats)` (`:44`).
- `library_stats_json` (guard `!= ""`) → `map[string]map[string]map[string]int64`; wrap `"parsing artist stats from db: %w"`. Sums the `"m"/"a"/"s"` per role into `Stats`; the `"total"` key adds into `Artist.Size/SongCount/AlbumCount` (`:63-67`); `model.RoleInvalid` keys are skipped (`:70`). Empty/NULL → `Stats` empty map, Size/Count zero. **`Stats/Size/SongCount/AlbumCount` are `structs:"-"`** — they exist *only* via this alias column.
- `similar_artists`: `a.Artist.SimilarArtists = nil` (`:83`), then `if a.SimilarArtists == "" { return nil }` (`:84`). Else unmarshal `[]dbSimilarArtist{id,name}`; wrap `"parsing similar artists from db: %w"`.

### 2c. mediafile — `mediafile_repository.go:49-67`
- Unconditional `m.RGTrackGain/…/RGAlbumPeak = m.Rg…` (`:50-53`) — 4 `*float64`.
- `participants` (**no guard**) → `m.MediaFile.Participants, err = unmarshalParticipants(m.Participants)`; wrap `"parsing media_file from db: %w"` (`:57`).
- `tags` (guard `!= ""`) → `m.MediaFile.Tags`; wrap `"parsing media_file from db: %w"`; **then** `m.Genre, m.Genres = m.MediaFile.Tags.ToGenres()` (`:64`).
- **`lyrics` is NOT decoded by PostScan** — `MediaFile.Lyrics string` (`mediafile.go:83`, `structs:"lyrics"`). It is a flat `string` row column scanned directly by dbx, *not* a PostScan field. The Rust record must carry it as a plain column, not a hook. `StructuredLyrics()` (`:161`) is a *model* method, not persistence. **Gap risk: a naive port might treat lyrics like tags.**

### 2d. folder — `folder_repository.go:38-46`
- `image_files` (guard `!= ""`) → `Folder.ImageFiles []string`; wrap `"parsing folder image files from db: %w"`.
- **Correction (landed): `Folder.UpdateAt` is NOT hydrated by the base read.** `ScanStruct` maps columns to struct fields by *field name* through `DefaultFieldMapFunc` (`dbx@v1.12.0/rows.go:60-85`); `UpdateAt` snake-cases to `update_at`, and `selectFolder` emits `updated_at` (the column, from `structs:"updated_at"`, `model/folder.go:32`). There is no `update_at` column, so the alias falls into dbx's throwaway `NullString` (`rows.go:84`) and Go's `Folder.UpdateAt` stays `time.Time{}` on **every** read (verified against the pinned dbx: a row holding `'2024-02-02 00:00:00+00:00'` in `updated_at` scans back `isZero() == true`). The `structs` tag governs only the WRITE map (`structs.Map`), so `put` still writes `updated_at`. The Rust seam reproduces both halves: `folder_row` emits `updated_at`, `row_to_folder` hydrates `go_zero_time()`.
- `Folder.ID` (`structs:"id"`) snake-cases to the emitted `id` and IS scanned: the record keeps each row's own id, including on bulk reads, and `folder_row` keeps `id` out of the write map (the repo appends it, like Go's `put(ctx, id, …)`).

### 2e. playlist — `playlist_repository.go:29-34`
- `rules` is a `sql.NullString` shim (`:26`), shadowing the model's `Rules *criteria.Criteria`. Guard `p.Rules.String != ""` (`:30`); `return json.Unmarshal([]byte(p.Rules.String), &p.Playlist.Rules)`. **No error wrapping** — the raw `json.Unmarshal` error is returned directly. `NULL` → `NullString.String==""` → skipped → `Rules` stays nil. ~~Note: the base read's `owner_name` (`user.user_name as owner_name`, `:259`) has no matching field → **discarded** by dbx (`rows.go:84`); the Rust mapper must *not* expect it.~~ **Correction (landed): that claim was wrong.** `ScanStruct` matches by FIELD NAME (`dbx@v1.12.0/rows.go:60-85`) and `Playlist.OwnerName` (`model/playlist.go:26`) snake-cases to `owner_name`, which `selectPlaylist` emits (`:259`) — so Go DOES scan it (verified against the pinned dbx: the join yields `OwnerName == "jdoe"`). `structs:"-"` only removes it from the write map, so `playlist_row` must still not emit it; a projection without the `user` JOIN leaves Go's zero value (empty string). `Playlist.ID` is likewise scanned, so the record keeps each row's own id and the write map stays id-free.
- The annotation stamps on the wire are Go's `encoding/json` form of `*time.Time` — **`time.RFC3339Nano`** (`2024-02-02T00:00:00Z`, trailing zero fraction dropped), NOT the SQLite storage spelling (`2024-02-02 00:00:00+00:00`) the columns hold. `hydrate::to_go_time_text` is the single renderer for model timestamp strings.

### 2f. playlist_track — `playlist_track_repository.go:27-34`
- `t.dbMediaFile.PostScan()` (`:28`) then `t.PlaylistTrack.MediaFile = *t.dbMediaFile.MediaFile` (`:31`) and **`t.PlaylistTrack.MediaFile.ID = t.MediaFileID`** (`:32`) — the row's own `id` is the track position; shallow-wins dbx shadowing puts `id` in `PlaylistTrack.ID`, so the media-file `id` is recovered from `media_file_id`. The `Read` SELECT (`:100-116`) emits coalesced `starred/play_count/rating` + pointer `starred_at/play_date/rated_at` **before** `f.*` then `playlist_tracks.*`.

### 2g. scrobble_buffer — `scrobble_buffer_repository.go:23-30`
- Identical to (2f): delegate to mediafile PostScan, `t.ScrobbleEntry.MediaFile = *…MediaFile` (`:27`), overwrite `MediaFile.ID = t.MediaFileID` (`:28`). `Next`'s SELECT is `SELECT m.*, s.*` with a **LEFT JOIN media_file** — on a missing media file the `m.*` columns are NULL; the PostScan runs over NULL participants/tags (→ error in Go, but `m.*` absent is the normal case and `getParticipants` at `:84` re-fetches).

### 2h. user — `user_repository.go:36-43`
- `libraries_json` (guard `!= ""`) → `u.User.Libraries`; wrap `"parsing user libraries from db: %w"`. The SELECT builds the column via `COALESCE(json_group_array(json_object(…)) FILTER (WHERE library.id IS NOT NULL), '[]') AS libraries_json` (`user_repository.go:76-84`) — so it is **never NULL, and is the literal string `"[]"` for a user with no libraries** (not empty). The `!= ""` guard is therefore effectively *always true*; `"[]"` parses to an empty `Libraries`. A NULL (only if the alias were dropped) would hit the guard and skip. **Rust precedent already landed**: `user_repo.rs` `UserRow`/`into_record` + `parse_libraries_json` (`:559,573`) — S-D3 should generalize this, not re-invent it.

## 3. Column → landed Rust decoder map

| Decoded column | Landed Rust decoder (nd-db) | Status |
|---|---|---|
| `discs` | `sql_json::unmarshal_discs` (`sql_json.rs:184`) → `Option<BTreeMap<i64,String>>` | **Exists** |
| `folder_ids` | `sql_json::unmarshal_nullable_strings` (`:212`) → `Option<Vec<String>>` | **Exists** |
| `image_files` | `sql_json::unmarshal_image_files` (`:283`) → `Option<Vec<String>>` | **Exists** |
| `tags` | `sql_tags::unmarshal_tags` (`sql_tags.rs:148`) → `(DbTags, String err)` | **Exists** |
| `participants` | `sql_participations::unmarshal_participants` (`sql_participations.rs:349`) → `(Participants, String err)` | **Exists**; role key via `sql_participations::Role` + `role_from_id`/`into_id_role` bridge (`:496,511`) |
| `rg_*` (4) | bare `Option<f64>` row read (NULL → `None`) | plain read, no codec |
| `rules` (playlist) | `nd_model::criteria::unmarshal_criteria_str` with `default_field_map()` | **Landed** — `hydrate::parse_playlist_rules` (`playlist_post_scan`), Go's un-wrapped error |
| `similar_artists` (artist) | decode `[]dbSimilarArtist{id,name}` → `Vec<SimilarArtist>` | **Gap** — no Rust decoder; the write side `marshal_similar_artists` (`artist_repo.rs:502`) is hand-escaped `{:?}`, a different shape. S-D3 needs a *decoder* (read path) and it does not exist |
| `library_stats_json` (artist) | decode `BTreeMap<String, BTreeMap<String, BTreeMap<String, i64>>>` + role-sum fold | **Gap** — no Rust decoder; the role-sum + `RoleInvalid`-skip + `total` fold logic is Go-specific and unported |
| `libraries_json` (user) | `parse_libraries_json` (`user_repo.rs:573`) → `Vec<UserLibrary>` | **Exists** (already the landed precedent) |
| annotation cols (all 4 base-read entities) | `coalesce`d `starred: bool` / `rating: i32` / `play_count: i64` (bare) + `starred_at/play_date/rated_at` → `Option<String>` carrying Go's RFC3339Nano text | **Landed** — `hydrate::row_to_annotation`, folded onto the record as a derived field; absent LEFT-JOIN columns decode to zero values |
| `bookmark_position` (mediafile) | `coalesce(position,0) as bookmark_position` → `i64` bare | **Gap** — no `Bookmarkable` reader |
| `average_rating` (all 4) | bare `f64` | plain read |

**Gaps (no decoder exists): (a) annotation-column reader + `Annotations` record; (b) `bookmark_position`; (c) `criteria.Criteria` rules decoder for playlist; (d) `similar_artists` decoder; (e) `library_stats_json` decoder + artist-stats fold; (f) `ToGenres` → Rust (below).**

### The `ToGenres` gap (important)
`a.Genre, a.Genres = a.Album.Tags.ToGenres()` (`album_repository.go:57`) / `mediafile_repository.go:64` is a **pure function** over the decoded tags: `Tags.Values("genre")` → first value = `Genre` string; each → `Genre{ID: new_tag_id("genre", g), Name: g}`. **No Rust equivalent exists in `nd-db` or `nd-mapping`.** It belongs in **nd-db** (it consumes the decoded `DbTags` and `nd_id::new_tag_id`, both nd-db-visible). S-D3 must add `fn tags_to_genres(tags: &DbTags) -> (String, Vec<Genre>)` to `sql_tags.rs` (or `hydrate.rs`) and call it after `unmarshal_tags`. This is a real missing decoder, not just a hook.

## 4. Exact Rust seam design for `hydrate.rs`

**Ownership decision: the hooks belong in `nd-db`, not `nd-mapping`.** Rationale: (1) every landed precedent is nd-db-local — `UserRecord`/`UserRow`/`into_record` (`user_repo.rs:498,542,559`), `ArtistRecord`/`put_cols` (`artist_repo.rs:136,524`), `PlayerRecord`/`row_to_player` (`player_repo.rs:207,1073`), `ShareWithOwner`/`row_to_share` (`share_repo.rs:319,477`); (2) the DTOs `nd-db` already carries (`UserRecord`, `Tag`, `Scrobble`, etc.) are the records the hooks hydrate; (3) `nd-mapping` is the *tag-reader → `Metadata` → `MediaFile`* ingest mapping (#16), a different direction (file→entity) with its own Go-oracle goldens — putting a DB-column decoder there would cross that boundary and couple it to rusqlite. `hydrate.rs` + the entity records go in `nd-db`.

### 4a. The row reader
One shared column accessor over `&rusqlite::Row<'_>`, in `hydrate.rs`. NULL semantics per Go (`database/sql/convert.go` — NULL into non-pointer numeric/string/time **errors**; NULL into pointer → nil):
```rust
pub trait RowGet {
    fn get_text(&self, col: &str) -> rusqlite::Result<String>;            // NULL -> Err (Go parity)
    fn get_bool(&self, col: &str) -> rusqlite::Result<bool>;               // coalesce'd cols: non-NULL
    fn get_i32(&self, col: &str) -> rusqlite::Result<i32>;
    fn get_i64(&self, col: &str) -> rusqlite::Result<i64>;
    fn get_f64(&self, col: &str) -> rusqlite::Result<f64>;
    fn get_f32(&self, col: &str) -> rusqlite::Result<f32>;
    fn get_opt_text(&self, col) -> rusqlite::Result<Option<String>>;
    fn get_opt_f64(&self, col) -> rusqlite::Result<Option<f64>>;           // rg_*, bit_depth, bpm via opt_i32
    fn get_opt_dt(&self, col) -> rusqlite::Result<Option<DateTime<Utc>>>;  // SQL NULL → None; unparseable text → Some(go_zero_time)
}
```
`get_*` (non-opt) must **error on NULL**, matching Go. Only the §-pointer set (RG×4, `bit_depth`/`bpm`, the annotation pointer times, `external_info_updated_at`, `evaluated_at`, `imported_at`) get `get_opt_*`.

**Correction (landed): timestamps never error on bad text.** The pinned driver decodes a DATETIME/DATE/TIMESTAMP cell by trimming a trailing `Z` and trying `SQLiteTimestampFormats` in order (`go-sqlite3@v1.14.52/sqlite3.go:2821-2842`, `:331-343`); when none parses it assigns `time.Time{}` and reports **no error**. `database/sql` then stores that value — into a non-pointer field directly, into a pointer by allocating one (`convert.go:491-496`). So `get_dt` errors ONLY on SQL NULL and `get_opt_dt` returns `None` ONLY on SQL NULL; every unparseable cell (the `'0000-00-00 00:00:00'` DDL default included) yields Go's zero time, which is why Go's JSON for such a field renders `0001-01-01T00:00:00Z` instead of dropping it. `hydrate::driver_parse_dt` reproduces the driver's nine formats; `go_zero_time`/`is_go_zero` are Go's `time.Time{}`/`IsZero()`.

### 4b. The hook / hydrator shape — avoid duplicating decoding
Do **not** write 8 near-identical `row_to_*`. Instead: a small shared **column-payload** read (the JSON shim strings + the float/timestamp columns) plus a **per-entity hook fn** that takes the already-read raw payload and produces the record. This mirrors Go's two-phase `Scan` (raw column) → `PostScan` (decode), and the landed `UserRow{record, libraries_json}` → `into_record()` split (`user_repo.rs:542,559`) which is the *exact* precedent to follow:
```rust
pub enum HydrateError {
    #[error("{0}")]              // carries Go's exact prefix, e.g. "parsing album discs from db: …"
    Decoded(String),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("record not found")]
    NotFound,
}
// e.g.
pub fn album_post_scan(row: &rusqlite::Row<'_>) -> Result<AlbumRecord, HydrateError>;
pub fn mediafile_post_scan(row: &rusqlite::Row<'_>) -> Result<MediaFileRecord, HydrateError>;
pub fn playlist_track_post_scan(row: &rusqlite::Row<'_>) -> Result<PlaylistTrackRecord, HydrateError>; // delegates, then overwrites
pub fn scrobble_buffer_post_scan(row: &rusqlite::Row<'_>) -> Result<ScrobbleEntryRecord, HydrateError>; // delegates, then overwrites
```
The two delegating hooks reuse `mediafile_post_scan` then apply `record.media_file.id = record.media_file_id;` — a two-step, no special mechanism (hydrate-contract §6 is right on this).

The record DTOs (`AlbumRecord`, `MediaFileRecord`, `FolderRecord`, `PlaylistRecord`, `PlaylistTrackRecord`, `ScrobbleEntryRecord`, `ArtistRecord`-read-side, `UserRecord`) **live in `nd-db`**, in `hydrate.rs` or the entity repo module — never in `nd-model` (hydrate-contract §6.4 + entity-write-contract: nd-model is #06's shared crate). The S-D2b2 `AlbumRecord`/`MediaFileRecord` that S-D2b2 must land are the *write* records; S-D3's *read* records may reuse or wrap them — **coordinate with S-D2b2's owner** so the read record is the same struct the writer produces (the `UserRow`/`UserRecord` pattern: read record IS the write record, hydration fills the derived fields).

**Where decoding is de-duplicated:** the JSON shim columns (`discs`/`folder_ids`/`image_files`/`tags`/`participants`) are read by the **one** `hydrate.rs` column-payload reader and decoded by the **landed** `sql_json`/`sql_tags`/`sql_participations` codecs — the per-entity hook fn only *wires* them to its record's fields. The `rg_*` floats and annotation columns are read once in the shared reader, copied into whichever record needs them. This is how S-D3 avoids 8 copies of the same unmarshal.

### 4c. Error type carries Go's prefix
`HydrateError::Decoded(String)` stores Go's literal prefix verbatim (`"parsing album discs from db: …"`, `"parsing similar artists from db: …"`, `"parsing artist stats from db: …"`, `"parsing user libraries from db: …"`, `"parsing folder image files from db: …"`). For the *unwrapped* ones (playlist `rules`, and `participants` where Go has no wrapper) the codec's own message (`sql_participations::unmarshal_participants` already emits `"parsing participants: …"`) is used. Do not invent new prefixes.

## 5. Row-column ↔ struct hazards (settled, and how Rust reads them)

| Hazard | Settled behaviour | Rust read |
|---|---|---|
| Sentinel `0000-00-00 00:00:00` | DDL default on `library.last_scan_at`, `last_scan_started_at`, `album.imported_at`, `folder.images_updated_at`; the driver cannot parse it, assigns `time.Time{}` and reports **no error**, so the row reads fine and callers test `!IsZero()`. **Correction (landed): the port does not reject or drop the sentinel** — it scans as `go_zero_time()`, and a record that spells "never stamped" as `None` maps Go's zero to `None`. (`time::optional_sentinel` remains the standalone helper for callers that want absent-on-sentinel; the hydration getters do not route through it.) | Both getters go through `hydrate::driver_parse_dt`: unparseable → `go_zero_time()`, SQL NULL → error (`get_dt`) / `None` (`get_opt_dt`). |
| Scan-side field-name mapping | `ScanStruct` maps a column onto a struct field by **field name** through `DefaultFieldMapFunc` (`dbx@v1.12.0/rows.go:60-85`); Navidrome sets no `FieldMapper`, so `structs` tags apply to the WRITE map only. A field whose snake-case has no matching column keeps Go's zero value. | Do not read a column a Go field cannot reach (`UpdateAt` ≠ `updated_at`), and do hydrate a field whose name does match even when its tag is `structs:"-"` (`OwnerName` ↔ `owner_name`). Keep both out of the write map. |
| NULL vs empty string | Go: NULL→non-pointer **errors**; NULL→pointer nil. JSON shims guard `!= ""` (empty string = skip, not error); `"[]"` is *not* empty (user `libraries_json` coalesces to the literal `"[]"` for no-libraries). | Non-NULL-expected cols via `get_*` (error on NULL); JSON shims: read as `Option<String>`/`String`, `if !s.is_empty() { decode }`; treat `"[]"` as valid-empty. |
| int / real bool conversions | `bool` columns stored as SQLite INTEGER (0/1); `coalesce(starred,0) as starred` yields `0`/`1`. `duration`/`rg_*`/`average_rating` are `real`/`REAL`. | `get_bool` (rusqlite maps i64→bool); `get_f64` for real; `get_i32`/`get_i64` for ints. |
| `*time.Time` vs non-pointer | Pointer times (annotation `play_date/rated_at/starred_at`, artist `created/updated/external_info_updated_at`, playlist `evaluated_at`, album `imported_at/external_info_updated_at`, user `last_login/last_access`, share `expires/last_visited`) → SQL NULL = nil, unparseable text = **pointer to the zero time**. Non-pointer times (`created_at`/`updated_at`/`birth_time`/`images_updated_at`) → SQL NULL **errors**; unparseable text scans as the zero time. | Pointer → `get_opt_dt`; non-pointer → `get_dt` (error on NULL only). On the wire a stamp is Go's `RFC3339Nano` text (`hydrate::to_go_time_text`), not the storage spelling. |
| RG shim fields | `rg_album_gain`/`rg_album_peak`/`rg_track_gain`/`rg_track_peak` are the **real** columns; Go's model field `RGAlbumGain` does *not* snake-case to `rg_album_gain` (dbx default mapper gives `rgalbum_gain`), so `dbAlbum`/`dbMediaFile` carry `RgAlbumGain *float64` shims and PostScan copies. | **Rust reads the real `rg_album_gain` column directly** — no shim needed, since Rust's record field can be named to read `rg_album_gain` verbatim. This is a *simplification*, not a hazard: S-D3 just reads `rg_album_gain`/`rg_track_gain` etc. as `Option<f64>`. |

## 6. Recommended S-D3 sub-slices (disjoint file ownership), ordered after S-D2b1/S-D2b2

S-D2b1 (folder+playlist records) and S-D2b2 (album+mediafile records) must land first so the read records match the write records. After that:

1. **S-D3a — `hydrate.rs` core + user (smallest, reuses landed precedent)**
   - Owns: new `nd-db/src/hydrate.rs` (RowGet, `HydrateError`, `UserRecord` generalization), one `lib.rs` `pub mod hydrate;` line, `tests/runtime_hydrate_user.rs`.
   - The `user` hook is already 90% done in `user_repo.rs` (`UserRow::into_record`); S-D3a *promotes* that pattern into the shared `hydrate.rs` and re-points `user_repo` at it (no behaviour change). Low-risk, establishes the seam.
   - **Executed read test seed:** a `user` row + `user_library` + `library` rows; assert `libraries` hydrated from `libraries_json` (both a user *with* libraries and one *without* → `"[]"` → empty vec).

2. **S-D3b — folder + playlist hydration**
   - Owns: `folder_repo.rs` + `playlist_repo.rs` read-side hooks, `tests/runtime_hydrate_folder_playlist.rs`.
   - `folder`: `image_files` + the row's own `id`; `library_path` (INNER JOIN `library`, the only library column `selectFolder` emits — there is no `library_name`). `Folder.UpdateAt` is NOT hydrated (`update_at` ≠ the emitted `updated_at`), while the write map keeps `updated_at`. `playlist`: `rules` (NULL→nil), `evaluated_at`, the annotation cols (base read joins them), `owner_name` **hydrated** (`structs:"-"` blocks the write map, not the scan), the row's own `id`, and the `SMART_OWNED_COUNTER_COLS`.
   - **Seeds:** a `folder` row (with and without `image_files`), a DDL-default row (sentinel timestamps scan as Go's zero), and a bulk read asserting each record keeps its own `id`; a plain and a **smart** `playlist` (rules set) + an annotation row; assert `Rules` decoded / `None`, annotation fields present, annotation stamps in RFC3339Nano form, `owner_name` from the JOIN (and empty on a projection without it).

3. **S-D3c — album + mediafile hydration (largest; carries the 2 corrections)**
   - Owns: `album_repo.rs` + `mediafile_repo.rs` read-side, `tests/runtime_hydrate_album_mediafile.rs`.
   - album: `discs`/`participants`/`tags`/`folder_ids`/`rg_album_*` + **annotation cols + `average_rating`** (correction 1) + `imported_at` sentinel. mediafile: `participants`/`tags`/4×`rg`/`lyrics` (plain col, *not* a hook) + **annotation cols + `average_rating` + `bookmark_position`** (corrections 1&2). Add `tags_to_genres` here (the `ToGenres` gap).
   - **Seeds:** an `album` with non-empty `discs`/`tags`/`participants`/`folder_ids` + an `annotation` row + `imported_at` sentinel; a `media_file` with `lyrics`, `tags`, `participants`, 4 RG cols, an `annotation` row, and a `bookmark` row; assert `Genre`/`Genres` from `tags_to_genres`, `bookmark_position` read, `None` annotation times when no annotation row (LEFT JOIN).

4. **S-D3d — artist hydration**
   - Owns: `artist_repo.rs` read-side (the *read* record, distinct from the landed write `ArtistRecord` at `:136`), `tests/runtime_hydrate_artist.rs`.
   - `library_stats_json` fold (`m/a/s` per role, `total`, `RoleInvalid` skip) + `similar_artists` decode + annotation cols. The artist-stats decoder is the most novel — give it its own sub-slice and its own unit test against a hand-built `library_stats_json` string.
   - **Seeds:** an `artist` with a `library_artist` row whose `stats` = `{"total":{"m":2,"a":1,"s":1024},"artist":{...}}` + `similar_artists` + an `annotation` row; assert `Stats` folded, `Size/SongCount/AlbumCount` from `total`, `SimilarArtists` decoded, and a second artist *without* `similar_artists` → `[]`.

5. **S-D3e — playlist_track + scrobble_buffer (delegating hooks)**
   - Owns: `playlist_track_repo.rs` + `scrobble_buffer_repo.rs` read-side, `tests/runtime_hydrate_track_scrobble.rs`.
   - Reuses S-D3c's `mediafile_post_scan`; applies the `media_file_id` → `media_file.id` overwrite. `playlist_track.Read` coalesced annotation cols; `scrobble_buffer.Next` `m.*` with a **missing** media file (NULL `m.*` → the delegated hook must tolerate absent `participants`/`tags`).
   - **Seeds:** a `playlist` + `playlist_tracks` row + its `media_file` (+ annotation) → assert track position in `id` and `media_file.id == media_file_id`; a `scrobble_buffer` row with a **deleted/missing** `media_file` → assert NULL `m.*` handled (the separate `getParticipants` is *out of scope* — pin only the PostScan-delegated fields).

6. **S-D3f — artwork `hydrateItems` (separate, its own sub-slice, NOT in `hydrate.rs`)**
   - Owns: a `nd-db/src/artwork_hydration.rs` + `tests/`. This is the *second* hydration layer (batched `item_artwork` lookup filling `ItemImage`), called from `album`/`mediafile`/`artist`/`playlist`/`folder` `GetAll`. Keep it **out** of the PostScan contract — it has no Go PostScan counterpart; it's a repository method. Sequenced last; does not block #17.

**Ordering rationale:** a→b→c→d→e (e reuses c). f is independent and last. Each has disjoint repo files, so lanes can run in parallel once the `hydrate.rs` core (S-D3a) lands.

## 7. Staleness / verification notes (S-D3a, verified 2026-11-12)

**Stale in the docs (corrected in source):**
- hydrate-contract "Parent-resolved open item" + §7: *"newSelect … is not the annotation join … base reads must not expect annotation columns."* **STALE.** `selectAlbum:230`, `selectMediaFile:207`, `selectArtist:214` call `withAnnotation` unconditionally → base reads **do** carry the 6 annotation columns (+ `average_rating`), and `selectMediaFile` also carries `bookmark_position`. The hydrators must decode them on every base read.
- hydrate-contract §top + next-work §1c: *"S-A/S-B/S-D1 not landed … HEAD 336f69ba is docs-only."* **STALE.** S-A `2a99a681`, S-B `3f8b5a8d`, S-D1 `cb11b645`, S-D2a `60198cec` all landed (verified in `git log` + the files exist: `sql_participations.rs` has `marshal_participants`/`unmarshal_participants`, `sql_feeders.rs`, `sql_json.rs`, `marshal.rs` all present). HEAD is `60198cec`, not `336f69ba`.

**Formerly unverified claims — now closed by live Go/Rust probes (lane `61acbfa8…`):**
- Integer-key JSON ordering is string-sorted (`{"1":"a","10":"c","2":"b"}`), confirming `marshal_discs`' stringify-then-sort rule; `AllNames()` map ordering is non-reproducible, and `search_participants` sorts/dedupes.
- `nd_id::new_tag_id` matches Go byte-for-byte, including Unicode final-sigma and U+0130 goldens. `nd_utils::str::to_lower` is exhaustively checked against Go `strings.ToLower` over all Unicode scalar values.
- S-A participant goldens are parse-compared, not byte-compared; Rust reads Go-written `\u003c`, `\u003e`, `\u0026`, `\u2028`, and `\u2029` escapes. All Rust decoders accept the probed Go-written album, participant, tag, discs, and folder_ids bytes. See `next-work.md`'s S-D3a verification entry for details.


**Verified against source (spot-checks of the doc's own claims):** all 8 PostScan file:lines and bodies; the 5 PostMapArgs; `withAnnotation`/`withBookmark` coalesce+pointer column sets; the 4 RG shim fields on `dbAlbum`/`dbMediaFile`; `album/artist/mediafile` base selects' unconditional annotation join; `library_stats_json` alias column; `user` `libraries_json` `COALESCE(…, '[]')`; `Artist.Stats/Size/SongCount/AlbumCount` `structs:"-"`; `MediaFile.Lyrics` plain column; `dbPlaylist` by-value embed + `sql.NullString` Rules shim; `dbPlaylistTrack`/`dbScrobbleBuffer` value embed + `media_file_id` overwrite; `time::optional_sentinel` at `time.rs:118`; `UserRow`/`into_record` at `user_repo.rs:542/559`; the `sql_json`/`sql_tags`/`sql_participations` decoder signatures.

## VERIFIED / UNVERIFIED tally
- **VERIFIED:** 8/8 PostScan hooks + exact guards/prefixes/zero-value; the two plan-changing corrections (base-read annotation join; mediafile bookmark col); all 5 PostMapArgs; all landed Rust decoders (`sql_json`×3, `sql_tags::unmarshal_tags`, `sql_participations::unmarshal_participants`+role bridge, `user` `parse_libraries_json`, `time::optional_sentinel`); all model pointer-field shapes (annotation `*time.Time`×3, RG `*float64`, `bit_depth`/`bpm *int`, artist/playlist/user timestamps); the `UserRow`/`UserRecord` precedent; S-A/S-B/S-D1/S-D2 landed state; and the three empirical claims above.
- **UNVERIFIED:** none of the former JSON-order, `new_tag_id`, or participant-golden-mode claims remain open. `nd_id::new_tag_id` and `nd_utils::str::to_lower` are byte-pinned against the actual Go implementation.

## Remaining blockers and order (updated 2026-11-12)
1. **`ToGenres` has no Rust home** — it is a decoder, not a hook; add `sql_tags::tags_to_genres` before S-D3c can be complete.
2. **No shared `Annotations`/`Bookmarkable` record exists** — S-D3c needs a new annotation-column reader + record.

## Recommended order
S-D2b1/S-D2b2 and S-D3a are landed. Continue **S-D3b** (folder+playlist) → **S-D3c** (album+mediafile, incl. `tags_to_genres` + annotation/bookmark readers) → **S-D3d** (artist) → **S-D3e** (track+scrobble, reuses c) → **S-D3f** (artwork, independent). One writer per repo file per lane; no `nd-model` edits; parent gates between each.