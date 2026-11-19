# DB runtime implementation subplan — assignment contracts, NOT implementation authorization

**Package `Status:` lines below are the planner's state at `60cdd184` and are superseded where they conflict with**
[`../current-status.md`](../current-status.md) **and** [`../subplans/README.md`](../subplans/README.md) **(2026-11-19):**
DB-01 LANDED `f57ae050`, DB-03 LANDED `4a8aac9f`, DB-00 LANDED `48e65806`/`ec84c176`, DB-15 render-only repair LANDED
`ab466928` (full DB-15 NOT done), scanner local-FS subset LANDED `e9dd8230`/`ff5b9c92` (not SC-01/SC-02), **DB-02 LANDED
`6d9d984d`/`74ebd81c`** (canonical TEXT stamps, `zero_time_value()`, artwork-purge cutoff; scrobble-buffer integer stamps and
the un-pinned `find_recent_files_by_properties_sql` cutoff remain open). **No lane is in flight; DB-04 is next.** Settled decisions now live in
[`../decisions.md`](../decisions.md); the record blocks in this file stay as evidence.

Baseline: `master 60cdd18402c0fd4b09b65e0b64ca494330501c02`. Parent owns orchestration/integration. Workspace `wks_7bb3344766422a39`, host `srv_QsD9e1TgIsNq`. This plan was produced read-only; no code, tests, formatting, delegates, commits or workspaces were created. Only this `/tmp` report was authored. Parent documentation changes appeared during planning; source references below remain pinned to the baseline, not to their evolving status prose.

Read the full mandatory `/tmp/navidrome-subplan-template.md`. This document applies all ten required fields to each package. Stable IDs are `DB-00` through `DB-21`; external semantic aliases `SC.*`, `BOOT.*`, `API.*` await parent normalization. READY means no unresolved design prerequisite, NOT permission to edit. BLOCKED means dependencies must land first. DECISION REQUIRED means the parent must record an explicit contract before implementation begins. No package authorizes production DB mutation.

## Shared assignment charter (binding on EVERY package)

**Ownership.** One writer per checkout, even for disjoint files. Stay in inherited workspace. No delegation/worktree/workspace creation, existing Go-source changes, generated-file hand edits, Cargo manifest/lockfile changes, commit/push, formatter auto-fix or unrelated cleanup. Scratch only `/tmp`. Proposed new Rust/testdata paths below do not exist yet unless called existing. Source documentation comments may change within owned code; repo planning documents are parent-owned (`docs/rust-port/current-status.md`, `next-work.md`, `hydrate-contract.md`, `entity-write-contract.md`, `hydrate-scan-contract.md`, `HANDOFF.md` and related ledgers). A child reports needed ledger deltas, not edits to those documents.

**Boundary.** DB owns local typed records, query/persistence APIs, SQLite configuration mechanics, transaction/error handling, and final DB write sequences. `SC.mapping-bridge` owns mapping DTO -> DB record construction, storage facts, participant sort-name input, pure album aggregation/equals, filesystem/controller/phase orchestration. DB MUST NOT move mapping/album domain logic into nd-db. `BOOT.db-open-policy` owns initialization policy and configured library-creation caller, not implicit schema seeds. `BOOT.auth-http` / `API.routes` own authentication, listener/routes, endpoint permissions, response/stream bodies and post-commit token behavior. Existing `nd-model` stays untouched. Scanner phase-1 readiness is not full #05 or #17 completion.

**V/E/I/U.** V = source/test-body inspected by this planner; E = executed evidence with attribution; I = inferred runtime result; U = unmeasured. Existing test names below mean tests exist, not that this planner ran them. Parent fresh gates at baseline were inspected in `/tmp/navidrome-plan-gates.5F9XLv`: fmt/build/clippy/nd-db/workspace/doc/diff-check exits 0; nd-db 737/0/0 across 26 result groups, workspace 1816/0/1 across 69; doc has 22 actual warnings (8 unresolved +14 private links). This is regression health, not runtime completion.

**Proposal notation.** Every NEW signature/type below is a proposal requiring the named decision gate, not an already available API. Existing public APIs are labeled existing. Prefer additive transaction-bound write functions `*_in_tx(&rusqlite::Transaction<'_>, ...)` or a small transaction-bound facade over hiding a transaction in a conn-only repo. Existing conn-bound read repos remain valid. Multi-statement mutations may not silently run in autocommit. Do not recursively check out SQLStore from a transaction closure. No raw SQL fragments/column identifiers taken from HTTP users; typed filters, column enums and registered table/kind constants only.

### G: validation and fresh review required for EACH package

From `rust/`, execute its focused command(s), then these gates (parent owns the integrated runs):

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p nd-db --all-targets --all-features --no-fail-fast
cargo test --locked --workspace --all-targets --all-features --no-fail-fast
cargo doc --locked -p nd-db --all-features --no-deps
```

From repository root: `git diff --check`, `git status --short`; record baseline and resulting HEAD. No autoformat/autofix. Schema/open work also runs `cargo test --locked -p nd-dbschema --all-targets --all-features --no-fail-fast`. External auth acceptance may require `cargo test --locked -p nd-auth --all-targets --all-features --no-fail-fast`, owned by BOOT/API after DB writer stops. Environment: installed pinned Rust toolchain and writable temp directory, file-backed SQLite fixtures; Go 1.27.x with pinned module dependencies and `sqlite_fts5` tag only when regenerating the proposed Go oracle. Do not download/change dependencies or silently substitute system SQLite.

After writing stops, a fresh read-only reviewer checks exact package scope, real SQLite effects, auth/library boundaries, rollback/retry, first-error order and the old-bug control. Apply only parent-selected fixes; parent reruns integrated G. Definition of done includes durable evidence and a package report: verdict; actual files changed; exact commands/exits/results; old-bug control status; open deviations/risks; blockers; next eligible IDs. No commit without explicit current approval; any authorized commit GPG signed, raw signature verified, no AI attribution.

### O: durable oracle contract

Pinned Go: `go.mod:3,9,21,40,47` (Go directive 1.27, squirrel1.5.4, structs1.1.0, go-sqlite3 1.14.52, dbx1.12.0). Rust rusqlite0.32.1/libsqlite3-sys0.30.1 are pinned. Report actual runtime Go/SQLite/Rust versions, compile options relevant to an assertion, source commit and output hash rather than assuming version equality. Previously reported bundled SQLite versions are not rerun evidence here.

**Proposed new durable oracle ownership:** DB-01 creates `rust/crates/nd-db/tests/testdata/go_db_runtime/{main.go,README.md,fixtures.json,expected.json,manifest.json}`. Later packages extend only the relevant fixture family in that SAME directory, serially. It is a new Go harness, not edits to existing Go persistence/tests; if a later assignment forbids ALL new Go source too, stop for approval. Use exported `persistence.New(*sql.DB)` (`persistence/persistence.go:72-74`) and public repo interfaces on an explicitly temporary DB, never the live singleton/path. If initializing through exported `db.Db`/`db.Init` is necessary, configure the temp path before first singleton access and verify `PRAGMA database_list`; read actual exported signatures before coding. `db.Init` (`db/db.go:81-122`) migrates a temp DB. Unexported helpers require evidence through public methods or an independently implemented narrow SQL expression labeled replicated; no in-package Go test injection without approval.

Proposed regeneration command from repo root:

```sh
go run -tags 'netgo sqlite_fts5' ./rust/crates/nd-db/tests/testdata/go_db_runtime \
  --fixtures rust/crates/nd-db/tests/testdata/go_db_runtime/fixtures.json \
  --out /tmp/navidrome-db-oracle.json
```

Flags are a proposed harness interface. Require exit0, expected case IDs/count, actual temp filename, source/version metadata, output SHA256 and successful independent comparison before replacing tracked expected data. Oracle temp DBs contain synthetic data only. Do not persist secrets/binaries/live DBs. Normalize only declared nondeterminism: Go map iteration as sets/multisets, dynamic `time.Now` via verified parse/storage-class/relative-order assertions, not fabricated constant bytes. Preserve IDs, row identities, ordered binds and deterministic timestamps exactly. Compare Rust storage using its approved chrono TEXT convention, not Go wire spelling. Existing JSON/feeder harnesses have replicated-unexported-method limitations and do not certify SQL execution. `/tmp` trace citations alone are not a durable oracle.

## DB-00 — Typed operation error and transaction seam
### D-ERROR DECISION (2026-11-18, parent decision: option B, narrow seam)

**Chosen:** option B — one `DbRuntimeError` for the new persistence seam, with explicit
`Domain(E)` / `Sqlite { stage, source }` / `BusyExhausted { attempts, stage, source }`
variants that keep the typed `rusqlite::Error` (and its extended code) reachable through
`Error::source()`. No new dependencies; `thiserror` is already used crate-wide. The existing
`with_tx`/`with_tx_retry`/`with_connection_sync`/`with_tx_sync`/`with_tx_retry_sync`/async
signatures stay untouched — new `try_with_tx`/`try_with_tx_retry` live beside them, so no
consumer is force-migrated and no repo error type is flattened into `StoreError::Other`'s
String. Option A (generic `OperationError<E>` over every existing callback) is rejected as
scope creep for this package; if it is ever wanted it can be layered on B later.

Verified Go ground truth for the retry contract (`persistence/persistence.go`):
- `txRetryDelay = 5s` (`:203`), `txMaxRetries = 3` (`:205`).
- `WithTxRetry` (`:207-228`) joins an existing tx when the store is already inside one
  (`:209-211`), reruns **only** when `db.IsBusy(err)` (`:218`), and returns any non-busy
  error immediately — so a domain error is never retried.
- Backoff is `(attempt + 1) * txRetryDelay` = 5s, 10s, 15s (`:225`), and the wait is
  cancellable via `ctx.Done()` (`:222-224`). The Rust port has no ambient context; the
  injected `Delay` covers the schedule, and cancellation is out of scope for this seam.
- `withBusyRetry` (`:232-236`) only reclassifies logging; no behavioral effect. Nothing to
  port.

Non-vacuity requirement carried into the package: a busy test must force a **real**
SQLITE_BUSY from a second connection holding a write lock, never a hand-built
`rusqlite::Error`; and the rollback test must write inside the transaction before returning
a domain error, so an implementation that commits `Ok(Err(…))`-style results fails.


1. **Status: DONE — DECIDED (parent, option B) and LANDED at `48e65806` + merge `ec84c176`.** Scope is the additive seam only (`try_with_tx`/`try_with_tx_retry`), not the DB lane; the retryable set is BUSY-only (`../decisions.md` H2, amended by review). The `D-ERROR DECISION` block below is the settled contract (`../decisions.md` E1-E3). Goal: freeze and implement a no-string-loss policy for domain failures crossing SQLStore callbacks. Prerequisites: parent decision `D-ERROR`. Non-goals: wholesale repository error migration, scheduler, mapping, new dependencies.
2. **Starting evidence (V):** existing `with_tx<T>(..., FnOnce(&Transaction)->Result<T,rusqlite::Error>)` and retry variant are `B/store.rs:83-138`; SQLStore sync/async counterparts `:196-264`. Entity repos return domain wrappers, e.g. `AlbumError`, `MediaFileError`; marshal errors cannot simply use `?` into those callbacks. `StoreError::BusyRetriesExhausted` retains a String cause (`:35-41`), not the actual final error. Existing tests `runtime_execution::store_transaction_commits_and_rolls_back_without_executor_blocking`, `store_transaction::with_tx_retry_runs_block_once_on_not_busy_failure`, `busy_retry_mapping::busy_lock_exhausts_tx_retry_at_tx_max_retries` cover the current SQLite-only seam.
3. **Ownership:** `rust/crates/nd-db/src/{store.rs,lib.rs}`, NEW `src/runtime_error.rs`, NEW `tests/runtime_typed_transactions.rs`; no repo docs. Shared collisions: store/lib with DB-03/11/14/18/20, error module with all new consumers. Keep existing public callbacks compatible unless parent explicitly approves migration.
4. **Contract proposal / gate:** choose either (A) generic typed `try_with_tx[_retry]` returning `OperationError<E>` with original E plus original SQLite begin/commit/retry causes and a trait for extracting a retryable SQLite cause, or (B) one `DbRuntimeError` enum for the new persistence seam with explicit SQLite/Marshal/NotFound/PermissionDenied/InvalidInput variants. **Recommend B at the narrow new seam**, additive typed store verbs, while existing callbacks remain; DB errors keep `rusqlite::Error` as typed source and retry classifier walks that variant only, never Display text. Gate decides final enum, Send/'static async bounds, warnings/outcome, retry-exhaustion typed cause and begin mode. No nested `Ok(Err(domain_error))` in transactions: it commits accidentally. No conversion to `Other(error.to_string())` or opaque boxed wrapper that hides busy classification.
5. **Steps:** record D-ERROR; add error conversions preserving SQLite extended codes; add typed transaction executor with explicit rollback; add retry using existing schedule/max policy; add async spawn_blocking entry; demonstrate one domain repo call through it without porting all repos. Inputs/IDs/timestamps are immutable or preplanned across retries; external side effects only after successful commit. Retain current drop-future-runs-to-completion behavior and document caller cancellation obligations.
6. **Acceptance / proposed tests:** `domain_error_rolls_back_and_retains_variant` (insert then Marshal/InvalidInput failure); `nested_sqlite_busy_retries_without_string_conversion` (typed SQLite busy, attempts and original code); `nonbusy_domain_error_runs_once`; `begin_commit_and_retry_exhaustion_keep_typed_causes`; `async_dropped_waiter_does_not_publish_commit_or_leak_lease`. Zero writes and pool-size1 behavior covered. Injected failures via private/test harness, no public production failpoint controls. Oracle O + Go `persistence/persistence.go:165-228` for retry/transaction rules; domain typing itself is Rust design.
7. **Non-vacuity:** make a closure return a domain failure after writing and prove nothing committed; a test that uses `Ok(Err(...))` would incorrectly commit and must fail the chosen API contract. No such mutation was executed during planning.
8. **Oracle:** O; keep current busy tests; use actual typed SQLite codes, not synthetic strings. No claim exact Go driver error text parity.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_typed_transactions --test runtime_execution --test store_transaction --test busy_retry_mapping`; G.
10. **Review/DoD:** G review plus independently inspect error source chain, drop/commit paths and retry idempotence; final decision/signatures recorded to parent. Unresolved D-ERROR blocks all NEW compound write APIs.

## DB-01 — Missing-state bind repair and executed regression gate (G0)

**Parent reproduction (2026-11-18, /tmp/db01 probe, migrated schema, real call):**
`mediafile_repo::mark_missing_sql` renders `UPDATE media_file SET missing = ?, updated_at = ?
WHERE id IN (?)` but binds `[Text(id), Bool(missing), Integer(updated_at)]`, so the IDs
occupy the SET placeholders:

```
args               : [Text("mf-target"), Bool(true), Integer(1234)]
rows affected      : 0        -> the call is a silent no-op; nothing is marked
same SQL, Go order : rows affected 1, that row becomes missing=1 updated_at=1234
```

