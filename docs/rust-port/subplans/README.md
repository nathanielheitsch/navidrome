# Subplan index — the one task/dependency map (baseline integrated `master` code HEAD `e3e0770a` plus DB-11 `52b04064` and DB-15 runtime `96c495e7`, updated 2026-11-20)

**Planning authority, NOT implementation authorization.** This file is the parent-owned index that reconciles the four
technical subplans into a single task graph, resolves the semantic prerequisite aliases each lane invented into actual
package IDs, and records launch order and open decisions. Live facts and gates are in
`../current-status.md`; binding decisions are in `../decisions.md`; milestone order is the current section of
`../next-work.md`. A `[planned]` label means nothing has been built; packages listed LANDED below are dated parent
measurements on pinned commits, and a package listed IN FLIGHT would be an unmerged branch that is **not** done. Live facts
and gates are in [`../current-status.md`](../current-status.md).

Lane files (verbatim planner output, parent-published):

| File | Covers | Packages | Verdict |
| --- | --- | --- | --- |
| [`db-runtime.md`](db-runtime.md) | #04/#05 executed persistence | DB-00 … DB-22 (DB-22 is parent-added) | DB-00/01/02/03 **and** DB-04 `c791dbc1`, DB-05 `60be7534`, DB-06 `bfdcfeca`, DB-07 `2911b9ab`/`9be6ad0e`, DB-08 `32c4fa5b`/`320b8b03`, DB-09 `d938d159`/`842c32a2`, DB-13 `eae4ba1d`/`4ab63cd5` LANDED (each a partial scope; no `*_in_tx` primitive has a production caller, although `nd-auth` calls `UserRepo` methods outside them); DB-15 render-only LANDED `ab466928` **and its runtime half LANDED `96c495e7`** (`AnnotationEntityKind`, `AnnotationWriteError`, `update_avg_rating_in_tx`, `reassign_annotation_in_tx`, 5 executed tests) — DB-15 is **not** fully DONE: the durable Go-harness oracle is deferred to the EVID lane (`../decisions.md` H6; authorization gate in this index). **DB-10 LANDED `65072b89`/`6e5ca97d`; DB-12 executed artwork invalidation/enqueue subset LANDED `e5d0592d` (rustdoc fix `f80b9704`), no production scanner callers or worker/full scanner.** **DB-22 LANDED `272c9c66` (executed genre/scrobble/playqueue/artwork reads; narrow-read subset per C5 — not full Go parity).** **DB-11 LANDED `52b04064`** (executed typed scanner reads + cursor contract + `ScannerScope`/`UserScope` + `get_visible_media`; 21 executed tests; not the full cursor/REST surface — the `SEEDEDRAND` seeded full-set render stays with the REST/API lane; no production scanner caller and no `nd-scanner` consumption). The **SC-02 partial LANDED `e3e0770a`** (local walker, `.ndignore`, folder hash, cancellation — not SC-01/SC-02 completion, no scanner entrypoint or ingest). **DB-14 LANDED `de0f49ea`** (4 files: NEW `src/scanner_writes.rs` 935 lines, NEW `tests/runtime_folder_persistence.rs` 17 executed tests, `lib.rs` +1, `album_repo.rs` +102 additions-only — `copy_album_created_at_in_tx`; no production caller, no `nd-db` dependency in `nd-scanner`). **DB-21(P1-DB) LANDED `632271a9`** (1 file: NEW `nd-db/tests/runtime_scanner_persistence_contract.rs` 4206 lines, 7 executed tests — the **DB-side-only P1 exit** per the parent scope narrowing of 2026-11-21 (`../decisions.md`): composed regression through the real pool + frozen API surface (51 compile-checked SigNN pins) + an executed CONSUMER_HANDOFF **U-ledger** pinning `SC.mapping-bridge`, `SC.phase1-fixture`, `BOOT.configured-library-caller` and `production_full_finalize_db_contract` as Unmet. **NOT done:** no production caller (SC-05 is the first consumer); no full-exit; the collection/count API for later API-F* still belongs to **DB-21(FULL)** and is NOT delivered; not #27A progress; the stable ID DB-21 is incomplete until FULL-SCANNER-DB). **DB-16 LANDED `7d022a02`** (10 files, +3507/−308: executed `move_matched_mediafile`, `remap_missing_mediafile`, NEW `reassign_references_in_tx`, `delete_mediafile_in_tx`, `delete_all_missing_in_tx`; A8 `ShareError::EmptyUpdateColumns`; **D-PLUGIN deletion executed** — `plugin_repo.rs` + `lib.rs` export deleted; NEW `tests/runtime_move_persistence.rs` 18 executed tests; no production caller — SC-06/BOOT maintenance are the consumers). **DB-17 LANDED `b3dee0a6`** (5 files, +2817: four caller-tx refresh helpers — `album_repo::refresh_play_counts_in_tx`, `artist_repo::refresh_play_counts_in_tx`, `tag_repo::update_counts_in_tx`, `library_repo::refresh_library_totals_in_tx(id, now)` (8 probes + 10-bind update; Go's `RowsAffected` discarded at `library_repository.go:249`, Rust returns the written `RefreshStatsTotals` — documented deviation) + NEW `tests/runtime_count_refresh.rs` 20 executed tests; the bare `play_date = excluded.play_date` REPLAY backwards-overwrite is faithful with NO monotonic guard (executed pin), caller-stamped canonical TEXT with no clock in SQL, and rollback stays the CALLER's obligation via the DB-00 seam; no production caller — SC's phase-3/`scanner.go:316`/`:344` triggers are the consumers). **Next in spine order is DB-18**: `DB-18 → 19 → 20 → DB-21(FULL-SCANNER-DB)` is the remaining forward spine (DB-18/20 need `D-STATS`/`D-GC`, DB-19 its boundary approval) |
| [`scanner.md`](scanner.md) | #17 staged scanner | SC-00 … SC-14 | **SC-03 LANDED `cb59a973`** (mapping bridge + singleton projection, 19 executed tests); **SC-05 IN FLIGHT, NOT ACCEPTED** (uncommitted working-tree vertical, **no owner**); **SC-04 READY** (prereq met, placement `../decisions.md` D-SC04); **SC-06 unauthorized**. A local-FS storage subset LANDED `e9dd8230`/`ff5b9c92`, and the **SC-02 partial LANDED `e3e0770a`** (fast-forwarded onto `2e7026b2`): bounded local walker, `.ndignore`/ignore policy, folder hashing and cancellation with executed `tests/{folder_hash,ignore_policy,storage_seam,walk_policy}.rs`, and the six approved dependencies recorded in `../decisions.md` S9 (`b1aeea2c`, corrected at `2e7026b2`). **Integrated-tree partial scope**: no scanner entrypoint, `MusicFS`/`ReadTags` dispatch, tag-to-DB ingest, full-scan orchestration or watcher — at `cb59a973` `nd-scanner` still declares **no `nd-db` dependency** (that delta exists only in the uncommitted SC-05 tree), so this is **not** SC-01 ratification, not SC-02 completion, not MusicFS readiness. `SC-01` was ratified **NARROW on 2026-11-21** (`../decisions.md` "SC-01 seams ratification": the five SC-05 seams + the authorized `nd-scanner` `nd-db` manifest delta), so SC-03+ proceed on those seams; the progress sink, watcher, remote storage and `MusicFS`/`ReadTags` stay unratified and the DB-21 U-ledger entries stay U until SC-03/SC-05 deliver. |
| [`boot-api.md`](boot-api.md) | #24, #33 graph, #27A | BOOT-00…04, API-00…06, API-F1…F7, BOOT-F1 | spec-first; #27A gated. **BOOT-02 landed** `c6e220eb`/`162fae1d` (login mints a native expiring session) — the login-session correction only, **not** full auth/API integration: **BOOT-00 LANDED `6d6938b1`** (gate work only: D3 manifest pin repair executed — `chrono 0.4`/`tracing 0.1`/`tracing-subscriber 0.3`/`base64 0.22`, zero new lock entries; D1 typed boundary + EXECUTED `dependency_graph_has_no_http_api_cycle` + the `RouterInjection` shape; D4 via B3-ratified; no production behavior, nothing listens). API-04 remains `DECISION REQUIRED`; API-00 is READY for specification and no longer blocked on BOOT-00; **BOOT-01 is the next launchable boot package** (it owns the `ND_CONFIGFILE` resolution gap and B2 ffmpeg-config injection) and BOOT-03/04 plus API-01/02/03/05/06 stay BLOCKED; no router is mounted. |
| [`evidence-ops.md`](evidence-ops.md) | parity residual + #03/#31 | EVID-01…08, OPS-01…07, FRONT-* | mostly BLOCKED/decision; EVID-01's snapshot/contract amendments largely folded into these docs |

