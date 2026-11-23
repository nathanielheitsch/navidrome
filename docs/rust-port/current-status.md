# Rust port — audited capability snapshot

**Authoritative baseline: this branch line at `cb59a973`** — **SC-03 LANDED `cb59a973`** (the `nd-mapping` tag→record
bridge + the singleton folder projection, 4 files +2 executed test suites, 19 executed tests) on top of **BOOT-00 LANDED
`6d6938b1`** (the construction/dependency/launch GATE: D3 manifest pin repair + the D1 dependency boundary with the
executed `dependency_graph_has_no_http_api_cycle`, no production behavior) and **DB-17 LANDED `b3dee0a6`** (four caller-tx
count-refresh helpers, 20 executed tests) on top of **DB-16 `7d022a02`**, **DB-21(P1-DB) `632271a9`**, **DB-14 `de0f49ea`**,
**DB-11 `52b04064`** and **DB-15's runtime half `96c495e7`** — landed on top of integrated `master`'s code HEAD `e3e0770a` (SC-02 partial: local walker,
`ndignore`, folder hash, cancellation) and its docs HEAD `795e8986`, dated 2026-11-20. That boundary extends the
previously audited `272c9c66` (DB-22) and `f80b9704` boundaries (DB-04…DB-10/12/13 and BOOT-02; DB-12 signed `e5d0592d`,
rustdoc fix `f80b9704`; DB-10 signed `65072b89` / merge `6e5ca97d`). Decision records: S9 (SC-02 dependency set +
close-out, `b1aeea2c`), D-CURSOR/D-SCOPE (now **consumed** by DB-11's executed code), D-PLUGIN, D-BATCH (now **consumed**
by DB-14's executed code), and the DB-15 durable
Go-oracle deferral (all in `decisions.md`); the 2026-11-21 records add the **DB-21 exit scope narrowing**,
the **renderer-prepare gate deferral**, the **proof-of-delivery** process ruling, the **SC-01 narrow ratification** and the
**BOOT-00 D1-D3** decision (`decisions.md`, `assignment-template.md`); the 2026-11-23 records add **D-SC04** (SC-04
placement is scanner-local), **D-SC05-MANIFEST** (`nd-utils` runtime + `nd-dbschema` dev-only, in addition to the ratified
`nd-db` edge), **D-SEAM2** (seam-2 wording corrected to `Option<&UserScope>`), **D-SC05-SLICE** (SC-05 narrow cycle/follow
design, **no SC-06 authorization**) and **D-SC05-PATH** (raw path identity is the contract; UTF-8 name text is an SC-02
encoding boundary, not a semantic precondition). **SC-05 is IN FLIGHT and NOT ACCEPTED** — see the in-flight block below.
**SC-03, DB-11, DB-15's runtime half, DB-14, DB-21(P1-DB), DB-16, DB-17 and BOOT-00 are LANDED** with executed tests, and
each remains partial: on the integrated tree no write primitive has a production scanner caller and there is no
`nd-scanner` consumption (DB-14's seam is landed but uncalled at `cb59a973` — its first caller is the **uncommitted** SC-05
lane below), SC-03 is the bridge + singleton projection only (multi-track groups refuse), there is no full
cursor/REST surface, and DB-15 keeps its durable Go oracle deferred. The **SC-02 partial** remains a scoped slice — **not**
SC-01 ratification, SC-02 completion, or MusicFS readiness. **DB-21(P1-DB) is the DB-side-only exit**: an executed composed
contract plus an API freeze and an executed U-ledger — it adds **no production caller** and is not the full DB-21 exit.
**DB-17 `b3dee0a6`** adds four caller-tx count-refresh helpers with **no production caller** (SC's phase-3 triggers are the
consumers) and **BOOT-00 `6d6938b1`** is **gate work only** — the CLI `main` is still empty and nothing listens or boots.
Latest integrated gates below are **parent integration evidence EXECUTED at
`6d6938b1`** on the merged tree; the SC-03 gate set is the commit-body evidence at `cb59a973` (parent-measured on the
merged tree), and the block directly under it is **non-integrated** in-flight evidence for the uncommitted SC-05 lane.
This document records **evidence**, not implementation authorization. Execution order lives in [`next-work.md`](next-work.md); binding decisions in
[`decisions.md`](decisions.md); bounded assignments in `subplans/`. Older dated blocks are history, not current
readiness. Resolve contradictions against the pinned source and the current contract amendments, never against a larger
test total. Entry point: [`README.md`](README.md).

## Verdict

**The Rust workspace passes its present regression gates; it is not a runnable server or scanner.** Shared codecs,
record marshaling, all eight `PostScan`-equivalent hooks, substantial executed reads, and — since `4ab63cd5` —
**executed transaction-bound leaf and entity writes** (property, library incl. scan lifecycle, tag, folder,
media-file identity/base/junction writes, artist, album, user epoch revoke, artwork invalidation/enqueue) have landed;
since `52b04064` the **typed filtered scanner reads and cursor contract** execute on a leased connection, and since
`96c495e7` so do annotation transfer and average-rating recalc transactions, and since `de0f49ea` so does the **per-folder
scan delta** (`persist_folder_delta`, D-BATCH), and since `7d022a02` so do the **executed move/reference-remap/delete
primitives** (`move_matched_mediafile`, `remap_missing_mediafile`, `reassign_references_in_tx`, `delete_mediafile_in_tx`,
`delete_all_missing_in_tx`) alongside the A8 share typed rejection and the D-PLUGIN deletion of `plugin_repo.rs`; since
`b3dee0a6` so do the **four caller-tx count-refresh helpers** (`album_repo::refresh_play_counts_in_tx`,
`artist_repo::refresh_play_counts_in_tx`, `tag_repo::update_counts_in_tx`,
`library_repo::refresh_library_totals_in_tx`), and `6d6938b1` (**BOOT-00**) repaired the workspace manifest pins and turned
`nd-http` into a typed dependency-boundary module — **gate work only, no production behavior**. **Scanner orchestration and the
CLI/API remain scaffolds**: `nd-http` carries `RouterInjection` plus the executed cycle test but **no router and no listener**,
`nd-api` is a scaffold (`nd-cli/src/main.rs` is an empty `main`); `nd-scanner` now
holds the tested local-FS walker slice (`storage.rs` + `.ndignore`/folder-hash/cancellation, SC-02 partial at `e3e0770a`),
the **SC-03 mapping bridge and singleton projection** (`mapping_bridge.rs`, `projection.rs` at `cb59a973`), and — **in the
working tree only, uncommitted and unaccepted** — an SC-05 phase-1 vertical (`phase1.rs` + `tests/phase1_*.rs`) that is the
**first caller** of `nd_db::scanner_queries::visit_media_in_folder` and `nd_db::scanner_writes::persist_folder_delta`
(SOURCE-VERIFIED in the working tree at `cb59a973`: `phase1.rs:568` call, and `nd-scanner/Cargo.toml` now declares `nd-db`,
`nd-id`, `nd-tagreader`, `nd-utils` + dev `nd-dbschema`). On the **integrated** tree `nd-scanner` declares no `nd-db`
dependency and DB-14's seam stays uncalled, so no integrated write primitive has a production caller. There is
**no end-to-end/server readiness**, no mounted router, no production scan, and no integrated stream HTTP adapter. DB-12 is only the executed artwork
invalidation/enqueue subset, not a worker or full scanner, and DB-11 is only the executed read/cursor contract, not the
full cursor or REST surface. That is narrower than "no repository method is ever called":
`nd-auth` **does** call
`UserRepo` methods from production code (`login.rs:95,108,227,247,308,332,337,355,356,374`, `setup_store.rs:66-86`,
`middleware.rs:93` — `find_by_username`, `count_all`, `count_by_username`, `put`, `update_last_login_at` through
`SQLStore::with_connection`/`with_tx_retry`), but that router is mounted only in crate tests, not in a server. A scaffold
test asserting an issue number is not runtime coverage, and executed test helpers are not production repository methods.

## SC-03 integrated gates (parent, HEAD `cb59a973`, 2026-11-23) — latest INTEGRATED boundary

SC-03 landed as one signed commit, 7 files, **+4312 additions-only**: NEW `nd-scanner/src/mapping_bridge.rs` (682), NEW
`nd-scanner/src/projection.rs` (505), NEW `nd-scanner/tests/mapping_bridge.rs` (2092), NEW
`nd-scanner/tests/projection_singleton.rs` (1014), plus `lib.rs` (+3 module exports), `nd-scanner/Cargo.toml` (+13) and
the lock delta (3 dependency-edge lines, zero new `[[package]]` entries). **19 executed tests** against the committed
`go_oracle.json` plus the real tagreader fixtures (`known.mp3/flac/m4a/ogg`); backend gaps pinned on both sides
(135/136, 124/125, 94/95, 0/30, m4a bitDepth). **Birth-time P1 ruling (parent, 2026-11-21, recorded ONLY in this commit
body — `decisions.md` carries no copy)**: Go substitutes `time.Now()` at MAPPING time (`local.go:116-128`,
`map_mediafile.go:81`) and `Put` stamps `CreatedAt`, not `BirthTime` (`mediafile_repository.go:184-186`); the bridge takes
the substitute as an **injected clock** (D-TIME: no clock reads inside) and `effective_birth_time` resolves ONCE.
Review REQUEST-CHANGES (P1 birth-time + 2 P2) → fixed; oracle integrity, self-corrections and PID fallback
reviewer-verified. Parent gates EXECUTED at `cb59a973` (commit body): workspace **2383 / 0 / 1 across 100 suites**,
`nd-scanner` **169 / 0**. **Scope**: the bridge and the singleton projection only. Multi-track album groups **refuse**
(`ProjectionError::MultiTrackGroup`) rather than aggregate; no equality, no DB writes, no scanner entrypoint. SC-04 owns
aggregation/semantic comparison, scanner-local (`decisions.md` D-SC04). The `6d6938b1` block below is dated history.

## Non-integrated: in-flight SC-05 lane (working tree at `cb59a973`, 2026-11-23) — NOT accepted, NOT landed

**Ownership is currently unassigned**: the lane's writer is gone and its work is uncommitted. Nothing below is integrated.
Working-tree delta (SOURCE-VERIFIED via `git status --porcelain --untracked-files=all`): NEW `nd-scanner/src/phase1.rs`,
NEW `nd-scanner/tests/{phase1_slice,phase1_faults,phase1_fixture}.rs` and `tests/common/mod.rs`, modified
`nd-scanner/src/{lib.rs,walk.rs}` (+188/−25), `nd-scanner/Cargo.toml`, `rust/Cargo.lock` (two dependency edges only —
`nd-utils`, `nd-dbschema` — matching `decisions.md` D-SC05-MANIFEST), plus the uncommitted 2026-11-23 parent decision
records in `docs/rust-port/decisions.md` and `docs/rust-port/subplans/scanner.md`. This is the **first caller** of the
DB-11 read seam and the DB-14 write seam: `capture_selected_folder` → `ingest_folder_snapshot` →
`nd_db::scanner_writes::persist_folder_delta`, with `visit_media_in_folder(conn, folder_id, None, …)` at `phase1.rs:568`.
Executed test inventory **in this tree**: `nd-scanner` **217 passed / 0 failed across 11 suites** (`--list` counts:
`phase1_slice` 11, `phase1_faults` 14, `phase1_fixture` 19, lib 24, plus the five SC-02/mapping suites).
Parent gates re-measured on this working tree (EXECUTED again by the parent at 15:3x local, cargo/rustc 1.99.1 Homebrew):
workspace **2436 / 0 / 1 across 129 targets** = 103 test binaries (**2430** passed, flake skipped) + 26 doctest suites
(**6** passed); the single ignore is the pre-existing `dump_divergences`. The earlier **2431 / 0 / 1 across 103 suites**
figure is the same tree counted as binaries-only **with the `nd-stream` flake passing** (2430 + 1), so the two figures agree.
`-p nd-db` **1079 / 0 / 0 across 50 targets** (1073 across 49 binaries + 6 doctests — same convention as the earlier
**1073 / 0 / 0 across 49**). fmt clean, workspace clippy `--all-targets --all-features -D warnings` clean, `git diff --check` clean.
**One pre-existing failure is not this lane's**: `nd-stream`
`transcode_body::tests::early_release_defeats_the_cap_and_accumulates_processes` failed once in a full `cargo test --workspace`
run (which then aborted early at 2078 passed / 91 targets) and passed on two solo reruns; `nd-stream` is untouched by this
lane (`git status --porcelain -- rust/crates/nd-stream` = 0 entries) and the flake is already tracked in `next-work.md`. With it skipped,
the workspace is 2436 / 0 / 1 across 129 targets.
**Rustdoc is NOT clean at this boundary** and is a fix before integration: `nd-scanner` now reports **12 warnings**, up
from the **4** recorded at `e3e0770a`. **Eight** are new intra-module links to `pub(crate)` items from the SC-05 module docs
(`phase1.rs:20,:50,:52,:60,:61,:261,:1652,:1653` → `ingest_folder_snapshot`, `capture_selected_folder`,
`crate::walk::selected_identity_context`, `crate::walk::enter_refusal`); the remaining **four** are **pre-existing** and untouched by
this lane (`contract.rs:299` `QUEUE_SPACE_RETRY_INTERVAL`, `contract.rs:420` `MAX_IGNORE_LINE_BYTES`, `storage.rs:31`
`decode_path`, `walk.rs:27` redundant explicit link). `nd-db` stays at **21 = post-DB-16 baseline** (untouched).
**Open review findings** (fresh read-only review of the last fix round, 2026-11-23; no High findings):
(a) Low-Medium — the capture's child-listing gate collapses every listing error to `is_ok()` (`phase1.rs:801`), so a
`NotFound`/`EntryIsSymlink`/other child failure yields a different warning **kind** than the walker's `enter_directory`
mapping (`walk.rs:971-997`); the fall-through itself is Go-accurate (`walk_dir_tree.go:156-176,:286-303`), the divergence
is the warning kind only, and there is no hash impact. (b) Low (latent, unreachable today) — `build_batch` renders
`folder.name`/`path` through lossy text (`phase1.rs:1356-1362`, `display_path`), which `preflight_component`
(`walk.rs:1306-1324`) makes unreachable; see `decisions.md` D-SC05-PATH. (c) untested parity — no non-UTF-8-name test
anywhere in `tests/phase1_*.rs`, and no test exercises `Phase1Error::Hash` or the `Projection`/`Bridge` arms beyond
multi-track; `tests/folder_hash.rs` goldens pin preimage→digest only, so captured-fact parity against a Go-computed
folder hash is unpinned. (d) that reviewer did NOT re-run the suites after the last edit at 06:49, so the totals above are
the parent's own re-measurement, not the reviewer's.
**Not done / not claimed**: SC-05 acceptance (multi-track aggregation, outdated/empty-skip policy, image-changes
collector, missing-folder finalizer are all still open — `decisions.md` D-SC05-SLICE), no SC-06/controller authorization,
and no integration merge. A branch or a green working tree is not a landed package.

**Resuscitation / capture receipt (2026-11-23, parent).** The SC-05 fix-round agent `4a8567e4-1ab8-417d-8758-0dc444b0971f`
("Fix SC-05 false sibling-cycle refusal and root facts") is **alive and idle** in workspace `wks_f8d2b72072a890ce` with its
persisted Pi session (`01a20827-4562-70a5-a237-27b05db0b8db`), so it is resumable via `send_agent_prompt` — nothing of it
was lost. Its last edits (06:49 local) are exactly the tree above: `src/phase1.rs` (false sibling-cycle accumulation removed,
root `name = "."` per Go `model/folder_test.go:115`, in-crate identity-mismatch abort test at `phase1.rs:1747`),
`tests/phase1_slice.rs` (`entered_siblings_are_not_active_ancestors_for_later_aliases` L499,
`a_wide_folder_is_never_accumulated_into_the_depth_cap` L680,
`scanning_the_library_root_stores_go_s_dot_name_and_root_identity` L764), `tests/common/mod.rs:613` and
`tests/phase1_faults.rs:940` + `:1044-1072` (non-vacuous `item_artwork` row/rollback assertions). No agent in this issue is
dead-with-lost-work; the worktrees that were removed belong to **older closed/archived lanes** (`rust-sc02-local-walker`,
`rust-db22-executed-reads`, `rust-db10-mediafile-writes`, `rust-db12-artwork`, `rust-db13-epoch-revoke`,
`rust-db07/08/09`, `rust-boot02-native-session`, `rust-docs-cleanup`, `rust-wave1-*`, `rust-wave2-status-refresh`), and that
work is landed as commits on this branch line. Durability note: because the SC-05 tree has **no commit and no branch**, the
parent took an out-of-band capture — a full `git diff --cached --binary` patch plus verbatim file copies — in a
user-local backup directory outside the repo (`~/backups/sc-05-capture-20261123-103223/`, 924 KB; `sc05-full.patch`
8918 lines covering `src/phase1.rs`, `src/lib.rs`, `src/walk.rs`, `Cargo.toml`, `Cargo.lock`,
`tests/phase1_{slice,faults,fixture}.rs`, `tests/common/mod.rs`). The worktree diff is still the only in-repo copy.


## Latest integrated gates (parent, HEAD `6d6938b1` / BOOT-00 on the merged tree, 2026-11-21)

DB-17 (`b3dee0a6`, 5 files in `nd-db`, +2817 additions-only) and BOOT-00 (`6d6938b1`, 6 files, +177/−19 — `rust/Cargo.toml`,
`rust/Cargo.lock`, `nd-http/{Cargo.toml,src/lib.rs}`, `nd-api/src/lib.rs`, `nd-cli/src/main.rs`) landed on this branch line.
**DB-17** executed four caller-tx refresh helpers — `album_repo::refresh_play_counts_in_tx`,
`artist_repo::refresh_play_counts_in_tx`, `tag_repo::update_counts_in_tx`, and
`library_repo::refresh_library_totals_in_tx(id, now)` (8 probes then the 10-bind update; Go discards `RowsAffected` at
`library_repository.go:249` and Rust returns the written `RefreshStatsTotals` — documented deviation) — pinned by NEW
`tests/runtime_count_refresh.rs` = **20 executed tests** (bare `play_date = excluded.play_date`, so Go's REPLAY
backwards-overwrite is faithful with **no monotonic guard added**; zero-arg statements; caller-stamped canonical TEXT with no
clock in SQL; NULL-exclusion; statement-level `RowsAffected` vs entity counts; probe-then-update order; failure in both
directions with **rollback as the CALLER's obligation via the DB-00 typed seam**). Review APPROVE-WITH-FIXES
(F1 `run.Parallel` misconception corrected — the production caller already shares one `*dbx.Tx` per `scanner.go:330-346`;
F2 rollback docs; F3 two helper-internal failure regressions; F4 three doc inaccuracies). **BOOT-00** executed the **D3 manifest
pin repair** (`chrono "^2"`→`"0.4"`, `tracing "^0.5"`→`"0.1"`, `tracing-subscriber "^0.1"`→`"0.3"`, `base64 "^0.8"`→`"0.22"`; **zero
NEW lock entries** — the lock diff is only the `nd-http` dependency-edge stanza) and the **D1 boundary**: the `nd-http` scaffold
became a typed module with the **EXECUTED** `dependency_graph_has_no_http_api_cycle` acceptance (`cargo metadata --locked` graph
check plus a negative control) and the `RouterInjection = fn(axum::Router, &nd_conf::Config) -> axum::Router` shape; `nd-cli`/`nd-api`
carry rustdoc boundary notes for the `ND_CONFIGFILE` resolution gap and the `Dir::path` `create_dir_all` hazard (**BOOT-01 owns
both**); **D4** was already resolved by the B3-ratified record. Parent ran the authoritative gates on the merged tree at
`6d6938b1` with **Homebrew cargo/rustc 1.99.1** (EXECUTED): workspace tests **98 suites, 2363 passed / 0 failed / 1 ignored**
(the single ignore is the pre-existing `dump_divergences`), `-p nd-db` all-targets **1073 / 0 / 0 across 49 suites**, `nd-http`
**1 / 1** (the cycle test), `cargo fmt --all -- --check` clean, `cargo clippy --locked --workspace --all-targets --all-features --
-D warnings` clean, `git diff --check` clean, and `nd-db` rustdoc at **21 warnings = the post-DB-16 baseline**. Arithmetic:
`2343` at `7d022a02` `+ 20` (DB-17's suite) `+ 0` (BOOT-00: `+1` cycle test `− 1` dropped `nd-http` scaffold test `nd_http_instantiates`)
`= 2363`, suites `97 → 98`. **Not done:** no production caller for DB-17 (SC's phase-3 `:138`/`:150` and `scanner.go:316`/`:344`
triggers are the consumers), DB-18/20 surfaces untouched apart from rustdoc boundary fencing, the DB-21 SigNN freeze is unaffected,
and BOOT-00 changed **no** production behavior — `main` is still empty. The `7d022a02` block below is dated history.

## Previous integrated gates (parent, HEAD `7d022a02` / DB-16, 2026-11-21)

DB-16 (`7d022a02`, 10 files in `nd-db`, +3507/−308) landed on this branch line: executed scanner move
(`move_matched_mediafile`, `WithTxRetry` semantics via `try_with_tx_retry_sync` — the target keeps its OWN
`created_at`/`birth_time` because Go's update arm drops them at `sql_base_repository.go:605/608`), maintenance remap
(`remap_missing_mediafile`, single `try_with_tx` — no retry), NEW `reassign_references_in_tx` (composes DB-15 annotation +
avg-recalc, executed `reassign_bookmark_in_tx`, then playlist/scrobble/buffer legs in Go order; render-only
`reassign_references_sql` untouched), `delete_mediafile_in_tx` (Go bare base-row delete; FK cascades + FTS trigger verified
against Go migrations), `delete_all_missing_in_tx` (opt-in; the admin gate is documented as caller policy), warn arms via the
DB-14 tx-loss handler, **A8** (`share update_sql` typed `EmptyUpdateColumns` rejection before any SQL text; DB-21 class-2 pin
re-executed) and **D-PLUGIN deletion executed** (`plugin_repo.rs` 243 lines / 3 tests + the `lib.rs` export deleted, class-3
probe now literal SQL, prose reworded, schema goldens intact, zero `plugin_repo` identifiers under `nd-db/src`).
NEW `tests/runtime_move_persistence.rs` = **18 executed tests**. Review APPROVE-WITH-FIXES (P2 copy-outcome
misclassification fixed in-lane pre-commit; P3 bookmark doc corrected). Parent ran the authoritative gates at `7d022a02`
with **Homebrew cargo/rustc 1.99.1** (EXECUTED): workspace tests **97 suites, 2343 passed / 0 failed / 1 ignored** (the single
ignore is the pre-existing `dump_divergences`), `-p nd-db` all-targets **1053 / 0 / 0 across 48 suites**, `cargo fmt --all --
--check` clean, `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean, `git diff --check` clean,
and `nd-db` rustdoc at **21 warnings = the post-DB-16 baseline** (22 − 1 deleted-module link from the D-PLUGIN deletion).
**Not done:** no production caller (SC-06 and BOOT maintenance are the consumers), no GC execution (caller step), no count
refresh (DB-17, since landed at `b3dee0a6`), no playlist cleanup (DB-20). The previous `632271a9` block below is dated history.

## Previous integrated gates (parent, HEAD `632271a9` / DB-21 P1-DB, 2026-11-21)

DB-21(P1-DB) (`632271a9`, 1 file in `nd-db`: NEW `tests/runtime_scanner_persistence_contract.rs` 4206 lines, 7 executed tests)
landed on this branch line as the **DB-side-only P1 exit** (parent scope narrowing, `decisions.md`). Parent ran the authoritative
gates at `632271a9` with this machine's toolchain — **Homebrew cargo/rustc 1.99.1** (`/opt/homebrew/bin`, rustup stable default;
the recorded gate baselines use it) — EXECUTED: workspace tests **96 suites, 2326 passed / 0 failed / 1 ignored** (the single
ignore is the pre-existing `dump_divergences` in `nd-mapping/tests/go_oracle.rs`), `-p nd-db` all-targets **1036 / 0 / 0 across
47 suites**, `cargo fmt --all -- --check` clean (exit 0), `cargo clippy --locked --workspace --all-targets --all-features --
-D warnings` clean (exit 0), `git diff --check` clean, and `nd-db` rustdoc at **22 warnings = the recorded baseline** (zero
warnings in the new file). Review came back REQUEST-CHANGES with 6 findings (vacuous second-pass stamps/mutations, unexecuted
`f`-alias, wrong-type cursor stops, lifecycle column, BOOT-00-vs-BOOT-01 ledger entry, misdescribed share prepare error) and
landed APPROVE-WITH-FIXES; the parent applied 3 P3 comment fixes. Accepted deviations: the `f`-alias executed via a
`#[doc(hidden)]` seam (DB-owned test, flagged), and `IdChunkedCursor` stop semantics pinned AS LANDED (final-chunk Stop
terminal; non-final Stop offers the next chunk; failure path fail()→Closed never resumes) — `D-CURSOR`(3) read as
post-completion only. **DB-21(P1-DB) is not the full exit:** no production caller (SC-05 is the first consumer), no full-exit,
the collection/count API for later API-F* still belongs to DB-21(FULL) and is NOT delivered, not #27A progress, and the stable
ID DB-21 stays incomplete until FULL-SCANNER-DB.


## Previous integrated gates (parent, HEAD `de0f49ea` / DB-14, 2026-11-20)

DB-14 (`de0f49ea`, 4 files in `nd-db`: NEW `src/scanner_writes.rs` 935 lines, NEW `tests/runtime_folder_persistence.rs`
17 executed tests, `src/lib.rs` +1, `album_repo.rs` +102 additions-only delivering `copy_album_created_at_in_tx`) landed
on this branch line. Parent ran the authoritative gates at `de0f49ea` with cargo/rustc **1.99.1** (EXECUTED): workspace
tests **95 suites, 2319 passed / 0 failed / 1 ignored** (the single ignore is the pre-existing `dump_divergences` in
`nd-mapping/tests/go_oracle.rs`), `cargo fmt --all -- --check` clean (exit 0), `cargo clippy --locked --workspace
--all-targets --all-features -- -D warnings` clean (exit 0), `git diff --check` clean, and `nd-db` rustdoc at **22
warnings = the recorded baseline** (zero warnings in `scanner_writes.rs`). Review came back REQUEST-CHANGES and both
findings were fixed before landing (P1 tx-loss atomicity — `warn_arm` detects a lost transaction via `is_autocommit` and
returns the original typed SQLite error as fatal; P2 consumed-transitions retry proof), with the parent re-running every
gate. DB-14 is not a full scanner package: no production caller, no `nd-db` dependency in `nd-scanner`, no walk/tag-mapping/
album aggregation (SC-owned) and no missing-folder finalize (SC-06). That block is dated history, not proof for a later diff.

## Previous integrated gates (parent, HEAD `96c495e7` / DB-11 + DB-15 runtime, 2026-11-20)

DB-11 (`52b04064`, 7 files in `nd-db`) and DB-15's runtime half (`96c495e7`, 2 files in `nd-db`) landed on this branch
line. Parent ran the authoritative gates at `96c495e7` with cargo/rustc **1.99.1** (EXECUTED): workspace tests
**94 suites, 2302 passed / 0 failed / 1 ignored** (the single ignore is the pre-existing `dump_divergences` in
`nd-mapping/tests/go_oracle.rs`), `cargo fmt --all -- --check` clean (exit 0), `cargo clippy --locked --workspace
--all-targets --all-features -- -D warnings` clean (exit 0), `git diff --check` clean, and `nd-db` rustdoc at **22
warnings = the recorded baseline** (zero warnings in `sql_annotations.rs`). Neither commit is a full package: DB-11 does
not complete the cursor/REST surface and neither adds a production caller, and DB-15's durable Go oracle stays deferred
to the EVID lane (`decisions.md` H6). The intermediate DB-11 gate at `52b04064` (**2297 / 0 / 1**) and the prior dated
block below are history, not proof for a later diff.

## Previous integrated gates (parent, code HEAD `e3e0770a`, 2026-11-20)

SC-02 partial (local walker + `.ndignore` + folder hash + cancellation, S7/S8/S9/S9b) fast-forwarded onto
`2e7026b2` at `e3e0770a`. Parent re-ran the authoritative gates on the integrated main checkout:
workspace tests **92 suites, 2276 passed / 0 failed / 1 ignored**; `cargo fmt --all -- --check` clean; workspace clippy
(`--all-targets --all-features -- -D warnings`) clean; `git diff --check` clean; `nd-scanner` rustdoc = 4
pre-existing warnings (none new from that slice). This was a partial SC-02, NOT SC-01/SC-02 completion or MusicFS
readiness, and NOT the runnable-milestone scanner entrypoint. Prior dated block below.

## Prior integrated gates (code HEAD `272c9c66` / DB-22, 2026-11-20)

Parent validated on the DB-22 branch after rebase onto `276eec6c` (fast-forward, no merge commit): fmt check,
workspace clippy, and workspace tests all succeed: **88 suites, 2171 passed / 0 failed / 1 ignored**. `nd-db` docs
report **23 rustdoc warnings**, all pre-existing (one new in `folder_repo.rs` came from already-integrated DB-07
work, not DB-22); no warning in a DB-22-touched file. Dated code-HEAD results, not proof for later diffs.

## Integrated gates history (code HEAD `f80b9704`, 2026-11-19)

Parent reports fmt check, workspace clippy, workspace tests and workspace docs all succeed at `f80b9704`. This lane
independently tallied `/tmp/nav-db10-integrated-tests.log`: **87 suites, 2159 passed / 0 failed / 1 ignored**.
Clippy log: `/tmp/nav-db10-integrated-clippy.log`; docs: `/tmp/nav-db10-integrated-doc.log`. `nd-db` still has
**22 rustdoc warnings** (pre-existing; no net increase after `f80b9704` fixed the DB-12 HTML-tag warning). Workspace docs
success does not mean warning-free. No new standalone DB/doctest run is claimed. These are dated code-HEAD results, not
proof for later diffs or mounted/end-to-end behavior.
## Earlier direct docs-lane gates (`4ab63cd5`, historical, 2026-11-19)

Measured by **this docs lane** on a clean integrated tree from `rust/` with its default `rust/target` (`cargo 1.99.1`,
`rustc 1.99.1`, Homebrew, macOS arm64); the test runs used `--locked --all-targets --all-features` as recorded per row
below, and every command exited **0**. `git diff --check` and `git status --porcelain` were clean before and after. These
are **dated measurements at `4ab63cd5`**, not proof for a later diff. Totals were re-tallied from the logs themselves
(`grep -c '^test result:'` plus a sum of the passed/failed/ignored fields): `/tmp/docs-sd3f-workspace-tests.log` — 85
`test result:` lines, 2114 passed, 0 failed, 1 ignored; `/tmp/docs-sd3f-nd-db-tests.log` — 40 lines, 929 passed, 0 failed;
`/tmp/docs-sd3f-doctests.log` — 6 passed.

| Command | Actual result | Source log |
|---|---|---|
| `cargo fmt --all -- --check` | Pass (diff 0, exit 0) | `/tmp/docs-sd3f-fmt.log` |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | Pass (exit 0) | `/tmp/docs-sd3f-clippy.log` |
| `cargo test --locked -p nd-db --all-targets --all-features` | **929 passed / 0 failed / 0 ignored** across **40** suites | `/tmp/docs-sd3f-nd-db-tests.log` |
| `cargo test --locked --workspace --all-targets --all-features` | **2114 passed / 0 failed / 1 ignored** across **85** suites | `/tmp/docs-sd3f-workspace-tests.log` |
| `cargo test --locked -p nd-db --doc` | **6 passed / 0 failed / 0 ignored** — a **separate axis**, never added to the 929 (the new one is `user_repo::revoke_tokens_in_tx`) | `/tmp/docs-sd3f-doctests.log` |
| `cargo doc --locked -p nd-db --all-features --no-deps` | Pass (exit 0), **22 warnings** (8 unresolved links + 14 private-item links; `grep -c '^warning:'` returns 23 because it also counts rustdoc's own summary line) | `/tmp/docs-sd3f-doc.log` |
| `cargo test --locked -p nd-dbschema --all-targets --all-features` | **61 passed / 0 failed / 0 ignored** across **3** suites | `/tmp/docs-sd3f-dbschema.log` |
| `cargo test --locked -p nd-auth --all-targets --all-features` | **107 passed / 0 failed / 0 ignored** across **3** suites (lib 79, `http_auth` 14, `login_http` 14) | `/tmp/docs-sd3f-auth.log` |
| `git diff --check` | Pass (exit 0) | this lane |

**Axis discipline:** `--all-targets` totals **exclude doctests**; the `6` doctests are their own run. Do not reconcile
totals across axes or against an earlier head.

Dated boundaries on this line of development (historical evidence, not current): `4ab63cd5` 929 DB / 2114 workspace
across 85 suites (this lane's re-measurement; the parent's integration runs recorded the same workspace total in
`/tmp/nav-db13-integrated-tests.log`); `842c32a2` 2096 workspace across 84 suites (`/tmp/nav-db09-integrated-tests.log`);
`320b8b03` 2078 across 83 (`/tmp/nav-db08-integrated-tests.log`); `9be6ad0e` 2065 across 82
(`/tmp/nav-db07-integrated-tests.log`); `162fae1d` 2055 across 81 (`/tmp/nav-boot02-integrated-tests.log`); `bfdcfeca` 2038
across 81 (`/tmp/nav-db06-parent-tests.log`); `60be7534` 2029 across 80 (`/tmp/nav-db05-parent-tests.log`); `c791dbc1` 2021
across 79 (`/tmp/nav-db04-parent-tests.log`); `74ebd81c` 841 DB across 33 suites / 2009 workspace across 78
suites; `ec84c176` 834 DB / 2002 workspace across 77 suites; `4a8aac9f`/`ff900d3e` 813 DB / 1938 workspace across 74
suites; `ab466928` 819 DB across 31 suites; `f57ae050` 778 DB / 1857 workspace; `62789ba2` 771 DB / 1850 workspace;
`f8ceeb8f` 765 DB; `60cdd184` 737 DB / 1816 workspace (69 groups). Those intermediate totals come from the parent's named
integration logs (`/tmp/nav-db04-parent-tests.log`, `/tmp/nav-db05-parent-tests.log`, `/tmp/nav-db06-parent-tests.log`,
`/tmp/nav-boot02-integrated-tests.log`, `/tmp/nav-db07-integrated-tests.log`, `/tmp/nav-db08-integrated-tests.log`,
`/tmp/nav-db09-integrated-tests.log`, `/tmp/nav-db13-integrated-tests.log`) and are re-tallied here from the logs, not
re-executed. Toolchain at the `60cdd184` audit: rustc/cargo **1.99.1**, Go **1.27.4**, macOS arm64,
ffmpeg/ffprobe **9.0.2** available. Some process tests return early when tools/codecs are unavailable; future lifecycle
gates must require their prerequisites and record what was actually exercised. No run above booted a server or scanned
a real library.

**Rustdoc debt:** the `nd-db` doc build succeeds with **21 warnings** at `6d6938b1` (unchanged from the **21** recorded at
`7d022a02`: the D-PLUGIN deletion removed one
deleted-module unresolved link from the previous **22** = 8 unresolved links + 14 private-item links),
re-measured at `4ab63cd5` by this lane; the set is otherwise unchanged from the earlier baseline and the parent's `96c495e7`
and `632271a9` gates both reported **22 = baseline** at their HEADs, with **zero** warnings in `sql_annotations.rs`
(DB-15 runtime) and zero in `runtime_scanner_persistence_contract.rs` (DB-21 P1-DB); the parent's `b3dee0a6`/`6d6938b1` gate
still reported **21 = the post-DB-16 baseline**, i.e. DB-17's new test file added no rustdoc warnings. Do not add rustdoc
`-D warnings` as a gate without resolving that debt, and do not call a successful doc build warning-free. Measurement
pitfall recorded so it is not repeated: `grep -c '^warning:'` also counts rustdoc's own summary line, which produced a
transient 23-warning report.

**Known flake (non-DB, tracked; record CORRECTED 2026-11-20; causal-wait fix LANDED `7369daf4` 2026-11-21 — hardened, still tracked):** the `nd-stream` **integration**
test `the_poller_exits_on_teardown_even_when_the_client_never_hangs_up` is at
`rust/crates/nd-stream/tests/transcode_cache.rs:1582-1617` — an integration test, not a lib test. The earlier wording
here ("took ~30s against its 3s deadline in one full-workspace run, 1837 passed / 1 failed") is replaced, dated 2026-11-20,
because it was wrong on both numbers: `wait_for_poller` (`transcode_cache.rs:183-191`) polls 600 × 20 ms = **12 s** in
every revision, so **no 3 s deadline ever existed**, and the ~30 s figure conflated wall-clock time spent serialized behind
`PROCESS_LOCK` (~12 lifecycle tests run one at a time in that binary) with the loop's own deadline. A read-only
root-cause investigation completed 2026-11-20 found no retained log corroborating a failure — the parent's record lists
85 recorded runs, all ok, and this lane re-grepped the retained `/tmp` integration logs (88 `... ok` occurrences of this
test, zero `FAILED`) — and a source audit of every poller
finish path found no unbounded hang path (SOURCE-VERIFIED, high confidence); 26+ clean runs including 3 full-binary runs
under 8-core CPU load. Judgment: harness/load-sensitive plus environmental, with a real product bug ruled out. A scoped
fix for a later writer — a causal `wait_for_entry_gone` before `wait_for_poller` (entry finalization proves the `finish()`
announcement landed), touching `nd-stream/tests/transcode_cache.rs` only — **LANDED at `7369daf4`** (2026-11-21), verified across
36 runs including 8-core CPU-load runs. The original flake log remains unrecoverable, so the test stays tracked as hardened
rather than claimed permanently fixed. `nd-stream`'s
`transcode_body::tests::early_release_defeats_the_cap_and_accumulates_processes` (counts live `sleep` stub PIDs via
`ps`) is the other timing-sensitive one. The ignored test is `dump_divergences` (`nd-mapping/tests/go_oracle.rs`).
`--all-targets` totals are not a claim that doctests were run.

## Landed history (labels are immutable; do not re-use them to schedule)

| Label | Commit | Actual scope |
|---|---|---|
| S-D3a | `3ba10ee1` | Shared hydration seam, folder/user hooks |
| S-D3b | `6cd6c8d2` | Album/media-file hydration |
| S-D3c | `1c215a7e` | Album/media-file repository reads, artwork hydration core |
| S-D3d | `0d2dc89d` | Artist stats/similar-artists hydration and reads |
| S-D3e + S-D3f | `6a201f4a` | Track/scrobble delegation, playlist/radio reads and artwork wrappers |
| S-D3g1 | `f210431f` | Executed library/property/tag reads |
| S-D3g2 | `a8e4d57c` | Executed folder/artwork-queue reads, narrowed folder drains |
| S-D3g2 follow-up | `60cdd184` | Single-input FolderID hash, parenthesis/whitespace-aware sort splitting, Go-shaped string conversion, per-row folder hook/error ordering, tests/evidence corrections |
| W4+W6 | `f8ceeb8f` | Executed reads incl. **transcoding `read_all`/`count_all` with `command` masking** |
| W7 sort gate | `62789ba2` | `RestQueryOptions` parity on executed REST reads |
| DB-01 | `f57ae050` | Missing-state bind order + executed regression gate |
| DB-03 / D-OPEN | `4a8aac9f` | Go-DSN parity with pool-bound initializer tokens, per-connection effective-state read-back |
| DB-15 **render-only** rating repair | `ab466928` | `update_avg_rating_sql` embeds the average-rating aggregate with 3 ordered binds, executed against a real schema by `runtime_annotation_rating.rs`; the three public renderers were kept and redocumented with their prepare failure pinned — **not** the full DB-15 annotation reassignment service. **Superseded on the runtime items by the `96c495e7` DB-15 runtime row below** (durable Go oracle still deferred) |
| DB-16 move/reference-remap/delete + A8 share rejection + D-PLUGIN deletion | `7d022a02` | Executed `move_matched_mediafile` (`WithTxRetry` semantics), `remap_missing_mediafile` (single tx, no retry), NEW `reassign_references_in_tx`, `delete_mediafile_in_tx`, `delete_all_missing_in_tx` (opt-in); warn arms via the DB-14 tx-loss handler; `share update_sql` typed `EmptyUpdateColumns` rejection (A8); `plugin_repo.rs` + `lib.rs` export DELETED (D-PLUGIN executed). NEW `tests/runtime_move_persistence.rs` 18 executed tests; parent gates 2343/0/1 across 97 suites, `nd-db` 1053/0/0 (48 suites), `nd-db` rustdoc 21 = post-DB-16 baseline. No production caller; no GC/playlist-cleanup execution (DB-20); count refresh landed later at `b3dee0a6` (DB-17, row below) |
| Scanner local-FS storage subset | `e9dd8230` + merge `ff5b9c92` | `storage.rs` only: std-`fs` `list`/`stat`/`read`/`read_link`/`resolve`/`exists`, logical paths, `file://` URI parsing, explicit symlink classification. At that commit the walker, `.ndignore`/ignore policy, folder hashing and cancellation were **not implemented** — **not** SC-01 ratification or SC-02 completion. **Superseded on those four items by the `e3e0770a` row below.** |
| Scanner **SC-02 partial** (local walker + `.ndignore` + folder hash + cancellation) | `e3e0770a` (fast-forwarded onto `2e7026b2`) | `nd-scanner`: `contract.rs`/`walk.rs`/`ignore.rs`/`folder.rs` plus the `lib.rs`/`storage.rs` seam, and executed `tests/{folder_hash,ignore_policy,storage_seam,walk_policy}.rs`; the six user-approved dependencies are recorded in `decisions.md` S9 and the manifest/lock delta is limited to that approved dep set. Review findings through S9b closed. **Partial scope**: no scanner entrypoint, `MusicFS`/`ReadTags` registry dispatch, tag-to-DB ingest, full-scan orchestration, watcher or progress protocol — **not** SC-01 ratification, SC-02 completion, or MusicFS readiness, and no `*_in_tx` write has a scanner caller. |
| DB-02 scanner timestamp contract | `6d9d984d` + merge `74ebd81c` | Scanner-facing stamps/cutoffs bound as canonical TEXT via `time::value`, `library_repo::zero_time_value()` replacing raw `ZERO_TIME` integers, artwork-purge cutoff fixed to a `DateTime` bound, `runtime_scan_clock.rs` executed coverage. **Not** full scanner writes: `scrobble_buffer` `play_time`/`enqueue_time` still bind integers (see Residuals) |
| Docs consolidation | `f03aff98` + merge `3b5e6c76` | `current-status.md`/`next-work.md`/`decisions.md` consolidated to one authority chain; historical blocks marked superseded |
| DB-04 executed **property** writes | `c791dbc1` | `property_repo::put_in_tx`/`delete_in_tx` executed on the caller's transaction through `execution::execute`, update-first/insert-on-zero-row per Go, `DbOperationError::Sqlite` arm retained for busy classification; `tests/runtime_property_writes.rs` (12 executed tests). No `PropertyRepo::put`/`delete` connection-bound method and no production caller |
| DB-05 executed **library** writes + scan lifecycle | `60be7534` | `library_repo::put_in_tx` (typed `LibraryColumn` set, id-1 path-immutability guard, autoincrement + predefined-id arms, admin assignment in the same tx), `scan_begin_in_tx`/`scan_end_in_tx`/`set_scanned_pid_in_tx`/`add_artist_in_tx`; `tests/runtime_library_writes.rs` (8 executed tests). **Not** the bootstrap caller: no code creates a configured library row |
| DB-06 executed **tag** dictionary + library links | `bfdcfeca` | `tag_repo::add_in_tx` runs each 200-id chunk's dictionary INSERT then `library_tag` link INSERT on the caller's tx (`ON CONFLICT DO NOTHING` preserved); `tests/runtime_tag_writes.rs` (9 executed tests). Counts/purge (`update_counts_sql`, `purge_unused_sql`) were renderers at that HEAD; `update_counts_sql`'s execution landed at `b3dee0a6` (DB-17, `tag_repo::update_counts_in_tx`) and `purge_unused_sql` remains a renderer owned by DB-20 |
| DB-07 folder missing-state + album touch primitives | `2911b9ab` + merge `9be6ad0e` | `folder_repo::put_folder_in_tx`, `mark_folders_missing_in_tx`, `mediafile_repo::mark_media_missing_in_tx`/`mark_media_missing_by_folder_in_tx`, `album_repo::touch_albums_in_tx` and `touch_albums_with_missing_folders_globally_in_tx` (deliberately GLOBAL, Go predicate retained); `tests/runtime_folder_writes.rs` (10 executed tests). `put_folder_in_tx` takes a caller-supplied id and refuses an empty one as `FolderWriteError::EmptyId` rather than giving Go's random-id arm; the mark-list primitives take id lists and run NO statement on an empty list (Go's own chunk loop), so they are not an empty-id refusal |
| DB-08 artist writes + exact partial timestamp selection | `32c4fa5b` + merge `320b8b03` | `artist_repo::put_artist_in_tx` with the typed `ArtistColumn` set; the pre-existing defect where a filtered update force-appended `updated_at` was **corrected** to Go's `selectUpdateColumns` semantics (`runtime_timestamp_convention.rs` re-pinned). `library_artist` link is NOT written here — it stays `library_repo::add_artist_in_tx` |
| DB-09 atomic album base + participant + tag writes | `d938d159` + merge `842c32a2` | `album_repo::put_album_in_tx` executes base upsert then unconditional participant delete + optional insert, then unconditional tag delete + optional insert, all on the caller's tx. **FTS is not issued by any statement**: the fresh schema's own `album_fts_ai`/`_au`/`_ad` triggers maintain `album_fts`, and `runtime_album_writes.rs` (18 executed tests) observes it through `MATCH` and the contentless row count. Album aggregation/`equals` remain SC-owned (`decisions.md` S4) |
| DB-10 identity/base/junction writes | `65072b89` + signed merge `6e5ca97d` | `put_mediafile_in_tx`, 31 executed tests. Empty-ID Match miss mints inside each tx attempt; retries may mint different GUIDs, failed attempts publish no ID, only committed result returns ID. BUSY test fails initial INSERT before junction/FTS, not commit failure of a completed bundle. Review APPROVE, follow-up P3s fixed (parent report); no production scanner caller |
| DB-12 artwork invalidation/enqueue subset | `e5d0592d` (fast-forward), rustdoc fix `f80b9704` | Transaction-bound delete-for-items and three enqueue variants; 14 executed tests; review APPROVE after follow-up test fixes (parent report). Not worker/full scanner; no production scanner caller |
| DB-13 epoch-only token revocation | `eae4ba1d` + merge `4ab63cd5` | `user_repo::increment_token_epoch_sql` (guarded `token_epoch = token_epoch + 1 … RETURNING token_epoch` with `typeof(token_epoch) = 'integer'` and `< i64::MAX`) executed by `revoke_tokens_in_tx` on the caller's tx, with `RevokeError::NotFound`/`Exhausted` classified by a same-tx existence read; `tests/runtime_token_revoke.rs` (18 executed tests). Epoch-only revoke is a **Rust-side addition** — Go bumps the epoch only inside the password UPDATE. No route/BOOT consumer exists |
| DB-11 executed scanner reads, cursor contract, API-safe point read | `52b04064` | NEW `nd-db/src/scanner_queries.rs`: row-visitor cursors on one leased connection (`Control`/`VisitOutcome`/`CursorError` implementing the D-CURSOR three-way stop taxonomy; `IdChunkedCursor` terminal after a failed chunk), eight executed scanner reads (`media_in_folder`/`media_in_album`/`media_in_filter` counted as one read, plus `missing_and_matching`, `recent_candidates`, `touched_albums`, `album_ids_by_folders`, `sole_album_artist_ids_in_subtrees`, `media_by_paths`), the opaque scope authorities `ScannerScope` (system, configured library ID only) and `UserScope` (resolved once per request: admin/headless(-1)/all-grants omit the clause, otherwise the deny clause `library_id IN (SELECT … FROM user_library …)`; zero-grant and empty-library arms both deny; **no `1=1` for users**), plus the `get_visible_media` point read as a thin wrapper over `MediaFileRepo::get`. Review fixes folded in: `folder_subtree_filter` returns a fresh conjunction root per Go (`folder_repository.go:214-216`; mixed root+path batches bind and execute), the sole-album-artist subtree builder drops `album.missing` (parity `album_repository.go:294-296`), and `CURSOR_CHUNK_SIZE` 300 → 600 (D-CURSOR(4)). NEW `tests/runtime_scanner_queries.rs` = **21 executed tests**. **Not done:** full cursor/REST completion (the `SEEDEDRAND` seeded full-set render is deferred to the REST/API lane), no production scanner caller, no `nd-scanner` consumption |
| DB-15 runtime annotation transfers + avg-rating recalc | `96c495e7` | `sql_annotations.rs`: `AnnotationEntityKind` closed enum and `AnnotationWriteError`; `update_avg_rating_in_tx` and `reassign_annotation_in_tx` execute on the caller's tx (DB-00 seam). Go `ReassignAnnotation` parity (`sql_annotations.go:185-197`): the OR IGNORE move keeps the target's conflict, leaves the conflicting source row, and recalculates the target average in the same tx. NEW `tests/runtime_annotation_transfers.rs` = **5 executed tests** (pre-fix param-count control, conflict identity, type isolation, absent-source no-op recalc, FK-injection rollback with row and cache read-back). **The durable Go-harness oracle is deferred to the EVID lane** (PARENT DECIDED 2026-11-20, `decisions.md` H6; authorization gate in `subplans/README.md`); Go behavior is pinned by source citations plus `/tmp`-local Go driver evidence, labeled local and not committed. With the `ab466928` render-only repair this covers DB-15's plan scope **except that oracle** — DB-15 is not fully DONE |
| DB-14 executed folder-delta persistence (DB-owned SC seam) | `de0f49ea` | **LANDED.** NEW `nd-db/src/scanner_writes.rs` (935 lines): `FolderWriteBatch` (immutable; SC pre-generated folder/artist/album IDs; caller stamps `FolderStamps`; `full_scan_mode`; `album_transitions`), `persist_folder_delta(store,&batch) -> Result<CommittedFolderOutcome,FolderRunError>` (DB-owned per-folder retrying tx per `D-BATCH`), `persist_folder_delta_in_tx` **pub(crate)**, `FolderWriteStage` (12 arms, `is_fatal`), `ScopeRejection` (11 arms, validated pre-checkout), `FolderWriteWarning{stage,item_id,related_id,source}`, `CommittedFolderOutcome` (publish-only-after-commit), `scan_artwork_item`. NEW `tests/runtime_folder_persistence.rs` = **17 executed tests**; `src/lib.rs` +1; `album_repo.rs` +102 additions-only delivering `copy_album_created_at_in_tx` (`album_repo.rs:1566`, Go `album_repository.go:331-357` incl. the `0001-`/empty skip). Go `persistFolder`/`persistAlbum` order (`phase_1_folders.go:348-506`); warn arms route through `warn_arm`, which detects transaction loss via `is_autocommit` and returns the original typed SQLite error as fatal (never a warning); ABORT-class statement failures still warn and commit; `MediaIdentity` per-record; Go `c == 0` missing-quirk preserved. Review REQUEST-CHANGES, both findings closed (P1 tx-loss atomicity, P2 consumed-transitions retry proof), parent re-ran all gates at `de0f49ea` (**2319 / 0 / 1 across 95 suites**, fmt + workspace clippy clean, `nd-db` rustdoc 22 = baseline). **Not done:** no production caller (SC-05 is the consumer, `nd-scanner` has no `nd-db` dependency), no walk/tag-mapping/album aggregation (SC-owned), no missing-folder finalize (SC-06) |
| DB-21 **P1-DB exit contract** (DB-side-only) | `632271a9` | **LANDED (P1-DB half).** NEW `nd-db/tests/runtime_scanner_persistence_contract.rs` (4206 lines, **7 executed tests**): (a) composed regression through the **real pool** — 4 deltas / 2 libraries / 3 album-ID transitions, both `full_scan_mode` values, a second pass over 7 committed dirty-state arms with stored-effect assertions, plus a control pass; (b) frozen API surface — 51 compile-checked SigNN pins across the landed phase-1 primitives plus the DB-11/14/15 public surface, Go literal values 50/600/200/100 pinned, an executed unknown-album queue-exclusion sentinel; (c) CONSUMER_HANDOFF **U-ledger** pinning `SC.mapping-bridge`, `SC.phase1-fixture`, `BOOT.configured-library-caller` and `production_full_finalize_db_contract` as **Unmet** (asserted by test, cannot drift green), plus `db_internal_contract` recording the `pub(crate)` `persist_folder_delta_in_tx` compile-pin gap. It also carries the executed 5-class inventory behind the **deferred renderer-prepare gate** (`deferred_renderer_prepare_gate_classes_are_executed`). Parent decisions: `decisions.md` "DB-21 exit scope and prepared-statement gates" (PARENT DECIDED 2026-11-21). Review REQUEST-CHANGES (6 findings) → APPROVE-WITH-FIXES, 3 P3 comment fixes by the parent. Parent gates at `632271a9`: **2326 / 0 / 1 across 96 suites**, `-p nd-db` 1036/0/0 (47 suites), fmt/clippy/`diff --check` clean, `nd-db` rustdoc 22 = baseline. **Not done:** no production caller (SC-05 is the first consumer); no full-exit — collection/count API for later API-F* still belongs to **DB-21(FULL)**; not #27A progress; stable ID DB-21 incomplete until FULL-SCANNER-DB |
| BOOT-02 native expiring login sessions | `c6e220eb` + merge `162fae1d` | `login.rs` mints via `Authenticator::create_native_session_token` (native audience + checked positive `exp`), replacing the audience-less `create_token` call that made the login token unusable at `AuthGate::Api`; `tests/login_http.rs` (14) and `tests/http_auth.rs` (14) executed, including a legacy audience-less **negative control** and an epoch-revoke round-trip. The remaining HTTP API-key extraction/issuance middleware is **not** part of this scope |
| DB-17 playcount/tag/library count refresh | `b3dee0a6` | Executed four caller-tx refresh helpers: `album_repo::refresh_play_counts_in_tx`, `artist_repo::refresh_play_counts_in_tx`, `tag_repo::update_counts_in_tx`, `library_repo::refresh_library_totals_in_tx(id, now)` (8 probes + 10-bind update; Go discards `RowsAffected` at `library_repository.go:249` and Rust returns the written `RefreshStatsTotals` — documented deviation). NEW `tests/runtime_count_refresh.rs` = **20 executed tests** pinning the faithful REPLAY backwards-overwrite (bare `play_date = excluded.play_date`, **no monotonic guard added**), zero-arg statements, caller-stamped canonical TEXT (no clock in SQL), NULL-exclusion, statement-level `RowsAffected` vs entity counts, probe-then-update order, and rollback as the **CALLER's** obligation via the DB-00 typed seam. Review APPROVE-WITH-FIXES (F1 `run.Parallel` misconception, F2 rollback docs, F3 two helper-internal failure regressions, F4 three doc inaccuracies). Merged-tree gates at `6d6938b1`: 2363/0/1 across 98 suites, `nd-db` 1073/0/0 (49 suites), `nd-db` rustdoc 21 = baseline. No production caller (SC's phase-3 `:138`/`:150` and `scanner.go:316`/`:344` triggers are the consumers); `purge_unused_sql` stays a renderer (DB-20) |
| BOOT-00 construction/dependency/launch gate | `6d6938b1` | **Gate work only, no production behavior.** D3 manifest pins executed: `chrono "^2"`→`"0.4"`, `tracing "^0.5"`→`"0.1"`, `tracing-subscriber "^0.1"`→`"0.3"`, `base64 "^0.8"`→`"0.22"` — **zero new lock entries** (the lock diff is only the `nd-http` dependency-edge stanza). D1 executed: `nd-http` scaffold → typed module with the **EXECUTED** `dependency_graph_has_no_http_api_cycle` acceptance (`cargo metadata --locked` graph check + negative control) and the `RouterInjection = fn(axum::Router, &nd_conf::Config) -> axum::Router` shape; `nd-cli`/`nd-api` rustdoc boundary notes record the `ND_CONFIGFILE` resolution gap (Go `conf/configuration.go:1201`; Rust doc-only at `nd-conf/src/config.rs:390`) and the `Dir::path` `create_dir_all` hazard. D4 via the B3-ratified record; D5 stays with the lane report. `nd-http` suite 1/1 (the cycle test replaces the dropped `nd_http_instantiates` scaffold test); `main` is still empty and nothing listens — **BOOT-01** owns `ND_CONFIGFILE` resolution and the B2 ffmpeg-config injection |

Executed write primitives are **transaction-bound and caller-trusted** (`WHERE id = ?` and nothing else, exactly like
Go): they own no scope filter, no auth check, and no commit. None has a production caller at `de0f49ea` either
(SOURCE-VERIFIED: `*_in_tx`, `scanner_queries` and `scanner_writes` references outside their definitions are tests/doctests or internal
helpers; `nd-scanner` declares no `nd-db` dependency).

The old scout's S-D3b meant folder/playlist and its S-D3c meant album/media-file; `hydrate-scan-contract.md` numbers
the same lanes differently (see the numbering-drift note in `next-work.md`'s historical index). All rows above are in
this baseline — do not cherry-pick or repeat them. Playlist rules decoding is wired; old labels saying otherwise are
stale.

## Capability axes (source-inspected, not a completeness percentage)

| Surface | Rendered / decoded | Production execution | Mounted / end-to-end |
|---|---|---|---|
| Shared marshal, JSON/role/FTS feeders | Present; eight row hooks exist | Consumers below; marshal tests also manually execute SQL bundles | Not a full entity-write or scanner contract |
| User/player | Read and mutation builders/records | Substantial reads and writes execute, incl. DB-13 `user_repo::revoke_tokens_in_tx` (`eae4ba1d`) | Auth consumers exist (login mints a native expiring session at `c6e220eb`); no server bootstrap, no revoke route |
| Share/user properties | Present | Raw-connection reads/writes execute, outside the shared execution helper | Full share service not implemented |
| Album/media-file/artist/folder/playlist/radio | Record/hook/read builders present | Substantial reads; executed entity writes landed for **artist** (`32c4fa5b`), **album** (`d938d159`), **folder** (`2911b9ab`) and media-file **missing-state plus identity/base/junction writes** (`put_mediafile_in_tx`, DB-10 `65072b89`) | No ingest or mounted API |
| Scanner reads: folder/album filters, missing+matching, recent candidates, touched albums, subtree artists, paths | `scanner_queries.rs` builders and scope types | **Executed reads landed at DB-11 `52b04064`** (row-visitor cursors on a leased connection, `ScannerScope`/`UserScope`, `get_visible_media` point read; 21 executed tests) | No scanner caller, no mounted API, not the full cursor/REST surface |
| Library/property/tag | Plain mappers and builders present | Reads **and** executed writes landed (property `c791dbc1`, library + scan lifecycle `60be7534`, tag `bfdcfeca`) | No library bootstrap/scan-lifecycle caller |
| Playlist tracks/scrobble buffer | Delegating hooks and builders present | Partial reads; buffer dequeue executes | Not full playlist/scrobbler services |
| Artwork queue | Builders and plain mapper present | Reads/counts, `purge_queued`, transaction-bound enqueue variants and artwork delete-for-items execute (DB-12 `e5d0592d`) | No production scanner caller; worker lifecycle not implemented |
| Transcoding module | Builders present | **Executed reads landed at `f8ceeb8f`** (`read_all`/`count_all`, `command` masked for a non-admin) | No route, no default-profile seed; profile **editing** out of scope |
| Artwork/genre/playqueue/scrobble/plugin/smart_playlist | genre/playqueue/scrobble executed reads **landed at `272c9c66` (DB-22)**; artwork item-image lookup and invalidation execute; plugin is **deleted** (D-PLUGIN executed at `7d022a02`); smart_playlist remains render-only | Artwork item-image lookup separately executes in `artwork_hydration.rs`; artwork `delete_for_items_in_tx` executes | Not full repository/service completion; `plugin` deleted by D-PLUGIN (`decisions.md`, executed at `7d022a02`, DB-16), so it is no longer a render-only module; `smart_playlist` not a separate Go repo |
| DB store/pool | Sync/async lease and transaction/retry APIs exist | SQLite work runs on the blocking boundary | Effective per-connection configuration **landed** (DB-03); typed seam **landed** (`48e65806`/`ec84c176`, DB-00) and DB-04…DB-10/12/13 plus DB-15's runtime writes consume it; DB-11's cursors hold one lease for the whole iteration (D-CURSOR). **DB-21(P1-DB) `632271a9`** adds the executed **DB-side-only P1 exit**: composed phase-1 regression driven through the real pool (both `full_scan_mode` values, second pass over committed dirty-state arms) plus a compile-checked API freeze (51 SigNN pins) — an executed contract on the pooled path, **still no production caller** |
| Schema | Fresh DDL, migrations/marker, extensions, indexes, backup/optimize | Executed schema tests (61 at `4ab63cd5`) | No default library row; repair deferred; no bootstrap caller |
| Tagreader/mapping | Real lofty backend and metadata mapping | Crate-level implementation/tests | No storage→record→DB ingest caller |
| Auth | Credential/JWT/login/refresh/middleware components | Crate-level mounted test routers (107 executed at `4ab63cd5`) | Login→native-audience round-trip **closed** at `c6e220eb`; HTTP API-key extraction/issuance middleware still absent; no production router |
| Stream #15 | Handler/cache/limiter/archiver implemented | Crate-level serving/lifecycle tests | **Component-level complete, not HTTP-ready**; header commitment/Send adapter gaps |
| Scanner/HTTP/API/CLI | Scaffolds / empty `main` | No production scan/listener/router | **Not started at the integration boundary**; local-FS storage subset `e9dd8230`/`ff5b9c92` then SC-02 partial walker landed `e3e0770a` (**not** SC-01/SC-02 completion) |
| Scheduler | Scheduling primitives implemented | Crate tests | Full maintenance/logging and bootstrap remain |

Repository sources: `rust/crates/nd-db/src/*_repo.rs`, `hydrate.rs`, `marshal.rs`, `store.rs`, `pool.rs`, `execution.rs`.
Executed reads do not prove all methods execute. Re-counted at integrated `master` `272c9c66` (DB-22):
(SOURCE-VERIFIED: grep for `execution::query`/`execution::execute`/`read_hydrated`/`prepare_and_then` across the 23
`*_repo.rs` modules): **19 modules execute through the shared helper** (artwork, property, playlist_track, radio,
scrobble_buffer, transcoding, album, artist, tag, folder, artwork_queue, mediafile, playlist, library, player, user,
**genre, scrobble, playqueue** — the last three added by DB-22 `272c9c66`),
**2 execute on raw connections** outside it (`share`, `user_props`), and at that boundary **two were render-only** (`plugin`,
`smart_playlist` — `smart_playlist` is not a separate Go repository). **Amended at `7d022a02` (DB-16): `plugin` is DELETED**
(D-PLUGIN, `decisions.md`, record at `89b72894`), so the census is now **22 modules — 19 via the shared helper, 2 on raw
connections, 1 render-only (`smart_playlist`)**.

Re-counted at `de0f49ea` (SOURCE-VERIFIED): the 23-module census above is unchanged, `src/scanner_queries.rs` remains a
**new executed-read module outside that census** — its visitors and `IdChunkedCursor` prepare and drain statements on a
leased `&Connection` held for the whole iteration (D-CURSOR), so they do not go through the `execution` helper, and
`get_visible_media` reaches the already-executed `MediaFileRepo::get`. `sql_annotations.rs` gained two executed
tx-bound writes (`update_avg_rating_in_tx`, `reassign_annotation_in_tx`) at `96c495e7`, and `src/scanner_writes.rs`
(**LANDED `de0f49ea`**) is a second module outside that census: it drives writes through the landed `*_in_tx` primitives
on the DB-owned retrying transaction rather than adding its own helper path.
**Twenty-five `pub` executed transaction-bound write functions plus one `pub(crate)` seam** exist
(`grep 'pub fn .*_in_tx'` = 25 at `de0f49ea`, plus `persist_folder_delta_in_tx`):
**Amended at `7d022a02` (DB-16): 29 `pub fn *_in_tx` + 3 `pub(crate) fn *_in_tx` = 32** (`git grep` at that HEAD) — DB-16 added four
`pub fn *_in_tx` (`mediafile_repo::reassign_references_in_tx`, `reassign_bookmark_in_tx`, `delete_mediafile_in_tx`,
`delete_all_missing_in_tx`) and two `pub(crate) fn *_in_tx` (`move_matched_mediafile_in_tx`, `remap_missing_mediafile_in_tx`)
behind the `pub` store-level entrypoints `move_matched_mediafile`/`remap_missing_mediafile`, and deleted
`plugin_repo` (no `*_in_tx` surface). **Amended at `b3dee0a6` (DB-17): 33 `pub fn *_in_tx` + 3 `pub(crate) fn *_in_tx` = 36** (`git grep` at that HEAD) — DB-17 added four `pub` refresh helpers (`album_repo::refresh_play_counts_in_tx`, `artist_repo::refresh_play_counts_in_tx`, `tag_repo::update_counts_in_tx`, `library_repo::refresh_library_totals_in_tx`); the one remaining private `fn assign_admins_in_tx` (`library_repo.rs:1301`) is a non-public helper and is not counted. The enumeration below is the `de0f49ea` inventory:
property `put_in_tx`/`delete_in_tx`, library `put_in_tx`/`scan_begin_in_tx`/`scan_end_in_tx`/`set_scanned_pid_in_tx`/
`add_artist_in_tx`, tag `add_in_tx`, folder `put_folder_in_tx`/`mark_folders_missing_in_tx`, mediafile
`mark_media_missing_in_tx`/`mark_media_missing_by_folder_in_tx`, artist `put_artist_in_tx`, album
`put_album_in_tx`/`touch_albums_in_tx`/`touch_albums_with_missing_folders_globally_in_tx`, user
`revoke_tokens_in_tx`, plus DB-10 `put_mediafile_in_tx`, artwork `delete_for_items_in_tx`, and queue
`enqueue_in_tx`/`enqueue_all_missing_in_tx`/`enqueue_if_missing_in_tx`, plus DB-15 `sql_annotations::update_avg_rating_in_tx`/
`reassign_annotation_in_tx` (`96c495e7`), plus DB-14 `album_repo::copy_album_created_at_in_tx` and the
`pub(crate)` `scanner_writes::persist_folder_delta_in_tx` (`de0f49ea`). `annotation_repo.rs` does not exist because annotations ride album/media-file
(DB-15), so that is not a gap.

## Landed safety gates (former open blockers, now closed with executed regressions)

- **Missing-update bind order — CLOSED at `f57ae050`** (DB-01): `mark_missing_sql` args follow Go's `.Set`/`.Where`
  order; `runtime_missing_state.rs` executes 7 real-schema tests plus a pre-fix control. Details: `decisions.md` W0.
  **Other** latent write defects remain — DB-08 found and fixed one of them (filtered artist updates force-appending
  `updated_at`, corrected at `32c4fa5b`); do not connect existing renderers unchanged.
- **Timestamp/idle semantics — CLOSED at `6d9d984d`/`74ebd81c`** (DB-02/D-TIME, `decisions.md` T1-T5): scanner-facing
  stamps and cutoffs bind canonical TEXT through `time::value`; the idle/scan-end comparison uses the deliberate
  `library_repo::zero_time_value()` API instead of raw `ZERO_TIME` integers; year-one `time::zero()` and the fresh-DDL
  `0000-00-00 00:00:00` sentinel behavior are preserved as inherited Go semantics (pinned, not "normalized"), and the
  artwork-purge integer-cutoff regression is fixed with executed coverage in `runtime_scan_clock.rs`. Not a demand for
  Go database migration support. The DB-04…DB-09 write lanes consume this convention for their stamps. **Known
  residual:** `scrobble_buffer.play_time`/`enqueue_time` still bind integers.
- **Pool configuration — CLOSED at `4a8aac9f`** (DB-03/D-OPEN): explicit typed options, per-connection PRAGMA,
  effective-state read-back before pooling, Go-compatible DSN parser (`decisions.md` P1). FK-on and the 5000 ms rusqlite
  default are pinned facts, not assumptions.
- **Typed repository/transaction errors — CLOSED at `48e65806`/`ec84c176`** (DB-00/D-ERROR, `decisions.md` E1-E3,
  retryable set H2): additive seam beside `with_tx*`, `RollbackFailed` keeps original + rollback errors reachable,
  `BusyExhausted` replaces strings, `Depleted`/`Poisoned` checkout states are distinct. Consumers adopted it lane by lane:
  DB-04…DB-09 and DB-13 all return `DbOperationError` with the concrete `rusqlite::Error` (extended code intact) on the
  `Sqlite` arm, so `store::is_busy` still classifies retryable failures.

First-wave lanes (DB-01, DB-03, DB-00, DB-02, plus the DB-15 render-only repair and the scanner local-FS storage
subset) are merged into `74ebd81c`, and DB-04 through DB-09 plus DB-13 and BOOT-02 are merged into `4ab63cd5`. DB-10 and
DB-12's executed subset are merged into `f80b9704`; **the SC-02 partial (local walker, `.ndignore`, folder hash,
cancellation) is LANDED at `e3e0770a`** — a partial slice, not SC-02 completion. DB-22's former DB-12 ownership block is
released. **DB-11 is LANDED at `52b04064`**, **DB-15's runtime half at `96c495e7`**, and **DB-14 at `de0f49ea`**, so
`D-CURSOR`/`D-SCOPE` (`decisions.md`, corrected record `8b5b3925`) and `D-BATCH` are now consumed by executed code; the
spine's next package is **DB-18** (`DB-18 → 19 → 20 → DB-21(FULL-SCANNER-DB)`, gated on `D-STATS`, which still has no canonical
`decisions.md` record). **DB-21(P1-DB) is LANDED at
`632271a9`** as the DB-side-only P1 exit (executed composed contract + API freeze + executed U-ledger), so it adds **no
production caller** and is not the full DB-21 exit; **DB-16 is LANDED at `7d022a02`** (executed move/reference-remap/delete
primitives, A8 share typed rejection, D-PLUGIN deletion; 18 executed tests, also with no production caller), **DB-17 is LANDED at
`b3dee0a6`** (four caller-tx count-refresh helpers, 20 executed tests, no production caller — SC's phase-3 `:138`/`:150` and
`scanner.go:316`/`:344` triggers are the consumers) and **BOOT-00 is LANDED at `6d6938b1`** (manifest pin repair + D1 dependency
boundary + the executed cycle test; **gate work only — no production behavior, nothing listens**).

## Remaining blockers and integration obligations (SOURCE-VERIFIED)

1. **Executed scanner writes and queries are partly missing.** Landed on this branch line: leaf property/library/tag
   writes, folder/album/media-file missing-state and touch primitives, artist and album entity writes with participant/tag
   junction atomicity (`c791dbc1`, `60be7534`, `bfdcfeca`, `2911b9ab`, `32c4fa5b`, `d938d159`), plus DB-13 epoch revoke
   (`eae4ba1d`), DB-10 identity/base/junction writes (`65072b89`), DB-12 artwork invalidation/enqueue (`e5d0592d`),
   **DB-11 typed scanner reads/cursors and the API-safe point read (`52b04064`)** and **DB-15 annotation transfer/avg-
   rating execution (`96c495e7`)**, **DB-14 executed folder-delta persistence (`de0f49ea`)**, **DB-16 move/reference/delete
   (`7d022a02`)** and **DB-17 count refresh (`b3dee0a6`)**. **Still unexecuted:** the scoped eleven-operation GC (DB-20) and
   playlist orphan cleanup (DB-19/20). `MediaFileRepo::get_all` still does not forward arbitrary scanner filters
   (DB-11's typed reads are the scoped path), and a raw user-supplied SQL fragment is not an acceptable substitute.
   Executed primitives also still have **no production caller**: nothing invokes them from a scanner, bootstrap, or route.
2. **Scanner mapping/equality seams** — `nd_mapping::map_file` (`nd-mapping/src/lib.rs:78-86`) forces library 1 and an
   empty folder id; `nd_model::album::equals` (`album.rs:174-176`) compares only ids. The scanner plan must own a tested
   aggregate/equality bridge or obtain the domain-owner decision (`decisions.md` S4). IPC correction: Go's external
   scanner gob carries `ProgressInfo` (`scanner/external.go:86-96`, `cmd/scan.go:71-76`), not tag-reader results;
   watcher/progress-protocol are later parity packages, not gates for a limited in-process fixture scan.
3. **Auth integration** — the login-audience gap is **CLOSED at `c6e220eb`** (BOOT-02): `login.rs:142-145` mints through
   `Authenticator::create_native_session_token` (`credentials.rs:163-175`, native audience plus a checked positive `exp`),
   so a login token now passes `AuthGate::Api` (`middleware.rs:121-129` → `check_api_claims`, `credentials.rs:356-367`);
   `tests/login_http.rs`/`tests/http_auth.rs` execute the round-trip, an epoch-revoke rejection, and a legacy audience-less
   **negative control**. Preserve expiring sessions, refresh/epoch checks and the strict native audience; do not replace
   sessions with non-expiring API tokens or weaken gates. **Still open:** no canonical HTTP API-key extraction/issuance
   middleware — `verify_api_key_hash` (`credentials.rs:395`) and `PlayerRepo::find_by_api_key` have no non-test caller, and
   `supplied_tokens`/`resolve_username` (`verifier.rs:78-108,122-140`) cover JWT header/cookie/query plus ExtAuth only.
4. **Stream integration** — `StreamHandler::serve` (`handler.rs:1302-1507`) returns status/headers after pumping and
   teardown, `prepare` is private and may change the body kind, and the boxed-future/trait-object `Send` contract needs a
   deliberate network adapter seam. Preserve bounded backpressure, cancellation, lease/slot ownership, cache semantics,
   and post-header versus pre-header error behavior. Network tests must exercise real body backpressure/drop, child
   cleanup and recovered slots with real tools/codecs; an early-returning test is not lifecycle evidence.
5. **No library row** — fresh DDL has none; configured-library creation belongs to an explicit bootstrap
   transaction/caller using typed executed DB methods, not an invented schema-golden seed.
6. **Scope decisions** — `plugin_repo.rs` is **DECIDED and EXECUTED** (D-PLUGIN, `decisions.md`, record `89b72894`: delete the module,
   its tests and the `lib.rs` export as out-of-scope dead code; **executed at `7d022a02`** by DB-16, so no render-only plugin surface
   remains). Still open: Part C per-key config
   scopes; oracle-harness authorization for new tracked Go harnesses.

## Bounded source-level residuals (amended at integrated `master` `f80b9704`; older line references retained, not a completeness claim; SC-02 partial landed later at `e3e0770a`, and DB-11 `52b04064` / DB-15 runtime `96c495e7` did not re-check these lines)

- `scrobble_buffer_repo::enqueue_sql` still binds `play_time`/`enqueue_time` as **integers** (`Value::Integer`,
  `scrobble_buffer_repo.rs:206-226`) on an INTEGER-affinity `datetime` column, while the DB-02 contract binds TEXT
  elsewhere. Distinct and deliberate: `scrobbles.submission_time` is intentionally an integer epoch and is not part of
  this convention.
- `find_recent_files_by_properties_sql` (`mediafile_repo.rs:1903`) is converted to the TEXT cutoff shape but has **no
  executed regression pin**; the sibling MBZ path is pinned (`runtime_scan_clock.rs:222`, which executes
  `find_recent_files_by_mbz_track_id_sql`), so the properties path is the uncovered one.
- **Still unexecuted on this branch line:** the scoped GC (DB-20) and playlist orphan cleanup (DB-19/20).
  DB-11's typed scanner reads/cursors landed at `52b04064` (the `SEEDEDRAND` seeded full-set render and cursor
  consumption on the REST/API side stay with that lane), DB-15's annotation execution at `96c495e7` (its durable Go
  oracle is deferred to the EVID lane), DB-16's move/reference transactions at `7d022a02` and DB-17's count refresh at
  `b3dee0a6` (`tag_repo::update_counts_in_tx`, `album_repo`/`artist_repo::refresh_play_counts_in_tx`,
  `library_repo::refresh_library_totals_in_tx` are executed). `tag_repo::purge_unused_sql` remains a renderer (DB-20).
