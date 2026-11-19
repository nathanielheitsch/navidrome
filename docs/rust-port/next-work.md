# Plan of record for the next body of work

Read order and authority: [`README.md`](README.md) → this current section (milestone order only) →
[`current-status.md`](current-status.md) (measured evidence) → [`decisions.md`](decisions.md) (binding decisions) →
`subplans/` (bounded assignments). Everything below the separator is a **dated historical index**, not a scheduler.

## Current execution plan (baseline integrated `master` `74ebd81c`, updated 2026-11-19)

`74ebd81c` = `ff900d3e` + `ab466928` (DB-15 render-only rating repair) + `e9dd8230`/`ff5b9c92` (scanner local-FS storage
subset) + `48e65806`/`ec84c176` (DB-00 typed transaction/error seam) + `6d9d984d`/`74ebd81c` (DB-02 scanner timestamp
contract). Evidence is in `current-status.md`; this section
only sets order. Nothing here authorizes implementation, commits, new workspaces, or completion claims. This docs lane is
cut from `ff900d3e`, so statements about integrated behavior are sourced from `master`, not measured here.

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

### Landed and verified (dated parent measurements, in this baseline)

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

### Landed since `ff900d3e` (parent-reviewed, signed, merged — DB-00/DB-02 and the storage subset each landed via a merge commit; scopes are deliberately partial)

| Assignment | Commit | Scope actually landed — and what is **not** done |
|---|---|---|
| DB-15 **render-only** rating repair | `ab466928` | `update_avg_rating_sql` embeds `average_rating_expr_sql()` with 3 ordered binds, and `tests/runtime_annotation_rating.rs` executes it against a real schema. The three public annotation renderers were deliberately **kept and redocumented** (their `annotation.id` prepare failure pinned), not left undecided. **Full DB-15 is NOT done:** typed annotation reassignment/merge transactions and the annotation read APIs are unimplemented. |
| Scanner local-FS storage subset | `e9dd8230` + merge `ff5b9c92` | `nd-scanner/src/storage.rs` only: std-`fs` `list`/`stat`/`read`/`read_link`/`resolve`/`exists`, logical-path handling, `file://` URI parsing and explicit symlink classification. **Not implemented:** the walker, `.ndignore`/ignore policy, folder hashing, cancellation, `MusicFS`/`ReadTags`, registry dispatch, watcher. **Not** SC-01 ratification and **not** SC-02 completion. |
| DB-02 scanner timestamp contract (D-TIME) | `6d9d984d` + merge `74ebd81c` | Scanner-facing stamps/cutoffs bind canonical TEXT via `time::value`; `library_repo::zero_time_value()` replaces raw `ZERO_TIME` integers; artwork-purge integer cutoff fixed with executed coverage. **Partial:** `scrobble_buffer` `play_time`/`enqueue_time` still bind integers, and `find_recent_files_by_properties_sql` has no executed pin. Not scanner writes.
| DB-00 typed transaction/error seam | `48e65806` + merge `ec84c176` | Additive `try_with_tx`/`try_with_tx_retry` beside `with_tx*`, `DbOperationError`/`DbRuntimeError`, safe rollback cleanup, BUSY-only retry (`decisions.md` E1-E3, H2). The seam only — DB-04 onward still consume it, and the DB lane is nowhere near done. |

### Live wave (in flight — never mark DONE from a message)

**No implementation lane is in flight as of `74ebd81c`**: all four first-wave lanes are merged and their
branches cleaned. The rows below are the next assignable packages, not pending merges.

| Assignment | Branch | Scope claimed | State |
|---|---|---|---|
| DB-04 executed **property** writes (consumes DB-00/02/03) | to be assigned | Executed property put/delete on a transaction through `try_with_tx*`, canonical TEXT stamps, typed errors | **Next assignable package.** Code not written; DB-02's timestamp contract is a prerequisite, not a substitute. Library writes are **DB-05**, not this package. |
| DB-05 executed **library** writes | after DB-04 | Executed library put/click/touch-style writes on the same seam | Assignable only after DB-04 integrates (shared renderer files) |

Merges are parent-controlled; this docs lane is support work, not a rebalance of implementation lanes.