`folder_repo::mark_missing_sql` (`folder_repo.rs:896`) and
`mediafile_repo::mark_missing_by_folder_sql` (`:1613`) already bind in SQL order, which is
why the sibling confidence check matters. The existing lib test passes on the wrong order
because it only asserts SQL text and `args.last() == Integer(99)`; it must assert the whole
positional vector. Correct Go citations: `mediafile_repository.go` MarkMissing **:455-469**,
MarkMissingByFolder **:472-487** (the file's `:433-455`/`:457-483` are wrong); note Go's
MarkMissing aborts on `err != nil || c == 0` (`:463-466`) while MarkMissingByFolder only
checks `err` (`:482`) — a documented difference, not implemented here.


1. **Status: DONE — LANDED at `f57ae050`,** so its executed oracle evidence exists and the sibling-renderer question is settled (below). No package prerequisites. Goal: repair `mark_missing_sql` and prove real Rust-generated statements target only intended rows. Non-goals: timestamps/signature conversion, scanner, production missing-file operations.
2. **V:** `B/mediafile_repo.rs:1587-1605` renders SET/WHERE but binds IDs before SET values; sibling folder `B/folder_repo.rs:896-910` and media mark-by-folder `:1613-1642` differ. Existing lib test `mark_missing_chunks_at_200_and_binds_the_flip` (`mediafile_repo.rs:2377-2398`) explicitly pins timestamp LAST. Go `persistence/mediafile_repository.go:456-483` sets missing/time before Where. **E (parent, not this planner):** a SQL-only reproduction using source-derived SQL/args changed decoy `id=99` instead of `mf-1`; no Rust function execution yet. That is NOT an executed regression test of `mark_missing_sql`.
3. **Ownership:** `src/mediafile_repo.rs` narrow renderer/lib test; NEW `tests/runtime_missing_state.rs`; oracle O directory. Read-only sibling renderers until a failing executed test justifies a concrete scoped correction; parent approval before unrelated fixes. Collisions: media file with DB-02/07/10/11/16; oracle shared.
4. **Contract:** keep existing signature `(missing:bool, ids:&[String], updated_at:i64)->Vec<Sql>` until DB-02 deliberately converts stamps. Every statement bind order `[missing,stamp,chunk IDs...]`; chunks200; empty IDs => no SQL. ID values bound, not interpolated. Existing no-permission builder is not an endpoint; synthetic DB only. Do not introduce new autocommit production mutations here.
5. **Steps:** create failing real-schema test invoking Rust function, not hand-copied SQL; fix args order; correct lib assertion; execute folder and media mark-by-folder for adjacent confidence; add Go oracle fixture/public MarkMissing trace.
6. **Matrix -> tests:** `single_id_marks_target_not_numeric_decoy`; `many_ids_keep_set_before_where`; `missing_flips_both_directions_without_touching_other_library`; `missing_chunks_zero_200_201`; `mark_by_folder_respects_current_flip_and_folder_scope`; `missing_updates_rollback_on_second_chunk_error`. Rows `mf-1`, `mf-2`, `99`, `1`, unrelated library row; read actual flags/time/type. Miss IDs are no-op not NotFound; include NULL/custom malformed row only where legal and show hard failure not false success. Existing i64 stamps are expected until DB-02.
7. **Non-vacuity:** old-order SQL control over identical synthetic rows must show wrong decoy effect, and a deliberate local restoration of old function order must fail the new function-invoking regression. Record control as planned until actually executed. Never mutate user data.
8. **Oracle:** O case `missing-bind-order`, Go public MediaFile.MarkMissing; preserve argument positions and returned row identities. Parent SQL probe is motivation, not durable acceptance evidence.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_missing_state`; `cargo test --locked -p nd-db --lib mark_missing`; G.
10. **Review/DoD:** fresh reviewer checks function actually invoked, unrelated IDs unchanged, chunk boundary and old-control are non-vacuous. G0 blocks DB-07/14 activation.

## DB-02 — Unified scanner timestamps and idle lifecycle (G1)
### D-TIME DECISION (2026-11-18, parent decision: faithful Go parity, TEXT canonical)

Chosen, with a Go oracle measured just now (`/tmp/tz`, go-sqlite3 v1.14.52, real SQLite):

- Go binds `time.Time{}` as the TEXT `"0001-01-01 00:00:00+00:00"`. Go's own scan of a
  `datetime` column re-formats values as `"0001-01-01T00:00:00Z"`, so a Go-side read-back is
  NOT the stored bytes — tests must compare against the Rust storage shape
  (`time::value`, rusqlite `%F %T%.f%:z`), not against Go's decoded string.
- The DDL sentinel is `default '0000-00-00 00:00:00'` (Rust `schema_ddl.rs:52`; Go
  `db/migrations/20241026183640_support_new_scanner.go:36`). It is **not** the model zero, so
  Go's `ScanInProgress` = `NotEq{"last_scan_started_at": time.Time{}}` (`db/../persistence/library_repository.go:190-194`)
  counts a freshly created library as **scan in progress** — measured: count = 1 for a row
  holding only the DDL default. That is inherited Go behavior, not our bug.
- Decision: **store every non-NULL scanner stamp as TEXT via `crate::time::value`; the idle
  predicate stays `<> canonical zero` so the DDL sentinel misclassification is preserved
  verbatim and pinned by a test.** Do NOT "normalize" the sentinel to idle: that would be a
  deliberate divergence needing its own approval, and it changes scan-boot behavior.
- `ZERO_TIME: i64 = 0` (`library_repo.rs:125`) and every `Value::Integer(stamp)` in the
  scanner paths are removed. **Affinity, stated precisely (measured 2026-11-18, `/tmp/db02b`):**
  a column declared `datetime` has **NUMERIC** affinity under SQLite's rules (the type name
  contains none of INT/CHAR/CLOB/TEXT/BLOB/REAL), so an integer bind is stored as INTEGER —
  `typeof(folder.updated_at)` after binding `1725000000` into a `datetime` column is `integer`.
  Comparing an INTEGER cell against a TEXT value is always false (numbers sort before all text),
  and `ORDER BY` puts integer rows first in ASC / last in DESC.
  Executed proof on the migrated schema, identical fixtures, one variable changed:
  ```text
  folder TEXT stamp    -> ["f1"]     # Go's GetTouchedWithPlaylists predicate matches
  folder INTEGER stamp -> []         # same row, epoch bound instead: silently dropped
  direct compare result (0=false): 0
  typeof folder.updated_at = integer (column declared datetime; NUMERIC affinity keeps INTEGER)
  ```
  plus album/artist equivalents (`/tmp/db02`): `album.imported_at > library.last_scan_at` returns
  `["a1"]` for the TEXT row and never the INTEGER row even though it is chronologically later, and
  `ORDER BY imported_at DESC` lists `a1` (2024-09-01) before `a2` (2024-08-30 epoch). Artist
  `updated_at > (SELECT last_scan_at …)` returns `["ar1"]` as TEXT and `[]` as INTEGER.
  That is the real defect D-TIME fixes; the idle predicate is only its visible symptom.

Inventory the package must convert — **every cited line verified to exist and to bind an integer
stamp** (`awk NR==<line>` over each file), plus a crate-wide grep for `Value::Integer` on a
timestamp column. The grep found **two lines the original inventory missed**:
`album_repo.rs:1149` (`touch_sql`, binds `Value::Integer(imported_at)` into `album.imported_at`,
which Go stamps with `time.Now()` at `persistence/album_repository.go:365`) and
`album_repo.rs:1170` (`touch_by_missing_folder_sql`, same column, Go `:377`). Those two are the
exact columns in the executed comparison above, so they are not optional.
Full list: `library_repo.rs:531,628/636,658/664,674/681/683,725,741,839,1398`
(`LibraryUpdate.updated_at`, `store_music_folder_sql`, `scan_begin_sql`, `scan_end_sql`,
`scan_in_progress*`, `refresh_stats_update_sql`, ZERO_TIME uses), `mediafile_repo.rs:1597/1607,1644`
(mark-missing stamps), `folder_repo.rs:896/900`, `album_repo.rs:1149,1170`,
`playqueue_repo.rs:136-137`, `artwork_repo.rs:77/85,166/182`, `plugin_repo.rs:123,183-184`
(unreachable module — see the plugin scope ask, but convert it anyway for consistency or delete it).
**Not in scope, by design:** `scrobble_repo.rs:100` binds `submission_time` as an epoch integer,
which is correct — Go stores `submissionTime.Unix()` (`persistence/scrobble_repository.go:52`) and
the column is an integer; converting it would be a divergence. `sql_annotations.rs:271/291/342`
bind `rating`/`1` as integers alongside TEXT `rated_at`/`played_at` — already correct.

Sequencing: DB-02 runs **after** DB-03 (this lane) completes, because both gates run the whole
workspace in one checkout and a second concurrent writer would corrupt the other's numbers.
Non-vacuity controls stay as planned (bind INTEGER deliberately and show the wrong cutoff/idle
result on identical fixtures).


1. **Status at plan time: DECISION REQUIRED; DECIDED (`../decisions.md` T1-T4) and LANDED `6d9d984d`/`74ebd81c`.** Prerequisite: parent `D-TIME` (canonical idle + DDL-sentinel interpretation); integrate after DB-01 (landed `f57ae050`) because same renderer file. Goal: one Rust storage/comparison contract across scanner paths, not Go migration support. Non-goals: plugin/playqueue timestamp cleanup, wire JSON-error parity, schema seeding.
2. **V:** `B/time.rs:1-52,76-84` defines chrono TEXT; record maps write it. `library_repo.rs:118-120,480,592,622-690,763-809` still bind epoch seconds (including ZERO_TIME0); `album_repo.rs:1144-1170` touch INTEGER; `mediafile_repo.rs:1587,1613,1716-1805` missing stamps/recent bounds INTEGER; `folder_repo.rs:896` likewise. Cross-table comparisons `album_repo.rs:1315-1335`, `mediafile_repo.rs:1691-1707`, folder touched playlists and artist touched stats. Hydration `hydrate.rs:1533-1549,3608-3638` reads INTEGER0 as 1970 not year1. Existing `library_scan_in_progress_follows_the_stamps_this_port_writes` pins TEXT=>started/INTEGER0=>idle, not lifecycle. Go `library_repository.go:162-193`, scanner `scanner.go:209-215` use zero time.
3. **Ownership:** `src/{time.rs,library_repo.rs,album_repo.rs,mediafile_repo.rs,folder_repo.rs}` only scanner-facing timestamp signatures/binds; `tests/runtime_timestamp_convention.rs`, `tests/runtime_hydrate_library_property_tag.rs`, DB-01 `runtime_missing_state.rs`; NEW `tests/runtime_scan_clock.rs`; oracle O. `hydrate.rs` read-only unless D-TIME needs a specific model-zero fix; ownership expansion requires parent. Collisions: most DB write packages.
4. **Proposal/gate:** use `DateTime<Utc>`/borrowed chrono input for all non-NULL scanner stamps/bounds; `time::value` only. Non-pointer idle remains `hydrate::go_zero_time()` rendered canonically, not INTEGER0/Unix epoch/NULL. **Recommend** typed library inserts initialize both scan fields explicitly; `scan_in_progress` recognizes canonical zero AND existing fresh-DDL zero-date sentinel as idle (or BOOT normalizes defaults before use). Parent must choose exact predicate/normalization policy: ordinary `<> canonicalZero` would misclassify DDL sentinel. Genuine malformed timestamp tolerance in row mapping does not license silently treating arbitrary SQL text as idle. No blanket manual parser rewrite.
5. **Steps:** approve D-TIME; enumerate every comparison writer/bound in these files with inventory assertion; convert input DTO fields including LibraryUpdate/RefreshStatsTotals; fix idle count/reset together; update callers/tests deliberately; exercise lifecycle via statements now, production methods added DB-05/07; retain stable injection clock. Do not stamp folder commit-time instead of caller scan UpdateAt (DB-07).
6. **Tests:** `fresh_defaults_and_typed_insert_are_idle`; `begin_end_reload_second_begin_agree_with_model_zero`; `interrupted_scan_retains_start_and_mode`; `text_storage_and_fractional_chronology`; `touch_and_missing_updates_select_only_newer_entities`; `recent_match_bounds_reject_old_equal_allow_new`; `scan_counts_and_oldest_library_artist_cutoff_agree`; `null_required_fails_malformed_read_policy_explicit`. Boundary zero/one/multiple libs, exact/equal/fractional times, DDL sentinel, chrono year1, no explicit NULL in required fields, full/quick flag; predicate SQL and decoded values checked together. Rollback resets to prior stamp; replay same clock does not move boundaries unexpectedly.
7. **Non-vacuity:** retain controls storing/binding INTEGER to show old wrong cutoff/idle outcome on identical fixtures; old ScanEnd0 must fail epoch-vs-zero assertion. Do not conflate Go-migration rows with Rust-created mixed storage.
8. **Oracle:** O `scan-lifecycle`, `recent-time`; Go dynamic stamps validated structurally/relationally, Rust canonical storage exact. Any deliberate sentinel count difference is approved deviation, not relabeled Go parity.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_scan_clock --test runtime_timestamp_convention --test runtime_missing_state --test runtime_hydrate_library_property_tag`; G.
10. **Review/DoD:** reviewer traces all scanner timestamp writes AND bounds, DDL default, model zero and SQL idle. D-TIME recorded before code; no runtime scanner use until this gate passes.

## DB-03 — Every-handle effective connection/path contract (G2)

1. **Status: DONE — DECIDED (user, option a) and LANDED at `4a8aac9f`,** review findings closed (`../decisions.md` P1). Prerequisite `BOOT.db-open-policy` + parent `D-OPEN`; independent of DB-00 implementation. Goal: apply and verify explicit settings on every pool handle. Non-goals: deciding configured music library existence, implicit seeds, changing bundled SQLite/dependencies.
2. **V:** `nd-dbschema/src/schema.rs:57-88` forwards effective path and registers extensions; migration toggles FK only on its supplied conn `:126-147`; `B/pool.rs:120-135` opens all via schema::open. **Contrary evidence:** libsqlite3-sys0.30.1/build.rs123 defaults FK ON, rusqlite inner_connection.rs119 installs busy_timeout5000. Go default DSN `consts/consts.go:14` specifies15000/WAL/FK-on/synchronous-normal. Thus FK-off/no-timeout claims are false; actual effective state is U. Existing player test opens two handles but migrates/reuses one (`runtime_player_repo.rs:39-44`); pool `pool_round_trip_hands_out_and_takes_back_every_lease` does not assert pragmas/path.
3. **Ownership:** `rust/crates/nd-dbschema/src/schema.rs`, `rust/crates/nd-db/src/pool.rs`, possibly additive SQLStore::open config propagation `store.rs`; NEW `nd-db/tests/runtime_pool_configuration.rs`; appropriate existing `nd-dbschema/tests/` open test (identify exact existing path before edit or use NEW `tests/open_configuration.rs`). Shared collisions DB-00/store, BOOT/schema, DB-11/pool/resource tests. No manifests/lockfiles.
4. **Proposal/gate:** explicit typed open options, e.g. `DbOpenOptions { path, busy_timeout, journal_mode, synchronous, foreign_keys }`, validated before pool creation; final type/defaults chosen BOOT. Recommend FK required-on, WAL for file-backed concurrent service, explicit timeout default (Go15000 recommended unless parent approves5000) and normal synchronous; memory DB may return memory journal mode with documented exception. Decide translate accepted Go DSN keys vs reject clearly; never forward driver-only flags and assume they apply. Verify effective filename for ordinary path versus `file:` URI, no accidental literal query-suffix database. Reject unsupported/contradictory options, not arbitrary SQL PRAGMA strings.
5. **Steps:** freeze D-OPEN with BOOT; parse/validate minimal supported input; configure each newly opened handle and verify returned effective state before pooling; ensure migration/open sequencing doesn't lose FK or settings; preserve extension registration; add all-handles tests. No mutation on live DB, no changing caller library policy.
6. **Tests:** `all_handles_enforce_config_and_expected_file` (hold all leases concurrently, distinct IDs/handles, query foreign_keys/journal_mode/busy_timeout/synchronous/database_list); `each_handle_rejects_invalid_fk_and_cascades`; `uri_and_plain_path_contract_no_shadow_filename`; `reopen_preserves_file_settings_and_reapplies_local_settings`; `memory_mode_supported_or_rejected_explicitly`; `contention_uses_configured_timeout_and_retry`. Zero/invalid pool size follows existing clamp; malformed/unknown DSN option rejected; setup failure drops all opened handles; journal-mode failure surfaces typed cause. No timing assertion alone replaces pragma reads.
7. **Non-vacuity:** migrating only one handle and releasing it to LIFO must not satisfy the test: hold every lease before any check. An ignored custom timeout should fail exact read-back against default5000. FK control must not assert off merely by omitting project PRAGMA (bundled default is on).
8. **Oracle:** O + pinned dependency source and SQLite PRAGMA semantics; compare intended approved effective values, not raw DSN bytes. Existing Go fixture records configured file/settings separately. Record actual SQLite compile options.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_pool_configuration --test pool_round_trip --test runtime_player_repo`; nd-dbschema suite plus G. Requires file-backed temp FS; if WAL unavailable on target FS stop/report, don't silently downgrade.
10. **Review/DoD:** DB+BOOT fresh reviewer confirms open-path policy ownership, all handles tested and FK/busy claims precise. G2 gates compound runtime acceptance.

## DB-04 — Executed property mutations

1. **Status: READY — the next assignable package.** Prereqs DB-00 (`48e65806`/`ec84c176`), DB-02 (`6d9d984d`/`74ebd81c`,
   canonical TEXT stamps) and DB-03 (`4a8aac9f`) are all landed; the checkout has no concurrent writer. Goal: small typed
   property put/delete on a transaction; no scanner state/controller policy.
2. **V:** existing `PropertyRepo::get/default_get` `B/property_repo.rs:162,188`, builders put60/delete87; `T/runtime_hydrate_library_property_tag.rs:626-661` manually executes writes. Existing tests `property_put_update_then_insert_pair_runs_in_go_order`, `property_delete_removes_the_row_and_get_then_reports_not_found` are test helpers, not runtime API. Go `persistence/property_repository.go:15-45` source to re-read on assignment.
3. **Own:** `src/property_repo.rs`, NEW `tests/runtime_property_writes.rs`, O property cases. Collisions DB-00 typed errors, DB-14 orchestration; no docs.
4. **Proposal:** `put_in_tx(tx,key:&str,value:&str)->Result<(),DbRuntimeError>`, `delete_in_tx(tx,key)->Result<(),...>`; bound keys/values; update first then insert iff affected0. Empty string value is stored value, missing read is NotFound; DefaultGet folds only NotFound. Delete absent key no-op per Go, not fabricated permission. Typed key constants in SC may wrap strings; no SQL fragments.
5. **Steps:** implement one-statement execution then update/insert branching on same tx; wrappers do not recheckout or commit; parent/SC puts scan metadata only at correct orchestration points.
6. **Tests:** `property_put_insert_update_empty_and_adversarial_key`; `delete_missing_is_noop_existing_is_not_found_after_commit`; `property_bundle_failure_rolls_back`; `retry_put_is_idempotent`. Zero/one/many independent keys, NULL not representable, malformed schema hard error, no default returned beside a hidden error. Public read-back via PropertyRepo.
7. **Non-vacuity:** test invokes new method, not duplicate test helper; forced failure after first update must preserve original value.
8. **Oracle:** O public Property methods; deterministic strings/row count; no byte-order nondeterminism.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_property_writes --test runtime_hydrate_library_property_tag`; G.
10. **Review/DoD:** G; callers can persist flags without raw SQL, errors remain typed, no launch of scanner yet.

## DB-05 — Typed library insert/update and scan lifecycle execution

1. **Status: BLOCKED.** Prereqs DB-00/02/03. Goal: supply BOOT/SC executed library mutations; BOOT alone decides which configured libraries to create or update. No schema seed, library deletion/service cache/plugin migration.
2. **V:** LibraryRepo reads934-1115; `LibraryUpdate`/`insert_sql(id:Option<i64>,values:Vec<Value>)` at `B/library_repo.rs:465-577` still accepts arbitrary ordered values; scan/link builders580-655, stats later. Go Put `persistence/library_repository.go:72-135`: ID0 autoincrement, nonzero update/fallback insert, ID1 path-change validation, stamp times, assign all admins. Existing `library_get_returns_the_full_record_from_an_integer_id_bind`, `library_get_all_plain_returns_every_row_and_binds_nothing` cover reads only.
3. **Own:** `src/library_repo.rs`, NEW `tests/runtime_library_writes.rs`, O library cases. Collision DB-02 signatures, DB-17 stats, BOOTcaller; schema_ddl forbidden.
4. **Proposal:** typed `NewLibrary`/`LibraryPatch` or validated `LibraryRecord` payload with registered update-column enum; `put_in_tx(...)->LibraryWriteResult{id,inserted}`, `scan_begin_in_tx(id,now,full)`, `scan_end_in_tx(id,now)`, `set_scanned_pid_in_tx(id,albumSpec,trackSpec)`, `add_artist_in_tx(libraryId,artistId)`; preserve Go update-miss semantics (do not universally turn rowcount0 into NotFound). Explicit insert all required stamps/zero fields; immutable created_at on update; updated_at always stamp; validate ID1 path against current row if it exists. Music-folder override, if required BOOT, a separate privileged operation rather than bypassing generic validation invisibly. Admin assignment happens inside same tx. No arbitrary caller `Vec<Value>` at public write boundary.
5. **Steps:** typed row serializer matched INSERT_COLUMNS; insert0/use last_insert_rowid same connection; update-first/explicit-ID fallback; ID1 guard; admin assignment; scan state writes/link; BOOT consumes result after commit, not before. No persistent cache yet, so no post-rollback cached paths.
6. **Tests:** `library_auto_and_explicit_id_insert_match_all_columns`; `library_partial_update_preserves_created_and_protected_path`; `library_admin_assignment_is_atomic_idempotent`; `scan_lifecycle_methods_agree_with_idle_and_pid`; `library_assign_or_insert_failure_rolls_back`. Empty paths remain valid DB values unless BOOT policy rejects; duplicate name/path unique error typed; missing artist/library link FK error; update column casing normalized by typed enum. Zero/one/many admins/libraries, absent target stamp no-op if Go, counters remain owned by stats.
7. **Non-vacuity:** no test helper manually constructing insert list; distinguish every field so rotated positional values fail; fail admin assignment and prove no library row/ID leaked as committed result; fresh second scan catches old INTEGER0.
8. **Oracle:** O Go Put/ScanBegin/End/AddArtist. Normalize only dynamic times; exact IDs, path validation, membership junctions.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_library_writes --test runtime_scan_clock --test runtime_hydrate_library_property_tag`; G.
10. **Review/DoD:** G reviewer verifies BOOT caller policy not stolen by DB, no schema seed, path1 behavior and transaction identity.

## DB-06 — Executed tag dictionary/library-link Add

1. **Status: BLOCKED.** Prereqs DB-00/03/05 (production library test seed). Goal: execute tag Add only; counts/purge assigned DB-17/20.
2. **V:** `B/tag_repo.rs:106-141` produces dictionary + library_tag pair per200; existing TagRepo594-662 only reads. Go `persistence/tag_repository.go:23-45`; current lib insert/chunk tests and `tag_get_all_scopes_rows_to_accessible_libraries_with_an_inner_join` are precedents.
3. **Own:** `src/tag_repo.rs`, NEW `tests/runtime_tag_writes.rs`, O tag-add. Collision DB-17/20, shared runtime errors. No genre fix here.
4. **Proposal:** `add_in_tx(tx,libraryId,tags:&[Tag])->Result<(),DbRuntimeError>`; derive/check tag IDs at SC->record boundary per approved ID rule, don't invent fourth role/ID scheme. Ordered binds dictionary(id,name,value) then link(tagId,libraryId); dictionary before FK link; chunk200; empty list no SQL; ON CONFLICT DO NOTHING preserves existing dictionary and counts.
5. **Steps:** execute each pair within caller tx; no per-chunk commit; read back using scoped TagRepo; reject cross-library batch metadata if present rather than silently rewrite IDs.
6. **Tests:** `tag_add_zero_one_200_201_and_duplicates`; `tag_second_library_links_without_overwriting_counts`; `tag_missing_library_or_second_chunk_error_rolls_back_dictionary_and_links`; `tag_ids_unicode_and_empty_name_policy_matches_go`. NULL strings impossible; SQL metacharacters bound; inaccessible tag read remains excluded by INNER join. Retry replays produce same dictionary/link set.
7. **Non-vacuity:** failure after dictionary insert must leave neither dictionary nor dangling link after rollback; omission of library link must fail scoped read-back, not pass dictionary-only count.
8. **Oracle:** O public Tag.Add; deterministic ID sets/counters; entry ordering as set. Existing participant/JSON oracle not substitute.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_tag_writes --test runtime_hydrate_library_property_tag`; G.
10. **Review/DoD:** G; scoped read-back proves both legs and no counts reset.

## DB-07 — Folder Put plus executed missing/touch primitives

1. **Status: BLOCKED.** Prereqs DB-00/01/02/03/05. Goal: bounded folder/missing/touch DB primitives, not walk/finalize policy or purge.
2. **V:** folder row/put711/839, mark896; `FolderRepo` has no writes. Media missing builders1587/1613, album touch1144/1163. Existing `folder_record_insert_update_readback`, `folder_record_rotated_insert_is_detected`, g2 update-info/read tests. Go folder Put `persistence/folder_repository.go:255-274` writes caller UpdateAt; current Rust put_sql stamps its `now` (`:839-852`), so call with caller folder update stamp, not commit-clock. Folder base reads intentionally return zero UpdateAt (dbx field-name mismatch), while update-info reads stored updated_at.
3. **Own:** `src/{folder_repo.rs,mediafile_repo.rs,album_repo.rs}` only new executed folder/missing/touch functions; NEW `tests/runtime_folder_writes.rs`; extend DB-01 runtime_missing_state/DB-02 clock tests only necessary. Collision DB-08-11/16; no filesystem code.
4. **Proposal:** `put_folder_in_tx(tx,record,updateStamp)->WriteResult`; `mark_media_missing_in_tx`, `mark_folders_missing_in_tx`, `mark_media_missing_by_folder_in_tx`, `touch_albums_in_tx`, `touch_albums_with_missing_folders_in_tx`. All consume typed scan authority/library scope and IDs, chrono stamp; empty IDs no-op, chunks200. Keep created_at immutable. Scope validation must prevent accidentally updating IDs belonging to another selected library; final safe predicate or existence-check chosen with DB-11 types. Go TouchByMissingFolder is global—do NOT silently claim it is scoped; if narrowing it to selected libraries is desired parent approves at D-GC/D-SCOPE.
5. **Steps:** folder update-first/insert fallback; ID is supplied by SC (DB does not recompute path IDs); write stamp = supplied caller UpdateAt; execute tested missing/touch SQL in caller tx; no marking based on filesystem failures—SC must only call after successful coverage.
6. **Tests:** `folder_put_preserves_caller_stamp_created_hash_images`; `missing_and_touch_subset_do_not_escape_library_scope`; `missing_folder_touch_null_empty_and_nested_folder_ids`; `folder_updateinfo_reads_stamp_while_base_read_keeps_go_zero`; `missing_chunk_failure_rolls_back_all_primitives`. Cases0/1/200/201, duplicated/missing IDs, flags both ways, malformed folder_ids JSON hard error, sentinel images stamp allowed, non-target libraries unchanged. If exact Go global touch retained, test/label that explicit policy instead of promising isolation.
7. **Non-vacuity:** pass scan stamp earlier than commit clock and assert storage retains it; old bind order caught by DB-01; touch a selected ID and a decoy other-library ID to exercise scope.
8. **Oracle:** O folder/missing/touch public APIs; Go FolderID constants already durable in g2 tests. Compare update-info separate from base Folder.UpdateAt.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_folder_writes --test runtime_missing_state --test runtime_scan_clock --test runtime_hydrate_folder_artwork_queue --test runtime_marshal_records`; G.
10. **Review/DoD:** G reviewer checks zero-vs-storage read distinction, caller timestamp, no unvisited-content policy inside DB.

## DB-08 — Transaction-bound artist Put and partial-update contract

1. **Status: BLOCKED.** Prereqs DB-00/02/03/05. Goal: artist write lifecycle for scanner partial fields, not stats/index/search/cursor.
2. **V:** `put_cols`675 and `put_sql`758-807 render; ArtistRepo1588/1631 only reads. Go `persistence/artist_repository.go:239-253`, shared selectUpdateColumns `sql_base_repository.go:593-610`. Existing `put_sql_update_leg_filters_cols_and_appends_updated_at` pins a documented divergence: Rust appends updated_at to ANY filtered update while Go filters exactly requested cols. Scanner phase1 explicitly includes updated_at (`scanner/phase_1_folders.go:403-405`), so broader external-info change is not required blindly.
3. **Own:** `src/artist_repo.rs`, NEW `tests/runtime_artist_writes.rs`, O artist-put. Collision DB-18/20. Existing hydration code protected except deliberate tests for round-trip.
4. **Proposal:** `put_artist_in_tx(tx,record,now,ArtistColumns)->WriteResult`; enum/all-vs-explicit typed fields; generated ID only if Go permits and caller does not supply, otherwise stable provided ID; sorted columns/binds, created_at immutable on update, empty sort/MBZ do not overwrite stored nonempty. Freeze behavior for filtered timestamp stamping: recommend match Go (updated_at changes only if explicitly requested), add regression for scanner's explicit list; do not change all external-info callers without audit.
5. **Steps:** normalize column selection through shared snake helper or typed enum; execute update then insert on miss, full row on insert; empty update set contract same DB-00; library link belongs DB-05/14; same tx caller.
6. **Tests:** `artist_put_insert_update_partial_scanner_columns`; `artist_empty_sort_mbz_do_not_clear_existing`; `artist_partial_external_info_does_not_force_timestamp`; `artist_insert_after_predefined_id_miss`; `artist_put_failure_retry_preserves_created`. Zero input rejected if no meaningful row, NULL optional times, adversarial names, deterministic full_text normalization; no side effects outside tx. Read back via ArtistRepo after library link.
7. **Non-vacuity:** older stored updated_at + restricted description-only update distinguishes old forced stamp; not merely snapshot string. A missing library_artist link should make ArtistRepo get miss, so fixtures must create link and explicitly assert it.
8. **Oracle:** O actual public Artist.Put; use semantic FTS values and independently expected field set. Dynamic Go clocks normalized per O.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_artist_writes --test runtime_hydrate_artist --test runtime_timestamp_convention`; G.
10. **Review/DoD:** G; exact partial-column rule recorded and scanner FTS fields updated intentionally, not arbitrary raw column names.

## DB-09 — Album Put: base + participants/tags + FTS

1. **Status: BLOCKED.** Prereqs DB-00/02/03/05/06/08. Goal: execute existing record bundle atomically; no aggregate/equals algorithm (SC-owned), moves/copy later.
2. **V:** `B/album_repo.rs:764,1030-1100` defines complete record map and base/junction bundle; conn repo only get/get_all. Go `persistence/album_repository.go:208-219` stamps imported_at, generic put, then participants/tags. Existing `album_record_insert_update_readback`, `album_record_rg_some_and_null_folder_ids_roundtrip`, `runtime_insert_alignment` and `runtime_hydrate_album_mediafile` assert pieces, not production transaction lifecycle.
3. **Own:** `src/album_repo.rs`, NEW `tests/runtime_album_writes.rs`, O album-put. May fix only directly proven bundle defect; sql_participations/sql_tags ownership expansion requires parent if tests expose a new helper fault. Collision DB-07/11/15-20.
4. **Proposal:** `put_album_in_tx(tx,id,record,now,AlbumColumns)->WriteResult`; consume DB record supplied by SC. Stamp only imported_at at repository Put; created/updated from record; never update created_at/birth_time; exclude annotation/average_rating/released orphan/image fields. Execute update if ID exists else insert; then participant delete/optional insert then tags delete/optional insert. Empty maps must clear junctions, not skip deletes. FTS comes from record-derived inputs once, not caller independent column arrays. Insert ID must be returned/known before junctions. All same tx.
5. **Steps:** choose final ID with stable retry behavior; build bundle from immutable record; base write branch; maintenance ordered; read back full entity and FTS/junction rows; optional scoped partial columns use enum. No cross-repo artist lookups added to PostScan.
6. **Tests:** `album_production_put_roundtrip_all_columns_and_fts`; `album_role_tag_removal_clears_stale_links`; `album_partial_update_insert_miss_and_immutable_created`; `album_fault_each_maintenance_leg_rolls_back_base_fts_and_links`; `album_busy_replay_equals_single_put`. Empty discs/folderIDs null-vs-empty; RG None/Some; nonexistent participant artists filtered by JOIN like Go; indexed tag scope; multiple subroles; quoted/Unicode name sort FTS input; warning-free invalid JSON rejected before write. Exact insert update row identities checked.
7. **Non-vacuity:** rollback proof must observe FTS trigger rows, not just base count. Skip a maintenance delete in a local control and stale link assertion fails; old rotated insert remains covered.
8. **Oracle:** O public Album.Put plus S-A/S-B existing goldens for pure codecs only. Junction results as sets; FTS deterministic except declared sorted participant projection; sort-name inputs remain SC-provided.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_album_writes --test runtime_marshal_records --test runtime_insert_alignment --test runtime_hydrate_album_mediafile`; G.
10. **Review/DoD:** G; typed record/atomic write contract exists without DB implementing album aggregation.

## DB-10 — MediaFile PutByMatch identity and atomic maintenance

1. **Status: BLOCKED.** Prereqs DB-00/02/03/05/06/08/09 (production fixture graph). Goal: real match/update/insert/ID result plus base/junction/FTS transaction; no tag reader/mapping, scan move selection or full REST.
2. **V:** `put_match_sql(path,library_id)`765 only renders SELECT; `put_sql(id,record,now,append_subtitle,cols)`1427 returns bundle; Repo get/get_all/get_random606-713 only reads. Go `mediafile_repository.go:184-197`, `sql_base_repository.go:576-591,618-642`: if explicit ID skip match; else SELECT by path+library then put; no upsert API. Existing `mediafile_record_insert_update_readback`, insert alignment, hydration tests need a new production wrapper test.
3. **Own:** `src/mediafile_repo.rs`, NEW `tests/runtime_mediafile_writes.rs`, O media-put. Collision DB-01/02/07/11/16; no SC mapping types edited.
4. **Proposal:** `put_mediafile_in_tx(tx,identity:MediaIdentity,record,now,append_subtitle)->WriteResult{id,disposition}`; identity explicit ID or match(path,libraryId). Match selected with Go case/path semantics, ID0/empty decisions explicit; generated ID stable to one operation/retry plan and returned only postcommit by outer operation. Created_at stamp only when model zero (`None` AND explicit go_zero as appropriate); preserve birth_time/created_at on updates. Bundle order base -> participant delete/insert -> tag delete/insert. Keep full_title config supplied typed bool not global. Match permission is scanner/system caller; API must not invoke unscoped match from a raw path string.
5. **Steps:** read match on same tx, distinguish no rows vs hard error; generate/preallocate if needed; execute branch; maintenance; return result without mutating shared retry input. Audit no unique path constraint reliance; concurrency serialized by transaction/retry, no fabricated upsert conflict key. Re-read production entity after commit.
6. **Tests:** `put_by_match_reuses_path_library_identity`; `same_path_different_library_stays_distinct`; `explicit_id_bypasses_match_and_preserves_immutable_columns`; `new_id_and_predefined_miss_return_persisted_id`; `media_each_leg_failure_rolls_back_fts_and_junctions`; `retry_match_same_result_no_orphan_id`. Empty paths per Go accepted/rejected explicitly, absent match, malformed read failure, subtitle on/off, bitdepth/BPM/RG optional, zero created_at representations, duplicate role/subroles, chunks in related helpers where applicable. Ensure caller record and batch untouched after rollback.
7. **Non-vacuity:** two calls with no ID at same path/library must produce one row via actual new API; omission of match SELECT would duplicate and fail; explicit ID branch with colliding path tests it doesn't incorrectly overwrite unrelated ID.
8. **Oracle:** O public MediaFile.Put; timestamps normalized, persisted IDs/state/junction set exact. Random IDs canonicalize only new unknown ID bijection; match and stable supplied IDs compared literally.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_mediafile_writes --test runtime_marshal_records --test runtime_hydrate_album_mediafile`; G.
10. **Review/DoD:** G; no raw-column caller, stable same-tx identity result, created/birth immutability and match scoping proven.

## DB-11 — Typed scanner reads, cancellation/resource decision and API-safe point read

1. **Status: DECISION REQUIRED.** Prereqs DB-00/02/03/05/07/09/10 plus parent `D-CURSOR` and `D-SCOPE`; handshake `SC.query-consumption`, `API.visible-media-contract`. Goal: executed scanner query shapes and explicit resource contract; not full REST/artist index/search, artwork cursor everywhere, mapping bridge or worker scheduler.
2. **V:** Media get_all634 lacks QueryOptions filters, album get_all1508 likewise. Go scanner per-folder cursor `phase_1_folders.go:231`, per-album media `phase_3_refresh_albums.go:79`, missing/matching74/recent235/254 in phase2, touched albums50 in phase3, image changes80/93. Builders exist: media1691/1727/1777/1651/1514, album1315/1290; execution query_all collects Vec (`execution.rs:53-60`), folder CursorDrain1284-1340 is full drain only. Go cursor protocol `sql_base_repository.go:392-438`. Existing `mediafile_repo_get_random_phase_two_reapplies_the_library_filter`, g2 cursor prefix test and two-library tests are narrow precedents.
3. **Own:** NEW `src/scanner_queries.rs`, NEW `tests/runtime_scanner_queries.rs`, `src/lib.rs`; add typed wrappers/internal renderer changes only in `mediafile_repo.rs`, `album_repo.rs`, `folder_repo.rs`, `artist_repo.rs` as enumerated before assignment. Existing raw SQL functions remain low-level trusted builders, not API inputs. Collision major repo files plus SC/BOOT consumers; no nd-model/mapping changes.
4. **Proposal/gate:** typed methods for `media_in_folder`, `media_in_album`, `missing_and_matching(libraryId)`, `recent_candidates(Mbz/Properties,since)`, `touched_albums(libraryId)`, `album_ids_by_folders`, `sole_album_artist_ids_in_subtrees`, `media_by_paths` (phase4). Registered filter enums/typed IDs and chrono bounds; explicit stable PID order. **Recommend synchronous row visitor** `FnMut(OwnedRecord)->ControlFlow<()>` on a leased conn, per-row PostScan inside callback, then close before returning. SC may adapt to bounded channel in spawn_blocking; SC owns backpressure, DB visitor owns early-stop/cancel closure and cannot recheck out store. Approve bounded per-folder owned Vec only as an explicit phase1 alternative, not a complete cursor. General external callbacks must not retain Row/Statement/lease or perform nested DB writes. Decide first-error/prefix return and use typed `VisitOutcome{visited,stopped}` + Err; no false cursor parity.
5. **Permissions contract:** separate `ScannerScope` built by trusted SC/BOOT from selected configured library IDs from `UserReadScope` resolved inside DB per request from authenticated user ID/admin record and user_library. Proposed `get_visible_media(id,userId,coverFlag)` may reuse existing MediaFileRepo::get ONLY with DB-built, tested `media_file.library_id` membership clause and same user annotations/bookmark; never accept raw user `Sql` or client-provided is_admin/library IDs. Alias contracts for `f`, media_file, artist.library join explicitly tested. Static membership snapshot only for that request; API owns revalidation/auth and response. Empty allowed-library set => no rows, NOT unrestricted. Scanner system scope not passed into public routes.
6. **Steps/tests:** implement scoped typed rendering then execution/visitor; `folder_album_filters_apply_before_page`; `missing_pid_order_and_recent_boundaries`; `touched_albums_and_image_id_reads_scope_chunks`; `paths_case_unicode_library_resolution`; `visitor_stops_before_bad_later_row_and_releases_lease`; `visitor_decode_error_is_first_and_closes`; `bounded_channel_cancel_closes_without_deadlock` (SC integration seam); `visible_point_read_denies_other_library_for_all_aliases`; `empty_scope_denies_not_headless`; `membership_change_next_request_reloads`. Sizes0/1/100/101 path chunks,200/201 IDs, no user/admin/restricted, NULL/malformed core, equal time, early stop; no artwork lookup on Go-unhydrated scanner reads. Resource tests use pool1/2 deliberately and no full producer task leak.
7. **Non-vacuity:** preexisting unfiltered get_all must fail two-folder target-set assertion; omitted alias or empty-scope=>None must expose unauthorized row in a negative control; a bad later row proves true early stop rather than collect-all-then-truncate. Do not claim dropping an async future stops SQLite by itself.
8. **Oracle:** O real public Go scanner reads/cursors + source columns; ordering exact only PID/explicit sort, unordered rows canonicalized as sets. Go partial-row-beside-error differences documented if intentionally not represented. D-CURSOR approval recorded.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_scanner_queries --test runtime_hydrate_folder_artwork_queue --test runtime_hydrate_album_mediafile`; G. API/SC later run their own bounded-channel/auth integration acceptance.
10. **Review/DoD:** G plus fresh security/resource review; typed interfaces delivered to SC/API, exact projection and cancellation obligations documented. No claiming full cursor/REST completion.

## DB-12 — Scanner artwork invalidation and enqueue subset

1. **Status: BLOCKED.** Prereqs DB-00/02/03. Goal: executed DeleteForItems, Enqueue, EnqueueIfMissing, EnqueueAllMissing only. Non-goals: image worker/blob IO, source bulk tools, purge production actions.
2. **V:** `B/artwork_repo.rs:232-251` deletion builder; queue123/170/176/187/219 render; existing ArtworkQueueRepo only reads/counts/purge731. Go `persistence/artwork_repository.go:150-158`, `artwork_queue_repository.go:46-105,142-160`: reset lifecycle vs preserve/if-missing. Kind prefixes `al/mf/ar/pl/ra`, not names. Existing g2 source-count old-bug control does NOT execute enqueue.
3. **Own:** `src/{artwork_repo.rs,artwork_queue_repo.rs}`, NEW `tests/runtime_scanner_artwork_writes.rs`, O artwork cases. Shared with DB-14/19/20; no artwork_hydration rewrite.
4. **Proposal:** transaction-bound `invalidate_items(kind:ArtworkKind,ids)`, `enqueue(items,now,mode)`, `enqueue_all_missing(kind,priority,now)->i64`; typed kind->fixed entity table whitelist. Enqueue chunk100, deletion200; empty list no SQL. Reset sets priority=max, retry_at/enqueued_at=now, attempts0, trace[]; if-missing never resets existing queued row or item_artwork-resolved row. Default image type primary when empty per Go, or explicit typed constructor. Return typed errors; caller chooses nonfatal artwork policy only after checking tx still viable, never suppress automatic transaction rollback/commit failure.
5. **Steps:** execute builder subsets; test actual conflict behavior; bind chrono stamps; use exact ID/type/kind scope; no fs side effects. Do not eagerly delete resolved art for album/artist merely to enqueue.
6. **Tests:** `invalidate_only_requested_kind_ids_and_type_contract`; `enqueue_reset_vs_if_missing_preserve_backoff`; `enqueue_missing_excludes_resolved_and_prequeued`; `artwork_chunks_zero_100_101_200_201`; `queue_fault_second_chunk_rolls_back`; `kind_name_rejected_prefix_accepted`. Duplicate queue keys, mixed priorities/types, empty trace/source state, timestamp equality, invalid owner kind rejected, no SQL injection in identifiers. Repeated full scan queue stable.
7. **Non-vacuity:** enqueue twice with spent attempts/backoff and verify exact mode differences; unsupported kind string cannot silently match nothing; deletion decoy same ID other kind retained.
8. **Oracle:** O public artwork/queue APIs, dynamic clocks relational; preserve JSON trace shape parsed, exact priority/attempt row identity. Read counts alone not proof of inserts.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_scanner_artwork_writes --test runtime_hydrate_folder_artwork_queue --test runtime_artwork_hydration`; G.
10. **Review/DoD:** G checks kind prefixes, scope and warning-vs-fatal transaction errors; no worker launch.

## DB-13 — Epoch-only token revocation primitive for BOOT/API

1. **Status: BLOCKED.** Prereqs DB-00/03; parent `D-REVOKE` approves semantics/overflow; external `BOOT.epoch-revoke`/`API.revoke-authorization`. Goal: narrow DB mutation, not endpoint or password encryption/token refresh.
2. **V:** existing `UserRepo::put` `B/user_repo.rs:760-811` bumps token_epoch only if password Some; no-password updates preserve it. It also rewrites fields and autoassigns admin libraries, so it is NOT a safe epoch-only revoke. Builder `put_password_update_sql:312-326` same-statement epoch+password exists. Existing `put_bumps_the_token_epoch_once_per_password_change`, `put_without_a_password_updates_the_row_and_leaves_the_epoch`. `nd-auth/middleware.rs:92-118` reloads user each request and revalidates epoch; credentials.rs273 rejects mismatch. No epoch-only production API found.
3. **Own:** `src/user_repo.rs`, NEW `tests/runtime_token_revoke.rs`; potentially runtime_error enum variant via parent-controlled shared edit. No nd-auth edits; BOOT/API owns those tests/routes. Collision user source may be used by BOOT; schedule explicitly.
4. **Proposal:** `revoke_tokens_in_tx(tx,userId)->Result<NewTokenEpoch,DbRuntimeError>` or additive `UserRepo::increment_token_epoch` under caller transaction. One UPDATE `SET token_epoch=token_epoch+1 WHERE id=? RETURNING token_epoch`; no password/updated_at/last_access/library changes, no insert fallback. Missing => NotFound. Guard i64 overflow BEFORE mutation (`token_epoch < i64::MAX` plus classify existent exhausted row) or chosen typed exhaustion behavior; never let SQLite promote REAL then fail read after autocommit. Authorization is trusted caller/API, not raw HTTP bool; caller observes result only after commit. Epoch-only revoke invalidates current token too unless API deliberately issues a new token after commit; no TokenEpochHolder refresh automatically implied.
5. **Steps:** decide D-REVOKE; builder + executed tx method; keep password Put behavior unchanged; export result; BOOT/API validates old/new token requests using production middleware. No caching mechanism invented: current middleware reads fresh row.
6. **Tests:** `revoke_changes_only_epoch_and_returns_committed_value`; `revoke_missing_never_creates_user`; `revoke_rollback_old_epoch_still_valid`; `concurrent_revokes_increment_atomically`; `revoke_overflow_leaves_row_unchanged`; BOOT/API-owned `old_token_rejected_after_revoke_new_login_works`. Snapshot all user cols, password bytes and library links;0/1/many revokes, database error not mistaken NotFound, typed new epoch each commit.
7. **Non-vacuity:** no-password Put would leave epoch unchanged; snapshot ensures implementation didn't reuse password Put merely to force a bump. Failed tx must not publish a new epoch/token to holder/response.
8. **Oracle:** Go password epoch contract (`persistence/user_repository.go:129-144`) motivates atomic increment; **standalone epoch-only operation is an approved Rust API addition, not claimed existing Go endpoint parity**. Exact DB field effects deterministic; security consumer test belongs BOOT/API.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_token_revoke --test runtime_user_repo`; G; BOOT/API later nd-auth suite + route test.
10. **Review/DoD:** G fresh security reviewer checks no extra field writes, overflow/authorization/postcommit boundaries. Report precise gap vs existing API to BOOT/API.

## DB-14 — Final folder-delta persistence transaction (DB-owned SC seam)

1. **Status: BLOCKED.** Prereqs DB-00/04/05/06/07/08/09/10/12/15 and `SC.folder-write-batch-contract`; DB-11 for integrated read-back. Goal: execute one folder delta in Go order with retry-safe inputs; NOT filesystem walk, tag mapping, album aggregation/equals, controller or phase scheduling.
2. **V:** Go `scanner/phase_1_folders.go:353-486` persists in one retryable tx: folder, tags, artists+library links, albums, tracks, media artwork invalidation, missing marks/touches, queue. AlbumIDMap cloned on every retry375-380, postcommit image changes at363-370. Rust currently has no combined operation; store has only generic transaction seam.
3. **Own:** NEW `src/scanner_writes.rs`, NEW `tests/runtime_folder_persistence.rs`, `src/lib.rs`; O folder-delta cases. Calls prior functions, no parallel edits to repo implementations or SC mapping files. Input types in same new module; collision SC consumes types, DB-16 may add transitions only after this writer stops.
4. **Proposal:** immutable `FolderWriteBatch { library_id,folder:FolderRecord,tags,artists,albums,media,missing_media_ids,album_transitions,queue_items,full_scan_mode,clock }`; SC supplies fully constructed typed records, IDs/storage facts and aggregate decision. `persist_folder_delta(store,batch)->CommittedFolderOutcome` opens typed retry tx; internal `persist_folder_delta_in_tx` ordered statements. Publish IDs/warnings/affected sets ONLY after commit. Keep scope checks against batch library and ID existence; no untrusted public actor. Error severity: core writes fatal, artwork invalidation/enqueue and Go annotation/copy warn-only where Go does so, but transaction-aborted/commit errors always fatal. Define structured warnings with original cause, no secrets/record dumps. No mutation of shared batch maps during retries.
5. **Steps:** validate structural consistency before checkout; preplan generated IDs stable per batch; execute Go sequence using previous APIs; use DB-15 transition helpers with Go poison guard and warning policy; commit then outcome for SC to record progress/image invalidation; no broker/filesystem side effect in callback. SC owns whether finalize may mark unvisited folders; DB rejects absent/invalid scope rather than infer scan success.
6. **Tests:** `folder_batch_real_record_graph_and_fts_roundtrip`; `folder_batch_second_identical_pass_idempotent`; `folder_batch_fault_after_each_core_stage_rolls_back_every_table`; `folder_retry_reuses_original_album_transition_and_ids`; `nonfatal_artwork_warning_preserves_core_commit_but_lost_tx_is_fatal`; `cross_library_record_in_batch_rejected_before_write`. Zero audio/tags/albums, missing-only folder, 201 missing IDs, multiple subroles and same library links; malformed record rejected, optional metadata preserved. Integrate SC fixture later, but synthetic valid DB records suffice here.
7. **Non-vacuity:** fail after media base/junctions and ensure folder/tag/artist/library/album/media/FTS/queue all unchanged; force exactly one busy retry after controlled partial writes and compare database state to one clean run. A retry map consumed in first attempt must not lose annotation transition in second.
8. **Oracle:** O exported Go scanner is not trivially injectable; use public repository sequence with source-labeled phase-order manifest, or SC-owned real scanner fixture once available. Do not call a copied SQL-only sequence “real Go scanner executed”. Deterministic final state, timestamps approved clock normalization.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_folder_persistence`; G; SC-owned future `SC.phase1-real-tree` tests execute after writer ends.
10. **Review/DoD:** G and fresh cross-boundary reviewer: DB owns writes only, SC constructs batch/aggregates; first honest phase1 persistence ready when this + DB-11 + external SC acceptance passes, not full #05.

## DB-15 — Average-rating renderer repair and typed annotation reassignment
### R2 independent audit verdict (2026-11-18, `/tmp/nd-independent-audit`)

Probe over the real fresh schema: 189 statement records → **178 OK / 9 ERROR / 2 MISMATCH**.
Accepted conclusions (I re-checked each against source myself):
- Go's annotation surface is the `AnnotatedRepository` mixin (`model/annotation.go:18-23`)
  implemented by `sqlRepository` methods `SetStar`/`SetRating`/`IncPlayCount`/`ReassignAnnotation`
  (`persistence/sql_annotations.go:137,142,162,185`). There is **no** annotation repository, but
  Go *does* read `annotation` via the `withAnnotation` LEFT JOIN (`:69-72`) and the
  `updateAvgRating` subquery (`:151-157`). My earlier "never selects from annotation" wording was
  wrong and is corrected above.
- `update_avg_rating_sql` (`sql_annotations.rs:301-306`) renders `SET average_rating = ?` while
  passing the two subquery args plus the row id → `Err(InvalidParameterCount(3, 2))` on execute.
  Root cause: the aggregate expression is built but never embedded in the SQL. **DB-15 owns the
  fix**; Go's shape is `Set("average_rating", Expr("coalesce((select … ) , 0)"))`.
- `annotation_count_sql` (`:405-409`) emits `count(distinct annotation.id)`; `annotation` has no
  id column, so preparation fails outright (`no such column: annotation.id`). Neither renderer has
  a non-test call site, so both are latent. **DB-15 also owns disposition**: delete the three
  standalone read renderers (`get_annotations_sql`, `annotation_exists_sql`, `annotation_count_sql`)
  or re-document them as an invented seam with the missing-id parity fact stated. Do not "fix" the
  count by inventing an id column.
### DB-15 fix design (parent, 2026-11-18) — verified arg order by execution

**The average-rating repair is exactly one line plus its doc, and the arg order is already right.**
`squirrel` renders SET before WHERE and inlines `Expr` args at their position, so Go's
`toSQL()` order is `(itemID, tableName)` from the subquery then `id` from the outer WHERE — the
same list `update_avg_rating_sql` already builds. The only bug is that the expression text is
never placed in the SQL. Correct renderer:

```rust
format!("UPDATE {table} SET average_rating = {expr} WHERE id = ?", expr = average_rating_expr_sql())
// args unchanged: [item_id, table, item_id]  → 3 placeholders, 3 args
```

Verified in `/tmp/w11check`: executing that statement with `params!["mf1","media_file","mf1"]`
returns `rows=1` and `average_rating=4` for ratings 5/3/0 across three users. Constraints the
implementation and tests must keep:
- No `user_id` predicate in the subquery — Go aggregates across **all** users
  (`persistence/sql_annotations.go:151-157`), and `rating > 0` only.
- `table` is bound as a **value** (`item_type = ?`), never interpolated.
- `average_rating_expr_sql()` stays the single source of the expression text; the fix embeds it,
  so the existing pin test on the raw expression still has meaning.
- `SETMap` sorting is irrelevant here (Go uses chained `Set`, one column), so column order is not
  a factor; do not "align" it with SetMap sorting.

**Disposition of the three standalone annotation read renderers** (`get_annotations_sql`,
`annotation_exists_sql`, `annotation_count_sql`): parent decision is **keep-and-redocument**, not
delete. Reasons: removing a public API is a deliberate divergence deserving its own approval, and
the honest statement is a documented seam, not silence. Concretely: drop the false claim that they
serve a lazy `annotation()` repository; state that Go has no annotation repository (its surface is
the `AnnotatedRepository` mixin, `model/annotation.go:18-23`, implemented by `sqlRepository` at
`persistence/sql_annotations.go:137,142,162,185`) and that Go reads `annotation` only via the
`withAnnotation` LEFT JOIN (`:69-72`) and the `updateAvgRating` subquery (`:151-157`); state that
`annotation` has **no id column** (`schema_ddl.rs:59`, PK `unique(user_id,item_id,item_type)`) so
`annotation_count_sql` cannot be prepared, and add an executed test asserting exactly that failure
(`no such column: annotation.id`) so the latent defect is pinned rather than silently shipped.
If you (reviewer) find a real Go read that needs one of these shapes, say so — that would change
the disposition to "implement Go's actual shape".

### Parent executed reproduction (2026-11-18, `/tmp/w11check`, fresh schema + FK-enforced seed)

Fixture: library 1, folder f1, media_file mf1, three users each annotating mf1 with ratings
**5, 3, 0**. Actual output:

```text
RENDERED: UPDATE media_file SET average_rating = ? WHERE id = ?
ARGS    : 3
EXECUTE ERROR: InvalidParameterCount(3, 2)
GO SQL rows=1 average_rating=4  (expected 4.0: ratings 5,3 counted, 0 excluded)
COUNT RENDERED: SELECT count(distinct annotation.id) as count FROM annotation WHERE ...
COUNT PREPARE ERROR: no such column: annotation.id ... at offset 22
count(distinct item_id) = 1 (the only id-ish column available)
```

Two facts the fix must preserve, both now proven by execution:
1. Go's `updateAvgRating` filters by `item_id`/`item_type` and `rating > 0` only — **not** by
   `user_id` — so the average spans every user's ratings for that item (3 annotators → 4.00,
   not a per-user average). A Rust fix that adds a user filter would be a silent behavior change.
