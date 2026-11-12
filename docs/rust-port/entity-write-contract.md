# Entity write contract (S-D2b source of truth)

Scoping brief for slice **S-D2b** (complete entity -> `(columns, values)` records that remove caller-supplied
column arrays). Produced by a read-only scout lane and tree-verified against HEAD `4988df71`; the four
`structs.Map` column sets were **executed** against the vendored `fatih/structs v1.1.0`, and the DDL column sets
were parsed from `rust/crates/nd-dbschema/src/schema_ddl.rs`.

Scope note: this is the **write** contract only. Row -> entity hydration and the 8 `PostScan` hooks belong to
S-D3 (`hydrate.rs`). S-D2a (`marshal.rs`, landed) supplies the sorted column/value unit and the PostMapArgs
transformations; S-D2b supplies the records that feed them.

---

# S-D2b scoping brief — complete entity→DB-column contract

All claims below re-derived against the working tree (`4988df71`), not from memory. `structs.Map` key sets were **executed** against `model.{Album,MediaFile,Folder,Playlist}` with the vendored `fatih/structs v1.1.0`. DDL parsed from `rust/crates/nd-dbschema/src/schema_ddl.rs`.

**Scope reality check:** S-D2b owns **write-side marshalling only** (entity→`(cols,values)`, eliminating caller-supplied column arrays). Row→entity hydration is **S-D3** (`hydrate.rs` + `PostScan`). This contract is complete for S-D2b's write path; read-side notes are flagged so S-D2b doesn't pre-empt S-D3.

## Write funnel recap (verified)
`toSQLArgs` (`persistence/helpers.go:20-42`): `structs.Map(rec)` → per-value `*time.Time` unwrap (nil stays nil→NULL) / `driver.Valuer.Value()` → `PostMapArgs` on the whole map. `squirrel.SetMap` sorts column names, emits values positionally. `put` (`sql_base_repository.go:613-642`): UPDATE-first (via `filterUpdateValues`→`selectUpdateColumns`), then INSERT with `id` added to the values map (`:637-641`). **`id` and `created_at` are never in the caller's `columns`/`values`** — the repo appends `id`; `created_at` is dropped on update by `selectUpdateColumns` (`:587-604`).

`selectUpdateColumns` (`sql_base_repository.go:587-604`) removes exactly: filters to `colsToUpdate` (snake_cased) when given, then **unconditionally deletes `created_at` and `birth_time`** (`:601-604`). `id` is re-added by `filterUpdateValues` (`:606-609`) for the update leg.

## 1. Go model/db-wrapper + persisted fields

### ALBUM
db wrapper `dbAlbum` (`persistence/album_repository.go:29-39`): `*model.Album structs:",flatten"` + shims, all `structs:"-"`:
- `Discs string` (:31), `Participants string` (:32), `Tags string` (:33), `FolderIDs string` (:34)
- `RgAlbumGain *float64` (:37), `RgAlbumPeak *float64` (:38) — shim because dbx default field-map renders `RGAlbumGain`→`rgalbum_gain`, so the real `rg_album_gain` column can't bind to the model field.

`model.Album` (`model/album.go:15-73`) — **46 persisted columns** (executed `structs.Map`, sorted):
`album_artist album_artist_id catalog_num comment compilation created_at date description discs duration embed_art_path explicit_status external_info_updated_at external_url folder_ids genre id imported_at large_image_url library_id max_original_year max_year mbz_album_artist_id mbz_album_comment mbz_album_id mbz_album_type mbz_release_group_id medium_image_url min_original_year min_year missing name order_album_artist_name order_album_name original_date participants release_date rg_album_gain rg_album_peak size small_image_url song_count sort_album_artist_name sort_album_name tags updated_at`

Embedded non-row structs (all `structs:"-"`, **never persisted**): `Annotations` (`album.go:16`, row only via base cols `average_rating`/annotation join — see §3), `ItemImage` (`:17`, all fields `structs:"-"`, `artwork.go:29-40`). Non-persisted model fields: `LibraryPath`/`LibraryName` (`:21-22` `structs:"-"`, join-only), `Genres` (`:65` `structs:"-"`).

