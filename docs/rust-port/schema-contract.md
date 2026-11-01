# #S2 — DB schema contract (frozen)

Owner: `#S2` · Replaces: `db/db.go`, `db/migrations/**`, `db/optimize.go`, `db/backup.go`, `db/repair.go`
· Consumed by: `#04` (`nd-dbschema`), `#05`, `#16`, `#17`, `#18`, `#20`, `#23`

This is a **written contract, not code**. It freezes the target table set, FK graph,
registered SQL functions, FTS5 wiring, collation rules, and property-table keys for the
Rust port. Every table cites the Go source that defines its final shape.

## 0. Basis and strategy (settled)

- Fresh service — there are **no existing installs**, so nothing is byte-frozen. The Go
  schema's *final shape* is the starting point, derived from the persistence interfaces
  (`persistence/*_repository.go`) and the newest schema-shaping migrations, **not** from
  the 138-file history (`db/db.go:32-35` embeds only `migrations/*.sql`; `.go` migrations
  carry Go logic, e.g. `db/migrations/20241026183640_support_new_scanner.go:106`).
- Exactly **ONE fresh Rust migration** ships as the basis and is edited in place as the
  schema evolves — **Part C item 16, DECIDED: regenerate** (`docs/rust-port/issues.md:506`).
  The 138-file Go migration history is never replayed; no upgrade path exists.
- Internal PK/column names, ANALYZE scheduling (`db/optimize.go:19-40`), and backup naming
  (`db/backup.go`) are all free — they are implementation, not contract.
- Dialect is SQLite (`db/db.go:27` `Dialect = "sqlite3"`, `db/db.go:28`
  `Driver = Dialect + "_custom"`), opened with the custom driver so per-connection hooks
  can register functions/collations (`db/db.go:44-51`). `:memory:` is rewritten to
  `file::memory:?cache=shared&_foreign_keys=on` and written back into config
  (`db/db.go:54-59`); connection pool is bounded by `conf.MaxOpenConns()`
  (`db/db.go:62`). Default path pragmas (WAL, `busy_timeout=15000`,
  `synchronous=normal`, `_foreign_keys=on`) come from the default DSN in
  `consts/consts.go:14` (`DefaultDbPath`). Migrations run with
  `PRAGMA foreign_keys=off` and re-enable it afterwards (`db/db.go:85-89`).

## 1. Final table set

35 tables total: 32 ordinary + 3 FTS5 virtual. Dropped-from-Go tables and deliberate
fresh-service deltas are listed in §1.2 / §1.3.

### 1.1 Core content tables

| Table | Final shape source | Notes |
| --- | --- | --- |
| `library` | `db/migrations/20240511220020_add_library_table.go:16-25`; stats `db/migrations/20250701010103_add_library_stats.go:16-24`; `total_duration`/`default_new_users` `db/migrations/20250701010108_add_multi_library_support.go:35,41`; PID columns `db/migrations/20260929221042_add_library_pid_columns.sql:3-14`; scan-state columns `db/migrations/20241026183640_support_new_scanner.go:35-38` | `id integer primary key autoincrement`, unique `name`/`path` |
| `media_file` | base `db/migrations/20200130083147_create_schema.go:90-113`; scanner columns `db/migrations/20241026183640_support_new_scanner.go:218-246` (`folder_id`, `pid`, `missing`, `mbz_release_group_id`, `tags`/`participants` JSONB, `bit_depth`, `explicit_status`); sort/collate columns `db/migrations/20241024125533_fix_sort_tags_collation_and_index.sql:154-216`; RG `db/migrations/20230117155559_add_replaygain_metadata.go`; FTS columns `db/migrations/20260220173400_add_fts5_search.go:28,32` | `library_id` FK `db/migrations/20240511220020_add_library_table.go:31-32` |
| `album` | base `20200130083147_create_schema.go:18-35`; reshape `20241026183640_support_new_scanner.go:253-286` (`imported_at`, `missing`, `tags`, `participants`, `folder_ids`, drop `artist`/`artist_id`/`all_artist_ids`/`paths`/`image_files`); RG `db/migrations/20260719005427_add_album_replaygain.go:16-17`; FTS columns `20260220173400_add_fts5_search.go:36,40` | `library_id` FK `20240511220020_add_library_table.go:33-34` |
| `artist` | base `20200130083147_create_schema.go:79-85`; reshape `20241026183640_support_new_scanner.go:289-315` (drop `album_count`/`song_count`/`size`/`similar_artists` text; add `missing`, `similar_artists` JSONB); NOCASE columns `20241024125533_fix_sort_tags_collation_and_index.sql:3-34`; FTS columns `20260220173400_add_fts5_search.go:44` | global stats column was later moved to `library_artist` (`20250701010108_add_multi_library_support.go:59`) |
| `folder` | `db/migrations/20241026183640_support_new_scanner.go:108-126` | `library_id` FK → library on delete cascade; `parent_id`; `image_files` JSONB |
| `tag` | `db/migrations/20241026183640_support_new_scanner.go:74-82` | unique `(tag_name, tag_value)`; genres live here since `20241026183640:85-88` |