2. `annotation` has no id column and its PK is `unique(user_id, item_id, item_type)`
   (`schema_ddl.rs:59`), so `count(distinct annotation.id)` can never prepare. Do not invent an
   id column to make it pass; the disposition is delete-or-redocument.

- R2's coverage claim is explicitly **bounded**: complex signatures and multi-shape inputs were
  sampled loosely, so "no other fixed-shape mismatch" is unproven globally, not disproved.
  Fragments must be wrapped before preparation (it also found `artwork_queue_filter_sql` and
  `child_playlist_filter_sql` are fragments).

### Parent verification of DB-15's premise (2026-11-18, while the DB-03 lane was running; DB-03 has since landed at `4a8aac9f`)

Confirmed independently, not copied from the plan:
- `sql_annotations.rs:301` `update_avg_rating_sql` emits `UPDATE {t} SET average_rating = ?
  WHERE id = ?` (2 placeholders) with 3 args `[item_id, table, item_id]`. Executed against
  SQLite: `Wrong number of parameters passed to query. Got 3, needed 2` — the renderer cannot
  execute at all, so this is not a silent no-op like DB-01 was.
- Go's `updateAvgRating` (`persistence/sql_annotations.go:151-160`) puts the aggregate
  **inside** the SET via `sq.Expr(...)`. Reproduced with pinned squirrel v1.5.4:
  `UPDATE media_file SET average_rating = coalesce((select round(avg(rating), 2) from
  annotation where item_id = ? and item_type = ? and rating > 0), 0) WHERE id = ?`,
  args `[mf1 media_file mf1]`. Executing that shape gives `round(avg(4,5),2) = 4.5`.