Rules that override any historical lane text: **one writer per checkout** — the user-approved exception is parallel
*isolated worktrees* with parent-controlled merges (one writer inside each worktree; a branch is never called done from
an agent message); a child inherits the parent's verified workspace; review never races edits; every package gets a
fresh review and parent CLI verification before it is claimed done; a green crate test is not proof of an integration.
The orders below are **dependency order**, not a ban on independent work with no unmet prerequisite: the approved
isolated worktrees let prerequisite-independent packages (DB-00's additive seam, DB-15's render-only repair, a local-FS
storage subset) proceed concurrently, while packages that genuinely need an earlier release still wait for it.

---

## Cross-lane prerequisite alias resolution

The `boot-api` lane invented `EXT-*` semantic aliases and the `db-runtime`/`scanner` lanes
invented `SC.*` / `BOOT.*` / `API.*` handshake names. They bind to concrete packages here.
**No implementation may launch while its prerequisite is unresolved or reported only as an
agent message — the dependency is met only by a verified, reviewed package.**

### `EXT-*` (boot-api → DB/SC)

| Alias | Concrete package(s) | Lane | Note |
| --- | --- | --- | --- |
| `EXT-DB-CONNECTION` | **DB-03** (+ **DB-00** typed seam) — **LANDED `4a8aac9f`** | DB | Decision `D-OPEN` is **LANDED and decided** (`../decisions.md` "DB open/configuration (D-OPEN, landed `4a8aac9f`", `db-runtime.md` "D-OPEN as implemented"); effective per-handle settings verified) — **BOOT-00 must not re-litigate DSN/PRAGMA policy**. Bundled SQLite is FK=1 / busy 5000 ms; do not assume FK-off. |
| `EXT-DB-LIBRARY` | **DB-05** | DB | BOOT-01 owns configured-library policy; DB-05 owns executed typed insert/update. |
| `EXT-DB-SCOPED-LOOKUP` | **DB-11** (API-safe point read) — **LANDED `52b04064`** | DB | `D-SCOPE` consumed by the executed `UserScope` authority: internally-built membership only, no raw user SQL, deny arms for zero-grant/empty library. Not #27A-blocking on its own; API-02 still owns the route. |
| `EXT-DB-EPOCH` | **DB-13** | DB | Epoch-only increment + RETURNING; `D-REVOKE`; consumed by BOOT/API, not a route. |
| `EXT-DB-QUERY` | **DB-11** — **LANDED `52b04064`** (+ later collection/count via **DB-21**) | DB | Typed executed scanner queries/cursors now exist; collection/count for later API-F* still belongs to **DB-21**; not a #27A prerequisite. |
| `EXT-SC-REAL-SCAN` | **SC-05** (min for #27A) / **SC-06** (complete phase 1) | SC | In-process phase-1 entry that really reads tagged media, commits, reopens, reads back. Full watcher/subprocess not required. |

### `SC.*` handshakes (db-runtime → scanner owner)

| Handshake | Provided by | Consumed by |
| --- | --- | --- |
| `SC.query-consumption` | SC-05/SC-06 | DB-11 (cancel/backpressure boundary) — **RESOLVED by DB-11 `52b04064`**: the DB side ships the visitor/`Control`/`VisitOutcome`/`CursorError` contract and owns early-stop plus lease release; SC still owns backpressure when it consumes these APIs |
| `SC.folder-write-batch-contract` | **RESOLVED as D-BATCH (PARENT DECIDED 2026-11-20, `../decisions.md` "Folder-write batch contract") and CONSUMED by DB-14 `de0f49ea`** — per-folder tx opened by DB, `persist_folder_delta_in_tx` pub(crate), warn-vs-fatal table, SC pre-generated IDs, immutable batch | SC-05/SC-06 consume the delivered batch types (`FolderWriteBatch`, `CommittedFolderOutcome`) | DB-14 (**LANDED `de0f49ea`**; no production caller yet) |
| `SC.mapping-bridge` | SC-03 | DB-21 (P1-DB exit) — **stays U even after SC-03 landed `cb59a973`**: the entry text also pins the distinct MediaFile DTO storage-field gate, which SC-03 does not deliver (`../decisions.md` "SC-03 birth-time ruling" section); the DB-21(P1-DB) contract test `632271a9` still asserts it Unmet |
| `SC.phase1-fixture` | SC-05 | DB-21 (P1-DB exit) — **stays U: asserted Unmet by the U-ledger in `632271a9`**. SC-05 is **IN FLIGHT, NOT ACCEPTED** (uncommitted, no owner), so this closes only with an accepted, integrated SC-05 — see `../decisions.md` D-SC05-INT |
| `SC.move-selection` | SC-07 | DB-16 |
| `SC.full-phase-contract` | SC-06…SC-10 | DB-21 (FULL-SCANNER-DB exit) |
| `SC.playlist-import-contract` | SC-09 (#23 `PL-SERVICE`) | DB-19/DB-21 |
| `SC.successful-phase-coverage` / `SC.purge-policy` | SC-07/SC-10 | DB-20 (destructive GC external gate) |

### `BOOT.*` / `API.*` handshakes (db-runtime → boot-api owner)

| Handshake | Provided by | Consumed by |
| --- | --- | --- |
| `BOOT.db-open-policy` | BOOT-00 | DB-03 (`D-OPEN`) |
| `BOOT.epoch-revoke` | BOOT + **DB-13** | API-03 |
| `API.revoke-authorization` | API-03 | DB-13 |
| `BOOT.configured-library-caller` | BOOT-01 (uses DB-05) | DB-21 (P1-DB), SC-14 — **stays U: asserted Unmet by the U-ledger in `632271a9`**; gate is **BOOT-01 implementing** with BOOT-00 a prerequisite (approval is not delivered caller evidence) |
| `API.visible-media-contract` | API-02 (+ DB-11) | DB-11 — **RESOLVED by DB-11 `52b04064`**: `get_visible_media` is the executed DB-side point read and typed scope authorities are the only public scope inputs; API-02 owns revalidation/auth/response and the route |

### `FRONT-*` (evidence-ops) — full service frontier, not #27A gates

`FRONT-18/19/20/21/22/23/26/27/28/32/33` are the remaining service tails. Each needs its own
dedicated plan + review when scoped; none gates #27A or the early HTTP boot. `FRONT-26` is
blocked by `D-PLAYER26`, now **decided** (retain player-session service, drop mpv/jukebox) — see that decision below.

---

## Open decisions (must be answered before the named package launches)

| ID | Decision | Blocks | Owner |
| --- | --- | --- | --- |
| `D-ERROR` | **DECIDED 2026-11-18 (parent, option B), LANDED `48e65806` + merge `ec84c176`.** Additive `try_with_tx`/`try_with_tx_retry` beside `with_tx*` (no signature churn): callback `DbOperationError<E>` (`Domain(E) / Sqlite{stage, source}`) and runtime `DbRuntimeError<E>` adds `RollbackFailed{original, rollback}` and `Pool(StoreError)`, SQLite cause retained through `Error::source()`, no string loss; COMMIT-stage busy is reachable so the Commit branch issues no second ROLLBACK. Retryable set is BUSY-only (H2, amended by review). Record: `../decisions.md` E1-E3, H2. | DB-00 (landed) and all compound writes | DB — resolved |
| `D-TIME` | **DECIDED 2026-11-18 (parent), LANDED `6d9d984d` + merge `74ebd81c`.** Scanner stamps stored as TEXT via `time::value`; Go's `<> canonical zero` idle predicate kept verbatim (the DDL sentinel makes a fresh library count as in-progress — inherited, pinned); the real defect was INTEGER affinity. Record: `../decisions.md` T1-T4. Residual: `scrobble_buffer` `play_time`/`enqueue_time` still bind integers and `find_recent_files_by_properties_sql` has no executed pin. | DB-02 (landed), DB-05/07/08/09/10 | DB — resolved |
| `D-OPEN` | **DECIDED 2026-11-18 (user, option a), LANDED at `4a8aac9f`.** Typed `DbOpenOptions` as explicit per-connection PRAGMA statements with effective-state read-back before pooling; Go-compatible DSN parser (keeps genuine URI params, rejects unknown ones); defaults matching Go (WAL for file DBs, 15000 ms, NORMAL, FK on; `:memory:` keeps the memory journal). Record: `../decisions.md` P1. | DB-03, every runtime acceptance | BOOT-00 + DB — resolved |
| `D-CURSOR` | **RECORDED 2026-11-20 (first draft corrected at `8b5b3925` after citation audit), CONSUMED by DB-11 `52b04064`.** Two typed cursor kinds (single-statement vs id-chunked), leased connection owned for the whole iteration, three-way stop taxonomy (`Sqlite` error vs `Closed` vs clean end), chunk 600, `SEEDEDRAND` full-set render not a cursor, prefix-drop deviation declared. `../decisions.md` "Cursor and scope". | DB-11 (landed) | DB + SC |
| `D-SCOPE` | **RECORDED 2026-11-20 (corrected with D-CURSOR at `8b5b3925`), CONSUMED by DB-11 `52b04064`.** `applyLibraryFilter` (`sql_base_repository.go:261-283`) omits the clause for admin/headless/all-grants, otherwise appends `library_id IN (SELECT … user_library …)`; no `1=1` for users; zero-grant non-admin matches nothing (deny). `library_filter: Option<Sql>` stays the only scope parameter, resolved once by the caller; scanner scope (configured library IDs) and user read scope stay distinct. `../decisions.md` "Cursor and scope". | DB-11 (landed), API-02 (assignable) | DB + API |
| `DB-21 exit scope` | **PARENT DECIDED 2026-11-21 (`../decisions.md` "DB-21 exit scope and prepared-statement gates")** — the launchable DB-21 exit is the **DB-side-only** P1 contract (composed regression + API freeze + executed U-ledger); `SC.mapping-bridge`/`SC.phase1-fixture`/`BOOT.configured-library-caller` stay U until SC-05/BOOT-01 deliver caller evidence. Per `db-runtime.md:895` a narrow exit may demonstrate only a strictly narrower scope, never the general one: **DB-21 is not complete until FULL-SCANNER-DB**. | DB-21 (P1-DB landed `632271a9`; FULL pending DB-16..20) | DB + SC + BOOT |
| `Prepared-statement gates` | **PARENT DECIDED 2026-11-21 (same `../decisions.md` section)** — the `db-runtime.md:628-632` "prepare every statement" gate is **DEFERRED** until DB-04..DB-17 mutation renderers stabilize; a naive all-statements gate false-fails on five documented classes (`refresh_counters_sql` partial shape; share empty-cols → `near "WHERE": syntax error`, a syntax error, NOT no such column; `plugin_repo` → `no such table: plugin` by D-PLUGIN; `annotation_count_sql` → `no such column: annotation.id`, pinned at DB-15; clause fragments not standalone-preparable). The 5-class inventory now lives **executed** in `runtime_scanner_persistence_contract.rs`. | DB-21(FULL) / the lane that adds the gate | DB |
| `D-REVOKE` | **IMPLEMENTED, but the canonical decision record is MISSING — a documentation gap for the parent, not a settled authority.** The DB-13 primitive LANDED `eae4ba1d` + merge `4ab63cd5`: epoch-only `revoke_tokens_in_tx` with `RETURNING`, an overflow/`typeof` guard, no-row `NotFound` classification and a same-tx existence read; `Exhausted` is terminal. That shape was settled *in the parent's review of the lane* (the `/tmp` review record), and no `D-REVOKE` entry exists in `../decisions.md`, so this row must not be read as a recorded decision. Consumers remain BOOT/API (`BOOT.epoch-revoke`, `API.revoke-authorization`), and post-commit token observation is still theirs to decide. | DB-13 (landed), API-03 | DB — implementation landed; **decision record outstanding**; BOOT/API consumption open |
| `D-STATS` | Retain Go partial-commit/nonfatal cleanup or approve stronger atomicity | DB-18 | DB |
| `D-GC` | Selective-scope GC semantics + uploaded-image post-commit ownership | DB-20 | DB + SC |
| `SC-01` | Scanner manifest/dependency/cancellation/domain-equality placement | SC-02..14 | SC |
| `SC-06/07` | Inaccessible-vs-deleted destructive-finalization policy; selective purge/GC scope; deterministic tie order | SC-06/SC-07 | SC + parent |
| `SC-12/13` | Watcher backend + external progress-IPC mode (deferred, not first-slice gates) | SC-12/13/SC-14 mode ledger | SC + parent |
| `D-PLAYER26` | **DECIDED 2026-11-18 — user-approved in-session (explicit): split #26.** Retain the player-session service (`Get`/`Register`/`Touch` over the landed `PlayerRepo`); **drop** the mpv/jukebox device service. Canonical register/touch placement belongs to API-01. See "#26 scope analysis" below. Neither half gates #27A. | `FRONT-26`, API-F1/F5, player wiring — resolved | parent/user |
| `D-PLAYER-EDIT` | **DECIDED 2026-11-18 — user-approved in-session (explicit): per-device preference editing is retained product scope.** A device profile is editable by its owner: `GET`/`PUT /api/player/{id}` stays canonical with ownership-restricted writes (`user_id` immutable, field whitelist enforced), and the profile list a client picks `transcoding_id` from needs a **read-only** `GET /api/transcoding` (profile *creation/editing* stays dropped, Part C item 1). The executing `transcoding_repo` read **landed at `f8ceeb8f`**, so the remaining prerequisites are the BOOT-owned `DEFAULT_TRANSCODINGS` seed and the route. See "Per-device editing (D-PLAYER-EDIT)" below. | API-00 registry capture, API-01, API-03, API-F1/F5, `FRONT-26` — read resolved; seed + route remain | parent/user |
| `plugin_repo` scope | **DECIDED 2026-11-20 (`../decisions.md` "Plugin scope (D-PLUGIN)")** — `plugin_repo.rs` is out of scope: the `plugin` table is dropped (`../schema-contract.md`, Part C item 14), `nd-dbschema`'s fresh DDL has no `plugin` table, and every statement the file renders fails with `no such table: plugin`. Decision: delete `rust/crates/nd-db/src/plugin_repo.rs`, its tests and the `lib.rs:35` export in a follow-up lane; Go's `plugin_cleanup.go` hooks are NOT ported. **EXECUTED at `7d022a02` (DB-16, 2026-11-21)** — `plugin_repo.rs` (243 lines, 3 tests) and the `lib.rs` export are deleted, the DB-21 class-3 probe is now literal SQL, prose mentions are reworded, schema goldens intact, and zero `plugin_repo` identifiers remain under `nd-db/src`. The `de0f49ea`/`632271a9` "deletion pending" wording above is dated history. | plugin surface in `FRONT-*` (dropped) | parent (decided); executed by DB-16 |
| Oracle harness | Approve new tracked Go harness under `testdata` (no Go-source edits) or an approved alternative; toolchain availability | EVID-02/03, SC-00 durable captures, and the **DB-15 durable annotation-transfer oracle deferred here (PARENT DECIDED 2026-11-20, `../decisions.md` H6)** | parent |
| Fixture/toolchain | Durable fixture location/recipe, synthetic-only credentials, `ffmpeg`/`ffprobe` provenance, native path disclosure/redaction | EVID-03, SC-00, API-06 | parent |

---

## Single launch order (serial writer lanes; read-only planning/review may overlap)

Two independent milestones from the boot-api lane, kept separate:

* **Track A — early HTTP:** runnable boot + fixture-backed, authenticated read/stream HTTP
  can proceed **without** the scanner.
* **Track B — real pipeline (#27A exit gate):** fresh DB → real track scan → authenticated
  lookup → direct Range → forced transcode → disconnect → zero surviving children. A fixture
  insert does **not** satisfy this.

### Step 0 — decisions & spec freeze (no code, parallel read-only)

`BOOT-00`, `API-00` (native wire-contract capture), `SC-00` (oracle/manifest prep),
`SC-01` (seam/dependency ratify — **RATIFIED NARROW 2026-11-21**, `../decisions.md` "SC-01 seams ratification": five seams + the `nd-scanner` `nd-db` manifest delta; progress sink, watcher, remote storage and `MusicFS`/`ReadTags` stay unratified) and `EVID-02` (oracle authorization gate). `plugin_repo` scope is **decided (D-PLUGIN)** — **deletion EXECUTED at `7d022a02`** (DB-16), see the decision table above.
`D-ERROR`/`D-TIME`/`D-OPEN`/`D-PLAYER26`/`D-PLAYER-EDIT` are recorded above, and `EVID-01`'s snapshot plus the
`DefaultGet`/gob/ProgressInfo contract amendments are already folded into these docs — its remaining part is durable
evidence capture, not new planning.

### Step 1 — DB persistence spine (one writer per checkout; isolated worktrees allowed where prerequisites are absent)

Dependency order: `DB-01` (**LANDED `f57ae050`**) → `DB-02` (**LANDED `6d9d984d`/`74ebd81c`**) → `DB-03` (**LANDED
`4a8aac9f`**) → `DB-00` (**LANDED `48e65806`/`ec84c176`, additive seam**) → `DB-04` (**LANDED `c791dbc1`**) → `DB-05`
(**LANDED `60be7534`**) → `DB-06` (**LANDED `bfdcfeca`**) → `DB-07` (**LANDED `2911b9ab`/`9be6ad0e`**) →
`DB-08` (**LANDED `32c4fa5b`/`320b8b03`**) → `DB-09` (**LANDED `d938d159`/`842c32a2`**) → `DB-10` (**LANDED
`65072b89`/`6e5ca97d`**) → `DB-11` (**LANDED `52b04064`: `D-CURSOR`/`D-SCOPE`, recorded in `../decisions.md`
(PARENT DECIDED 2026-11-20, corrected record `8b5b3925`), now consumed by executed code**) → `DB-12`
(**LANDED executed subset `e5d0592d`, rustdoc fix `f80b9704`**) → `DB-15` (**runtime half LANDED `96c495e7`** on the
render-only repair `ab466928`) → `DB-14` (**LANDED `de0f49ea`** — `SC.folder-write-batch-contract` resolved as D-BATCH and consumed) → `DB-21(P1-DB)` (**LANDED `632271a9`** — DB-side-only exit per the 2026-11-21 scope narrowing; external acceptances stay U in the executed U-ledger).
The forward spine is now `DB-18 → DB-19 → DB-20 → DB-21(FULL-SCANNER-DB)`: **DB-16 LANDED `7d022a02`** (executed move/reference-remap/delete primitives, A8 share typed rejection, D-PLUGIN deletion executed) and **DB-17 LANDED `b3dee0a6`** (5 files, +2817: four caller-tx count-refresh helpers + NEW `tests/runtime_count_refresh.rs` 20 executed tests; no production caller). `DB-18` is the next package and is gated on `D-STATS`, which still has no canonical record in `../decisions.md`; `DB-19` needs its boundary approval and `DB-20` needs `D-GC`.
`DB-15`'s **render-only** rating repair LANDED at `ab466928` and its **runtime half** — the typed annotation
reassignment/average-recalc transactions — LANDED at `96c495e7`; DB-15 is still **not** fully DONE because the durable
Go-harness oracle is deferred to the EVID lane (`../decisions.md` H6), and no annotation service or route caller exists.
`DB-13` (**LANDED `eae4ba1d`/`4ab63cd5`**) was inserted for BOOT/API revoke after `DB-00/03`; nothing calls it yet.
`DB-18 → 19 → 20`
extend phase 2+/GC for the full-scanner exit (**DB-17 LANDED `b3dee0a6`**).

### Step 2 — scanner (gated on DB releases; a local-FS subset and the SC-02 partial have landed)
`SC-00/01` decisions → `SC-02` → `SC-03` (**LANDED `cb59a973`**) → **`SC-05` first vertical** (needs the DB phase-1 write subset +
DB-11 typed folder query — **DB-11 LANDED `52b04064`, so that prerequisite is met**; `SC-01` was ratified **NARROW** on 2026-11-21 and
authorizes the `nd-scanner` `nd-db` manifest delta, so it is no longer an open gate) → `SC-04` (**READY**, prereq met, placement
`../decisions.md` D-SC04; **not** a prerequisite of SC-05) → `SC-06` complete phase 1 (**unauthorized**) → `SC-07` →
`SC-08` → (`PL-SERVICE` #23, then) `SC-09` → (`DB-20` released, then) `SC-10` → `SC-11` →
`SC-14` integrated in-process release. `SC-12/13` recorded early but off the critical path.
A local-filesystem storage/symlink-safe-classification subset LANDED at `e9dd8230`/`ff5b9c92` (std-`fs` list/stat/read/`read_link`/resolve/exists, `file://` URI parsing); it was a prerequisite-independent slice, **not** SC-01 ratification or SC-02 completion. **The SC-02 remainder (bounded walker, `.ndignore` ignore policy, folder hashing, cancellation) LANDED at `e3e0770a`**, fast-forwarded onto `2e7026b2` after rebase and re-review; the parent's gates at that HEAD are recorded in `../current-status.md`. That is still **partial**, not SC-02 completion: no scanner entrypoint, `MusicFS`/`ReadTags` registry dispatch, tag-to-DB ingest, full-scan orchestration or watcher. `SC-01` was **ratified NARROW on 2026-11-21** (`../decisions.md` "SC-01 seams ratification" — the five SC-05 seams plus the `nd-scanner` `nd-db` manifest delta), so SC-03+ proceed on those seams; the progress sink, watcher, remote storage and full `MusicFS`/`ReadTags` remain gated.

### Step 3 — boot/API (Track A early; Track B last)

`BOOT-00` (**LANDED `6d6938b1`** — gate/manifest/D1 boundary work; nothing listens) + `API-00` →
`BOOT-01` (after DB-05 seam; it owns the `ND_CONFIGFILE` resolution gap and B2 ffmpeg-config injection) →
`BOOT-03` HTTP shell → independent read-only/serialized `API-04` (#15 stream handshake) →
`BOOT-04` minimal CLI/service graph → `API-01` principal/key middleware — where the retained
`D-PLAYER26` player-session service gets its canonical placement (resolve/touch the player and its
transcoding row into the request context; provisioning a new row must use a validated
`PlayerRepo::put`, not `save` unchanged, since `save` at `player_repo.rs:1253` requires a
client-supplied key and returns `ApiKeyValidation` without one) → `API-02` scoped lookup (DB-11) →
`API-03` key/revoke (DB-13; issuance/revocation use the already-landed `set_api_key` `:1305`, not
`Register`/`Touch`) → `API-05` stream route + Axum body bridge → **`API-06` = #27A gate**, which
stays BLOCKED until `SC-05`/`EXT-SC-REAL-SCAN` and real ffmpeg disconnect evidence exist.
`API-F1…F7`, `BOOT-F1` are later full-service expansion. The full player-session contract
(write-throttle/`LastSeen` rules, default transcoding, owner rules, persisted `MaxBitRate` reaching
the stream decision and scrobbler gate, and the canonical meaning of the retained `report_real_path`
column) is `FRONT-26` plus a `player-service-contract.md` **to be authored there** — it does not exist
yet. No mpv/socket/process-reap scope belongs in any lane.

### Step 4 — evidence & operations

`EVID-01 → 02 → 03 → 04/05/06/07/08`; `OPS-01 → 02 → 03` (schema golden, integrity, FTS
rebuild), `OPS-04` logging, `OPS-05 → 06` maintenance, `OPS-07` remap dedicated plan. These
do not gate #27A; the durable Go oracle captures are the recommended evidence-strengthening
step now that code has landed.

---

## Current assignable packages (as of `6d6938b1`; DB-17 `b3dee0a6` and BOOT-00 `6d6938b1` added 2026-11-21)

**LANDED:** DB-10 signed implementation `65072b89` / signed merge `6e5ca97d`; DB-12 signed implementation `e5d0592d`
fast-forwarded, rustdoc fix `f80b9704`. Parent reports DB-10 review APPROVE with P3s fixed and DB-12 APPROVE after test fixes.
Neither has a production scanner caller; DB-12 is not a worker/full scanner. Former DB-10/12 worktrees and branches are
absent on the final verified worktree/ref check (external cleanup during this refresh, not performed by this lane).
**One sibling worktree still exists:** `~/.paseo/worktrees/0xo97qr3/boot00` on branch `boot00-manifest-pins-scaffold`, sitting at
the same commit `6d6938b1` with its BOOT-00 work already parent-integrated into this branch line — record it as integrated and
eligible for user cleanup; this lane did not archive, delete, or otherwise touch it.

**LANDED since that row, partial scopes:** **DB-11 `52b04064`** (executed typed scanner reads, the D-CURSOR cursor
contract, `ScannerScope`/`UserScope`, `get_visible_media`; 21 executed tests; no production scanner caller, no
`nd-scanner` consumption, not the full cursor/REST surface) and **DB-15's runtime half `96c495e7`** (annotation transfer
+ average-rating recalc on the caller's tx; 5 executed tests; durable Go oracle deferred to EVID — `../decisions.md` H6,
so DB-15 is not fully DONE), plus the **SC-02 partial** at `e3e0770a` (fast-forwarded onto `2e7026b2`): local
walker, `.ndignore`/ignore policy, folder hash and cancellation in `nd-scanner` with executed walker/ignore/hash/seam tests,
manifest/lock delta limited to the six approved dependencies (`decisions.md` S9, `b1aeea2c`; corrected S9b `2e7026b2`).
It is a partial slice — **not** SC-01 ratification, SC-02 completion or MusicFS readiness — and the former
`rust-sc02-local-walker` worktree and branch are now absent (parent-controlled merge, then cleanup).

**Landed since `ff900d3e`, partial scopes:** `DB-15` render-only repair `ab466928` (superseded on the runtime items by
`96c495e7`; the durable oracle and any annotation service/route remain NOT done),
scanner local-FS subset `e9dd8230`/`ff5b9c92` (NOT SC-01/SC-02), `DB-00` typed seam `48e65806`/`ec84c176` (seam only),
`DB-02` `6d9d984d`/`74ebd81c` (canonical TEXT stamps/cutoffs, `library_repo::zero_time_value()`, artwork-purge cutoff
with executed coverage; it left scrobble-buffer integer stamps and no executed pin for
`find_recent_files_by_properties_sql` open — those belong to the lane that owns those statements),
`DB-04` `c791dbc1`, `DB-05` `60be7534` (**no bootstrap caller: nothing creates or updates a configured library row, and
fresh DDL has none**), `DB-06` `bfdcfeca`, `DB-07` `2911b9ab`/`9be6ad0e`, `DB-08` `32c4fa5b`/`320b8b03` (which also removed
the force-appended `updated_at`), `DB-09` `d938d159`/`842c32a2` (FTS is maintained by schema triggers, not by a Rust
statement), and `DB-13` `eae4ba1d`/`4ab63cd5`. None of the executed `*_in_tx` write primitives has a **production** caller;
`nd-auth` does call `UserRepo` methods (`find_by_username`, `count_all`, `count_by_username`, `put`,
`update_last_login_at`) from its own production code, but that router is mounted only in crate tests, not in a server.

**Ready now:**

* **DB-22 LANDED `272c9c66`** (parent-validated 2026-11-20): executed genre/scrobble/playqueue/artwork reads with Go-compatible scalar codecs, strict timestamp scanning, BLOB-coercion fixtures, an allowlisted safe sort, and all-or-error queue chunks. C5 documents the narrow boundary (no expression/random sorts, no optioned Genre reads).
* **DB-11 LANDED `52b04064`** and **DB-15's runtime half LANDED `96c495e7`** (dated parent measurements 2026-11-20): the
  executed scanner read/cursor contract with typed scope authorities, and annotation transfer + average-rating recalc on
  the caller's tx. Both stay partial — no production caller, and DB-15's durable Go oracle is deferred to the EVID lane
  (`../decisions.md` H6). **DB-14 LANDED `de0f49ea`** (per-folder persistence seam; `SC.folder-write-batch-contract` resolved as
  **D-BATCH** — per-folder tx opened by DB, `persist_folder_delta_in_tx` pub(crate), parent-controlled `album_repo.rs` copy helper
  `copy_album_created_at_in_tx` at `album_repo.rs:1566`; `../decisions.md` "Folder-write batch contract"). **DB-21(P1-DB) LANDED `632271a9`** (DB-side-only exit contract: 4206 lines, 7 executed tests, 51 SigNN pins, executed U-ledger — `../decisions.md` scope narrowing 2026-11-21), **DB-16 LANDED `7d022a02`** (executed move/reference-remap/delete primitives, A8 share typed rejection, D-PLUGIN deletion executed; 18 executed tests) and **DB-17 LANDED `b3dee0a6`** (four caller-tx count-refresh helpers, 20 executed tests), so **next is DB-18** (pending `D-STATS`) on the spine toward DB-21(FULL-SCANNER-DB). DB-18..20 retain later decisions (`D-STATS`, `D-GC`) and their
  DB-11/DB-15 dependencies, which are now satisfied. DB-21(FULL) still owns the collection/count API for later API-F*, and no executed write has a production caller (SC-05 is DB-14's consumer; SC's phase-3 triggers are DB-17's).
* **`SC-00` / `API-00`** (read-only/spec) — oracle manifest + native wire-contract capture, pending the
  oracle-authorization decision above.
* **`OPS-01`** — re-derive the schema golden and keep DDL/data-seeding separate; prerequisite-independent and it also
  settles where the `DEFAULT_TRANSCODINGS` seed belongs (BOOT/#05 policy, never static DDL).
* **`BOOT-01`** — **BOOT-00 LANDED `6d6938b1`**, so BOOT-01 is the next launchable boot package; it owns the `ND_CONFIGFILE`
  resolution gap and the B2 ffmpeg-config injection. `API-00` is likewise no longer blocked on BOOT-00.

**Done (dated parent measurements):** `DB-01` `f57ae050`, `DB-03`/`D-OPEN` `4a8aac9f`, sort gate `62789ba2`,
executed transcoding reads `f8ceeb8f`, `DB-00` `48e65806`/`ec84c176`, `DB-02` `6d9d984d`/`74ebd81c`, `DB-04` `c791dbc1`,
`DB-05` `60be7534`, `DB-06` `bfdcfeca`, `DB-07` `2911b9ab`/`9be6ad0e`, `DB-08` `32c4fa5b`/`320b8b03`, `DB-09`
`d938d159`/`842c32a2`, `DB-13` `eae4ba1d`/`4ab63cd5`, `BOOT-02` `c6e220eb`/`162fae1d`, `DB-11` `52b04064`,
`DB-15` runtime half `96c495e7`, `DB-14` `de0f49ea`, **`DB-21(P1-DB)` `632271a9`** (DB-side-only P1 exit contract; not the full exit, no production caller), **`DB-16` `7d022a02`** (executed move/reference-remap/delete, A8 share rejection, D-PLUGIN deletion; no production caller), **`DB-17` `b3dee0a6`** (four caller-tx count-refresh helpers, 20 executed tests, no production caller) and **`BOOT-00` `6d6938b1`** (manifest pin repair + D1 dependency boundary + executed cycle test; gate work only, no production behavior). Historical gates at `4ab63cd5` are
recorded in [`../current-status.md`](../current-status.md); the latest parent gates at `6d6938b1` (98 suites, 2363/0/1; `nd-db` 1073/0/0 across 49 suites, `nd-http` 1/1, `nd-db` rustdoc 21 = post-DB-16 baseline), the `7d022a02` block (97 suites, 2343/0/1; `nd-db` 1053/0/0 across 48 suites), the
`632271a9` block (96 suites, 2326/0/1), the `de0f49ea` block (95 suites, 2319/0/1), the `96c495e7` block (94 suites, 2302/0/1), the intermediate
`52b04064` block (2297/0/1), the `e3e0770a` block (92 suites, 2276/0/1) and the earlier `f80b9704` block are recorded there as well.

Everything else waits on a recorded decision or an earlier release. Implementation assignments go one writer per
checkout (or one per approved isolated worktree), each with its own fresh review and parent verification.

---

## #26 scope analysis (players / playback / mpv)

#26 bundles two unrelated things. Verifying against the Go source splits them cleanly:

**1. Player-session service — worth building canonically, so port it.** `core/players.go` (`Get`/
`Register`/`Touch`) plus `model/player.go` record fields. Important: this lifecycle does **not**
exist on the canonical surface today — Go registers/touches players only on the cancelled
Subsonic/Jellyfin surfaces (see below) — so this is a capability to **choose and build**, not
behavior inherited from a live path. Its fields feed services that survive the port:

- `MaxBitRate` → the transcode decision, already ported and tested in
  `nd-stream/src/legacy_client.rs` (`player_max_bit_rate` forces downsampling,
  `decider.go:159-161` precedence). The full chain is
  `PlayerRepo::get`/`find_match`/`put` → the `Players` service resolves the player and its
  transcoding row → request context (`request.PlayerFrom`) → `decider.go:159-161` →
  `nd-stream` `DecisionOptions::player_max_bit_rate` (`types.rs:265`; `legacy_client.rs:38,55-59,110-112`).
  **Nothing registers or persists the player row and no production caller sets that field**, so the
  cap is dead in practice — an integration gap, not a missing feature.
- `ScrobbleEnabled` → the scrobbler (#20) gates Now Playing and scrobbles on it
  (`core/scrobbler/play_tracker.go:366,436,473`).
- `TranscodingId` → Go's `refresh` also loads the transcoding row alongside the player
  (`core/players.go:87-91`). This is a real second dependency, and with `D-PLAYER-EDIT` it is a
  **first-class** one — a device cannot choose a profile it cannot list:
  * `nd-db/src/transcoding_repo.rs` rendered SQL only when this analysis was written (`get_sql` :77,
    `find_by_format_sql` :87, `read_all_sql` :114, `put_sql` :147, `delete_sql` :202). **Correction 2026-11-19:** the
    executed reads **landed at `f8ceeb8f`** — `read_all`/`count_all` execute and mask `command` for a non-admin; `put` and
    `delete` remain renderers. The seed gap below is still open.
  * `nd-dbschema/src/schema_ddl.rs:56` creates the `transcoding` table but inserts nothing, so a fresh
    Rust DB has **zero profiles**. Go seeds them (`db/migrations/20200706231659_add_default_transcodings.go:33`,
    one `id.NewRandom()` per row when the table is empty; `20260309203355_ensure_default_transcodings.go`
    tops up missing defaults idempotently). The definitions are already carried in Rust as
    `nd-conf` `DEFAULT_TRANSCODINGS` (`rust/crates/nd-conf/src/consts.rs:242`, exactly 4 entries: mp3,
    opus, aac, flac — byte-identical commands to `consts/consts.go:176-206`), so the seeding gap is a
    wiring decision, not missing data — but seeding belongs to BOOT/#05 policy, never to static DDL
    (`OPS-01` keeps DDL and data seeding separate). Upgrade-only migrations
    (`20260307175815_add_codec_and_update_transcodings.go`, `20260618120509_add_metadata_to_default_transcodings.go`)
    rewrite rows that still hold an *older* unmodified default, so an upgraded Go DB can legitimately hold
    a command neither table has (e.g. the ipod/fragmp4 AAC row). That is an upgrade-state divergence, not a
    fresh-install one. `player.transcoding_id` has no foreign key (`schema_ddl.rs:65`), so an unknown id is a
    silent no-profile rather than an error — worth an explicit validation choice at the API edge.
  Neither the read nor the seed is provided by `D-PLAYER26`; the player service cannot resolve or offer
  a profile until they land.
- `ReportRealPath` is a retained DB/preferences **column**, not an inherited consumer: its only
  non-test reader is the cancelled Subsonic wire (`server/subsonic/helpers.go:227`, absolute path
  vs fake path) and its seeding key `Subsonic.DefaultReportRealPath` is itself dropped
  (`config-contract.md:220`, sole reader `core/players.go:56`). Its canonical meaning must be
  re-specified in FRONT-26/API-01 rather than assumed to keep this half load-bearing.
- Key issuance/revocation consumes the **repository** (`PlayerRepo::set_api_key` `player_repo.rs:1305`,
  `save` `:1253`) plus a player row — it does **not** need `Players::Register`/`Touch` and does not
  depend on #15. It needs the service only if provisioning a row without a client-supplied key is
  folded into it.
- The `player` table, `PlayerRepo` CRUD (`get` :1119, `find_match` :1140, `find_by_api_key` :1156,
  `put` :1215, `save` :1253, `set_api_key` :1305, `delete` :1384) and unsalted-sha256
  `hash_api_key` :378 already execute in `nd-db/src/player_repo.rs`; only the service lifecycle
  above them is missing. Go's lifecycle has **no expiry mechanism**: `refresh` stamps
  `LastSeen = time.Now()` (`players.go:78`) and throttles writes with
  `utils.Limiter{Interval: consts.UpdatePlayerFrequency}` (`players.go:26`, `consts/consts.go:89` =
  1 minute). Nothing ages or deletes player rows; the only `last_seen` filter is
  `core/metrics/insights.go:306` (7 days, #32 deferred).

There is **no mpv dependency**. Where registration happens today: Go registers/touches players only
in the Subsonic middleware (`server/subsonic/middlewares.go:291-316`, `Touch` :305 / `Register` :307,
`WithPlayer`/`WithTranscoding` :312-314), mounted on 13 of the 14 protected groups in
`server/subsonic/api.go` at 107, 113, 133, 143, 151, 159, 169, 174, 179, 184, 199, 210, 222; the outer authenticated
group also serves routes with no player resolution — the throttled `getCoverArt` group :193-197 and the `h501`/`h410`
stubs from :230), plus Jellyfin's `withPlayer` (`server/jellyfin/middlewares.go:193-224`, `Register` :209, mounted at
`server/jellyfin/api.go:110`). Both surfaces are cancelled. The native API never registers or touches: it exposed
plain rest-CRUD `rx(r, "/player", api.ds.Player(), true)` (`server/nativeapi/native_api.go:75`), and
API-key auth only attached the key's player (`server/subsonic/middlewares.go:182`). So the canonical
surface must **deliberately choose** where register/touch runs (recommend: the API-01 principal/API-key
middleware, resolving the player plus its transcoding row into the request context). That placement is
API-01 design work, not behavior inherited from a live path; without it the stream `MaxBitRate` cap and
scrobbler gate stay dead.

**2. mpv / server-side playback device — a genuinely optional niche feature, DROPPED by
`D-PLAYER26`.** `core/playback/**` (`playbackserver`, `device`, `queue`, mpv IPC) spawns a real
`mpv` process on the **server host** and drives it over a unix/named pipe — the Subsonic
`jukeboxControl` feature. Evidence for dropping:

- Its **only entry point is Subsonic**, which C20 cancels (`server/subsonic/api.go:220-226`,
  `jukebox.go`). No canonical native route exposes it and no replacement exists elsewhere.
- It is **off by default** (`jukebox.enabled` false, admin-only true,
  `conf/configuration.go:1058-1061`), and `cmd/root.go:372-380` no-ops when disabled.
- The settled config contract already drops `Jukebox.*`, `MPVPath`, `MPVCmdTemplate`
  (`config-contract.md:223`; `nd-conf` `keys.rs`/`config.rs` mark them absent).
- The bundled JS UI — the main way anyone reached it — is DROPPED (C25; the ~47k-LOC `ui/src` tree.
  C32 is backgrounds, a separate drop. The only surviving JS mention is a `[Jukebox]` config-dump
  label in `ui/src/dialogs/aboutUtils.test.js:368,379`, not a playback path).
- It spawns processes on the host and needs OS-specific socket handling plus process reap;
  `playbackserver.go:54` leaves subprocess termination as an unfinished TODO, so even upstream it is
  incomplete. It also needs an external `mpv` binary nobody else in the port requires.

No **surviving** code consumes `core/playback/**`. The complete consumer set, for the record, so a
later sweep does not read these as contradictions: the Subsonic entry point
(`server/subsonic/api.go:19,52,61`, `jukebox.go:8,47`), DI/boot plumbing
(`core/wire_providers.go:35`, `cmd/wire_injectors.go:150`, `cmd/wire_gen.go:22,112,227-230`,
`cmd/root.go:90,372-382`), and three cancelled-surface e2e suites that pass
`playback.PlaybackServer(nil)` (`server/subsonic/e2e/e2e_suite_test.go:429`,
`subsonic_artwork_test.go:260`, `subsonic_sonic_similarity_test.go:50`). `GetDeviceForUser` is only
"the default device" (`playbackserver.go:117-127`) and is called solely from `jukebox.go:47`.
`Jukebox.Enabled` has two further readers, both on non-surviving scopes:
`server/subsonic/users.go:29-30` (`jukeboxRole`, cancelled #25) and `core/metrics/insights.go:218`
(#32, deferred). So dropping it removes no capability any surviving service needs.

**Decision (`D-PLAYER26`, user-approved in-session 2026-11-18)**: split #26 — retain the player-session
service as scoped work (register/touch/get + write-throttled persistence, whose resolved player and
transcoding row feed the stream decision and the scrobbler gate), with its canonical placement chosen
in API-01; mark the mpv/jukebox device service **DROPPED**, as justified above. Neither half gates
#27A, and the retained half is API/BOOT-adjacent work rather than a monolithic `nd-players` crate.
Reopening jukebox would require a dedicated platform/socket/process-reap contract and a canonical
(non-Subsonic) route surface; it is not scheduled.

---

## Per-device editing (D-PLAYER-EDIT, user-approved in-session 2026-11-18)

**Go exposes the routes today; the Rust repository legs are landed but nothing calls them.**
`GET`/`PUT`/`DELETE /api/player` are real native Go routes (`server/nativeapi/native_api.go:75`,
`rx(r, "/player", api.ds.Player(), true)` — the **fourth** argument, `persistable` (`:106`), is what enables
POST/PUT/DELETE; GET stays open regardless). On the Rust side the repository already executes the
ownership-safe legs — but `nd-api` is a scaffold (`rust/crates/nd-api/src/lib.rs`, 765 bytes), no `/api`
router is mounted anywhere in `rust/crates`, and `PlayerRepo` has zero non-test callers. So **no endpoint
works end to end yet**; "editable" below means repository capability plus the Go oracle, not a live route:

- `update_owned_sql` (`player_repo.rs:961`) drops `user_id`/`created_at` from the SET, keeps `id`, and
  restricts via `owned_row` — mirroring `updateOwned` (`persistence/sql_base_repository.go:506-514`,
  which does `delete(updateValues, "user_id") // ownership is immutable on update`) and `ownedRow`
  (`:97-102`): `ownerOnly` pins `user_id = loggedUser`, `ownerOrAdmin` falls back to `addRestriction`.
- Both legs exist: `update_owned` uses `WriteAccess::OwnerOrAdmin` (:1345), while `set_api_key` keeps
  the split — revoke is `ownerOrAdmin`, **mint is `ownerOnly`** (:1305), so an admin cannot hand someone
  else a login. The field whitelist is enforced (`:538-549`: ID, Name, UserAgent, UserId, Client, IP,
  LastSeen, TranscodingId, MaxBitRate, ReportRealPath, ScrobbleEnabled).
- `PlayerRepo::read(id, user)` (:1129) is the ownership-**restricted** read and `get(id)` (:1119) is
  unrestricted. Which one a route calls is the entire authorization decision for `GET`.

**Editable by owner once a route exists, with no new logic.** `TranscodingId`, `MaxBitRate` and
`ScrobbleEnabled` are settable through that ownership-restricted path today (exercised by the crate’s own
tests only). `MaxBitRate` is precisely the field the stream decision already honors
(`core/stream/decider.go:159-161` → `nd-stream` `DecisionOptions::player_max_bit_rate`,
`legacy_client.rs:38,55-59`), so "lower the bitrate on my phone" needs only a mounted route and one
persisted row — no new decision logic.

**Two gaps to close, plus one carve-out:**

1. **No profile list to pick from.** `transcoding_id` is meaningless to a client without a read. Go's
   `GET /api/transcoding` existed but its write access was gated on `EnableTranscodingConfig`
   (default false, `native_api.go:76`), and Part C item 1 drops the **config editor**. With this
   decision, a **read-only** `GET /api/transcoding` returning id/name/target_format/default_bit_rate
   becomes required API scope. `mask_command`/`mask_commands` (`transcoding_repo.rs:60,69`) implement Go's
   masking (`persistence/transcoding_repository.go:54-60,:75-78`) but **only hide the command when a caller
   applies them** — today they have no caller outside the crate's tests, so the route must apply them.
   Profile creation/editing stays dropped. Note that `EnableTranscodingConfig` is **carried** in the fresh
   TOML (`nd-conf/src/keys.rs:189-193`, `config.rs:435,:662`; it appears in the frozen key list at
   `tests/loader.rs:27`, and `config-contract.md` §1 "TOML root keys (carried)" lists it at `:60`), so the key
   is accepted and parsed — it simply has **no consumer** outside `nd-conf`. Do not describe it as removed:
   unknown keys are fatal, so dropping it later would be its own contract change.
2. **No seeded profiles; the executing read landed** — see the `TranscodingId` bullet above. Without rows a picker
   renders empty, and `transcoding_repo::read_all`/`count_all` have executed since `f8ceeb8f`, so only the seed remains.
   `DEFAULT_TRANSCODINGS` already exists in
   `nd-conf/src/consts.rs:242` (exactly 4 entries — mp3, opus, aac, flac — with commands byte-identical to
   `consts/consts.go:176-206`), so this is wiring, not missing data. Seeding belongs to BOOT/#05 policy;
   static DDL stays data-free (`OPS-01`).
   `player.transcoding_id` carries no foreign key (`schema_ddl.rs:65`), so an unknown id is silently
   "no profile" rather than an error — pick explicit validation at the API edge.
3. **Carve-out:** `ReportRealPath` remains editable-but-meaningless until its canonical semantics are
   re-specified (its only reader was the cancelled wire, `server/subsonic/helpers.go:227`). Do not put
   it in a client-facing picker as though it does something: either define canonical path reporting in
   FRONT-26 or hide it. Keep `user_id` immutable, never accept an owner/admin field from the request
   body, and treat the route shape as a **new** contract captured in API-00 — Go's rest-CRUD shape is
   reference data, not the frozen native contract.

**Where it lands:** API-00 captures the player/transcoding wire contracts before any handler; API-01
owns principal resolution and ownership enforcement; **API-03 owns the player route file
`nd-api/src/player.rs`** (key issuance plus profile `GET`/`PUT` — no PATCH verb in this contract);
API-F1/F5 own the read-only transcoding list and `nd-api/src/player_preferences.rs`, and must not
duplicate that route; FRONT-26 owns the service semantics and `player-service-contract.md` (to be
authored). #05 owns the executing `transcoding_repo` read (**landed `f8ceeb8f`**); #04/#05 the seed policy. **#27A is unchanged:**
its exit criteria remain fresh DB → real track scan → authenticated lookup → direct Range → forced
transcode (`subplans/boot-api.md` §1), and a fixture-inserted player only proves the stream-cap hook. No
part of `D-PLAYER-EDIT` gates that slice.

**Decision (`D-PLAYER-EDIT`, user-approved in-session 2026-11-18):** retain per-device preference
editing as canonical scope — owner-scoped player `GET`/`PUT` with immutable ownership and an enforced
field whitelist, a read-only transcoding profile list, an executing `transcoding_repo` read, and a
BOOT-owned default-profile seed. Transcoding profile **editing**, `ReportRealPath` semantics, and any
jukebox surface stay out of scope; `EnableTranscodingConfig` keeps its carried-but-unused status. Recorded update
2026-11-19: the executing read in that prerequisite list **landed at `f8ceeb8f`**; the seed and the canonical routes
remain.

**Status-code parity (measured, not assumed).** Go distinguishes the two failure kinds and the port
already mirrors both: a cross-owner **read** yields `model.ErrNotFound` (the ownership-restricted select
matches no row — pinned by `persistence/player_repository_test.go:163-167`) → 404, while a cross-owner
**write** yields `rest.ErrPermissionDenied` via `classifyOwnedWriteMiss`
(`persistence/sql_base_repository.go:554-563`, pinned at `:139`) → 403. Rust equivalent:
`player_repo.rs:1063-1069` `classify_owned_write_miss`. So the canonical contract is **404 on
cross-owner reads, 403 on cross-owner writes**. Collapsing both to 404 to suppress enumeration is a
deliberate deviation API-00 must record as an accepted difference — not parity, and not silent.
