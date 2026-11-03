# Rust port — issue plan (paste-onto-your-tracker edition)

Repo: this navidrome fork (Go server, GPL-3.0). Companion doc: `docs/rust-port/component-map.md`
(component graph, IDs `C01`–`C33`). This file is the build-order issue list: 4 design issues
(`#S1`–`#S4`) that freeze shared contracts, then the numbered implementation issues `#01`–`#34`
(of which #25 Subsonic shim, #29 plugin host, #30 Jellyfin API, #31 sonic similarity are CANCELLED and #34 backgrounds
is DROPPED — 29 live issues), in build order. Every claim is a path/line citation from a read-only pass
over this checkout; the Progress ledger below records what has since been executed and gate-verified.

## Revision (post-milestone-0) — settled decisions

1. **ONE canonical API** = the existing native surface (`server/nativeapi/**`, mounted at `consts/consts.go:58` `/api`). Reason: the owners' Flutter client speaks it; Subsonic becomes a baseline reference, not the trunk.
2. **Subsonic wire layer CANCELLED — clean API break** (#25). The canonical API is *inspired by* Subsonic, not bound to it: keep its sound semantics (typed-ID prefixes `al-`/`mf-`/`pl-`, artwork-ID composition, transcode-decision split), drop the wire anachronisms (XML, auth-in-query, `u/t/s`, `.view`, camelCase verbs). Reason: a faithful `t = md5(password+salt)` shim requires the reversible password store back — re-importing model B's removed debt — and the `apiKey` alternative works only for clients that can send `apiKey`, which major Subsonic clients cannot. Third-party Subsonic clients will not work against this server; that is a deliberate product decision. The 126 goldens become reference data only, and a future community adapter would consume the published OpenAPI spec, not server internals.
3. **WASM plugin host cancelled** (#29). Reason: the Extism/wazero host ABI is the largest single lane and delivers nothing the canonical API or clients need; `plugins/capabilities/*.yaml` remain as the trait spec for a future Rust-native layer.
4. **Credential model B is the settled default** (#S4): hashed passwords (argon2id) + long-lived hashed API keys + JWT sessions with `user.token_epoch` revocation. There is no reversible AES-GCM store anywhere on the trunk; decryption exists only inside the one-time `migrate-credentials` path, then is deleted. With #25 cancelled, model A has no host at all.
5. **JS UI dropped unconditionally** (#28): the bundled react-admin UI is deleted (the Flutter client comes after the server translation); only the share-page HTML contract survives.
6. **No upgrade path, fresh service**: there are NO existing installs to migrate. The trunk ships argon2id from day one; no `migrate-credentials` exists — first-run bootstrap (initial admin creation, `server/initial_setup.go` pattern) replaces it. The whole byte-compat apparatus is dropped: DB schema is a free set (one fresh migration, edited as the schema evolves), config keys are free (clean TOML, no deprecation table), and the ID scheme inherits the 22-char base62 + typed-prefixes shape because it is good design, not for compat.
7. **Jellyfin API dropped** (#30 CANCELLED): `server/jellyfin/**` and the `jellyfin` JWT audience are not ported; Quick-Connect survives re-owned by #27 (device pairing, not Jellyfin-specific).

Fields per issue: **Build** (what to write, in order) · **Test** (strategy + Go oracle) · **Risk**
(`CRITICAL` byte-exact wire oracle; `HIGH` compat/process/FFI/ABI; `MED` behavior-preserving;
`LOW` free; `DEFERRABLE` stub, don't block) · **dependsOn** (issue numbers here — start only when all
are green) · **Owns** (Rust paths this issue may create/edit) · **Replaces** (Go paths retired).

Global rules:
- Crate boundary = ownership unit. No path appears under two issues; if you want another issue's
  path, you are consuming it — file a dependency, do not edit.
- Every crate is GPL-3.0 (`LICENSE:1-4`). Third-party license planes stay separate. The extism/wazero plane (`extism-pdk = "1.2"` at `plugins/pdk/rust/nd-pdk/Cargo.toml:17`, `go.mod:20,58`) is no longer ported at all — #29 is CANCELLED; the taglib fork stays pinned at `go.mod:6`.
- Wire goldens are **data** and, post-clean-break, gate **nothing** (#25 is CANCELLED): `server/subsonic/responses/.snapshots/` stays in-tree as reference data — a recorded behavioral spec (field shapes, streaming/Range/ETag semantics) to consult while authoring the canonical API. Regeneration stays Go-only (`Makefile:167-169`).
- The trunk's oracle is **native-API response-shape goldens captured from the Go binary before the issue opens** (#27, and likewise #28); nothing on the trunk is gated by the Subsonic goldens.
- Gate #S1–#S4 with an approval step: they change contracts nothing else absorbs.

---

## Progress ledger (updated 2026-11-03 — this is the port's live state, not the plan)

Re-validate with: `cd rust && cargo fmt --all -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-targets --all-features`
(last run: fmt 0 diffs, clippy 0 errors, 60 test groups / 0 failures — nd-ffmpeg 51 lib + 2 golden, nd-dbschema 3 lib + 7 golden, nd-model 5 lib).

| issue | crate | state |
|---|---|---|
| #S1–#S4 (contracts) | workspace manifest + docs | DONE |
| #01 | `nd-utils` (22 modules) | DONE |
| #02 | `nd-id` (id/artwork/participants) | DONE |
| #03 | `nd-conf` (keys/config/dir/bytes/duration/crontab/consts) | DONE |
| #04 | `nd-dbschema` (fresh schema DDL + open/init + `SEEDEDRAND`/`NATSORT` + marker-guard migration runner + `goose_db_version`/full-scan property + 3 idempotence tests) | **PARTIAL** — fresh schema + runner green (10 tests); optimize/backup/repair (FTS5 reindex, `db/repair.go`) still pending |
| #08 | `nd-tagreader` (storage registry, extractors, gotaglib transforms, shim `RawTags` sidecars + 12 tests) | DONE — **shim backend** chosen (pure-Rust taglib unavailable offline; C binding would fork the pinned go-taglib raw surface). Goldens emitted by the Go oracle harness `adapters/gotaglib/golden_dump_test.go` (`ND_GOLDEN_DUMP=1 go test ./adapters/gotaglib/ -run TestGoldenDump -v` → sidecars under `rust/crates/nd-tagreader/tests/fixtures/`); regenerate if the pinned fork bumps |
| #09 | `nd-lyrics/src` (lrc/srt/ttml/normalize/parse/lyricsfile/model DTOs) | DONE (parsers half; #22 fetch/merge is stubbed inside the crate) |
| #10 | `nd-playlists/src/parsers` (m3u/nsp/import + playlist DTO) | DONE (parsers half; #23 store/import stubbed) |
| #11 | `nd-events` (broker/event/sse) | DONE |
| #12 | `nd-sched` | **PARTIAL** — only `crontab.rs`; logging/maintenance pending |
| #07 | `nd-ffmpeg` (args tables byte-exact, resolved ffmpeg lookup, ffprobe JSON parser, FLAC STREAMINFO patcher + 13 goldens, `ffCmd` process type with `process_group(0)` spawn / SIGKILL+reap cancel / 4096 stderr cap, encoder-probe memo, `ProbeAudioStream` 5s-bounded launch, `Transcode`/`ExtractImage`/`ConvertAnimatedImage` argv) | **PARTIAL** — 50 lib + 2 golden tests green, gate clean. Pending from the Test block: `tests/mock_ffmpeg.go` rebuilt as a Rust mock binary (lanes used `/bin/sh` + `/bin/echo` stand-ins instead), and the container fixtures (`test.aiff m4a ogg opus wav wma`, invalid-frame, symlink chain) for the live-ffmpeg cases. Deviations pinned in-module: no `killpg` (std/tokio gap) — signal goes to the fresh group's root pid; probe `-encoders` 5s bound only on the probe path; `patchFLACDuration` deferred to the consumer (#14) because the port cannot patch a stream it never sees |
| #06 | `nd-model` (entity DTOs: album/annotation/entity/wire serde shapes with Go camelCase renames, 5 tests) | **PARTIAL** — DTO half landed; criteria DSL JSON form and `model/{searchable,get_entity,library_matcher}.go` pending |
| #05, #13–#24, #26–#28, #32, #33 (all remaining live issues; #29–#31 CANCELLED, #34 DROPPED) | remaining `nd-*` crates | SCAFFOLD (`OWNED_BY_ISSUE` marker; instantiates, no behavior) |

Next in build order: #07 test-plane tail (mock_ffmpeg as a Rust mock binary was dropped by the last lane; `/bin/sh`+`/bin/echo` stand-ins + the new live-container fixture pins cover the launch paths instead — the Go stand-in golden remains uncovered), then **#05 `nd-db`** (repository layer, gated by #04 — runner now green) and finish #04 (optimize/backup/repair) and #06 (criteria DSL). #12 `nd-sched` logging/maintenance depends on #07 + #13 + #06 + #05.

---
## Part A — shared artifacts first (design issues, blocking)

### #S1 — Fix the Rust workspace and crate graph

- **Build**: one `nd-*` workspace member per owning issue below: `nd-utils nd-id nd-conf nd-dbschema
  nd-db nd-model nd-ffmpeg nd-tagreader nd-lyrics nd-playlists nd-events nd-sched nd-auth nd-stream
  nd-mapping nd-scanner nd-artwork nd-public nd-api nd-players nd-ui-contract
  nd-cli nd-metrics nd-agents nd-http nd-scrobbler` — **26 owned crates** after the
  clean break and no-upgrade-path decisions (`nd-native-api` → `nd-api`; `nd-subsonic-compat`, `nd-sonic`, `nd-plugin-host`, `nd-backgrounds`, `nd-jellyfin` all removed).
  Crate-vs-issue arithmetic, frozen here: 34 numbered issues − #25/#29/#30/#31 CANCELLED − #34 DROPPED = 29 live issues,
  and 26 crates because three pairs share a crate: #14 inside `nd-stream` (#15), #09 inside `nd-lyrics` (#22), #10 inside
  `nd-playlists` (#23). 29 − 3 = 26. ✓
  `nd-mcp` is deliberately NOT a workspace member — it is a separate client artifact talking to the documented canonical API (see "Not ported"). Freeze the DI replacement
  once: Go builds its graph in `cmd/wire_gen.go` / `cmd/wire_injectors.go`, with the datastore
  singleton at `model/datastore.go` and provider selection at `core/wire_providers.go`
  (`persistence/persistence.go:17-40` declares repository interfaces); Rust replaces codegen with
  explicit construction (typed service struct per crate, wired in `nd-cli`'s `main.rs`).
  Decide here, once: crate naming/granularity, error type, async runtime, trait-object vs generic
  services — later issues inherit, they may not diverge.
- **Test**: compile-only — `cargo build`, `cargo fmt --all -- --check`,
  `cargo clippy --all-targets --all-features -D warnings`; one Arrange-Act-Assert stub per crate
  proving it instantiates without pulling the world in.
- **Risk**: MED (design freeze, no behavior).
- **dependsOn**: —
- **Owns**: `Cargo.toml` (workspace manifest), `docs/rust-port/component-map.md`, `docs/rust-port/issues.md`
- **Replaces**: DI codegen — `cmd/wire_gen.go`, `cmd/wire_injectors.go`, `core/wire_providers.go`,
  `model/datastore.go`

### #S2 — Freeze the DB schema contract

- **Build**: a written contract (no code): target table set, FK graph, registered SQL funcs, and the
  MUST-RESOLVE strategy question. Hard set, from `db/db.go:27` (`Dialect="sqlite3"`), `db/db.go:28`
  custom driver, `db/db.go:85-89` pragmas, `db/db.go:54-59` `:memory:` rewrite, `db/db.go:62`
  `SetMaxOpenConns`: table list minus dropped `search` (`20200319211049`) and `genre*` (dropped by
  `20241026183640:85-88`); FK graph incl. `library_id` FKs and `on delete cascade`
  (`20240511220020:17-56`, `20241026183640:40-125`); 22-char varchar entity ids
  (`model/id/id.go:1-48`); FTS5 tables `media_file_fts/album_fts/artist_fts` with `_ai/_ad/_au`
  triggers (`db/migrations/20260220173400_add_fts5_search.go:24-`, repaired by `db/repair.go:12,14`);
  `WITHOUT ROWID` + composite PKs + covering index
  `ix_artwork_queue_drain(item_kind, priority DESC, enqueued_at, retry_at)`
  (`db/migrations/20260722023032_add_artwork_tables.sql:1-46`); canonical-id column set
  (`20260720015443_uniform_canonical_ids.go:55`); `property` keys
  (`consts/consts.go:15-29,33-34,49-51,65-67,162`). Registered SQL funcs must be re-registered per
  connection in the Rust SQLite binding: `SEEDEDRAND` (`db/db.go:46`, from `utils/hasher`), `NATSORT`
  (`db/db.go:49`, `utils/natural.CompareFold`).
- **Fresh service — no compat set**: there are NO existing installs, so nothing is byte-frozen. The
  Go schema's FINAL shape is the starting point (derive from the repos + the last schema-shaping
  migrations, not the 138-file history: `.go` migrations carry Go logic, `20241026183640.go:106`,
  `db/db.go:32-35` embeds only `migrations/*.sql`); edit freely from there. Exactly ONE fresh Rust
  migration ships as the basis and is edited in place as the schema evolves. Internal PK/column
  names, ANALYZE scheduling (`db/optimize.go:19-40`), backup naming (`db/backup.go`) are all free.
- **Credential columns** (#S4, settled): `user.password` is argon2id from day one (no reversible
  `enc:` payload, no `PasswordsEncryptedKey`); `user.token_epoch`
  (`db/migrations/20260822062750_add_user_token_epoch.sql`) and `player.api_key_hash` + unique index
  (`db/migrations/20260924010054_add_player_api_key_hash.sql`) are load-bearing.
- **Test**: none — the artifact must cite every table to its source (Go schema or explicit change)
  so #04 can act on it.
- **Risk**: design issue, MED (a regeneration, not a compat port).
- **dependsOn**: #S1 · **Owns**: `docs/rust-port/schema.md` · **Replaces**: — (consumed by
  #04, #05, #16, #17, #18, #20, #23)

### #S3 — Freeze the config-format contract

- **Build**: enumerate keys verbatim from `conf/configuration.go:33-165` (`configOptions`): root keys
  `Address Port UnixSocketPerm EnforceNonRootUser MusicFolder DataFolder CacheFolder DbPath LogLevel
  BaseURL/BasePath/BaseHost/BaseScheme TLSCert/Key Enable* TranscodingCacheSize ImageCacheSize
  FFmpegPath MPVPath MPVCmdTemplate *ArtPriority LyricsPriority MaxImage(Upload)Size DefaultTheme
  AuthRequestLimit PasswordEncryptionKey Agents …` plus nested sections `Search Matcher ExtAuth
  Plugins HTTPHeaders Prometheus Scanner Jukebox Backup PID Inspect Subsonic Transcoding LastFM Deezer
  ListenBrainz Jellyfin Tags` and `Dev*` (`:134-164`, incl. `DevAPIv1`), free-form prefixes `Tags.`
  and `DevLogLevels.` (`:766-768`), non-settable marker (`configuration.go:34` `ConfigFile`).
  Verbatim is required because `release/linux/navidrome.toml`, `contrib/*`, `Dockerfile:208`
  (`ND_CONFIGFILE`) are artifacts users already have. Record the deprecation table to implement, not
  skip: 10 renames (`conf/configuration.go:539-549`) — `Scanner.GenreSeparators`,
  `Scanner.GroupAlbumReleases`, `DevEnableBufferedScrobble`, `SearchFullString→Search.FullString`,
  `ReverseProxyWhitelist→ExtAuth.TrustedSources`, `ReverseProxyUserHeader→ExtAuth.UserHeader`,
  `HTTPSecurityHeaders.CustomFrameOptionsValue→HTTPHeaders.FrameOptions`,
  `CoverJpegQuality→CoverArtQuality`, `SimilarSongsMatchThreshold→Matcher.FuzzyThreshold`,
  `EnableTranscodingCancellation→Transcoding.EnableCancellation` — and removals `Spotify.ID`,
  `Spotify.Secret` (`:552`), with "explicit new name wins" precedence (`:623-628`, `:630`).
  Record the semantics layer: case-insensitive keys (`:707-780`), INI `[default]` merge (`:784-800`,
  `conf/testdata/cfg.ini`), ND_-env remap with duplicate hard error (`:589-617`, `:690-692`),
  unknown-key reporting (`:676-690`), value types — `Dir` lazy `MkdirAll` (`conf/dir.go:1-77`),
  byte sizes (`:524-526`, `:857-867`), durations, crontab validation (`:905`),
  `Search.Backend ∈ {fts, legacy}` fallback (`:933-940`).
- **Test**: none — contract must be checkable line-range by line-range against `conf/configuration.go`.
- **Risk**: design issue, HIGH stakes for existing installs.
- **dependsOn**: #S1 · **Owns**: `docs/rust-port/config.md` · **Replaces**: — (consumed by #03, #14,
  #17, #18, #24, #32)

### #S4 — Credential model (settled: B — clean break, no model A anywhere)

- **Build**: a settled decision record — model B is the trunk credential model; there is no open A-vs-B question left.
  - **Trunk credentials (B)**: long-lived hashed API keys + JWT session tokens, revoked per user through `token_epoch` (`db/migrations/20260822062750_add_user_token_epoch.sql`, `persistence/user_repository.go:129-146`, `server/auth.go:320`).
  - Passwords are hashed (argon2id) by default. There is **no reversible AES-GCM password store on the trunk**.
  - **Nothing of model A exists.** With #25 CANCELLED and a fresh service (no existing installs), `t = md5(password+salt)` (`server/subsonic/middlewares.go:281`), `enc:` passwords (`:243`), the `subsonic`/`jellyfin` JWT audiences, and the AES-GCM reversible store keyed `sha256(PasswordEncryptionKey | "just for obfuscation")` (`utils/encrypt.go:15,40`, `core/auth/auth.go:198-205`, `consts/consts.go:47-50`, `persistence/user_repository.go:389,394-450,453,464`) are **never ported**. Passwords are argon2id from day one; no migration command exists. Prefer vetted Rust crates over hand-rolled crypto (`argon2`, `jsonwebtoken`/`jose`, `rand`/`getrandom`) — nothing here is novel cryptography.
  - **First-run bootstrap** replaces any migration: initial admin creation follows the `server/initial_setup.go` pattern. There is no upgrade path and no stored-ciphertext question.
  - Sub-decisions that are not A-specific and must not be silently relaxed: property keys `JWTSecret/JWTPublicSecret/PasswordsEncryptedKey` (`consts/consts.go:33,34,50`); two signing secrets — `TokenAuth` rotation vs `PublicTokenAuth` survival (`core/auth/auth.go:23-29,50,172`); audiences `jellyfin|subsonic|native` (`core/auth/auth.go:33-36`); claim shape (`core/auth/claims.go:11,31-70`); `ExtAuth.TrustedSources` CIDR + unix-`@` case (`server/auth.go:212,418,427`); rate-limit semantics (`server/auth.go:100` 8KiB body, `server/server.go:212`, `server/subsonic/auth_limiter.go:15`, form-body cap 10MB `middlewares.go:34`).
  - `PasswordEncryptionKey` (`consts/consts.go:50`) is dead config — not carried into the fresh TOML.
- **Test**: none (decision record) — but it must name the gated consumers so the tracker can block them.
- **Risk**: design issue, HIGH — gates #13, #14, #18, #19, #24, #26, #27, #28 and DB import (#25/#29/#30/#31 cancelled and own nothing).
- **dependsOn**: #S1 · **Owns**: `docs/rust-port/credentials.md` · **Replaces**: —

Already-fixed shared artifacts needing no design issue: the **ID scheme** (`model/id/id.go` byte-exact
base62/MD5, sentinels `consts/consts.go:214,218`, PID rules `consts/consts.go:157-159`) — owned by
#02; **wire fixtures** (126 goldens, 63 shape variants × JSON/XML, `.gitattributes:1 * eol=lf`) —
reference data only — nothing gates on them (#25 is CANCELLED); #27 and #28 capture their own goldens from the Go binary; **tag vocabulary** (`resources/mappings.yaml`, `model/tag_mappings.go`)
— shipped with #03/#16; **test mocks as spec** (`tests/mock_ffmpeg.go`, `tests/mock_*_repo.go`,
`tests/test_helpers.go`, `tests/navidrome-test.toml`) — consumed by #07, #05, #15.

---

## Part B — implementation issues in build order

### #01 — Pure primitives (`nd-utils`)
- **Build**: pure first — `slice str hasher natural collation shellquote nanoid number gg chrono merge
  jsoncommentstrip index_group_parser` — then effectful `run singleton cache httpclient netguard
  limiter files gravatar encrypt req ioutils pl random context`. Compat traps: `utils/hasher` backs
  the `SEEDEDRAND` SQLite func (`db/db.go:46`), `utils/natural.CompareFold` backs `NATSORT`
  (`db/db.go:49`) — byte-identical or old queries change meaning.
- **Test**: port each `utils/*_test.go` (`utils/str/str_test.go` 14K, `utils/slice/slice_test.go`,
  `utils/shellquote/shellquote_test.go`); property tests for sort/collation over a Go string corpus.
- **Risk**: LOW–MED (the two registered funcs HIGH).
- **dependsOn**: #S1 · **Owns**: `nd-utils/**` · **Replaces**: `utils/**` (21 dirs)

### #02 — ID scheme (`nd-id`)
- **Build**: 22-char base62 ids; `NewHash` = MD5 over args joined by U+200B (`model/id/id.go:1-48`);
  `model/artwork_id.go`, `model/participants.go`; PID rules `consts/consts.go:157-159` and
  `PIDTrack`/`PIDAlbum` overrides (`20241026183640.go:96-97`). Emit sentinels as constants, not
  runtime derivations: `VariousArtistsID = "63sqASlAfjbGMuLP4JhnZU"` (`consts/consts.go:214`),
  `UnknownArtistID` (`consts/consts.go:218`) — legal only while `NewHash` stays byte-exact.
- **Test**: golden round-trips against ids produced by the Go build; length/alphabet assertions.
- **Risk**: HIGH (every stored id in existing DBs).
- **dependsOn**: #S1 · **Owns**: `nd-id/**` · **Replaces**: `model/id/id.go`, `model/artwork_id.go`,
  `model/participants.go`

### #03 — Config loader and constants (`nd-conf`)
- **Build**: TOML + INI loader with the precedence ladder from #S3 (file → ND_-env remap with
  duplicate hard error `conf/configuration.go:589-617`/`:690-692` → INI `[default]` merge `:784-800`),
  case-insensitive keys, unknown-key reporting (`:676-690`); the whole `configOptions` key set; the
  deprecation/removal table; value types (`Dir` `conf/dir.go:1-77`, byte sizes, durations, crontab
  validation `:905`, `Search.Backend` fallback `:933-940`); constants as a typed table
  (`consts/consts.go`, 234 LOC); build-tag equivalents (`conf/buildtags/{netgo,sqlite_fts5,doc}.go`)
  and MIME gating (`conf/mime/mime_types.go`, `resources/mime_types.yaml`).
- **Test**: port `conf/configtest/`; key-set snapshot diff against `consts/consts.go`; assert
  `release/linux/navidrome.toml` and `contrib/*` load unchanged; deprecation-pair table.
- **Risk**: MED (key spelling HIGH).
- **dependsOn**: #S1, #S3 · **Owns**: `nd-conf/**` · **Replaces**: `conf/**`, `consts/consts.go`

### #04 — Schema, migrations, DB maintenance (`nd-dbschema`)
- **Build**: the ~25 schema-shaping files of ~138 in `db/migrations/**` —
  `20200130083147_create_schema.go`, `20240511220020_add_library_table.go`,
  `20241026183640_support_new_scanner.go`, `20260220173400_add_fts5_search.go`,
  `20260722023032_add_artwork_tables.sql`, `20260720015443_uniform_canonical_ids.go` — plus runner,
  open/init (`db/db.go:27,28,32-35,54-59,62,85-89`), `db/optimize.go` ANALYZE scheduling, `db/backup.go`,
  `db/repair.go` (`:12,14` FTS5 repair). Register `SEEDEDRAND` (`db/db.go:46`) and `NATSORT`
  (`db/db.go:49`) per connection. Resolve #S2's strategy question before writing.
- **Test**: DB byte-compat harness — build a DB with the Go binary, open it in Rust, assert table set /
  FK graph / FTS5 triggers / `property` keys survive; per-migration shape goldens;
  `navidrome-test.toml` `DbPath=:memory:` counterpart for unit runs.
- **Risk**: HIGH.
- **dependsOn**: #S1, #S2, #S3, #S4, #01, #02 · **Owns**: `nd-dbschema/**` · **Replaces**: `db/db.go`,
  `db/migrations/**`, `db/optimize.go`, `db/backup.go`, `db/repair.go`

### #05 — Repository layer (`nd-db`)
- **Build**: `persistence/persistence.go` interface layer (`:17-40`), `sql_base_repository.go:28-49`
  builder, 24 `*_repository.go`, `sql_{search,search_fts,search_like,bookmarks,participations,tags,
  annotations,restful}.go`, `criteria_sql.go` (904 LOC), `plugin_cleanup.go`; keep `WithTx/WithTxRetry`
  retry-on-contention and the `SQLStore` indirection the 24 repository interfaces expose. Deliberate
  split: `persistence/criteria_sql.go` lives here, `model/criteria/` (the DSL) lives in #06 — the
  DSL↔SQL bridge is the only touch point.
- **Test**: port `tests/mock_*_repo.go` behavioral specs (`mock_user_repo.go`, `mock_scrobble_buffer_repo.go`,
  `mock_tag_repo.go`, `mock_transcoding_repo.go`, `mock_share_repo.go`, `mock_user_props_repo.go`) as
  Rust fixtures; query tests against a SQLite harness modeled on
  `server/subsonic/e2e/e2e_suite_test.go:417`.
- **Risk**: HIGH (this issue gates ~20 later ones).
- **dependsOn**: #04, #02 · **Owns**: `nd-db/**` · **Replaces**: `persistence/**`

### #06 — Model / DTO layer (`nd-model`)
- **Build**: interface-only structs mirroring `model/*.go`: entity DTOs, `model/criteria/*` (search DSL +
  JSON form), `model/{searchable,get_entity,library_matcher}.go`, `model/tag.go`, `model/annotation.go`,
  `model/scanner.go`, lyric DTOs; tag-vocabulary loader contract (`model/tag_mappings.go`, also consumed
  at `model/criteria/fields.go:147`). Pure transforms only — the SQL half is #05's.
- **Test**: JSON round-trips against shapes pinned in `server/subsonic/responses/.snapshots/`; port
  `model/criteria/*_test.go`; field-order tests where the wire depends on order.
- **Risk**: MED.
- **dependsOn**: #S1, #02, #03 · **Owns**: `nd-model/**` · **Replaces**: `model/*` (DTO half),
  `model/criteria/*`, `model/{searchable,get_entity,library_matcher,errors}.go`

### #07 — ffmpeg process adapter (`nd-ffmpeg`)
- **Build**: `core/ffmpeg/ffmpeg.go` — `Transcode / ExtractImage / ConvertAnimatedImage / ProbeAudioStream /
  CmdPath / IsAvailable`; FLAC `STREAMINFO` patching (`core/ffmpeg/flac_streaminfo.go`). Hard requirement:
  cancel kills the child process group, no zombies, no orphaned stdout.
- **Test**: `tests/mock_ffmpeg.go` rebuilt as a Rust mock binary; `tests/fixtures/**` containers
  (`test.aiff m4a ogg opus wav wma`, invalid-frame, symlink chains); golden patched FLAC header; cancel-soak
  asserting zero surviving PIDs.
- **Risk**: HIGH.
- **dependsOn**: #S1, #01 · **Owns**: `nd-ffmpeg/**` · **Replaces**: `core/ffmpeg/**`

### #08 — Tag extractor (`nd-tagreader`)
- **Build**: `adapters/gotaglib/gotaglib.go`, `core/storage/**`, `core/storage/local/extractors.go` —
  `Extractor{Parse → map[string]Info, Version}`, `RegisterExtractor("taglib")`. Go pins the fork
  `github.com/deluan/go-taglib` (`go.mod:5-6`); choose once here: C bindings, pure Rust, or a shim —
  the contract above must survive whichever you pick.
- **Test**: per-container expected `Info` maps over `tests/fixtures/**`; diff against output produced by the
  Go build (Go tags are the oracle).
- **Risk**: HIGH (FFI/WASM equivalence unresolved).
- **dependsOn**: #S1, #01 · **Owns**: `nd-tagreader/**` · **Replaces**: `adapters/gotaglib/**`, `core/storage/**`

### #09 — Lyrics parsers, pure half (`nd-lyrics/src/parsers`)
- **Build**: `model/lyrics_{lrc,srt,ttml}.go`, `model/lyrics_normalize.go`, `model/lyrics_parse.go` — parse +
  normalize only. BOM and time-axis normalization are the compat traps. Fetch/merge is #22.
- **Test**: parser goldens from `tests/fixtures/` (LRC BOM case); normalize round-trip idempotence.
- **Risk**: MED.
- **dependsOn**: #S1, #01 · **Owns**: `nd-lyrics/src/parsers/**` · **Replaces**: `model/lyrics_*.go`

### #10 — Playlist parsers (`nd-playlists/src/parsers`)
- **Build**: `core/playlists/{parse_m3u,parse_nsp}.go` and import semantics (`PlaylistsImportPending` key,
  `consts/consts.go:19`); the `rest_adapter.go` shape. Repo wiring lands in #23.
- **Test**: parser table (extended M3U directives, BOM, CRLF, nested playlists, comments); NSP branch golden;
  `model/playlist.go` DTO round-trip.
- **Risk**: MED.
- **dependsOn**: #S1, #01, #06 · **Owns**: `nd-playlists/src/parsers/**` · **Replaces**: `core/playlists/{parse_m3u,parse_nsp,import}.go`

### #11 — Events / SSE (`nd-events`)
- **Build**: `server/events/*` — SSE `/api/events`, `Event`/`Broker` broadcast, drop-on-disconnect.
- **Test**: frame-shape test for event names scanner/artwork workers emit later; broker fan-out table;
  disconnect cancellation.
- **Risk**: LOW.
- **dependsOn**: #S1, #03 · **Owns**: `nd-events/**` · **Replaces**: `server/events/*`

### #12 — Scheduler, logging, maintenance (`nd-sched`)
- **Build**: `scheduler/{scheduler,crontab_schedule,log_adapter}.go`, `log/{log,formatters,journal,redactrus}.go`,
  `core/maintenance.go`. Crontab parsing must agree with the `ScanSchedule` validator path
  (`conf/configuration.go:905`). Keep the redaction set (`log/redactrus.go`) — user-visible.
- **Test**: port `scheduler/scheduler_test.go` and `crontab_schedule_test.go` (5.8K); redaction table;
  maintenance-op exit codes (CLI wiring deferred to #33).
- **Risk**: LOW.
- **dependsOn**: #S1, #03 · **Owns**: `nd-sched/**` · **Replaces**: `scheduler/**`, `log/**`, `core/maintenance.go`

### #13 — Credential and claim model (`nd-auth`)
- **Build**: implement #S4's settled choice (model B; the `p`/`t+s`/`enc:` arms of the Go credential switch are NOT ported at all — #25 is CANCELLED). `core/auth/auth.go`, `core/auth/claims.go:11,31-70`,
  `server/auth.go`, `server/initial_setup.go`, the `apiKey`-JWT-internal arms of the credential switch in
  `server/subsonic/middlewares.go` (the `p` and `t+s` arms are NOT ported — #25 CANCELLED), `server/subsonic/auth_limiter.go:15`
  (fail-closed limiter), `core/publicurl/publicurl.go`. Modern-Rust auth: vetted crates only (`argon2`, `jsonwebtoken`/`jose`,
  `rand`/`getrandom`) — dual signing secrets (`core/auth/auth.go:23-29,50,172`), epoch revocation
  (`persistence/user_repository.go:129-144`), audience discipline `native` only (Part C #10; `jellyfin` audience removed with #30,
  `subsonic` never minted), the 3-way credential switch (jwt / apiKey / internal),
  `X-ND-Authorization` (`consts/consts.go:31`), `ExtAuth.TrustedSources` CIDR + unix-`@`
  (`server/auth.go:212,418,427`), body caps (`server/auth.go:100` 8KiB, `middlewares.go:34` 10MB).
- **Test**: claim/verdict table across the 3 credential forms (aud-less tokens REJECTED once the
  discipline settles); cross-audience rejection cases; epoch
  invalidation; rate-limit window tests; initial-setup flow driven from `conf/testdata`.
- **Risk**: HIGH (security) — blocks every wire surface.
- **dependsOn**: #05, #03, #01, #S4 · **Owns**: `nd-auth/**` · **Replaces**: `core/auth/**`, `server/auth.go`,
  `server/initial_setup.go`, auth slice of `server/subsonic/middlewares.go`, `server/subsonic/auth_limiter.go`,
  `core/publicurl/**`

### #14 — Transcode decision tables (`nd-stream/src/decision`)
- **Build**: `core/stream/{types,decider,codec,limitations,aliases,legacy_client,token}.go` —
  `TranscodeDecider{MakeDecision, CreateTranscodeParams, ResolveRequestFromToken}`; defaults at
  `consts/consts.go:176 DefaultTranscodings`; DB history `20200706231659_*`, `20260309203355_*`,
  `20260618120509_*`. Pure tables first; the token half needs #13 — that is why #14 waits for it.
- **Test**: `core/stream/decider_test.go` (~1300 LOC) is the spec, port it as the decision table;
  `codec_test.go`, `aliases_test.go`, `token_test.go` as branch tables.
- **Risk**: HIGH (protocol tables).
- **dependsOn**: #07, #13, #06, #05 · **Owns**: `nd-stream/src/decision/**` · **Replaces**: `core/stream/{types,decider,codec,limitations,aliases,legacy_client,token}.go`,
  `model/transcoding.go`

### #15 — Stream serving, Range, archiver (`nd-stream`)
- **Build**: `core/stream/media_streamer.go` (`NewStream/Serve`), `core/stream/limiter.go`, `core/archiver.go`,
  `server/public/handle_streams.go`. Cancellation end-to-end (client hangup kills #07's child) and the
  transcoding-file cache singleton's eviction contract.
- **Test**: Range matrix (partial, beyond-EOF, zero-length, malformed header); cancellation soak with the
  ffmpeg mock; zip-layout goldens for the archiver.
- **Risk**: HIGH.
- **dependsOn**: #14, #07 · **Owns**: `nd-stream/**` minus `src/decision/**` · **Replaces**: `core/stream/media_streamer.go`,
  `core/stream/limiter.go`, `core/archiver.go`, `server/public/handle_streams.go`

### #16 — Tag/metadata mapping, `Info` → entities (`nd-mapping`)
- **Build**: `model/metadata/*` — `map_mediafile`, `map_participants`, `persistent_ids`, `legacy_ids`;
  `model/tag_mappings.go`, `model/file_types.go`, format branches via `adapters/gotaglib`. This is the ID
  canonicalization step: wrong here, DBs diverge silently.
- **Test**: golden `Info → MediaFile/ArtistRole` round-trips over the fixture corpus; PID rules and legacy-id
  fallback assertions; multi-valued tag split table.
- **Risk**: HIGH.
- **dependsOn**: #02, #08, #03, #06 · **Owns**: `nd-mapping/**` · **Replaces**: `model/metadata/*`, `model/tag_mappings.go`, `model/file_types.go`

### #17 — Scanner (`nd-scanner`)
- **Build**: `scanner/` 12 src files — `controller.go`, `scanner.go`, `walk_dir_tree.go`, `ignore_checker.go`,
  `watcher.go`, `phase_1_folders.go`–`phase_4_playlists.go`, `image_changes.go`, `folder_entry.go`, `external.go`;
  contract `ScanAll/ScanFolders/Status`, `New/CallScan/IsScanning/LockForMaintenance`, `ProgressInfo` events
  (consumed by #11). Read `scanner/README.md` first. Three hard surfaces: symlink semantics
  (`ignore_checker.go` + `walk_dir_tree.go`), subprocess IPC (external calls), fsnotify watching — plus phase
  ordering and rescan idempotence. It feeds the `artwork_queue` drain in #18 and the FTS5 triggers in #04.
- **Test**: port `scanner/scanner_test.go` (50K), `scanner_multilibrary_test.go` (39K), `walk_dir_tree_test.go`
  (27K), `watcher_test.go` (17K), `scanner_selective_test.go` (12K); symlink-chain fixtures from `tests/fixtures/`;
  assert an idempotent second pass mutates no ids.
- **Risk**: HIGH (XL, ~3.4k src / 7k test LOC).
- **dependsOn**: #05, #06, #16, #08, #11 · **Owns**: `nd-scanner/**` · **Replaces**: `scanner/**`, `model/scanner.go`

### #18 — Artwork pipeline (`nd-artwork`)
- **Build**: `core/artwork/**` — `artwork.go`, `worker.go`, `image_cache.go`, `image_store.go`, `sources.go`,
  `processor.go`, `resize.go`, `resolve.go`, `gate.go`, `housekeeping.go`, `prune.go`, `uploader.go`, `disc.go`,
  `animation.go`, `blurhash/`, `thumbhash/`, `dominant/`, `wire_providers.go`, `folders_album.go`, `folders_artist.go`,
  `agent_images.go`, `library_fs.go`, `trace.go`; plus `model/artwork.go` and the `artwork_queue` drain contract
  (`db/migrations/20260722023032_add_artwork_tables.sql:1-46`, covering index
  `ix_artwork_queue_drain(item_kind, priority DESC, enqueued_at, retry_at)`). `thumbhash`/`blurhash` must be byte-compatible
  with `ui/src/utils/thumbhash.js` — `core/artwork/thumbhash/testdata/{golden.json,generated.json,thumbhash.js,gen_*.mjs}`
  is the oracle.
- **Test**: thumbhash/blurhash goldens first (deterministic, cheap); queue-drain ordering against the index columns;
  resolution/gate tests from `resolve_test.go`/`sources_internal_test.go`/`gate_test.go`; drain throughput modeled on
  `worker_soak_test.go`/`worker_timing_test.go`.
- **Risk**: HIGH.
- **dependsOn**: #05, #07, #08, #17 · **Owns**: `nd-artwork/**` · **Replaces**: `core/artwork/**`, `model/artwork.go`

### #19 — Public share pages (`nd-public`)
- **Build**: `server/public/{public,handle_shares,handle_images,handle_downloads}.go`, `core/share.go`,
  `core/publicurl`, `model/share.go`, `persistence/share_repository.go`. Unauthenticated surface: token handling,
  `Content-Disposition` quoting (`utils/str/content_disposition.go` contract), share-image/stream handlers, the
  `ShareInfo` template payload for `ui/index.html`.
- **Test**: token matrix (expired / revoked / malformed / replay); disposition quoting cases; string sanitization against
  `utils/str/sanitize_strings.go` and `utils/str/normalize_fts.go` behavior.
- **Risk**: HIGH (public unauth surface).
- **dependsOn**: #05, #13, #15, #18 · **Owns**: `nd-public/**` · **Replaces**: `server/public/{public,handle_shares,handle_images,handle_downloads}.go`,
  `core/share.go`, `core/publicurl/**`, `model/share.go`

### #20 — Scrobbler (`nd-scrobbler`)
- **Build**: `core/scrobbler/{interfaces,play_tracker,buffered_scrobbler,nowplaying_worker,playbackreport_worker}.go`,
  `model/scrobble.go`, `model/scrobble_buffer.go`, `persistence/{scrobble,scrobble_buffer}_repository.go`. Preserve
  `ErrRetryLater` *identity* (callers branch on the error value) and the buffered flush window
  (`DevEnableBufferedScrobble` is a deprecated key — #S3).
- **Test**: port `tests/mock_scrobble_buffer_repo.go` / `mock_scrobble_repo.go` semantics; now-playing expiry clock table;
  retry-later round-trip.
- **Risk**: MED.
- **dependsOn**: #05 · **Owns**: `nd-scrobbler/**` · **Replaces**: `core/scrobbler/**`, `model/scrobble.go`, `model/scrobble_buffer.go`

### #21 — External metadata agents + matcher (`nd-agents`)
- **Build**: `core/agents/*` capability structs, `core/external/{provider,provider_similarsongs}.go`, `core/matcher/matcher.go`,
  `adapters/{lastfm,listenbrainz,deezer}/*`, `core/wire_providers.go` DI selection. The matcher is flagged red in the graph —
  budget real work, not a straight port.
- **Test**: canned HTTP from `tests/fixtures/**` provider responses (lastfm/deezer/listenbrainz JSON + HTML); matcher fuzz
  table against `Matcher.FuzzyThreshold` (renamed key, #S3); provider-selection tests for the `Agents` config key.
- **Risk**: MED–HIGH.
- **dependsOn**: #05, #01, #13 · **Owns**: `nd-agents/**` · **Replaces**: `core/agents/*`, `core/external/**`, `core/matcher/matcher.go`, `adapters/{lastfm,listenbrainz,deezer}/*`, `core/wire_providers.go`

### #22 — Lyrics fetch + merge (`nd-lyrics`)
- **Build**: `core/lyrics/{lyrics,sources}.go` — source resolution and merge precedence against `LyricsPriority`; parsers come
  from #09. Handler lives in #27 (no #25 exists).
- **Test**: merge-precedence table; canned provider responses; BOM/time-axis cases from `tests/fixtures/`.
- **Risk**: MED.
- **dependsOn**: #06, #09, #05, #21 · **Owns**: `nd-lyrics/**` minus `src/parsers/**` · **Replaces**: `core/lyrics/**`

### #23 — Playlists, store + import (`nd-playlists`)
- **Build**: `core/playlists/{playlists,rest_adapter}.go` + repository layer (`persistence/{playlist,playlist_track}_repository.go`),
  `model/playlist.go`; import semantics and the pending flag from #10.
- **Test**: import round-trip through the repo layer; smart-playlist refresh (`SmartPlaylistRefreshDelay` key); DTO round-trip.
- **Risk**: MED.
- **dependsOn**: #05, #06, #10 · **Owns**: `nd-playlists/**` minus `src/parsers/**` · **Replaces**: `core/playlists/{playlists,rest_adapter}.go`, `model/playlist.go`

### #24 — HTTP shell and middleware pipeline (`nd-http`)
- **Build**: `server/server.go`, `server/middlewares.go`, `server/serve_index.go`, `server/throttle_backlog.go`, `server/filter/filters.go`,
  `server/imghttp/headers.go`, `consts/consts.go:57-63`. BasePath handling, HTML template injection, image ETag/304, backlog
  throttling. Mount order and header-merge order are byte-visible.
- **Test**: middleware-order table; BasePath prefix cases; ETag/304 conditional matrix; port `server/throttle_backlog_test.go` (7.5K) as the
  queueing spec.
- **Risk**: MED.
- **dependsOn**: #13, #03 · **Owns**: `nd-http/**` · **Replaces**: `server/{server,middlewares,serve_index,throttle_backlog}.go`, `server/filter/filters.go`, `server/imghttp/headers.go`

### #25 — Subsonic/OpenSubsonic wire layer — CANCELLED (clean API break, not ported) (`nd-subsonic-compat`)

- **Cancelled**: no compat shim, no wire translation; `server/subsonic/**` and the Subsonic-era auth surface are not ported. Rationale (settled, Revision item 2): a faithful `t+s` shim requires the reversible password store back (`server/subsonic/middlewares.go:281`), re-importing the security debt model B removed; `apiKey`-over-`/rest` works only for clients that can send `apiKey`, which the major Subsonic clients cannot; and the shim would permanently pull canonical design toward Subsonic's lowest-common-denominator wire shapes. Third-party Subsonic clients (play:Sub, Symfonium, Feishin, DSub…) will not work against this server — a deliberate product decision, to be stated in the fork README.
- **What survives**: the 126 goldens under `server/subsonic/responses/.snapshots/` stay in-tree as **reference data** (a recorded behavioral spec of field shapes and streaming/Range/ETag semantics) consulted while authoring #27 — they gate nothing; the fixture *discipline* transfers to the canonical API (#27 captures its own recorded goldens from the Go binary). Subsonic *semantics* worth borrowing are recorded in #27's Build: typed-ID prefixes `al-`/`mf-`/`pl-` (`model/artwork_id.go`), artwork-ID composition `dc-<album>:<disc>`, the transcode-decision split (`core/stream/decider.go`). A future community Subsonic adapter, if ever wanted, would consume #27's published OpenAPI spec — a separate artifact, not a server lane.
- **Risk**: — (not ported). **dependsOn**: — · **Owns**: — · **Replaces**: — (the Go paths retire unported)

### #26 — Players / jukebox (`nd-players`)
- **Build**: `core/players.go`, `core/playback/{playbackserver,device,queue}.go`, `core/playback/mpv/{mpv,track,sockets,sockets_win}.go`, `model/player.go`,
  `server/subsonic/jukebox.go`. Keep the track-IPC message shape and `GetDeviceForUser` resolution; everything else is OS-conditional.
- **Test**: socket-level fake-mpv harness; queue mutation table; `Players.Get/Register/Touch` expiry tests.
- **Risk**: MED.
- **dependsOn**: #05, #15, #13 · **Owns**: `nd-players/**` · **Replaces**: `core/players.go`, `core/playback/**`, `model/player.go`, `server/subsonic/jukebox.go`

### #27 — Canonical API surface (`nd-api`) (TRUNK)

- **Build**: this is the trunk — the surface the owners' Flutter client and the MCP client artifact speak. `server/nativeapi/{native_api,plugin}.go`, `server/apiv1/api.go`, `api/bundled/openapi.{json,yaml}`, `api/embed.go`; mounted at `consts/consts.go:58` `/api`. The chi handlers are the source of truth; the OpenAPI document is **regenerated in Rust from the registered routes**, not shipped verbatim — today `api/bundled/openapi.json` documents only `/server`, `/openapi.json`, `/openapi.yaml`, a known gap, so "served byte-for-byte" is not the contract. `/api/plugin*` routes (`server/nativeapi/plugin.go:16-30`, DI binds `cmd/wire_gen.go:271`) become 404-or-empty per the Decisions section (Part C, item 14).
- **Test**: response-shape goldens captured from the Go binary **before** this issue starts (as today) + OpenAPI self-consistency against the registered route set; error-envelope table. The 126 Subsonic goldens are reference data only — they gate nothing.
- **Risk**: HIGH (external-client contract).
- **dependsOn**: #13, #05 · **Owns**: `nd-api/**` · **Replaces**: `server/nativeapi/**`, `server/apiv1/api.go`, `api/**`

### #28 — Share-page + root HTML contract (`nd-ui-contract`)

- **Build**: server side only — `ui/index.html` placeholders `{{ .AppConfig }}` (`:35`), `{{ .ShareInfo }}` (`:38`), rendered at `server/serve_index.go:122-136` and `:183`. The bundled react-admin JS UI (~47k LOC under `ui/src`) is **DROPPED** by default: a Flutter client replaces it, so nothing here may depend on it.
- **Test**: template-variable presence test for `AppConfig`/`ShareInfo`; base-path/URL assembly for the share URLs the payloads carry. (No JS-client test remains — the `t+s` token flow it exercised has no client any more.)
- **Open decision**: once the JS UI is dropped, what is `/` (root mount `server/server.go:161-167`, `:61`) — redirect, 404, or a landing page? Unresolved; see Part C, item 13. Share pages reuse the same template (`serve_index.go:183 ShareInfo`), so the choice must not break them.
- **Risk**: HIGH (compat).
- **dependsOn**: #27, #24, #19, #11 · **Owns**: `nd-ui-contract/**` · **Replaces**: the server half of `ui/**` (`ui/embed.go`, `ui/index.html` template data)

### #29 — Plugin host — CANCELLED (not ported)

- **Cancelled**: the Extism/wazero host ABI + sandbox (`plugins/{manager,manager_loader,manager_call}.go`, `capabilities*`, `host*`, `sandbox_fs.go`, `host_netguard.go`, `manifest*`, `package.go`, `plugins/cmd/ndpgen/*` codegen, `go.mod:20,58` — 14 host services, ~250 files) was the largest single lane and serves nothing the canonical API (#27) or the clients need. The capability model survives only as data: `plugins/capabilities/*.yaml` is a trait spec for a future Rust-native provider layer. The `PluginLoader`/`PluginManager` DI binds in `cmd/wire_gen.go:271` are simply absent. The GPL-3.0 Rust PDK crates `plugins/pdk/rust/nd-pdk*` and the `plugins/cmd/ndpgen` codegen are not ported.
- **Owns**: — (no crate) · **dependsOn**: — · **Replaces**: — (server-side plugin routes are handled by #27 as 404)

### #30 — Jellyfin API — CANCELLED (not ported) (`nd-jellyfin`)
- **Cancelled** (user decision, Part C #15 resolved): the Jellyfin-compatible surface is not ported. Rationale: it serves only third-party Jellyfin clients — the same ecosystem argument as the cancelled Subsonic shim — and its in/out flow was always DEFERRABLE, never on the spine. The `jellyfin` JWT audience is removed from #13/#S4 (mint and accept `native` only). **Quick-Connect survives**: `core/quickconnect/**` is not Jellyfin-specific and is re-owned by #27 (`nd-api`), which implements `/api/quickconnect` (`server/nativeapi/quickconnect.go:24-33` lookup/authorize device-pairing flow) with its own recorded fixtures.
- **Owns**: nothing · **Replaces**: — (`server/jellyfin/**` not ported; `core/quickconnect/**` → #27)

### #31 — Sonic similarity — CANCELLED (not ported) (`nd-sonic`)

- **Cancelled with #25**: its only consumers were `server/subsonic/api.go:129` (`getSonicSimilarTracks`) and the dropped JS UI (`ui/src/subsonic/index.js:126`) — both gone post-clean-break. Archive with a revival path if a canonical similar-tracks endpoint is ever wanted.
- **Risk**: — (not ported). **dependsOn**: — · **Owns**: — · **Replaces**: — (`core/sonic/sonic.go`, `server/subsonic/sonic_similarity.go` retire unported)

### #32 — Diagnostics / metrics (DEFERRABLE) (`nd-metrics`)
- **Build**: `core/metrics/{insights,prometheus,insights_linux,data}.go`, `core/inspect.go` — Prometheus text + library insights. Tied to DB keys `LastDBAnalyzeAt`,
  `LastDBAnalyzeAttemptAt`, `DBAnalyzePending`, `DBAnalyzeFailureCount` (`consts/consts.go:23-26`) and the insights key (#S3).
- **Test**: Prometheus text golden per metric group; port `core/inspect_test.go` for the insights shape.
- **Risk**: MED, DEFERRABLE.
- **dependsOn**: #05, #03 · **Owns**: `nd-metrics/**` · **Replaces**: `core/metrics/**`, `core/inspect.go`

### #33 — CLI and service entrypoints (`nd-cli`)
- **Build**: `cmd/*.go` (root/scan/inspect/doctor/backup/pls/missing/artwork/plugin/svc/user/search) plus the hand-written replacement for `cmd/wire_gen.go` +
  `cmd/wire_injectors.go` DI codegen — there is no Rust analogue, so write explicit construction in `nd-cli/src/main.rs`. Subcommand surface stays flag-compatible;
  DI scaffolding may stay stubbed until the lanes stabilize (flagged in the graph as scaffold-not-final).
- **Test**: `--help` and exit-code tables per subcommand; arg-parse round-trip against Go; registry snapshot diff against `cmd/`.
- **Risk**: HIGH.
- **dependsOn**: #04, #05, #03, #17, #12, #32 · **Owns**: `nd-cli/**` · **Replaces**: `cmd/*.go`

### #34 — Backgrounds / wallpaper — DROPPED (not ported) (`nd-backgrounds`)

- **Dropped** (Part C #17 resolved): its sole consumer was the JS UI's login background fetch (`cmd/root.go:153` mount of `conf.Server.UILoginBackgroundURL`, `consts/consts.go:71`) — dropped with #28. Re-open only if the Flutter client asks for it.
- **Risk**: — (not ported). **dependsOn**: — · **Owns**: — · **Replaces**: — (`server/backgrounds/handler.go` retires unported)

## Dependency spine (for the tracker's `blockedBy` fields)

- **Spine gate**: #S1 → {#S2,#S3,#S4} → #04 → #05. ~20 issues are `blockedBy` #05; nothing runtime-bearing compiles before it lands.
- **Leaf lane** (parallel with the spine): #01, #02, #03, #06, #11, #12, #09, #10 — no DB writes.
- **Media adapters** (parallel with the spine): #07, #08, #09 — consume #01 only.
- **Auth**: #13 gates #14, #15, #19, #24, #26, #27, #28 (#25/#29/#30 cancelled and own nothing).
- **Stream chain** (strict): #07 → #14 → #15 → (#19, #26). Streaming reaches the wire only through #27's canonical endpoints.
- **Scan pipeline**: #08 → #16 → #17 → #18. #17 feeds the `artwork_queue` drain in #18 and the FTS5 triggers in #04.
- **Wire lane** (last): #24 → #27 → #28 — canonical surface only.
- **Satellites** (last, concurrent): #20, #21, #22, #23, #32, #33.
- **Deferred tail**: none (#30 CANCELLED).

## Not ported

CANCELLED / DROPPED issues (kept for number stability; each retires its Go paths unported): #25 Subsonic wire layer, #29 WASM plugin host, #31 sonic similarity, #34 backgrounds. State the Subsonic ecosystem loss (third-party Subsonic clients will not work) in the fork README.
No issue needed: `git/pre-commit`, `git/pre-push`, `.github/`, `release/`, `contrib/`, `.devcontainer/`, `scripts/setup-worktree.sh`, `bench/` — repo/CI tooling only.
Re-check `release/linux/navidrome.toml` and `Dockerfile:208` whenever config keys change: #03 owns that coupling.
MCP is a separate client artifact, not part of this Rust workspace: it speaks the canonical API (#27) with one API key, is not GPL-encumbered, and needs no crate.

---

## Part C — Contract decisions (settled at #S1; all items decided)

These are the open contract questions the revision did not settle. Each names the issues it blocks; none of them may be resolved silently inside a later issue.

1. **Canonical API KEEP/DROP per route — DECIDED**: keep CRUD + `search` + share + admin/account + `quickconnect` (re-owned per item 15); drop `/inspect`, `/config/*`, `/plugin*` (item 14), `/transcoding` config editor (#32 dropped); `/share` kept only as share-page HTML (#28). Blocks #27.
2. **Native equivalents for Subsonic-only features — DECIDED (PRIORITY #1: add them to the canonical surface)** — with #25 CANCELLED, playback has no path at all. Decisions (endpoint proposals, not validated designs): `GET /api/stream`, `GET /api/download`, `GET /api/artwork`, `GET /api/search` modelled on the Go `/rest` implementations (`server/subsonic/stream.go`, `download.go`, `artwork.go`, `search3.go`) and the native CRUD shapes in `server/nativeapi/native_api.go:59-104`; bookmarks/radio/play-queues/lyrics keep their semantics (they are Subsonic-only *features* in the code, not a wire contract). Their fixtures are **authored by transcribing the Go `/rest` behavior** (Range/ETag headers, `server/subsonic/stream.go` + `core/stream/token.go:106,111` transcode decision) BEFORE #27/#15 open. Blocks #27, #15, #19.
3. **Streaming URL contract — DECIDED (PRIORITY #2: canonical `GET /api/stream?...` forms)** — the Flutter client (built after the server translation) speaks the canonical forms from item 2, not `/rest/stream?u&t&s` (`ui/src/subsonic/index.js:17-22`) or `getTranscodeStream?...transcodeParams=<jwt>&offset=` (`ui/src/transcode/decisionService.js:73-83`). Share URLs keep the share-page HTML contract (#28, `ui/src/utils/urls.js:28-40`).
4. **API-key issuance on the canonical surface — DECIDED (PRIORITY #3: first-class `POST /api/player/:id/key`)** (re-issue rotates the hash; owner is the player). Implementation moves from the Subsonic middleware (`server/subsonic/middlewares.go:191-222`) into #27's route, hashing per `persistence/player_repository.go:120,127,154-158,200-207` (`^nds_[0-9A-Za-z]{22}$`). Blocks #27, #13, #26.
5. **Per-key scopes/labels — DECIDED: keep one-hash-per-`player`, no scopes** (owner's minimal default; unique index `db/migrations/20260924010054_add_player_api_key_hash.sql:3,4` stands). A player row ≈ a device with one key; revocation = null the hash. If per-key scopes ever appear, they are a deliberate #S2 schema delta, reopened then.
6. **Existing-install password migration — MOOT (fresh service, no upgrade path)**: no legacy installs exist; passwords are argon2id from day one; the AES-GCM code and `PasswordEncryptionKey` config key are never ported. Blocks nothing.
7. **`subsonicSalt`/`subsonicToken` in the NATIVE login payload — DECIDED: drop entirely** — `server/auth.go:84-97 buildAuthPayload` (called `:68`, `:415`) is removed from the login payload; its only consumers are the dropped legacy JS player (`ui/src/utils/tokens.js:14-17`, `ui/src/subsonic/index.js:5-9`, `ui/src/subsonic/albumList.js:105-109`, `ui/src/multipleInheritance.js:26`). Blocks #13, #27.
8. **Public-token surface — DECIDED: keep** (share pages are the only HTML surface). SECURITY-CRITICAL: Go verifies with HMAC (`core/auth/auth.go:125-131`); the Rust port must verify signatures, never decode — `server/public/handle_images.go:76-93` calls `PublicTokenAuth.Decode` (signature-bypassing) while granting `IsAdmin:true` (`:42-44`), a real bug to fix not copy. Blocks #19, #28.
9. **Auth limiter for the trunk — DECIDED: apply on login + key-issuance paths, fail-closed** (unmatched route = rate-limited); do NOT copy the `newAuthLimiter` allows-all at `limit<=0` (`server/subsonic/auth_limiter.go:39-48,54-56`) — `limit<=0` means misconfigured, not unguarded. Blocks #27, #13.
10. **JWT audience discipline — DECIDED: mint and accept only `native`** — `jellyfin`/`subsonic` audiences are never minted or accepted; aud-less acceptance is closed from day one (no legacy tokens to preserve). Use a vetted Rust JWT crate (`jsonwebtoken`) with explicit aud/exp checks plus token_epoch. Blocks #13.
11. **Per-key revocation — DECIDED: `token_epoch` bump revokes all of a user's sessions; nulling `player.api_key_hash` revokes only that key** (`persistence/user_repository.go:129-146`, `core/auth/auth.go:147-149`, `persistence/player_repository.go:37-39,209+`), with a user-visible `POST /api/sessions/revoke`. Blocks #13.
12. **Config contract — DECIDED: clean TOML, no deprecation table** — fresh service, so no user TOML exists to pin: `Subsonic.*`, `Plugins.*`, `Jukebox.*` (`conf/configuration.go:188-196`, `:115`, `:119`) and the compat toggle are simply absent from the fresh TOML; unknown keys in a config file are a startup **error**, not a warning (`conf/configuration.go:676-690`). Blocks #S3, #03.
13. **Root path once the JS UI is dropped — DECIDED: `GET /` → 404** — `server/server.go:161-167`, `server/serve_index.go:122-136`, `ui/index.html:35,38` are not ported; the share-page HTML (#28, share template `serve_index.go:183 ShareInfo`) is the only HTML surface. Blocks #28, #24, #19.
14. **`/api/plugin*` with the host cancelled — DECIDED: route removed, 404** — `server/nativeapi/plugin.go:16-30`, DI `cmd/wire_gen.go:271`, `persistence/plugin_cleanup.go` are not ported. Blocks #27.
15. **Jellyfin drop — DECIDED: drop** (user decision). `server/jellyfin/**` is not ported (#30 CANCELLED); the `jellyfin` JWT audience is removed (mint/accept `native` only, item 10). Quick-Connect is NOT Jellyfin-specific: `core/quickconnect/**` is re-owned by #27 (`nd-api`) and kept, backing `/api/quickconnect` (`server/nativeapi/quickconnect.go:24-33`).
16. **DB migration strategy — DECIDED: regenerate** (no-upgrade-path decision). One fresh Rust migration is the basis, edited in place as the schema evolves; the 138-file Go migration history is never replayed. Blocks nothing.
17. **Zero-consumer crates — DECIDED**: `nd-backgrounds` (#34) and `nd-sonic` (#31) are out of the workspace; re-open individually if a Flutter need appears.
