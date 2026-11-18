# Plan of record for the next body of work

## Current execution plan (audited 2026-11-18 at `60cdd184`)

**Current evidence:** [`current-status.md`](current-status.md). The parent freshly measured **1816 passed / 0 failed /
1 ignored** workspace tests, **737 / 0 / 0** DB tests, clean fmt/build/clippy/diff checks, and a successful doc build
with **22 warnings**. This is a green regression suite, **not** scanner or server readiness. S-D3g1 (`f210431f`),
S-D3g2 (`a8e4d57c`) and follow-up (`60cdd184`) are landed. All eight row hooks exist; playlist rules decoding is wired.

### Authority and launch rules

- This current section sets milestone order; `current-status.md` records measured capabilities/evidence; the
  [`subplans/`](subplans/) assignment contracts refine method-level scope. Scoped contracts below retain source
  history but their current amendments supersede old missing/remaining labels. Do not schedule from historical tables.
- **State axes:** rendered SQL / decoded records / executed reads / executed writes / mounted routes / real end-to-end
  behavior. Never infer one axis from another or from test counts.
- Today's work is **planning/documentation only**. A subplan is not implementation authorization. Resolve its decisions,
  exact prerequisites and file ownership before assigning one bounded task. No commit/push authorization is implied.
- Discover current Paseo profiles; inherit the verified parent workspace. One writer for the entire shared checkout,
  not one writer per file. No new workspace/worktree or destructive cleanup without user approval. Fresh read-only
  review after writing stops, then selected fixes and parent CLI gates. Scratch only in `/tmp`; no automatic formatting.

### Revised milestones and dependencies

1. **DB safety and configuration gates, before enabling writes:** repair media-file missing-update bind order; unify
   scanner TEXT timestamps/idle sentinels across writers, comparisons and hydration; measure intended path/PRAGMAs
   on every pooled connection; settle typed repository-error/transaction composition. Add executed regressions, not
   more renderer-only assertions. Existing helpers have latent defects and must not be connected unchanged.
2. **Bounded DB runtime packages:** executed property/library/tag/folder writes; artist/album/media-file Put/PutByMatch
   with base/FTS/junction/link atomicity; typed filtered scanner reads; artwork invalidation/queue enqueue subset.
   Library bootstrap policy belongs to the bootstrap caller; DB supplies typed executed operations. Test rollback,
   retry, immutable fields, non-target rows, empty/chunk boundaries, and actual read-back through production methods.
3. **Honest scanner phase-1 vertical slice:** storage/stat/symlink/ignore and metadata-to-record adapters, actual
   library/folder IDs, pure album aggregation/change detection, and one fixture-tree ingest over those DB APIs.
   Prove unchanged second-pass behavior and failure-safe missing marking. Call it phase 1, not complete #17.
   The old '8 repos + 1 mapper' is a core-read inventory, not a full scanner completion gate.
4. **Bootstrap/HTTP/canonical route contracts:** capture the precise scoped API manifest and native/derived stream
   fixtures before handlers. Build minimal configuration -> schema/pool -> configured library/admin -> router/listener
   wiring. Close login-session/native-audience and API-key HTTP integration, and prepare/header/Send/body-lifecycle
   seams in the stream adapter. Do not weaken auth or buffer an entire transcode merely to adapt existing APIs.
   Contract capture and non-writing investigations can proceed before ingest; implementation still has one writer.