- The crate already exposes the expression as `average_rating_expr_sql()` (`:314`) and it is
  referenced ONLY from its own test (`grep` shows definition at `:314`, use at `:608`) — the
  renderer never embeds it, and the lib test at `:602` pins the broken shape.
- Correct fix = embed the expression and keep the same 3 args (verified executing).
- Blast radius in Go for the DB-01 class of bug: `scanner/phase_1_folders.go:446` calls
  `mfRepo.MarkMissing(ctx, true, entry.missingTracks...)` and `:534` calls
  `Folder().MarkMissing`, so an ids-first bind in the Rust port would leave missing tracks
  flagged as present after a scan. That is why DB-01 needed executed tests, not more
  renderer assertions.
- `reassign_annotation_sql` (`:359`) matches Go's `Expr` render exactly (`update or ignore
  annotation set item_id = ? where item_type = ? and item_id = ?`, args `[newID table
  prevID]` — squirrel probe confirmed), so only the average leg is broken.

### Renderer placeholder/arg audit (2026-11-18)

A structural scan of every `pub fn … -> Sql` for `?` count vs bound-arg count flagged these
candidates; all were inspected and only DB-15's is a real defect. The rest legitimately have
dynamic counts (`vec![..]` for the fixed prefix + per-chunk/per-column expansion) or return a
`(Sql, Sql)` pair where the caller supplies the aggregate values:
`album_repo.rs:1235 copy_attributes_update_sql` (args come from the caller's `values`, count
follows `columns`), `playlist_repo.rs:476 refresh_counters_sql` (update leg intentionally
partial: caller fills duration/size/count — pin this shape when DB-17 executes it),
`radio_repo.rs:501 enqueue_artwork_sql` (6 placeholders, 6 args), `folder_repo.rs:975
purge_empty_sql`, `mediafile_repo.rs:1514 find_by_paths_sql`, `artwork_queue_repo.rs:305
source_filter_sql`, `sql_annotations.rs:143 ann_id`, `sql_bookmarks.rs:56 bmk_id`, the
`put_sql`/`put_update_sql` family (`artist/library/radio/user/playlist/album`, args built from
the record), and `share_repo.rs:248 update_sql` (args from `cols`).
**Mechanical execution of that audit (2026-11-18, `/tmp/paramcheck`, ~75 renderers against the
migrated schema).** Result: exactly **one** placeholder/arg mismatch among fixed-shape
statements — `annotations::update_avg_rating_sql` (2 placeholders, 3 args), confirming DB-15.
`playlist::refresh_counters_sql`'s update leg shows 5 placeholders vs 2 args **by design** (the
caller supplies duration/size/count); when DB-17 executes it, bind all five or the statement
fails with `Wrong number of parameters`. Three other findings the by-inspection pass missed:
- `sql_annotations.rs:381/392/405` (`get_annotations_sql`, `annotation_exists_sql`,
  `annotation_count_sql`) have **no Go counterpart as standalone reads** — correction after the
  R2 audit: Go *does* read `annotation` in production, but only through the `withAnnotation`
  LEFT JOIN (`persistence/sql_annotations.go:69-72`) and the `updateAvgRating` subquery
  (`:151-157`, `coalesce((select round(avg(rating), 2) from annotation where item_id = ? and
  item_type = ? and rating > 0), 0)`). What does not exist is an annotation **repository**:
  `SQLStore` has no annotation field (`persistence/persistence.go`) and no
  `Select().From(annotationTable)` anywhere. Worse,
  `annotation_count_sql` emits `count(distinct annotation.id)` — `annotation` has **no id
  column** (`schema_ddl.rs:59`, PK is `unique(user_id,item_id,item_type)`), so it fails
  preparation outright: `no such column: annotation.id`. Its doc claim that these serve "the
  lazy `annotation()` repository (`persistence.go:17-40`)" is false. Nothing calls them outside
  their own unit tests, so they assert a shape Go never produces; the DB-15 lane should delete
  them or re-document them as an invented seam (the generic
  `sqlRepository.count`/`exists` cannot be instantiated over `annotation` precisely because of
  the missing id column — that is the real parity fact). R2 independently reproduced the
  prepare failure (`no such column: annotation.id`) and the rating mismatch
  (`Err(InvalidParameterCount(3, 2))` on execute) against the real fresh schema.
- Clause **fragments** cannot be prepared standalone by design (`ann_id`, `bmk_id`,
  `with_bookmark`, `annotation_join`, `folder::library_filter`, `folder_subtree_filter`,
  `playlist_track::library_filter_on`, `artwork_queue::source_filter_sql` all return WHERE/JOIN
  pieces). A future param-count gate must wrap them in a statement instead of preparing them
  directly — record that so the gate does not report false failures.
- `plugin` is absent from the fresh DDL; `DELETE FROM plugin` therefore fails with
  `no such table: plugin`. Not a repo bug, but `plugin_repo.rs:11-14` claims the schema slice
  "owns adding it", which contradicts `schema-contract.md:93-96` (the table is deliberately
  dropped). That comment is stale and needs the scope decision below.
- `share::update_sql` with an empty `cols` renders `UPDATE share SET  WHERE id = ?`, a syntax
  error. Go's `updateOwned` path can never produce an empty SET because `filterUpdateValues`
  always adds `id` (`persistence/sql_base_repository.go:612-616`, `selectUpdateColumns :593-609`
  keeps every key when `colsToUpdate` is empty). Low priority, but worth one assertion in the
  share/DB-16 lane: an empty column list must not be handed to this renderer.


Recommendation: when DB-21 integrates acceptance, add ONE executed gate that prepares every
renderer's statement against the migrated schema and asserts `parameter_count() ==
args.len()`. That would have caught DB-01 and DB-15 mechanically instead of by review, and it
costs one test file. Do not add it before the mutation renderers stabilize (DB-04..DB-17), or
it will fail on intentionally-partial shapes like `refresh_counters_sql`.


1. **Status: repair portion LANDED at `ab466928` (render-only rating repair — this does NOT complete DB-15); runtime half BLOCKED.** Prereqs DB-00 (landed `48e65806`/`ec84c176`) / DB-03 (landed `4a8aac9f`) for the executed tx helper; the render-only regression shipped first. Goal: prevent activating two-placeholder/three-bind renderer and preserve annotation average on ID changes. Non-goals: general rating/starring endpoints.
2. **V, newly found during planning:** `B/sql_annotations.rs:301-309` `update_avg_rating_sql` emits `SET average_rating=? WHERE id=?` but args `[itemId,table,itemId]`; the actual aggregate expression is separately `average_rating_expr_sql:314-316`, not embedded. Existing `update_avg_rating_binds_subquery_args_before_where_id`602-620 pins that inconsistent shape. Go `persistence/sql_annotations.go:151-162` embeds coalesced rounded avg positive ratings; `ReassignAnnotation:185-196` UPDATE OR IGNORE then update average. No execution by planner/parent claimed for this new renderer defect.
3. **Own:** `src/sql_annotations.rs`, NEW `tests/runtime_annotation_transfers.rs`, O annotation fixtures. Prefer new runtime helper in this file or fixed registered entity enum in runtime_error/types; no arbitrary table input. Collision DB-16/20, shared helpers potential external API later.
4. **Proposal:** repair SQL to embed average_rating_expr_sql and exactly3 ordered binds; `reassign_annotation_in_tx(tx,EntityKind,old,new)->Result<(),DbRuntimeError>` short-circuits empty/equal IDs per Go, moves OR IGNORE, recalculates target average in same tx. Duplicate per-user target annotation wins; do not merge or delete source conflict rows silently. Preserve zero/NULL/unrated average rule and type string `album` vs artwork prefix (not interchangeable).
5. **Steps:** execute function-generated average update on seeded tables to expose parameter mismatch; fix lib pin; add transfer wrapper; recompute average after moved population; query exact target/source annotations and cached averages; rollback injection after move.
6. **Tests:** `avg_rating_embeds_subquery_and_binds_exactly`; `avg_rating_zero_positive_and_rounded_population`; `reassign_keeps_target_conflict_and_moves_other_users`; `reassign_empty_equal_ids_noop`; `reassign_failure_rolls_back_rows_and_cache`. Two item types same IDs to prove type restriction; NULL ratings/custom malformed schema hard errors as applicable; absent source, missing target row Go no-op update? pin exact public behavior; no invented item validation policy.
7. **Non-vacuity:** existing renderer must fail actual execute parameter-count check before fix; control incorrect numeric literal average cannot pass expected rounded aggregate. Old OR REPLACE/merge shortcut would lose target annotation and fail row assertions.
8. **Oracle:** O public Album/MediaFile.ReassignAnnotation methods, source exact average expression; deterministic sums/rounding and source/target conflict identities. Preserve raw typed SQLite cause for malformed test fixture.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_annotation_transfers`; `cargo test --locked -p nd-db --lib update_avg_rating`; G.
10. **Review/DoD:** G, source-level placeholder/bind check AND real read-back; no runtime transfer caller before this passes. Parent may split a READY repair-only assignment without changing stable ID.