### 1.2 Association / junction tables

| Table | Final shape source | Notes |
| --- | --- | --- |
| `media_file_artists` | `20241026183640_support_new_scanner.go:40-55` | FKs → `media_file`, `artist`, both `on delete cascade`; unique `(artist_id, media_file_id, role, sub_role)` |
| `album_artists` | `20241026183640_support_new_scanner.go:57-72`; unique reordered to lead with `artist_id` in `db/migrations/20260810143445_reorder_album_artists_unique_constraint.sql:7-22` | FKs → `album`, `artist` on delete cascade |
| `media_file_tags` | `db/migrations/20260811023338_add_item_genre_tag_indexes.sql:2-10` | `unique (media_file_id, tag_id)`, FKs on delete cascade |
| `album_tags` | `db/migrations/20260811023338_add_item_genre_tag_indexes.sql:12-19` | `unique (album_id, tag_id)`, FKs on delete cascade |
| `library_artist` | `db/migrations/20240511220020_add_library_table.go:38-50`; fixed shape `db/migrations/20260405124200_fix_schema_inconsistencies.sql:15-24`; per-library `stats` `db/migrations/20250701010108_add_multi_library_support.go:46` | unique `(library_id, artist_id)`, FKs on delete cascade |
| `library_tag` | `db/migrations/20250701010108_add_multi_library_support.go:62-70` | PK `(tag_id, library_id)`, FKs on delete cascade |
| `user_library` | `db/migrations/20250701010108_add_multi_library_support.go:17-23` | PK `(user_id, library_id)`, FKs on delete cascade |
| `annotation` | `db/migrations/20250701010102_add_annotation_user_foreign_key.sql:3-16`; `rated_at` `db/migrations/20251109010105_add_annotation_rating_date.sql` | unique `(user_id, item_id, item_type)`; `user_id` FK on delete/update cascade |
| `user_props` | `db/migrations/20210625223901_add_referential_integrity_to_user_props.go:6-14` | PK `(user_id, key)`; FK → `user` on update/delete cascade |
| `playlist_tracks` | `db/migrations/20200516140647_add_playlist_tracks_table.go:13-21`; FKs `db/migrations/20200608153717_referential_integrity.go:60+` | unique `(playlist_id, id)` where `id` = position |
| `playlist_fields` | `db/migrations/20211008205505_add_smart_playlist.go:13-22` | unique `(field, playlist_id)`; FK → `playlist` on update/delete cascade |
| `scrobbles` | `db/migrations/20251206013022_create_scrobbles_table.sql:3-13`; PK added `db/migrations/20260712211040_add_primary_key_and_update_index_for_scrobbles.sql:2-19` | `id integer primary key`, FKs → `media_file`, `user` on delete/update cascade; index `scrobbles_user_time(user_id, submission_time)` |

### 1.3 User-facing / peripheral tables