### MEDIAFILE
db wrapper `dbMediaFile` (`persistence/mediafile_repository.go:29-39`): `*model.MediaFile flatten` + shims `Participants string` (:31), `Tags string` (:32), `RgAlbumGain/Peak/RgTrackGain/Peak *float64` (:35-38).

`model.MediaFile` (`model/mediafile.go:27-107`) — **64 persisted columns** (executed, sorted):
`album album_artist album_artist_id album_id artist artist_id birth_time bit_depth bit_rate bpm catalog_num channels codec comment compilation created_at date disc_number disc_subtitle duration explicit_status folder_id genre has_cover_art id library_id lyrics mbz_album_artist_id mbz_album_comment mbz_album_id mbz_album_type mbz_artist_id mbz_recording_id mbz_release_group_id mbz_release_track_id missing order_album_artist_name order_album_name order_artist_name order_title original_date original_year participants path pid probe_data release_date release_year rg_album_gain rg_album_peak rg_track_gain rg_track_peak sample_rate size sort_album_artist_name sort_album_name sort_artist_name sort_title suffix tags title track_number updated_at year`

Embedded non-row: `Annotations` (:28), `Bookmarkable` (:29 → `bookmark_position` `structs:"-"`, no col), `ItemImage` (:30), `AlbumImage` (:34, `structs:"-"` — hydrated elsewhere, never a column). Non-persisted: `LibraryPath`/`LibraryName` (:39-40), `Genres` (:72). **68 DDL cols** — see §6 (extra `average_rating`, `full_text`, `search_participants`, `search_normalized`).

### FOLDER
db wrapper `dbFolder` (`persistence/folder_repository.go:25-28`): `*model.Folder flatten` + shim `ImageFiles string structs:"-"` (:27) shadowing `Folder.ImageFiles []string`.

`model.Folder` (`model/folder.go:18-33`) — **13 persisted columns**:
`created_at hash id image_files images_updated_at library_id missing name num_audio_files num_playlists parent_id path updated_at`
Note: `UpdateAt time.Time structs:"updated_at"` (:31) — Go field name is `UpdateAt`, column is `updated_at`. Non-persisted: `LibraryPath` (:21 `structs:"-"`).

### PLAYLIST
db wrapper `dbPlaylist` (`persistence/playlist_repository.go:24-27`): `model.Playlist` **by value** `structs:",flatten"` + `Rules sql.NullString structs:"-"` (:26).

`model.Playlist` (`model/playlist.go:20-45`) — **17 persisted columns**:
`comment created_at duration evaluated_at external_image_url id imported_hash name owner_id path public rules size song_count sync updated_at uploaded_image`
Embedded non-row: `Annotations` (:21), `ItemImage` (:22). Non-persisted: `OwnerName` (:30 `structs:"-"`, join), `Tracks` (:33 `structs:"-"`).

## 2. Flattening / shims / Valuer / time; must-NOT-persist