## DB-16 — Move/reference/copy transaction primitives
### Pre-flight parity finding: empty column list (2026-11-18, parent-measured)

`share::update_sql(id, &[], owner)` renders `UPDATE share SET  WHERE id = ?`, a SQLite syntax
error; `ShareRepo::update` therefore returns `ShareError::Sqlite` on an empty `cols`. Go's
semantics differ and must be pinned, not guessed: `shareRepository.Update`
(`persistence/share_repository.go:158-166`) appends `updated_at` **only when `len(cols) > 0`**,
then `updateOwned` → `filterUpdateValues` → `selectUpdateColumns`, whose predicate is
`len(c2upd) == 0 || found` (`persistence/sql_base_repository.go:600`) — so an empty list keeps
**every** model column, `created_at`/`birth_time` are deleted (`:605,:608`), `user_id` is deleted
(`:512`), and `updateValues["id"] = id` (`:614`) makes the SET never empty. Net Go behavior: an
empty `cols` updates the whole row (including `updated_at`) rather than failing.

The only production caller passes two columns (`core/share.go:51` `last_visited_at`,
`visit_count`; `core/share.go:148` forwards the API's `cols...`, which could be empty for a
client sending none), so the divergence is reachable through the API path in principle.

**DB-16 (or a dedicated share lane) must:** (a) decide and document faithful parity — empty
`cols` means "update all model columns except created_at/birth_time/user_id, plus id" — or a
deliberate rejection error, with a citation; (b) prove the chosen behavior by executing against
`apply_fresh_schema`, including the `updated_at` inclusion rule (`updated_at` is appended only
when `cols` is non-empty, so an explicit `cols` list must already include `updated_at` to
refresh it); (c) never emit an empty SET silently. Do not "fix" this by defaulting to
`updated_at`-only, which is neither behavior.


1. **Status: BLOCKED.** Prereqs DB-00/02/03/07/09/10/11/15; external `SC.move-selection` for integrated scanner matching, not DB implementation. Goal: user-data-preserving executed operations; DB does not choose ambiguous candidate or maintain SC processed-album map.
2. **V:** `B/mediafile_repo.rs:1818-1853` reference bundle omits target avg recalculation because it calls only low-level reassign SQL; Go `persistence/mediafile_repository.go:410-430` calls full ReassignAnnotation, bookmark, playlist, scrobbles, OR IGNORE buffer. Album copy builders `B/album_repo.rs:1220,1235`; Go CopyAttributes331-356 casts text and skips NULL/empty/0001-created_at poison. Scanner move291-335 in phase2 puts old ID onto target, deletes discarded new row, optional album reassignment/copy (not identical to generic ReassignReferences).
3. **Own:** `src/{mediafile_repo.rs,album_repo.rs,sql_bookmarks.rs}` narrow runtime helpers, extend `scanner_writes.rs` if move executor belongs there; NEW `tests/runtime_move_persistence.rs`; O moves/reference/copy cases. Collision DB-14/15/20; no SC selection files.
4. **Proposal:** `reassign_references_in_tx(tx,old,new)` uses full DB-15 then bookmark then playlist_tracks/scrobbles/buffer in Go order; `copy_album_created_at_in_tx` whitelists only needed column (not arbitrary SQL identifiers). `apply_selected_move(store,MoveWritePlan)->CommittedMoveOutcome`: immutable selected target snapshot/old identity/album transition already decided by SC; each retry clones original target, preserves missing.created_at, puts old ID at new path then deletes discarded target; no blanket reassign of old identity because that would contradict scanner move semantics. Separate generic reference API for actual callers. Warn/fatal classification mirrors Go caller policies, original errors available.
5. **Steps:** implement leaf reference/copy helpers; add avg update absent in existing bundle; add poison guard; compose atomic selected-move sequence; publish committed outcome only; SC locks/maps updated/rolled back outside DB with explicit handshake. DeleteAllMissing primitive only under approved purge policy, not automatic move phase default.
6. **Tests:** `move_keeps_identity_created_birth_and_all_user_references`; `reference_reassign_duplicate_target_annotations_bookmarks_buffer`; `copy_created_at_skips_null_empty_zero_and_preserves_valid_text`; `move_midstep_failure_and_busy_retry_leave_no_half_move`; `cross_library_move_scope_and_unrelated_data`; `purge_missing_requires_system_policy_and_is_rollbackable`. 0/noop old==new, missing source/target distinct Go behavior, malformed source timestamp cast preserves storage text but poison skipped, conflict users/duplicate queue keys, multiple library candidates SC-owned choice. IDs/ratings/stars/playcount/bookmark/playlist/scrobble/buffer checked individually.
7. **Non-vacuity:** old low-level reference bundle must fail cached-average assertion; modifying shared target before retry must fail preserved-created assertion. A missing/delete-only implementation fails reference and identity snapshots.
8. **Oracle:** O public ReassignReferences/CopyAttributes and source-labeled scanner move order; do not require summing conflicted ratings (Go OR IGNORE retains target). For true Go scanner matching require SC-owned end-to-end oracle later.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_move_persistence --test runtime_annotation_transfers --test runtime_mediafile_writes`; G.
10. **Review/DoD:** G fresh user-data reviewer, no implicit production purge, commit/retry leaves caller state consistent. Record source-only renderer findings now covered by execution.

## DB-17 — Playcount, tag and library count refresh execution

1. **Status: BLOCKED.** Prereqs DB-00/02/03/05/06/08/09/10. Goal: execute count refresh statements, not aggregate MediaFiles->Album (SC) or artist role-stats batching (DB-18).
2. **V:** album refresh1253, artist refresh940, tag update_counts148, library probes715/update787 only render. Go album411-428, artist441-460, tag48-69, library196-251. Library refresh has8 probes then update; Go uses parallel closures, Rust same-connection serial execution is the safe equivalent snapshot. Existing lib SQL pins and artist read/stats-fold tests do not execute these refresh lifecycles.
3. **Own:** `src/{album_repo.rs,artist_repo.rs,tag_repo.rs,library_repo.rs}` new executed refresh helpers; NEW `tests/runtime_count_refresh.rs`; O counts. Collision DB-18/20, no hydrate algorithm changes.
4. **Proposal:** `refresh_album_playcounts_in_tx`, `refresh_artist_playcounts_in_tx` -> affected rows; `update_tag_counts_in_tx`; `refresh_library_totals_in_tx(libraryId,now)`. Typed RefreshStatsTotals with chrono from DB-02, no positional caller aggregate arrays. Execute probes same tx sequentially, NULL aggregates use Go coalesce/zero, counts typed i64, durations exact expected f64 tolerance declared. Preserve annotation stars/ratings while updating play_count/play_date; item-type filtering exactly Go unless a discovered source issue requires parent decision. Tag updates only Go-defined genre projections; do not invent clearing/extra counts.
5. **Steps:** seeded production graph; execute each refresh; probe/update library; count/map fields audited by index; return typed result/no external progress; repeat to settle idempotently. No per-query new lease inside tx.
6. **Tests:** `playcount_refresh_preserves_other_annotation_fields`; `tag_counts_by_library_genre_and_repeated_scan`; `library_eight_totals_distinct_missing_empty_and_file_counts`; `refresh_failure_after_probe_or_first_update_rolls_back`; `chrono_cutoffs_and_oldest_scan_after_refresh`. Zero media/albums, only missing, shared artist/album across libraries, multiple images/playlists in folder counts, NULL malformed schema error, exact latest playdate and positive counts, counters>i32, rounding/float policy. Affected rows distinguished from entities processed.
7. **Non-vacuity:** rows with same source counts but distinct total_files/size/duration detect swapped probe assignment; existing ratings/stars detect whole-row replacement. Fail final update and prove no partial annotation/tag commit if invoked in same caller tx.
8. **Oracle:** O public RefreshPlayCounts/Tag.UpdateCounts/Library.RefreshStats. Canonicalize result row order only; counts exact, chrono relations approved, preserve Go sum/filter behavior.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_count_refresh --test runtime_scan_clock`; G.
10. **Review/DoD:** G; count semantics source-traced; SC phase3 calls DB refresh, not DB recreating domain album equals.

## DB-18 — Artist per-library role-stat refresh batches

1. **Status: BLOCKED.** Prereqs DB-00/02/03/05/08/10; parent `D-STATS` approves Go-compatible partial-commit/nonfatal cleanup behavior. Goal: execute role stats ID pass/batches/cleanup, not change existing artist PostScan fold.
2. **V:** `B/artist_repo.rs:965,988,1068,909` builders and `REFRESH_STATS_BATCH_SIZE=1000:159`; Go `persistence/artist_repository.go:465-614`: no IDs=>return0 before cleanup; ids repeated4 times per1000 batch; global cleanup stats='{}'; mark orphans if cleanup removed rows OR allArtists; cleanup/orphan errors logged nonfatal; partial rows affected returned on batch error. Existing runtime_hydrate_artist tests fold stored stats but not whole refresh sequence.
3. **Own:** `src/artist_repo.rs`, optional NEW `src/maintenance.rs` facade if approved DB-00, `src/lib.rs` only if new; NEW `tests/runtime_artist_stats_refresh.rs`; O artist-stats. Collision DB-08/17/20.
4. **Proposal:** `refresh_artist_stats(store,Mode::All/Touched)->RefreshStatsOutcome{rows_updated,warnings}`; typed ID read, chunk1000, binds four complete chunk repetitions in order; per-batch transaction on a lease, not one giant lock unless parent approves changed atomicity. Preserve original typed failure and completed-batch count if operation returns partial result/error (choose error envelope DB-00). Cleanup and orphan flags follow exact Go conditions, including zero-ID early return. Do not broaden selective library scope silently: Go touched cutoff reads oldest library globally, stats spans artist's libraries, cleanup global.
5. **Steps:** freeze D-STATS; read stable ID list; execute batches; accumulate rows; nonfatal cleanup warning outcome; markOrphans only required branches; compare refreshed stats via existing ArtistRepo hydration. Async orchestration runs blocking DB work off executor; cancellation between batches explicitly returns partial-progress outcome, no claim all-or-nothing.
6. **Tests:** `artist_stats_all_and_touched_oldest_cutoff`; `artist_stats_zero_999_1000_1001_fourfold_binds`; `role_total_maincredit_counts_distinct_media_albums`; `stats_empty_cleanup_orphan_conditions`; `batch2_failure_keeps_batch1_and_typed_progress`; `cleanup_failure_warns_without_lying_successful_cleanup`; `batch_retry_no_double_counts`. Missing media inclusion follows Go (do not add missing=false unless source does), bad JSON stats source failure, mixed roles/subroles across libraries, NULL sums zero rules, full vs touched noIDs branch.
7. **Non-vacuity:** omission/misordering of one of four bind lists must fail runtime parameter/result assertions; >1000 fixture proves actual second batch, not one-row branch; failure behavior distinguishes all-transaction rewrite from approved Go partial batches.
8. **Oracle:** O actual Artist.RefreshStats, canonical JSON objects/role sets, totals exact, dynamic cutoff relation. SQL-only fold oracle insufficient. Nonfatal branch hard to trigger via public Go may need replicated SQL/error-injection clearly labeled.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_artist_stats_refresh --test runtime_hydrate_artist`; G.
10. **Review/DoD:** G; every nonfatal behavior and partial count intentional, no swallowed busy cause or all-or-nothing claim.

## DB-19 — Minimal playlist orphan cleanup dependency (NOT full playlist runtime)

1. **Status: BLOCKED.** Prereqs DB-00/02/03/12; parent approval for DB-owned minimal cleanup boundary vs `SC.playlist-import` full service. Goal: support GC op11 only; not playlist CRUD/track editing/smart refresh/M3U import.
2. **V, newly inspected:** `B/playlist_repo.rs:832-842` emits orphan filter `mf.id = ?` + Null, unlike Go `persistence/playlist_repository.go:439-473` Eq{nil}=>IS NULL, so existing builder cannot find orphans. Existing `remove_orphans_and_renumber_pin_go_literals`1783 pins it. `refresh_counters_sql:476-498` returns update with5 placeholders but only clock/id arguments: explicitly a caller-fill TEMPLATE, not executable as returned. `renumber_sql:865-885` has two steps but not subsequent queue/counters. Go renumber475-500 does negate -> CTE -> cover enqueue(nonfatal) -> counters; Go counter aggregates destination fields float32 (`:338`), potential precision behavior must be decided/replicated explicitly.
3. **Own:** `src/playlist_repo.rs`, NEW `tests/runtime_playlist_gc_cleanup.rs`, O orphan/renumber cases. If extracting narrow NEW `playlist_maintenance.rs`, parent approves and owns lib.rs collision; do not expand playlist_track public API. Shared with SC/phase4 future writer.
4. **Proposal:** `remove_orphan_tracks_in_tx(tx,scope,now)->PlaylistCleanupOutcome`; repair predicate to IS NULL (no bound Null), select distinct affected playlists; delete only actual orphans; renumber negation/CTE preserving original order/duplicates; enqueue cover only on content change through DB-12; read aggregates into typed `PlaylistCounters`, construct COMPLETE update binds `[duration,size,count,time,id]`. Keep counters semantics including missing media per Go. Decide Go float32 cast vs lossless Rust counters explicitly; recommend faithful observed values for compatibility, not accidental lossy casts. All core cleanup steps same outer GC tx; queue error warning policy per Go.
5. **Steps:** failing real orphan-select test before repair; typed counter constructor replaces template public misuse (retain low-level compatibility only clearly marked); execute delete+renumber+queue+refresh; no orphan cases no cover rebuild; ensure existing playlist reads unchanged.
6. **Tests:** `orphan_select_uses_is_null_and_finds_real_ghost`; `cleanup_deletes_only_orphans_and_renumbers_original_order`; `cleanup_refreshes_counters_and_enqueues_cover_once`; `no_orphans_no_rebuild_zero_tracks_zero_counters`; `cleanup_step_failure_rolls_back_negative_positions_and_counts`; `large_counter_precision_contract`. Use controlled foreign_keys=off ONLY to construct intentional legacy ghost fixtures, re-enable and verify before tested operation; every-handle FK normal tests stay separate.0/1/many playlists/tracks, duplicate media tracks, negative/zero existing positions policy, missing rows vs NULL/custom corruption, two owners/library mixes/smart playlist path; ordering exact.
7. **Non-vacuity:** old `=NULL` returns no orphan and must fail new function-result test; directly executing current incomplete counter template must be rejected/not used; injected failure after negation must restore positive original positions by rollback.
8. **Oracle:** O public GC/removeOrphans reachable through DataStore.GC and before/after playlist state; separate public track delete may exercise renumber but is not exact orphan entry. Label any copied private-method SQL as replicated. No existing Go file edits required.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_playlist_gc_cleanup --test runtime_hydrate_playlist_radio_artwork --test runtime_hydrate_playlist_track_scrobble`; G.
10. **Review/DoD:** G reviewer confirms truly complete binds/predicate, minimal boundary and no full playlist completeness claim. Required before DB-20 op11 is wired.

## DB-20 — Eleven-operation GC with explicit selective scope and postcommit effects

