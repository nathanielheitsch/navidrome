# Plan of record for the next body of work

## Current state (2026-11-10) — supersedes the 2026-11-04 boundary below

Measured gate at `a462b1f2` (parent-measured, integrated): fmt clean, workspace build clean, workspace
clippy `-D warnings` clean, and `cargo test --locked --workspace --all-targets --all-features
--no-fail-fast` = **1183 passed / 0 FAILED**. The earlier `29d9d9d0` figure — 1067 passed / 4 FAILED —
was `nd-ffmpeg` fixture paths hardcoded to a retired worktree, so the container fixtures executed
**zero** files off that host; `5e56f7e8` resolves them from `CARGO_MANIFEST_DIR` to the repo root. The
"969 passed / 0 failed" figure further down is a dated historical boundary, not green state.

**The Phase 1–4 plan below is accurate as a record of what was *shaped*, but shape ≠ wired.**
The runtime spine has started, but nothing boots yet:

- `nd-db` — execution seam (`execution.rs`, `store::SQLStore` sync + async verbs) and **one** executing
  repository (`user_repo`, with `UserRecord`/`UserRow` hydration and the `WHERE`-before-`GROUP BY`
  fix). The other ~14 repos only render SQL; general row hydration (`sql_base_repository.go:445,480`)
  exists only for `dbUser`. Pagination renders decimal literals with zero bound args, matching
  squirrel (`select.go:376-388,140-150`); `b78c01a2` fixed a crate-wide divergence where the port
  bound `LIMIT ?`/`OFFSET ?` as args. Offset-without-limit keeps the cited `LIMIT -1` deviation
  because squirrel's bare `OFFSET n` is a SQLite parse error. **Player repository landed**
  (`c8c71f8d`: `PlayerRepo`, `PlayerError`, `save`/`set_api_key`/`find_by_api_key`, 19 executed tests),
  so `/player` and API-key issuance have a store — nothing mounts it yet.
- `nd-dbschema` — `schema_ddl.rs` emits **no secondary indexes** (~107 in the Go migrations, 2 unique);
  `player_api_key_hash` UNIQUE is what settles API-key races, and `schema_diff.rs` compares only FTS5
  triggers/tables so it cannot catch this.
- `nd-tagreader` — reads `<media>.tags.json` sidecars with **no runtime producer** (only the Go
  golden-dump fixture test writes them), so #16 → #17 ingest is blocked upstream.
- `nd-auth` — credentials verified, not mountable; no router middleware.
- `nd-db` player repository — **landed** `c8c71f8d`, so `/player` and API-key issuance have a store to
  build on; auth middleware (A10) is the next dependency. `nd-stream` — Range/conditional matrix,
  bounded direct adapter, probe adapter, and process-backed
  transcode body landed; cache, limiter, and archive have not. `nd-cli` `main()` is empty and
  `nd-http`/`nd-api`/`nd-scanner`/`nd-mapping` are scaffold-only.

### Active lanes (user-approved 2026-11-10)

1. **DB runtime spine** (`rust/db-runtime`, A7/A8) — **LANDED** `3ce1b154` + `dee593b5`: execution seam,
   `SQLStore`, corrected user SQL, `user_repo` executed with hydration. Player repository (A9) and auth
   wiring (A10) are NOT started.
2. **`nd-ffmpeg`** (`rust/ffmpeg-readiness`, A2/A3/A4/A5/A11) — **LANDED** `5e56f7e8` + `8152ac5a`: portable
   fixtures, `process_group(0)` spawn + `kill_process_group` SIGKILL with ESRCH fallback, `STDERR_CAP`
   4096 with the overflow still consumed, `TEARDOWN_TIMEOUT_MS` bounded drain/reap. Grandchild-leak test
   verified non-vacuous (stubbing the group kill fails it with a leaked pid).
3. **Stream serving** (`rust/stream-serving`, A12) — **PARTIAL** `aa9b280e` + `a462b1f2`: Range/conditional
   matrix at `net/http` parity, bounded direct-file adapter, probe adapter, transcode body with explicit
   disconnect cancel. Remaining: transcoding cache, rate-limiting semaphore, archive, HTTP handler.
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
5. **Finish #15** — transcode cache + rate-limiting semaphore (`core/stream/limiter.go`), archive, and the
   HTTP handler that consumes Range/direct/transcode bodies.
6. ~~A real tag backend~~ (#08, A15) — **landed** `beaf63d7`, merged; gate 1270/0. `lofty = "=0.25.5"`
   (pure Rust, MIT/Apache-2.0, +11 new lock entries) reads MP3/FLAC/M4A/OGG with multi-values, comments,
   language-coded USLT/SYLT, TIPL/TMCL, MP4 atom precedence, pictures, and malformed-file skipping; five
   ffmpeg-synthesized fixtures plus eight Go media fixtures compared to their Go oracles. **Ingest (#16 → #17)
   is no longer blocked upstream.** Declared gaps to respect when wiring #16/#17: ASF/WMA unsupported;
   AAC-in-MP4 bit depth 0 vs Go's 16; bitrate estimates differ (FLAC 103/104, Vorbis 16/19 kbps);
   MPEG-frame-timestamp SYLT skipped. A fresh review is checking whether these surface honestly or silently
   persist wrong values. Note the exact pin: bumping lofty regenerates nothing automatically, and the Go
   goldens must be re-dumped if the go-taglib fork changes.
7. `nd-dbschema/src/repair.rs` (`db/repair.go`, 367L FTS5 verify/repair) — still deferred, its own lane.
8. Specify + build the #12 maintenance/logging half.
9. **First runnable slice** (`#27A`): fresh DB → scan a real track → authenticated API lookup → direct
   Range + forced transcode + client disconnect with **zero surviving child processes**. Full #27 needs
   share/playlists/artwork/external/quick-connect services (`server/nativeapi/native_api.go:38-55`);
   deferrable #32 must **not** gate this minimal slice.

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