| Table | Final shape source | Notes |
| --- | --- | --- |
| `user` | base `20200130083147_create_schema.go:163-178`; NOCASE `name` `20241024125533_fix_sort_tags_collation_and_index.sql:246-262`; `token_epoch` `db/migrations/20260822062750_add_user_token_epoch.sql:3` (#S4, load-bearing); `scrobble_filter` `db/migrations/20260815015320_add_user_scrobble_filter.sql:3`; `password` is **argon2id** from day one (#S4/Part C item 6 — no `enc:` payload, no AES-GCM store) | `user_name` unique |
| `playlist` | base `20200130083147_create_schema.go:127-137`; `rules`/`evaluated_at` `20211008205505_add_smart_playlist.go:6-12`; `average_rating` `db/migrations/20260714120000_add_playlist_average_rating.sql:3`; `imported_hash` `db/migrations/20260818002312_add_playlist_imported_hash.sql`; `owner_id` FK (`20211029213200_add_userid_to_playlist.go`); case-insensitive-name index `db/migrations/20260104203627_playlist_case_insensitive_name.sql` | smart-playlist `rules` JSON |
| `playqueue` | final `db/migrations/20250611010101_playqueue_current_to_index.go:15-25` (`current` is integer), FK → `user(id)` on update/delete cascade | `items` JSON id list (canonicalized `20260720015443_uniform_canonical_ids.go:88`) |
| `bookmark` | `db/migrations/20200801101355_create_bookmark_table.go:15-30` | unique `(user_id, item_id, item_type)`; FK → `user` on update/delete cascade |
| `player` | final `db/migrations/20240802044339_player_use_user_id_over_username.go:9-24` (`user_id` FK, drop `user_name`/`type`); `api_key_hash` + unique index `db/migrations/20260924010054_add_player_api_key_hash.sql:3-4` (#S4/Part C item 5: one hash per player, no scopes) | `transcoding_id` FK → `transcoding` on delete restrict (`20200310181627_add_transcoding_and_player_tables.go:33-35`) |
| `transcoding` | `db/migrations/20200310181627_add_transcoding_and_player_tables.go:9-19`; metadata `db/migrations/20260618120509_add_metadata_to_default_transcodings.go`; codec/command fixes `20260307175815`, `20260310113858` | unique `name`, `target_format` |
| `radio` | `db/migrations/20230115103212_create_internet_radio.go:9-18`; uploaded-image column `db/migrations/20260318182414_add_radio_uploaded_image.go`; NOCASE unique name `20241024125533_fix_sort_tags_collation_and_index.sql:154-172` | |
| `share` | `db/migrations/20230119152657_recreate_share_table.go:9-25`; `user_id` FK on delete cascade `db/migrations/20250522013904_share_user_id_on_delete.sql` | `resource_ids` CSV id list (canonicalized `20260720015443:89`) |
| `scrobble_buffer` | fixed shape `db/migrations/20260405124200_fix_schema_inconsistencies.sql:32-48` (adds `id`, drops duplicate `user_id` from the unique) | unique `(user_id, service, media_file_id, play_time)`; `id` unique index `:50` |
| `property` | `db/migrations/20200130083147_create_schema.go:142-147` | key/value; keys in §5 |
| `artwork` | `db/migrations/20260722023032_add_artwork_tables.sql:2-12` | `hash` TEXT PK; binary image metadata |
| `item_artwork` | `20260722023032_add_artwork_tables.sql:14-26`; trace/last-failure JSONB `db/migrations/20260819204637_add_artwork_trace_columns.sql:3-4`; null-failure normalization `db/migrations/20260901225726_normalize_artwork_last_failure.sql` | **`WITHOUT ROWID`**, PK `(item_kind, item_id, image_type)`; `ix_item_artwork_hash` |
| `artwork_queue` | `20260722023032_add_artwork_tables.sql:28-41`; trace JSONB `20260819204637:5` | **`WITHOUT ROWID`**, PK `(item_kind, item_id, image_type)` |

### 1.4 Dropped from the Go final shape — not in the Rust schema

- `search` — merged away by `db/migrations/20200319211049_merge_search_into_main_tables.go`,
  then dropped; replaced by FTS5 (§3).
- `genre`, `media_file_genres`, `album_genres`, `artist_genres` — dropped by
  `db/migrations/20241026183640_support_new_scanner.go:85-88`; genres live in
  `tag` + `media_file_tags`/`album_tags`.

### 1.5 Fresh-service deltas (deliberate, Part-C-driven)

- **`plugin` is DROPPED.** Part C item 14 (`docs/rust-port/issues.md:504`) removes
  `/api/plugin*` and does not port `persistence/plugin_cleanup.go`, and #S3's settled
  config contract carries no `Plugins.*` keys — there is no host left to own the table
  (`db/migrations/20260106000620_create_plugin_table.sql` is therefore not ported).
- **`user.password` is argon2id** from day one; no `enc:` payload, no
  `PasswordsEncryptedKey` property, no AES-GCM store (#S4 / Part C item 6).
- `library.pid_album`/`pid_track`/`scanned_pid_*` columns stand (`20260929221042`);
  the `PIDTrack`/`PIDAlbum` **property** rows were deleted by that same migration
  (`20260929221042_add_library_pid_columns.sql:11-13`) and are not part of the fresh
  property key set.
- Everything else matches the Go final shape above; further deltas must reopen this doc.

## 2. FK graph

All FKs are SQLite-declared and enforced with `foreign_keys=on` (`db/db.go:54-59`,
`consts/consts.go:14`). Conventions from the Go final shape: child → parent
`on delete cascade` everywhere except `player.transcoding_id` (→ `transcoding`,
`on update restrict on delete restrict`, `20200310181627:33-35`).

- `media_file.library_id` → `library.id` **cascade** (`20240511220020:31-32`)
- `media_file.folder_id` → `folder.id` (logical; declared in the model layer —
  `model/mediafile.go` / scanner), `media_file.album_id`/`artist_id`/`album_artist_id`
  are logical references maintained by the scanner, not SQL FKs.
- `album.library_id` → `library.id` **cascade** (`20240511220020:33-34`)
- `folder.library_id` → `library.id` **cascade** (`20241026183640:111-113`)
- `folder.parent_id` → `folder.id` (logical self-reference)
- `media_file_artists.media_file_id` → `media_file.id` **cascade**;
  `.artist_id` → `artist.id` **cascade** (`20241026183640:40-50`)
- `album_artists.album_id` → `album.id` **cascade**; `.artist_id` → `artist.id`
  **cascade** (`20241026183640:57-67`)
- `media_file_tags.media_file_id` → `media_file.id` **cascade**; `.tag_id` → `tag.id`
  **cascade** (`20260811023338:3-8`)
- `album_tags.album_id` → `album.id` **cascade**; `.tag_id` → `tag.id` **cascade**
  (`20260811023338:13-18`)
- `library_artist.library_id` → `library.id` **cascade**; `.artist_id` → `artist.id`
  **cascade** (`20260405124200:17-22`; original `20240511220020:38-50`)
- `library_tag.tag_id` → `tag.id` **cascade**; `.library_id` → `library.id` **cascade**
  (`20250701010108:62-70`)
- `user_library.user_id` → `user.id` **cascade**; `.library_id` → `library.id`
  **cascade** (`20250701010108:17-23`)
- `annotation.user_id` → `user.id` **cascade / update cascade** (`20250701010102:3-16`)
- `user_props.user_id` → `user.id` **cascade / update cascade** (`20210625223901:6-14`)
- `bookmark.user_id` → `user.id` **cascade / update cascade** (`20200801101355:17-21`)
- `playqueue.user_id` → `user.id` **cascade / update cascade** (`20250611010101:18-20`)
- `playlist_tracks.playlist_id` → `playlist.id` **cascade / update cascade** and
  `.media_file_id` → `media_file.id` **cascade** (`20200608153717:60+`)
- `playlist_fields.playlist_id` → `playlist.id` **cascade / update cascade**
  (`20211008205505:15-19`)
- `player.user_id` → `user.id` **cascade / update cascade** (`20240802044339:13-15`);
  `player.transcoding_id` → `transcoding.id` **restrict** (`20200310181627:33-35`)
- `share.user_id` → `user.id` **cascade** (`20250522013904`)
- `scrobble_buffer.user_id` → `user.id` **cascade**; `.media_file_id` → `media_file.id`
  **cascade** (`20260405124200:33-44`)
- `scrobbles.media_file_id` → `media_file.id` **cascade / update cascade**;
  `.user_id` → `user.id` **cascade / update cascade** (`20251206013022:4-11`)
- `artwork_queue`/`item_artwork` have no FKs (PKs over free-form `item_kind`,
  `item_id` strings, `20260722023032:14-41`).

## 3. FTS5 tables and triggers

Contentless FTS5 (`content=''`, `content_rowid='rowid'`,
`tokenize='unicode61 remove_diacritics 2'`) — `db/db.go` does **not** gate FTS at compile
time in the Rust port; the tables are always created (config-level `Search.Backend`
belongs to #S3/#03, `conf/configuration.go:933-940`).

- `media_file_fts(title, album, artist, album_artist, sort_title, sort_album_name,
  sort_artist_name, sort_album_artist_name, disc_subtitle, search_participants,
  search_normalized)` — `20260220173400_add_fts5_search.go:117-123`
- `album_fts(name, sort_album_name, album_artist, search_participants, discs,
  catalog_num, album_version, search_normalized)` — `:130-135`
- `artist_fts(name, sort_artist_name, search_normalized)` — `:142-146`
- Triggers `<table>_fts_ai` (AFTER INSERT), `<table>_fts_ad` (AFTER DELETE, using the
  contentless `INSERT INTO <fts>(<fts>, rowid, …) VALUES ('delete', …)` idiom), and
  `<table>_fts_au` (AFTER UPDATE with a `WHEN OLD.x IS NOT NEW.x …` guard):
  media_file `:193-252`, album `:259-319`, artist `:326-365`.
- The base tables carry the feeder columns `search_participants` (media_file, album)
  and `search_normalized` (all three) — `:28-44`.
- **Repair contract**: `db/repair.go:12,14` enumerates the FTS tables
  (`media_file_fts`, `album_fts`, `artist_fts`) and trigger suffixes (`_ai`, `_ad`,
  `_au`) for rebuild/verify; #04 must port this against the same names.

## 4. Registered SQL functions and collation (per connection)

Both are registered in the driver's `ConnectHook`, i.e. **per connection** — a new Rust
SQLite binding (e.g. `rusqlite` `functions`/`create_collation`) must re-register them on
every connection or old queries silently change meaning (`db/db.go:44-51`).

- **`SEEDEDRAND`** — scalar function from `utils/hasher.HashFunc()`
  (`db/db.go:46`); the Rust `nd-utils::hasher` port must be byte-identical (#01).
- **`NATSORT`** collation — `const NaturalCollation = "NATSORT"`
  (`db/db.go:24`, registered `db/db.go:49`) backed by `utils/natural.CompareFold`
  (`db/db.go:17`); only referenced when `EnableNaturalSorting` is on, but always
  registered. Byte-identical `nd-utils::natural` required (#01).
- **Column collation rules** (from `20241024125533_fix_sort_tags_collation_and_index.sql`):
  `NOCASE` on the `order_*` / `sort_*` columns of `artist`, `album`, `media_file`
  (`:7-8,17-19,86-88,155-160`), `radio.name` (`:157`), `user.name` (`:249`),
  plus `user_username_password` index on `(user_name, password)` (`:263`).
- **Expression sort indexes** (kept in final shape):
  `media_file_sort_title(sort_album_name)` family dropped in
  `20260703013908_optimize_media_file_sort_indexes.sql:22-26` (kept only the composite
  sort indexes `:8-14`); `artist_sort_name`/`album_sort_name` kept as NOCASE expression
  indexes (`20241024125533:33-34,83-84`).
- **`ix_artwork_queue_drain`** covering index —
  `artwork_queue(item_kind, priority DESC, enqueued_at, retry_at)`
  (`20260722023032:41`): `item_kind` leads so each drain pool seeks straight to its own
  kinds; the trailing columns make the `DequeueBatch` scan covering.

## 5. Property-table keys

`property(id, value)` (`20200130083147:142-147`). Final key set from
`consts/consts.go`:

- Boot/setup: `InitialSetup` (`:15`), `FullScanAfterMigration` (`:16`),
  `PlaylistsImportPending` (`:19`)
- Scanner state: `LastScanError` (`:20`), `LastScanType` (`:21`),
  `LastScanStartTime` (`:22`)
- DB maintenance (backs `db/optimize.go` scheduling): `LastDBAnalyzeAt` (`:23`),
  `LastDBAnalyzeAttemptAt` (`:24`), `DBAnalyzePending` (`:25`),
  `DBAnalyzeFailureCount` (`:26`)
- Artwork: `ArtConfFingerprint` (`:29`)
- Auth (#S4): `JWTSecret` (`:33`), `JWTPublicSecret` (`:34`) — two distinct signing
  secrets, session vs public-link tokens
- Integration: `JellyfinServerID` (`:67`), `InsightsID` (`:162`)
- **Dead keys, never created in the fresh schema**: `PasswordsEncryptedKey`
  (`consts/consts.go:50`, dead per #S4/Part C item 6) and `PIDTrack`/`PIDAlbum`
  (deleted by `20260929221042_add_library_pid_columns.sql:11-13`, replaced by
  `library` columns).

## 6. ID columns

- Entity ids are **22-char base62 encodings of 128-bit values**
  (`model/id/id.go:9-12` `Encode`, `:33-42` `NewHash` = MD5 over args joined by U+200B;
  `consts/consts.go:113` `Zwsp`). `NewTagID` lower-cases name+value
  (`model/id/id.go:45-47`).
- Every id-bearing column is enumerated in
  `db/migrations/20260720015443_uniform_canonical_ids.go:54-80` (`idColumns`, the
  canonical-id guard list) and `:84-92` (`embeddedIDColumns`: `playqueue.items`,
  `share.resource_ids`, `plugin.users` (dead — table dropped), `playlist.rules`).
  The fresh schema creates these columns already-canonical; the Rust migration must
  reproduce that shape and keep the guard lists consistent with it.
- Sentinels stay constants, not derivations: `VariousArtistsID`
  (`consts/consts.go:214`), `UnknownArtistID` (`:218`) — #02 owns the scheme.
- `library.id` is the sole `integer primary key autoincrement`
  (`20240511220020:17`); `scrobbles.id` is `integer primary key` (`20260712211040:3`),
  `folder.id`/`tag.id` are 22-char base62.

## 7. WITHOUT ROWID / composite PKs

- `item_artwork` — `WITHOUT ROWID`, PK `(item_kind, item_id, image_type)`
  (`20260722023032:14-25`)
- `artwork_queue` — `WITHOUT ROWID`, PK `(item_kind, item_id, image_type)`
  (`20260722023032:28-37`)
- `user_library` — PK `(user_id, library_id)` (`20250701010108:17-23`)
- `library_tag` — PK `(tag_id, library_id)` (`20250701010108:62-70`)
- `user_props` — PK `(user_id, key)` (`20210623155401_add_user_prefs_player_scrobbler_enabled.go:15-24`)
- `scrobble_buffer` — unique `(user_id, service, media_file_id, play_time)` plus
  unique index on `id` (`20260405124200:32-50`)
- `playlist_tracks` — unique `(playlist_id, id)` where `id` = track position
  (`20200516140647:20-21`)
- `bookmark`, `annotation` — unique `(user_id, item_id, item_type)`

Everything else keeps a single-column PK (`id`) with `rowid` storage, which the FTS5
`content_rowid='rowid'` wiring depends on (§3).

## 8. Timestamps and JSON columns

- Timestamp columns are SQLite `datetime`/`TIMESTAMP` **strings in space-separated
  format** (never `T`-separated RFC3339, which breaks string ORDER BY) — see
  `db/migrations/20260316000000_normalize_timestamps.sql:5-40` for the rule and the
  column-by-column list.
- JSON columns: `media_file.tags`, `media_file.participants`, `album.tags`,
  `album.participants`, `album.discs`, `album.folder_ids`, `artist.similar_artists`,
  `folder.image_files`, `item_artwork.trace`, `item_artwork.last_failure`,
  `artwork_queue.trace`, `playlist.rules`, `playqueue.items`,
  `media_file.lyrics`. `plugin.manifest`/`config`/`users`/`libraries` die with the
  dropped table (§1.5).