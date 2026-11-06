# Handoff prompt — Rust port of Navidrome (Go → `nd-*` Rust workspace)

Paste everything between the `===PROMPT BEGIN===` / `===PROMPT END===` markers into the new session.

===PROMPT BEGIN===

You are continuing an existing multi-session task. Do not re-plan from scratch, do not re-do landed work, and do not widen scope. Repo root: `/Users/nathanielheitsch/emdash/worktrees/navidrome-ea7ae954/issue-planning-rust-sdhnn`. Go source at root (`persistence/*.go`, `model/*.go`, `utils/*.go`, `rest/*.go`). Rust workspace at `rust/` (26 `nd-*` crates, edition 2021, GPL-3.0). Ledger: `docs/rust-port/issues.md`. Plan of record: `docs/rust-port/next-work.md`. Schema contract: `docs/rust-port/schema-contract.md`.

## State (verify, do not assume)

Phase 3 is DONE and committed. Phase 4's `#05` nd-db **slice 4** is in flight and nearly done. Latest commit is `a137c46c docs(ledger): #12 nd-sched scheduler-half DONE …`. Untracked/uncommitted right now: the 15 slice-4 repo files in `rust/crates/nd-db/src/` (`album_repo.rs`, `artist_repo.rs`, `artwork_queue_repo.rs`, `artwork_repo.rs`, `folder_repo.rs`, `genre_repo.rs`, `library_repo.rs`, `mediafile_repo.rs`, `playlist_repo.rs`, `playlist_track_repo.rs`, `playqueue_repo.rs`, `plugin_repo.rs`, `property_repo.rs`, `radio_repo.rs`, `smart_playlist_repo.rs`) plus modified `lib.rs`, `sql_search.rs`, `sqlrepo.rs`.

Gate right now (`cd rust`): `cargo build -p nd-db` **compiles**, `cargo fmt -p nd-db -- --check` **clean**, `cargo clippy -p nd-db --all-targets --all-features -- -D warnings` **clean**, `cargo test -p nd-db` = **279 passed / 4 failed**. All 4 failures are in `artist_repo.rs`'s `mod tests`.

## First task — the 4 failing tests (finish in this order)

Run `cargo test -p nd-db --lib` and read the actual-vs-expected output. For each failure, decide whether the **code** or the **test expectation** is wrong, judged against the Go source — never bend code to a bad assertion. Starting diagnostics (verify, do not trust):

