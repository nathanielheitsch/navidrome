# Plan of record for the next body of work

Read order and authority: [`README.md`](README.md) → this current section (milestone order only) →
[`current-status.md`](current-status.md) (measured evidence) → [`decisions.md`](decisions.md) (binding decisions) →
`subplans/` (bounded assignments). Everything below the separator is a **dated historical index**, not a scheduler.

## Current execution plan (baseline this branch line at `cb59a973` — SC-03 `cb59a973` + BOOT-00 `6d6938b1` + DB-17 `b3dee0a6` + DB-16 `7d022a02` + DB-21(P1-DB) `632271a9` + DB-14 `de0f49ea` + DB-15 runtime `96c495e7` + DB-11 `52b04064` on integrated `master` code HEAD `e3e0770a` (SC-02 partial), re-dated 2026-11-23)


`96c495e7` adds **DB-15's runtime half** (annotation transfer + average-rating recalc on the caller's tx, 5 executed
tests) and `52b04064` adds **DB-11** (executed typed scanner reads, the D-CURSOR cursor contract, the API-safe point
read, 21 executed tests) on top of `e3e0770a`, the **SC-02 partial** — the bounded local walker, `.ndignore`/ignore
policy, folder hashing and cancellation in `nd-scanner`, fast-forwarded onto `2e7026b2` — which sat on the prior
boundary `272c9c66`, which added DB-22 (executed
genre/scrobble/playqueue/artwork reads, C5 narrow boundary) to `f80b9704`;
`f80b9704` added DB-12 `e5d0592d` (signed implementation, fast-forward), DB-10 `65072b89` (signed implementation) / signed
merge `6e5ca97d`, and the DB-12 rustdoc fix `f80b9704` to the prior boundary:
`4ab63cd5` = `74ebd81c` + `f03aff98`/merge `3b5e6c76` (docs consolidation) + `c791dbc1` (DB-04 property writes) +
`60be7534` (DB-05 library writes + scan lifecycle) + `bfdcfeca` (DB-06 tag dictionary/library links) + `2911b9ab`/merge
`9be6ad0e` (DB-07 folder missing-state + album touch) + `32c4fa5b`/merge `320b8b03` (DB-08 artist writes + timestamp
parity) + `d938d159`/merge `842c32a2` (DB-09 atomic album writes) + `c6e220eb`/merge `162fae1d` (BOOT-02 native expiring
login sessions) + `eae4ba1d`/merge `4ab63cd5` (DB-13 epoch-only revocation). Evidence is in `current-status.md`; this
section only sets order. Nothing here authorizes implementation, commits, new workspaces, or completion claims. The
**SC-02 partial** is a scoped slice — not SC-01 ratification, SC-02 completion, MusicFS readiness, or a runnable scanner;
**DB-11** is the executed read/cursor contract only (no production scanner caller, no `nd-scanner` consumption, not the
full cursor/REST surface) and **DB-15** is its runtime half only (durable Go oracle deferred to the EVID lane).
`632271a9` adds **DB-21(P1-DB)**, `7d022a02` adds **DB-16** (executed move/reference-remap/delete primitives, the A8
share typed rejection and the D-PLUGIN deletion; 18 executed tests), `b3dee0a6` adds **DB-17** (four caller-tx count-refresh
helpers + `tests/runtime_count_refresh.rs`, 20 executed tests) and `6d6938b1` adds **BOOT-00** (D3 manifest pin repair + the D1
dependency boundary with the executed `dependency_graph_has_no_http_api_cycle`; gate work only, no production behavior). This docs
lane runs at `6d6938b1`; latest Cargo gates are parent integration results on the merged tree (logs checked here), while this
lane's earlier `4ab63cd5` measurements remain historical.

### Authority and launch rules

- This current section sets milestone order; `current-status.md` records measured capabilities/evidence; `decisions.md`
  holds binding decisions and durable contracts; the [`subplans/`](subplans/) contracts refine method-level scope.
  Scoped contracts retain source history, but their current amendments supersede old missing/remaining labels. Do not
  schedule from historical tables.
- **State axes** (see `decisions.md` C3): rendered SQL / decoded records / executed reads / executed writes / mounted
  routes / real end-to-end behavior. Never infer one axis from another or from test counts.
- Docs/planning work is **not** implementation authorization. Resolve a package's decisions, exact prerequisites and
  file ownership before assigning one bounded task. No commit/push authorization is implied.
- Discover current Paseo profiles; inherit the verified parent workspace. **One writer per checkout**, not one writer
  per file; the user-approved exception is **parallel isolated worktrees** with parent-controlled merges (each
  worktree has one writer; different worktrees may run concurrently). Read-only planning/review may overlap. Fresh
  read-only review after writing stops, then selected fixes and parent CLI gates. Scratch only in `/tmp`; no automatic
  formatting.

### Landed and verified (dated measurements, in this baseline; latest INTEGRATED parent gates on `master` at `342afd12` (129 targets / 2444 / 0 / 1, `nd-db` 1079 / 0 / 0 across 50 targets, `nd-scanner` 224 / 0 across 12 targets); `cb59a973` (100 suites / 2383 / 0 / 1, `nd-scanner` 169 / 0); `6d6938b1` (98 suites / 2363 / 0 / 1, `nd-db` 1073 / 0 / 0 across 49 suites, `nd-http` 1 / 1), `7d022a02` (97 suites / 2343 / 0 / 1, `nd-db` 1053 / 0 / 0 across 48 suites), `632271a9` (96 suites / 2326 / 0 / 1), `de0f49ea` (95 suites / 2319 / 0 / 1), `96c495e7`, `e3e0770a`, `272c9c66`, `f80b9704` and docs-lane totals at `4ab63cd5` are history)


1. **Sort-gate parity — DONE at `62789ba2`.** Executed REST reads in `player_repo.rs`, `library_repo.rs`, `tag_repo.rs`,
   `radio_repo.rs` take `RestQueryOptions` and parse internally, matching Go; `runtime_rest_sort_gate.rs` proves hostile
   sorts are dropped. Audit: `subplans/db-runtime.md` "Executed-read sort gate audit".
2. **DB-01 — DONE at `f57ae050`.** `mark_missing_sql` was a silent no-op (and with numeric-looking ids could corrupt
   the wrong row); args now follow Go's `.Set`/`.Where` order (`mediafile_repository.go:459-461`) and
   `runtime_missing_state.rs` executes 7 real-schema tests plus a pre-fix control (reverting the fix fails 5 of 7).
   Decision record: `decisions.md` W0. G0 now unblocks DB-07/14.
3. **DB-03 / D-OPEN — decided (user: option a), DONE and landed at `4a8aac9f`** (docs `ff900d3e`). Typed
   `DbOpenOptions` + a Go-compatible DSN parser + per-connection PRAGMA statements with effective-state read-back
   before pooling; defaults matching Go (WAL for file DBs, 15000 ms, NORMAL, FK on; `:memory:` keeps the memory
   journal). rusqlite honors real SQLite URI params (`mode=`, `cache=`) but not go-sqlite3's `_journal_mode`/
   `_busy_timeout`/`_foreign_keys`/`synchronous=` conventions. Decision record: `decisions.md` P1. The earlier
   `db.rs::NewSqliteDb` citation was wrong — that symbol does not exist here (Go uses the `db.Db()` singleton,
   `db/db.go:41-68`).
4. **Transcoding executed reads — LANDED at `f8ceeb8f`.** `transcoding_repo::read_all`/`count_all` execute and mask
   `command` for a non-admin; the module is no longer builder-only. Remaining for `D-PLAYER-EDIT`: the BOOT-owned seed
   from `nd-conf` `DEFAULT_TRANSCODINGS` (`consts.rs:242`, 4 entries) and the canonical route.
5. **W4/W6/W7 hydration and executed-read rounds** (`f8ceeb8f`, `62789ba2`) are landed; see the historical index and the
   contract residual blocks for their durable findings.

### Landed since `ff900d3e` (parent-reviewed, signed, merged — each landed as a commit or a merge commit; scopes are deliberately partial)

