# Plan of record for the next body of work

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
- **Pin in the brief**: the pool mechanism (single `Mutex<Connection>` vs pool
  crate — single-owner matches Go's `SetMaxOpenConns(1)` best), which rusqlite
  errors retry vs fail (`SQLITE_BUSY`/`SQLITE_LOCKED` retry; everything else
  fail), and that bundled SQLite (rusqlite `bundled` feature) is what FTS5 +
  JSON1 run against.
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

- **rustc 1.99.1 rejects Go-flavored pseudo-code** (`a + b` concat, `+=` on
  `&str`, `.append()`, `std::map<...>`, map-literal constructors, missing `->`).
  Workers loop write → `cargo build -q -p CRATE` → fix from compiler messages.
  One writer per crate at a time.
- **Worker exhaustion**: executors have died mid-thought twice on this port.
  Slices are bounded so a dead lane loses a slice, not a phase. Verify with
  `ls`/`grep` before trusting a crew's "completed" report.
- **`Connection` is `!Sync`**: the pool choice in #05 slice 1 is load-bearing —
  pin it in the brief, not in a worker's judgment.
- **`bench/`, `docs/perf/`**: untracked artifacts of a prior session's perf run
  (Nov 2). Not mine; commit or remove — ask, don't decide unilaterally. Same
  class of leftover as `docs/rust-port/drafts/` if the #06 lane doesn't consume
  it.