1. **Status: DECISION REQUIRED.** Prereqs DB-00/03/05/06/07/08/09/10/15/19; parent `D-GC` chooses selective-scope semantics and uploaded-image side-effect ownership. External execution gate `SC.successful-phase-coverage` / `SC.purge-policy` before calling on real data. Goal: DB sequence/transaction proof, no production cleanup authority.
2. **V:** Go `persistence/persistence.go:263-275`11 ops: album purge, artist purge, artist markMissing, folder purge, album/artist/media/playlist annotation clean, media bookmark clean, unused tags, orphan playlist tracks. Only album/folder directly accept selected library IDs; other ops global. Existing Rust store has NO gc, only per-op builders. Artist purge `B/artist_repo.rs:897-905` selects filenames then deletes; Go `artist_repository.go:370-405` best-effort os.Remove inside method. DB must not perform filesystem deletes before retry transaction commits. Album purge scope1200, folder purge975, annotation clean370/bookmark clean211/tag purge174 exist. No executed GC lifecycle tests yet.
3. **Own:** NEW `src/gc.rs`, NEW `tests/runtime_gc.rs`, `src/lib.rs`; add narrow transaction-bound purge/clean leaf helpers in `album_repo.rs`, `artist_repo.rs`, `folder_repo.rs`, `tag_repo.rs`, `sql_annotations.rs`, `sql_bookmarks.rs`; shared ownership list closed before launch, no concurrent editors. No filesystem cleanup or SC code; parent docs only.
4. **Decision contract:** `GcScope` MUST distinguish All from explicit Libraries(nonempty). Empty subset must not silently mean All. **Recommend GoSelective-compatible scope initially**: album/folder constrained to selected IDs, other11-chain operations global by their own referential predicates; document that this is NOT strict isolation of preexisting unrelated orphans. If parent requires strict selected-library-only cleanup, STOP: orphan annotation/playlist ownership may need pre-delete candidate sets to preserve scope, and global artist/tag shared entities need a separately specified policy. No claim fully scoped by passing a vector to two builders. Full referentially reachable content/user data must survive either policy.
5. **Proposal/algorithm:** `gc_in_tx(tx,scope,now)->GcOutcome{operation_counts,uploaded_image_candidates,warnings}` in exact11 order; outer typed transaction/retry owns atomic DB changes. Classify failures/context with retained typed SQLite errors; op failures abort except explicitly Go-nonfatal cover queue warnings. Capture artist filenames BEFORE delete; return validated relative file IDs as postcommit candidates, no fs access in DB/tx. `BOOT/SC.postcommit-image-cleanup` owns safe root containment/path validation, best-effort removal/logging after commit; change from Go precommit deletion requires recorded durability deviation (avoid rollback deleting live image). Never delete absolute/untrusted filenames from DB directly.
6. **Tests:** `gc_all_runs_eleven_ops_in_order_and_removes_only_unreachable`; `gc_selected_scope_policy_exact_and_empty_subset_rejected`; `shared_artists_tags_annotations_references_survive`; `gc_fail_each_operation_rolls_back_all_previous_and_fts`; `gc_busy_retry_one_effect_and_postcommit_candidates_only`; `gc_no_changes_idempotent_and_no_image_deletion_before_commit`; `playlist_op11_renumber_counters_queue_integrates`. Synthetic orphan vs retained-missing media, unrelated library content/orphans per chosen policy, same annotation IDs different kinds/users, NULL behavior, empty DB, chunk>200 entities, FK cascades observed, FTS integrity read checks. Phase failure gating tested in SC later; DB never derives coverage success itself.
7. **Non-vacuity:** trap op11 failure after ten mutations and prove all DB tables/FTS revert; record postcommit candidate absence on failed attempt; old orphan `=NULL` must fail integrated cleanup result if restored. At least one live shared artist/tag survives, not an all-orphan fixture.
8. **Oracle:** O actual Go DataStore.GC with optional library IDs, operation trace and before/after sets; uploaded-image filesystem behavior NOT reproduced automatically, approved deviation tested by BOOT/SC after commit. Go partial scoping must be shown explicitly in manifest, not mislabeled strict scope parity.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_gc --test runtime_playlist_gc_cleanup --test runtime_annotation_transfers`; G. No real filesystem image removal/production GC; synthetic DBs only.
10. **Review/DoD:** G fresh user-data review plus parent D-GC approval; exact11 operation table in report with dependencies/count semantics. GC not a prerequisite to advertise a strictly phase1-only fixture slice, but mandatory for full Go scanner finalize.

## DB-21 — Integrated DB persistence acceptance and consumer handoff

1. **Status: BLOCKED.** Two explicit exits: **P1-DB** depends DB-00..12 + DB-14/15 + `SC.mapping-bridge`/`SC.phase1-fixture`/`BOOT.configured-library-caller`; **FULL-SCANNER-DB** additionally DB-16..20 + `SC.full-phase-contract`/`SC.playlist-import-contract`. DB-13 separately gates `BOOT.epoch-revoke`/`API.revoke-authorization`, not phase1 ingest. Goal: verify actual production interfaces compose; no broad #05/#17 completion claim, no new schema seeds/server listener.
2. **V:** nd-scanner is scaffold, nd-cli main empty; distinct nd-mapping MediaFile DTO (`map_mediafile.rs:24`, `mdfile.rs:37,131-170`) has storage fields absent until SC supplies them; DB records/role bridges are not that adapter. Existing parent green suite doesn't execute scanner. Source audit `/tmp/navidrome-plan-db-audit.md` is background, not substitute acceptance.
3. **Own:** NEW `nd-db/tests/runtime_scanner_persistence_contract.rs`; optional NEW synthetic fixture JSON under testdata O. No editing SC/BOOT/API files in this package, no repo docs; parent integrates ready-state ledger. Consumer writers own their test modules.
4. **Contract:** freeze exact exported DB records/method signatures/cancellation/errors and semantic aliases with SC/BOOT/API. No dependency cycles: SC constructs records/batches and controls phase order; DB persists/queries; BOOT creates configured library via DB typed put; API resolves authenticated membership and uses DB-visible point reads; revoke outcome postcommit. Phase1 exit explicitly excludes move/GC/phase4/watcher/full REST. Full #05 remains incomplete for unrelated renderer-only repos even after full scanner DB exit.
5. **Steps:** consume completed production APIs only to create configured library/synthetic folder delta and read back; run rollback/retry variant; integrate actual mapped real track via SC-owned harness after writers stop; full path adds move/count/stats/GC with approved selective behavior; capture interface/deviation/evidence handoff. Do not paper over compile failure by reintroducing raw Sql in consumers.
6. **Tests/evidence:** DB-owned `production_db_phase1_contract_first_and_second_pass`; `production_batch_failure_does_not_advance_library_completion`; `production_read_scopes_and_queue_fts_match`; `production_full_finalize_db_contract` (full exit only). SC owns real-tag tree walk/metadata equality/missing-after-walk-error/phase ordering/cancellation; BOOT/API owns fresh DB configured caller, authenticated inaccessible-ID denial, epoch old-token rejection and stream/disconnect body lifecycle. Each external acceptance must report actual command/log/hash before parent marks exit. Unknown external coverage remains U, not inferred green.
7. **Non-vacuity:** forbid SQL mutation helpers in integrated path except controlled fault-injection fixture setup; exact persisted ID/FTS/junction/queue data checked, second pass must not duplicate; mapped sentinel fields distinguish default-filled fake adapter. Old wrong-bind/idle configs should fail constituent gates, no need corrupt integrated real fixture.
8. **Oracle:** O, consumer fixture manifests and actual source commit; immutable deterministic IDs and row sets, explicit dynamic clock canonicalization, query/orchestration evidence distinct. Parent links /tmp reports into durable repo ledger; no requirement to retain live session transcripts.
9. **Validation:** `cargo test --locked -p nd-db --test runtime_scanner_persistence_contract`; G plus consumer-reported focused CLI commands chosen by SC/BOOT/API. Parent runs final combined workspace gates after every writer stops. Coverage failures/blockers explicitly retained.
10. **Review/DoD:** fresh independent read-only integrated review at quiescent checkout, parent-selected fixes and final gates; final handoff lists P1-DB vs FULL-SCANNER-DB vs epoch readiness separately and remaining full#05 APIs. No authorization to commit/deploy/mutate production.

## D-ERROR finding, independently reproduced: `#[error(transparent)]` skips one chain node

W10 reported that thiserror 1.0.69's `#[error(transparent)]` does not forward `source()` to the
wrapped error. I reproduced it myself (`/tmp/techeck`, rusqlite 0.32.1, thiserror 1.0.69 — which
`cargo tree -p nd-db` confirms is the resolved version for this crate) with a real NOT NULL
violation wrapped in `TranscodingError::Sqlite` (`transcoding_repo.rs:419`):

```text
raw rusqlite error: NOT NULL constraint failed: user.created_at
raw downcast rusqlite::Error: Some("SqliteFailure(Error { code: ConstraintViolation, extended_co")
raw downcast ffi::Error: None
WRAPPED chain:
  msg=NOT NULL constraint failed: user.created_at
  msg=Error code 1299: A NOT NULL constraint failed
WRAPPED downcast rusqlite::Error: false
WRAPPED downcast ffi::Error: false
chain-walk finds rusqlite::Error=false ffi::Error=true
```

Mechanism, from the vendored macro (`thiserror-impl-1.0.69/src/expand.rs:62-70`): the transparent
`source()` body is `std::error::Error::source(self.<field>.as_dyn_error())` — it calls `source()`
**on** the wrapped error instead of returning it, so the wrapped `rusqlite::Error` node is
**skipped**. Consequences the seam must handle:
- `downcast_ref::<rusqlite::Error>()` fails at **every** node of a transparent-wrapped chain; only
  `rusqlite::ffi::Error` is reachable, and only at the second node.
- The raw `rusqlite::Error` does **not** downcast to `rusqlite::ffi::Error` (it is not a transparent
  wrapper of it — `source()` yields the ffi error as a child).
- Therefore a busy predicate that walks `Error::source()` must test BOTH `rusqlite::Error` and
  `rusqlite::ffi::Error`, exactly as Go's `errors.As` on `db.ErrorCodes` does at
  `db/db.go:128-134`. Pin this with a test using a real crate error variant, not a synthetic one.

### Resolved: go-sqlite3's "WAL forces NORMAL" is a no-op (2026-11-18, parent-read vendor source)

W9 raised whether the port must mimic go-sqlite3 coupling `synchronous` to WAL. Read the vendor
source directly (`github.com/mattn/go-sqlite3@v1.14.52/sqlite3.go`):
- `synchronousMode := "NORMAL"` is the default (`:1241`).
- The journal block sets `synchronousMode = "NORMAL"` on WAL (`:1450-1454`).
- The synchronous block runs **later** and overwrites it whenever `_synchronous`/`_sync` is
  present (`:1534-1556`, assignment at `:1546-1549`).

Therefore WAL-forces-NORMAL can never change Go's observable state: `_journal_mode=wal` alone →
NORMAL (already the default), and `_journal_mode=wal&synchronous=full` → **FULL**, because the
later block wins. `DbOpenOptions` with `journal_mode=Wal, synchronous=Full` therefore **matches**
Go rather than diverging — no coupling to implement, and no divergence to record. Recorded here so
a later reviewer does not re-open it as a missing behavior.

## DB-22 — Executed read parity for genre, scrobble, playqueue, artwork (parent-added, 2026-11-18)

1. **Status: READY as a plan; BLOCKED on checkout** — run only after DB-03 (**landed `4a8aac9f`**; the `19a5a57a` cited here is a
   superseded pre-review commit), DB-00 (**landed `48e65806`/`ec84c176`**) and DB-02 (**landed `6d9d984d`/`74ebd81c`**) have
   landed, so it is assignable now: this lane edits `src/{genre_repo.rs,scrobble_repo.rs,playqueue_repo.rs,artwork_repo.rs}`
   and runs the whole workspace, so a concurrent writer would poison its numbers.
2. **Goal:** execute the four repos' read statements against `apply_fresh_schema` and pin Go's
   observed results. Zero executed reads today (measured: 0 execution calls in each module).
3. **Go ground truth (verified this turn, cite in code):**
   - genre: `selectGenre` = `newSelect().Columns("tag.tag_value as name")`
     (`persistence/genre_repository.go:22-24`); `GetAll` `:26-31`; `Get` adds `Where(Eq{"tag.id": id})`
     `:33-38`; `Read` delegates to `Get` `:42-44`; `ReadAll` = `GetAll(parseRestOptions)` `:46-48`.
     Genre is a tag-repository override, so **verify** whether the tag library filter reaches
     `GetAll` before asserting an unfiltered row set — Rust's `get_all_sql(user_id: Option<&str>)`
     currently takes a user id, and that parameter's meaning must be proven against Go, not assumed.
   - scrobble: `baseQuery` = `newSelect().Columns("id","media_file_id","submission_time")
     .Where(Eq{"scrobbles.user_id": user.ID})` (`persistence/scrobble_repository.go:25-31`); model
     registers `from`/`to` filter fns (`:37-40`) and the `submission_time` sort mapping (`:41-43`);
     `CountAll` `:59-61`, `GetAll` `:74-79`, `Read` `:81-83`. `submission_time` is stored as
     **Unix seconds** (`RecordScrobble` `:47-54`, `submissionTime.Unix()`), so `from`/`to` mapping
     and ordering must be pinned against epoch integers, not TEXT.
   - playqueue: `RetrieveWithMediaFiles` and `Retrieve` both
     `newSelect().Columns("*").Where(Eq{"user_id": userId})` + `queryOne`
     (`persistence/playqueue_repository.go:78-92`); `Retrieve` wraps with `new(...)` (a
     nil-tolerant copy) while `RetrieveWithMediaFiles` calls `loadTracks` (`:83`, `:133-166`).
     Rust `retrieve_sql`/`load_tracks_sql` must be executed and the items-column encode/decode
     (`column_to_items`/`items_to_column`) pinned with real JSON round-trips.
   - artwork: `GetImage` = `Select("*").From(artwork).Where(Eq{"hash": hash})`
     (`persistence/artwork_repository.go:33-40`); `GetItemArtwork` =
     `Select("*").From(artwork_items).Where(Eq{"item_kind","item_id","image_type"})` `:112-120`;
     `GetMimeByHash` `:58-72` returns a hash→mime map (multi-row aggregate read); `GetInfoForItems`
     `:159+` chunks ids. `PutImage`/`PutItemArtwork` stamp `created_at`/`updated_at` from the clock
     (`:44`, `:123-125`) — DB-02's D-TIME decision (TEXT via `time::value`) governs those columns, so
     this lane must land **after** DB-02 or state clearly which storage shape its assertions use.
4. **Own:** `src/{genre_repo.rs,scrobble_repo.rs,playqueue_repo.rs,artwork_repo.rs}` plus NEW
   `tests/runtime_hydrate_genre_scrobble_playqueue_artwork.rs`. Do NOT touch `sql_annotations.rs`
   (DB-15 owns it) or `store.rs`/`runtime_error.rs` (DB-00 owns them).
5. **Tests:** per repo, at least one executed read on seeded rows plus one empty-table read;
   scrobble from/to epoch bounds; playqueue JSON items round-trip; artwork hash and
   item-kind/id/image_type lookups including the not-found error shape.
6. **Non-vacuity:** each test must assert a value that a wrong column list, wrong user filter, or
   wrong epoch mapping would change — state it per test.
7. **Validation:** `cargo test --locked -p nd-db --test runtime_hydrate_genre_scrobble_playqueue_artwork --all-features`,
   then `cargo test -p nd-db --all-targets --all-features --no-fail-fast`, workspace tests,
   clippy, fmt, `cargo doc -p nd-db --all-features --no-deps` (expect 22 warnings), `git diff --check`.
8. **Oracle:** Go `GenreRepository`/`ScrobbleRepository`/`PlayQueueRepository`/`ArtworkRepository`
   against a migrated SQLite DB; deterministic IDs and epoch stamps; record actual dynamic values.
9. **Review/DoD:** fresh read-only reviewer confirms Go citations, executed (not string-only)
   assertions, and that no repo error variant was flattened into a String.

## Dependency DAG and serial execution policy

Logical dependencies (these do NOT authorize simultaneous editing):

```text
D-ERROR -> DB-00
DB-01 ------------------------------------> DB-07
D-TIME -> DB-02 ---------------------------> DB-05/07/08/09/10/11/12/14/16/17/18/19
BOOT.db-open-policy + D-OPEN -> DB-03 -----> every runtime write/read acceptance
DB-00 + DB-03 -> DB-04
DB-00 + DB-02 + DB-03 -> DB-05 (all landed)
DB-00 + DB-03 + DB-05 -> DB-06
DB-00/01/02/03/05 -> DB-07
DB-00/02/03/05 -> DB-08
DB-00/02/03/05/06/08 -> DB-09
DB-00/02/03/05/06/08/09 -> DB-10
D-CURSOR + D-SCOPE + SC.query-consumption + DB-00/02/03/05/07/09/10 -> DB-11
DB-00/02/03 -> DB-12
D-REVOKE + DB-00/03 -> DB-13 -> BOOT.epoch-revoke / API.revoke-authorization
DB-00/03 -> DB-15 (renderer repair can precede these)
SC.folder-write-batch-contract + DB-00/04/05/06/07/08/09/10/11/12/15 -> DB-14
SC.mapping-bridge + SC.phase1-fixture + BOOT.configured-library-caller + DB-14 -> DB-21(P1-DB)
DB-00/02/03/07/09/10/11/15 -> DB-16 -> SC.phase2
DB-00/02/03/05/06/08/09/10 -> DB-17
D-STATS + DB-00/02/03/05/08/10 -> DB-18
DB-00/02/03/12 -> DB-19
D-GC + DB-00/03/05/06/07/08/09/10/15/19 -> DB-20
DB-16/17/18/19/20 + SC.full-phase-contract + SC.playlist-import-contract -> DB-21(FULL-SCANNER-DB)
```

DB-21's P1 exit requires only genuinely consumed phase1 APIs; e.g. API visible-point test and future phase2 reads in DB-11 need not delay a parent-approved narrower DB-11 phase1 handoff, but cannot be reported complete. If split, retain stable IDs with explicit acceptance checklist states; do not silently delete the unimplemented obligations. DB-14 needs DB-15 for phase1 album-ID transitions; a no-transition first fixture may demonstrate only a strictly narrower exit, not general phase1 parity.

Recommended **serial** writer order (DB-01/02/03/00 are done, so the sequence resumes at DB-04): DB-01 -> DB-02 -> DB-03 ->
DB-00 -> DB-04 -> DB-05 -> DB-06 -> DB-07 -> DB-08 -> DB-09 -> DB-10 -> DB-11 -> DB-12 -> DB-15 -> DB-14 -> DB-21(P1-DB), with DB-13 inserted at BOOT/API need after DB-00/03. Then DB-16 -> DB-17 -> DB-18 -> DB-19 -> DB-20 -> DB-21(FULL-SCANNER-DB). Independent decision/oracle investigations may be read-only; implementation always one writer. Parent may reorder independent completed prerequisites for consumer value, not bypass safety gates.

## MUST-answer questions before relevant launch

1. **D-ERROR:** common typed error vs generic typed callbacks? Final enum/API, retained SQLite cause, retry-exhaustion source, warnings/partial outcome, begin mode and cancellation semantics? A String conversion or `Ok(Err(...))` transaction workaround is prohibited.
2. **D-TIME:** canonical year1 idle + DDL zero-date handling, required-field NULL policy, exact scan-in-progress predicate and typed signatures; no Go DB migration expected. Where is normalization applied, without schema seed invention?
3. **D-OPEN / BOOT:** accepted path/URI/Go DSN grammar and defaults (15000 vs5000 timeout, WAL, synchronous, FK-on), memory mode exception, error on unsupported settings, exact caller sequence? FK is NOT presumed off.
4. **D-CURSOR + SC:** borrowed visitor vs bounded-owned phase1 subset; how does SC apply bounded backpressure/cancel while DB owns synchronous row iteration and closes lease? Are long readers allowed to pin WAL snapshot, pool capacity requirements, and no reentrant store checkout explicit?
5. **D-SCOPE + API/SC:** authority types and alias-safe membership construction; system scanner vs authenticated user interfaces; empty-scope deny behavior; same-request static membership vs cross-request reload. No route may accept raw user SQL or admin flag.
6. **D-REVOKE:** epoch-only operation authorization and whether caller token is deliberately revoked, overflow policy, postcommit token observation/response; DB does not implement route or password encryption.
7. **D-STATS:** retain Go per-batch partial commits/nonfatal cleanup (recommended) or approved stronger atomicity with lock impact? Exact partial-result/error contract required.
8. **D-GC:** Go-compatible partially scoped selective GC or stronger strict selected-library scope? Global artists/tags/annotation/playlist cleanup cannot be honestly called isolated without a separate ownership/candidate-set design. Who owns safe postcommit uploaded-image deletion, and is the deviation from Go precommit removal approved?
9. **Playlist cleanup:** accept Go float32 aggregate conversion or approve lossless counters; confirm only GC subset belongs DB now, full phase4 import/service belongs SC/related owner. Existing counter SQL is a template and requires complete typed binds.
10. **Durable oracle:** approve new standalone Go harness under testdata (no existing Go-source edits), exact exported setup/temp path recipe and tools; otherwise provide an approved alternative. No live DB, secrets, large binaries or /tmp-only proofs committed.
11. **Consumer gates:** normalize SC/BOOT/API aliases and agree who owns each real-track/auth/revoke/phase2/full-finalize CLI test; dependency cannot be considered met solely by an agent message. Current docs may change under parent ownership; implementation starts from a verified pinned source baseline.
12. **Operation safety:** later assignments authorize only synthetic tests/code. Actual destructive GC/purge, secret handling, deployment and any irreversible user-data action still require explicit user confirmation; no package here grants it.