| Assignment | Commit | Scope actually landed — and what is **not** done |
|---|---|---|
| SC-03 tag→record mapping bridge + singleton folder projection | `cb59a973` (7 files, +4312 additions-only, `nd-scanner`) | **LANDED.** NEW `src/mapping_bridge.rs` (682), NEW `src/projection.rs` (505), NEW `tests/mapping_bridge.rs` (2092), NEW `tests/projection_singleton.rs` (1014), `lib.rs` +3, `Cargo.toml` +13 (`nd-db`/`nd-id`/`nd-tagreader` path deps, parent-authorized, all already locked — lock delta is 3 edge lines, zero new packages). **19 executed tests** vs the committed `go_oracle.json` + real tagreader fixtures; backend gaps pinned both sides (135/136, 124/125, 94/95, 0/30, m4a bitDepth). Checked i32 narrowing (A5: overflow → `BridgeError`), participants via the checked `role_from_id` bridge, genre left empty scan-side, hydration fields zero. **Birth-time P1 ruling** (`local.go:116-128`, `map_mediafile.go:81`, `mediafile_repository.go:184-186`): injected clock, `effective_birth_time` resolved once, album draft reuses it — durable record now in `decisions.md`. Review REQUEST-CHANGES (P1 birth-time + 2 P2) → fixed. Parent gates at `cb59a973`: **2383 / 0 / 1 across 100 suites**, `nd-scanner` **169 / 0**. **Not done:** multi-track album groups **refuse** (`ProjectionError::MultiTrackGroup`) rather than aggregate; no album/media equality (SC-04, scanner-local per `decisions.md` D-SC04); no DB writes; no scanner entrypoint (SC-05). |
| BOOT-00 construction/dependency/launch gate (D1-D3 executed, D4 via B3-ratified) | `6d6938b1` (6 files, +177/−19; `rust/Cargo.toml`, `rust/Cargo.lock`, `nd-http`, `nd-api`, `nd-cli`) | **LANDED** (isolated-worktree lane, parent-merged and re-verified on the merged tree). **D3 manifest pin repair executed:** `chrono "^2"`→`"0.4"` (B1 was decided but never applied), `tracing "^0.5"`→`"0.1"`, `tracing-subscriber "^0.1"`→`"0.3"` (both previously unrecorded and unresolvable), `base64 "^0.8"`→`"0.22"`; **zero NEW lock entries** — the lock diff is only the `nd-http` dependency-edge stanza (axum/nd-auth/nd-conf, all already locked). **D1:** the `nd-http` scaffold became a typed module with the **EXECUTED** `dependency_graph_has_no_http_api_cycle` acceptance (`cargo metadata --locked` graph check + a negative control that fails on an injected `nd-api` edge) and the `RouterInjection = fn(axum::Router, &nd_conf::Config) -> axum::Router` shape; `nd-cli`/`nd-api` rustdoc boundary notes record the `ND_CONFIGFILE` resolution gap (Go `conf/configuration.go:1201`; Rust doc-only at `nd-conf/src/config.rs:390`) and the `Dir::path` `create_dir_all` hazard. **D4** was already resolved by the B3-ratified record; D5 (recipe ownership) stays with the lane report. Parent gates on the merged tree at `6d6938b1`: **2363/0/1 across 98 suites**, `-p nd-db` **1073/0/0** (49 suites), `nd-http` **1/1**, fmt + workspace clippy `-D warnings` clean, `nd-db` rustdoc **21** = post-DB-16 baseline. **Not done:** no production behavior — `main` is still empty, nothing listens or boots; **BOOT-01** owns `ND_CONFIGFILE` resolution, B2 ffmpeg-config injection and the bootstrap sequence. |
| DB-17 playcount/tag/library count refresh execution | `b3dee0a6` (5 files, +2817 additions-only, `nd-db`) | **LANDED.** Executed four caller-tx refresh helpers: `album_repo::refresh_play_counts_in_tx`, `artist_repo::refresh_play_counts_in_tx`, `tag_repo::update_counts_in_tx`, `library_repo::refresh_library_totals_in_tx(id, now)` (8 probes then the 10-bind update; Go discards `RowsAffected` at `library_repository.go:249` and Rust returns the written `RefreshStatsTotals` — documented deviation). Semantics pinned by **20 executed tests** in NEW `tests/runtime_count_refresh.rs`: bare `play_date = excluded.play_date` — **Go's REPLAY backwards-overwrite is faithful, NO monotonic guard added** (executed pin); zero-arg statements; caller-stamped canonical TEXT (no clock in SQL); NULL-exclusion (an empty population writes nothing); statement-level `RowsAffected` vs entity counts; probe-then-update sequencing; swallowed- and propagated-failure both directions with **rollback as the CALLER's obligation via the DB-00 typed seam** (all four helpers' docs say so). Review APPROVE-WITH-FIXES (F1 `run.Parallel` misconception corrected — the production caller already shares one `*dbx.Tx` per `scanner.go:330-346`; F2 rollback docs; F3 two helper-internal failure regressions; F4 three doc inaccuracies). **Not done:** no production caller (the phase-3 `:138`/`:150` and `scanner.go:316`/`:344` triggers are SC's); DB-18/20 surfaces untouched (rustdoc boundary fencing only); DB-21 SigNN freeze unaffected. |
| DB-16 executed move/reference-remap/delete primitives + A8 share rejection + D-PLUGIN deletion | `7d022a02` (10 files, +3507/−308, `nd-db`) | **LANDED.** Executed: `move_matched_mediafile` (scanner move per `phase_2_missing_tracks.go:291-335`, `WithTxRetry` semantics via `try_with_tx_retry_sync`; target keeps its OWN `created_at`/`birth_time` — Go's update arm drops them at `sql_base_repository.go:605/608`, the carried stamp is inert; album reassign + copy ONLY on album-ID change, negative-controlled), `remap_missing_mediafile` (maintenance remap per `core/maintenance.go`, single `try_with_tx` — no retry) composing the full bundle with the old-album zero-track gate, NEW `reassign_references_in_tx` (DB-15 annotation + avg-recalc, executed `reassign_bookmark_in_tx`, then playlist/scrobble/buffer legs in Go order; render-only `reassign_references_sql` untouched), `delete_mediafile_in_tx` (Go bare base-row delete; FK cascades + FTS trigger verified against Go migrations), `delete_all_missing_in_tx` (opt-in; the admin gate is documented as caller policy); warn arms route through the DB-14 tx-loss-as-fatal handler, and `CommittedMoveOutcome.copy_outcome` is `None` on failure (P2 fix — a failed copy is no longer misclassified as `SkippedPoisonedSource`). **A8/D-SHARE:** `share update_sql` rejects empty cols with typed `ShareError::EmptyUpdateColumns` before any SQL text (Go's empty path unreachable in production — `core/share.go:141` discards `cols`); the DB-21 deferred-gate class-2 pin re-executed. **D-PLUGIN deletion executed:** `plugin_repo.rs` (243 lines, 3 tests) + the `lib.rs` export deleted, the class-3 probe replaced with literal SQL, prose reworded, schema goldens intact, zero `plugin_repo` identifiers under `nd-db/src`. NEW `tests/runtime_move_persistence.rs` = **18 executed tests**. Review APPROVE-WITH-FIXES (P2 copy-outcome misclassification fixed in-lane pre-commit; P3 bookmark doc corrected). Parent gates at `7d022a02`: **2343/0/1 across 97 suites**, `-p nd-db` **1053/0/0** (48 suites), fmt + workspace clippy clean, `nd-db` rustdoc **21 = post-DB-16 baseline** (22 − 1 deleted-module link). **Not done:** no production caller (SC-06/BOOT maintenance are the consumers), no GC execution (caller step), no count refresh (DB-17), no playlist cleanup (DB-20). |
| DB-21 **P1-DB exit contract** (DB-side-only) | `632271a9` (1 file, `nd-db`) | **LANDED (P1-DB half only).** NEW `tests/runtime_scanner_persistence_contract.rs` (4206 lines, **7 executed tests**): composed regression through the **real pool** (4 deltas / 2 libraries / 3 album-ID transitions, both `full_scan_mode` values, a second pass over 7 committed dirty-state arms with stored-effect assertions, control pass); frozen API surface (**51 compile-checked SigNN pins** across the landed phase-1 primitives + DB-11/14/15 public surface, Go literals 50/600/200/100, executed unknown-album queue-exclusion sentinel); executed **U-ledger** pinning `SC.mapping-bridge`, `SC.phase1-fixture`, `BOOT.configured-library-caller` (gate **BOOT-01 implementing**, BOOT-00 prerequisite — approval is not delivered caller evidence) and `production_full_finalize_db_contract` as **Unmet**, plus `db_internal_contract` for the `pub(crate)` compile-pin gap; and the executed 5-class inventory behind the **deferred renderer-prepare gate**. Parent decisions recorded in `decisions.md` "DB-21 exit scope and prepared-statement gates" (PARENT DECIDED 2026-11-21). Review REQUEST-CHANGES (6 findings) → APPROVE-WITH-FIXES; parent applied 3 P3 comment fixes. Parent gates at `632271a9`: **2326/0/1 across 96 suites**, `-p nd-db` 1036/0/0 (47 suites), fmt/clippy/`diff --check` clean, `nd-db` rustdoc 22 = baseline. **Not done:** no production caller (SC-05 is the first consumer); no full-exit — the collection/count API for later API-F* still belongs to **DB-21(FULL)**; not #27A progress; stable ID DB-21 incomplete until FULL-SCANNER-DB. |
| DB-14 executed folder-delta persistence (DB-owned SC seam) | `de0f49ea` (4 files, `nd-db`) | **LANDED.** NEW `src/scanner_writes.rs` (935 lines): `FolderWriteBatch` (immutable; SC pre-generated folder/artist/album IDs; caller-stamped `FolderStamps`; `full_scan_mode`; `album_transitions`), `persist_folder_delta(store,&batch) -> Result<CommittedFolderOutcome,FolderRunError>` (DB-owned per-folder retrying tx per D-BATCH), `persist_folder_delta_in_tx` **pub(crate)**, `FolderWriteStage` (12 arms, `is_fatal`), `ScopeRejection` (11 arms, validated pre-checkout), `FolderWriteWarning{stage,item_id,related_id,source}`, `CommittedFolderOutcome` (publish-only-after-commit), `scan_artwork_item`. NEW `tests/runtime_folder_persistence.rs` = 17 executed tests; `lib.rs` +1; `album_repo.rs` +102 additions-only delivering `copy_album_created_at_in_tx` (`album_repo.rs:1566`, Go `album_repository.go:331-357` incl. `0001-`/empty skip — the helper DB-16 reuses). Warn arms route through `warn_arm`, which detects transaction loss via `is_autocommit` and returns the original typed SQLite error as FATAL (never a warning); ABORT-class statement failures still warn+commit; `MediaIdentity` per-record; Go `c == 0` missing-quirk preserved. Review REQUEST-CHANGES then both findings fixed (P1 tx-loss atomicity, P2 consumed-transitions retry proof); parent re-ran all gates at `de0f49ea` (**2319/0/1 across 95 suites**, fmt/clippy clean, `nd-db` rustdoc 22 = baseline). **Not done:** no production caller (SC-05 is the consumer; `nd-scanner` still has no `nd-db` dependency), no walk/tag-mapping/album aggregation (SC-owned), no missing-folder finalize (SC-06). Not move/GC semantics. |
| DB-15 **render-only** rating repair | `ab466928` | `update_avg_rating_sql` embeds `average_rating_expr_sql()` with 3 ordered binds, and `tests/runtime_annotation_rating.rs` executes it against a real schema. The three public annotation renderers were deliberately **kept and redocumented** (their `annotation.id` prepare failure pinned), not left undecided. **Full DB-15 is NOT done:** typed annotation reassignment/merge transactions and the annotation read APIs are unimplemented. |
| Scanner local-FS storage subset | `e9dd8230` + merge `ff5b9c92` | `nd-scanner/src/storage.rs` only: std-`fs` `list`/`stat`/`read`/`read_link`/`resolve`/`exists`, logical-path handling, `file://` URI parsing and explicit symlink classification. At that commit the walker, `.ndignore`/ignore policy, folder hashing, cancellation, `MusicFS`/`ReadTags`, registry dispatch and watcher were **not implemented**; the walker/ignore/hash/cancellation portion landed later at `e3e0770a` (row below). **Not** SC-01 ratification and **not** SC-02 completion. |
| Scanner **SC-02 partial** (local walker, `.ndignore`, folder hash, cancellation) | `e3e0770a` (fast-forwarded onto `2e7026b2`) | **LANDED partial slice.** `nd-scanner`: `contract.rs`/`walk.rs`/`ignore.rs`/`folder.rs` plus the `lib.rs`/`storage.rs` seam, with executed `tests/{folder_hash,ignore_policy,storage_seam,walk_policy}.rs`. The manifest/lock delta is limited to the six user-approved dependencies recorded in `decisions.md` S9 (`b1aeea2c`; false order-divergence claim withdrawn at `2e7026b2`). Review findings through S9b closed; parent gates at `e3e0770a` are 92 suites / 2276 passed / 0 failed / 1 ignored with fmt, workspace clippy and `git diff --check` clean. **Not implemented:** a scanner entrypoint, `MusicFS`/`ReadTags` registry dispatch, tag-to-DB ingest, full-scan orchestration, watcher or progress protocol. **Not** SC-01 ratification, SC-02 completion or MusicFS readiness. The former `rust-sc02-local-walker` worktree and branch are removed. |
| DB-11 executed typed scanner reads, cursor contract, API-safe point read | `52b04064` (7 files, `nd-db`) | **LANDED.** NEW `src/scanner_queries.rs`: row-visitor cursors on one leased connection (`Control`/`VisitOutcome`/`CursorError` three-way stop taxonomy per `D-CURSOR`; `IdChunkedCursor` terminal after a failed chunk), eight executed scanner reads (`media_in_folder`/`media_in_album`/`media_in_filter`, `missing_and_matching`, `recent_candidates`, `touched_albums`, `album_ids_by_folders`, `sole_album_artist_ids_in_subtrees`, `media_by_paths`), opaque scope authorities `ScannerScope` (system, library-ID only) and `UserScope` (resolved once per request: admin/headless(-1)/all-grants omit the clause, otherwise the deny clause `library_id IN (SELECT … user_library …)`; zero-grant and empty-library arms both deny; no `1=1` for users), and the `get_visible_media` point read (thin wrapper over `MediaFileRepo::get`). Review fixes folded in: `folder_subtree_filter` fresh-conjunction root return per Go (`folder_repository.go:214-216`; mixed root+path batches bind/execute), sole-artist subtree builder dropped `album.missing` (parity `album_repository.go:294-296`), `CURSOR_CHUNK_SIZE` 300 → 600 (`D-CURSOR`(4)). NEW `tests/runtime_scanner_queries.rs` = 21 executed tests. **Not done:** full cursor/REST completion (the `SEEDEDRAND` seeded full-set render is deferred to the REST/API lane), no production scanner caller, no `nd-scanner` consumption. `D-CURSOR`/`D-SCOPE` handshakes `SC.query-consumption` and `API.visible-media-contract` are RESOLVED by this diff (typed scope authorities are the only public scope inputs). |
| DB-15 runtime annotation transfers + avg-rating recalc | `96c495e7` (2 files, `nd-db`) | **LANDED runtime half.** `AnnotationEntityKind` closed enum, `AnnotationWriteError`, `update_avg_rating_in_tx` + `reassign_annotation_in_tx` on the caller's tx (DB-00 seam). Go `ReassignAnnotation` parity (`sql_annotations.go:185-197`): the OR IGNORE move keeps the target conflict, leaves the conflicting source row, and recalculates the target average in the same tx. NEW `tests/runtime_annotation_transfers.rs` = 5 executed tests (pre-fix param-count control, conflict identity, type isolation, absent-source no-op recalc, FK-injection rollback with row + cache read-back). **PARENT DECISION (`decisions.md` H6): the durable Go-harness oracle is DEFERRED to the EVID lane** (`subplans/evidence-ops.md`; authorization gate in `subplans/README.md`); Go behavior is pinned by source citations plus `/tmp`-local Go driver evidence (labeled local, not committed). With the `ab466928` render-only repair this covers DB-15's plan scope **except that oracle** — DB-15 is **not** fully DONE beyond that DoD caveat. |
| DB-02 scanner timestamp contract (D-TIME) | `6d9d984d` + merge `74ebd81c` | Scanner-facing stamps/cutoffs bind canonical TEXT via `time::value`; `library_repo::zero_time_value()` replaces raw `ZERO_TIME` integers; artwork-purge integer cutoff fixed with executed coverage. **Partial:** `scrobble_buffer` `play_time`/`enqueue_time` still bind integers, and `find_recent_files_by_properties_sql` has no executed pin. Not scanner writes. |
| DB-00 typed transaction/error seam | `48e65806` + merge `ec84c176` | Additive `try_with_tx`/`try_with_tx_retry` beside `with_tx*`, `DbOperationError`/`DbRuntimeError`, safe rollback cleanup, BUSY-only retry (`decisions.md` E1-E3, H2). The seam only — DB-04 onward consume it, and the DB lane is nowhere near done. |
| Docs consolidation | `f03aff98` + merge `3b5e6c76` | `current-status.md`, `next-work.md`, `decisions.md` and the subplan headers consolidated into one authority chain; historical blocks marked superseded. |
| DB-04 executed property writes | `c791dbc1` | `property_repo::put_in_tx`/`delete_in_tx` on the caller's tx (update-first, insert on zero rows, Go order), 12 executed tests. No connection-bound `PropertyRepo::put`/`delete`, no production caller. |
| DB-05 executed library writes + scan lifecycle | `60be7534` | `library_repo::put_in_tx` (typed `LibraryColumn`, id-1 path guard, autoincrement/predefined-id arms, admin assignment in-tx), `scan_begin_in_tx`/`scan_end_in_tx`/`set_scanned_pid_in_tx`/`add_artist_in_tx`, 8 executed tests. **Not** the bootstrap caller — no code creates a configured library row. |
| DB-06 executed tag Add | `bfdcfeca` | `tag_repo::add_in_tx` — dictionary INSERT then `library_tag` link INSERT per 200-chunk on the caller's tx, 9 executed tests. `update_counts_sql`/`purge_unused_sql` were renderers at that HEAD; `update_counts_sql`'s execution **landed at `b3dee0a6`** (DB-17, `tag_repo::update_counts_in_tx`), `purge_unused_sql` remains a renderer owned by DB-20. |
| DB-07 folder missing-state + album touch | `2911b9ab` + merge `9be6ad0e` | `put_folder_in_tx`, `mark_folders_missing_in_tx`, `mark_media_missing_in_tx`, `mark_media_missing_by_folder_in_tx`, `touch_albums_in_tx`, `touch_albums_with_missing_folders_globally_in_tx` (deliberately GLOBAL, Go predicate retained), 10 executed tests. Not walk/finalize/purge policy. |
| DB-08 artist writes + partial timestamp parity | `32c4fa5b` + merge `320b8b03` | `artist_repo::put_artist_in_tx` with typed `ArtistColumn`; **fixed** the renderer defect that appended `updated_at` to every filtered SET (Go `selectUpdateColumns` semantics restored and re-pinned in `runtime_timestamp_convention.rs`). `library_artist` link stays DB-05's `add_artist_in_tx`. 10 executed tests. |
| DB-09 atomic album base/participants/tags | `d938d159` + merge `842c32a2` | `album_repo::put_album_in_tx`: base upsert, unconditional participant delete + optional insert, unconditional tag delete + optional insert, one tx. FTS is maintained by the schema's own `album_fts_*` triggers, not by an issued statement — the tests observe it via `MATCH`/row count. 18 executed tests. Album aggregation/`equals` remain SC-owned (`decisions.md` S4). |
| BOOT-02 native expiring login sessions | `c6e220eb` + merge `162fae1d` | Login mints via `create_native_session_token` (native audience + checked positive `exp`), so a login token passes `AuthGate::Api`; executed `tests/login_http.rs`/`tests/http_auth.rs` coverage incl. an epoch-revoke round-trip and an audience-less negative control. **Not** HTTP API-key extraction/issuance middleware; no production router. |
| DB-13 epoch-only token revocation | `eae4ba1d` + merge `4ab63cd5` | `increment_token_epoch_sql` (guarded `typeof(token_epoch)='integer'` and `< i64::MAX`, `RETURNING token_epoch`) executed by `revoke_tokens_in_tx` on the caller's tx; `RevokeError::NotFound`/`Exhausted` classified by a same-tx existence read; 18 executed tests. Epoch-only revoke is a Rust-side addition (Go bumps only inside the password UPDATE); no BOOT/API consumer exists. |