- Executed write primitives are **caller-trusted primitives, not services**: no scope filter, no auth check, no commit,
  and no production caller. Empty-id `Put` refusal — `FolderWriteError::EmptyId`, `ArtistWriteError::EmptyId` and
  `AlbumWriteError::EmptyId` in `put_folder_in_tx`/`put_artist_in_tx`/`put_album_in_tx` — is a **recorded port-side
  deviation from Go** (Go generates a random id), documented on each function rather than silently applied. The
  mark-list primitives (`mark_folders_missing_in_tx`, `mark_media_missing_in_tx`,
  `mark_media_missing_by_folder_in_tx`) are NOT part of that deviation: they take id lists, accept an empty one and
  run no statement, exactly like Go's chunk loop.
- The scanner local-FS subset, the **SC-02 partial** at `e3e0770a`, DB-11 `52b04064` and the DB-15 runtime half
  `96c495e7` remain **partial scopes** (not SC-01/SC-02 completion, not the full annotation service, not the full
  cursor/REST surface, and no executed read or write has a production caller); storage→record ingest, aggregation/
  equality bridge, controller/watcher and progress protocol are unstarted. There is **no server readiness**: BOOT-00 landed
  the `nd-http` dependency boundary (`RouterInjection` + the executed cycle test) but there is still no router,
  listener, library bootstrap, or integrated stream HTTP adapter.