- **Flattened embeds → columns:** `Annotations` (via its own `structs` tags: `play_count,play_date,rating,rated_at,starred,starred_at,average_rating` — but the *row* carries these on the base table as `average_rating` + an `annotation` LEFT JOIN; the write path never sets them, `SetRating`/`IncPlayCount`/`ReassignAnnotation` are separate UPDATEs, `sql_annotations.go:142-190`). In practice `structs.Map` on the entity does **not** emit annotation columns (they ride on a separately-joined row), so the write marshaler should NOT emit `play_count`/`rating`/`starred`/`average_rating` — they are annotation-managed, not part of the entity put map.
- **Shadowing (shallow wins):** dbAlbum/dbMediaFile flat string shims `Discs/Participants/Tags/FolderIDs` shadow the model's typed fields of the same db-name — so `structs.Map` (which operates on the *db* struct in `toSQLArgs`) would flatten the model's `Discs Discs`, `Tags Tags`, `Participants Participants` fields into keys `discs`/`tags`/`participants`/`folder_ids`… **but those are maps**, and the real write relies on `PostMapArgs` overwriting them with JSON. `structs.Map(model.Album{})` empirically yields `discs, tags, participants, folder_ids` keys (values are the map types). `PostMapArgs` then replaces those 4 keys with JSON strings and adds `full_text/search_participants/search_normalized`.
- **driver.Valuer:** none of these four entities define `Value()` — the only `driver.Valuer` in `helpers.go:28` matters if a field implements it; `model.Tags`/`Participants`/`Discs` do **not** implement `Valuer` (checked — no `func (.*) Value()`), so the per-value switch passes them through untouched; JSON-ification is done exclusively by `PostMapArgs`.
- **\*time.Time conversion:** `*time.Time` non-nil → deref, nil → NULL. Affected: album `external_info_updated_at` (`album.go:62`); mediafile has **no** `*time.Time` in row (its time fields `birth_time/created_at/updated_at` are non-pointer `time.Time`); playlist `evaluated_at` (`playlist.go:44`).
- **Must-NOT-persist (json/hash/derived-only):** `ItemImage.*` (all), `Genres`, `LibraryPath`/`LibraryName` (all 4), `Bookmarkable.BookmarkPosition`, `AlbumImage`, playlist `OwnerName`/`Tracks`, album `releases` (see §6 — **DDL has it, Go never writes it**).

## 3. PostMapArgs adds/overwrites/removes (exact lines)

| Entity | file:lines | Adds | Overwrites | Removes |
|---|---|---|---|---|
| album | `album_repository.go:71-97` | `full_text` (:78), `search_participants` (:79), `search_normalized` (:80) | `tags` (:82 `marshalTags`), `participants` (:83 `marshalParticipants`), `folder_ids` (:89 JSON), `discs` (:95 JSON) | — |
| mediafile | `mediafile_repository.go:69-80` | `full_text` (:74), `search_participants` (:75), `search_normalized` (:76) | `tags` (:77), `participants` (:78) | — |
| folder | `folder_repository.go:48-59` | — | `image_files` (:50 `"[]"` when nil, else :56 JSON) | — |
| playlist | `playlist_repository.go:36-51` | `rules` (:39 JSON, only when `IsSmartPlaylist()`) | — | **smart:** delete `song_count` (:44), `duration` (:45), `size` (:46); **non-smart:** delete `rules` (:49) |

album `full_text` input groups (`:72-78`): Name, SortAlbumName, AlbumArtist, `Participants.AllNames()`, `maps.Values(Discs)`, `Tags[TagAlbumVersion]`, `Tags[TagCatalogNumber]` → `formatFullText`. mediafile `full_text` (`:70-74`): `FullTitle()`, Album, Artist, AlbumArtist, SortTitle, SortAlbumName, SortArtistName, SortAlbumArtistName, DiscSubtitle, `+AllNames()`.

## 4. `selectUpdateColumns` + entity-specific cols_to_update callers

`selectUpdateColumns` drops `created_at` + `birth_time` always; `id` re-added by `filterUpdateValues`. Entity-specific partial-update callers:

- **album `UpdateExternalInfo`** (`album_repository.go:223`): `cols = ["description","small_image_url","medium_image_url","large_image_url","external_url","external_info_updated_at"]`. Full `Put` (`:210`) passes no cols (all columns).
- **playlist `Update`** (`playlist_repository.go:431-437`): `r.put(ctx, id, pls, append(cols, "updatedAt")...)` — caller-driven partial (`core/playlists/rest_adapter.go` passes e.g. `["sync"]`/`["public"]`/`["name","comment","public","owner_id","rules"]`). `Put` (`:110-146`) passes `cols...` (empty → full).
- **folder/mediafile:** `Put` uses full-column `r.put` (no `colsToUpdate`); mediafile `Put` (`mediafile_repository.go:188`) via `putByMatch` (`path`,`library_id`) with empty `m.ID` → SELECT-then-put.