### Live wave (verified checkout state; never mark DONE from a message)

At `cb59a973` (this branch line: SC-03 `cb59a973`, BOOT-00 `6d6938b1`, DB-17 `b3dee0a6`, DB-16 `7d022a02`, docs sync `149403e3`, DB-14 `de0f49ea`,
DB-21(P1-DB) `632271a9`, the `nd-stream` causal-wait fix `7369daf4`, A8/B3/D-PLUGIN decision records `2b3fa921`,
the SC-01/BOOT-00 docs record `f3bb81b2`, DB-11 `52b04064` and DB-15 runtime `96c495e7` on top of integrated
`master` code HEAD `e3e0770a`), **DB-10, the DB-12 executed subset, DB-22, the SC-02 partial, SC-03, DB-11, DB-15's runtime half,
DB-14, DB-21(P1-DB), DB-16, DB-17 and BOOT-00 are LANDED**. **SC-05 is LANDED AND INTEGRATED — `5c463aa1`, fast-forwarded into
`master` at `342afd12` and pushed** — see the row below.
Verified `git worktree list` and branch refs 2026-11-24: `master` and this worktree share the same tip (**`6a3f0af7`**; the
integrated SC-05 code boundary is `342afd12`, the commits above it are docs-only), `master` fast-forwarded from `2b3fa921` with
the old tip recoverable there, `origin/master` pushed and in sync, and
`resuscitate-dead-agent-capture-worktree-state` is now identical to `master` (0 ahead / 0 behind) — a spent branch whose deletion
is the user's call, since nothing on it is unique. The former DB-10/DB-12 and `rust-sc02-local-walker`
worktrees/branches are absent (parent-controlled merges, then cleanup). **Sibling cleanup executed 2026-11-24:**
`~/.paseo/worktrees/0xo97qr3/boot00` (branch `boot00-manifest-pins-scaffold`, `6d6938b1`) was **archived at the user's request**
via `paseo workspace archive wks_929b07163c95a811`: Paseo removed the owned worktree and archived its agent; the branch is kept
locally (it was never pushed to `origin`) and is an ancestor of `master`, so nothing is lost. `git worktree prune -n` reports nothing stale.