5. **First runnable slice (#27A):** fresh DB -> scan a real track -> login -> authorized canonical lookup -> direct Range
   and forced transcode -> real socket disconnect with zero live children and recovered concurrency slots. Require
   ffmpeg/codecs and exercise the paths, rather than accepting skip-as-success. Full scanner/watchers, full #27,
   repair/full maintenance and deferrable metrics are not automatic gates for this deliberately limited slice.
6. **Complete scanner and DB consumers:** phase-2 move/missing identity and annotation/reference preservation;
   phase-3 aggregates/stats; phase-4 playlist service; scoped eleven-operation GC/finalize; controller/progress,
   restart/selective/cancellation/resource behavior; then watcher/process protocol parity. Go gob carries scanner
   `ProgressInfo`, not tag-reader results. Initial in-process ingest does not require mixed Go/Rust IPC.
7. **Consumer-gated residuals and later services:** durable g1/g2 Go evidence; sibling first-error reproductions;
   genre/options/participant/cursor/smart-playlist/track query issues; schema-golden verification; FTS repair and
   maintenance/logging; full artwork/share/scrobbler/external/lyrics/playlists/player-session/API/share-HTML expansion.
   The player-session service is retained (API-01-placed register/touch over the landed `PlayerRepo`); the mpv/jukebox
   playback-device scope is dropped unported (`D-PLAYER26`). Per-device preference editing is retained (`D-PLAYER-EDIT`): owner-scoped
   `GET`/`PUT /api/player/{id}` (ownership-restricted `read`/`update_owned`, immutable `user_id`, whitelist enforced) plus
   a **read-only** `GET /api/transcoding` profile list, which needs #05's executing `transcoding_repo` read and a BOOT-owned
   seed from `nd-conf` `DEFAULT_TRANSCODINGS` — a picker with no rows cannot set `transcoding_id`. Profile **editing** stays dropped. Keep accepted differences, known bugs and unverified behavior
   separate. Complex service tails remain specification-blocked until their own contracts are ready; cancelled product
   surfaces stay cancelled.

Milestones 3-5 form the minimal vertical path; milestone 6 is needed for a complete #17 claim, not every first-playback
test. The subplan index resolves exact task IDs, cross-plan prerequisite aliases, decision gates, file collisions, and
READY/BLOCKED status before launch. Do not revive the old dependency cycles or make metrics a first-boot prerequisite.

---

## Historical state (2026-11-11, at `12e270f7`) — superseded by the audited plan above

The remainder is a dated implementation/review record, not an active scheduler or orchestration permission. Its
old models, workspace paths, lane status, test counts and 'remaining' assertions must not override the current plan.

Measured gate (parent, integrated): fmt clean; workspace build clean; clippy `-D warnings` clean;
`cargo test --locked --workspace --all-targets --all-features --no-fail-fast` = **1514 passed / 0 failed** across
57 test binaries. Progression: `d19526e3` (#15 transcode cache) 1442 → `a7afda2a` (timestamp convention) 1453 →
`8c89aeb9` + `12e270f7` (INSERT alignment, playlist `INSERT … SET`, `0000-00-00` sentinel) 1459 →
`2a99a681` + `3f8b5a8d` (participant JSON + FTS feeders) 1490 → `cb11b645` (JSON codecs + Role bridge) 1514 → S-D2a (marshal core + PostMap hooks) 1536 → S-D2b1 (folder/playlist records) 1550 → S-D2b2 (**album/mediafile records**) **1558**.

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
| **S-D2b2 records: album+mediafile** | write | `album_repo.rs`, `mediafile_repo.rs`, executed write->read tests | **LANDED (review APPROVE, gate 1558/0)** |
| **S-D3 row mapper/hydration scout** | read-only, now | none (contract brief) | S-D3 |
| **S-D3 scout brief** | read-only | `hydrate-scan-contract.md` (8-hook inventory, decoder gaps, seam design) | **DONE** |
| **S-D3a hydrate core + folder/user** | write, after S-D2b1 | new `hydrate.rs` + `lib.rs`, folder/user hooks, executed reads | S-D3b |
| **S-D3b album/mediafile hydration** | write, after S-D3a | `hydrate.rs` (`row_to_album`/`album_post_scan`/`row_to_mediafile`/`mediafile_post_scan`), `album_repo::AlbumRow`, `mediafile_repo::MediaFileRow`, `sql_tags::tags_to_genres` + `Genre`, `tests/runtime_hydrate_album_mediafile.rs` | **IMPLEMENTED, review APPROVE** (uncommitted working tree; fresh-round reviewer found no P1/P2/P3; gate re-measured: fmt clean, clippy `-D warnings` clean, 1665 passed / 0 failed / 1 ignored across 63 binaries; blocks S-D3c) |
| **S-D3c artist/playlist/track/scrobble** | write, after S-D3b | stats/similar-artists/criteria decoders + delegating hooks | #17. **PARTIAL.** The ARTIST part LANDED as lane "S-D3d" (stats fold + similar-artists decoder + `ArtistRow`/`ArtistRepo` reads + `hydrate_artist_artwork`, `tests/runtime_hydrate_artist.rs`, gate 1720/0 across 65 after fix round 2). The playlist_track + scrobble_buffer part LANDED as lane **S-D3e** (contract `:208-211`): delegating post-scan records + `id` overwrite on the seam, `PlaylistTrackRepo::read`/`get_all`, `ScrobbleBufferRepo::next`, `hydrate_track_artwork` via the media-file wrapper, `tests/runtime_hydrate_playlist_track_scrobble.rs` (24 tests, re-measured), gate 1746/0 across 66 (that workspace figure is one short of the 1747 its own `nd-db` total implies — see the arithmetic note in the S-D3f section) — see the S-D3e section below. The playlist + radio `hydrateItems` wrappers AND both repos' read sides LANDED as lane **S-D3f** (contract `:213-215`): `hydrate_playlist_artwork`/`hydrate_radio_artwork` over the shared `hydrate_items`, `PlaylistRepo::get`/`get_all`/`find_by_path`/`get_playlists`, `RadioRepo::get`/`get_all`, `tests/runtime_hydrate_playlist_radio_artwork.rs` (20 tests), gate 1776/0 across 67. Still open: playlist `criteria` rules hook wiring beyond S-D3a, `getParticipants`/`GetWithParticipants`, and all cursor/streaming (`streamByIDs`/`hydrateCursor`) |

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
cancellation soak (decider's probe path is stubbed to `ProbeInput::NotAvailable` until this lands); (2) `nd-dbschema/src/repair.rs`; (3) specify + build the
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
## S-D3a landed + a Go lowercasing divergence (2026-11-12)

**S-D3a (shared hydration seam)** — `nd-db/src/hydrate.rs` (new) provides the
row→record seam for the eight PostScan hooks: `RowGet` (NULL-into-non-pointer
errors like Go's `database/sql`, `get_opt_dt` routed through
`time::optional_sentinel`, `has_column` for absent joined columns),
`HydrateError::Decoded` carrying Go's wrapper prefix verbatim, and the two-phase
raw-row → decode shape (`row_to_user` + `user_post_scan`). `user_repo` is
re-pointed at it with no behaviour change.

**Go `strings.ToLower` divergence (found by the S-D3 verification lane, fixed by
parent).** Go derives tag ids with `NewHash(strings.ToLower(name),
strings.ToLower(value))` (`model/id/id.go:46-48`); Rust used
`str::to_lowercase()`. A differential sweep of every scalar value against the
real Go function (1488 runes change in Go) found exactly two divergences:

- Go's non-ASCII path is `Map(unicode.ToLower, s)` — rune-independent, so it
  never applies Unicode final-sigma folding. Rust folds: `ΟΔΥΣΣ` → Go `οδυσσ`,
  Rust `οδυσς`; `ΙΣ` → Go `ισ`, Rust `ις`.
- Go maps U+0130 to plain `i`; Rust yields `i` + U+0307 (two code points).

Because tag ids are persisted and tag *values* are arbitrary user text, this
would have minted different ids than an existing Go-written database for the
same tag. `nd_utils::str::to_lower` now replicates Go exactly, and
`nd_id::new_tag_id` uses it, with Go-measured goldens
(`ΟΔΥΣΣ` = `0UEOhdgRzYCxNU4pOSozfz`, `ΙΣ` = `3iW1R9fgXXkzfoV7433up2`,
`İstanbul Jazz` = `3zPshTNGcEwd2fDkQE5r0A`, `İ` = `6JztUNsZpP9q8P7zDbGeMs`).

`nd-mapping/src/title_tables.rs::LOWER_SPECIAL` is *not* affected: it models Go's
`Title()` title-casing path, a different function that intentionally matches
Rust-style folding.

**Reviewer follow-up (agent `59273ee5…`, APPROVE; full scalar sweep + Go goldens):**
three more `to_lowercase` callsites produce persisted identity/search/sort values
and are fixed in this worktree before scanner writes are wired:

- `nd-mapping/src/persistent_ids.rs`: PID album-artist/album values and
  `legacy_album_id` are hashed into persisted `pid`/`album_id`; those values now
  use `nd_utils::str::to_lower` (the spec/attribute name itself is config, but
  it uses the same helper). This is the same class/severity as `new_tag_id`.
- `nd-utils/src/str.rs`: `sanitize_field_for_sorting*` produce persisted
  `sort_*`/`order_*` fields; `sanitize_strings` feeds FTS columns; and
  `normalize_for_fts` uses lowercase to dedupe which variants get persisted.
  All now use `to_lower`; tests pin Greek final sigma and U+0130 through the
  persisted sort and sanitization paths.

Other usages are safe to defer: criteria/map keys are normalized on both sides
inside the Rust process and configuration tag names are ASCII; extension/codec
comparisons are likewise ASCII; `nd-playlists` M3U keys are built and looked up
symmetrically in Rust; `nd-auth` username comparison has only a non-ASCII edge
case, not persisted identity. `nd-mapping/title_tables.rs::LOWER_SPECIAL` models
Go `Title()` (not `strings.ToLower`) and remains unchanged.

**Verification lane results (lanes `61acbfa8…`, parent-confirmed):** integer-key
JSON ordering is string-sorted lexicographically ({1,2,10} →
`{"1","10","2"}`), confirming `marshal_discs`; `new_tag_id` matches Go byte-for-byte
on ASCII; S-A participant goldens are **parse**-compared, and Rust reads Go's
escaped bytes (`\u003c \u003e \u0026 \u2028 \u2029`) correctly, so reading a
Go-written DB is safe; `AllNames()` order is genuinely non-reproducible (6
distinct orderings over 300 iterations), which is why `search_participants` sorts
and dedupes; all Rust read decoders accept Go-written album/participant/tag/disc/
folder_ids bytes with no divergence.

Final workspace gate after the PID/sort/FTS lowercasing follow-up and review: fmt
clean, clippy `-D warnings` clean, **1579 passed / 0 failed** across 60 binaries.

## S-D3b: album + mediafile row hydration implemented, review in progress (2026-11-13)

**S-D3b** — `hydrate.rs` gained `row_to_album`/`album_post_scan` and
`row_to_mediafile`/`mediafile_post_scan` (Go's exact statement order and guards:
discs guarded, participants **unguarded**, tags guarded then `ToGenres`,
folder_ids guarded, then the unconditional `rg_*` copies), plus the four Go prefix
consts (`ALBUM_DISCS_PREFIX`, `ALBUM_PREFIX`, `ALBUM_FOLDER_IDS_PREFIX`,
`MEDIAFILE_PREFIX`) and the `join_text` accessor that splits
"column absent" (Go's zero, per `dbx@v1.12.0/rows.go:80-86`) from "column present
and NULL" (Go's scan failure). `album_repo::AlbumRow` / `mediafile_repo::MediaFileRow`
hold the unparsed shim columns and delegate `into_record`; the records gained the
hydration-only fields (`id`, `library_path`, `library_name`, `annotation`,
`bookmark_position` on media_file, `genres`), and `sql_tags` gained `tags_to_genres`
with an nd-db-local `Genre` DTO mirroring `model.Genre`.

**Bugs found by executing the statements, not by a renderer test** (three of them,
all SQL-text defects the renderer tests could not see):
1. `select_media_file` appended the annotation and bookmark LEFT JOINs **before**
   `FROM`, so every logged-in media_file read failed with `near "LEFT": syntax
   error`. squirrel emits the column list, `FROM`, then joins in append order
   (`select.go:92-118`), verified against the pinned builder.
2. `album_repo::count_all_sql`/`get_all_ids_sql` pushed `WHERE` before the
   annotation `LEFT JOIN`, producing the same `near "LEFT"` error for
   filter-plus-annotation counts (`album_repository.go:193-200` applies the library
   filter first, but the builder renders joins first, so the join's args bind
   first). Both now match the mediafile twin.
3. `mediafile_repo::get_sql` appended a **second** ` WHERE media_file.id = ?` onto
   `select_media_file`, which already ends with ` WHERE <library clause>` whenever a
   filter is present — `near "WHERE": syntax error`. Go's `Get` is `GetAll` with
   `Eq{"media_file.id": id}` and squirrel AND-joins accumulated `WhereParts`
   (`select.go:112-117`), so the id predicate now joins the open clause with
   ` AND `, the same rule `exists_sql` already followed.

Each of the three has an executed test that runs the statement against the real
fresh schema (not just string rendering), including a case with BOTH a library
filter and `need_annotation`, an ungranted user whose identical statement returns
zero rows, and the `GetRandom` two-pass pair run end to end (rowid pick under a
filter clause AND the library clause, then the phase-2 hydrate). The pinned SQL
unit tests were updated to the corrected rendering. Auditing for the same two shapes (`get_all_sql`,
`get_random_rowid_sql`, `get_random_hydrate_sql`, `count_by_suffix_sql`, the album
`exists_sql`/`select_album_sql`) found no further double-WHERE or join-vs-WHERE
defect: the rowid pick AND-joins its filter and library clause into one `WHERE`, the
phase-2 hydrate passes `library_filter: None`, and `count_by_suffix_sql` has no
`WHERE` at all.

**Also fixed in this follow-up:** the normalized album play count
(`sql_annotations.go:80-83`) is an expression column, so SQLite gives it REAL
storage and rusqlite's Integer-only `FromSql for i64` aborted the read.
`hydrate.rs::annotation_play_count` now reproduces Go's int64 scan arm —
`asString`'s `%g` spelling (`database/sql/convert.go:497-508,570-571`) then
`strconv.ParseInt`, reusing `criteria_sql::go_fmt_float64` — so the normalized cell for
7 plays over 3 songs hydrates as 2 (its `typeof()` is `real` and its `%g` is `"2"`, verified
against the pinned driver — go-sqlite3 v1.14.52's bundled SQLite, `select sqlite_version()` = **3.53.4** — while an exponent-form spelling
(`1e+06`) returns Go's conversion error naming `play_count`. Truncation was deliberately NOT
adopted: Go surfaces the failure.
`created_at`/`updated_at` write→hydrate round-trips are executed for both entities.
For media_file (and folder/playlist) `put_sql`'s stamp arm means a Rust-written row never
stores NULL, so the NULL read error there is a foreign/legacy-row case only; album's `Put`
mirrors Go and stamps only `imported_at` (`album_repository.go:208-219`), so a Rust album
write whose record carries `created_at: None` DOES store NULL — and Go's non-pointer
`time.Time` fails that same cell, which is why the read errors (asserted in
`tests/runtime_marshal_records.rs:1064-1073` for the INSERT leg and
`tests/runtime_hydrate_album_mediafile.rs` for the read). A cell that scans as Go's ZERO time is
returned as `None` rather than a zero stamp — a record-shape divergence from Go's non-pointer
`time.Time`, documented in `hydrate-contract.md`'s residual list and in
`hydrate.rs::go_zero_or_none`, and consistent with the write path treating `None` as
"never stamped".

**Residuals carried forward** (recorded in `hydrate-contract.md`, none of them
write-path): hydration overwrites caller-populated `participant_names` with sort
names dropped (Go's base `GetAll` never calls `getParticipants`,
`sql_participations.go:61-76`, so Go's hydrated participants carry none either);
participant role order is `BTreeMap`-sorted (~~vs Go's random map order~~ **corrected
in S-D3e**: `json.Marshal` sorts map keys, executed stable across runs, so the column
bytes MATCH Go; only `AllNames`' iteration order is random, and
`sql_feeders.rs::search_participants` sorts and re-dedupes it); annotation
stamps keep the stored zone while record stamps render as `Z` (the folder/playlist
precedent); and ~~no repo-level read API is wired to `row_to_album`/
`row_to_mediafile` yet — tests only~~, which **S-D3c below landed** (repo reads plus
`hydrateArtwork`).

**Write-map invariance holds:** `album_row` stays at 48 keys (45 base + 3 FTS) and
`mediafile_row` at 66 (63 base + 3 FTS), with none of the hydration-only columns
present, and a hydrated record re-renders byte-identical INSERT/UPDATE legs to one
whose derived fields are empty. Executed in
`rust/crates/nd-db/tests/runtime_hydrate_album_mediafile.rs` (29 tests).

Final workspace gate after the S-D3b review follow-up: fmt clean, clippy
`-D warnings` clean, **1665 passed / 0 failed / 1 ignored** across 63 binaries
(`nd-db`: 586), `cargo doc -p nd-db --all-features --no-deps` at the same 23
intra-doc-link warnings as pristine `HEAD` (message-set diff empty), and
`git diff --check` clean.

## S-D3c: artwork hydration landed, album + mediafile repo reads wired (2026-11-13)

**Lane-id note (numbering drift between the two docs).** The parent issued this as
"S-D3c". Its content is the scan contract's sub-slice **S-D3f**
(`hydrate-scan-contract.md:213-214` — artwork `hydrateItems`, "its own sub-slice,
**NOT** in `hydrate.rs`", which this slice's module placement follows) plus the
repo-read wiring S-D3b left open. The two docs number the remaining work
differently: `hydrate-scan-contract.md:198-199` calls album + mediafile hydration
S-D3c (what the slice table above calls S-D3b), and what the slice table above calls
"S-D3c — artist/playlist/track/scrobble" is that contract's **S-D3d** (`:203-206`)
and **S-D3e** (`:208-211`). None of that is touched here and it stays open; neither
table was renumbered.

**Landed** (single writer lane, uncommitted at `6cd6c8d2`):

- `nd-db/src/artwork_hydration.rs` (NEW, 465L) — the port of
  `persistence/artwork_hydration.go` (176L): `ItemImage` (`model/artwork.go:29-40`),
  `ItemArtworkInfo` (`:70-78`) with `absent()` (`:81`) and `image()` (`:84-93`),
  `hydrate_item_images` (`:58-68`), `apply_item_image` (`:71-75`), `hydrate_items`
  (`:79-90`), `hydrate_album_artwork` (`album_repository.go:257-260`) and
  `hydrate_media_file_artwork` (`:94-135`) with the branch matrix reproduced
  statement-for-statement including every `continue`. It is a separate module, NOT
  part of `hydrate.rs`, exactly as `hydrate-scan-contract.md:54` and `:213-214`
  require (the eight PostScan hooks are pure-column and none of them queries).
- `ReadOutcome<T> { items, artwork_error }` — Go swallows a failed artwork lookup,
  logs `log.Error` and returns an empty map so the page still renders
  (`artwork_hydration.go:62-66`). nd-db has no logging dependency
  (`sqlrepo.rs:13-17`), so the port swallows the same failure and EXPOSES the text
  (the `ShareRepo::load` precedent, `share_repo.rs:436-472`).
- `AlbumRepo<'a>` (`album_repo.rs:1440`) and `MediaFileRepo<'a>`
  (`mediafile_repo.rs:527`) over `&rusqlite::Connection`, the `UserRepo`/`PlayerRepo`
  DI shape: a two-variant `thiserror` enum (`Sqlite(#[from] rusqlite::Error)` +
  `NotFound` = "record not found"), execution only through `crate::execution`, and
  hydration through `row_to_album` → `AlbumRow::into_record` /
  `row_to_mediafile` → `MediaFileRow::into_record` with
  `HydrateError::into_sqlite` as the exit shape. Wired reads: album `Get`/`GetAll`
  (`album_repository.go:234-243`, `:245-256`); media_file `Get`/`GetAll`/`GetRandom`
  (`:212-221`, `:232-242`, `:250-283`, both passes, with phase 1 read as bare `i64`
  rowids and never handed to a hydrator, Go's library filter threaded into BOTH
  passes (`selectMediaFile` applies `applyLibraryFilter` unconditionally at
  `mediafile_repository.go:209`, so phase 2's row set is {rowids} ∩ {visible
  libraries}), and Go's phase-2 `ORDER BY random()` re-shuffle kept as the builder
  emits it). No new SQL: every read reuses the existing builders.
- Kind-value defect FIXED. `artwork_repo::OWNER_TABLES` (was `:38-44` at `HEAD`, now
  `:55-61`) keyed `item_kind`
  by the kind NAME (`"album"`, `"media_file"`, …) and its unit tests pinned that, so
  the statement matched nothing. The column holds `Kind.Prefix()`: the comment at
  `model/artwork_id.go:22` defines the prefix as that column's token, the kinds are
  `Kind{"al","album"}` … (`:30-35`), `purgeDangling` binds `kind.Prefix()`
  (`persistence/artwork_repository.go:96`), `GetInfoForItems` binds it (`:169`,
  `Eq{"ia.item_kind": kind.Prefix()}` in the `Where(And{…})` at `:168-172`), and
  every writer binds it too (`core/artwork/artwork.go:233`, `:346`;
  `core/artwork/housekeeping.go:168`;
  `persistence/artwork_queue_repository.go:38`, `:66-67`, `:108`, `:133`, `:201`). Keys
  are now nd-id's `ARTWORK_KIND_*_PREFIX` constants over the same entity-table values
  (Go's map is kind→table at `:83-89`), with the fixtures in `artwork_repo.rs` and
  `artwork_queue_repo.rs` moved to prefixes and two new assertions that the emitted
  `item_kind` BIND VALUE is the prefix. `runtime_timestamp_convention.rs` already
  used `"ra"` and was not touched; `radio_repo.rs:54`/`playlist_repo.rs:62` were the
  existing correct precedent. `artwork_hydration.rs` also carries three unit tests for
  `absent()`/`image()`/the missing-entry zero state.
- Hydration-only record fields: `AlbumRecord.image`, `MediaFileRecord.image` +
  `.album_image` (Go embeds `ItemImage` at `model/album.go:17`, `model/mediafile.go:30`
  and adds `AlbumImage` at `:34`). Both Go types are `structs:"-"`, so they are
  read-path state only and `album_row`/`mediafile_row` still emit 48 and 66 keys —
  re-asserted for a record that came back through the repo read, not just a
  hand-built one.

**Executed tests** (all against `nd_dbschema::schema::apply_fresh_schema`, not string
rendering): `tests/runtime_artwork_hydration.rs` (NEW, 14) covers found /
known-absent / unresolved album states, `album_image` populated on EVERY mediafile
branch including the own-art-wins `continue`, the inherit / not-eligible /
album-row-absent stays unresolved cases, a multi-disc track never inheriting a found
album hash and staying requestable when the album is absent, eligible-but-unresolved
staying optimistic and keeping a bare hash even when the album art is found, the
`enable_media_file_cover_art = false` fallback, blur/thumb/dominant-color/width/height
surviving the join through BOTH the own-art and inherited branches, `item_kind` as the
prefix (seeding `"album"`/`"media_file"` finds NOTHING), the 200-id chunk boundary
across two chunks, and a dropped `item_artwork` table leaving the read SUCCESSFUL with
`artwork_error` set and every item zero-valued — plus a 12-row branch-matrix table
whose expectations are read off Go's line numbers and whose album state rows are
derived from that same table so data and expectations cannot drift. Six repo-read
tests were added to `tests/runtime_hydrate_album_mediafile.rs` (23 → 29):
`AlbumRepo::get`/`get_all` and `MediaFileRepo::get`/`get_all`/`get_random` executing
through the seam, each row keeping its own id, a zero-row `Get` returning `NotFound`
while `GetAll` returns an EMPTY `Vec`, logged-in annotation/bookmark columns hydrating,
an ungranted user's library filter returning zero rows, hydrated records
re-rendering byte-identical write maps, and `GetRandom` phase 2 excluding a rowid from
an ungranted library. `artwork_repo.rs` gained two unit tests (6 → 8),
`artwork_hydration.rs` three (`absent()`/`image()`/the missing-entry zero state) and
`mediafile_repo.rs` one phase-2 rendering test, so 26 new tests in total:
14 + 6 + 2 + 3 + 1.

**Gate after S-D3c and its review follow-up** (from `rust/`, all actually run):
`cargo fmt --all -- --check` 0;
`cargo clippy --all-targets --all-features -- -D warnings` 0; `cargo test --all-targets
--all-features` 0 — **1691 passed / 0 failed / 1 ignored across 64 binaries**
(baseline 1665/0/1 over 63; `nd-db` 586 → **612** across its 21 `--all-targets`
binaries, the +1 workspace binary being the new test file; the `nd-db` lib doctest target
contributes 0 tests on both sides);
`cargo doc -p nd-db --all-features --no-deps` still 23 intra-doc-link warnings with an
empty message-set diff vs pristine `HEAD` (one new link I added was fixed to keep that
at parity); `git diff --check` 0.

**Residuals recorded** in `hydrate-contract.md`'s S-D3c list: the `optimizePagination`
asymmetry (album `GetAll` passes NO options to `queryAll`, `album_repository.go:248`,
so it never takes the offset rewrite, while media_file `GetAll` forwards them,
`mediafile_repository.go:235`, so it does past `DevOffsetOptimize` = 50000,
`conf/configuration.go:1129`) — neither Rust read applies it; repo-level `get_all` being
the order-by/pagination subset with no caller `QueryOptions.Filters` (Go applies
`options[0].Filters` via `newSelect`→`applyFilters`, `sql_base_repository.go:139-146`,
`:247-252`, so `GetAllByTags` at `mediafile_repository.go:284-307` is one Go call the
repo API cannot express; filtered reads take the `select_*_sql(where_clauses)` seam);
Go's `Get` returning `nil` beside the error on both paths (`album_repository.go:236-238`,
`:239-241`; `mediafile_repository.go:214-216`, `:217-219`), so `Result` is NOT a
divergence — an earlier revision of this list claimed a non-nil pointer, corrected
against the pinned sources; `Get` discarding `artwork_error` because Go's
`Get` swallows it by delegating; a failed lookup discarding earlier chunks; the flag
read once in Rust vs twice in Go (`:102` and `:111`); `i32` widths; the prefix spelling
contract for future callers; `artworkChunkSize` = 600 not exposed (streaming lane);
Search results NOT artwork-hydrated because the Rust search projection from
`sql_search::new_select` (`sql_search.rs:129-131`) emits no
`library_path`/`library_name`/annotation columns and is therefore not hydratable
through this seam at all; cursor/streaming reads (`streamByIDs`, `chunkOptions`,
`hydrateCursor`, `hydratePlaylistTrackArtwork`) unwired with `cursor_chunk_sql` left
for that lane (`hydratePlaylistTrackArtwork` since LANDED in S-D3e over the media-file wrapper;
`streamByIDs`/`hydrateCursor` still unported); `getParticipants`/`GetWithParticipants` unported; and the explicit list
of Go reads that do NOT hydrate (`GetTouchedAlbums`, media_file `GetCursor` — asserted
by `persistence/artwork_hydration_test.go:633` — `GetMissingAndMatching`,
`FindByPaths`, `FindRecentFilesBy*`).

**Construction sites outside the brief's file list:** `tests/runtime_insert_alignment.rs`
and `tests/runtime_marshal_records.rs` each gained the new hydration-only fields in
their hand-built `AlbumRecord`/`MediaFileRecord` literals (required to compile), with a
comment saying the write legs take the id/hydration state from elsewhere. No other file
outside the brief's ownership list was touched.

**Not changed:** nd-model, nd-stream, nd-artwork, nd-conf, nd-dbschema, every
Cargo.toml/lock, generated files, Go sources, and every previously documented
limitation.

## S-D3c review follow-up: GetRandom phase 2 library filter, doc corrections (2026-11-16)

Reviewer-selected fix round on the uncommitted S-D3c change, single writer, no new files.

- **Behavioral (the only code change): `get_random_hydrate_sql` takes `library_filter`**
  and `MediaFileRepo::get_random` threads the caller's clause into BOTH passes. Go's
  phase 2 (`mediafile_repository.go:274`) runs through `selectMediaFile`, whose last
  statement is an unconditional `applyLibraryFilter` (`:209`), so a hardcoded
  `library_filter: None` was a real gap: a rowid from a library the user cannot see
  could be hydrated whenever phase 1's `opt.Filters` did not exclude it. Rendering
  follows `get_sql`'s rule — the filter owns the first `WHERE` and the rowid IN-list
  AND-joins it (`select.go:111-114`), args in clause order. Call sites updated: the
  repo (`mediafile_repo.rs`), its in-file unit test, and the phase-2 assertions in
  `tests/runtime_hydrate_album_mediafile.rs`.
- **Doc corrections.** The `Get`-returns-a-non-nil-pointer claim was FALSE: both
  repositories return `nil` on both error paths (`album_repository.go:236-238`,
  `:239-241`; `mediafile_repository.go:214-216`, `:217-219`), so `Result` is not a
  divergence — fixed in all four places (both repo `get` docs, this file's residual
  list, `hydrate-contract.md`). The `item_artwork.item_kind` bind citation moved from
  `artwork_repository.go:171` to `:169` (the actual `Eq{"ia.item_kind": kind.Prefix()}`
  line; `:171` is the `item_id` chunk). The garbled cross-doc citation in
  `artwork_hydration.rs` now names `docs/rust-port/next-work.md:775-779` as the
  write-map-invariance pin (verified with `sed -n`). The mediafile branch-matrix
  comment that denied the `eligible` guard now states that the guard IS present
  (`artwork_hydration.rs`, matching Go's compound condition at
  `artwork_hydration.go:112-116`).
- **New scope notes** on both repo module docs and in `hydrate-contract.md`:
  repo-level `get_all` is the order-by/pagination subset with no caller
  `QueryOptions.Filters`; filtered reads take the `select_*_sql(where_clauses)` seam
  until the REST/options lane wires them. Citations corrected to current line numbers
  where this round touched them (`get_sql`'s clause-order note is `mediafile_repo.rs:371-388`,
  `applyOptions` is `sql_base_repository.go:149-162`).
- **Tests added (4):** `mediafile_repo::get_random_phase_two_reapplies_the_library_filter`
  (rendering + args), `runtime_hydrate_album_mediafile::mediafile_repo_get_random_phase_two_reapplies_the_library_filter`
  (executed two-library read plus the unfiltered control), and
  `runtime_artwork_hydration::get_random_succeeds_with_the_artwork_table_gone` /
  `::duplicate_ids_keep_their_bindings_and_hydrate_identically`. The mediafile
  write-map forbidden-key list gained `blur_hash`, `thumb_hash`, `dominant_color`,
  `image_width`, `image_height`.

**Gate after this round** (all actually run): fmt 0, clippy `-D warnings` 0, test 0 with
**1691 passed / 0 failed / 1 ignored across 64 binaries** (`nd-db` **612** across its 21
`--all-targets` binaries), `cargo doc -p nd-db --all-features --no-deps` at the same 23
intra-doc-link warnings with an empty message-set diff, `git diff --check` 0.

**Round-2 review (a third model, fresh and read-only): APPROVE — no P1/P2.** It
independently re-verified F1-F7 as landed and complete, confirmed every previously
documented divergence is still accurately documented rather than silently changed, and
re-measured the gates to the same numbers. It raised two P3s and the parent found a
third, all applied directly (comment/test-only, no behavioral change):
- `get_all_sql`'s doc cited `mediafile_repository.go:221-227` for Go's `GetAll`; the
  real location is `:232-242` (`:221-227` is the `Get`/`GetWithParticipants` range). The
  drift predates this slice — it is identical at `HEAD` — and belongs to the separate
  repo-wide citation sweep already recorded in `hydrate-contract.md`; corrected here
  because this round touched the same file.
- The new unit test's comment cited `selectMediaFile(ctx)` at `:273`; the phase-2 call is
  `:274`.
- The two-library control now passes `user_id: Some("u-1")`, identical to the filtered
  read above it, so the library clause is the only difference between the two
  statements. The reviewer judged the original `None` form a valid control (the
  annotation/bookmark joins are LEFT JOINs and cannot exclude a row), so this is
  precision rather than a correctness fix.

**Reviewer's gate note, verified by the parent:** the first full-suite run of that
review reported one failure — `nd-stream`
`transcode_body::tests::early_release_defeats_the_cap_and_accumulates_processes`, a
timing-sensitive multi-thread test that counts live `sleep` stub PIDs via `ps`. `git
status` shows `rust/crates/nd-stream` clean, the parent ran it in isolation 3× → 3
passed, and both parent full-suite runs reported 0 failed. Environmental flakiness in an
untouched crate, not a regression from this slice; worth decoupling from the workspace
gate in a later lane.

## S-D3d: artist stats fold, similar-artists decode, artist read record + repo reads (2026-11-17)

**Lane name vs the two numbering schemes.** The parent issued this as "S-D3d"
(`hydrate-scan-contract.md:203-206`, the artist sub-slice). In the slice table above it
is the ARTIST half of the row **S-D3c artist/playlist/track/scrobble**, which is now
marked PARTIAL: the artist part landed, playlist_track/scrobble_buffer and the
playlist/radio artwork wrappers did not. The parent's file list for this lane was
`hydrate.rs`, `artist_repo.rs`, `artwork_hydration.rs`, the new
`tests/runtime_hydrate_artist.rs`, and these two docs; nothing else was touched and
nothing was committed.

**Landed** (single writer, no new crate, no dependency change):

- `nd-db/src/hydrate.rs` (+400L) — `ARTIST_STATS_PREFIX` = `"parsing artist stats from db: "`
  and `ARTIST_SIMILAR_PREFIX` = `"parsing similar artists from db: "`, copied from
  `fmt.Errorf("…: %w", err)` at `artist_repository.go:49` / `:89` including the trailing
  space. `row_to_artist` (phase 1, the `dbArtist` columns `:32-36`) and `artist_post_scan`
  (phase 2, `PostScan` `:43-98`) statement by statement: the stats map created
  UNCONDITIONALLY before the `!= ""` guard (`:44`, `:46`); the nested
  `map<library, map<role, map<string,int64>>` decode with Go's zero-fill for a missing key
  and for `null` at every nesting level (executed Go oracle: `"null"`, `"{"1":null}"`,
  `"{"1":{}}"`, missing `a`/`s` all decode with `err == nil`); the `key == "total"` arm
  ACCUMULATING with `+=` onto the artist's own `size`/`song_count`/`album_count` (`:63-67`)
  and then being dropped by the invalid-role `continue` (`:69-72`) so it never becomes a
  stats key; unknown roles contributing NOTHING (the `+=` block is gated on `"total"`, not
  on the role parsing) with the `invalid role:` text discarded exactly as Go's bare
  `continue` discards it; per-role `+=` merge (`:74-78`); then the similar-artists decode
  with its guard (`:83-90`). Reuses the existing `row_to_annotation`, `join_text`
  absent-column tolerance and `sql_participations::Role::from_str` rather than inventing
  new plumbing; `narrow_i32_go` accumulates the stats value with `wrapping_add` and
  CLAMPS it to the `i32` field's bounds. It does NOT reproduce Go's conversion: Go's
  `int` is 64-bit (executed `int bits=64`), so `int(stat["m"])` narrows nothing and the
  port's `i32` fields are a deliberate port-wide narrowing Go does not perform — see the
  S-D3d residual list.
- `nd-db/src/artist_repo.rs` (+306L) — `ArtistRecord` gained the seven hydration-only
  read fields (`stats`, `size`, `album_count`, `song_count`, `annotation`, `image`,
  `similar`), none of them in `put_cols`; `ArtistRow::into_record` delegating to
  `artist_post_scan` (the `album_repo.rs:962` shape); `ArtistError { Sqlite, NotFound }`
  with `"record not found"`; `ArtistRepo<'a>` with `get`/`get_all` plus private
  `read_hydrated`/`hydrate_artwork` over `crate::execution::query_all`, mirroring
  `album_repo.rs:1448-1557`. Module doc updated: the PostScan fold and artwork hydration
  are no longer listed as unported, and what stays unported keeps its reason.
- `nd-db/src/artwork_hydration.rs` (+30L) — `hydrate_artist_artwork` over the existing
  `hydrate_items` with `nd_id::ARTWORK_KIND_ARTIST_PREFIX` (`"ar"`, `artwork.rs:30-35`;
  Go binds the PREFIX at `artwork_repository.go:169`, `model/artwork_id.go:29`).
- `nd-db/tests/runtime_hydrate_artist.rs` (NEW, 2502L, 31 `#[test]` functions — 53 `fn` occurrences counting helpers) — the read, artwork and write tests execute against
  `nd_dbschema::schema::apply_fresh_schema` on a real on-disk pooled DB. The earlier "never against
  string-rendered SQL" claim was overstated: `get_all_sql_ignores_the_sort_leg_until_options_are_wired` asserts on the RENDERED statement text for the sort/filter/pagination legs ("no ORDER BY", `ends_with(" GROUP BY artist.id LIMIT 10 OFFSET 5")`, `WHERE` presence) and on a deliberately-broken stats-backed sort executed against the fresh schema, while the guard/decode arms assert on the `artist_post_scan` seam directly. Coverage: multi-library fold with a `total` key; `total` never a
  stats key; unknown role contributes nothing; pre-seeded totals accumulate; the total-arm
  and per-role WRAPPING accumulation (no panic at `i64::MAX`, with the `i32`-clamping
  interplay pinned next to Go's executed value); the guard and
  decode arms (`""`, `null`, `{}`, null library entry, null role entry, and the rejection
  arms with Go's exact prefix); missing `a`/`s` and null stats zero-fill; the four
  similar-artists empty forms plus `[null,{…}]` and unknown JSON fields; the similar
  decode's FIELD-level Go error sentences (`.0.id`, `.0.name`, `.0.ID`, `.1.id`) plus the
  error-free null/unknown-field arms; the similar
  failure carrying its prefix with the stats fold proven to have run first; annotation with
  and without a row plus `annotation_user = None`; `GROUP BY` one row per artist; `Get`
  NotFound for unknown id / ungranted non-admin / no junction row; `get_all` returning an
  empty Vec; the unrendered ORDER BY leg (`get_all_sql` with a non-empty sort emits no
  `ORDER BY`, and the dropped `artist.stats` column makes the stats-backed sort keys fail
  on the fresh schema); the three artwork states under the `ar` prefix; the artwork-failure channel
  (`get_all` reports it, `get` discards it); write-map key-count invariance with the count
  DERIVED (`put_cols` 18 keys with sort+mbz set, 16 with both empty, minus `created_at` in
  the UPDATE leg) and byte-identical UPDATE/INSERT text and args for a hydrated record vs a
  hydration-reset clone; raw `similar_artists` surviving a read-then-write cycle; the
  `Put` stamping contract after hydration; the INNER-vs-LEFT join wording pin; and the
  NULL-alias required-column error.

**Fix round after review (2026-11-17), two behavioral corrections.** (1) The fold used a
plain `+=`, which PANICS where Go wraps: overflow checks are on in this workspace
(`rust/Cargo.toml`'s `[profile.dev]`/`[profile.test]` set only `debug`/`incremental`),
executed probe → `attempt to add with overflow` at `i64::MAX += 1`, while Go wraps
(`2147483647 += 2147483647` → `4294967294`). All six accumulation sites now add through
`hydrate.rs::wrapping_int64` / the `wrapping_add` inside `narrow_i32_go`.
(2) A non-string `id`/`name` in a similar-artists element fell through to serde's text
(`invalid type: integer \`3\`, expected a string`); the decoder now reads the two fields
field-by-field and emits Go's field-level sentence verbatim, including the `.0.id` path and
the `string` target (executed oracle). See the S-D3d residual list for the two residuals
that remain in that decoder.

**Claims in the parent's brief that were wrong at source, corrected against the pinned
files.** (1) `dbArtist` is `artist_repository.go:32-36` and `dbSimilarArtist` is `:38-41`,
not `:34-36`/`:38-41` as cited; `Get`/`GetAll` are `:255-267`/`:269-279`, not `:263-273`/
`:297-299`; `hydrateArtwork` is its OWN method at `:293-297` (comment `:293`, signature
`:294`, body `:295-296`) — the brief cited that range under `Get`, whose only connection is
that `Get` CALLS it at `:265` (and `GetAll` at `:277`); `PostMapArgs`
is `:100-121`, not `:98-105`; `marshal_similar_artists`' Go counterpart is `:101-106`, not
`:502` (`RefreshStats` is `:465`, its `batchUpdateStatsSQL` string starts at `:494` and its
`WITH artist_role_counters AS (` CTE is `:495`). (2) `squirrel` renders `JOIN` for `.Join` at
`select.go:298-299` — confirmed, and `InnerJoin` at `:313-314` is what emits `INNER JOIN`.
(3) The brief's `put_cols` figure of 18/16 keys is right, but the UPDATE arg count is 18
(17 SET + `WHERE id`), not 19: the function is `selectUpdateColumns` (there is no
`select_update_cols` in the tree), `sql_base_repository.go:593-610`, and
`delete(updateValues, "created_at")` is `:605`. (4) `RowGet::get_opt_text` is NOT what reads the stats
alias — `row_to_artist` uses the established `join_text` helper (`hydrate.rs:2314`), which
maps an ABSENT column to `""` and a present NULL to an error.
(5) A reviewer claim that the annotation `LEFT JOIN` placement diverges from squirrel is
WRONG and was rejected: squirrel renders ALL joins as one space-joined block BEFORE `WHERE`
(`select.go:103-109`), and Go's `withAnnotation` appends to that same `Joins` list even
though it is called after `GroupBy` (`artist_repository.go:212-214`,
`sql_annotations.go:68-72`). The port matches; no code or test changed.

**Two divergences the brief predicted differently than the executed builds behaved**, both
recorded rather than papered over: a NULL `library_artist.stats` cell does NOT fold to the
empty string on the bundled build (`json_group_object(1, jsonb(null))` → `{"1":null}` on
SQLite 3.46.0, executed) so it decodes as a zero-contributing entry, and a NULL `library_id`
KEY renders as the unquoted `{:{}}` on 3.46.0 — which Go's JSON decoder rejects — while
3.53.4 drops the entry entirely; both are unreachable under `schema_ddl.rs:62`'s
`library_id NOT NULL`. Likewise a null ROLE value still CREATES a zero-valued stats entry,
which the brief did not state.

**Gate after the fix round** (from `rust/`, all actually run): `cargo fmt --all -- --check`
0; `cargo clippy --all-targets --all-features -- -D warnings` 0; `cargo test --all-targets
--all-features` 0 — **1719 passed / 0 failed / 1 ignored across 65 binaries**; `nd-db`
**640** across its 22 `--all-targets` binaries; `cargo test -p nd-db --test
runtime_hydrate_artist` 0 — 28 passed / 0 failed; `cargo doc -p nd-db --all-features
--no-deps` at the SAME 23 intra-doc-link warnings (9 unresolved-link + 14 private-item,
including the PRE-EXISTING `select_artist_parts` link at `artist_repo.rs:4`) with no new
warning; `git diff --check` 0. No test was lost; the known `nd-stream` timing flake did not
fire in this run. (Earlier in this lane, before the fix round: 1715 passed / `nd-db` 636,
which the +4 tests replaced.)

**Gate after fix round 2** (from `rust/`, all actually run): `cargo fmt --all -- --check` 0;
`cargo clippy --all-targets --all-features -- -D warnings` 0; `cargo test --all-targets
--all-features` 0 — **1720 passed / 0 failed / 1 ignored across 65 binaries**; `nd-db`
**641** across its 22 `--all-targets` binaries; `cargo test -p nd-db --test
runtime_hydrate_artist` 0 — **29 passed / 0 failed** (the 28 above, plus
`digit_path_tokens_drop_the_go_struct_field_phrase`); `cargo doc -p nd-db --all-features
--no-deps` 0 with the SAME 23 intra-doc-link warnings (9 unresolved-link + 14 private-item,
including the PRE-EXISTING `select_artist_parts` link) — no new warning from `go_into_what`,
whose intra-doc links resolve; `git diff --check` 0. The known `nd-stream`
`transcode_body::tests::early_release_defeats_the_cap_and_accumulates_processes` flake did
not fire in this run (it passed).

**Fix round 2 after review (2026-11-17), the `Go struct field ` phrase.** The stats decoder
hard-coded Go's `intoWhat` choice per arm — the library-level arm always DROPPED the phrase,
the role and stat-key arms always KEPT it — which diverged from Go in two symmetric
directions, both executed against Go 1.27.4: `{"alpha":5}` keeps it
(`json: cannot unmarshal number into Go struct field .alpha of type
map[string]map[string]int64`) while the port dropped it; `{"1":{"2":5}}` drops it
(`json: cannot unmarshal number into .1.2 of type map[string]int64`) and
`{"1":{"albumartist":{"5":true}}}` drops it (`json: cannot unmarshal bool into
.1.albumartist.5 of type int64`) while the port kept both. Go's rule is POSITIONAL, not
type-driven: `encoding/json/v2_decode.go:143-147` drops the phrase when the token after the
LAST `.` of `UnmarshalTypeError.Field` is non-empty and all ASCII digits, so the same arm
reads both ways depending on the key's spelling, and `{"01":5}` counts as all-digits
(`TrimRight` strips a cutset, it does not parse a number). `hydrate.rs::go_into_what` now
computes it from the last path token and is applied in all FOUR `decode_library_stats` arms;
`go_type_error`'s signature and the root-level `Go value of type` arm are unchanged (Go's
root case carries no field, `v2_decode.go:149-151`). The similar-artists decoder was left
alone on purpose: Go ignores an unrecognized key there (`[{"5":3}]` → `err == nil`), so no
digit token can be the last token of one of its field sentences, and its element arm already
passes `into_what = ""` for the bare `.0` index, which is what Go's rule produces. Pinned by
the new test plus five additional rows in `stats_guard_and_decode_arms_match_go`'s malformed
table, every expectation a Go sentence executed against
`map[string]map[string]map[string]int64` rather than port output. What is now byte-exact is
the sentence SHAPE; the remaining value-level divergence is only the number LITERAL
(serde renders its parsed `Number`, Go echoes the source text), scoped with executed
evidence in `hydrate-contract.md`'s S-D3d list: `1.0`, `1.5`, `12345678901234567890`,
`9223372036854775808` and `-0.0` agree, exponent spellings (`1e3`/`1E3`/`-1e3`/`1e9`) and
integers above `u64::MAX` (`-9223372036854775809`, `18446744073709551616`) do not. Found
while pinning those literals: a BEHAVIORAL divergence the review had not flagged — Go
accepts the integer spelling `-0` into int64 (executed: `err == nil`, value 0), but
serde_json stores `-0` as a float so `as_i64` is `None` (executed probe) and the port
rejects it; recorded as its own residual bullet and pinned in the new test.

**Stale citations corrected at source in `artist_repo.rs`** (three comment lines only, each
verified with `cat -n` against `persistence/artist_repository.go`): `TOTAL_ROLE` cited
`sanitizeArtistStatsRole :83-84` -> real `:175-183` (`:83-84` is `PostScan`'s
similar-artists null guard); `role_filter` cited `roleFilter :71-77` -> real `:163-170`;
`marshal_similar_artists` cited `:31-34`/`PostMapArgs :102-107` -> `dbSimilarArtist` is
`:38-41` and the marshal statement is `:101-106`. The `similar_artists` field doc's
`PostMapArgs :102-107` was left as-is: that citation names the whole `PostMapArgs` derived-
column block, which really is `:102-107` (`m["similar_artists"] = string(similarArtists)` is
`:106`), so it was never wrong. MORE stale citations for the same symbols remain in the tree
and were NOT touched, because they sit outside the lines this lane was scoped to: module doc
`:56` and the `sanitize_artist_stats_role` fn doc (`:580`) cite `:79-88` (real `:175-183`);
module doc `:58` and the test comment `:1706` cite `roleFilter :71-77` (real `:163-170`);
test comments `:1683` and `:1953` repeat `:79-88` and `:31-34`/`:102-107`; and `FALSE_EXPR`
cites `:76`, `:706` while `Eq{"1": 2}` is actually at `:169` and `:642`. Flagged for the next
lane rather than silently widened.

**Fix round 3 (2026-11-17), citation hygiene — round 2's "three stale cites" claim was FALSE.** Round 2's "Stale citations corrected" block above corrected three comment lines and then asserted the rest were either never-wrong or out-of-scope, implying three was the complete set. That was wrong. A mechanical audit of the lane's files found **25 mismatched func-ranged comment citations — 22 pre-existing at HEAD, 3 introduced by this lane's own edits** — not three. (My own independent re-audit, which additionally covers the no-paren `//!` module-header idiom and cross-file function-body cites that a paren-only `` `Name` (`:a-b`) `` regex misses, flagged a further 22 comment lines in `artist_repo.rs`; round 3 rewrote **47 comment lines** (its own count) — all 25 from the audit plus those 22 — and the file-aware re-audit afterwards reported 0 non-OK func-range cites. The cumulative lane diff now carries 57 changed comment lines in `artist_repo.rs` (rounds 1-4 combined), so 47 is round 3's own figure, not a number reproducible from HEAD.) The audit's method is reproducible: parse `func (recv) Name(` boundaries in each pinned Go source (`persistence/artist_repository.go`, `sql_base_repository.go`, `sql_participations.go`, `artwork_hydration.go`, `sql_annotations.go`, `sql_search.go`, `artwork_repository.go`, `mediafile_repository.go`) to get each function's true body (start = the `func` line, end = the matching closing `}`), then compare every `` `Name` (`:a-b`) `` / `` `Name` `:a-b` `` citation in `///`/`//!`/`//` comments against that body and flag any cite whose range is not contained in the named function's real range; every corrected range's end was confirmed as a real closing brace via `cat -n`. The three round-2 fixes (`sanitizeArtistStatsRole :79-88`→`:175-183`, `roleFilter :71-77`→`:163-170`, `marshal_similar_artists :31-34`→`:38-41`) were themselves part of the 25, not the only ones: the audit additionally found the same symbols cited stale in the module header (e.g. `CountAll :217-227`→`:217-226`, `getIndexKey :310-325`→`:310-322`, `markOrphansMissing :408-419`→`:410-418`, `markMissing :421-439`→`:421-437`, `RefreshPlayCounts :441-463`→`:441-460`, `searchCfg :616-630`→`:618-633`, `artistLibraryFilter :632-649`→`:640-650`, `Search :651-665`→`:652-671`, `searchScope :667-700`→`:676-696`, `requestedLibraryIDs :702-710`→`:700-707`, `isLibraryIDFilter :712-720`→`:711-718`, `sortMappingsForRole :704-716`→`:730-744`) and cross-file body cites in other sources (e.g. `selectUpdateColumns :587-604`→`sql_base_repository.go:593-610`, `filtersNeedAnnotation`→`sql_annotations.go:55-61`, `getSearchStrategy`→`sql_search.go:41-49`, `hydrateRowidPage`→`sql_search.go:117-130`, `chunkOptions`→`artwork_hydration.go:41-54`, `put`→`sql_base_repository.go:618-642`). Round-2's own edits had introduced two of the 25 (`GetAll :269-282`→`:269-279` at `:80`, `queryAll` call-site `:272-279` at `:1613`); the remaining 22 of the 25 pre-dated this lane.
**Gate after fix round 3** (from `rust/`, all actually run): `cargo fmt --all -- --check` 0; `cargo clippy --all-targets --all-features -- -D warnings` 0; `cargo test --all-targets --all-features` 0 — **1720 passed / 0 failed / 1 ignored across 65 binaries** (identical to round 2 — comment-only edits cannot change a test outcome); `cargo doc -p nd-db --all-features --no-deps` 0 with the SAME 23 intra-doc-link warnings (9 unresolved-link + 14 private-item); `git diff --check` 0. The known `nd-stream` `transcode_body::tests::early_release_defeats_the_cap_and_accumulates_processes` flake passed (did not fire). Comment-only change confirmed: `git diff` shows only `//`/`///`/`//!` lines altered in `artist_repo.rs` (and one line in `runtime_hydrate_artist.rs`); no code line, no Cargo.toml/Cargo.lock, no generated file, no Go source touched.
**Residuals recorded** in `hydrate-contract.md`'s S-D3d list (28 bullets plus three
sub-bullets on the number-literal split, each with a citation and, where behavioral, the test
that executed it): stats-prefix parity but not PARSE-error wording, with the value-level
sentence SHAPE now byte-exact once Go's positional digit-token rule is applied and the
number LITERAL as the only remaining value-level divergence — a claim round 4 found
FALSE (duplicate-key ACCEPT, error ORDERING, and JSON-Pointer escaping all diverge
inside a value-level sentence; see the round-4 section below and the narrowed
hydrate-contract.md bullet); similar-artists FIELD-level sentences reproduced BYTE-EXACTLY (executed oracle: the cell `[{"id":3}]` yields exactly
`json: cannot unmarshal number into Go struct field .0.id of type string`, and the
null/unknown-field/`{}`/`[null,{…}]` arms stay error-free),
with two residuals inside that decoder — element/root sentences name the port's own type
name, and when BOTH fields of one element are wrong (or two keys case-fold to the same
field) Go resolves in SOURCE order while the port walks serde_json's `BTreeMap`; the RAW-`similar_artists` round-trip direction (the port preserves unknown
fields Go would drop, and a caller that edits `similar` must re-marshal via
`marshal_similar_artists`); the `i32` narrowing being a port choice Go does NOT make
(`int bits=64`, so `int(stat["m"])` narrows nothing) and accumulation going through
`wrapping_add` so an overflow WRAPS instead of panicking — with `narrow_i32_go`
(clamp-on-overflow) and `hydrate.rs::narrow_i32` (`IntegralValueOutOfRange`) diverging by
design; the discarded
`invalid role:` text; `"total"` never a stats key and accumulation semantics; the
unconditionally-created map; stats-before-similar ordering (port returns `Err` instead of
reproducing Go's partial mutation); the executed 3.46.0-vs-3.53.4 NULL-key divergence;
NULL-vs-`''` cell behavior (`''` is a hard `malformed JSON` read failure on BOTH builds, so
no Rust pre-filter was added); NULL alias error vs absent alias tolerance; artist reads
never taking `optimizePagination`, `get_all` forwarding `QueryOptions.Filters` (which makes
the S-D3c "repo `get_all` cannot express filters" residual entity-specific, not
port-wide) but NOT rendering the ORDER BY leg at all — `QueryOptions.sort`/`order` are
silently ignored, rows come back in SQLite's order, and the stats-backed sort mappings
reference the `artist.stats` column that
`db/migrations/20250701010108_add_multi_library_support.go:59` dropped and that the fresh
schema's artist DDL does not have (so those keys fail upstream too); `NotFound` coming from `len == 0` with the dbx proof that `All` never yields
`sql.ErrNoRows`; `Get` hydrating the whole 0-or-1 slice and discarding the artwork failure;
`annotation_user = None` needing no extra projection column because `artist.*` already
carries `average_rating` (the 19 columns enumerated from `schema_ddl.rs:48`); dropped
`full_text`/`search_normalized`; `JOIN` vs `INNER JOIN` wording; `GetCursor`/`GetIndex`
inheriting hydration transitively while remaining statement-only builders; and the unported
playlist/radio/playlist-track artwork wrappers plus `getParticipants`/
`GetWithParticipants`. **[S-D3e update, 2026-11-17: `hydratePlaylistTrackArtwork` LANDED as
`playlist_track_repo::hydrate_track_artwork`, so the playlist-track wrapper is no longer unported;
`getParticipants`/`GetWithParticipants` and the playlist/radio `hydrateItems` wrappers still are.
See the S-D3e section below.]**

**Round 4 (2026-11-16), residual documentation plus two new tests — no production code changed.** Scope: the round-3 review's documentation-completeness findings. The parent's verified fact list was re-executed end to end (Go 1.27.4 via `$(go env GOROOT)`, serde_json 1.0.154 exactly as in `rust/Cargo.lock`, no `preserve_order` feature), and all new claims below carry executed strings. Production code was NOT touched; the parent added two test functions after the writer stopped (`duplicate_keys_and_sorted_error_order_diverge_from_go` by the writer, `path_escaping_and_empty_key_diverge_from_go` by the parent), and the parent corrected the writer's Go type legs in this section and in `hydrate-contract.md` — see (D).
 - **Line-count correction (task a).** The `Landed` bullet's `NEW, 2123L, 28 tests` pre-dated the fix rounds' additions; measured after round 4's edits and the parent's follow-up test the file is **2502 lines with 31 `#[test]` functions** (53 `fn` occurrences counting helpers — "tests" means `#[test]` fns).
 - **"Never against string-rendered SQL" was overstated (task b).** `get_all_sql_ignores_the_sort_leg_until_options_are_wired` asserts on the RENDERED statement text for the sort/filter/pagination legs (no `ORDER BY`, `ends_with(" GROUP BY artist.id LIMIT 10 OFFSET 5")`, `WHERE` presence) and executes one deliberately-broken stats-backed `ORDER BY` against the fresh schema (to prove the dropped `artist.stats` column fails); the guard/decode arms assert on the `artist_post_scan` seam directly, and the read/artwork/write tests execute against the fresh schema. The corrected bullet is in the Landed list above.
 - **What round 4 added to `hydrate-contract.md`'s S-D3d list (each VERIFIED by execution, not inferred).** All type legs below are the ones Go actually prints, re-executed by the parent against `map[string]map[string]map[string]int64` (`persistence/artist_repository.go:47`): Go names the type of the DESTINATION level, so a bad library-level value names `map[string]map[string]int64`, a bad role-level value names `map[string]int64`, and a bad stat value names `int64` — only a payload that is not an object at all names the full 3-level type. (A) duplicate keys inside a stats cell — a BEHAVIORAL divergence: Go keeps the first value and errors (executed: `{"albumartist":{"m":true,"m":1}}` → `json: cannot unmarshal bool into Go struct field .albumartist.m of type map[string]int64`; `{"1":{"albumartist":{"m":true,"m":1}}}` → `… of type int64`), while serde_json collapses to the last value so the port ACCEPTS with `m = 1` (executed probe and seam); reachable only through a hand-edited/corrupted `library_artist.stats` TEXT cell, not through a normal scan. (B) error ordering — Go reports the first offending entry in DOCUMENT order (executed: `{"b":{"y":true},"a":{"x":true}}` → `.b.y`), the port walks serde's `BTreeMap` and reports the first in SORTED order (seam: `.a.x`); the similar-artists bullet's B-tree-walk root cause is named as shared. (C) JSON-Pointer escaping — Go escapes keys in `UnmarshalTypeError.Field` (executed: `{"a/b":5}` → `… Go struct field .a~1b of type map[string]map[string]int64`, `{"a~b":5}` → `… .a~0b …`, deeper `{"1":{"a/b":{"m":true}}}` → `… .1.a~1b.m of type int64`), the port interpolates the raw key (`.a/b`, `.a~b`, `.1.a/b.m` — seam-executed); unreachable under the fresh DDL (`library_artist.library_id INTEGER NOT NULL`, `schema_ddl.rs:62`), same reachability class as the documented NULL-key bullet. (D) EMPTY library-id key — executed Go: `{"":5}` → `json: cannot unmarshal number into Go value of type map[string]map[string]int64` (Go's `Field` is empty at that level, so the sentence has NO field leg); the seam produces `parsing artist stats from db: json: cannot unmarshal number into Go struct field . of type map[string]map[string]int64` (executed by `path_escaping_and_empty_key_diverge_from_go`) — the port BUILDS a field `"."` that Go never has, so the divergence is the `Go struct field .` phrase, NOT the port naming its own Rust type. An earlier `/tmp` probe of a hand-copied `go_into_what`/`go_type_error` and the parent's round-4 brief both claimed a 3-level type leg here; both were wrong — the library-level arm names `map[string]map[string]int64` (`hydrate.rs:2755`), which is Go's type for that level, and the 3-level spelling only appears in the port's ROOT arm (`hydrate.rs:2656`), where it matches Go verbatim. Deeper levels DO match byte-for-byte: `{"a":{"":5}}` → `json: cannot unmarshal number into Go struct field .a. of type map[string]int64` on both sides (executed). (E) `put_sql`'s forced `updated_at` — the comment above `put_sql` (now `artist_repo.rs:742-756`) now states BOTH cases: TRUE for `Put` (no `cols_to_update`), FALSE for filtered updates; the push itself is at `:765-767`, and the comment HEAD carried at `artist_repo.rs:597` said only "every save re-stamps `updated_at`", where Go's `selectUpdateColumns` (`sql_base_repository.go:593-610`) filters the SET map to exactly the requested columns minus `created_at`/`birth_time` and `UpdateExternalInfo` (`artist_repository.go:247-253`) does not list `updated_at`, so Go never writes it on that path; the divergence is PRE-EXISTING at HEAD (`git show HEAD:rust/crates/nd-db/src/artist_repo.rs | grep -n 'wanted.push'` → `:609`), pinned by the pre-existing `put_sql_update_leg_filters_cols_and_appends_updated_at`, and deliberately NOT changed (write path outside S-D3d's read scope).
 - **New tests (task 4):** `runtime_hydrate_artist.rs::duplicate_keys_and_sorted_error_order_diverge_from_go` pins the duplicate-key ACCEPT (stat-key level and role-key level) and the sorted-vs-document error ordering through the real `artist_post_scan` seam; `path_escaping_and_empty_key_diverge_from_go` (parent-added after the writer stopped) pins the raw-key rendering for `/` and `~`, the `Go struct field .` shape for an empty library id, and the deeper `{"a":{"":5}}` case where the two sides agree. Go strings are quoted in the doc comments, not assertable in Rust. Existing tests untouched.
 - **Rounds 2-3 citation audit, for the record (task c):** the stale-cite sweep found 25 mismatched Go citations (22 pre-existing at HEAD, 3 lane-introduced) by a brace-matched audit; round 3 rewrote 47 comment lines (its own count; see the round-3 entry above, which notes the cumulative lane diff now shows 57 changed comment lines in `artist_repo.rs` across rounds 1-4).

**Gate after round 4** (from `rust/`, all run by the parent after the writer stopped and after the parent's follow-up test; all actually executed): `cargo fmt --all -- --check` 0; `cargo clippy --all-targets --all-features -- -D warnings` 0; `cargo test --all-targets --all-features --no-fail-fast` 0 — **1722 passed / 0 failed / 1 ignored across 65 binaries** (round-3 baseline 1720 plus the two new tests); `cargo test -p nd-db --all-features --test runtime_hydrate_artist` 0 — 31 passed / 0 failed; `cargo doc -p nd-db --all-features --no-deps` 0 with the SAME 23 intra-doc-link warnings (9 unresolved + 14 private); `git diff --check` 0. The known `nd-stream` `transcode_body::tests::early_release_defeats_the_cap_and_accumulates_processes` flake did not fire in these runs. No executable code line changed in round 4: `git diff` shows only comment lines in `artist_repo.rs`, plus the new test function in `runtime_hydrate_artist.rs` and the two doc files; no Cargo.toml/Cargo.lock, no generated file, no Go source touched.

**Round 5 (2026-11-16), fresh read-only review - APPROVE WITH FINDINGS, no P0/P1 defect in the code.** Reviewer 2ad490bb (bifrost/openrouter/reasoning, a different model from the round-4 writer) built its own Go 1.27.4 oracle and confirmed every quoted string in bullets A-E and in both new tests is byte-identical to Go, including all destination-level type legs, and confirmed the port-side asserted strings are what the seam produces and that neither new test can pass vacuously. Findings: (i) a protocol P0 - hydrate-contract.md changed from 59c197d1... to 40184ff4... DURING its session, because the parent corrected bullet E mid-review; the reviewer verified the post-edit citations as accurate and flagged the snapshot drift; (ii) a P1 - next-work.md:1226 still cited artist_repo.rs:748 for the put_sql comment (HEAD :748 is markOrphansMissing; the comment HEAD carried is at HEAD :597, now :742-756), corrected by the parent afterwards. The parent independently re-verified all 12 third-party citations against the pinned sources (Go 1.27.4 encoding/json/v2_decode.go:149-151 and decode.go:136-141; dbx v1.12.0 rows.go:82-86, :244, :294; squirrel v1.5.4 select.go:103-109, :298-299, :313-314) and the Put/UpdateExternalInfo/selectUpdateColumns column lists.

**Round 6 (2026-11-16), final read-only verification - APPROVE, zero findings.** Verifier 5e8590e1 (bifrost/openrouter/coding, a different model from the round-5 reviewer) ran against a frozen hash snapshot that stayed unchanged for its whole session and re-measured every doc assertion: put_sql's executable body identical to HEAD (sed -n '600,644p' of HEAD vs sed -n '758,802p' of the worktree with comments excluded, diff exit 0 - the parent reproduced this), the updated_at push at :767 inside the if at :766-768 (the docs cite the enclosing :765-768 span), comment at :742-756, HEAD comment at :597, HEAD wanted.push at :609, markdown integrity (even backtick and quote counts on the lane's lines), and the metrics (2502 lines / 31 #[test] / 53 fn; 28 residual bullets; 1722/0/1 across 65; nd-db 643 across 22; 23 doc warnings, 9 + 14). The known nd-stream flake passed in its full run.

**Gate after rounds 5-6** (run by the parent after the last doc edit; real exit codes): cargo fmt --all -- --check 0; cargo clippy --all-targets --all-features -- -D warnings 0; cargo test --all-targets --all-features --no-fail-fast 0 - **1722 passed / 0 failed / 1 ignored across 65 binaries**, reproduced in a clean copy of rust/ outside the checkout with identical totals; cargo test -p nd-db --all-targets --all-features --no-fail-fast - **643 passed / 0 failed / 0 ignored across 22 binaries** (was 612 across 21 at the S-D3c baseline); cargo test -p nd-db --all-features --test runtime_hydrate_artist 0 - 31/0/0; cargo doc -p nd-db --all-features --no-deps 0 with 23 warnings (9 unresolved + 14 private, unchanged baseline); git diff --check 0. Status at lane close: HEAD was `1c215a7e` when the gates were measured; the lane is then committed as one GPG-signed commit on master atop it. Working tree = 5 modified + 1 untracked before that commit; no Cargo.toml/Cargo.lock, no generated file, no Go source, no scratch dir touched. The only executable lines this lane changed relative to HEAD are the S-D3d read/hydration additions themselves (get_all_sql gained the annotation threading, ArtistRepo::get/get_all were wired, the artist post-scan seam and artist artwork wrapper added); rounds 4-6 changed comments, docs and tests only.

## S-D3e: playlist_track + scrobble_buffer hydration, repo reads, playlist-track artwork (2026-11-17)

**Numbering.** This is `hydrate-scan-contract.md:208-211` — the delegating-hook slice for
`dbPlaylistTrack` (`persistence/playlist_track_repository.go:27-34`) and `dbScrobbleBuffer`
(`persistence/scrobble_buffer_repository.go:23-30`). `next-work.md`'s slice table files the same
material under "S-D3c artist/playlist/track/scrobble"; the artist half landed as lane S-D3d above,
and this closes the playlist_track/scrobble_buffer half.

**What landed** (4 modified sources + 1 new test file + these two docs):
- `hydrate.rs` — the two raw row readers `row_to_playlist_track` (`:2795`) and `row_to_scrobble_buffer`
  (`:2873`), and the two post-scan hooks `playlist_track_post_scan` (`:2828`) and
  `scrobble_buffer_post_scan` (`:2921`), each delegating to the existing `mediafile_post_scan` and then
  applying the signature `MediaFile.ID = MediaFileID` overwrite through the shared
  `apply_embedded_mediafile_id` (`playlist_track_repository.go:31-32`,
  `scrobble_buffer_repository.go:27-28`). Both readers build their media file through
  `mediafile_record_from_row` (`:2621`) with an `entry_id` override, so the media-file projection, read
  order and NULL behavior have ONE source of truth and cannot drift between a `media_file` read and a
  joined track or scrobble read. `id_field_from_row` (`:734`) resolves the DUPLICATED `id` name by
  walking every column of that name (dbx maps all of them to one field — `dbx@v1.12.0/struct.go:203-207`
  — and `sql.Rows.Scan` converts in emitted order, so the LAST non-NULL cell wins while any NULL cell
  aborts the scan). The record/row types live with their repos: `PlaylistTrackRecord`/`PlaylistTrackRow`
  in `playlist_track_repo.rs:79`/`:130`, `ScrobbleEntryRecord`/`ScrobbleBufferRow` in
  `scrobble_buffer_repo.rs:97`/`:147`.
- `playlist_track_repo.rs` — `PlaylistTrackRepo::read` (Go `Read` `:98-121`, a `queryOne`, so a miss is
  `model.ErrNotFound`) and `get_all` (Go `GetAll` `:123-129` → `playlistRepository.loadTracks`
  `playlist_repository.go:399-408`), plus `hydrate_track_artwork`, the port of
  `hydratePlaylistTrackArtwork` (`artwork_hydration.go:169-181`), which REUSES
  `artwork_hydration::hydrate_media_file_artwork` over `tracks[i].MediaFile` — `model/artwork_id.go:29-33`
  defines no `pt` kind, so there is no playlist-track artwork lookup to invent.
- `playlist_repo.rs` — `tracks_query`/`load_tracks_sql` carry the `library_path`/`library_name` legs and
  Go's filter-first clause order; `LOAD_TRACKS_WHERE` holds the `missing = false` + `playlist_tracks.id`
  order `GetWithTracks` adds (`:160-162`).
- `scrobble_buffer_repo.rs` — `next_sql` keeps Go's `SELECT m.*, s.*` order and the LEFT JOIN verbatim
  (`:66-70`, "Put `s.*` last or else m.id overrides s.id"); `ScrobbleBufferRepo::next` maps dbx's
  no-rows case to `Ok(None)` exactly like Go's `(nil, nil)` (`:78-80`).
- `tests/runtime_hydrate_playlist_track_scrobble.rs` — NEW, 24 `#[test]` functions, executed
  against `nd_dbschema::schema::apply_fresh_schema` (behavior assertions are all runtime; the
  statement-shape assertions are the documented exception already used by `runtime_hydrate_artist.rs`).
  Coverage: the `media_file_id → media_file.id` overwrite on both repos; a track whose `playlist_tracks.id`
  differs from its media file id; annotation columns with and without an annotation row for `Read` and for
  `tracksQuery` (with `library_path`/`library_name` present only in the latter); library-filter exclusion;
  `Next`'s `ORDER BY play_time, s.rowid` FIFO and the nil-nil empty queue; a LEFT JOIN row whose
  `media_file` is deleted; mediafile post-scan reuse (participants/tags/four `rg_*`/lyrics) reached through a
  track row; media-file-kind artwork states reached through tracks; `get_all` artwork hydration through the
  wrapper with an empty batch issuing no query; and write-map invariance asserted as
  `playlist_track`/`scrobble_buffer` maps EQUAL to `mediafile_row` key-for-key rather than a derived count.

**Three claims that execution overturned** — each re-verified by running the REAL Go repositories in a
`/tmp` harness (`replace github.com/navidrome/navidrome => <this checkout>`, Go 1.27.4, `-tags fts5`),
not by reading:
1. **`ScrobbleEntry.Participants` is not a separate field.** `model/scrobble_buffer.go:8-16` embeds
   `MediaFile`, so the entry's participants ARE the media file's (executed `ALIASED=true`,
   `&e.Participants == &e.MediaFile.Participants`). The first implementation left the entry's map empty
   and called that "not fabricating participants"; both the code and the test were wrong, and the test is
   now `scrobble_entry_participants_are_the_media_file_alias_not_a_fabricated_map`. Go's `getParticipants`
   re-read (`scrobble_buffer_repository.go:84`) is still NOT performed, so the port returns Go's value
   BEFORE the re-read. Executed contrast with JSON names `JSON-A1/A2/A3`, an `artist` row for `a1`/`a3` and
   a `library_artist` row only for `a1`: Go's `Next` yields `a1 name="DB-A1" sort="SortA1" order="db-a1"
   mbid="mbid-a1"` and leaves `a2`/`a3` at their JSON values; `Get` keeps all three JSON names.
2. **`m.*` does not lack `participants`/`tags`.** `rust/crates/nd-dbschema/src/schema_ddl.rs:64` declares
   both `jsonb NOT NULL DEFAULT '{}'`, so `SELECT m.*` carries them and the delegated PostScan decodes them
   (executed in Go: `len(Participants)=1`,
   `ParticipantsDetail={role=artist subRole="" id="a1" name="Artist A"}`). The older doc claim that the
   delegation "runs over absent JSON columns" is corrected in `hydrate-contract.md` §1 and in
   `hydrate-scan-contract.md` §2c/§2g.
3. **A missing `media_file` is a scan ERROR, not a tolerated hook.** `m.id` is NULL, dbx binds it into the
   promoted non-pointer `MediaFile.ID`, and Go fails before PostScan: executed
   `entry=nil err="sql: Scan error on column index 0, name \"id\": converting NULL to string is unsupported"`
   with `errors.Is(err, model.ErrNotFound) == false` and `errors.Is(err, sql.ErrNoRows) == false`. The brief's
   expectation ("the delegated hook must tolerate absent `participants`/`tags`") is not the behavior, and the
   `(nil, nil)` empty-queue contract stays scoped to a genuinely empty queue.

**Also found while verifying (recorded, not changed):** `playlistTrackRepository.Count` LEFT-joins
`media_file` (`:86`) while `CountAll` INNER-joins it (`:82`), and both add `applyLibraryFilter(..., "f")`
(`:85`, `:88`) — so for an admin (filter skipped, `sql_base_repository.go:265-271`) Go's `Count` includes a
track whose media file is gone and `CountAll` does not. Executed against ghost rows:
`adm Count=4 err=<nil> | CountAll=2`, `lim Count=2 | CountAll=2`. The port's `count_query` uses `inner_join`
(`playlist_track_repo.rs:227-232`), which PRE-DATES this lane (`git show HEAD:...` shows the same shape), and
`Count` is a `rest.Repository` seam (`rest.QueryOptions` + `parseRestOptions`, `:91`) belonging to the REST
lane — recorded as a residual rather than half-fixed.

**Gate after this lane** (from `rust/`, all run by the parent after the writer stopped; real exit codes):
`cargo fmt --all -- --check` 0; `cargo clippy --all-targets --all-features -- -D warnings` 0;
`cargo test --all-targets --all-features --no-fail-fast` 0 — **1747 passed / 0 failed / 1 ignored across 66
binaries** (baseline 1722 / 0 / 1 across 65; +25 = 24 lane tests + 1 new `playlist_repo` unit test);
`cargo test -p nd-db --all-targets --all-features --no-fail-fast` 0 — **668 passed / 0 failed / 0 ignored
across 23 binaries** (baseline 643 across 22); `cargo test -p nd-db --all-features --test
runtime_hydrate_playlist_track_scrobble` 0 — 24/0/0; `cargo doc -p nd-db --all-features --no-deps` 0 with
**22 warnings** — same 14 private warnings as baseline and 8 unresolved vs baseline's 9: the lane fixed one
PRE-EXISTING baseline warning by correcting an intra-doc link that pointed at the Go name
`get_all_ids_query` instead of the Rust `get_all_ids_sql` (`playlist_repo.rs:34`), and added none. Measured by
stashing the lane's `src/` changes and re-running the same command (baseline 23: 9 unresolved + 14 private).
`git diff --check` 0 from the repo root. `nd-stream`'s
`transcode_body::tests::early_release_defeats_the_cap_and_accumulates_processes` is the known timing-sensitive
flake (it counts live `sleep` stub PIDs via `ps`); it passed in this lane's full run and was not touched.

**Review round 2 (2026-11-17, read-only reviewer `0e801085…`, `bifrost/vllm/strata-coder`, same workspace,
no edits).** Verdict APPROVE WITH FINDINGS. Behavioral parity: the reviewer found NO undocumented divergence
in `PostScan` ×2, `Read`, `GetAll`/`tracksQuery`, or scrobble `Next` (join order, column interleave, filter
position FIRST vs LAST, LIMIT/ORDER BY, `ErrNotFound` → `(nil,nil)`, unported `getParticipants`), and confirmed
the participants alias, the 180-assertion executed coverage (138 `assert_eq!` + 42 `assert!`), and the key-for-key write map. Its gate run matches
the parent's exactly: 66 binaries / 1746 passed / 0 failed / 1 ignored, clippy clean, 22 doc warnings.

**Parent adjudication of its findings (each re-verified by the parent, not accepted on claim).**
- **P1 "participant role order is a write-map divergence" — REJECTED, the reviewer is wrong and the docs were
  corrected.** `marshalParticipants` passes a `map[model.Role][]participant` to `json.Marshal`
  (`sql_participations.go:50-59`), and Go's `encoding/json` SORTS map keys — `encode.go:781-794` extracts with
  `v.MapRange()` then `slices.SortFunc` over the resolved key strings (documented at `encode.go:184-186`).
  Executed against the real `model.Participants` with roles inserted out of string order: 5 consecutive
  marshals byte-identical, keys `albumartist, artist, producer`; a separate 5-run plain-map oracle and an
  8-run loop over non-ASCII keys gave one distinct output each. So the Rust `BTreeMap` key order REPRODUCES
  Go's column bytes. What IS nondeterministic is `Participants.AllNames()` (`model/participants.go:175-185`),
  which walks the map directly: 40 executed calls over the same input produced 3 distinct name orders. That
  feeds `full_text`/`search_participants`, which `sql_feeders.rs::search_participants` sorts and re-dedupes
  (`:67-76`), so it stays unobservable. `hydrate-contract.md`'s S-D3c bullet and §7 plus `next-work.md`'s
  carried-forward bullet were rewritten accordingly.
- **P2(c) "`join_text` turns a missing participants column into empty where Go errors" — REFUTED BY
  EXECUTION.** `join_text` yields `""`, and Go's unguarded `unmarshalParticipants`
  (`mediafile_repository.go:55-58`) fails on `""` (`json.Unmarshal("")` → `unexpected end of JSON input`,
  executed in a Go oracle). New executed test
  `a_projection_without_the_participants_shim_still_fails_the_unguarded_decode` projects EVERY fresh-DDL
  `media_file` column except `participants`/`tags` (derived from `pragma_table_info`, not hand-listed) and
  asserts the hook still reports `parsing media_file from db: parsing participants:`. The leniency itself —
  an absent column reads as the zero value rather than a missing-column error — mirrors dbx binding refs only
  for result columns (`dbx@v1.12.0/rows.go:76-87`) and is now its own residual bullet; it is unreachable from
  any live read: every projection that REACHES the entity hydrator carries both shims —
  `get_sql`/`get_all_sql`/`get_random_hydrate_sql` all render `select_media_file`'s
  `SELECT media_file.*, library.path as library_path, …` (`mediafile_repo.rs:283-285`, which
  `get_random_hydrate_sql` reuses at `:511`; phase 1's `get_random_rowid_sql` is the one that emits
  only `media_file.rowid` at `:452`), and `find_by_paths_sql`/`get_missing_and_matching_sql` emit
  `SELECT * FROM media_file`/`SELECT media_file.*` (`:1541`, `:1694`). The id-only projections
  (`get_all_ids_sql` `:742`, `get_random_rowid_sql` `:452`) are never passed to
  `row_to_mediafile` — `get_all_ids_sql` has no repo-level caller in the port at all (only its own
  unit test), and `get_random_rowid_sql`'s rowids feed `get_random_hydrate_sql`, which does carry
  the columns.
- **P2(a), P2(b), P2(d), P2(e), P3 "four new tests"/"annotation_play_count unguarded"/P2(f) HANDOFF totals —
  REJECTED as fabricated.** None of the quoted text or symbols exists in the tree: `grep` for "117 tests",
  "96 pre-existing", `load_tracks_interleaves_annotation_columns_in_go_order`,
  `load_tracks_renders_the_library_join_legs`, `get_all_executes_against_a_real_database`,
  `playlist_track_write_map`, `scrobble_write_map`, `select.go:305`, `hydrate-contract.md:205`,
  "free to reorder", "minus id/library_path/library_name/genres", "unguarded Go-parity decoder", and
  "verified 2026-11-12" in `hydrate-contract.md` all return nothing; `playlist_repo.rs` has 20 `#[test]`s in
  51 `fn`s, not 149/116; the cited line ranges hold unrelated code; `mediafile_row`'s second parameter is
  `append_subtitle`, not an FTS flag, so the lane's 66-key figure matches the executed precedent at
  `runtime_hydrate_album_mediafile.rs:1301`/`:2520`; `annotation_play_count` (`hydrate.rs:1933`) has no guard
  at all (the guard the reviewer describes belongs to the caller-side `row_to_annotation`); `join_text`
  pre-dates this lane (`git show HEAD:...hydrate.rs:2314`) and cites nothing.
- **P2(g) 22 intra-doc-link warnings — confirmed, repo-wide, 0 from the lane's four files, no action.**
- **Its INFERRED/GAPS note is accurate**: no Go harness was run, so cross-language byte goldens for the
  playlist/scrobble write maps were source-argued. The parent closed part of that gap with its own Go oracles
  (participants marshalling, `AllNames` ordering, `json.Unmarshal("")`).

**Docs-evidence audit (2026-11-17, read-only auditor `4a831abf…`, `bifrost/vllm/strata-coder`, same
workspace, no edits).** Verdict APPROVE WITH FINDINGS. It independently reproduced every gate number
(1747/0/1 across 66; nd-db 668/0/0 across 23; lane 24/0/0; fmt 0; clippy 0; doc 22 = 14 private + 8
unresolved; `git diff --check` 0), confirmed all four parent edits accurate, re-ran the participants oracle
(5 runs byte-identical) and `AllNames` (40 calls → exactly 3 orders), and confirmed `json.Unmarshal("")` →
`unexpected end of JSON input`. Parent adjudication of its findings:
- **F1 ACCEPTED — a real undocumented divergence the parent had wrongly rejected.** Executed against the real
  Go repository (`/tmp/sd3e_f1`, `-tags netgo,sqlite_fts5`): with `participants = ''`,
  `playlistTrackRepository.Read` returns `ptrNil=false ID="1" MediaFileID="m1" MediaFile.ID="" Title=""`
  **beside** `err="parsing media_file from db: parsing participants: unexpected end of JSON input"`, because
  dbx allocates the shared embedded pointer (`struct.go:251-258` `indirect`) and fills it before `PostScan`
  (`rows.go:88-99`). On a missing row it IS nil (`ptrNil=true err="data not found"`), and `GetAll` returns a
  nil slice, so the port's empty `Vec` is faithful. Recorded as a new S-D3e residual bullet; the §S-D3b
  `Get`-returns-nil precedent is scoped to say it does not cover this path. Not emulated: `Result` has no
  value-beside-error and no Go caller reads the value on error (only non-test caller
  `server/subsonic/sharing.go:99` is the share repo; `deluan/rest@v1.0.1/controller.go:26-31` marshals only
  when `err == nil`).
- **F3 ACCEPTED** — the deleted-media-file bullet named `playlist_track_left_join_over_a_deleted_media_file_fails_the_scan`
  as demonstrating `Read`; it actually asserts a hand-written LEFT JOIN, while `Read` INNER-joins and yields
  `NotFound` (which the other test asserts). Reworded to say exactly that.
- **F4 ACCEPTED** — "54-assertion" was stale; the file has 180 assertion invocations (138 `assert_eq!`,
  42 `assert!`).
- **F6/F7 ACCEPTED** — the claim that `get_random_rowid_sql` "never reaches the hydrator" was over-broad and
  `get_random_hydrate_sql` was mis-described as emitting bare `media_file.*`. Rewritten: it reuses
  `select_media_file` (`mediafile_repo.rs:283-285` at `:511`), so its projection DOES carry both shims and
  does reach the hydrator; only `get_all_ids_sql` (`:742`, no repo-level caller) and `get_random_rowid_sql`
  (`:452`, whose rowids feed `get_random_hydrate_sql`) are id/rowid-only and never passed to
  `row_to_mediafile`.
- **F9 ACCEPTED (partly)** — `hydrate-contract.md:102`'s blanket "no non-nil pointer beside `ErrNotFound`"
  is now scoped: true for album/mediafile `Get` and for track `Read` on `ErrNotFound` (executed
  `ptrNil=true`), false for track `Read` on a `PostScan` failure.
- **F2 REJECTED** — `hydrate-contract.md:151` lists exactly `(a)`–`(d)` and says "four ways"; there is no
  `(e)` on that line. The auditor's count came from its own line-wrapping of the bullet.
- **F5 REJECTED as fabricated** — `next_orders_by_play_time_then_id_fifo` appears nowhere in the docs
  (`grep -rn "then_id_fifo" docs/rust-port/` is empty); the docs use the correct name.
- **F8 REJECTED for this lane** — `sql_participations.rs:26-33`'s stale entry-order rationale is real but
  PRE-EXISTING (`git show HEAD:rust/crates/nd-db/src/sql_participations.rs` shows identical text), outside
  S-D3e's scope; recorded here so the S-A lane can correct it, not edited here.
- **F10/F11 REJECTED as fabricated** — no text claiming `Count` "keeps `ORDER BY`" and none claiming
  `hydrate_media_file_artwork` "runs on the `get_all` path" exists in the docs or the lane's sources
  (`grep` empty). The auditor was describing the code's behavior, not a claim it made.

**Round-2 reviewer re-audited itself and REVISED its verdict to APPROVE** (`0e801085…`, same workspace,
read-only). It voided its own P1, P2(a), P2(b), P2(c), P2(d), P2(e), P2(f) and P3 items as **fabricated
evidence** — not line drift, since it confirmed the four sources were byte-identical to what it reviewed
while its post-review reconstruction only needed the OLD test hash. It independently confirmed the parent's
corrections: Go sorts map keys (`encode.go:781-794` over `resolveKeyName`, with `model/participants.go:58-60`
`Role.MarshalText` supplying the resolved string) and Rust's `Role(String)` derives `Ord` on the inner string
and serializes as `serialize_str`, so `BTreeMap` gives the same bytes; `join_text` predates the lane and cites
`album_repository.go:43`/`mediafile_repository.go:59`, never `hydrate-contract.md:205`. Its citation re-audit
with a corrected resolver found 0 out-of-range citations (333 in the sources + new test, 94 in the docs' added
lines) — its earlier "24 problems" was resolver error, having matched `convert.go`/`row.rs` to go-sqlite3 and
sqlx. It reproduced the gates (1747/0/1 across 66; nd-db 668; lane 24/0/0) and flagged one REAL item:

- **P2 ACCEPTED — `hydrate.rs`'s `join_text` comment over-claimed (pre-existing HEAD text, now scoped).** It
  said the shim columns' absent value makes "`PostScan`'s `!= ""` guard take the skip branch … That is why
  the light projections (`getAllIDs`, the rowid pick) hydrate without erroring." That holds only for the
  GUARDED shims (`album_repository.go:43`/`:52`/`:59`, `mediafile_repository.go:59` — Rust mirrors each at
  `hydrate.rs:2540`/`:2554`/`:2566`/`:2767`); `participants` is UNGUARDED (`album_repository.go:48`,
  `mediafile_repository.go:55-58`), so an omitted column ERRORS — exactly what this lane's new test proves,
  which contradicted the comment. Rewritten to separate guarded from unguarded, and to state the real reason
  the light projections never error: `queryAllSlice` (`sql_base_repository.go:460-466`) → dbx `Query.Column`
  (`query.go:308-316`) → `Rows.column` (`rows.go:200-233`) scans each row into a bare element and never calls
  `ScanStruct`, so `PostScan` is never invoked (verified against the vendored module; Go's `getAllIDs`,
  `album_repository.go:262-271`, scans `[]string` the same way). Docs-only; no behavior change, gates
  re-run: fmt 0, clippy 0, doc 22, workspace 1747/0/1 across 66, nd-db exit 0, lane 24/0/0, `git diff --check` 0.
- **Two P3 items declined as out-of-lane**: `HANDOFF.md:24-30` is already date-scoped ("Measured at `12e270f7`
  (2026-11-11)") and untouched by this lane; `hydrate-scan-contract.md:218`'s "verified 2026-11-12" names its
  own lane (S-D3a), so it is accurate as written.
- **Its own caveat is recorded**: it is the same model family as the parent, produced fabricated citations in
  its first pass, and no independent provider was available on either side (every non-`pi/bifrost/vllm` route
  is disabled or budget-capped).

**Round-3 verification of that fix (`0e801085…`, read-only, no edits). VERDICT: APPROVE, no findings open.**
It proved the fix is comment-only structurally rather than by trust: only `hydrate.rs` moved since its
round-2 audit, the other three sources and the test file byte-identical, every symbol before the `join_text`
block at an unchanged line and every symbol after it shifted by exactly +9 with identical bodies, and
`join_text`'s body byte-compared against HEAD (parent re-ran this independently: identical, 6 lines). It also
verified the new citations — `ScanStruct` is DEFINED at `dbx rows.go:65` and invoked ONLY at `:270` inside
`Rows.one`, while `Rows.column` calls plain `r.Scan` at `:226`, so `PostScan` genuinely never runs on the
light projections — and confirmed the guarded/unguarded split and the four Rust `is_empty()` mirrors.
- **Its correction applied**: it flagged `Rows.column`'s span as `rows.go:200-233`, not the `200-221` the
  parent's comment claimed (the function ends at `:233` with `return r.Close()`; `:221` is only the
  `refs[0] = ev.Interface()` arm). Both the comment and this document now say `200-233`. Parent re-verified
  the span and `Query.Column` at `query.go:308-316` directly.
- Its two remaining flagged citations were checked by the parent and are NOT defects:
  `persistence/helpers.go:89-94` is `naturalSort`, `deluan/rest@v1.0.1/errors.go:14` is
  `ErrNotFound = errors.New("data not found")`.
- Gates re-run by the parent after the range fix: fmt 0; clippy 0; `cargo doc -p nd-db` 22 warnings;
  workspace 66 binaries / 1747 passed / 0 failed / 1 ignored (exit 0).
**Process note for the record**: this reviewer's FIRST report contained fabricated citations and must not be
cited as evidence; the self-audit and this round-3 pass are the ones to rely on. It is the same model family
as the parent, so both are corroborating rather than independent.

**Residuals** are recorded in `hydrate-contract.md`'s new S-D3e block (12 bullets): the participants alias and
the unperformed re-read; deleted-media-file scan error vs the empty-queue contract; the `m.*` participants/tags
correction; the absent-`participants`/`tags` shim column yielding `""` and still failing the unguarded
decode; the duplicated-`id` last-column rule and its NULL abort; the four `Read`-vs-`tracksQuery`
asymmetries (column spelling/order, filter-first vs filter-last bind order, the `library` join only in
`tracksQuery`, NULL annotation timestamps when no annotation row exists); Go's `JOIN` text and nested
`WHERE (…) AND …` vs the port's `INNER JOIN` and flat `WHERE`; `GetAll` forwarding `QueryOptions` so
`Sort`/`Order`/`Max`/`Offset` AND `Filters` reach Go's tracks select while the port renders none of them (with
`optimizePagination` inexpressible and Go's `Offset`-only option a SQLite syntax error, executed); `NotFound`
from `queryOne` vs an empty `Vec` from `queryAll`, and `Read` being the only track read that skips artwork
hydration; Go's non-nil partially-populated `*PlaylistTrack` beside a `PostScan` error vs the port's bare
`Err`; `ORDER BY play_time, s.rowid` over text timestamps with `Length` counting rows `Next` can never
return; and the pre-existing `Count` LEFT-join vs `CountAll` INNER-join divergence. Still unported:
`playlistTrackRepository.GetCursor` (`:131-143`, `queryWithStableResults` + `hydrateCursor`),
`streamByIDs`/`artworkChunkSize`, and `getParticipants`/`GetWithParticipants`. (The playlist and radio
`hydrateItems` wrappers were unported at that writing; **S-D3f below landed both**.)

## S-D3f: playlist + radio artwork hydration wrappers and both repo read sides (2026-11-17)

**Numbering.** This is `hydrate-scan-contract.md:213-215` — sub-slice **S-D3f**, the *second*
hydration layer (`hydrateItems`' batched `item_artwork` lookup filling `ItemImage`), kept out of
`hydrate.rs` by that contract. `next-work.md`'s slice table files the same material under
"S-D3c artist/playlist/track/scrobble"; S-D3d landed the artist wrapper and S-D3e the
playlist-track one, and this closes the remaining two (`hydrateItems` has exactly four Go call
sites: `album_repository.go:258`, `artist_repository.go:295`, `playlist_repository.go:192`,
`radio_repository.go:81`). It also wires the READ side of both repositories, which until now were
statement-only builders (`git show HEAD:rust/crates/nd-db/src/playlist_repo.rs` and
`…/radio_repo.rs` expose only `*_sql` builders — no `PlaylistRepo::get`/`get_all`/`find_by_path`/
`get_playlists`, no `RadioRepo::get`/`get_all`).

**What landed** (5 modified sources + 1 new test file + these docs):
- `artwork_hydration.rs` — `hydrate_playlist_artwork` (`playlist_repository.go:191-194`) and
  `hydrate_radio_artwork` (`radio_repository.go:80-83`), both over the existing `hydrate_items`, so
  the four states come from ONE lookup: empty ids issue no statement (`artwork_hydration.go:59-61`),
  a failed lookup yields an EMPTY map with earlier chunks discarded
  (`artwork_repository.go:175-176` + caller `:65`), `BATCH_SIZE = 200` chunks
  (`artwork_repository.go:16`, used at `:161`), ids collected in slice order **without dedup**
  (`:81-84`). Kind is the PREFIX (`pl`/`ra`, `model/artwork_id.go:31,33`), bound as
  `artwork_repository.go:169` binds it — never the kind name. The module header's residual list is
  corrected: it previously said the folder `image_files` artwork path was unported at
  `artwork_hydration.go:169`, but `:169` is `hydratePlaylistTrackArtwork` and there is NO folder
  wrapper in that file at all.
- `playlist_repo.rs` — `PlaylistRepo::get`/`get_all`/`find_by_path`/`get_playlists`, all through
  `crate::execution` + `hydrate::row_to_playlist` + the new wrapper, with Go's NotFound decided by
  ROW COUNT before hydration (`findBy` `:182-187`) rather than by a driver no-rows condition, and
  the artwork failure swallowed by `get`/`find_by_path` but surfaced as `ReadOutcome::artwork_error`
  by `get_all`/`get_playlists` — exactly Go's swallow-and-log (`artwork_hydration.go:62-66`).
- `radio_repo.rs` — `RadioRepo::get` (queryOne ⇒ NotFound before hydration, `:58-66`) and
  `get_all` (`:68-77`), neither carrying a user or any visibility clause, plus the
  `ItemImage` field on both records so hydration has a destination (the `structs:"-"` embeds are
  read-path-only: neither write map gained an artwork leg, asserted against the live DDL).
- `tests/runtime_hydrate_playlist_radio_artwork.rs` — NEW, 20 `#[test]` functions, executed against
  `nd_dbschema::schema::apply_fresh_schema`. Coverage: all four artwork states per repo through
  both `get` and `get_all`; NotFound for both repos with the artwork lookup provably never run
  (`item_artwork` dropped first, so a lookup that ran would have produced Go's swallowed-error
  shape instead); a failed lookup reported by `get_all` and swallowed by `get`; empty tables as
  empty `Vec` with no artwork query at all; kind-prefix isolation and no cross-kind id collision;
  `FindByPath` emitting no visibility clause; `GetPlaylists` hydrating only the matching playlist;
  the 201-id chunk boundary (2 statements, no dedup); write-map invariance DERIVED from
  `PRAGMA table_info` rather than a guessed count (playlist 15 keys — `id`/`rules`/
  `average_rating` excluded and why — radio 7, `image` never a leg); and the offset-only page
  executing as the builder documents.

**Three defects found and fixed in this lane** — each confirmed by executing the pinned sources
against a real DB or the pinned squirrel, never by reading alone:
1. **`PlaylistRepo::get` defeated the visibility clause.** `get_sql` pushed `user_filter`'s
   `public = ? OR owner_id = ?` raw into a flat `WHERE a AND b`, so SQL's precedence made the
   predicate `(id AND public) OR owner_id`. Executed against the pinned squirrel, Go renders
   `WHERE (playlist.id = ? AND (public = ? OR owner_id = ?))` (`expr.go:384-386` wraps every
   conjunction group). Executed rows over {adm-private, adm-public, lim-private} as a non-admin:
   Go gave `data not found` for the admin-private id and for a missing id, and the specific row for
   each visible id; the flat port form returned the `lim`-owned row for ALL FOUR requests,
   including `pl-nobody`. Fixed by wrapping the clause as the five sibling builders already did;
   `get_all_sql` wraps it too for text parity. Pinned by 3 new unit tests plus the executed
   `playlist_get_visibility_clause_still_restricts_the_requested_id` (four request arms, exact row
   identity, `GetAll` 2 vs 3 matching the oracle `adm=3 | lim=2`). Blast radius was limited to
   `get_sql`: `find_by_path_sql` carries one clause, and every other builder was already wrapped.
2. **`radio_repo::select_update_cols` matched column names verbatim.** Go's `Put` appends the
   camelCase `"UpdatedAt"` (`radio_repository.go:95-97`) and `selectUpdateColumns` runs every
   requested name through `toSnakeCase` (`sql_base_repository.go:596-598`); radio's real callers
   pass camelCase (`server/subsonic/radio.go:122` → `"StreamUrl", "HomePageUrl", "Name"`;
   `server/nativeapi/radios.go:48`,`:70` → `"UploadedImage"`). Executed before the fix: those three
   columns were silently dropped and the statement became
   `UPDATE radio SET updated_at = ? WHERE id = ?`. Fixed by normalizing through
   `crate::marshal::to_snake_case` (the seam `playlist_repo` already uses), with a camelCase
   `"UpdatedAt"` collapsing instead of duplicating the leg. Pinned by 2 new unit tests.

3. **The two playlist REST filter builders were wrong against squirrel** (latent — neither has a
   consumer in `rust/crates` yet, and `playlist_repo` exposes no `RestRepository`). Executed against
   the pinned squirrel (`/tmp/sd3f_p1/args.go`, `/tmp/sd3f_p1/q.go`):
   `smartPlaylistFilter` renders `(rules = ? OR rules IS NULL)` with ONE arg (a nil `Eq` becomes
   `IS NULL` with no placeholder, `expr.go:182-183` — `if val == nil`, not a `case nil:` arm), but the port emitted
   `rules = ? OR rules = ?` with a NULL bind — and `rules = NULL` is never true, so it excluded
   exactly the NULL-`rules` rows a non-smart `Put` writes (`playlist_repository.go:49`).
   `playlistFilter` flattened every word of BOTH fields into one `OR`, while Go's two
   `substringFilter` legs are `And`s over `strings.Fields` (`sql_restful.go:119-126`), so the port
   matched a row with one word per field Go rejects. And an empty `q` returned `1=0`: Go's `Or` legs
   are empty `And`s, rendered `1=1` each (`sqlTrue`), so `q=` matches everything. All three now match
   squirrel's text and argument counts, and every `Or` these builders emit is parenthesized
   (`expr.go:384-386`). Pinned by 4 unit tests and 2 executed row-set tests
   (`smart_filter_clause_includes_a_null_rules_row`,
   `q_filter_requires_all_words_within_one_field_not_across_fields`), each of which also asserts the
   pre-fix text as the counterfactual. Four fragments still return a bare `OR` and are recorded as
   residuals rather than changed — `child_playlist_filter_sql:293`, `criteria_sql::Cond`'s `Or` join
   (`criteria_sql.rs:1385-1389`), `playlist_track_repo::add_discs_sql:585`, and
   `artwork_queue_repo::source_filter_sql:284-287` — each executed against squirrel/SQLite to show the
   flat form changes a row set. Round 2's justification that Go passes `child_playlist_filter_sql` as
   the ONLY filter is wrong: `GetAll` appends `.Where(r.userFilter(ctx))` (`playlist_repository.go:196-197`),
   so a conjunct does sit beside it. See the S-D3f residual block in `hydrate-contract.md`.

**Also fixed in this lane but previously unlisted:** `tracks_query` (`playlist_repo.rs`) now applies
the library filter BEFORE `playlist_id`, matching Go's `applyLibraryFilter` first
(`playlist_repository.go:375`) and `Where(Eq{"playlist_id": id})` last (`:396`); it used to render
`WHERE playlist_id = ? AND <filter>` with the playlist id bound first. Clause order cannot change a
row set (both legs are AND-ed), so this is SQL-text and bind-order parity with Go's trace —
`playlist_track_repo::read_sql` keeps the opposite order because Go's `Read` applies the filter last
(`playlist_track_repository.go:117`), and the executed
`library_filter_arg_order_differs_between_read_and_tracks_query` plus
`library_filter_excludes_tracks_from_unseen_libraries` (row set, in the S-D3e file) pin both sides.

**A doc claim corrected against the source, not softened:** `radio_repo`'s header said
`cols_to_update`'s names are "the fixed snake_case column identifiers, never user text" — that was
not the contract Go's callers use. Also corrected in the source comments: radio's unfiltered reads
are unfiltered because there is no `userFilter` and `newSelect` adds nothing
(`sql_base_repository.go:139-147`), NOT because of the `Exists` comment at `:38`; `RadioRepo::get`'s
`# Errors` no longer lists the OFFSET-without-LIMIT syntax error, which the builder's `LIMIT -1`
deviation makes unreachable here; `hydrate::row_to_radio` no longer claims `GetAll` returns
`len == 0` for a NULL timestamp — dbx appends before aborting (`rows.go:169-171`, append at `:192`)
so Go returns the rows scanned so far beside the error (executed `len == 2`; `len == 0` only when
the failing row is first), and playlist genuinely returns `nil, err` (`playlist_repository.go:200-201`).
`radio_repo::count_all_sql` now renders the `ORDER BY radio.id` leg Go's `count` always installs
(`sql_base_repository.go:569`, traced in the oracle), which `playlist_repo::count_all_sql` already
emitted.

**Gate after this lane** (run by the parent; real exit codes): `cargo fmt --all -- --check` 0;
`cargo clippy --all-targets --all-features -- -D warnings` 0; `cargo test --workspace --all-targets
--all-features --no-fail-fast` 0 — **1776 passed / 0 failed / 1 ignored across 67 binaries**;
`cargo test -p nd-db --all-targets --all-features` 0 — **697 passed / 0 failed / 0 ignored across 24
binaries**; `cargo test -p nd-db --all-features --test runtime_hydrate_playlist_radio_artwork` 0 —
20/0/0; `cargo doc -p nd-db --all-features --no-deps` 0 with **22 warnings**
**Arithmetic check on the recorded totals (verified, not re-measured):** the non-`nd-db` suite mass is
constant at 1079 across these three lanes (`1776 − 697 = 1079` now, `1770 − 691 = 1079` and `1722 − 643 = 1079` at the S-D3d
baseline), so the S-D3e section's workspace figure **1746** was one short of what its own `nd-db`
figure implies (`1079 + 668 = 1747`); `nd-db` itself is self-consistent (`643 + 25 + 23 + 6 = 697`). The
gate numbers in THIS section are the parent's own measured output, so the chain is now pinned at 1776 (1770 before this round's 6 added tests).
 (8 unresolved + 14
private — one unresolved link FEW than the S-D3d/S-D3e baseline of 9 + 14, because a stale
`[`get_all_ids_query`]` reference was removed and none was added); `git diff --check` 0. The
known `nd-stream` flake
(`transcode_body::tests::early_release_defeats_the_cap_and_accumulates_processes`) passed in the
full parallel run and was not touched. Working tree at lane close: 3 modified docs + 5 modified
nd-db sources + 2 untracked nd-db test files; no Cargo.toml/Cargo.lock, no generated file, no Go
source, no scratch dir touched or deleted.

**Round 3 (2026-11-18, after the third fresh review):** the reviewer's verdict was APPROVE-WITH-FIXES with
no P1, and it independently re-executed both round-2 fixes and confirmed them. Every finding was re-verified
against the pinned sources before acting; round-3 changes are documentation/comment corrections only, so no
test was added or removed and the gate numbers above are unchanged. Fixed: the `radio_repository.go:70-73`
citation still present at `runtime_hydrate_playlist_radio_artwork.rs:1082` (correct range `:71-73`), that file's
`model/artwork.go:49` and `:81-93` refs (actual `:50` and `:82-95`), the misquoted squirrel arm (`case nil:` →
`expr.go:182-183` `if val == nil`) in `playlist_repo.rs:118` and both doc bullets, `playlist_repo.rs`'s `Delete`
citation (`sql_base_repository.go:644-647`, not `:649-655`), `radio_repo.rs`'s `deleteByID` citation (`:650-658`,
not `:594-601`) and its `GetAll`-empty-`res` range, `next-work.md:355`'s "(18 tests)" (the file has 20), and the
OR-precedence residual bullet, which round 2 had under-enumerated and justified with a claim the source
contradicts. The `playlist_track_repo::library_filter` alias contract is now documented at its definition after
executing the parse error it causes. Refuted one reviewer claim: it asserted `read_sql` normalizes the library
filter's alias the way `tracks_query` does — `read_sql` has no `replace` call (`playlist_track_repo.rs:253-263`),
so it carries the same requirement as the builder-shaped queries, and the doc was written to say that.
Reviewer citations it did NOT change and this lane leaves alone: `artwork_repo.rs:197`, `runtime_artwork_hydration.rs:10`/`:280`
and `smart_playlist_repo.rs:32` carry the same off-by-one `model/artwork.go` refs but are committed files outside
this lane's ownership.