Rust `NEVER_UPDATE_COLS = ["created_at","birth_time"]` already present (`playlist_repo.rs:486`); `filter_update_cols` (`:541`) mirrors `selectUpdateColumns`.

## 5. Reusable Rust local types vs gaps (in nd-db; no nd-model edits)

**Precedent to follow:** `ArtistRecord` (`artist_repo.rs:136-162`) is the *one* entity already fully record-driven with its own PostMapArgs inline in `put_cols` (`:524-591`, incl. `retain`-deletion of empty `sort_artist_name`/`mbz_artist_id` at `:583-586`) and `put_sql(artist, now, cols_to_update)` (`:600`). **S-D2b's four records generalize this shape.**

**No `AlbumRecord`/`MediaFileRecord`/`FolderRecord`/`PlaylistRecord` exist yet** — verified (grep for `pub struct *Record` in those files: none). All four `put_sql`/`put_legs` currently take **caller-supplied** `columns:&[String], values:Vec<Value>`:
- `album_repo.rs:459` `put_sql(id, columns, values, participants_json, indexed_tag_ids)`, `put_legs` `:505-530`
- `mediafile_repo.rs:498` `put_sql(id, columns, values, participants_json, indexed_tag_ids)`, `put_legs` `:556-586`
- `folder_repo.rs:334` `put_sql(id, columns, values)`, legs `:347-366`
- `playlist_repo.rs:502` `put_sql(id, cols:&[Col], all_cols:&[Col], cols_to_update)` + local `Col` (`:450-460`), `filter_update_cols` `:541`

**Reusable building blocks (S-D1/S-A/S-B landed):**
- `sql_feeders.rs`: `album_fts_columns` (:167), `mediafile_fts_columns` (:122), `format_full_text` (:45), `search_normalized` (:54), `search_participants` (:67), `full_title` (:85) — produce the 3 FTS cols.
- `sql_json.rs`: `marshal_discs` (:153, **stringify-then-sort** Go key order), `unmarshal_discs` (:184), `marshal_nullable_strings` (:194, folder_ids), `marshal_image_files` (:262, `"[]"` for nil), `unmarshal_image_files` (:283).
- `sql_tags.rs`: `marshal_tags` (:129), `unmarshal_tags` (:148), `indexed_tag_ids` (:164).
- `sql_participations.rs`: `marshal_participants` (:319), `unmarshal_participants` (:349), `marshal_flat_participants` (:297), `Role` bridge `role_from_id`/`into_id_role` (:496/:511).
- `time.rs`: `now()` (:68), `optional_col` (:132, nil `Option<DateTime<Utc>>`→`(col,Value::Null)`), `optional_sentinel` (:118, `0000-00-00`→None).
- `artist_repo::put_cols` (`:524`) as the reference record→cols+PostMapArgs shape.

**Missing (S-D2b must create, all local to nd-db):**
- `AlbumRecord`, `MediaFileRecord`, `FolderRecord`, `PlaylistRecord` structs + `put_cols`/`row()` per entity that (a) emit every §1 column, (b) apply that entity's §3 PostMapArgs, (c) sort keys, (d) let `put_legs` append `id` last.
- mediafile **`FtsColumns`** (`mediafile_repo.rs:459-468`) already a DTO — feed it from `mediafile_fts_columns`; wire, don't reinvent.
- playlist `Col`→reuse or fold into the shared `Value` pair; the smart/non-smart rule/counter delete must move from caller into the record's PostMapArgs.

## 6. Fresh DDL columns/defaults/nullability + reconciliation

DDL lines: album `:58`, media_file `:64`, folder `:61`, playlist `:66`. Reconciliation (DDL col set vs executed Go write-set):