### Next dependencies (order, not estimates)

1. **DB-04 → DB-05 → DB-06 → DB-07 …** — executed **property** writes first (DB-04), then **library** (DB-05),
   **tag** (DB-06), **folder** (DB-07), then artist/album/media-file `Put`/`PutByMatch` with base/FTS/junction/link
   atomicity (DB-08…10), typed filtered scanner reads, artwork invalidation/queue-enqueue subset. Each lane consumes the
   landed DB-00 typed transaction/error seam, the DB-02 canonical TEXT stamp convention, and the DB-03 effective pool
   configuration — adopt them rather than re-deriving helper behavior. Add executed write→read-back regressions, not more
   renderer-only assertions (`decisions.md` W0/W1); the DB-02 gaps (`scrobble_buffer` integer stamps,
   `find_recent_files_by_properties_sql` executed pin) belong to whichever lane owns those statements.
2. **Re-date the gate block** on the integrated tree after each merge; the current block in `current-status.md` is dated
   parent truth at `74ebd81c`, not proof for a later diff.
3. **Honest scanner phase-1 vertical slice** — storage/stat/symlink/ignore and metadata→record adapters, actual
   library/folder IDs, pure album aggregation/change detection, one fixture-tree ingest over those DB APIs
   (`decisions.md` S1-S4). Call it phase 1, not complete #17.
4. **Bootstrap/HTTP/canonical route contracts** — scoped API manifest capture, configuration → schema/pool → configured
   library/admin → router/listener wiring, login-session/native-audience and HTTP key integration, stream
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

Zero executed reads remain in `artwork`, `genre`, `playqueue`, `plugin` (scope pending), `scrobble`, `share`,
`smart_playlist`, `user_props`; `annotation_repo.rs` does not exist because annotations ride album/media-file (DB-15),
so that is not a gap. Non-DB follow-up: the `nd-stream` teardown flake (`current-status.md`).

### Decision ask (scope): does `plugin_repo.rs` belong in the port at all?

`schema-contract.md` says the `plugin` table is **dropped** — Part C item 14 removes `/api/plugin*` and does not port
`persistence/plugin_cleanup.go`, so `db/migrations/20260106000620_create_plugin_table.sql` is not ported.
`plugin_repo.rs` was ported anyway (its header notes the table is absent yet claims the schema slice will add it) and is
exported publicly (`lib.rs`), so callers can invoke its renderers — but every statement it renders fails with
`no such table: plugin` against the shipped schema, including the probed path (`DELETE FROM plugin WHERE id = ?`).
Options: (a) delete `plugin_repo.rs` and its tests as out-of-scope dead code (recommendation: it can never run against
the schema we ship and its tests only assert rendered strings); (b) keep it and mark the module
`#[cfg(feature = "plugin-table")]`/documented-unreachable until a schema decision reverses that section. Needs the
parent's call before any lane touches it — deleting a public module is a deliberate divergence, not a fix. **No source
deleted by the docs lane; decision still pending.**

### Revised milestones and dependencies

1. **DB safety and configuration gates before enabling writes** — **all four DONE**: bind order (DB-01 `f57ae050`),
   canonical TEXT timestamps + D-TIME (DB-02 `6d9d984d`/`74ebd81c`), effective pool configuration (DB-03 `4a8aac9f`), and
   typed error/transaction composition (DB-00 `48e65806`/`ec84c176`). Existing helpers still carry latent defects — the
   scrobble-buffer integer stamps and the un-pinned `find_recent_files_by_properties_sql` cutoff among them — so do not
   connect renderers unchanged.
2. **Bounded DB runtime packages** — as item 2 above; library bootstrap policy belongs to the bootstrap caller, DB
   supplies typed executed operations. Test rollback, retry, immutable fields, non-target rows, empty/chunk boundaries,
   and actual read-back through production methods.
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
| `74ebd81c` (DB-02 merge, current head) | 841 DB across 33 suites / 2009 workspace across 78 suites | current dated gates |

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
`ab466928` covers **only** that repair (LANDED; full DB-15 NOT done — annotation reassignment transactions and the
annotation read APIs are still unimplemented). Recorded for DB-16: share's empty-`cols` divergence and the
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