## Planner validation/report

Actually performed: full template read; current source/test-body inspection, pinned dependency inspection from prior audit, targeted git/source inventories and parent-log inspection already attributed above. No tests, SQL probes, code changes, formatting, delegation or commits. Newly found average-rating and orphan-NULL defects are V source facts with I runtime consequences, not E executed Rust tests. Parent's mark-missing decoy reproduction is E SQL-only, not E Rust function invocation. Parent documentation edits were observed and not touched. No assertion that checkout is currently clean because parent is integrating docs. This plan is ready for parent decision normalization and bounded assignments, not implementation execution.

## Count-renderer ORDER BY parity (audit 2026-11-18)

Go's `count` (`persistence/sql_base_repository.go:565-576`) always rewrites the
projection to `count(distinct <table>.id) as count`, drops offset/limit, **appends**
`ORDER BY <table>.id` ("to remove any ORDER BY clause that could slow down the query"),
then applies filters. The shared port `sqlrepo::Repository::count`
(`rust/crates/nd-db/src/sqlrepo.rs:294`) drops order/limit/offset but never reinstalls
that reset leg, so every caller depending on it is a text-level gap. Value impact is
nil (an ORDER BY cannot change a single aggregate row); treat it as wire-shape parity,
not a correctness bug.

Measured per renderer (`file:line` = renderer, `g` = committed golden):

| repo | count renderer | reset `ORDER BY t.id`? | golden | verdict |
|---|---|---|---|---|
| transcoding | `transcoding_repo.rs:130` | **yes (fixed 2026-11-18)** | `:648` | MATCH after fix |
| radio | `radio_repo.rs:238` | yes (`:248`) | `:741` | MATCH |
| library | `library_repo.rs:240,:440,:699` | yes | `:1127+` | MATCH |
| player | `player_repo.rs:817` | yes (`:825`) | `:1587+` | MATCH |
| folder | `folder_repo.rs:353,:390` | yes | `:1475+` | MATCH |
| playlist | `playlist_repo.rs:300` | yes | `:1390` | MATCH (filters/options gap tracked separately) |
| artist | `artist_repo.rs:504` | no | `:1820` | GAP (renderer asserts the no-ORDER-BY shape) |
| album | `album_repo.rs:338` | no | `:1658` | GAP |
| mediafile | `mediafile_repo.rs:318` | no | `:1901` | GAP |
| playlist_tracks | `playlist_track_repo.rs:234,:247` | no | `:801` | GAP |
| plugin | `plugin_repo.rs:83` | no | none | GAP |
| scrobble | `scrobble_repo.rs:109` | no | `:173` | GAP |

- **Also found (2026-11-18 review):** `rust/crates/nd-db/src/library_repo.rs:84` and `:410` claim
  `conf.Server.DevOffsetOptimize` has a Go default of `0`, making the rowid pagination rewrite
  unreachable. It does not: the field is `conf/configuration.go:145` with viper default `50000`
  (`:1129`, and `docs/rust-port/config-contract.md:196` records it), and the Rust config already
  carries `dev_offset_optimize: 50000` (`nd-conf/src/config.rs:746`, key at `keys.rs:535`).
  `mediafile_repo.rs:58` states the correct default. So `optimizePagination`’s rewrite is live in Go
  for offsets above 50000 and is unported in the read lanes — fix the two library comments and decide
  whether to port the rewrite, in a bounded package with its own executed test.
- Do **not** bulk-edit these renderers to green: each golden is a committed contract, so
every change needs its own package with the expectation updated and the Go method cited
(`persistence/<repo>_repository.go` -> `count`). The shared fix belongs in
`Repository::count` (or a documented `count_reset_order` helper) plus all goldens in one
reviewed package, not in twelve separate edits. `mediafile_repo.rs:344` (`CountBySuffix`)
and `player_repo.rs:793` (`CountByClient`) are N-A — Go builds those with `newSelect`,
not `count`.

## Executed-read sort gate audit (2026-11-18, at `f8ceeb8f`)

Go parses rest options **inside** every REST read — `grep -l parseRestOptions
persistence/*.go` hits album, artist, genre, library, mediafile, player, playlist,
playlist_track, plugin, radio, scrobble, share, sql_restful, sql_tags, transcoding,
user. So `sanitizeSort` (`sql_restful.go:69-86`) always runs before an ORDER BY is
rendered. The Rust executed reads do not all mirror that:

Measured against Go (`library_repository.go:325`/`:312`, `radio_repository.go:121`/`:113`,
`player_repository.go:114`/`:103`, `sql_tags.go:224`/`:213`). Status after `62789ba2`:

| repo | executed read | gate |
|---|---|---|
| transcoding | `read_all` `transcoding_repo.rs:566` | **gated inside** (`f8ceeb8f`) |
| player | `read_all` `player_repo.rs:1167` | **gated inside** via `Options::from_rest` (`62789ba2`) |
| library | `get_all` `library_repo.rs:987` | **gated inside** (`62789ba2`) |
| tag | `get_all` `tag_repo.rs:620` | **gated inside** (`62789ba2`) |
| radio | `get_all` `radio_repo.rs:606` | **gated inside**; new `parse_rest_options`/`sanitize_sort`/empty `SORT_MAPPINGS` (`62789ba2`) |

`runtime_rest_sort_gate.rs` executes six tests over the migrated schema (three hostile
arms per repository, stable order, no ORDER BY leg, whitelisted sorts still order, counts
invariant). Each repository also gained a REST-shaped `count`; the raw `count_all*` keep
already-parsed options because `count` drops the page and installs `<table>.id`
(`sql_base_repository.go:568-569`). Library is the one place a caller sort really renders
ahead of the reset leg — `CountAll` forwards `newSelect`'s output
(`library_repository.go:291-292`) — confirmed with a squirrel probe:
`ORDER BY library.id, total_songs asc` style output, no bound argument.

Impact: an unsanitized `QueryOptions` handed to an executed read puts the caller's sort
string straight into `ORDER BY`. On `transcoding` that was a measured information leak
(masked `command` readable through row order); the other tables exposed ORDER BY
expressions over their own columns, so lower severity, same shape. All five now parse
internally, raw renderers stay untightened (Go's renderer accepts whatever `applyOptions`
hands it, `sql_base_repository.go:157-158`) and carry the WARNING contract.

## D-OPEN probe: rusqlite ignores go-sqlite3's DSN pragmas (2026-11-18, refines `5ebb8110`)

Historical pre-DB-03 probe (the measured unconfigured-handle rows below are not
the current configured `schema::open` behavior). This workspace uses SQLite 3.46.0
BUNDLED via rusqlite 0.32.1/libsqlite3-sys 0.30.1; the old nonbundled diagnosis was
incorrect. See the implementation/adjudication section below for current policy.

| open argument | file actually created | journal / busy_timeout / synchronous |
|---|---|---|
| `…/navidrome.db?cache=shared&_busy_timeout=15000&_journal_mode=WAL&_foreign_keys=on&synchronous=normal` (Go's `DefaultDbPath` shape, `consts/consts.go:14`) | `navidrome.db?cache=shared&_busy_timeout=…&synchronous=normal` — the query string is part of the filename | `delete` / 5000 / 2 |
| `file:<abs>/navidrome.db?_journal_mode=WAL&_busy_timeout=15000&synchronous=1` | `navidrome.db` (URI parsed correctly) | still `delete` / 5000 / 2 |
| `file:c.db?mode=memory&cache=shared` | real URI params **are** honored (`journal=memory`) | — |

Conclusion, replacing the earlier "pass a `file:` URI and the pragmas apply" framing:
`_journal_mode`, `_busy_timeout`, `_foreign_keys` and `_synchronous` are go-sqlite3
conventions, not SQLite URI parameters; bare `synchronous` is ignored by Go too.
rusqlite honors genuine URI parameters (`mode=`, `cache=`), but explicit
per-connection PRAGMAs are necessary for Go conventions. The current implementation
below applies these plus `_cache_size`; this historical probe predates that code.
Measured that explicit statements do work on this handle: `PRAGMA journal_mode=WAL` ->
`wal`, `busy_timeout=15000` -> 15000, `synchronous=NORMAL` -> 1.

Two corrections to earlier notes: (1) `PRAGMA busy_timeout=N` returns a row, so it must be
read with `query_row`, not `execute` (`execute` fails with `ExecuteReturnedResults`);
(2) `PRAGMA foreign_keys` reads **1** on every handle here, including
`rusqlite::Connection::open`. Go gets FK=1 for its default DSN from the DSN's own
`_foreign_keys=on` pragma (`netgo,sqlite_fts5` build tags, `Makefile:5` — no
`sqlite_foreign_keys` tag), and FK=0 for a bare path. Rust's default is now established:
`nd-dbschema/Cargo.toml:14` builds rusqlite 0.32.1 with `bundled`, so SQLite comes from
`libsqlite3-sys` 0.30.1, which passes `-DSQLITE_DEFAULT_FOREIGN_KEYS=1`
(`libsqlite3-sys-0.30.1/build.rs:123`), and `PRAGMA compile_options` on the built handle
reports `DEFAULT_FOREIGN_KEYS` alongside SQLite 3.46.0. It is a compile-time default
enforced explicitly by the port when a DSN names FK, not a build-tag effect.

D-OPEN options for the user: (a) typed `DbOpenOptions { journal_mode, busy_timeout,
synchronous, foreign_keys }` applied as explicit pragmas with read-back per handle —
reproduces Go's effective state and is testable; (b) literal DSN pass-through — would still
require translating go-sqlite3 conventions into pragmas, so it is really (a) plus a parser.
Also unresolved: `db.rs::NewSqliteDb` self-compares `db` to `db` (`:100-103`), so Go's
"one file per DB in a process" isolation does not hold; fix before DB-backed Go oracle
fixtures are relied on.

## Read-lane inventory and remaining gaps (2026-11-18)

Executed-query count per repo (`grep -c execution::query`): library 7, folder 5,
player 5, artwork_queue 5, tag 4, user 5, transcoding 3, mediafile 2, playlist_track 2,
radio 2, scrobble_buffer 2, album 1, artist 1, property 1. **Zero executed reads:**
artwork, genre, playqueue, plugin, scrobble, share, smart_playlist, user_props.
Note `annotation_repo.rs` does not exist while `persistence/` has no annotation file
either — annotations ride album/mediafile (`DB-15`), so no gap there.

Near-term order after this commit, all DB-owned **(superseded 2026-11-19: DB-01 LANDED `f57ae050`,
DB-03 / D-OPEN LANDED `4a8aac9f`, DB-00 LANDED `48e65806`/`ec84c176`, DB-15 render-only LANDED `ab466928`, DB-02 LANDED
`6d9d984d`/`74ebd81c` — **no lane is in flight**; read
`../subplans/README.md` and `../next-work.md`'s current section for the live order)**:
1. **Sort-gate parity** for player, library, tag, radio — **done** at `62789ba2`.
2. **DB-01** missing-state bind repair — READY then, no prerequisites, and G0 gates
   DB-07/14. Unrelated to the gates, so it can run in a separate lane only if the
   checkout is free; otherwise keep it serial after the gate work.
3. **DB-03 / D-OPEN** — DECISION REQUIRED then. This is where the reproduced
   DB-path/pragma divergence (`docs/rust-port/subplans/db-runtime.md` above the DAG and
   `current-status.md`) gets resolved: a `DbOpenOptions` typed open plus effective-state
   read-back per handle. Needs the user's call on defaults (WAL for file DBs, FK on,
   busy_timeout 15000) before launch, and it is a `nd-dbschema`+`nd-db` change.
4. **Count-renderer ORDER BY parity** — renderer-only cleanup across
   artist/album/mediafile/playlist_tracks/plugin/scrobble; one reviewed package with the
   goldens, not twelve edits. Low priority: nil value impact.
5. **Then mutations** (DB-04..DB-10), which is the real critical path into #17.
6. **Cross-cutting follow-up (not DB-owned):** `nd-stream` lib test
   `the_poller_exits_on_teardown_even_when_the_client_never_hangs_up` occasionally takes
   ~30s against a 3s deadline under full-workspace load. Reproduced as a single failing
   binary in one run, clean in the other three runs and isolated. Needs its own
   investigation in `nd-stream`, not a DB change.

## Coverage audit: renderers with no executed coverage (2026-11-18, parent-measured)

Two independent measurements, both mechanical, so the next lanes target real gaps:

**(1) Repos with zero executed reads** (`grep -c 'with_connection_sync\|prepare\|query_row\|execute('`
per module): `genre_repo` 0, `scrobble_repo` 0, `scrobble_buffer_repo` 0 (its entry model hydrates
but nothing runs), `smart_playlist_repo` 0, `user_props_repo` 0 (it has 6 exec calls but zero
read-path coverage), `playqueue_repo` 0, `artwork_repo` 0, `artwork_queue_repo` 0 (hydrates via
`RowGet` inside `runtime_hydrate_folder_artwork_queue.rs`, but its own statements never execute),
`tag_repo` 0, `plugin_repo` 0 (its renderers are callable — `lib.rs:35` exports the module — but no statement can run against the shipped schema because the table is dropped). Executed today: share 8, property 1,
radio 1, library 1, plus user/player/transcoding/album/artist/mediafile/folder/playlist paths.

**(2) Public functions never named by any test** (script over `src/*.rs` vs `tests/*.rs`):
303 `pub fn` total, 176 named somewhere. Modules with large blind spots:
`sql_annotations.rs` 19/0 and `sql_bookmarks.rs` 9/0 — **zero** test coverage of any kind;
`criteria_sql.rs` 7/22; `sql_search*` 3/23; `sql_tags.rs` 2/15; `sql_participations.rs` 5/14;
`sql_restful.rs` 6/18; `sqlrepo.rs` 8/17; `playqueue_repo.rs` 1/6; `artwork_repo.rs` 1/10;
`smart_playlist_repo.rs` 2/7; `scrobble_buffer_repo.rs` 5/13; `hydrate.rs` 26/36 (post-scan hooks
`folder_post_scan`, `playlist_post_scan`, `playlist_track_post_scan`, `scrobble_buffer_post_scan`,
`row_to_*` untested).

**Scope limits, stated honestly.** (a) R2's probe covered 180 renderer names / 189 statement
records — 173 non-plugin complete-renderer names — and does **not** prove global exclusivity: it
omits complicated signatures, return types, and multi-shape inputs. (b) My count above measures
whether a function is *named* in a test, not whether its SQL executes; at least three reads are
executed that my grep attributed elsewhere (`mediafile_repo::get_all_ids_sql`
`runtime_hydrate_album_mediafile.rs:923-928`, `album_repo::get_all_ids_sql` `:1444-1453`,
`playlist_repo::load_tracks_sql` `runtime_hydrate_playlist_track_scrobble.rs:631-633`). Treat
these as an inventory of suspected gaps to confirm per lane, not as a certified coverage number.
(c) The two annotation defects have unit-test *string* assertions only and no non-test source
references, which is why they never surfaced.

**Reading:** the annotation/bookmark seams are both untested and (per the R2 audit) partly without
a Go counterpart, so they are the highest-value cleanup, not a coverage gap to fill with tests.
For the read-only repos, the gap is that no lane has executed their statements against
`apply_fresh_schema` — DB-04..DB-17 own the mutation half; a read-parity lane should cover
genre/scrobble/scrobble_buffer/smart_playlist/playqueue/artwork/user_props reads with Go-cited
fixtures. Do not schedule that lane while DB-03/DB-00 are writing in this checkout.

## D-OPEN DECISION (2026-11-18, user-approved: option a)

**Decision:** typed open options applied as explicit per-connection PRAGMA statements, with
effective-state read-back before a handle joins the pool. Literal DSN pass-through is
rejected as the mechanism, but the **config value stays Go-compatible**, so the loader must
translate it rather than require users to rewrite it.

Historical DB-03 proposal, superseded by the implementation/adjudication below
(in particular default-vs-driver state, exact URI forwarding, and typed admission):
1. `DbOpenOptions { journal_mode, busy_timeout, synchronous, foreign_keys }` with Go's
   effective defaults: WAL for file-backed DBs, `busy_timeout = 15000`,
   `synchronous = NORMAL` (1), `foreign_keys = on`. `:memory:` keeps `journal = MEMORY`
   semantics (SQLite rejects WAL on a memory DB, so the translation must not force WAL there).
2. `effective_path` must stop handing SQLite a filename that includes the query string. It
   parses Go conventions `_journal_mode`, `_busy_timeout`, `_foreign_keys`, `_synchronous`
   and `_cache_size` into options; bare `synchronous` is ignored. Genuine file: URIs
   retain their ORIGINAL query; non-URI queries are stripped (default-cache exception).
   silently dropped. `nd-conf`'s `DEFAULT_DB_PATH` (`rust/crates/nd-conf/src/consts.rs:16`,
   mirroring `consts/consts.go:14`) is the reference input and must parse to exactly the
   defaults above with path `navidrome.db`.
3. Pragmas are applied per handle **after** extension registration and **read back**; a
   handle whose effective state differs from the request fails pool creation. Remember
   `PRAGMA busy_timeout=N` returns a row — use `query_row`, not `execute`
   (`execute` → `ExecuteReturnedResults`).
4. Migration sequencing must not lose the settings: Go disables FK around migrations and
   re-enables it (`db/db.go:85-91`), and the pragma state is per-connection, so verify each
   pooled handle ends with `foreign_keys = on` regardless of what migration ran on it.
5. Corrected build evidence: rusqlite 0.32.1 uses BUNDLED SQLite 3.46.0 via
   libsqlite3-sys 0.30.1, whose build.rs:123 sets `SQLITE_DEFAULT_FOREIGN_KEYS=1`.
   This explains FK=1 on an unconfigured handle; it is not Go's default.
6. `Pool::new` (`rust/crates/nd-db/src/pool.rs:120-135`) passes only `config.path` today;
   thread the options through `PoolConfig`/`SQLStore::open` (`store.rs:188`) without
   changing the public `with_connection_sync`/`with_tx_sync` surface.

Out of scope for DB-03: the `NewSqliteDb` per-DB isolation note — that symbol does not
exist in this checkout (`grep -rn 'NewSqliteDb' --include='*.go' .` is empty; Go uses the
`db.Db()` singleton at `db/db.go:41-68` and `tests/harness/harness.go:52-58` sets
`conf.Server.DbPath` to a temp file + `?_journal_mode=WAL` with `SetMaxOpenConns(1)`). The
earlier `db.rs:100-103` note was a bad citation and must be corrected, not implemented.

## D-OPEN as implemented (2026-11-18, DB-03 + review adjudication)

Point 1 of the contract above said "Go's effective defaults: WAL for file-backed DBs,
15000, NORMAL, FK-on". That conflated two different things, and the review corrected it.
Measured against go-sqlite3 v1.14.52 (`sql.Open` + `SELECT 1` + pragma read-back):