| Table | DDL cols | Go put cols | DDL-only (NOT written by entity Put) |
|---|---|---|---|
| album | 51 | 46 | `average_rating`, `full_text`, `releases`, `search_normalized`, `search_participants` |
| media_file | 68 | 64 | `average_rating`, `full_text`, `search_normalized`, `search_participants` |
| folder | 13 | 13 | none — exact match |
| playlist | 18 | 17 | `average_rating` |

**No Go field has no DDL column** (all 46/64/13/17 are present). DDL-only columns resolve as:
- `full_text`, `search_participants`, `search_normalized` (album, media_file) — **written by PostMapArgs** (they *are* part of the final write map; my Go set was the pre-PostMapArgs `structs.Map`). So the effective Rust write-set is 49 (album) / 67 (media_file) after PostMapArgs. These feed the FTS5 virtual tables `album_fts`/`media_file_fts` (DDL `:80,:79`) via AFTER INSERT/UPDATE **triggers** (`:85-87`, `:82-84`) which read `NEW.search_participants`/`NEW.search_normalized`/`NEW.discs`/`NEW.tags`/`NEW.catalog_num`. **9 triggers total** in the DDL.
- `average_rating` (album, media_file, playlist) — **derived**, written only by annotation operations (`sql_annotations.go:154` `Set("average_rating", ...)`), never by entity Put. **Exclude from the record's put map.**
- **`releases` (album) — ORPHAN.** Present in DDL (`:58`, `releases integer default 0 not null`) but has **no Go model field and no Go writer** anywhere (`grep \breleases\b` finds only `release_date`/prose). Legacy leftover. Rust put must NOT emit it (no Go field → not in write map); it will default to 0 on insert. **This is the one "DDL column with no Go field" the brief asked to flag.**