## Residuals (parity debt vs first-boot gates, kept distinct)

Genre JOIN/filtering, smart-playlist/playlist-track OR grouping and alias hazards must close **before their consumers
are exposed**; they are not accepted behavior. Participant artist re-fetch, true lazy cursors, and per-row hook/error
ordering in sibling bulk readers need named consumer gates; sibling error precedence is source-inferred, not a freshly
executed counterexample, and folder's full-drain adapter is not a claim of universal lazy-cursor parity. Accepted
deviations (`decisions.md` A1-A7: offset-only `LIMIT -1`, JSON escaping/diagnostics ordering, Rust error shapes,
`i32` narrowing, artwork error exposure, atomic first-admin) stay distinct from deferred bugs and unverified behavior.
Do not silently fix unrelated parity details under a scanner task.

Go harnesses and traces for the hydration lanes exist locally but are **not clone-reproducible tracked fixtures**; the
previously tracked participant/feeder/plain-JSON harnesses have narrower scopes and cannot certify transaction/query
behavior they do not execute. Durable captures, pinned versions, canonicalization and regeneration recipes belong to
the EVID lane (`subplans/evidence-ops.md`), and adding or changing Go harness sources requires explicit permission.
Never derive Go expectations from Rust or commit scratch binaries/DBs.