* the **driver's own** state for a path nobody configures is journal mode UNSET
  (`journalMode` empty, `sqlite3.go:1236`, so no `PRAGMA journal_mode` is exec'd),
  `busy_timeout = 5000` (`:1231`, exec'd `:1665`), foreign keys UNSET
  (`foreignKeys = -1`, `:1234`, so no `PRAGMA foreign_keys`, `:1851-1853`),
  `synchronous = NORMAL` exec'd unconditionally (`:1241`, `:1906`). A bare path
  therefore reads `delete` / 5000 / sync 1 / fk 0.
* **Navidrome's WAL / 15000 / NORMAL / FK-on** comes from `consts.DefaultDbPath`
  (`consts/consts.go:14`, mirrored at `rust/crates/nd-conf/src/consts.rs:16`), which
  `conf/configuration.go:404-406` substitutes **only when `DbPath` is empty**. It is a
  config preset, not a driver default, and a user-configured custom `DbPath` gets the
  driver state plus whatever its own query asks for.

### The two grammars, not one normalized query

1. `DbOpenOptions { journal_mode: Option<JournalMode>, busy_timeout: u64,
   synchronous: Synchronous, foreign_keys: Option<bool>, cache_size: Option<i64> }` separates unnamed settings
   (no pragma/read-back assertion) from requests. The memory journal expectation is
   inferred from the decoded URI path and final ordered `mode` value, not Go's map.
2. Go parses a query only if the first `?` index is >= 1 (`sqlite3.go:1247-1250`):
   `?_fk=off` is a literal filename. `url.ParseQuery` percent-decodes names and values,
   maps `+` to space, and supplies the FIRST value per decoded name. Alias presence
   overrides canonical presence regardless of order, even if the alias value is empty
   (`:1338-1345`, `:1401-1408`, `:1438-1445`, `:1538-1545`). Names are case-sensitive;
   the four convention values are case-insensitive. This map extracts conventions
   and the initial zVfs argument; it does NOT reconstruct the SQLite URI.

   For an accepted `file:` DSN, SQLite receives `file:<path-as-written>?<ORIGINAL query>`,
   byte-for-byte: no re-encoding, deduplication, or '+' conversion. Underscore
   conventions stay in the URI (SQLite ignores them; the driver options apply them).
   SQLite percent-decodes ONCE (`sqlite3.c:182839-182870`) with '+' left literal, then
   processes ALL ordered mode/cache/vfs occurrences (`:182903-182965`). Later values
   overwrite earlier ones where allowed; invalid intermediate transitions still fail.
   Thus `mode=rwc&mode=ro` opens read-only, but `mode=ro&mode=rwc` fails with
   `access mode not allowed: rwc`. `mode=memory&mode=rwc` creates a real file on the
   delete journal, while the reverse stays in memory. `cache=shared&cache=private`
   is private. URI `vfs=` OVERWRITES the zVfs argument: `vfs=unix&vfs=not-a-vfs`
   fails, while the reverse opens. Go's first-value rule is NOT SQLite's URI rule.
   This loop belongs to `sqlite3ParseUri` (declared `sqlite3.c:182763`), not a VFS
   directory opener. Other readers are platform-specific: `exclusive` is Windows VFS
   (`sqlite3.c:51816`); `rbu_memory` is RBU VFS (`:223262`), not Unix pager state.
   Decoded NUL discards the rest of its path/name/value component (`:182849-182863`).
   Memory classification mirrors that truncation only; the URI stays verbatim.

   **Parameter policy** (matches `parse_open_input`'s actual branches):
   * `_journal_mode`/`_journal`, `_busy_timeout`/`_timeout`, `_synchronous`/`_sync`,
     `_foreign_keys`/`_fk`: resolve alias/first-value precedence into open options; an
     empty winning value is unset. Retained but inert to SQLite in a file: URI.
   * `_cache_size`: first value, empty means unset; otherwise signed base-10 int64
     validation (`sqlite3.go:1572-1577`) and verbatim normalized-int64 rendering into
     `PRAGMA cache_size` (`:1917-1921`). Read-back is compared to SQLite's resolved
     value: exact at both i32 endpoints and within that range, otherwise 0 (including
     2147483648, -2147483649, 4294967297, i64::MAX and i64::MIN). Shipped
     `rust/Cargo.lock:667-668` resolves libsqlite3-sys **0.30.1**: its SQLite
     `PragTyp_CACHE_SIZE` is `sqlite3.c:139581`, calling `sqlite3Atoi` at `:139586`.
     `sqlite3Atoi` (`:35917`) initializes x=0 and discards `sqlite3GetInt32`'s result;
     GetInt32 (`:35858`) leaves x untouched at overflow exits `:35899` (i>10) and
     `:35903` (v-neg>2147483647). This is SQLite TEXT-to-int conversion, NOT
     driver-side low-word truncation, and is distinct from the earlier `_journal_mode`
     URI pointer-arithmetic truncation issue. Mismatch messages retain requested
     (including its sign), resolved and actual values. EffectiveState/token admission
     compares the actual resolved state, so a changed cache size still fails.
     None emits no pragma/no assertion; it is NOT a state-free driver cache.
   * `vfs`, `cache`, `mode`, `immutable`, `nolock`, `psow`, `readonly_shm`, `exclusive`,
     `modeof`, `8_3_names`, `rbu_memory`: reject an empty FIRST Go-decoded value;
     otherwise SQLite interprets the untouched URI query. A non-URI query is stripped.
     First Go-decoded `vfs` additionally configures zVfs for both shapes.
   * `journal_mode`, `journal`, `busy_timeout`, `timeout`, `synchronous`, `sync`,
     `foreign_keys`, `fk`: accept any value, ignore, retain in a file: URI.
   * `_txlock`, `_loc`: empty accepted; otherwise validate transaction grammar or ASCII
     zone-name shape, configure no SQLite state, retain but inert in a file: URI.
     Host zoneinfo existence is deliberately not checked (`_loc=UTC/Paris` may be
     accepted here when Go rejects it).
   * `_auto_vacuum`/`_vacuum`, `_case_sensitive_like`/`_cslike`, `_defer_foreign_keys`/
     `_defer_fk`, `_ignore_check_constraints`, `_locking_mode`/`_locking`, `_query_only`,
     `_recursive_triggers`/`_rt`, `_secure_delete`, `_writable_schema`, `_mutex`,
     `_stmt_cache_size`, `_auth`, `_auth_user`, `_auth_pass`, `_auth_crypt`, `_auth_salt`:
     resolve the winning canonical/alias name by PRESENCE before any validation.
     The loser's value is never inspected. An empty winner configures nothing, except `_auth`,
     whose presence alone triggers rejection (Go also fails without `_auth_user`, wording
     differs). Nonempty winners are Go-grammar validated THEN refused. Thus
     `_auto_vacuum=nonsense&_vacuum=` opens; `_vacuum=nonsense&_auto_vacuum=` errors.
     `_stmt_cache_size` remains a driver-side statement-cache option.
   * Any other name (including uppercase or misspelled conventions and `verbose`) is
     rejected as unknown.

   Explicit input restriction: percent-decoded query names/values must be UTF-8,
   including inert values (`_loc=%FF`, `journal_mode=%FF`). Go permits arbitrary bytes;
   the port rejects these with `not valid UTF-8` rather than replacing bytes silently.
   Raw semicolons (Go's ParseQuery error), malformed escapes, bare items without '=',
   empty names, and '#' fragments in file: URIs are also rejected. Empty pairs (&&)
   are skipped. Plain filenames retain literal '#' and percent escapes.
3. Pragmas are applied per handle after extension registration, in go-sqlite3's order
   (busy `:1665`, FK `:1852`, journal `:1866`, synchronous `:1906`, cache `:1917-1921`), and the effective
   state is read back; a mismatch fails pool creation. `PRAGMA busy_timeout` is read with
   `query_row` because it returns a row. The ORDER is not observable on this build —
   measured: `PRAGMA journal_mode=wal` changes neither `busy_timeout` nor `synchronous`
   (SQLite applies `SQLITE_DEFAULT_WAL_SYNCHRONOUS` to a btree only when it differs from
   `SQLITE_DEFAULT_SYNCHRONOUS`, `sqlite3.c:73776`/`:73875`, and `PRAGMA compile_options`
   reports both as 2) — so busy-before-journal is kept because it is Go's order and stays
   correct if SQLite ever gives WAL its own defaults, not because this SQLite needs it.
   `busy_timeout`'s numeric domain is reproduced rather than rejected
   (`SQLITE_BUSY_TIMEOUT_MAX_MS`): go-sqlite3 sends the value as pragma TEXT
   (`sqlite3.go:1665`) and SQLite's `sqlite3Atoi` returns 0 — with no error — above
   `INT_MAX` or past 10 digits (`sqlite3.c:141340`, `:35858-35910`), so 2147483648,
   4294967297, 10000000000 and every negative value end at 0 on BOTH drivers (measured),
   while 2147483647, 777 and `000000000000000000005` (Go normalizes to `5`) are applied
   verbatim. Only what go-sqlite3's own `strconv.ParseInt` rejects
   (`9223372036854775808`, `0x10`, `1_000`, `12.5`, `+5`, `abc`) is an error here,
   mirroring Go's `Invalid _busy_timeout`. So no DSN is rejected, and no read-back
   fails, because of its `busy_timeout` value: `%2B5` decodes to `+5`, which Go's
   `ParseInt` accepts (a leading sign is valid) and this port resolves to 5, while a RAW
   `+` decodes to a space and fails on both sides. `_loc` is the one place where the port
   cannot match Go exactly — it validates the zone-name SHAPE but cannot consult the
   host's zoneinfo, so a well-formed name that is not installed (`_loc=UTC/Paris`, which
   Go rejects with `not a directory`) is accepted; that parameter configures no SQLite
   state, so nothing about the handle differs.
4. `Pool::new` returns `(Pool, Initializer)`: the held-out handle is wrapped in an
   unforgeable, pool-bound token (private fields, immutable Deref only). Schema
   initialization takes `&Connection`, so no mutable/extracting accessor is needed.
   `Pool::put_back` accepts ONLY this token, never a raw Connection, and compares an
   internal shared pool identity (not DSN or pragma equality). Thus foreign read-only
   or private-memory handles are structurally impossible to admit. A wrong-pool token
   is closed and decrements its issuing pool's count. Effective-state comparison,
   including cache_size, and SEEDEDRAND/NATSORT execution defend mutated issued tokens.
   One `Arc<AtomicUsize>` counts idle + leased + issued-but-not-returned handles;
   token Drop/rejection closes and decrements, while queue/lease transfers do not.
   Capacity is structural, not enforced by unreachable arithmetic guards: new opens
   exactly N handles, only one pool-bound token is issued, transfers mint nothing,
   and foreign tokens cannot enter. Thus live>N and idle>=N while a valid token is
   held are unreachable. The misleading over-capacity test was renamed to
   `r6_f2_put_back_refuses_foreign_owner_and_updates_live_counts`; it exercises owner
   rejection and counter side effects, not a capacity branch. Real capacity is
   exercised by `r5_h3_live_capacity_includes_lease_and_transaction` (blocked checkout).
   No public lease API yields `&mut Connection`: transactions borrow the lease and
   return a Transaction with immutable Deref, so handles cannot be substituted.
   `SQLStore::open` returns the same token, and exposes checked `put_back`,
   `initialize(token, closure)` and `idle_count` on its facade. Initialization errors
   or panics drop the token without queueing it. Size-1 store usability and full size-N
   idle counts are tested, as are wrong-pool tokens, live checkouts/transactions,
   dropped tokens, and failed initializer closures. Existing callers migrate the issued
   token then handle the checked return result; none must access SQLStore's private pool.
   FK error contract, verified against all five scenarios a reviewer probed with a
   rusqlite `hooks` authorizer in a scratch copy (`nd-dbschema` does not enable `hooks`,
   so the executed suite reproduces the same situations with mechanisms SQLite really
   has: `PRAGMA foreign_keys` inside an open transaction is silently ignored because the
   flag pragma's mask is cleared when `db->autoCommit == 0`, `sqlite3.c:139857-139861`,
   and `PRAGMA max_page_count=1` makes the next CREATE fail with `database or disk is
   full`):
   * DDL OK + restore denied → `Error::OpenConfiguration` naming the state the handle is
     actually left at (`foreign_keys=false`). This used to surface as `Error::Failed`.
   * DDL denied + restore OK → `Error::Failed` carrying the DDL cause, FK restored.
   * DDL denied + restore denied → `Error::OpenConfiguration` naming both the DDL failure
     and the leaked state.
   * disable denied (migration called inside a transaction) → `Error::OpenConfiguration`
     naming the disable step; the handle never lost enforcement.
   * nothing interfered → `Ok(())`, and the handle ends in its OWN configured FK state —
     a deliberately FK-off handle stays off, which Go's unconditional
     `foreign_keys=on` (`db/db.go:85-91`) would have overwritten. That unconditional exec
     is unconditional because Navidrome's *default* DSN asks for FK on; a custom
     `DbPath` without `_foreign_keys` leaves go-sqlite3 at `foreignKeys = -1` and its
     handle at FK off.
   The public test probe's read-only contract is not enforced by its type: a mutating
   probe may return Ok if FK restoration succeeds. Panics are caught with
   `catch_unwind(AssertUnwindSafe(...))`; FK restore is attempted and READ BACK before
   resuming the original unwind payload. Only if restore fails/mis-reads does the
   payload become `(original Box<dyn Any + Send>, Error::OpenConfiguration)` naming
   the leaked state. Both paths are executed in tests; BEGIN inside the panicking
   probe forces restore to be ignored, requiring no `hooks` feature.
   Accepted facade divergence: StoreError::Open(String) erases the schema error's
   InvalidDsn/OpenConfiguration discriminant and source chain. The FULL message
   must survive; no error is swallowed and a failed token is closed (live=0, idle=0).
   Callers requiring typed schema errors must inspect them before mapping.
   Two mapped-error tests distinguish the fault-injection phases without hooks:
   BEGIN **before** apply_fresh_schema prevents FK disable (FK remains true, restore
   succeeds, DDL creates no objects after rollback). BEGIN **inside the mid-DDL
   apply_fresh_schema_with probe**, after FK was disabled and a statement executed,
   prevents restoring ON (actual leaked FK=false, error names failed restore). The
   latter intentionally mutates the public test probe, violating its read-only
   contract for fault injection; it is NOT a claim that BEGIN-before-apply leaks FK.
   This covers the restore-failure branch in shipped tests without adding hooks.
5. Point 5 of the contract asked for FK's origin to be pinned honestly. It is a
   compile-time default, not an observed mystery and not go-sqlite3 behavior: the crate
   builds `rusqlite 0.32.1` with the `bundled` feature (`nd-dbschema/Cargo.toml:14`), so
   SQLite comes from `libsqlite3-sys` 0.30.1, which passes
   `-DSQLITE_DEFAULT_FOREIGN_KEYS=1` (`build.rs:123`). Go's handle for a bare path reads
   fk=0. The port leaves FK alone when the DSN names nothing, so that compiled default
   stays in charge exactly as it did before DB-03; `foreign_keys: Some(_)` is applied and
   verified.
6. `cache=shared` reaches SQLite only through a `file:` URI. go-sqlite3 strips the whole
   query for a non-`file:` DSN (`sqlite3.go:1599-1601`), so **Go's SQLite never receives
   `cache=shared` for Navidrome's default DSN** (measured: the default DSN and the same
   DSN minus `cache=shared` give byte-identical pragma state and file name). Forwarding
   it anyway for that DSN ONLY — and only `cache=shared` — is a deliberate choice of the
   D-OPEN decision above and of this round's measurements, not something inherited from
   DB-02, whose section does not establish a shared-cache requirement. The tradeoff is
   explicit and measured on this build: with a shared cache, two handles writing the same
   table fail immediately with `SQLITE_LOCKED_SHAREDCACHE` (extended 262, "database table
   is locked") instead of waiting out the busy timeout for `SQLITE_BUSY` (extended 5);
   `nd_db::store::is_busy` classifies **only** primary BUSY (5, including extended BUSY codes) as retryable, so
   LOCKED_SHAREDCACHE (262) fails immediately — asserted by `nd-db/tests/busy_retry_mapping.rs`'s shared-cache test and
   matching Go, whose `db.IsBusy` compares the primary code only (`db/db.go:137-140`, `persistence/persistence.go:218`).
   Preset matching runs on the DECODED, order-insensitive parameter set, so
   `navidrome.db?cache=shared&_busy%5Ftimeout=15000&_journal%5Fmode=WAL&_foreign%5Fkeys=on&synchronous=normal`
   gets the same options and the same forwarded `cache=shared` as the plain spelling
   (measured in Go: identical pragma state). Matching on raw items used to miss it and
   silently changed the lock class (262 vs 5) for an identical configuration. The shape
   caveat stands and is a documented divergence: a user-supplied
   `/data/navidrome.db` with that query matches too and gets the forwarded shared cache,
   which go-sqlite3 would not give it.
   File: inputs are NEVER rebuilt from the Go map. Measured modeof traps prove the
   distinction: literal `mask+a` (0711) and `mask a` (0755) coexist, and the URI with
   literal '+' copies 0711, just like `%2B`. `%26` names `mask&a` (0700), never a
   stray `mask` (0755); `%2526` names literal `mask%26a`, failing with disk I/O error
   when absent. SQLite performs exactly one decode.
   A non-URI path beginning with '//' stays a plain filename, not a synthesized
   authority. SQLite's authority rejection is `sqlite3.c:182815-182824`.
   Go extracts first-value `vfs` before stripping a non-URI query (`sqlite3.go:1595-1597`)
   and passes it to `sqlite3_open_v2` (`:1607-1614`), mirrored by rusqlite's
   `Connection::open_with_flags_and_vfs`. For URI inputs SQLite's ordered `vfs=`
   occurrences OVERWRITE that zVfs argument (`sqlite3.c:182903-182908`).
7. `:memory:` keeps Go's rewrite (`db/db.go:54-55`) as `file::memory:?cache=shared` with
   FK on, and the boundary is Go's exact test `Path == ":memory:"` — no query, no `file:`
   prefix. Measured with go-sqlite3, two handles per DSN: `:memory:` (rewritten) is
   SHARED; `file::memory:` and `:memory:?_fk=off` are NOT-SHARED, because they do not
   match the rewrite, so neither gets an injected `cache=shared` nor the FK-on default.
   In particular `:memory:?cache=shared` and `:memory:?_fk=off&cache=shared` stay
   PRIVATE because the non-URI query is stripped; `:memory:?mode=ro` is writable.
   `file::memory:?cache=shared` is shared, but `file::memory:?mode=ro` is read-only.
   `file:%3Amemory%3A?_journal_mode=wal` decodes to a memory path and accepts SQLite's
   memory-journal downgrade (no backing file).
   SQLite's bare `:memory:` (without the rewrite) is a private DB per handle; the
   shared-cache URI names one shared DB. `mode=memory` counts ONLY for a genuine
   `file:` URI: a non-URI `x.db?mode=memory` has its query stripped like any other
   non-URI DSN, so SQLite's option reader never sees it and a REAL file is opened
   (measured: journal `delete`, `PRAGMA database_list` names the file). SQLite fixes a
   memory DB's journal at `memory`/`off` (`sqlite3.c:64459-64464`), so only a WAL request
   is accepted as a downgrade; a DELETE/TRUNCATE/PERSIST request that reads back `memory`
   is reported.

Evidence lives in the executed suites, not in prose:
`rust/crates/nd-dbschema/tests/dsn_open_options.rs` (parse/precedence/URI/state, the
non-URI `mode=memory` case, the `:memory:` rewrite boundary with two handles per shape,
the untouched URI grammar via `modeof` permission bits, the `//`-authority case, percent-encoded
preset matching, the `busy_timeout` domain, and `vfs` handed to `open_with_flags_and_vfs`
on both URI and plain inputs),
`rust/crates/nd-dbschema/tests/fresh_schema.rs` (the five FK authorizer scenarios of the
error contract above), and
`rust/crates/nd-db/tests/runtime_pool_configuration.rs` (every pool handle, all leases
held concurrently, `PRAGMA database_list` file names, directory listings, and
`idle_count() == config.size` after initialization).
