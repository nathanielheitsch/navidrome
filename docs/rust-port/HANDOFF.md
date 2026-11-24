# Handoff — bounded-task workflow for the Rust port

Read the entry point [`README.md`](README.md) first. This file states how a bounded task is launched, executed,
reviewed and reported. It does **not** carry status: current milestone order is the current section of
[`next-work.md`](next-work.md), measured evidence is [`current-status.md`](current-status.md), binding decisions are
[`decisions.md`](decisions.md), and bounded assignments are the [`subplans/`](subplans/) contracts. It authorizes
nothing — no implementation, no commits, no new workspaces, no completion claims.

## Locate and verify

- Repository: this navidrome fork checkout (Go sources at the root, Rust workspace `rust/`). Resolve paths from the
  working directory — never a developer's absolute path, and never a hardcoded worktree name.
- Authoritative source baseline: this branch line at `cb59a973` — **SC-03 LANDED `cb59a973`** (the `nd-mapping` tag→record
  bridge + the singleton folder projection, 19 executed tests) on top of **BOOT-00 LANDED `6d6938b1`** (gate/manifest/D1
  boundary work only) on top of **DB-17 `b3dee0a6`**, **DB-16 `7d022a02`**, **DB-21(P1-DB) `632271a9`**, **DB-14 `de0f49ea`**, **DB-11 `52b04064`** and
  **DB-15's runtime half `96c495e7`**, which sit on integrated `master`'s code HEAD `e3e0770a` (**SC-02 partial** — local walker,
  `.ndignore`, folder hash, cancellation — fast-forwarded onto `2e7026b2`), which extends `272c9c66` (DB-22) and `f80b9704` (prior
  `4ab63cd5` plus DB-12 signed `e5d0592d`, DB-10 signed `65072b89` / signed merge `6e5ca97d`, DB-12 rustdoc fix `f80b9704`).
  That SC-02 slice is partial and claims neither SC-01/SC-02 completion nor MusicFS readiness; DB-11 is the executed
  read/cursor contract only, DB-15 is its runtime half only (durable Go oracle deferred — `decisions.md` H6), and DB-14 is the
  executed folder-delta persistence seam only. **SC-05 is LANDED AND INTEGRATED at `5c463aa1`** (signed, pushed to
  `origin/resuscitate-dead-agent-capture-worktree-state`, and **fast-forwarded into `master` at `342afd12`**):
  the
  phase-1 vertical (`nd-scanner/src/phase1.rs`, `tests/phase1_{slice,faults,fixture}.rs`, `tests/common/mod.rs`, plus the
  `src/{lib.rs,walk.rs,projection.rs}` and manifest/lock delta) is committed in 10 files, +9103/−54 and is now mainline, so
  `master` has an in-crate caller of the DB-11 read and DB-14 write seams — but `phase1::scan_folder` still has **no production
  entrypoint** (test harness only) and the DB-21 U-ledger entries stay U. A docs
  lane may be cut from an earlier boundary (`ff900d3e`, `74ebd81c`) — say so, and cite integrated behavior from `master`
  instead of measuring it in your own worktree. Earlier audited boundaries are listed in `current-status.md`'s
  landed-history table; older handoff baselines are history. Two docs-only boundaries sit above the code HEAD (`f3bb81b2`,
  and the `b3dee0a6`/`6d6938b1` pair carries the DB-17 and BOOT-00 code); the docs lane that synced them ran at `6d6938b1`.
- Read order: `README.md` → `next-work.md` **current section only** → `current-status.md` → `decisions.md` → the
  subplan index → the assigned package and its prerequisites → the relevant source contract. Contracts
  (`schema-contract.md`, `config-contract.md`, `credentials.md`, `entity-write-contract.md`, `hydrate-contract.md`,
  `hydrate-scan-contract.md`) carry source history; their **current amendments** override historical scout assertions.
  Product scope/ownership lives in `issues.md` and `component-map.md`, as amended by the current plan — dated
  historical tables there do not override the snapshot or these orchestration rules. Proposed API signatures in a
  subplan are not automatically frozen contracts.
- Start every assignment with `git log -5 --oneline`, `git status --porcelain --untracked-files=all`, and the assigned
  method/test sources. Report baseline drift, existing changes, unmet prerequisites and missing decisions **before**
  editing. Never discard another writer's work. Resolve fixtures from `CARGO_MANIFEST_DIR`/repository discovery.