**Derived / trigger-managed columns:** `full_text`, `search_participants`, `search_normalized` are base columns that also seed FTS via triggers (but they're still set by PostMapArgs — the trigger only mirrors them into the fts5 shadow tables). `average_rating` is base + annotation-only. No DDL column for these entities is trigger-*written* back into the base table (triggers only touch the `*_fts` virtual tables), so there is no write-conflict.

**Time-column nullability / defaults (must be bound explicitly, not relied on):**
- album: `created_at datetime` (nullable, no default), `updated_at datetime` (nullable, no default), `external_info_updated_at datetime` (nullable — pointer), `imported_at datetime NOT NULL DEFAULT '0000-00-00 00:00:00'`.
- media_file: `created_at`/`updated_at datetime` (nullable, no default), `birth_time datetime NOT NULL DEFAULT current_timestamp` (never updated — `selectUpdateColumns` drops it), `bit_depth`/`bpm integer` (nullable → pointer).
- folder: `updated_at`/`created_at datetime NOT NULL DEFAULT (datetime(current_timestamp,'localtime'))`, `images_updated_at datetime NOT NULL DEFAULT '0000-00-00 00:00:00'`.
- playlist: `created_at`/`updated_at datetime` (nullable, no default), `evaluated_at datetime` (nullable → pointer), `rules varchar` (nullable — JSON when smart, NULL/'' otherwise).
- album RG `rg_album_gain/rg_album_peak real` (nullable → pointer); mediafile all 4 RG `real` (nullable → pointer).

Per the 2026-11-11 decision (next-work.md :208-246), bind `DateTime<Utc>` explicitly via `time::optional_col`/`now()`; do **not** rely on `current_timestamp`/localtime defaults — Go sets `created_at`/`updated_at` explicitly at the repo layer (album `ImportedAt=Now` `:210`; playlist `CreatedAt`/`UpdatedAt=Now` `:121-123`; folder `NewFolder` sets `UpdateAt`/`CreatedAt`).

## 7. S-D2b file ownership + sequencing

**Recommendation: sequential, all four in one lane, but as 4 independent sub-slices** (each is a self-contained record + `put_cols` + executed write→read test; no inter-dependency). Order by risk/complexity:

1. **folder** — simplest (13 cols, 1 JSON shim, no FTS, no RG, no partial-update caller). Warm-up that validates the record+`put_legs`+sort pattern.
2. **album** — 46+FTS, 4 JSON overwrites, RG shims, `UpdateExternalInfo` partial cols.
3. **playlist** — smart/non-smart rule-vs-counter PostMapArgs, `Cols`→shared seam, `cols_to_update` plumbing, `evaluated_at`/`rules` NULLs.
4. **mediafile** — largest (64+FTS, 4 RG shims, `putByMatch` SELECT-then-put, `FtsColumns` wiring). Do last.

**File ownership (one writer, nd-db only):**
- `album_repo.rs` — add `AlbumRecord` + `put_cols`/`album_row`, rewire `put_sql`/`update_external_info_sql` to take `&AlbumRecord` (keep `put_legs` append-id unchanged).
- `mediafile_repo.rs` — `MediaFileRecord` + `mediafile_row`, rewire `put_sql`; feed `FtsColumns` from `mediafile_fts_columns`.
- `folder_repo.rs` — `FolderRecord` + `folder_row`, rewire `put_sql`.
- `playlist_repo.rs` — `PlaylistRecord` + `playlist_row`, rewire `put_sql`/`put_smart_cols`, move PostMapArgs deletes into the record.
- One line in `lib.rs` **only if** a shared `crate::marshal` module is added — but §1c assigns `marshal.rs` to **S-D2a**, not S-D2b. **S-D2b must reuse S-D2a's shared sorted-col/value unit; it should NOT create `marshal.rs`.** Flag below.
- Tests: extend `tests/runtime_insert_alignment.rs` pattern or add per-entity executed write→read tests.

**Safe sub-slices (if the lane must split):** folder+playlist (no FTS/RG shims) as one lane; album+mediafile (both FTS + RG + participant/tag feeders) as another. Both lanes share the S-D2a core, so they remain ownership-safe (disjoint repo files).

**Minimum executed write→read test per entity** (mirror `runtime_insert_alignment.rs:34-46` harness: temp on-disk DB, `apply_fresh_schema`, `Pool`, `_foreign_keys=on`, `SQLStore`):
- **folder:** seed `library`; `put_sql` a `FolderRecord` with non-empty `image_files` (`["a.jpg","b.jpg"]`) + distinct strings per column; execute `insert`; read back every column via `SELECT *` and assert each value in its own column (rotation-detector, distinct values); assert `image_files` round-trips as JSON; re-run as update path and assert `created_at` is NOT overwritten (update leg drops it) while `updated_at` is.
- **album:** seed `library`; insert `AlbumRecord` with populated `Discs`/`Tags`/`Participants`/`FolderIDs` + RG pointers set & unset; assert (a) `discs`/`tags`/`participants`/`folder_ids` stored as JSON matching S-D1 `sql_json`/`sql_tags`/`sql_participations` output, (b) `full_text`/`search_participants`/`search_normalized` non-empty and equal to `sql_feeders::album_fts_columns`, (c) `rg_album_gain` NULL when `None`, (d) FTS5 `album_fts` row populated by the trigger (proves columns feed search), (e) `releases` defaulted to 0 (never written). `UpdateExternalInfo` leg: run with the 6 external cols only and assert name/title untouched.
- **playlist:** seed `user` (owner_id FK). Smart: `PlaylistRecord` with `rules` set + nonzero `song_count/duration/size`; assert stored `rules`=JSON and the **counters keep their prior values** (not overwritten) — i.e. update leg must not SET `song_count/duration/size`. Non-smart: assert `rules` stored NULL/'' and counters present. `Update` with `cols=["sync"]` asserts only `sync`+`updated_at` change.
- **mediafile:** seed `library`; `putByMatch` with new `path`+`library_id`; assert all 64 cols land correctly (distinct values), 4 RG NULLs, `FtsColumns` from `mediafile_fts_columns`, `media_file_fts` trigger row present, `participants`/`tags` JSON, `birth_time` present on insert but NOT in a subsequent update's SET list.

**FK seed requirements (from `runtime_insert_alignment.rs:92-124`):** `library (id,name,path)` before album/media_file/folder (all `references library`); `user (id,user_name,name,email,password,is_admin,created_at,updated_at)` before playlist (`owner_id NOT NULL references user`). Both helpers already exist in that test file — reuse verbatim.

## 8. Contradictions in the docs

1. **`hydrate-contract.md` §7 (line 168) is stale:** it says *"S-A is not landed in this checkout (HEAD `336f69ba` is docs-only since `12e270f7`)"* and lists `sql_participations.rs` as having "only filter/subquery fns — no marshaler yet." **False now.** HEAD is `4988df71`; `sql_participations.rs` has `marshal_participants` (:319)/`unmarshal_participants` (:349)/`marshal_flat_participants` (:297), `sql_feeders.rs` exists with all feeders, `sql_json.rs` exists with discs/folder_ids/image_files codecs. S-A, S-B, S-D1 are all **landed** (next-work.md :9, :344-347). S-D2b can consume these directly.
2. **`next-work.md` :194-196 ("exactly three hand-written mappers")** and **`hydrate-contract.md` :3-11** were written pre-S-D1; the `marshal_similar_artists` `{:?}` escaping note (artist_repo.rs:502-516) is now superseded by `sql_json.rs` (single serde encoder per the §1b decision). Not a live contradiction, but the "hand-escaped `{:?}`" caveat in hydrate-contract §7:165 no longer reflects the shared `sql_json` encoder.
3. **`releases` album column** appears in DDL but is absent from both docs' entity field lists (they correctly omit it since Go never writes it) — but it is a genuine **DDL column with no Go field** that the "re-derive golden DDL" follow-up (next-work.md :74, :125-126) should flag as an orphan to drop.

## VERIFIED / UNVERIFIED tally

**VERIFIED (executed or direct tree read):**
- All 4 `structs.Map` column sets (executed Go against vendored structs v1.1.0)
- All 4 db-wrapper shims + line refs (album :29-39, mediafile :29-39, folder :25-28, playlist :24-27)
- All 4 `PostMapArgs` bodies + exact add/overwrite/remove lines
- `selectUpdateColumns` `created_at`/`birth_time` drops + `id` re-add (`sql_base_repository.go:587-609`)
- Entity-specific `colsToUpdate` callers (album `:223`, playlist `:435`, rest_adapter)
- Full DDL column set per table + defaults/nullability (`schema_ddl.rs:58,61,64,66`)
- DDL-vs-Go reconciliation incl. `releases` orphan, `average_rating` annotation-only, FTS trigger set (9 triggers, `:79-90`)
- Existing Rust `put_sql`/`put_legs`/`put_cols` line refs; no Album/MediaFile/Folder/Playlist Record exists yet
- Reusable S-D1/S-A/S-B codecs (all fn + line refs confirmed present)
- FK seed requirements (reused from executed test)

**UNVERIFIED / needs S-D2a:**
- **S-D2a's `marshal.rs` sorted-col/value core API is not in this checkout** — the exact seam signature S-D2b's `put_cols` must call is still being built by the concurrent S-D2a lane. S-D2b should code against `artist_repo::put_cols`'s `Vec<(String,Value)>` shape as the provisional contract and adapt when S-D2a lands. **This is the one true blocker for writing S-D2b blindly.**
- Go's `map[int]string` integer-key JSON ordering and participant map iteration ordering (empirically doc-asserted in §1b; S-D1's `sql_json::marshal_discs` already encodes the stringify-then-sort decision — S-D2b just calls it, so no re-verification needed at S-D2b).

**Blockers:** (a) S-D2a core seam signature must land first (S-D2b is explicitly ordered after it — correct); (b) nothing else blocks the write-side contract — all Go/DDL/code facts are verified above.

**Recommended order:** S-D2a core → S-D2b folder → album → playlist → mediafile (each with the executed write→read test above before the next).