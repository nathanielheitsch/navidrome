# Plan of record for the next body of work

## Current state (2026-11-11, at `12e270f7`) — supersedes the boundary below

Measured gate (parent, integrated): fmt clean; workspace build clean; clippy `-D warnings` clean;
`cargo test --locked --workspace --all-targets --all-features --no-fail-fast` = **1514 passed / 0 failed** across
57 test binaries. Progression: `d19526e3` (#15 transcode cache) 1442 → `a7afda2a` (timestamp convention) 1453 →
`8c89aeb9` + `12e270f7` (INSERT alignment, playlist `INSERT … SET`, `0000-00-00` sentinel) 1459 →
`2a99a681` + `3f8b5a8d` (participant JSON + FTS feeders) 1490 → `cb11b645` (JSON codecs + Role bridge) 1514 → S-D2a (**marshal core + PostMap hooks**) **1536**.

Three silent-corruption defects, found while reviewing the shared-layer contract, were all **invisible to the
existing renderer tests** — those only compared emitted SQL strings:
- **INSERT columns and values misaligned** in `album_repo`, `folder_repo`, `mediafile_repo`: the column list ended
  with `id` (`columns.chain(["id"])`) while the VALUES tuple began with it. Any multi-column insert stored every
  value in the wrong column — verified against SQLite *before* the fix (`INSERT INTO album (name, album_artist, id)`
  with `('alb-1','Song Title','Jane Doe')` stored `id = 'Jane Doe'`). Go cannot have this bug: squirrel `SetMap` sorts
  column names and emits values positionally against that order (`squirrel@v1.5.4/insert.go:271-283`), and Go `put`
  appends `id` to the values map (`sql_base_repository.go:637-641`). The invariant is **columns and args emitted
  together**, not "id binds first" — the port's doc comments asserted the invented rule that encoded the bug.
- **`playlist_repo`'s insert leg rendered `INSERT INTO playlist SET …`**, MySQL syntax SQLite rejects outright
  (`near "SET": syntax error`), so that leg could never execute. Third instance of the class the timestamp lane found
  in `radio_repo`/`artist_repo`.
- **`get::<Option<DateTime>>` errored on the DDL `DEFAULT '0000-00-00 00:00:00'`** on `library.last_scan_at`,
  `library.last_scan_started_at` and `album.imported_at`; chrono parses neither that sentinel (month/day are 0) nor
  SQLite's naive `CURRENT_TIMESTAMP`. Go discards its parse error → zero `time.Time`, and callers test
  `!LastScanAt.IsZero()` (`model/library.go:64`, `scanner/controller.go:124`), so `None` is the faithful spelling.
  `time::optional_sentinel()` maps it to `None` while a genuinely malformed date remains an error. No caller changed
  (nothing reads those columns yet) — a latent hazard removed for the scanner lane, not a behaviour change.

`nd-db/tests/runtime_insert_alignment.rs` executes the statements against a fresh schema and reads the stored row
back, with a non-vacuity arm reproducing the pre-fix argument order. Proven non-vacuous: restoring id-first makes the
album test fail. **Lesson for the shared layer: renderer tests cannot catch this class — every write path needs an
executed write→read-back test.**

**Verified against the tree, not from memory** (three claims previously written here were fabricated and are
removed): there is **no** `mediafile_repo::upsert_sql`, **no** `ON DUPLICATE KEY UPDATE` anywhere in `nd-db` or in
Go `persistence/`, **no** `todo!()`/`unimplemented!()` in `nd-db`, and **no** `media_file_pkey` index. Go's
`mediaFileRepository.Put` (`persistence/mediafile_repository.go:184-197`) uses `putByMatch` — a SELECT-then-`put`,
not an upsert — and the Rust `put_match_sql` + `put_sql` pair already models that correctly. Do not "fix" an upsert
that does not exist.

The figures below (1442, 1230, 1183, and the 1453 in its progression line) are dated historical boundaries.

## Previous state (2026-11-11, at `a7afda2a`) — record

Measured gate at `a7afda2a` (2026-11-11, parent-measured, integrated): fmt clean, workspace build clean, workspace
clippy `-D warnings` clean, and `cargo test --locked --workspace --all-targets --all-features --no-fail-fast` =
**1442 passed / 0 FAILED** (1 ignored) across 52 test binaries, and zero surviving `ffmpeg` processes after the suite.
Progression: `94ffa0ca` 1281 → `18f6bc5e` (#15 limiter wired) 1291 → `b419472b` (#16 mapping + Go oracle) 1301 →
`b5c317f8` (#15 archiver + zip oracle) 1371 → `38b0b812` (#15 HTTP handler + Go duration oracle) 1406 →
`d19526e3` (**#15 transcode cache — #15 is now COMPLETE**) 1442 → `a7afda2a` (**timestamp convention**) **1453**.
The `a462b1f2` figure below — 1183 —
and the `29d9d9d0` figure — 1067 passed / 4 FAILED —
are dated: the latter had `nd-ffmpeg` fixture paths hardcoded to a retired worktree, so the container fixtures executed
**zero** files off that host; `5e56f7e8` resolves them from `CARGO_MANIFEST_DIR` to the repo root. The
"969 passed / 0 failed" figure further down is a dated historical boundary, not green state.

**The Phase 1–4 plan below is accurate as a record of what was *shaped*, but shape ≠ wired.**
The runtime spine has started, but nothing boots yet:

- `nd-db` — execution seam (`execution.rs`, `store::SQLStore` sync + async verbs) and **four** executing
  repositories: `user_repo` (`UserRecord`/`UserRow` hydration, `WHERE`-before-`GROUP BY` fix),
  `player_repo` (`c8c71f8d`), and `share_repo`/`user_props_repo` (raw `conn`, bypassing `execution.rs`).
  **The other 17 repos only render SQL.** The real gap is not hydration alone: there is no shared row mapper
  *and* no shared entity→`(columns, values)` marshaler, and nothing computes the media_file FTS columns. See
  §Then item 2 for the scoping brief's findings. Pagination renders decimal literals with zero bound args, matching
  squirrel (`select.go:376-388,140-150`); `b78c01a2` fixed a crate-wide divergence where the port
  bound `LIMIT ?`/`OFFSET ?` as args. Offset-without-limit keeps the cited `LIMIT -1` deviation
  because squirrel's bare `OFFSET n` is a SQLite parse error. **Player repository landed**
  (`c8c71f8d`: `PlayerRepo`, `PlayerError`, `save`/`set_api_key`/`find_by_api_key`, 19 executed tests),
  so `/player` and API-key issuance have a store — nothing mounts it yet.
- `nd-dbschema` — **landed** `15426bbc`: the fresh DDL now carries all 80 index rows the migrated Go schema
  ends with (4 UNIQUE, incl. `player_api_key_hash`), and `schema_diff.rs` diffs index rows plus per-table
  column sets. Follow-up worth scoping: re-derive the golden DDL once against the corrected shape.
- `nd-tagreader` — **landed** `beaf63d7`: real `lofty` 0.25.5 parsing replaces the sidecar-only reader, so
  #16 → #17 ingest is no longer blocked upstream. Declared gaps to respect in #17: ASF/WMA unsupported,
  AAC-in-MP4 bit depth 0 vs Go's 16, bitrate estimates differ, MPEG-frame-timestamp SYLT skipped.
- `nd-auth` — **wired** `bd8f7a84..cfea1e08` (merged `0fad9985`): mountable request middleware, credential
  verifier, login with transactional first-admin, post-handler refresh, trusted-header provisioning,
  `GuardedLimiter`. Open item: the documented `createAdmin` atomicity deviation needs its own ledger row.
- `nd-db` player repository — **landed** `c8c71f8d`, and auth middleware now builds on it. `nd-stream` —
  Range/conditional matrix, bounded direct adapter, probe adapter, process-backed transcode body, limiter,
  archiver, **HTTP handler**, and the **transcoding-file cache** all landed — **#15 is complete**. `nd-cli` `main()`
  is still empty and `nd-http`/`nd-api`/`nd-scanner` are scaffold-only, so **nothing boots**.

### Active lanes (user-approved 2026-11-10)

1. **DB runtime spine** (`rust/db-runtime`, A7/A8) — **LANDED** `3ce1b154` + `dee593b5`: execution seam,
   `SQLStore`, corrected user SQL, `user_repo` executed with hydration. Player repository (A9) and auth
   wiring (A10) are NOT started.
2. **`nd-ffmpeg`** (`rust/ffmpeg-readiness`, A2/A3/A4/A5/A11) — **LANDED** `5e56f7e8` + `8152ac5a`: portable
   fixtures, `process_group(0)` spawn + `kill_process_group` SIGKILL with ESRCH fallback, `STDERR_CAP`
   4096 with the overflow still consumed, `TEARDOWN_TIMEOUT_MS` bounded drain/reap. Grandchild-leak test
   verified non-vacuous (stubbing the group kill fails it with a leaked pid).
3. **Stream serving** (`rust/stream-serving`, A12) — **LANDED** `aa9b280e` + `a462b1f2`: Range/conditional
   matrix at `net/http` parity, bounded direct-file adapter, probe adapter, transcode body with explicit
   disconnect cancel. Semaphore (`164780ee`+`fc48ff3f`), archiver (`8b9f48cf`…`b5c317f8`), and HTTP handler
   (`db9f1e42`…`38b0b812`) landed after it. Remaining: the transcoding-file cache only.
4. **Ledger correction** (`rust/ledger-correction`, A6) — **LANDED** `0309b5aa`, superseded by this revision.

All four lanes are finished or dead (provider limits). Two reported commits that exist on no branch
(`5d1c207f`, `8842a742`, `6b289012`) and a `procgroup.rs` module that was never written; one left files
uncommitted. Verify every report against `git log` / `git status` / the files themselves before integrating.

### Next body of work, in order

1. ~~Land the four lanes above~~ — done; integrated to `issue/planning-rust-sdhnn` at `a462b1f2`, gate
   re-measured in the parent at 1230 passed / 0 failed. There is no `main` branch: remote HEAD is
   `master` (still `6b2ed391`, tree-identical to `29d9d9d0`), so this work lives on the planning branch
   and is unpushed.
2. ~~Player repository~~ (#05 remainder, A9) — **landed** `c8c71f8d`. Its pagination unit test was
   rewritten against squirrel's literal rendering during the `b78c01a2` rebase (it had pinned `LIMIT ?`).
3. ~~Auth wiring~~ (A10) — **landed** `bd8f7a84..cfea1e08`, merged `0fad9985`; gate re-measured at 1243/0.
   Mounted middleware, credential verifier, login with transactional first-admin, post-handler refresh,
   trusted-header provisioning, `GuardedLimiter`. Two open items: a fresh reviewer (A16) is checking whether
   the Go-parity allow-all limiter primitive is still reachable from a mountable route, and the port deviates
   from Go by making `createAdmin` atomic (`server/auth.go:134-146` is non-atomic) — that deviation is
   documented in code and needs a ledger row of its own.
4. ~~`nd-dbschema` secondary indexes~~ (#04 follow-up, A13) — **landed** `15426bbc`. The fresh DDL now
   carries all 80 index rows the migrated Go schema ends with (4 UNIQUE, incl. `player_api_key_hash`), and
   `schema_diff.rs` diffs index rows (name + uniqueness) plus per-table column sets. It also recovered 8
   columns the golden had dropped (`album.rg_album_gain`/`rg_album_peak`, `artist.uploaded_image`,
   `media_file.codec`/`probe_data`, `playlist.uploaded_image`/`external_image_url`, `radio.uploaded_image`) —
   `media_file_codec` could not even be created without `codec`. Follow-up worth scoping: the golden DDL was
   derived by replaying migrations, so re-derive it once against this corrected shape to confirm nothing else
   silently fell out.
5. ~~Finish #15~~ — **COMPLETE**. Semaphore `164780ee`+`fc48ff3f`, archiver `8b9f48cf`…`b5c317f8`, HTTP handler
   `db9f1e42`…`38b0b812`, transcode cache `8abefec1`/`46b2a1ca`/`d19526e3`. See item 10.
6. ~~A real tag backend~~ (#08, A15) — **landed** `beaf63d7`, merged; gate 1270/0. `lofty = "=0.25.5"`
   (pure Rust, MIT/Apache-2.0, +11 new lock entries) reads MP3/FLAC/M4A/OGG with multi-values, comments,
   language-coded USLT/SYLT, TIPL/TMCL, MP4 atom precedence, pictures, and malformed-file skipping; five
   ffmpeg-synthesized fixtures plus eight Go media fixtures compared to their Go oracles. **Ingest (#16 → #17)
   is no longer blocked upstream.** Declared gaps to respect when wiring #16/#17: ASF/WMA unsupported;
   AAC-in-MP4 bit depth 0 vs Go's 16; bitrate estimates differ (FLAC 103/104, Vorbis 16/19 kbps);
   MPEG-frame-timestamp SYLT skipped. A fresh review is checking whether these surface honestly or silently
   persist wrong values. Note the exact pin: bumping lofty regenerates nothing automatically, and the Go
   goldens must be re-dumped if the go-taglib fork changes.
7. ~~#16 tag/metadata mapping~~ (`nd-mapping`) — **landed** `4463670b` + `5d12b5ad`, merged `b419472b`,
   gate 1301/0. `Info → Metadata → MediaFile` pinned by a Go oracle harness (`tests/testdata/go_oracle/main.go`
   → `tests/data/go_oracle.json`, 51 rows). **Ingest is now #17 only.** Hazard for #17: Go's `parseID3Pairs`
   (`model/metadata/metadata.go:276-290`) iterates a map, so pair-tag and sub-role ordering is unreproducible
   across runs of the same binary (parent-verified: three regenerations differ byte-wise, agree only sorted).
   Rust is `BTreeMap`-ordered; #17 must persist no ordering inherited from either.
8. ~~#15 archiver~~ — **landed** `8b9f48cf`…`b5c317f8` (6 signed commits, agent `5b52e50f…`), gate 1371/0.
   `core/archiver.go` → `nd-stream/src/archiver.rs` (2546L) pinned by a real Go `archive/zip` oracle
   (`tests/testdata/go_zip/main.go`, 8 goldens + manifest); parent-verified byte-reproducible for the deterministic
   goldens and mutation-checked. Declared divergence: cover entries encode the instant in UTC while Go encodes it
   with its zone (`archive/zip/writer.go:325-335`), so the legacy MS-DOS field differs by the zone offset; the
   extended-timestamp extra field is asserted byte-exact.
9. ~~#15 HTTP handler~~ — **landed** `db9f1e42` + `63272830` + `e35a7d2d` + `cc125af4` + `38b0b812`
   (5 signed commits; the delegate lane died twice on provider aborts and the parent finished it), gate
   **1406/0**. `StreamHandler<D>` consumes direct/transcode/cache bodies and reproduces both callers'
   status/message/`Retry-After` arms; `X-Content-Duration` is pinned digit-for-digit against a Go
   `strconv.FormatFloat` oracle (`tests/testdata/go_duration`). Criterion 5 is asserted end-to-end: four
   lifecycle tests each prove the pid is gone from the process table and the concurrency slot returns.
   Three Go-parity gaps were fixed on the way (a non-zero ffmpeg exit propagates through the stdout pipe
   as a read error, `Content-Encoding` suppresses the estimate length, `http.Error` overwrites the staged
   Content-Type).
10. ~~#15 transcoding-file cache~~ — **landed** `8abefec1` (cache core) + `46b2a1ca` (lane tests + haunter walk) +
    `d19526e3` (parent fix), gate **1442/0**. `utils/cache/file_caches.go` port behind the `TranscodeSource` seam:
    sha1 spread layout (`root/xx/xx/<40hex>`), Go's `Cached` semantics, fan-out, background drain, scrub/eviction.
    Findings worth keeping:
    - **Go's `Cached` does not mean "complete."** It means *not the writer-side creator* (`file_caches.go:157-164`);
      a joiner on an in-progress entry gets `Cached=true` with a nil writer. The first Get is `Cached=false`. Pinned
      by the Go test at `file_caches_test.go:286-308`.
    - **The seam was too narrow, so it changed** (deviation approved by the parent, documented in code):
      `TranscodeLease::Cached` gained `signal` + `expected_size` and the pump arm in `handler.rs` now takes the signal
      from the lease. A writer-side lease is `Cached=true` but non-seekable, so the old `Cached => None` would have
      returned a **200 with 0 bytes** and skipped the disconnect race entirely. All 35 handler tests still pass.
    - Dep delta: **one new third-party crate, `sha1` 0.10** (its deps already sit in the lock via `sha2`).
    - `d19526e3` fixed a test-side deadlock: `assert_no_poller_leak` re-acquired `PROCESS_LOCK` while its caller held
      it (tokio `Mutex` is not reentrant), and some baselines were sampled before the lock so a parallel test's poller
      contaminated the comparison. Pass baseline-after-lock, never nested.
10. **The `nd-db` shared marshal + hydration layer** — **now the critical path**; see §Then item 1. #17 cannot start
    without it.
11. **#17 scanner** (`nd-scanner`) — XL; starts after item 10. See §Then item 2 for the minimum executed surface
    (8 repos + 1 shared mapper, not ~14) and the watcher licence answer.
12. `nd-dbschema/src/repair.rs` (`db/repair.go`, 367L FTS5 verify/repair) — still deferred, its own lane.
13. Specify + build the #12 maintenance/logging half.
14. **First runnable slice** (`#27A`): fresh DB → scan a real track → authenticated API lookup → direct
   Range + forced transcode + client disconnect with **zero surviving child processes**. Full #27 needs
   share/playlists/artwork/external/quick-connect services (`server/nativeapi/native_api.go:38-55`);
   deferrable #32 must **not** gate this minimal slice.

## Then, in order (updated 2026-11-11 at `d19526e3`, gate 1442/0)

Two read-only scoping briefs (2026-11-11) replace the old "hydration for the remaining ~14 repos" line. Every claim
below was verified against the tree by the parent, not taken on faith.

### 1. `nd-db` shared marshal + hydration layer (critical path)

**The gap is two-sided and larger than hydration.** Go funnels every read through one reflection-based scanner
(`dbx@v1.12.0/rows.go:65-103`, reached from `sql_base_repository.go:381,429,450,466`) and every write through
`structs.Map` + `PostMapArgs` (`sql_base_repository.go:445,480`). Rust has **no counterpart to either**: exactly three
hand-written mappers exist (`user_repo.rs:857`, `player_repo.rs:1074`, `share_repo.rs:449`), and `share_repo` plus
`user_props_repo` bypass `execution.rs` entirely with raw `conn`.

Executing surface (`grep -ln "use rusqlite::"`): `execution.rs`, `pool.rs`, `store.rs`, `user_repo.rs`, `player_repo.rs`,
`share_repo.rs`, `user_props_repo.rs` — nothing else. The other 17 repos are render-only.

Hazards, ranked by how silently they corrupt data:
1. **media_file / album FTS columns are produced by nothing.** `mediafile_repo.rs:459-469` declares `FtsColumns` for
   the caller to supply; the only producer in the tree is `artist_repo.rs:418`. Go computes them at Put
   (`persistence/mediafile_repository.go:69-79`, `album_repository.go:68-96`). A scanner writing rows today would
   persist empty `search_*`/`full_text` and break search with no error anywhere.
2. `PostScan` aggregation for album/artist/mediafile/folder (`Discs`, `Participants`, `Tags`, `FolderIDs`,
   `LibraryStatsJSON`) has no Rust counterpart; `artist_repo.rs:236` is an explicit stub.
3. **Timestamp convention — DECIDED by the user 2026-11-11: use the Rust standard; no Go-migration
   compatibility is required.** This port is not a path users migrate a Go Navidrome DB into, so matching Go's stored
   representation is explicitly *not* a goal. Choose for Rust ergonomics and correctness.

   **The brief's `nd-db/src/time.rs:43-69` does not exist, and neither does `now_sql()`.** Both were fabricated. There
   is no timestamp module and no central convention: `chrono` is not even a dependency of `nd-db`. The real state,
   measured:
   - `user_repo` / `player_repo` carry timestamps as TEXT and the tests use two different shapes —
     `runtime_user_repo.rs:17` `"2024-01-01T00:00:00Z"` vs `runtime_player_repo.rs:27` `"2024-01-02 03:04:05"`.
   - `share_repo` (`Time` = epoch seconds, `share_repo.rs:32,87-101`), `radio_repo.rs:23-24,124` and
     `artist_repo.rs:566` bind **`Value::Integer` epoch seconds into `datetime` columns**, and the code already admits
     the divergence in its own doc comments (`radio_repo.rs:23-24`, `share_repo.rs:32`).

   **This is a live correctness bug, not a parity nit — parent-verified against SQLite.** In the affinity system an
   INTEGER beats a `datetime` column, so the stored value keeps `typeof()` = `integer`, and INTEGER sorts before TEXT.
   Two consequences, both silent:
   - a `Gt{"media_file.created_at": since}` / `Lt` predicate bound as TEXT (`mediafile_repository.go:533,559` — Go
     binds `time.Time`) **excludes** INTEGER-stored rows entirely: `where created_at > '2020-01-01 00:00:00'` returned
     only the text row and skipped the integer one. That is the recent-files / scanner-missing path.
   - `ORDER BY` is non-chronological in **both** directions: INTEGER sorts before TEXT, so ASC yields INTEGER rows
     first and DESC yields them last — verified (an epoch row holding a 2100 instant came first ASC and last DESC).
   - `share` expiry (`core/share.go:44-47`) is currently self-consistent because both sides are integers, so it works —
     but it will break the moment anything else reads those columns.

   **Chosen convention: rusqlite's own `chrono` feature, with `DateTime<Utc>` everywhere.** That *is* the Rust standard
   here, so no hand-rolled bind/parse helper is needed: `rusqlite-0.32.1/src/types/mod.rs:80-82` gates `mod chrono`,
   whose `ToSql for DateTime<Utc>` (`types/chrono.rs:88-93`) emits `%F %T%.f%:z` →
   `2024-01-01 00:00:00+00:00` and whose `FromSql` (`:117-136`) parses RFC3339 and **falls back to `NaiveDateTime`**
   when there is no offset — so it reads both its own output and SQLite's naive `current_timestamp` default.
   Add `chrono` to the three rusqlite pins (`nd-db`, `nd-dbschema`, `nd-auth` all repeat the same
   `features = ["bundled", "collation", "functions", "backup"]`, per the `nd-dbschema/Cargo.toml:14` template).
   `chrono` 0.4.45 is already in `Cargo.lock`, so this adds no new third-party crate to the lock — only a feature.
   Parent-verified in SQLite that this fixes both bugs: rows stored as `2024-01-01 00:00:00+00:00` (with and without
   fractional seconds) sort correctly and are included by a `Gt` bound, and the naive `2026-11-11 23:19:10` default
   sorts last rather than first. Set `created_at`/`updated_at` explicitly instead of relying on that default — Go sets
   them explicitly too (`persistence/artist_repository.go:241`, `user_repository.go` `values["created_at"] = time.Now()`),
   so this is not a behavioural change. Keep the type opaque at the repository boundary where it already is
   (`user_repo.rs:490-492` deliberately stops at the row), but make the *storage* type uniform.
   Pin with an executed write→read→compare round-trip against a real DB, not a string assertion.
4. `playlist_track_repo` has no executed read path at all, so Go's `PostScan` overwrite `MediaFile.ID = t.MediaFileID`
   (`playlist_track_repository.go:27-33`) has no Rust counterpart yet — the hydration layer must supply it.
5. **Two more brief claims are false — do not chase them.** `user_props_repo.rs:108` does *not* map `value` twice: its
   `get` selects the single `value` column exactly as Go does (`user_props_repository.go:30-40`, verified). And
   `artist_repo.rs:236` is not a `PostScan` stub; it is the annotation-column join inside `new_select_sql`.
6. **The write feeders exist as types but nothing produces them.** *(CLOSED by `2a99a681` + `3f8b5a8d`:
   `sql_participations.rs` now marshals/unmarshals the participant column, `sql_feeders.rs` produces all three
   FTS columns for album and mediafile, both pinned by Go oracles that drive the real Go types. What remains for
   this hazard is only the `discs`/`folder_ids` JSON marshalers and wiring the feeders into repo call sites — S-D.)* `mediafile_repo::FtsColumns` (`:459`) documents
   that the caller supplies `full_text`/`search_participants`/`search_normalized`, and `put_sql` takes
   `participants_json` as an already-marshalled string — but **no production code computes either**. Verified: the
   only `full_text`/`search_normalized` producer in the crate is `artist_repo.rs:576-581` (2-arg and 1-arg helpers),
   which cannot express album's inputs (disc values, album-version/catalog-number tags) or mediafile's (participants,
   sort fields, `FullTitle` subtitle). `sql_tags::marshal_tags` (`:129`) is faithful and unused outside its own tests,
   and there is **no participant marshaller at all** (`sql_participations.rs` has only filter/subquery helpers, vs Go
   `sql_participations.go:50-65`). No `discs`/`folder_ids` JSON marshaler exists either. A scanner writing rows today
   would persist empty search columns and no participant/tag rows with **no error anywhere**. §1b "Search feeders are
   artist-only" states this precisely; it is the first thing the shared layer must close.
7. **`put` alignment is fixed; the executed-read half is not.** `album_repo`/`folder_repo`/`mediafile_repo` appended
   `id` to the column list while prepending it to the args, so every multi-column insert stored each value in the
   wrong column; `playlist_repo` emitted `INSERT … SET`, which SQLite cannot parse. Both are corrected in `8c89aeb9`
   and pinned by `tests/runtime_insert_alignment.rs`. Nothing in this hazard list is a hypothetical — it is the
   measured state.

**Shape to build:** one file, one seam — entity→`(columns, values)` marshal and row→entity mapper, both pinned against a
Go oracle, with `PostScan`/`PostMapArgs` hooks; then per-repo adapters. DTOs stay **local to `nd-db`**: `nd-model` is
#06's crate and a shared file, and `UserRecord`/`PlayerRecord`/`RadioRecord`/`Tag`/`Scrobble`/`Transcoding`/`Genre`
already set that precedent. Do not touch `nd-model` for hydration. Harness precedent to reuse verbatim:
`nd-db/tests/runtime_player_repo.rs:34-46` (`apply_fresh_schema` + `Pool` + `SQLStore`), `_foreign_keys=on` in the DSN
(`db/db.go:55`), `SEEDEDRAND`/`NATSORT` registered per connection via `schema::open`.

### 1b. Go contract for that layer (lane `e189495e…`, 2026-11-11, parent-verified)

A high-reasoning lane produced the Go-side contract; its corrections and my own verification follow. Two of *my* brief's
claims were wrong and are fixed here: `Discs` is `map[int]string` (`model/album.go:124`), not `map[string]Disc`; and the
Rust search helpers are at `artist_repo.rs:419,425`, not `:412,418` (`:412` is an attribute).

**Write funnel** — `toSQLArgs` (`persistence/helpers.go:20-42`) is **one** per-value type switch, not two passes:
`structs.Map` → for each value, unwrap a non-nil `*time.Time` else call `driver.Valuer` → then `PostMapArgs`. A **nil**
`*time.Time` stays nil and therefore binds NULL. Read funnel: `queryWithStableResults` (`sql_base_repository.go:411`)
scans via `ScanStruct` (`:429`); `queryAll` at `:441`; `queryAllSlice` at `:460`. dbx maps columns by **`db` tags** (not
`structs` tags), recurses into anonymous/nested non-scanner structs, prefers shallower shadowing fields, initializes nil
embedded pointers, and **discards unknown columns into `sql.NullString`** (`dbx@v1.12.0/rows.go:65-104`,
`struct.go:160-206,249-257`).

**8 models have `PostScan` row hooks** (`grep -c 'func.*PostScan' persistence/*.go` = 8): album, artist, folder,
mediafile, playlist, playlist_track, scrobble_buffer (delegates to mediafile's and overwrites
`MediaFile.ID = media_file_id`, `scrobble_buffer_repository.go:23`), and **user** — `dbUser.PostScan` unmarshals
`LibrariesJSON` (`user_repository.go:36-42`), so user is *not* hook-free as an earlier revision of this doc claimed.
5 have `PostMapArgs` write hooks: album, artist, folder, mediafile, playlist. library, tag, genre, radio, share,
scrobble, transcoding, property, artwork, artwork_queue, plugin, user_props and smart_playlist hydrate a **plain
model with no hooks** — Rust must not invent any. `smart_playlist` is not a separate repository at all (`smart_playlist_repository.go:15-23`). Note
`sql_tags.go:15-20`'s `dbTag` is the **JSON column representation**, not the tag repo's row.

**NULL semantics differ from Go and must be pinned, not assumed.** Go delegates to `database/sql`: NULL into a
non-pointer numeric/string/time field **errors**, it does not zero (`convert.go:489-535`); pointer fields become nil.
Go's nullable pointers: album RG + `external_info_updated_at` (`album.go:53-54,62`), mediafile bit depth/BPM/RG
(`mediafile.go:67,84,95-98`), artist timestamps (`artist.go:39,45-46`), playlist evaluated (`playlist.go:44`), user
last login/access (`user.go:16-17`), share expiry/last visited (`share.go:19-20`), annotation timestamps
(`annotation.go:10-14`). `model/artwork.go:65-67` explicitly warns about non-pointer `attempted_at`/`updated_at`.

**JSON: two traps, both verified empirically.**
1. **Tags are not a plain map dump.** `marshalTags` (`sql_tags.go:37-47`) emits role-keyed arrays of `{id, value}` where
   `id = model.NewTag(name, value).ID`, drops empty value lists, and serializes nil/empty as `{}`. Rust already has a
   faithful port — `sql_tags.rs:129 marshal_tags` using `nd_id::new_tag_id` — **but nothing calls it** (only its own
   tests), and **there is no participant marshaller at all**: `sql_participations.rs` has only filter/subquery helpers,
   no `marshalParticipants`/`unmarshalParticipants` equivalent (`sql_participations.go:50-65`, which also errors on an
   empty participant string).
2. **Integer-key ordering.** Go marshals `map[int]string` with keys sorted **as strings**: verified `{"1":"a","10":"j",
   "2":"b"}`. A Rust `BTreeMap<i32,String>` emits `"1","2","10"` — different bytes. Keys must be stringified before
   sorting.
3. **Escaping.** Go escapes `<`, `>`, `&`, U+2028, U+2029 (verified: `"a<b>c&d"`); serde_json does not.
   **Decision given the user's no-migration rule: match Go's *shape and round-trip fidelity*, not its escaped bytes.**
   The JSON is internal, self-consistent, and the existing `nd-mapping` goldens are *parsed*, not byte-compared
   (`go_oracle.rs:88`; zero `u0026`/`u003c` occurrences), so serde's raw output is fine as long as decode is exact and
   key ordering is stable. Document this as a deliberate divergence rather than silently diverging.

**Search feeders are artist-only and cannot express album/mediafile.** `formatFullText` = one leading space +
`SanitizeStrings` (`sql_search.go:15-17`); `SanitizeStrings` trims/concats, `Clear`, slash→space, lowercases,
transliterates, strips quote/bracket/comma chars, splits, **sorts and dedupes** (`str/sanitize_strings.go:14-36`).
`NormalizeForFTS` preserves input order and adds transliterated variants, skipping empty/unchanged/case-dupes
(`str/normalize_fts.go:10-44`). Album's inputs are name, sort name, album artist, participant names, disc values,
album-version and catalog-number tag values (`album_repository.go:71-80`); mediafile's are title/album/artists/sort
fields/disc subtitle/participants (`mediafile_repository.go:69-78`), and `FullTitle` may append a subtitle
(`mediafile.go:109-113`). The two existing Rust helpers take 2 and 1 args. `Participants.AllNames` iterates a Go map,
so participant ordering is **not stable** (`participants.go:174-185`) — safe for `full_text` because it sorts words, but
`search_participants` bytes depend on order, so **persist no ordering inherited from either side** (same rule as #16).

### 1c. Shared-layer slice plan (lanes launched 2026-11-11, provider `pi/bifrost/openrouter/coding`)

No configured Paseo profile names that model, so the lanes use provider discovery as a deliberate fallback.
`PASEO_WORKSPACE_ID` is unset for the parent, so children inherit the parent workspace by agent-scoped creation;
no new workspace or worktree is created. **One writer per file, no overlap, children do not commit** — the parent
integrates, runs the gate, and commits.

| Slice | Kind | Owns | Blocks |
|---|---|---|---|
| **S-A participant JSON** | write | `sql_participations.rs`, `tests/participants_json.rs`, `testdata/go_participants/` | **LANDED `2a99a681`** |
| **S-B search feeders** | write | new `sql_feeders.rs` + one `lib.rs` line, `tests/search_feeders.rs`, `testdata/go_feeders/` | **LANDED `3f8b5a8d`** |
| **S-C PostScan contract brief** | read-only | → `docs/rust-port/hydrate-contract.md` | **LANDED `deeb837d`** |
| **S-D1 JSON/Role bridge** | write | `sql_participations.rs`, new `sql_json.rs` + one `lib.rs` line, `tests/sql_json.rs`, Go oracle | **LANDED `cb11b645` (review APPROVE, gate 1514/0)** |
| **S-D2a marshal core/hooks** | write | `marshal.rs`, `lib.rs`, album/mediafile/folder adapters, `runtime_marshal.rs` | **LANDED (review APPROVE, gate 1536/0)** |
| **S-D2b1 records: folder+playlist** | write, after S-D2a | `folder_repo.rs`, `playlist_repo.rs`, executed write->read tests | S-D2b2 |
| **S-D2b2 records: album+mediafile** | write, after S-D2b1 | `album_repo.rs`, `mediafile_repo.rs`, executed write->read tests | S-D3 |
| **S-D3 row mapper/hydration scout** | read-only, now | none (contract brief) | S-D3 |
| **S-D3 scout brief** | read-only | `hydrate-scan-contract.md` (8-hook inventory, decoder gaps, seam design) | **DONE** |
| **S-D3a hydrate core + folder/user** | write, after S-D2b1 | new `hydrate.rs` + `lib.rs`, folder/user hooks, executed reads | S-D3b |
| **S-D3b album/mediafile hydration** | write, after S-D3a | album/mediafile repo read adapters + annotation/bookmark/ToGenres readers | S-D3c |
| **S-D3c artist/playlist/track/scrobble** | write, after S-D3b | stats/similar-artists/criteria decoders + delegating hooks | #17 |

S-D3 is pre-scoped in [`hydrate-scan-contract.md`](hydrate-scan-contract.md): all 8 `PostScan` hooks inventoried
with exact guards/prefixes, column-to-decoder map, the six missing decoders (annotation columns, `bookmark_position`,
`criteria` rules, `similar_artists`, `library_stats_json`, `ToGenres`), and the ownership decision that hooks and record
DTOs live in nd-db, not nd-mapping. It corrects this repo's earlier claim that base reads do not join `annotation`.

The entity write contract for S-D2b is pinned in [`entity-write-contract.md`](entity-write-contract.md):
executed `structs.Map` column sets, db-wrapper shims, PostMapArgs add/overwrite/delete lines, `selectUpdateColumns`
drops, DDL reconciliation (`releases` orphan, annotation-only `average_rating`), nullable time/RG fields, partial
`colsToUpdate` callers, and per-entity executed write->read requirements. Follow it literally; do not re-derive.

The original S-D was too large for one ownership-safe lane. **S-D1** is landed. **S-D2a** built the shared
sorted-column/value unit and faithful PostMapArgs transformations, with executed writes through the existing `put_sql`
APIs. **S-D2b** must then eliminate caller-supplied column arrays with complete repo-local records/adapters — S-D2a is
not allowed to declare victory by merely moving an arbitrary base map to another module. **S-D3** owns row→entity
hydration and the 8 `PostScan` hooks from `hydrate-contract.md`. The parent reviews and gates between every slice.

S-A and S-B are independent by construction: the feeder seam takes **participant names as `&[String]`**, so it does
not depend on the participant type.

**Review findings that matter for S-D's oracle:**
- **Neither Go golden is wholly byte-stable.** `go_participants` reorders entries/flat rows across runs because
  `Participants` is a map (observed `conductor`/`lyricist` swaps). `go_feeders` is stable except for
  `search_participants`, whose `AllNames` input has the same map-order instability (reviewer reproduced two orderings
  in 5 runs at golden lines 41 and 73). Compare those fields as **multisets**, never bytes. All other feeder fields
  are byte-stable after regeneration. The committed Rust tests follow these rules.
- **The two oracle harnesses take different third arguments** — `go_participants` wants a **directory**, `go_feeders`
  wants the **output file**. Invoking either wrongly exits non-zero without writing, so an unchecked `diff` against
  the old committed golden matches trivially and looks green. Run exactly as each README documents, require exit 0,
  and confirm its `wrote …` line before trusting a diff.
- **Role unification is an S-D acceptance criterion.** Three equivalent representations currently exist:
  `sql_participations::Role` (owned `String`), `artist_repo::ALL_ROLES` (`&str`), and `nd_id::Role` (`&'static str`).
  S-D must define one explicit conversion seam and pin all 14 keys equivalent; it must not silently invent a fourth.

`serde_json` is already an `nd-db` dependency (`Cargo.toml:24`), so these slices added no crate. Oracle precedent:
`nd-mapping/tests/go_oracle.rs` + `tests/testdata/go_oracle/` (Go harness dumps parsed goldens; unstable collections
compare canonically rather than byte-for-byte).

### 2. #17 scanner — minimum executed surface is 8 repos + 1 mapper, not ~14

A fresh scan **reads back** what it wrote, so phases 2/3/4 and `finalize` need executed *reads*: an executed-read seam
is unavoidable. Required executed repos: `library_repo`, `folder_repo`, `mediafile_repo`, `tag_repo`, `album_repo`,
`artist_repo`, `property_repo`, `artwork_queue_repo`, plus item 1's shared mapper. `User().FindFirstAdmin`
(`phase_4_playlists.go:56`) already executes (`user_repo.rs:693`).

Writes already rendered today: `Property` Put/DefaultGet/Delete; `Library` ScanBegin/ScanEnd/SetScannedPID/AddArtist/
RefreshStats; `Folder` Put/MarkMissing; `Tag` Add/UpdateCounts/purgeUnused; `Artist` Put/purgeEmpty/markMissing/
refreshStats; `Album` Put/Touch/TouchByMissingFolder/ReassignAnnotation/CopyAttributes/RefreshPlayCounts/purgeEmpty;
`MediaFile` Put/MarkMissing/MarkMissingByFolder/Delete/DeleteAllMissing; `Artwork` DeleteForItems; `ArtworkQueue`
Enqueue/EnqueueIfMissing/EnqueueAllMissing; and `DataStore.GC` (11 sequential sub-ops, `persistence/persistence.go:246-277`).

Orchestration order (`scanner/scanner.go:150-179`): phase 1 → 2 sequential, 3 ∥ 4, then GC → enqueue-missing-artwork →
refresh-stats. Open sub-problems, each its own lane rather than one XL blob: symlink semantics (`core/storage/`);
subprocess IPC — Go encodes tag-reader results with `gob` over pipes, so the Rust port needs a **wire-format decision**,
not a translation; the file watcher; phase ordering; and the idempotent second pass.

**Watcher licence answer:** Go pins `github.com/rjeczalik/notify v0.9.3`, which is **MIT** (verified in the module
cache), so licensing is not a blocker — choose a Rust watcher on behaviour.

**Sequencing conclusion:** item 1 gates a *vertical* #17 slice. The first honest #17 deliverable is phase 1 over a
fixture tree with executed writes, not an end-to-end scan.

---

## Historical plan (2026-11-04, end of the Phase-4 body) — kept as the phase record

Phases 1–4 below were **DONE except #15** as *shape*. Verified gate at that boundary:
`cargo fmt --all -- --check` clean, `cargo clippy --workspace --all-targets
--all-features -- -D warnings` clean, `cargo test --workspace` = **969 passed / 0
failed**; worktree clean, all commits GPG-signed. That figure is historical — see the
2026-11-10 gate above.

- **Phase 1** — #06 criteria DSL DONE (nd-model, 40 lib tests); #04 optimize/
  backup DONE, `repair.rs` (`db/repair.go`, 367L FTS5 verify/repair) still
  DEFERRED as its own lane; #07 tail landed with `mock_ffmpeg.rs` + live
  container fixtures (symlink-chain/invalid-files cases uncovered).
- **Phase 2 slices 1–4** — #05 `nd-db` DONE: pool/store/harness + `sql_*`
  helpers + all repos + `criteria_sql.rs` (904L criteria→SQL compiler:
  `Cond` tree, threaded field map, join planner, orderBy coalesce/cast/role
  forms, JSON merge groups @350 batch, NFC/NFD `pathVariants`); 316 lib tests.
  Two-phase rowid semantics pinned to `sql_search.go:126-141`: base pagination
  stripped from BOTH sql and args, page args bind AT the JOIN position.
  Slice-4 security gate verified structurally (artist/radio via
  `with_field_white_list`; scrobble/tag/transcoding/album take
  caller-materialized fragments; `criteria_sql.rs` consumes no `QueryOptions`)
  — a dedicated security-reviewer subagent pass over slice 4 was NOT run.
- **Phase 3** — #13 `nd-auth` DONE (66 tests, security MUST-FIXes applied);
  #12 `nd-sched` scheduler half DONE (39 tests); its maintenance/logging half
  is still unspecified.
- **Phase 4** — #14 `nd-stream` decision half DONE (types/aliases/codec/
  limitations/decider/probe_json/token/legacy_client; 103 tests; HS256 tokens
  with explicit secret + injected clock, issuer = `nd-conf` `JWT_ISSUER`).

**Next body of work (2026-11-04 text, superseded by the 2026-11-10 section above):** (1) #15 `nd-stream`
serving half — `media_streamer`, `limiter`, `archiver`, `handle_streams`, Range matrix +
cancellation soak (decider's probe path is stubbed to `ProbeInput::NotAvailable until this lands); (2) `nd-dbschema/src/repair.rs`; (3) specify + build the
#12 non-scheduler half.

Supersedes the earlier packet. Written after a full re-read of the ledger
(`issues.md`), the dependency graph, and an advisor review of the sequencing.
State at time of writing: 59 test groups green, fmt 0 diffs, per-crate clippy
clean, worktree clean. Done: #01, #02, #03, #07 (code-complete + MockFFmpeg),
#08, #09 (parsers), #10 (parsers), #11, #04 (fresh schema + migration runner),
#06 (DTO half). The abandoned criteria drafts are archived at
`docs/rust-port/drafts/{fields,operators}.rs` (untracked work, not deleted —
the verdict + fix list is in git history alongside this file's previous text).

## Phase 1 — close the three open tails (one lane each, all independent)

Three crates, three owners, no shared files. Any two can run in parallel.

### Lane 1 — #06 `nd-model`: criteria DSL
- `model/criteria/criteria.go` (164L): `Criteria` container, `all`/`any` conflict
  rejection, `EffectiveLimit`/`ResolveLimit`/`IsPercentageLimit`, `limitPercent`
  clamp, `refreshDelay` parse/format (needs `nd-utils` dep: `parse_duration`/
  `format_duration`), child-playlist id/path extraction + dedupe.
- `model/criteria/json.go` (187L): `unmarshalExpression` + `optionalConjunction`
  present-flag, `normalizeBoolFields`, `marshalExpression` length-1 rule.
- `model/criteria/sort.go` (62L): `OrderByFields`/`SortFieldNames`.
- `fields.go`/`operators.go`: reuse the archived drafts as base (they were
  ~compiles-close and half-real respectively); apply the fix list — missing `->`,
  `Expression::Contains {…}` map-literals, `..FieldInfo::default()` spreads,
  `&mut FieldMap` params, `assert!`/`assert_eq!`.
- Tests: all 40 `It(...)` blocks of `criteria_test.go` + `fields_test.go` +
  `operators_test.go` + `sort_test.go` + `walk_test.go`.
- Gate: `cargo build/test/clippy -p nd-model`, fmt at root.
- **Note**: `walk.go` folds into `operators.rs` (37L, not worth a module).

### Lane 2 — #04 tail `nd-dbschema`: optimize/backup/repair
- `optimize.rs` ← `db/optimize.go` (274L): ANALYZE scheduling.
- `backup.rs` ← `db/backup.go` (184L).
- `repair.rs` ← `db/repair.go` (367L): FTS5 verify/repair, `ftsTables` `:12`,
  `VerifyFTS` `:102` semantics.
- Tests: one per module against a fresh-schema `:memory:` DB (corrupt an FTS
  index, repair it, re-verify).
- Style: copy `schema.rs`/`migrate.rs` match-based `Result` handling exactly.

### Lane 3 — #07 tail `nd-ffmpeg`: remaining fixture cases
- Symlink-chain probe cases (`tests/fixtures/symlink*`), `invalid-files`
  directory cases (unparsable containers must fail as launch errors, never
  parse errors — the invariant `probe_exec.rs` already encodes).
- Small; do alongside either lane above.

## Phase 2 — #05 `nd-db`: the repository layer (4 bounded slices)

The critical path. rusqlite is pinned (sqlx dropped — never resolved into
`Cargo.lock`, referenced nowhere in docs). The advisor's one structural fix:
**the SQLite harness moves into slice 1**, not slice 4 — retry semantics
(`WithTxRetry` 3 retries/5s delay) and `SetMaxOpenConns(1)` behave differently
over a real `Mutex<Connection>`-style pool than against mocks, and every later
slice builds on those semantics.

### Slice 1 — connection + ownership core + minimal harness
- `SQLStore` from `persistence.go:17-40,71,171,203-235`: lazy repo builders,
  `WithTx`/`WithTxRetry` (txMaxRetries=3, delay 5s), busy-retry.
- **Pool mechanism — DECIDED (2026-11-03)**: a fixed pool of N owned
  `rusqlite::Connection`s behind `Mutex`, N = `max(4, available_parallelism())`
  (`conf/configuration.go:1213-1216`, NOT 1 — that value is the e2e harness's
  test setting, `e2e_suite_test.go:243`). `Connection` is `!Sync` and sqlite
  serializes writers on a single write lock anyway (`configuration.go:1211`
  comment) — more conns buy reader parallelism while disk-I/O-blocked, which is
  exactly what Go's pool does. No pool crate, no async connection surface:
  sync rusqlite inside the async shell, blocking calls via `tokio::task::
  spawn_blocking` at the store boundary. Reuse #04 `schema::open` per connection
  so `SEEDEDRAND`/`NATSORT` registration (`schema.rs:86`) is per-connection as
  `schema-contract.md:177` requires.
- **Retry mapping — DECIDED**: `SQLITE_BUSY`/`SQLITE_BUSY_SN`/`SQLITE_LOCKED`
  (`rusqlite::Error::SqliteFailure` extended codes 5/17, plus `Sys` wrap)
  → retry up to 3 with 5s delay; every other error → fail immediately. Go
  reaches this via ctx-value + `withBusyRetry` (`persistence.go:234`); the
  port classifies at the error site instead — same observable semantics,
  no ambient context.
- **Minimal harness now**: temp file DB → #04 `open` + fresh schema → one trivial
  repo round-trip. Harness grows with each slice.
- **Schema-drift guard**: dump the fully-migrated Go schema (`go run` with
  `DbPath` pointed at a temp file, `sqlite3 .schema`) and diff it against the
  Rust fresh DDL — machine diff, not eyeball parity. The 23 repos' SQL will
  silently break on column/type drift otherwise.

### Slice 2 — `sql_base_repository.go` (678L) + `sql_*` helpers (1,567L)
- dbx→Rust query builder: `toSQL()` `:358`, `logSQL()` `:661`, `sqlRepository`
  base. Then `sql_search`, `sql_search_fts` (`buildFTS5Query` `:108`, fts5
  regexes `:38-44`), `sql_search_like`, `sql_bookmarks`, `sql_participations`,
  `sql_tags`, `sql_annotations`, `sql_restful`.
- **Security pass (advisor's second fix)**: `sql_search_fts` + the order-by/
  sort expression path are the injection surfaces (user-supplied filter/sort →
  SQL). Parameterized values only; where dbx interpolates identifiers (order-by
  column names), an allowlist built from the `fields.rs` registry. A
  security-reviewer pass is part of this slice's definition of done.

### Slice 3 — leaf repos, mock-repo-backed, #13's dep first
- **SECURITY GATE (security-reviewer, slice-2 pass, finding 1)**: `sql_restful.rs`
  `parse_rest_filters` interpolates the user-controlled filter *key* as an SQL
  identifier when no whitelist is registered — faithful to Go (`sql_restful.go:33-44`,
  nil whitelist = allow) but every slice-3 repository consuming it MUST call
  `with_field_white_list` with names from the `nd-model/src/fields.rs` registry
  before exposing the repository to user filters. Lanes that ship a restful-backed
  repository without registering a whitelist are rejected at review.
- Order within the slice: **`user_repository` first** (`persistence/
  user_repository.go:129-144` epoch revocation is #13's hard dep) — this is the
  earliest legal point for #13 to start. Then the Test-block six:
  `user`, `user_props`, `scrobble_buffer`, `scrobble`, `share`, `tag`,
  `transcoding` (that's 7 files; the plan's Test block names 6 mocks).
- Port `tests/mock_*_repo.go` (21 files on disk, incl. the oddly named
  `mock_radio_repository.go`) as Rust fixtures as each repo lands.

### Slice 4 — remaining 15 repos + full harness
- album, artist, mediafile, library, folder, playlist, playlist_track, playqueue,
  then plugin, property, radio, artwork, artwork_queue, smart_playlist, genre.
- `criteria_sql.go` (904L) LAST — needs #06 green (the bridge is the only
  touch point; `issues.md:228-229`). If #06's lane has finished, slot it here;
  otherwise it is explicitly deferred past this phase, not blocking slices 1–3.
- e2e-style SQLite harness per `e2e_suite_test.go:417`.

## Phase 3 — parallel fill-in (while/after Phase 2)

- **#12 `nd-sched`** (LOW risk): `scheduler/{scheduler,crontab_schedule,
  log_adapter}.go`, `log/**`, `core/maintenance.go`; crontab parsing must agree
  with the `ScanSchedule` validator (`conf/configuration.go:905`). Deps #S1+#03
  green; also needs the #04 tail (maintenance ops) — which is why Lane 2 runs in
  Phase 1.
- **#13 `nd-auth`** (HIGH, security): starts when `user_repository` lands
  (slice 3), not after all of #05. Model B credential scheme, dual signing
  secrets, epoch revocation, 3-way credential switch (jwt/apiKey/internal —
  the `p`/`t+s` arms are NOT ported, #25 CANCELLED), `X-ND-Authorization`,
  CIDR+unix-`@` trusted sources, body caps. Vetted crates only (`argon2`,
  `jsonwebtoken`/`jose`, `rand`). Fresh security-reviewer lane mandatory.
  **Stay sequential with #05** despite the overlap temptation — the
  Go-pseudo-code hazard makes parallel-API rework expensive.

## Phase 4 — stream, last of this body

- **#14 `nd-stream/src/decision`**: transcode decision tables; pure tables
  first, token half after #13. `decider_test.go` (~1300L) is the spec.
- **#15 `nd-stream`** serving half: `media_streamer`, `limiter`, `archiver`,
  `handle_streams`; Range matrix + cancellation soak (the #07 mock finally
  pays off here).

## Gate discipline (every slice/lane)

Per-crate: `cargo build -q -p CRATE && cargo clippy -p CRATE --all-targets
--all-features -- -D warnings && cargo test -p CRATE`. At each **phase
completion** (advisor's gate-gap fix): workspace-wide `cargo clippy --all-targets
--all-features -- -D warnings` + workspace `cargo test` + `cargo fmt --all
-- --check` — per-crate gates miss cross-crate feature interactions.
Commits GPG-signed, no co-author trailers, no gpgsign bypass.

## Standing hazards

- **Lane losses are real.** Three lanes died on 2026-11-10 (one provider usage limit, two provider
  `429 max connections`, one daemon `spawn pi ENOENT`) and their **uncommitted work was destroyed with the
  worktree**. Bank progress in commits per milestone; a dead lane should lose a milestone, not a phase.
- **Do not archive a lane whose worktree holds uncommitted work** — archiving removes the worktree. Collect the
  diff or commit first.
- **Provider capacity is 4 concurrent connections.** Running four Strata lanes plus the parent can hit it;
  launch fewer at once or accept the 429.
- **Provider budget exhaustion (2026-11-11).** All `openai-codex/*` models hit the ChatGPT Plus limit
  (~130 min cooldown) and `bifrost/openrouter/coding` hit a hard 20 USD cap, killing three lanes mid-run —
  the #08 reviewer and the first #16 attempt died with their analysis unsaved. Only
  `pi/bifrost/vllm/strata-coder` (local) stayed usable. When delegating, prefer the local model, keep briefs
  small enough to finish in one budget, and check `paseo_get_agent_status` for `status: error` + the stop
  reason before relaunching. Children must commit per milestone so a budget death costs a milestone, not the
  analysis (the parent then performs the lost review directly).
- **rustc 1.99.1 rejects Go-flavored pseudo-code** (`a + b` concat, `+=` on
  `&str`, `.append()`, `std::map<...>`, map-literal constructors, missing `->`).
  Workers loop write → `cargo build -q -p CRATE` → fix from compiler messages.
  One writer per crate at a time.
- **Worker exhaustion**: executors have died mid-thought twice on this port.
  Slices are bounded so a dead lane loses a slice, not a phase. Verify with
  `ls`/`grep` before trusting a crew's "completed" report.
- **`Connection` is `!Sync`**: pool mechanism + retry mapping for #05 slice 1
  are now DECIDED in the plan above — workers implement, they don't re-derive.
- **`bench/`, `docs/perf/`**: COMMITTED (2026-11-03) as the crew-run perf
  baseline for future comparison; worktree is fully clean.