| Assignment | Commit / branch | Verified state |
|---|---|---|
| DB-10 MediaFile `PutByMatch` identity + atomic maintenance | `65072b89`, signed merge `6e5ca97d` | **LANDED**. `put_mediafile_in_tx` plus 31 executed tests; review APPROVE and follow-up P3s fixed (parent report). Empty-ID Match miss mints inside each tx attempt; retries may mint different GUIDs; failed attempts publish no result; only committed result returns ID. BUSY test fails initial INSERT before junction/FTS, not commit failure after a completed bundle. No production scanner caller. |
| DB-12 scanner artwork invalidation/enqueue subset | `e5d0592d` (signed, fast-forward), rustdoc fix `f80b9704` | **LANDED subset**. Transaction-bound delete-for-items and three enqueue variants, 14 executed tests; review APPROVE after follow-up test fixes (parent report). No production scanner caller, artwork worker or full scanner. |
| SC-02 walker/ignore/folder hash remainder | `e3e0770a` (was `rust-sc02-local-walker`, worktree and branch now removed) | **LANDED as a partial slice.** Fast-forwarded onto `2e7026b2` after rebase from base `60be7534`; parent gates at that HEAD: 92 suites / 2276 passed / 0 failed / 1 ignored, fmt, workspace clippy and `git diff --check` clean, `nd-scanner` rustdoc 4 pre-existing warnings. Review findings through S9b closed (`decisions.md` S7/S8/S9/S9b; `b1aeea2c`, `2e7026b2`). **Partial**: no scanner entrypoint, `MusicFS`/`ReadTags` dispatch, tag-to-DB ingest, full-scan orchestration or watcher — not SC-01 ratification, SC-02 completion or MusicFS readiness. Historical note below. |
| DB-11 executed typed scanner reads, cursor contract, API-safe point read | `52b04064` (7 files, `nd-db`) | **LANDED** on this branch line (`master` is its ancestor; integration/merge is the parent's). NEW `src/scanner_queries.rs` + `tests/runtime_scanner_queries.rs` (21 executed tests) with additive scoped variants in `mediafile_repo.rs`/`album_repo.rs`/`folder_repo.rs`/`artist_repo.rs` and the `lib.rs` export; review fixes folded in (`folder_subtree_filter` fresh-conjunction root per `folder_repository.go:214-216`, sole-artist subtree drops `album.missing` per `album_repository.go:294-296`, `CURSOR_CHUNK_SIZE` 300 → 600). Parent gate at that HEAD: 2297 / 0 / 1 (dated history). **Not done:** full cursor/REST completion (`SEEDEDRAND` seeded full-set render deferred to the REST/API lane), no production scanner caller, no `nd-scanner` consumption. |
| DB-15 runtime annotation transfers + avg-rating recalc | `96c495e7` (2 files, `nd-db`) | **LANDED runtime half.** `AnnotationEntityKind`, `AnnotationWriteError`, `update_avg_rating_in_tx`, `reassign_annotation_in_tx` on the caller's tx; `tests/runtime_annotation_transfers.rs` (5 executed tests). The durable Go-harness oracle is **deferred to the EVID lane** (PARENT DECIDED, `decisions.md` H6). DB-15 is **not** fully DONE; no annotation service/route caller exists. |
| DB-16 executed move/reference-remap/delete + A8 share rejection + D-PLUGIN deletion | `7d022a02` (10 files, +3507/−308, `nd-db`) | **LANDED.** `move_matched_mediafile` (`WithTxRetry` semantics via `try_with_tx_retry_sync`), `remap_missing_mediafile` (single `try_with_tx` — no retry), NEW `reassign_references_in_tx` (DB-15 annotation + avg-recalc, executed bookmark leg, playlist/scrobble/buffer legs in Go order), `delete_mediafile_in_tx`, `delete_all_missing_in_tx` (opt-in; admin gate is caller policy); NEW `tests/runtime_move_persistence.rs` = 18 executed tests. A8 `ShareError::EmptyUpdateColumns` before any SQL text; D-PLUGIN `plugin_repo.rs` + `lib.rs` export deleted (class-3 probe now literal SQL). Review APPROVE-WITH-FIXES (P2 copy-outcome fixed in-lane; P3 bookmark doc). Parent gates: **2343/0/1 across 97 suites**, `nd-db` **1053/0/0** (48 suites), fmt/clippy clean, `nd-db` rustdoc **21** = post-DB-16 baseline. **Not done:** no production caller (SC-06/BOOT maintenance), no GC execution, no count refresh (DB-17), no playlist cleanup (DB-20). |
| DB-17 playcount/tag/library count refresh | `b3dee0a6` (5 files, +2817 additions-only, `nd-db`) | **LANDED.** `album_repo::refresh_play_counts_in_tx`, `artist_repo::refresh_play_counts_in_tx`, `tag_repo::update_counts_in_tx`, `library_repo::refresh_library_totals_in_tx(id, now)` (8 probes + 10-bind update; Go's `RowsAffected` discarded at `library_repository.go:249`, Rust returns the written `RefreshStatsTotals` — documented deviation); NEW `tests/runtime_count_refresh.rs` = 20 executed tests (faithful REPLAY backwards-overwrite with no monotonic guard, zero-arg statements, caller-stamped canonical TEXT, NULL-exclusion, statement-level `RowsAffected` vs entity counts, probe-then-update order, failure both directions with rollback as the CALLER's obligation via the DB-00 seam). Review APPROVE-WITH-FIXES (F1/F2/F3/F4). Parent gates on the merged tree at `6d6938b1`: **2363/0/1 across 98 suites**, `nd-db` **1073/0/0** (49 suites), fmt/clippy clean, `nd-db` rustdoc **21** = post-DB-16 baseline. **Not done:** no production caller (SC's phase-3 `:138`/`:150` and `scanner.go:316`/`:344` triggers), DB-18/20 surfaces untouched, DB-21 SigNN freeze unaffected. |
| BOOT-00 construction/dependency/launch gate | `6d6938b1` (6 files, +177/−19; was branch `boot00-manifest-pins-scaffold` in `~/.paseo/worktrees/0xo97qr3/boot00`, parent-merged) | **LANDED AND INTEGRATED** (gate work only, re-verified on the merged tree; now on `master` at `342afd12`). D3 pins repaired (`chrono 0.4`, `tracing 0.1`, `tracing-subscriber 0.3`, `base64 0.22`; zero new lock entries — lock diff is only the `nd-http` dependency-edge stanza); D1 `nd-http` typed boundary with the EXECUTED `dependency_graph_has_no_http_api_cycle` + `RouterInjection` shape; D4 via B3-ratified. `nd-cli`/`nd-api` rustdoc record the `ND_CONFIGFILE` gap and the `Dir::path` hazard for BOOT-01. **No production behavior:** `main` empty, nothing listens. The sibling `boot00` worktree was **archived at the user's request on 2026-11-24** (`paseo workspace archive wks_929b07163c95a811`); the branch `boot00-manifest-pins-scaffold` at `6d6938b1` is kept locally (never pushed to `origin`) and is an ancestor of `master`. |
| SC-05 phase-1 vertical (walk → tag → bridge → batch → DB-14 persist) | `5c463aa1` (10 files, +9103/−54: NEW `nd-scanner/src/phase1.rs`, NEW `tests/{phase1_slice,phase1_faults,phase1_fixture}.rs` + `tests/common/mod.rs`, modified `src/{lib.rs,walk.rs,projection.rs}`, `nd-scanner/Cargo.toml`, `rust/Cargo.lock`) on branch `resuscitate-dead-agent-capture-worktree-state`, **pushed to origin**, signed, and **fast-forwarded into `master` at `342afd12` (2026-11-24, user-approved)** | **LANDED AND INTEGRATED — accepted by the parent and merged into `master`** (`master` `2b3fa921 → 342afd12`, old tip recoverable there, `origin/master` in sync). Cargo.lock carries two dependency edges only per `decisions.md` D-SC05-MANIFEST with **zero** new `[[package]]` entries (`git show 5c463aa1 -- rust/Cargo.lock` adds no `[[package]]` line). It is the **first caller** of DB-11's `visit_media_in_folder` (`phase1.rs:568`) and DB-14's `persist_folder_delta` (`scan_folder` `:1740`, `capture_selected_folder` `:694`, `ingest_folder_snapshot` `:1566`). Parent gates re-measured **after** the commit (cargo/rustc 1.99.1), and again **on the integrated main checkout** after the fast-forward at `342afd12`: `cargo test --workspace --locked` **exit 0** = **2444 / 0 / 1 across 129 targets** (103 binaries 2438 + 26 doctests 6), `-p nd-db` **1079 / 0 / 0 across 50 targets**, `nd-scanner` **224 / 0 across 12 targets** (lib 29, `walk_policy` 45, `local_storage` 34, `phase1_fixture` 19, `ignore_policy` 24, `folder_hash` 17, `mapping_bridge` 16, `phase1_faults` 15, `phase1_slice` 12, `storage_seam` 9, `projection_singleton` 4, doctests 0), fmt + workspace clippy `--all-targets --all-features -D warnings` + `git diff --check` clean. **Rustdoc: the lane's own warnings are ZERO** — `cargo doc --no-deps -p nd-scanner` exits **0** with **4 warnings, all pre-existing**; `-D warnings` fails identically at integrated `cb59a973`, so that baseline gate is **not** SC-05 scope (`decisions.md` D-SC05-REVIEW item i). The `nd-stream` `early_release_defeats_the_cap_and_accumulates_processes` teardown flake is pre-existing and not this lane's (`nd-stream` 0 porcelain entries). Fresh read-only review `c5ead1e6-92fa-4b18-b271-14d7bfce283a` (2026-11-23): **APPROVE-WITH-FIXES, no High findings**; its four selected findings are FIXED in `5c463aa1` — the wrong Go line citations in `listing_failure_warning` (doc-only, fatal class now labeled a declared divergence), the `annotation`/`album.average_rating` snapshot blind spot, the duplicate writable-table lists + overstated injection-matrix doc, and the unreachable `unwrap_or_else` fallback at `phase1.rs:954`. Declined dispositions and remaining open items are recorded in `decisions.md` D-SC05-REVIEW and `current-status.md` (annotation parity not driven end-to-end; non-UTF-8 arm Linux-only; `Hash`/`Bridge` arm-level only; captured-fact folder hash unpinned against a Go-computed hash; fatal listing class has no end-to-end test; the latent non-UTF-8 **ancestor** refusal recorded in D-SC05-PATH is still unfixed and is an SC-02/SC-05 boundary item, not silently patched here). SC-05 acceptance items stay open by design (multi-track aggregation = SC-04, outdated/empty-skip, image-changes collector, missing-folder finalizer — D-SC05-SLICE), so **`SC.phase1-fixture` stays U** even after the merge — its "no scanner entrypoint" clause is still true. **Next actions: (1) DONE 2026-11-24 — the branch was fast-forwarded into `master` at `342afd12` and pushed (`decisions.md` D-SC05-MERGE); (2) DONE 2026-11-24 — the stale `SC.phase1-fixture` / `SC.mapping-bridge` / `BOOT.configured-library-caller` gate text in `nd-db/tests/runtime_scanner_persistence_contract.rs:584-658` was reworded by the parent with statuses left at U, and the two assertions that pinned satisfied clauses were corrected (`decisions.md` D-SC05-GATEWORD; workspace gate re-measured at 2444 / 0 / 1 across 129 targets, identical to baseline). No SC-06 authorization exists. |
| Wave-2 docs refresh | `rust-wave2-status-refresh`, base `320b8b03`; no worktree | 0 ahead / 8 behind `master`. Stale docs attempt, no recovered output; superseded by this refresh. |

Merges are parent-controlled; this docs lane is support work, not a rebalance of implementation lanes. Historical record:
before the merge the SC-02 remainder sat uncommitted on `rust-sc02-local-walker` (base `60be7534`, 15 behind `master`) with
the `/tmp/nav-sc02-final-review.md` request-changes verdict on that uncommitted diff — an unmerged-branch review, not an
integrated-tree one. That round and the subsequent S8/S9/S9b dispositions are recorded in `decisions.md`; the slice landed
at `e3e0770a`.

### Next dependencies (order, not estimates)

1. **DB-11 LANDED → DB-15 runtime LANDED → DB-14 LANDED `de0f49ea` → DB-21(P1-DB) LANDED `632271a9` → DB-16 LANDED `7d022a02` → DB-17 LANDED `b3dee0a6` → next DB-18 (pending `D-STATS`)** — DB-04…DB-10/DB-12/DB-13 are
   landed and the spine's last five packages are in: **DB-11 LANDED `52b04064`** (typed filtered scanner reads, D-CURSOR cursor
   contract, `ScannerScope`/`UserScope`, `get_visible_media`), **DB-15's runtime half LANDED `96c495e7`** (annotation transfer +
   average-rating recalc, durable Go oracle deferred to the EVID lane — `decisions.md` H6) and **DB-14 LANDED `de0f49ea`**
   (`SC.folder-write-batch-contract` resolved as **D-BATCH** and consumed: `FolderWriteBatch`, `persist_folder_delta`,
   `persist_folder_delta_in_tx` `pub(crate)`, warn-vs-fatal table, SC pre-generated IDs, and the parent-controlled
   `album_repo.rs` copy helper — landed as `copy_album_created_at_in_tx` at `album_repo.rs:1566`, which DB-16 reuses; 17 executed
   tests; review findings P1/P2 closed and the parent re-ran all gates), and **DB-21(P1-DB) LANDED `632271a9`** (1 file: NEW
   `tests/runtime_scanner_persistence_contract.rs` 4206 lines, 7 executed tests — the **DB-side-only P1 exit** per the parent scope
   narrowing of 2026-11-21, `decisions.md` "DB-21 exit scope and prepared-statement gates": composed regression through the real
   pool, frozen API surface (51 SigNN pins), and an executed U-ledger pinning `SC.mapping-bridge`, `SC.phase1-fixture`,
   `BOOT.configured-library-caller` and `production_full_finalize_db_contract` as Unmet; **not done** — no production caller,
   no full-exit, collection/count still belongs to DB-21(FULL), not #27A progress). `D-CURSOR`/`D-SCOPE`
   were recorded in `decisions.md` (PARENT DECIDED 2026-11-20, corrected at `8b5b3925`) and are **consumed by executed code**.
   **Next is DB-18**, on the forward spine `DB-18 → 19 → 20 → DB-21(FULL-SCANNER-DB)`; **DB-17 LANDED `b3dee0a6`**
   (four caller-tx count-refresh helpers — album/artist playcounts, tag `update_counts_in_tx`, library 8-probe + 10-bind
   `refresh_library_totals_in_tx`; 20 executed tests; faithful REPLAY backwards-overwrite with no monotonic guard; rollback stays
   the CALLER's obligation via the DB-00 seam; no production caller — SC's phase-3 `:138`/`:150` and `scanner.go:316`/`:344`
   triggers are the consumers) and **DB-16 LANDED `7d022a02`**
   (executed move/reference-remap/delete primitives, A8 share typed rejection, D-PLUGIN deletion executed, 18 executed tests,
   no production caller — SC-06/BOOT maintenance are the consumers). **DB-18 is gated on `D-STATS`, which still has no canonical
   `decisions.md` record**, and DB-20 retains `D-GC`; DB-19 needs its boundary approval. DB-12 has already released the artwork subset.
   **DB-22 LANDED `272c9c66`** (executed genre/scrobble/playqueue/artwork reads; C5 documents the narrow boundary).
   DB-18…20 (artist stats,
   playlist cleanup, GC) extend the phase-2+/full-scanner exit; DB-16 (move/reference) and DB-17 (tag/library counts) are in. Each lane consumes the landed
   DB-00 typed transaction/error seam, the DB-02 canonical TEXT stamp convention, and the DB-03 effective pool
   configuration — adopt them rather than re-deriving helper behavior. Add executed write→read-back regressions, not more
   renderer-only assertions (`decisions.md` W0/W1); the DB-02 gaps (`scrobble_buffer` integer stamps,
   `find_recent_files_by_properties_sql` executed pin) belong to whichever lane owns those statements. Landed lanes also
   still need **production callers on an integrated tree**. SC-05 closed one gap: DB-14's folder-delta seam and DB-11's read seam
   now have **integrated callers inside `nd-scanner`** on `master` (`phase1.rs:568`, and `persist_folder_delta` reached from
   `scan_folder`/`capture_selected_folder`/`ingest_folder_snapshot`), but `phase1::scan_folder` itself has **no production caller**
   — only the test harness `nd-scanner/tests/common/mod.rs` — and `nd-cli/src/main.rs` is still an empty `fn main()`, so nothing
   bootstraps a scan. DB-17's refresh helpers are landed
   but uncalled (SC's phase-3 triggers), and no executed write or scanner read is invoked from an integrated
   bootstrap or route yet.
2. **Re-date the gate block** after each merge; `current-status.md` records the latest **integrated** parent gates on `master` at
   **`342afd12`** (`cargo test --workspace --locked` exit 0 = **2444 / 0 / 1 across 129 targets**, `-p nd-db` **1079 / 0 / 0 across
   50 targets**, `nd-scanner` **224 / 0 across 12 targets**, `cargo doc --no-deps -p nd-scanner` **exit 0 with 4 warnings, all
   pre-existing**), re-measured by the parent on the main checkout after the fast-forward. The previous integrated block at
   `cb59a973` (**2383 / 0 / 1 across 100 suites**, `nd-scanner` 169 / 0) is dated history.
   Earlier figures on this branch line — **2431 / 0 / 1 across 103 suites** and **2436 / 0 / 1 across 129 targets** with
   `nd-scanner` 217 / 0 across 11 suites and 12 rustdoc warnings (8 from the lane) — are the same lane at earlier states and
   are superseded by the review fix pass and by `5c463aa1`; `rustdoc -D warnings` failing on 4 pre-existing links is a
   **baseline** gate owned by
   a future docs/gate micro-lane, not SC-05 scope (`decisions.md` D-SC05-REVIEW). Earlier merged-tree blocks
   at `6d6938b1`
   (this machine's toolchain: **Homebrew cargo/rustc 1.99.1**, `/opt/homebrew/bin`, EXECUTED on the merged tree): workspace tests
   **2363 / 0 / 1 across 98 suites** (the single ignore is the pre-existing `dump_divergences`), `-p nd-db` all-targets
   **1073 / 0 / 0 across 49 suites** and `nd-http` **1 / 1** (the BOOT-00 cycle test), with `cargo fmt --all -- --check`, workspace clippy
   (`--all-targets --all-features -- -D warnings`) and `git diff --check` clean, and `nd-db` rustdoc at **21 warnings =
   post-DB-16 baseline** (22 − 1 deleted-module link from the D-PLUGIN deletion). Arithmetic: `2343` at `7d022a02` `+ 20`
   (DB-17's suite) `+ 0` (BOOT-00: `+1` cycle test `− 1` dropped `nd-http` scaffold test) `= 2363`; suites `97 → 98`.
   The block at `7d022a02` (**2343 / 0 / 1 across 97 suites**, `-p nd-db` 1053/0/0 across 48 suites), the block at `632271a9` (**2326 / 0 / 1 across
   96 suites**, `-p nd-db` 1036/0/0 across 47 suites, rustdoc 22 = baseline), the block at `de0f49ea` (**2319 / 0 / 1 across 95 suites**),
   the block at `b213755f` (**2317 / 0 / 1** — the docs-only sync commit on top of
   `96c495e7` before DB-14's suites were added — superseded by `de0f49ea`), the block at `96c495e7` (**2302 / 0 / 1 across 94 suites**),
   the intermediate block at `52b04064` (**2297 / 0 / 1**), the prior
   blocks at `e3e0770a` (92 suites / 2276 / 0 / 1), `272c9c66` (88 suites / 2171 / 0 / 1) and `f80b9704` (87 suites /
   2159 / 0 / 1, logs `/tmp/nav-db10-integrated-{clippy,tests,doc}.log`) remain dated history, not proof for a later diff.
3. **Honest scanner phase-1 vertical slice** — storage/stat/symlink/ignore landed (`e9dd8230`/`ff5b9c92`, then the
   SC-02 partial walker/ignore/hash/cancellation slice at `e3e0770a`, then the **SC-03 metadata→record bridge and
   singleton projection at `cb59a973`** (19 executed tests). The **metadata→record adapter is landed**; what remains is
   pure album aggregation/change detection (SC-04 — **INTEGRATED into `master` at `eaff4c11`**, code `c0b2952e`+`7c92e4cf`, scanner-local per
   `decisions.md` D-SC04; multi-track groups now **aggregate** and `MultiTrackGroup` is retired; **integrated into `master`
   by fast-forward to `eaff4c11`** per `decisions.md` D-SC04-MERGE), the fixture-tree ingest over the DB APIs (SC-05 — **integrated at `342afd12`**, its slice landed with its own
   acceptance items open), and SC-06's phase-1 completion. Call it phase 1, not complete #17: on the integrated tree there is still
   no scanner entrypoint (only a test harness calls `phase1::scan_folder`) and no production tag-to-DB pipeline.

   library/folder IDs, pure album aggregation/change detection, and one fixture-tree ingest over those DB APIs
   (`decisions.md` S1-S4). Call it phase 1, not complete #17: there is still no scanner entrypoint or tag-to-DB pipeline.
4. **Bootstrap/HTTP/canonical route contracts** — **BOOT-00 LANDED `6d6938b1`** (D3 manifest pins repaired with zero new lock
   entries; D1 `nd-http` typed boundary + executed `dependency_graph_has_no_http_api_cycle` + the `RouterInjection` shape; D4 via
   B3-ratified; **no production behavior — `main` is still empty and nothing listens**), so **BOOT-01** is the next launchable boot
   package and owns the `ND_CONFIGFILE` resolution gap (`nd-conf/src/config.rs:390`, Go `conf/configuration.go:1201`) and the B2
   ffmpeg-config injection. Then scoped API manifest capture (API-00, no longer blocked on BOOT-00), configuration → schema/pool →
   configured library/admin → router/listener wiring, HTTP API-key extraction/issuance middleware (login-session/native-audience is
   landed at BOOT-02 `c6e220eb`/`162fae1d`), stream
   prepare/header/Send/body-ownership seams. Contract capture and read-only investigation may proceed before ingest;
   implementation still has one writer per checkout.
5. **First runnable slice (#27A)** — fresh DB → scan a real track → login → authorized canonical lookup → direct Range
   and forced transcode → real socket disconnect with zero live children and recovered concurrency slots; require
   ffmpeg/codecs and actually exercise the paths. Full scanner/watchers, full #27, repair/full maintenance and
   deferrable metrics are **not** automatic gates for this slice.
6. **Complete scanner and DB consumers** — phase-2 move/missing identity and annotation/reference preservation, phase-3
   aggregates/stats, phase-4 playlist service, scoped eleven-operation GC/finalize, controller/progress/restart/
   cancellation, then watcher/progress-protocol parity.
7. **Consumer-gated residuals and later services** — durable g1/g2 Go evidence, sibling first-error reproductions,
   genre/options/participant/cursor/smart-playlist/track query issues, schema-golden re-derivation, FTS repair and
   maintenance/logging, full artwork/share/scrobbler/external/lyrics/playlists/player-session/API/share-HTML expansion.
   Per-device editing needs only the BOOT seed plus the route now that the transcoding read executes.

Executed reads landed for `genre`, `scrobble` and `playqueue` at DB-22 `272c9c66`, artwork item-image lookup and
invalidation execute, and DB-11 `52b04064` added `scanner_queries.rs` — eight executed scanner reads plus the
`get_visible_media` point read on a leased connection, outside the 23-module `*_repo.rs` census and outside the shared
`execution` helper; the render-only set no longer includes `plugin` (D-PLUGIN deletion **EXECUTED at `7d022a02`** — the file
and its renderers are gone) and
`smart_playlist` (not a separate Go repository). `share` and `user_props` execute only on **raw connections**, outside the
shared `execution` helper. `annotation_repo.rs` does not exist because annotations ride album/media-file, where DB-15's
tx-bound transfers now execute at `96c495e7`, so that is not a gap. Non-DB follow-up: the `nd-stream` teardown flake
(causal-wait fix **LANDED `7369daf4`**, 2026-11-21 — still tracked as hardened, record corrected 2026-11-20, `current-status.md`).

### Decision ask (scope): does `plugin_repo.rs` belong in the port at all?

`schema-contract.md` says the `plugin` table is **dropped** — Part C item 14 removes `/api/plugin*` and does not port
`persistence/plugin_cleanup.go`, so `db/migrations/20260106000620_create_plugin_table.sql` is not ported.
`plugin_repo.rs` was ported anyway (its header notes the table is absent yet claims the schema slice will add it) and is
exported publicly (`lib.rs`), so callers can invoke its renderers — but every statement it renders fails with
`no such table: plugin` against the shipped schema, including the probed path (`DELETE FROM plugin WHERE id = ?`).
**DECIDED (D-PLUGIN, `decisions.md`, PARENT DECIDED 2026-11-20):** option (a) — delete `plugin_repo.rs`, its tests, and
the `lib.rs` export as out-of-scope dead code; do not port `persistence/plugin_cleanup.go` (no `plugin` table in the shipped
schema, so Go's cleanup updates have no referent and user/library delete simply drops that step). **EXECUTED at `7d022a02`
(DB-16, 2026-11-21):** the file (243 lines, 3 tests) and the export are deleted, the DB-21 class-3 probe is literal SQL, prose
mentions are reworded, schema goldens are intact, and zero `plugin_repo` identifiers remain under `nd-db/src`. Evidence: audit `94589a34` + parent
re-verification (`schema_ddl.rs` has no `plugin` table; `lib.rs:35` export and a `time.rs` doc reference are the only mentions
outside the file; `plugin_cleanup.go:8,50` updates the `plugin` table from user/library delete).

### Revised milestones and dependencies

1. **DB safety and configuration gates before enabling writes** — **all four DONE**: bind order (DB-01 `f57ae050`),
   canonical TEXT timestamps + D-TIME (DB-02 `6d9d984d`/`74ebd81c`), effective pool configuration (DB-03 `4a8aac9f`), and
   typed error/transaction composition (DB-00 `48e65806`/`ec84c176`). Existing helpers still carry latent defects — the
   scrobble-buffer integer stamps and the un-pinned `find_recent_files_by_properties_sql` cutoff among them (DB-08 fixed
   one of them, the force-appended `updated_at`, at `32c4fa5b`) — so do not connect renderers unchanged.
2. **Bounded DB runtime packages** — DB-04 property, DB-05 library + scan lifecycle, DB-06 tag, DB-07 folder/missing/touch,
   DB-08 artist, DB-09 album and DB-13 epoch revoke are **landed**; DB-10/12 are now landed too, as is DB-22 `272c9c66`,
   **DB-11 `52b04064`** (`D-CURSOR`/`D-SCOPE`, recorded at `8b5b3925`, now consumed by executed code) and **DB-15's runtime
   half `96c495e7`**, **DB-14 `de0f49ea`** (D-BATCH consumed), **DB-21(P1-DB) `632271a9`** (DB-side-only P1 exit contract)
   and **DB-16 `7d022a02`** (executed move/reference-remap/delete, A8 share rejection, D-PLUGIN deletion) and
   **DB-17 `b3dee0a6`** (four caller-tx count-refresh helpers, 20 executed tests); **BOOT-00 LANDED `6d6938b1`** on the boot track;
   the spine's next package is **DB-18**,
   gated on `D-STATS` and not picked by the docs lane (DB-16 landed and reused the landed
   `album_repo::copy_album_created_at_in_tx`; DB-21(FULL-SCANNER-DB) needs SC/BOOT consumers plus DB-18..20). Library
   bootstrap policy belongs to the bootstrap caller, DB supplies typed executed operations. Test rollback, retry, immutable
   fields, non-target rows, empty/chunk boundaries, and actual read-back through production methods.
3. **Honest scanner phase-1 vertical slice** — as above; the old "8 repos + 1 mapper" is a core-read inventory, not a
   completion gate.
4. **Bootstrap/HTTP/canonical route contracts** — as above; do not weaken auth or buffer an entire transcode merely to
   adapt existing APIs.
5. **First runnable slice (#27A)** — as above.
6. **Complete scanner and DB consumers** — as above; Go gob carries scanner `ProgressInfo`, not tag-reader results, and
   initial in-process ingest does not require mixed Go/Rust IPC.
7. **Consumer-gated residuals and later services** — as above; complex service tails remain specification-blocked until
   their own contracts are ready, and cancelled product surfaces stay cancelled.

Milestones 3-5 form the minimal vertical path; milestone 6 is needed for a complete #17 claim, not every first-playback
test. `subplans/README.md` resolves exact task IDs, cross-plan prerequisite aliases, decision gates, file collisions,
and READY/BLOCKED status before launch. Do not revive the old dependency cycles or make metrics a first-boot
prerequisite.

### Residual pointers (durable homes, not restated here)

- Query-shape residuals (genre JOIN/filtering, smart-playlist/playlist-track OR grouping, alias hazards, participant
  re-fetch, lazy cursors, sibling error precedence, the bare-`OR` fragment list) live in `hydrate-contract.md` and
  `hydrate-scan-contract.md` residual blocks; they must close **before their consumers are exposed** and are not
  accepted behavior. Accepted deviations are enumerated in `decisions.md` A1-A7 and stay distinct from deferred bugs.
- Go harness evidence for the hydration lanes exists locally but is **not clone-reproducible tracked fixtures**;
  durable captures belong to the EVID lane (`subplans/evidence-ops.md`), with the authorization gate in
  `subplans/README.md`. Never derive Go expectations from Rust; never commit scratch binaries/DBs.
- Repair, full maintenance/logging, complete API/services, HTML shares and metrics remain separate scopes and are not
  first-track/first-boot prerequisites. First playback uses the canonical API, never resurrected `/rest` routes.

---

## Historical index and compatibility pointers (dated records, superseded)

Everything after this line is a dated record. Old models, workspace paths, lane status, test counts and "remaining"
assertions must not override the current plan. Named headings below are kept because Rust sources, test comments, and
oracle READMEs cite them; **numeric line references in those sources are stale by design after this curation** and are
listed for the parent to repair deliberately (this lane does not edit Rust sources).

### Gate totals (dated boundaries, EXECUTED by the parent at the time)

| Boundary | Workspace total | Notes |
|---|---|---|
| Phase-4 close (2026-11-04) | 969 passed / 0 failed | historical boundary, not green state |
| `a7afda2a` (2026-11-11, timestamp convention) | 1442 → 1453 | 52-53 binaries |
| S-D2a/S-D2b1/S-D2b2 | 1536 / 1550 / 1558 | marshal + records rounds |
| S-D3b follow-up | 1665 / 1 ignored, 63 binaries | |
| S-D3c round | 1691 / 1 ignored, 64 binaries | |
| S-D3d fix rounds | 1719 → 1720 | |
| S-D3e | 1746 → **1747** (arithmetic corrected in S-D3f; `1079 + 668`) | |
| S-D3f | 1776 / 1 ignored, 67 binaries | |
| `f8ceeb8f` (W4+W6) | 1776 → 765 DB | transcoding executed reads |
| `62789ba2` (W7 sort gate) | 771 DB / 1850 workspace | |
| `f57ae050` (DB-01) | 778 DB / 1857 workspace | |
| `4a8aac9f`/`ff900d3e` (DB-03 + docs) | 813 DB / 1938 workspace, 74 suites | superseded boundary |
| `ab466928` (DB-15 render-only) | 819 DB, 31 suites | `824` = `819 + 5` doctests, a different axis |
| `ec84c176` (DB-00 merge) | 834 DB / 2002 workspace, 77 suites | superseded boundary |
| `74ebd81c` (DB-02 merge) | 841 DB across 33 suites / 2009 workspace across 78 suites | superseded boundary |
| `c791dbc1`/`60be7534`/`bfdcfeca` (DB-04/05/06) | 2021 / 2029 / 2038 workspace, 79/80/81 suites | parent integration logs |
| `9be6ad0e`/`320b8b03`/`842c32a2` (DB-07/08/09 merges) | 2065 / 2078 / 2096 workspace, 82/83/84 suites | parent integration logs |
| `162fae1d` (BOOT-02 merge) | 2055 workspace across 81 suites (auth 107) | parent integration log |
| `4ab63cd5` (DB-13 merge) | 929 DB across 40 suites / 2114 workspace across 85 suites / doctests 6 / dbschema 61 / auth 107 | historical direct docs-lane measurements (commands/logs in `current-status.md`) |
| `f80b9704` (DB-10/12 integrated) | 2159 workspace across 87 suites, 0 failed / 1 ignored; nd-db docs 22 warnings | parent integration logs `/tmp/nav-db10-integrated-{clippy,tests,doc}.log`; fmt/clippy/tests/docs pass, latest totals independently tallied here |
| `272c9c66` (DB-22 integrated) | 2171 workspace across 88 suites, 0 failed / 1 ignored; nd-db docs 23 rustdoc warnings (all pre-existing) | parent integration at that HEAD; superseded boundary |
| `e3e0770a` (**SC-02 partial integrated; code HEAD superseded by `52b04064`/`96c495e7`**) | 2276 workspace across 92 suites, 0 failed / 1 ignored; `nd-scanner` rustdoc 4 pre-existing warnings | parent gates on the integrated main checkout (fmt check, workspace clippy, `git diff --check` clean); superseded boundary |
| `52b04064` (DB-11 landed) | 2297 workspace, 0 failed / 1 ignored | parent gate at that HEAD; superseded boundary |
| `96c495e7` (**DB-11 + DB-15 runtime, current HEAD of this branch line**) | 2302 workspace across 94 suites, 0 failed / 1 ignored (the ignore is the pre-existing `dump_divergences`); `nd-db` rustdoc 22 warnings = baseline, zero for `sql_annotations.rs` | parent gates EXECUTED at cargo/rustc 1.99.1 (fmt check, workspace clippy, `git diff --check` clean); this docs lane did not re-run Cargo |

The non-`nd-db` suite mass was constant at 1079 across the S-D3d/e/f lanes; that arithmetic is how the S-D3e workspace
figure `1746` was found to be one short of `1747`.

### Abandoned criteria drafts (deleted 2026-11-19, user-authorized)

`docs/rust-port/drafts/fields.rs` and `docs/rust-port/drafts/operators.rs` were the abandoned pre-#06 drafts. Production
implementations exist and are the only real home: `rust/crates/nd-model/src/fields.rs`, `operators.rs`, `criteria.rs`
(verified present; `nd-model/src/lib.rs` exports both modules; no build/test/include reference to `docs/rust-port/drafts/`
existed). The earlier text that archived them as "untracked work, not deleted" is obsolete; their lessons are the
rustc-pseudo-code note below.

### Lane index (numbering drift included — do not re-use these labels to schedule)

| Lane | Commit/baseline | Landed |
|---|---|---|
| DB runtime spine | `3ce1b154`, `dee593b5` | execution seam, `SQLStore`, `user_repo` executed |
| `nd-ffmpeg` fixtures + teardown | `5e56f7e8`, `8152ac5a` | portable fixtures, process-group kill |
| Stream serving | `aa9b280e`, `a462b1f2` | Range matrix, direct/transcode adapters |
| Ledger correction | `0309b5aa` | superseded by later revisions |
| Player repository | `c8c71f8d` | executed, 19 tests |
| Auth wiring | `bd8f7a84`…`cfea1e08` (merged `0fad9985`) | middleware, login, refresh |
| Secondary indexes | `15426bbc` | 80 index rows, 8 recovered columns |
| #15 archiver / handler / cache | `8b9f48cf`…`b5c317f8`; `db9f1e42`…`38b0b812`; `8abefec1`/`46b2a1ca`/`d19526e3` | **#15 complete at crate level** |
| #08 real tag backend | `beaf63d7` | lofty 0.25.5 |
| #16 mapping | `4463670b`, `5d12b5ad` (merged `b419472b`) | Go oracle pinned |
| S-A / S-B / S-C / S-D1 / S-D2a / S-D2b1 / S-D2b2 | `2a99a681`, `3f8b5a8d`, `deeb837d`, `cb11b645`, —, —, — | participant JSON, feeders, contract, JSON/Role bridge, marshal core, records |
| S-D3a…S-D3f | `3ba10ee1`, `6cd6c8d2`, `1c215a7e`, `0d2dc89d`, `6a201f4a`, then `f210431f`, `a8e4d57c`, `60cdd184` | hydration seam → executed reads |
| W4+W6 | `f8ceeb8f` | executed reads incl. transcoding |
| W7 sort gate | `62789ba2` | REST sort parity |
| DB-01 | `f57ae050` | missing-state bind repair |
| DB-03 | `4a8aac9f` | DSN/pool parity |
| DB-02 | `6d9d984d`/merge `74ebd81c` | canonical TEXT stamps, `zero_time_value()` |
| DB-00 | `48e65806`/merge `ec84c176` | additive typed tx/error seam |
| DB-04…DB-09, DB-13, BOOT-02 | `c791dbc1`, `60be7534`, `bfdcfeca`, `2911b9ab`/`9be6ad0e`, `32c4fa5b`/`320b8b03`, `d938d159`/`842c32a2`, `eae4ba1d`/`4ab63cd5`, `c6e220eb`/`162fae1d` | executed tx-bound writes + native expiring login sessions. **These rows are commit provenance only** — current scopes, non-scopes and residuals live in `current-status.md` and the current section above, not here. |
| DB-11 | `52b04064` | executed typed scanner reads, cursor contract, `ScannerScope`/`UserScope`, `get_visible_media`. Provenance only; scope and not-done list in the current section and `current-status.md`. |
| DB-15 runtime half | `96c495e7` (on the `ab466928` render-only repair) | executed annotation transfer + avg-rating recalc; durable Go oracle deferred to EVID (`decisions.md` H6). Provenance only. |

**Numbering drift:** the parent issued lanes as S-D3c/d/e/f while `hydrate-scan-contract.md` numbers the same work
S-D3c…S-D3f differently (its `:198-199` = this table's S-D3b; its `:203-206`/`:208-211` = the artist and
playlist_track/scrobble halves; its `:213-215` = the playlist/radio artwork wrappers). Neither table was renumbered;
read the contract with this note. The old scout's S-D3b meant folder/playlist and its S-D3c meant album/media-file —
do not re-use those aliases.

### Standing hazards (durable rules; provider/agent failure histories deliberately not canonicalized)

- **Bank progress in commits per milestone.** Lanes have died mid-run (provider limits, budget caps, daemon errors) and
  uncommitted work died with the worktree. A dead lane should lose a milestone, not a phase; collect the diff before
  archiving anything. Report incomplete coverage and resume rather than declaring approval.
- **Verify every lane report against `git log`/`git status`/the files.** Lanes reported commits that exist on no branch
  (`5d1c207f`, `8842a742`, `6b289012`) and a module that was never written.
- **rustc 1.99.1 rejects Go-flavored pseudo-code** (`a + b` concat, `+=` on `&str`, `.append()`, `std::map<…>`,
  map-literal constructors, missing `->`). Workers write → `cargo build -q -p CRATE` → fix from compiler messages; one
  writer per crate at a time.
- **`Connection` is `Send`-not-`Sync`**; pool mechanism and retry mapping are DECIDED (`decisions.md` H1/H2) — workers
  implement, they do not re-derive.
- Keep `bench/`, `docs/perf/` as the committed perf baseline; do not treat rustdoc warnings as zero (see
  `current-status.md` documentation debt).

### Phase record (2026-11-04, shape only — "shape ≠ wired")

Phase 1 closed the #06 criteria DSL, #04 optimize/backup, and the #07 fixture tail; `db/repair.go` (367L FTS5
verify/repair) stays DEFERRED as its own lane. Phase 2 shaped #05 `nd-db`: pool/store/harness, `sql_*` helpers, all
repos, and `criteria_sql.rs` (904L criteria→SQL compiler — Cond tree, threaded field map, join planner, orderBy
coalesce/cast/role forms, JSON merge groups, NFC/NFD `pathVariants`), with two-phase rowid semantics pinned to
`sql_search.go:126-141` (base pagination stripped from BOTH sql and args; page args bind AT the JOIN position).
Phase 3 shaped #13 `nd-auth` (security MUST-FIXes applied) and #12 `nd-sched`'s scheduler half; its
maintenance/logging half is still unspecified. Phase 4 shaped #14's decision half. Slice-2's security finding remains
load-bearing: `sql_restful.rs::parse_rest_filters` interpolates a user-controlled filter **key** as an SQL identifier
when no whitelist is registered (faithful to `sql_restful.go:33-44`), so every repository consuming it MUST call
`with_field_white_list` with names from the `nd-model/src/fields.rs` registry before exposure; lanes that ship a
restful-backed repository without a whitelist are rejected at review. rusqlite is pinned (sqlx was never resolved into
`Cargo.lock` and is referenced nowhere).

### Slice-1 pool decision (compatibility pointer for `nd-db/src/store.rs:73`)

Pool mechanism, busy-retry mapping, minimal harness, and the schema-drift guard for #05 slice 1 are recorded as
`decisions.md` H1, H2, H3 and H5. The source comment's numeric range is stale after this curation; the decision content
is unchanged.

### Schema-drift guard (compatibility pointer for `nd-db/tests/schema_diff.rs`)

Regenerate the Go-migrated `.schema` dump with the `nd-go-dump` harness (`go run` with `DbPath` at a temp file, then
`sqlite3 .schema`) and re-run; see `decisions.md` H3. Go truth is the migrations, not a hand-written golden; the golden
DDL was derived by replaying migrations and still deserves one re-derivation against the corrected shape (`OPS-01`).

### §1b Go contract for the shared layer (compatibility pointer; cited as "next-work.md §1b")

Binding content moved to `decisions.md` J1 (tags are not a map dump), J2 (integer keys sort as strings), J3 (escaping
divergence), O1/O3 (ordering rules and golden stability), W4 (NULL semantics), W5 (hook inventory: 8 `PostScan`, 5
`PostMapArgs`, hook-free models get no invented hooks, `smart_playlist` is not a repository), plus the write/read
funnel facts: Go's `toSQLArgs` (`persistence/helpers.go:20-42`) is **one** per-value type switch (unwrap non-nil
`*time.Time`, else `driver.Valuer`, then `PostMapArgs`; a **nil** `*time.Time` stays nil → NULL), and reads go through
`queryWithStableResults`/`ScanStruct`/`queryAll`/`queryAllSlice` (`sql_base_repository.go:411,:429,:441,:460`) with dbx
mapping by **`db` tags**, preferring shallower shadowing fields, initializing nil embedded pointers, and discarding
unknown columns into `sql.NullString` (`dbx@v1.12.0/rows.go:65-104`, `struct.go:160-206,249-257`). Search feeders were
artist-only and could not express album/mediafile inputs — closed by `sql_feeders.rs` (`3f8b5a8d`).

### §1c Shared-layer slice plan (compatibility pointer; cited as "next-work.md §1c")

`decisions.md` R1 (three role representations, one explicit conversion seam, all 14 keys equivalent, never a fourth) and
H4 (oracle harness invocation rules). Slice outcomes are in the lane index above; S-A and S-B were independent by
construction because the feeder seam takes participant names as `&[String]`.

### §Then-7 / §Then (compatibility pointers; cited by `sql_feeders.rs`)

#16's landed state and the pair-tag map-order hazard are `decisions.md` O2; the scanner minimum-executed-surface,
orchestration order, watcher MIT licence answer, and gob=ProgressInfo correction are `decisions.md` S1-S3. The writes
already rendered today (property/library/folder/tag/artist/album/mediafile/artwork/artwork_queue plus `DataStore.GC`'s
11 sequential sub-ops, `persistence/persistence.go:246-277`) remain the DB lane's inventory.

### §Current-state pointer (cited by `nd-db/src/marshal.rs:7`)

The pre-S-D2a parallel-list defect class, the INSERT column/value alignment fix (`8c89aeb9`), the `INSERT … SET`
defects (`radio_repo`, `artist_repo`, `playlist_repo` — MySQL syntax SQLite rejects), and the "columns and args emitted
together, not id-first" invariant are `decisions.md` W1; the executed-regression lesson is W0. Verified non-claims:
no `mediafile_repo::upsert_sql`, no `ON DUPLICATE KEY UPDATE` anywhere in `nd-db` or Go `persistence/`, no
`todo!()`/`unimplemented!()` in `nd-db`, no `media_file_pkey` index; Go's `mediaFileRepository.Put`
(`persistence/mediafile_repository.go:184-197`) is `putByMatch` (SELECT-then-`put`), already modeled correctly.

### Write-map invariance (compatibility pointer for `artwork_hydration.rs:17` and `runtime_hydrate_album_mediafile.rs:2453`)

`album_row` 48 keys / `mediafile_row` 66 keys, hydration-only columns never written, byte-identical re-render:
`decisions.md` W2. Related landed facts kept in the contracts: artwork `ReadOutcome`, kind **prefix** binding
(`OWNER_TABLES` was keyed by kind NAME and matched nothing — fixed to `Kind.Prefix()`), the `get_random` phase-2
library-filter re-application (Go applies `applyLibraryFilter` unconditionally, `mediafile_repository.go:209`),
`playlist_repo::get_sql`'s `OR`-precedence visibility fix (a flat `WHERE a AND b` returned the wrong rows, executed),
`radio_repo`'s camelCase `cols_to_update` normalization through `to_snake_case`, and `count_all_sql`'s
`ORDER BY <table>.id` leg (`sql_base_repository.go:569`). Count-renderer `ORDER BY` parity for the remaining
goldens (artist/album/mediafile/playlist/plugin/scrobble) is renderer-only, nil value impact, lowest priority.

### DB-15 and count-renderer notes

DB-15's render-only repair design (embed `average_rating_expr_sql()` into the SET, keep the 3-arg list, executed test
proving `average_rating = 4` for ratings 5/3/0 across three users, redocument-and-pin the `annotation.id` prepare
failure rather than deleting the public renderers) is in `subplans/db-runtime.md` "DB-15 fix design"; the landed
`ab466928` covers **only** that repair (LANDED; at that boundary full DB-15 was NOT done — annotation reassignment
transactions and the annotation read APIs were still unimplemented). **Dated amendment 2026-11-20:** the annotation
reassignment/average-recalc transactions landed later at `96c495e7` (see the current section); the durable Go-harness
oracle is deferred to the EVID lane (`decisions.md` H6), so DB-15 stays not-fully-DONE on that DoD caveat. Recorded
for DB-16: share's empty-`cols` divergence and the
`refresh_counters_sql` five-arg bind. A renderer `parameter_count` gate (placeholders vs args) would have caught DB-01
and DB-15 mechanically; still an unassigned idea.
### Hydration-lane findings (durable homes: contract residual blocks, not restated here)

The S-D3b…S-D3f transcripts were removed; their durable findings live in `hydrate-contract.md` (per-lane residual
blocks: S-D3c, S-D3d, S-D3e, S-D3f) and `hydrate-scan-contract.md`. Unique results that only ever lived in this file,
recorded here so nothing is lost:

- `ScrobbleEntry.Participants` **is** `MediaFile.Participants` (EXECUTED: `&e.Participants == &e.MediaFile.Participants`
  is `true`, because `model/scrobble_buffer.go:8-16` embeds `MediaFile` and declares no participants field), and
  `SELECT m.*` carries `participants`/`tags` (`schema_ddl.rs:64` declares both `jsonb NOT NULL DEFAULT '{}'`), so the
  delegated PostScan decodes them. The older claim that delegation "runs over absent JSON columns" was wrong.
  `getParticipants` (`sql_participations.go:127-146`) is still unported, so `ScrobbleBufferRepo::next` returns Go's value
  **before** Go's re-read; a participant re-fetch remains a named consumer gate.
- A missing `media_file` under a joined read is a **scan error**, not a tolerated hook (EXECUTED: NULL into the promoted
  non-pointer `MediaFile.ID` fails before `PostScan`, and the error is neither `model.ErrNotFound` nor `sql.ErrNoRows`);
  the `(nil, nil)` contract is scoped to a genuinely empty queue. On a missing joined row dbx allocates and fills the
  shared embedded pointer before `PostScan`, so Go can return a **value beside an error** — the port cannot (`Result` has
  no such shape) and no non-test Go caller reads a value on error; recorded as an S-D3e residual, not emulated.
- `hydrate-contract.md`'s participant-order bullet was corrected: `json.Marshal` sorts map keys, so participant column
  bytes MATCH Go and only `AllNames`' iteration order is nondeterministic (`decisions.md` O1).
- Sibling first-error precedence in bulk readers is SOURCE-INFERRED, not a freshly executed counterexample; the folder
  full-drain adapter is not a claim of universal lazy-cursor parity.
- Reviewer/reported findings were adjudicated against source before acting; several quoted symbols and totals did not
  exist in the tree. Treat agent-quoted line ranges and test counts as UNVERIFIED until checked (`decisions.md` C4).