1. `get_index_key_matches_prefixes_case_insensitively` — `artist_repo.rs:1633`. Go `getIndexKey` (`persistence/artist_repository.go:310-321`) does `source = cmp.Or(SortArtistName, OrderArtistName)` **when `PreferSortTags` is on**, so a non-empty sort tag wins and the prefix never matches. The test passes `sort="foo"` while expecting the `order`-based result → the assertion contradicts Go. Fix the test (and keep the code's Go-faithful source selection).
2. `put_sql_update_leg_filters_cols_and_appends_updated_at` — `artist_repo.rs:1550`. The only diff is `external_url` vs `similar_artists` swapped. `model/artist.go:33-39` declares `biography, small_image_url, medium_image_url, large_image_url, external_url, similar_artists, external_info_updated_at` — that is `structs:` field order, which is Go's `toSQLArgs` order. The code already matches; the expectation is wrong.
3. `search_empty_query_hydrates_the_natural_order_rowid_page` — `artist_repo.rs:1822`, `sql.ends_with("GROUP BY artist.id ORDER BY _ranked._rn")`. Compare against `sql_search.go` two-phase execute (`executeTwoPhase` keeps the rowid-core's ORDER BY **inside** the inner query; `hydrateRowidPage` keeps the outer WHERE) and print the actual rendered tail before editing.
4. `search_fts_strategy_hydrates_the_rowid_page` — `artist_repo.rs:1938`: `args` order `Integer(10)` vs `Text("(beatles OR beatles*)")`. The MATCH param must bind **at the JOIN position** (`sqlrepo.rs` `join(table, on, args)` contract), ahead of the pagination args.

Then: re-run the crate gate (fmt, clippy `-D warnings`, test) → **all green** → commit slice 4 as ONE signed commit (`git add rust/crates/nd-db/src rust/crates/nd-db/tests docs/rust-port && git commit -m "feat(#05 slice 4): nd-db leaf repositories …"`) — no `-c commit.gpgsign=false`, no AI co-author trailer, then verify with `git cat-file commit HEAD | grep -q '^gpgsig'`.

## Then, in order

1. `criteria_sql.go` (`persistence/criteria_sql.go`, 904L) — the **last** item of slice 4: criteria→SQL generation, bridging nd-model's 40-test criteria DSL (done) into `smart_playlist_repo.rs`, whose seam is documented in its header. Port it as its own module, then per-crate gate + signed commit.
2. Update `docs/rust-port/issues.md` (#05 slice 4 DONE, `criteria_sql` state) with a signed `docs(ledger)` commit.
3. Phase-4 boundary workspace gate: `cd rust && cargo fmt --all -- --check && cargo clippy --all-targets --all-features -D warnings` (workspace-wide) and the full test run.
4. `#14` `nd-stream/src/decision` transcode decision tables (pure tables, no serving).
5. `#15` `nd-stream` serving half: `media_streamer`, `limiter`, `archiver`.

## Hard constraints (all still binding)

- Plain Rust, house style: snake_case functions, UPPER constants, `///` doc comments citing `GoFile.go:LINE-LINE` for every ported function, `///` on every public struct field.
- Errors: `thiserror` in libraries, `anyhow` only at the `nd-cli` boundary. Async: tokio v1. DI: explicit construction in `nd-cli/src/main.rs` — no ambient-context parameter in the SQL layer (Go's `ctx` becomes the caller's).
- rusqlite 0.32 (bundled/collation/functions/backup) is the schema-contract binding. sqlx is dead — do not reintroduce it.
- rusqlite `Connection` is `Send` but not `Sync`: the pool owns N = `max(4, available_parallelism())` connections behind one `Mutex`; no cross-thread leasing.
- `PRAGMA has('fts5')`/`has('json1')` do not exist in SQLite; macOS `/usr/bin/sqlite3` (3.54.0) lacks the FTS5 amalgamation — rusqlite bundled does have it.
- rustc 1.99.1 rejects Go-flavored pseudo-code: no adjacent string literals (use `concat!`), no `let Ok(..)` patterns, no `+` string concat, no map literals as struct constructors, patterns bind by value (`Some(ref x)`), `.to_string()` required at `String` call sites. Per-module loop: write → `cargo build -p nd-db` → fix from compiler messages.
- `cargo clippy --fix` applies nothing under `-D warnings`; fix by hand. Clippy traps already hit: no `#[must_use]` on a function already returning `Result` (`double_must_use`), `x == None` → `x.is_none()`, `(b'0'..=b'9').contains(&b)` → `b.is_ascii_digit()`, no `format!` without args (use `.to_string()`/`concat!`), no needless borrows, no collapsible match, no dead helpers.
- Never run filesystem-wide scans (`find /`, `locate`, `updatedb`) in a task — that hung a worker at 122s and killed it. Scope discovery to the project tree.
- Security gate is part of the definition of done: every slice-3/4 repository consuming `sql_restful.rs` `QueryOptions` **must** call `with_field_white_list` (nil-whitelist finding from slice 2, recorded in `next-work.md`). The security-reviewer pass is mandatory before declaring slice 4 done.
- Validation gate per crate: `cargo build -p CRATE` && `cargo fmt -p CRATE -- --check` && `cargo clippy -p CRATE --all-targets --all-features -- -D warnings` && `cargo test -p CRATE`. Workspace-wide at each phase boundary. No LSP diagnostics exist — the CLI toolchain is the gate; use `ast_grep` for structural searches.
- Tests use the Arrange/Act/Assert pattern and pin rendered SQL **and** bound args, so the Go oracle stays visible forever.
- One writer per file. `album_repo.rs` was previously damaged by a runaway write loop (29MB, `cursor_chunk_sql` duplicated 31,268 times); it was recovered with real content and verified to contain exactly one `mod tests` — re-verify each landed file for duplication before trusting any "completed" report (`ls -l`, `grep -c 'mod tests'`).

## Delegation

Subagent spawning was blocked in the previous session ("Peer-only/cancelled turn cannot authorize task creation"). It may work in the new session — if it does, use the standard shape `scout → planner → worker → reviewer(s)` with narrow single-file worker tasks (30-minute timeout per subagent; bank progress one module at a time) and keep ownership non-overlapping. If it is blocked again, work in-session module-by-module and say so; never fall back to an external CLI agent (`codex*`, `claude-code*`, `cursor-agent*`).

## Also still open (do not forget)

- `nd-dbschema/src/repair.rs` (367L FTS5 verify/repair) — explicitly deferred as its own lane, still not landed.
- `nd-sched/src/crontab.rs` orphan noted by an earlier audit; `nd-sched` scheduler half is DONE, the other half of #12 is not yet specified.
- Untracked `bench/results/team_20261104145948_*.json` and `docs/perf/perf-report-team_20261104145948_*.md` are stray artifacts — decide whether they belong in the tree or should be deleted before the next commit.
- `Cargo.lock` gains new deps at each crate addition; fold it into the cleanup commit, not a separate one.

Start by confirming the state above with `git log --oneline -3`, `git status --short`, `ls rust/crates/nd-db/src`, and one `cargo test -p nd-db` run. Report what differs from this brief before acting on it.

===PROMPT END===
