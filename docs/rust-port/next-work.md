# Next body of work — task packet for the following session

State at time of writing (verified): #07 `nd-ffmpeg` code-complete (54 lib + 2 golden,
incl. `MockFFmpeg` port of `tests/mock_ffmpeg.go`); #04 `nd-dbschema` = fresh schema +
marker-guard migration runner green (10 tests), `optimize/backup/repair` NOT written;
#06 `nd-model` = DTOs green (5 tests), criteria DSL NOT written. Gate: fmt 0 diffs,
clippy clean per crate, 59 test groups / 0 failures.

## Pinned decisions (do not re-litigate)

1. **rusqlite, not sqlx, for all SQLite crates.** The plan docs never freeze a binding
   (`docs/**` mention sqlx zero times; rusqlite appears only as an example at
   `schema-contract.md:177`). The workspace declares `sqlx = { version = "^0.2", ... }`
   at `rust/Cargo.toml:53` but sqlx is absent from `Cargo.lock` — it never resolved.
   nd-dbschema already pins rusqlite 0.32 (bundled/collation/functions), which is the
   binding `schema-contract.md:177` requires (per-connection re-registration of
   `SEEDEDRAND`/`NATSORT` + FTS5 hooks). Delete the sqlx line when #05 starts.
2. **#04's optimize/backup/repair do NOT block #05.** Their only Go call sites are
   `scanner/controller.go:248`, `cmd/scan.go:136` (Optimize) and `cmd/root.go:284`,
   `cmd/backup.go:82` (Backup) — i.e. they gate #12/#14-style consumers, not the
   repository layer. What #05 DOES need from #04 is the FTS5 tables (already created
   by the fresh schema) and per-connection extension registration (already in
   `schema.rs:86`).
3. **`persistence/criteria_sql.go` (904L) is the only #06↔#05 touch point**
   (`issues.md:228-229`). Defer it until the #06 criteria DSL is green; the other
   23 repos + 8 `sql_*` helpers are #06-independent.
4. **23 `*_repository.go`, not 24** (`issues.md` says 24; disk has 23 — one is
   `mock_radio_repository.go`, oddly named). Fix the count in the ledger.
5. Go's `MockFFmpeg` (the in-process interface double) is ported; the container fixtures
   `test.{aiff,m4a,opus,wav,wma,ogg}` are pinned live. Still uncovered: the symlink
   chain (`tests/fixtures/symlink*`) and the `invalid-files` directory cases.

## Tasks, in build order (each bounded, one crate owner)

1. `nd-model` — finish the #06 criteria slice: `model/criteria/{criteria,json,sort}.go`
   (containers, JSON round-trip with `optionalConjunction` present-flag + the
   length-1 marshal rule, `OrderByFields`/`SortFieldNames`) + the 40 `It(...)` cases in
   `criteria_test.go` (all of them: `all`/`any` conflict rejection, `limitPercent`
   clamp/round-trip, `EffectiveLimit`/`ResolveLimit`/`IsPercentageLimit` 17 cases,
   `refreshDelay` parse/format/reject 8 cases, child-playlist id/path dedupe 8 cases).
   Needs `nd-utils` as a dep of `nd-model` (`parse_duration`/`format_duration` for
   `refreshDelay`). Transforms only — no rusqlite, no SQL.
2. `nd-db` — `SQLStore` ownership core from `persistence/persistence.go`
   (`:17-40` lazy repo builders, `:71`/`:171` construction, `:203-235`
   `WithTxRetry`: txMaxRetries=3, delay 5s, busy-retry) over rusqlite, reusing
   `nd-dbschema` `schema.rs:48 open` / `:86 register_extensions`. Reproduce
   `db.Db().SetMaxOpenConns(1)` contention semantics for the harness.
3. `nd-db` — `sql_base_repository.go:1-678`: dbx→Rust query builder, `toSQL()` `:358`,
   `logSQL()` `:661`. Biggest blast radius: land it before any leaf repo.
4. `nd-db` — the 8 `sql_*` helpers (1,567L) + the 6 repos named in the #05 Test block
   (`user, user_props, scrobble_buffer, scrobble, share, tag, transcoding`), using the
   `tests/mock_*_repo.go` specs on disk (21 files, no Rust fixtures exist yet).
5. `nd-db` — remaining leaf repos (album/artist/mediafile/library/folder/playlist/
   playlist_track/playqueue, then plugin/property/radio/artwork/artwork_queue/
   smart_playlist/genre) + e2e-suite-style SQLite harness (`e2e_suite_test.go:417`).
6. `nd-dbschema` — the still-pending `optimize.rs` (`db/optimize.go` 274L),
   `backup.rs` (184L), `repair.rs` (367L, incl. `repair.go:12/:102` ftsTables/VerifyFTS
   semantics). Independent of 2–5; slot it whenever the lane is free.
7. `nd-ffmpeg` tail — symlink-chain + `invalid-files` fixture cases for the live-probe
   path (small; do alongside 6).

## Gate (unchanged)

`cd rust && cargo fmt --all -- --check && cargo clippy -p CRATE --all-targets
--all-features -- -D warnings && cargo test -p CRATE` per crate; never workspace-wide
clippy. Commits GPG-signed (no gpgsign bypass), no AI co-author trailers.

## Style hazard (repeat offender)

rustc here is 1.99.1 and rejects Go-flavored pseudo-code outright: `a + b` string
concat, `+=` on `&str`, `.append()`, `std::map<...>`, `Vec.new()`, `assert(x, "msg")`,
missing `->` in signatures, map-literal struct construction. Workers must loop
write → `cargo build -q -p CRATE` → fix from compiler messages, and copy the
match-based `Result` handling style already in `nd-dbschema/schema.rs`.