Repair, full maintenance/logging, complete API/services, HTML shares and metrics remain separate scopes and are not
automatic first-track/first-boot prerequisites. Cancelled Subsonic/Jellyfin/plugin/sonic/UI work stays cancelled; first
playback uses the canonical API, not resurrected `/rest` routes. Player scope is settled — the player-session service
and owner-scoped editing are retained, mpv/jukebox is dropped (`D-PLAYER26`, `D-PLAYER-EDIT`; full evidence in
`subplans/README.md`), the transcoding read now executes, and only the BOOT-owned `DEFAULT_TRANSCODINGS` seed plus the
canonical routes remain. Neither half gates #27A, and #26 is not DONE: no player route is mounted (`nd-api` is a
scaffold).

## Provenance

Conclusions here come from **this lane's own executed gate runs on the clean integrated `master` worktree at
`4ab63cd5`** (logs under `/tmp/docs-sd3f-*.log`), from source inspection at the pinned commits, and from the parent's
dated integration logs for intermediate boundaries. Read-only investigation briefs' local `/tmp` reports are supporting
evidence only (agent IDs and provider failure histories are deliberately not canonicalized in these docs); in-flight
lane worktrees were inspected read-only for **branch state only**, never treated as merged results. Nothing is complete
merely because it is planned, and no implementation is authorized by this snapshot.