## Current reality (see `current-status.md` for numbers)

The workspace passes its regression gates and is **not** a runnable server or scanner: `nd-scanner` holds the tested
local-FS walker slice (SC-02 partial at `e3e0770a`) and the **SC-03 mapping bridge + singleton projection** (`mapping_bridge.rs`,
`projection.rs` at `cb59a973`), but on the integrated tree has **no scanner entrypoint, production tag-to-DB pipeline or full-scan
orchestration** — the first caller of the DB-11 read seam and DB-14 write seam is **integrated on `master`**
(`5c463aa1`, fast-forwarded at `342afd12`; see `current-status.md`'s SC-05 block), yet `phase1::scan_folder` is invoked only from
`nd-scanner/tests/common/mod.rs`, so no bootstrap or route performs a scan; `nd-http` is BOOT-00's typed
dependency-boundary module (`RouterInjection` plus the executed cycle test — **no router, no listener**), `nd-api` remains a scaffold, and the CLI `main` is still empty at `6d6938b1`. Shared codecs, record marshaling, the eight row
hooks, substantial
executed reads **and thirty-six executed transaction-bound `*_in_tx` primitives** (33 `pub`, plus three `pub(crate)`:
DB-14's `persist_folder_delta_in_tx` and DB-16's `move_matched_mediafile_in_tx`/`remap_missing_mediafile_in_tx`, which the
`pub` store-level entrypoints `move_matched_mediafile`/`remap_missing_mediafile` wrap; **amended at `b3dee0a6` — DB-17 added four
`pub` refresh helpers**, so the count moves 29→33 `pub` and 32→36 total) are landed, including DB-10 identity/base/
junction writes and DB-12 artwork invalidation/enqueue; **DB-11 LANDED `52b04064`** adds the executed typed scanner reads,
the D-CURSOR cursor contract, the `ScannerScope`/`UserScope` authorities and the `get_visible_media` point read (21
tests), **DB-15's runtime half LANDED `96c495e7`** adds annotation transfer + average-rating recalc (5 tests), and
**DB-14 LANDED `de0f49ea`** adds executed per-folder persistence (`FolderWriteBatch`, `persist_folder_delta`,
`CommittedFolderOutcome`, `FolderWriteStage`/`ScopeRejection`/`FolderWriteWarning`, `scan_artwork_item`; 17 executed tests), and
**DB-21(P1-DB) LANDED `632271a9`** adds the executed DB-side-only P1 exit contract (composed regression + API freeze + U-ledger;
no production caller, not the full exit), and **DB-16 LANDED `7d022a02`** (10 files, +3507/−308) adds the executed move/reference-remap
primitives (`move_matched_mediafile` with `WithTxRetry` semantics via `try_with_tx_retry_sync`, `remap_missing_mediafile` on a single
`try_with_tx` — no retry, NEW `reassign_references_in_tx` composing DB-15 annotation + avg-recalc with the executed bookmark leg and the
playlist/scrobble/buffer legs in Go order, `delete_mediafile_in_tx`, `delete_all_missing_in_tx` opt-in), the **A8** typed
`ShareError::EmptyUpdateColumns` rejection, and the **D-PLUGIN deletion** (`plugin_repo.rs` + `lib.rs` export gone); 18 executed tests,
no production caller (SC-06/BOOT maintenance are the consumers), and **DB-17 LANDED `b3dee0a6`** (5 files, +2817 additions-only) adds the
four caller-tx count-refresh helpers — `album_repo::refresh_play_counts_in_tx`, `artist_repo::refresh_play_counts_in_tx`,
`tag_repo::update_counts_in_tx`, `library_repo::refresh_library_totals_in_tx(id, now)` (8 probes then the 10-bind update; Go discards
`RowsAffected` at `library_repository.go:249` and Rust returns the written `RefreshStatsTotals` — documented deviation) — with
NEW `tests/runtime_count_refresh.rs` = 20 executed tests. Pinned semantics: bare `play_date = excluded.play_date` (Go's REPLAY
backwards-overwrite is faithful, **no monotonic guard added**), zero-arg statements, caller-stamped canonical TEXT (no clock in SQL),
NULL-exclusion, statement-level `RowsAffected` vs entity counts, probe-then-update order, and **rollback stays the CALLER's
obligation via the DB-00 typed seam** (all four helpers' docs say so). Typed
specialized scanner reads are no longer "not landed" — count refresh landed at `b3dee0a6` and move/reference at `7d022a02`, while GC (DB-20) still is — and neither DB-11's
reads nor any write has a **production** caller: `nd-scanner/src/phase1.rs` calls both the DB-11 read and the DB-14 write seam and
is integrated on `master` since `5c463aa1`/`342afd12`, but `scan_folder` is reachable only from the crate's test harness, so no
bootstrap, route or scanner entrypoint drives them. Tests that manually execute SQL bundles are not production repository methods,
production code, but its router is mounted only in crate tests). DB-01 (missing-state bind order), DB-03/D-OPEN
(effective per-connection configuration), the sort gate, DB-00 (typed transaction/error seam, `48e65806`/`ec84c176`),
DB-02 (`6d9d984d`/`74ebd81c`), DB-15's **render-only** rating repair (`ab466928`) **and its runtime half (`96c495e7`)** are
landed with executed regressions, as
are the executed write lanes DB-04 `c791dbc1`, DB-05 `60be7534`, DB-06 `bfdcfeca`, DB-07 `2911b9ab`/`9be6ad0e`, DB-08
`32c4fa5b`/`320b8b03`, DB-09 `d938d159`/`842c32a2` and DB-13 `eae4ba1d`/`4ab63cd5` — DB-15's durable Go oracle is
**deferred to the EVID lane** (`decisions.md` H6), no annotation service or route exists, and the
full DB lane is **not** done. **DB-10 and DB-12's executed subset are LANDED** (commits above), with no production scanner caller; DB-12 is not
worker/full scanner completion. The **SC-02 partial is LANDED at `e3e0770a`** (fast-forwarded onto `2e7026b2` after
rebase; review findings through S9b closed, the S9 dependency set recorded at `b1aeea2c` and corrected at `2e7026b2`):
local walker, `.ndignore`/ignore policy, folder hash and cancellation with executed walker/ignore/hash/seam tests. That is
a **partial slice** — no scanner entrypoint, `MusicFS`/`ReadTags` dispatch, tag-to-DB ingest, full-scan orchestration or
watcher, and **not** SC-01 ratification, SC-02 completion or MusicFS readiness; the former `rust-sc02-local-walker`
worktree/branch are removed. **DB-11 LANDED `52b04064`** (executed typed scanner reads, cursor contract,
`ScannerScope`/`UserScope`, `get_visible_media`) and **DB-15's runtime half LANDED `96c495e7`** (annotation transfer +
average-rating recalc on the caller's tx); `D-CURSOR`/`D-SCOPE` (`decisions.md`, corrected record `8b5b3925`) are now
consumed by executed code, and **DB-14 LANDED `de0f49ea`** (`SC.folder-write-batch-contract` resolved as **D-BATCH** and consumed:
per-folder tx opened by DB, `persist_folder_delta_in_tx` `pub(crate)`, warn-vs-fatal table, SC pre-generated IDs, immutable
no-clock batch, and the parent-controlled `album_repo.rs` copy helper — landed as `copy_album_created_at_in_tx`,
`album_repo.rs:1566`, which DB-16 reuses). DB-22
LANDED `272c9c66`. **DB-21(P1-DB) LANDED `632271a9`** — one file, NEW `nd-db/tests/runtime_scanner_persistence_contract.rs` (4206 lines, 7 executed tests): the **DB-side-only P1 exit** per the parent scope narrowing of 2026-11-21 (`decisions.md` "DB-21 exit scope and prepared-statement gates"). It executes composed regression through the real pool (4 deltas / 2 libraries / 3 album-ID transitions, both `full_scan_mode` values, a second pass over 7 committed dirty-state arms with stored-effect assertions, plus a control pass), the frozen API surface (51 compile-checked SigNN pins across the landed phase-1 primitives plus DB-11/14/15 public surface; Go literals 50/600/200/100 pinned; an executed unknown-album queue-exclusion sentinel), and an executed **U-ledger** asserting `SC.mapping-bridge`, `SC.phase1-fixture`, `BOOT.configured-library-caller` (gate **BOOT-01 implementing**, BOOT-00 a prerequisite — now met at `6d6938b1`, but approval is not delivered caller evidence) and `production_full_finalize_db_contract` as **Unmet**, so they cannot drift green. **BOOT-00 LANDED `6d6938b1`** (6 files, +177/−19, an isolated-worktree lane merged by the parent and re-verified on the merged tree): the **D3 manifest pin repair** executed (`chrono ^2`→`0.4`, `tracing ^0.5`→`0.1`, `tracing-subscriber ^0.1`→`0.3`, `base64 ^0.8`→`0.22`; **zero NEW lock entries** — the lock diff is only the `nd-http` dependency-edge stanza) and the **D1 boundary** — the `nd-http` scaffold became a typed module carrying the **EXECUTED** `dependency_graph_has_no_http_api_cycle` acceptance (`cargo metadata --locked` graph check plus a negative control) and the `RouterInjection = fn(axum::Router, &nd_conf::Config) -> axum::Router` shape; `nd-cli`/`nd-api` carry rustdoc boundary notes (the `ND_CONFIGFILE` resolution gap and the `Dir::path` `create_dir_all` hazard — **BOOT-01 owns both**); **D4** was already resolved by B3-ratified. **No production behavior changed**: `main` is still empty and nothing listens. Latest parent gates at `6d6938b1` (Homebrew cargo/rustc **1.99.1**, EXECUTED on the merged tree): workspace **2363/0/1 across 98 suites**, `-p nd-db` all-targets **1073/0/0 across 49 suites**, `nd-http` **1/1** (the cycle test), fmt, workspace clippy `-D warnings` and `git diff --check` clean, `nd-db` rustdoc **21 warnings = the post-DB-16 baseline**. Arithmetic: `2343` at `7d022a02` `+ 20` (DB-17's suite) `+ 0` (BOOT-00: `+1` cycle test `− 1` dropped `nd-http` scaffold test) `= 2363`. The prior gates at `7d022a02` (**2343/0/1 across 97 suites**, `-p nd-db` **1053/0/0 across 48 suites**), at `632271a9` (**2326/0/1 across 96 suites**, `-p nd-db` **1036/0/0 across 47 suites**, rustdoc 22 = baseline) and at `de0f49ea` (cargo/rustc 1.99.1, EXECUTED): **2319/0/1 across 95 workspace suites**
(the ignore is the pre-existing `dump_divergences`), fmt check, workspace clippy and `git diff --check` clean, `nd-db`
rustdoc at 22 warnings = baseline (zero for `scanner_writes.rs`); the `96c495e7` block (2302/0/1 across 94 suites), the
intermediate `52b04064` block (2297/0/1), the
`e3e0770a` block (2276/0/1 across 92 suites, `nd-scanner` docs at 4 pre-existing warnings), the earlier `272c9c66` block
(2171/0/1 across 88 suites, `nd-db` docs at 23 pre-existing warnings) and older blocks remain dated history. See
`current-status.md` for attribution/logs. **Next on the spine is DB-18** (`DB-18 → 19 → 20 → DB-21(FULL-SCANNER-DB)`): **DB-17 LANDED `b3dee0a6`** (four caller-tx count-refresh helpers, 20 executed tests, no production caller — SC's phase-3 `:138`/`:150` and `scanner.go:316`/`:344` triggers are the consumers; DB-18/20 surfaces untouched apart from rustdoc boundary fencing; the DB-21 SigNN freeze is unaffected). **DB-18 is gated on `D-STATS`, which still has no canonical record in `decisions.md`**; DB-20 needs `D-GC` and DB-19 its boundary approval. On the other tracks the next launchable packages are **BOOT-01** (BOOT-00's prerequisite is met at `6d6938b1`; it owns the `ND_CONFIGFILE` resolution gap and B2 ffmpeg-config injection) and **SC-03** on the ratified narrow SC-01 seams. **DB-21(P1-DB) LANDED `632271a9`** as the **DB-side-only P1 exit** (NEW `nd-db/tests/runtime_scanner_persistence_contract.rs`, 4206 lines, 7 executed tests; parent scope narrowing 2026-11-21, `decisions.md` "DB-21 exit scope and prepared-statement gates"): executed composed regression through the real pool + frozen API surface (51 SigNN pins) + an executed **U-ledger** asserting `SC.mapping-bridge`, `SC.phase1-fixture`, `BOOT.configured-library-caller` and `production_full_finalize_db_contract` as Unmet. It claims **no** production caller (SC-05 is the first consumer), **no** full-exit, and the collection/count API for later API-F* still belongs to DB-21(FULL); the stable ID DB-21 is not complete until FULL-SCANNER-DB. The renderer-prepare gate (`subplans/db-runtime.md:628-632`) is deferred by the same decision with its five false-failure classes now pinned executed in that test. After a false-completion incident on this package, `assignment-template.md` gained a mandatory **Proof of delivery** section (literal `wc -l`/`ls -la`, `git status --porcelain --untracked-files=all`, literal toolchain versions, literal exits and test totals) — completion claims without those outputs are treated as unverified.
**SC-05 is unblocked on the DB side by DB-14**, and **SC-01 was RATIFIED NARROW on 2026-11-21** (`decisions.md` "SC-01 seams
ratification": the five SC-05 seams plus the authorized `nd-scanner` `nd-db` manifest delta; the progress/sink protocol, watcher,
remote storage and full `MusicFS`/`ReadTags` stay unratified), so SC-03+ may proceed on the ratified seams and the DB-21 U-ledger
entries stay U until SC-03/SC-05 deliver. SC-05 still needs SC-00/02/03. **BOOT-00 is LANDED `6d6938b1`** (its D1-D3 were
recorded and D4 B3-ratified in `decisions.md` on 2026-11-21, and the D3 manifest pin repair plus the D1 boundary are now
executed — `rust/Cargo.toml:60/61/66/68` repaired, zero new lock entries), so **BOOT-01** is the next launchable boot package.
BOOT-02's login-session
native-audience correction **landed** (`c6e220eb`/`162fae1d`) — HTTP key middleware, a mounted router, and stream
prepare/header/`Send`/body ownership each still need their own bounded package. Do not connect existing renderers
unchanged; missing-state, move, annotation and cleanup operations require executed read-back, rollback/retry and
non-target/user-data preservation tests.
**SC-05 lane note
(2026-11-23/24, after the resuscitation, two review rounds, parent integration onto this branch, and the fast-forward into
`master`):** the SC-05 vertical is
**committed as `5c463aa1`** (signed, +9103/−54 across 10 files) and **integrated**: `master` fast-forwarded from `2b3fa921` to
`342afd12` on the main checkout and pushed, so `master` and this worktree are now at the same commit. Before the commit the parent held three out-of-band
captures outside the repo (`~/backups/sc-05-capture-*`, newest `sc-05-capture-20261123-133629/`), verified by applying the patch
to a fresh clone at `cb59a973` and byte-comparing all 10 files — now redundancy only. The implementer session
`4a8567e4-1ab8-417d-8758-0dc444b0971f` was **alive and idle with its Pi session persisted**, so it was **resumed twice** via
`send_agent_prompt` (fix round, then the review fix pass) rather than re-launched; it and the other 18 finished children of this
issue are now **archived** (soft-deleted — Pi session JSONL files remain on disk and can be re-imported with
`paseo agent import`). A fresh read-only reviewer
(`c5ead1e6-92fa-4b18-b271-14d7bfce283a`) returned **APPROVE-WITH-FIXES with no High findings**, and its four selected findings
are in `5c463aa1`: wrong Go line citations in `listing_failure_warning` (doc-only; `walk_dir_tree.go` is 352 lines, so the cited
`:995-998` did not exist and `:301-312` is `isDirReadable`, which emits no Go `ScanWarning`), the `annotation` /
`album.average_rating` snapshot blind spot, duplicate writable-table lists plus an overstated injection-matrix doc, and the
unreachable `unwrap_or_else` fallback. `nd-scanner` is now **224/0 across 12 targets**, the workspace is **2444/0/1 across 129
targets (exit 0)**, and the lane's own rustdoc warnings are **zero**. Two review suggestions were **declined** with reasons in
`decisions.md` D-SC05-REVIEW: the 4 pre-existing rustdoc `-D warnings` failures (they fail identically at integrated
`cb59a973`, so they are a baseline gate, not SC-05 scope) and the latent `to_string_lossy` ancestor split (it lives in
`ignore.rs:435`, which this lane never touched). **Done: the signed commit and durable receipt** (`decisions.md` D-SC05-INT step 4).
**Still open: whether to fast-forward `master` to this branch**, and a DB-owned micro-lane to reword the stale
`SC.phase1-fixture` gate text in `nd-db/tests/runtime_scanner_persistence_contract.rs:596-598`. **SC-04 is launchable independently** (prereq SC-03 met;
placement decided by `decisions.md` D-SC04); **SC-06 remains unauthorized** and must not be launched on
`decisions.md` D-SC05-SLICE.

**Worktree note:** besides this worktree (`wizardly-elephant`, branch `resuscitate-dead-agent-capture-worktree-state` at
`342afd12`), the
isolated worktree `~/.paseo/worktrees/0xo97qr3/boot00` on branch `boot00-manifest-pins-scaffold` (commit `6d6938b1`, BOOT-00 work
already integrated) was **archived at the user's request on 2026-11-24** — `paseo workspace archive wks_929b07163c95a811`, which
removed the worktree and archived its agent. Verified beforehand: 0 porcelain entries, 0 stashes, and the branch an ancestor of
`master`; the branch is **kept** locally and on origin, so nothing was lost. After that removal `git worktree list` matches the
on-disk directories exactly and `git worktree prune -n` reports nothing stale
for navidrome and for every other registered repo; the only truly dangling artifact found earlier — an empty
`~/.paseo/worktrees/29g9rl3m/` directory whose three `pi_agent_rust` workspaces were archived on 2026-11-11 — was removed after
verifying it held 0 entries and that its branches (`diag/main-p0a-baseline` `fd341e588`, `issue/main-12j9w` `c11032705`) survive
locally and on origin.

Scope facts that keep agents from re-litigating settled things: #26 is split (`D-PLAYER26`) — the player-session service
(`Get`/`Register`/`Touch` over the landed `PlayerRepo`, canonical placement chosen by API-01) is retained and the
mpv/jukebox playback-device scope is **dropped** unported; #26 is not DONE and no player route is mounted. Per-device
preference editing is retained scope (`D-PLAYER-EDIT`): owner-scoped `GET`/`PUT /api/player/{id}` with immutable
`user_id` and an enforced field whitelist, plus a **read-only** `GET /api/transcoding` list; profile **editing** stays
dropped. The executing transcoding read landed at `f8ceeb8f`, so only the BOOT-owned `DEFAULT_TRANSCODINGS` seed and the
route remain. Go registered/touched players only in the now-cancelled Subsonic/Jellyfin middlewares, so canonical
placement is a deliberate API-01 decision, not inherited behavior; Go has no player expiry (only `LastSeen` stamping and
a 1-minute write limiter), and key issuance needs `PlayerRepo::set_api_key`, not the service. Full evidence:
`subplans/README.md`.

The first runnable slice (#27A) is fresh DB → configured library → real track scan → canonical authenticated lookup →
direct Range/forced transcode/disconnect cleanup. Phase-1-only ingest is not a complete #17; repair, full maintenance,
metrics and every full-API service are not automatic prerequisites for that slice.

## Non-negotiable task boundaries

1. The parent owns decomposition, decisions, integration and final verification. Children do not delegate unless
   expressly authorized. Every implementation assignment specifies exact task ID, baseline, file ownership, prerequisite
   APIs/decisions, acceptance tests, exclusions and report format. Stop when scope must expand.
2. Use the installed **Paseo** skill. Discover tools/hosts/profiles, read all profile notes, and materialize the
   selected provider/model/mode/thinking/features into the creation settings. Do not hard-code a model from this
   document or substitute Pi-local agents.
3. Keep the issue in the parent's **verified existing workspace**. Prefer inherited agent-scoped creation and verify the
   returned workspace ID; the same directory is not proof of workspace identity. No new workspace/worktree, remote
   placement, archive/delete/cleanup or isolation without explicit user approval, and no blind expansion of unset IDs.
4. **One writer per checkout**, even for disjoint files. Parallel work is read-only by default. The user-approved
   exception is **parallel isolated worktrees with parent-controlled merges**: one writer inside each worktree, and no
   claim that a branch is done from an agent message. Serialize writers and any tests that mutate fixtures or generated
   files. Scratch/probe artifacts only in `/tmp`.
5. Freeze writes before a fresh read-only review. A parent source check is useful but not a substitute for the required
   independent reviewer. On provider failure, report incomplete coverage and resume or replace the review; never
   silently declare approval. Use completion notifications; do not poll or send hurry-ups.
6. No existing Go-source, Cargo manifest/lockfile or generated-file changes unless the particular assignment explicitly
   authorizes them. Plans needing new dependencies or tracked Go harnesses must resolve that gate first. No `unsafe`,
   no lint bypasses, no auto-formatting or auto-fixes — deliberate minimal edits only.
7. No commits or pushes without fresh explicit authorization. Any authorized commit must be GPG signed — never disable
   signing or add AI attribution trailers — and verified with
   `git cat-file commit HEAD | grep -q '^gpgsig'`. Pushing needs separate authorization. Do not archive a
   workspace/agent merely because its work was committed, and never archive one holding uncommitted work.
8. Destructive or irreversible changes, credentials/secrets, production DB/file mutations and ambiguous cleanup require
   user confirmation. Test only fresh synthetic fixtures; never read or expose real credentials to build fixtures.

## Technical rules and acceptance

- Rust conventions: snake_case functions, documented public types/fields/functions, pinned Go symbol/path citations
  where behavior is ported. `thiserror` in libraries, `anyhow` at the CLI boundary, Tokio async, explicit typed DI in the
  CLI. DTO ownership stays as assigned (`nd-db`-local DTOs; do not touch `nd-model` for hydration); no opportunistic
  cross-crate model refactors.
- SQLite uses pinned rusqlite (not sqlx); connections are single-owner and blocking work goes through SQLStore's
  `spawn_blocking` boundary. Transaction callbacks share one leased connection — never recursively check out the pool.
  Retry closures may run again, so no repeated filesystem/network/event side effects inside them. Dropping an async await
  does not cancel an already-started blocking SQLite transaction. Compose domain errors without string loss.
- Values are parameterized; sort identifiers/options use allowlists; restful filter keys require a registered field
  whitelist. Library/user restrictions are deliberate typed inputs, never raw request SQL. Assert SQL **and ordered
  binds**, then execute on a fresh schema and read stored rows, side tables and FTS back. Renderer-only comparisons
  missed real data-corruption and syntax defects before (`decisions.md` W0/W1).
- Pin zero/NULL/malformed/default behavior, transaction rollback/retry, empty/one/many and chunk boundaries, stable
  identity, created/birth immutability, first-error order, and cross-user/library non-target behavior as relevant.
- Do not weaken strict native audience, session expiry/refresh/epoch, key hashing/ownership or trusted-proxy boundaries.
  First-admin atomicity is a retained deliberate improvement over Go, not parity debt to undo.
- Stream/network tests must exercise real body backpressure/drop, child cleanup and recovered slots, and must require
  their external tools/codecs. Keep post-header failures distinct from pre-header error responses and preserve
  cache/lease/cancellation ownership.
- Label evidence EXECUTED / SOURCE-VERIFIED / INFERRED / UNVERIFIED and verify reports against files, history and logs.
  Use real pinned Go behavior, not a copied algorithm or Rust-produced expected output. Record nondeterministic set
  normalization, output hashes, exact commands/exits and toolchain versions. `/tmp`-only oracles are local evidence, not
  clone-reproducible reproducibility; regenerate expectations only from the Go side, never to make a failing
  implementation pass.
- Product decisions stay settled (`decisions.md` C1): fresh service with no Go upgrade path, canonical API only, no
  reversible passwords; Subsonic/Jellyfin/plugin/sonic cancelled; bundled JS UI and backgrounds dropped; `/` is 404;
  Quick-Connect belongs to later canonical API work. Borrow Go behavior without resurrecting cancelled wire surfaces.

## Validation and completion report

From `rust/`, use focused build/tests after small steps, then the package's full acceptance matrix. Integrated gates:

```sh
cargo fmt --all -- --check
cargo build --locked --workspace
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p nd-db --all-targets --all-features --no-fail-fast
cargo test --locked --workspace --all-targets --all-features --no-fail-fast
cargo doc --locked -p nd-db --all-features --no-deps
```

At the repository root run `git diff --check` and `git status --porcelain --untracked-files=all`. Compare rustdoc
warnings against the recorded baseline (22 for `nd-db`) and never call a successful doc build warning-free. Record
failures and flakes, not just the last green rerun. The parent validates the integrated result after selected review
fixes.

Report: **verdict; exact files changed; actual commands/exits/test totals/ignored and tool versions; evidence labels and
non-vacuity; deviations closed or retained; blockers; exact next package.** No implementation is complete merely because
a child says it is or the current suite passes.
