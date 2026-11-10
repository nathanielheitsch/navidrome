# Handoff prompt — Rust port of Navidrome (Go → `nd-*` Rust workspace)

Paste everything between the `===PROMPT BEGIN===` / `===PROMPT END===` markers into the new session.

> Path note (updated 2026-11-10): the worktree this document previously pointed at,
> `/Users/nathanielheitsch/emdash/worktrees/navidrome-ea7ae954/issue-planning-rust-sdhnn`, is **retired** and
> no longer exists. Use the paths below; resolve Go and Rust roots from `CARGO_MANIFEST_DIR` ancestors
> containing `go.mod` rather than from any absolute path baked into a doc or test.

===PROMPT BEGIN===

You are continuing an existing multi-session task. Do not re-plan from scratch, do not re-do landed work, and do not widen scope.

- Repo root: `/Users/nathanielheitsch/projects/barnstorm/navidrome`. Go source at root (`persistence/*.go`,
  `model/*.go`, `utils/*.go`, `rest/*.go`, `core/**`, `server/**`). Rust workspace at `rust/`
  (26 `nd-*` crates, edition 2021, GPL-3.0).
- Ledger: `docs/rust-port/issues.md`. Plan of record: `docs/rust-port/next-work.md`. Schema contract:
  `docs/rust-port/schema-contract.md`.
- Migration lanes run as Paseo worktrees under `/Users/nathanielheitsch/.paseo/worktrees/0xo97qr3/`, one writer
  per worktree; the parent session owns integration and final verification.

## State (verify, do not assume)

Measured at `15426bbc` (2026-11-10, after the A13 index pass), `cd rust`:

- `cargo fmt --all -- --check` — clean
- `cargo build --locked --workspace` — clean
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` — clean
- `cargo test --locked --workspace --all-targets --all-features --no-fail-fast` — **1230 passed / 0 FAILED**

Landed since the 1067/4 measurement: `dee593b5` (execution seam + `SQLStore` + executed `user_repo` with
hydration), `5e56f7e8` (portable fixture resolution from `CARGO_MANIFEST_DIR` to the repo root — the 4 failures were
hardcoded paths from a retired worktree), `8152ac5a` (`process_group(0)` spawn + `kill_process_group` SIGKILL,
`STDERR_CAP` with overflow still consumed, `TEARDOWN_TIMEOUT_MS` bounded drain/reap), `aa9b280e`/`a462b1f2`
(Range/conditional matrix, bounded direct adapter, probe adapter, process-backed transcode body),
`c8c71f8d` (player repository + API-key path, 19 executed tests), `15426bbc` (80 secondary indexes +
the 8 dropped columns + index/column drift guards).

Do **not** trust the historical "969 passed / 0 failed" (2026-11-04) or "1067 passed / 4 FAILED" boundaries as current.

**Shape ≠ wired.** Phases 1–4 completed *decisions and SQL shaping*; runtime integration is a separate axis:

- `nd-db`: has an execution seam (`execution.rs` + `store::SQLStore` sync/async verbs) and **one** executing
  repository (`user_repo`, 15 executed tests on per-test temp DBs). The other ~14 repos only render SQL; general
  row hydration (`sql_base_repository.go:445,480`) exists only for `dbUser`. Sibling offset-only renderings still
  pagination renders decimal literals with zero bound args, matching squirrel (`select.go:376-388,140-150`);
  `b78c01a2` removed a crate-wide divergence where the port bound `LIMIT ?`/`OFFSET ?` as args, so
  **`Repository::to_sql()` arg vectors no longer carry bounds** — never index pagination positions.
  Offset-without-limit renders `LIMIT -1` first (cited deviation: squirrel's bare `OFFSET n` is a SQLite parse error).
- `nd-tagreader`: consumes `<media>.tags.json` sidecars that **no runtime producer writes** (only the Go
  `ND_GOLDEN_DUMP=1` fixture harness does). Real tag backend is a prerequisite for #16 → #17.
- `nd-auth`: credentials/claims verified; no router middleware. The player repository is now ported
  (`c8c71f8d`: `PlayerRepo`, `PlayerError::{NotFound,PermissionDenied,ApiKeyValidation,Sqlite}`, `save`,
  `set_api_key`, `find_by_api_key`), but nothing mounts it.
- `nd-dbschema`: `schema_ddl.rs` now carries the **80 secondary indexes** (4 UNIQUE) and the
  8 late-window columns that were missing (`album.rg_album_gain`/`rg_album_peak`,
  `artist.uploaded_image`, `media_file.codec`/`probe_data`, `playlist.uploaded_image`/
  `external_image_url`, `radio.uploaded_image`) - landed `15426bbc`. `player_api_key_hash`
  UNIQUE therefore backs `PlayerRepo::save`'s race settlement, and `runtime_player_repo.rs`
  no longer hand-creates it. `schema_diff.rs` now diffs index rows (name + uniqueness) and
  per-table declared column sets, both verified non-vacuous by mutation; keep that coverage
  when the schema changes.
- `nd-stream`: Range/conditional matrix, bounded direct adapter, probe adapter, and transcode body exist; the
  transcoding cache, the rate-limiting semaphore (`core/stream/limiter.go`), and archive do not — today one ffmpeg
  process per call with no concurrency cap. `nd-cli/src/main.rs` `main()` is empty and
  `nd-http`/`nd-api`/`nd-scanner`/`nd-mapping` are scaffold-only, so nothing boots.

## Active lanes (user-approved 2026-11-10 — IN PROGRESS, not done)

| lane | branch | scope | asks | state |
|---|---|---|---|---|
| DB runtime spine | `rust/db-runtime` | execution seam, `SQLStore`, corrected user SQL, user hydration | A7, A8, A11 | **landed** `3ce1b154`, `dee593b5` |
| FFmpeg readiness | `rust/ffmpeg-readiness` | portable fixtures, process-group cancellation, bounded teardown | A2, A3, A4, A5 | **landed** `5e56f7e8`, `8152ac5a` |
| Stream serving | `rust/stream-serving` | Range/conditional matrix, direct adapter, probe adapter, transcode body | A12 | **partial** `aa9b280e`, `a462b1f2`; limiter/cache/archive pending |
| Ledger correction | `rust/ledger-correction` | this document + `issues.md` + `next-work.md` | A6 | **landed** `0309b5aa` |
| Player repository | next | `persistence/player_repository.go` incl. owner-only API-key rules | A9 | not started |
| Auth wiring | after player repo | middleware, `AuthResult` → claims, `/player`, API-key issuance | A10 | not started |

All lanes above are finished or dead (provider limits: `429 max connections reached: 4`, Codex usage limit,
OpenRouter budget). **Two of them reported commits that do not exist** (`5d1c207f`, `8842a742`, `6b289012`, and a
`procgroup.rs` module absent from every branch); one wrote files without committing them. Always verify a report
against `git log`, `git status`, and the actual files before integrating.

Before consuming another lane's output, read its current code. Streaming consumes `nd-ffmpeg`'s `new_ffmpeg_cmd()`
and its `cancel()`/`wait()` verbs only; `FFCmd`'s `pgid`/`closed`/`child` fields stay private.

## Then, in order

1. Integrate the four lanes; re-run the workspace gate (fmt, clippy `-D warnings`, full test) in the parent.
2. A **real tag backend** for #08 — until a producer exists, the scanner path cannot be exercised at all. Then
   #16 mapping → #17 scanner.
3. `nd-dbschema/src/repair.rs` (`db/repair.go`, 367L FTS5 verify/repair) — explicitly deferred, its own lane.
4. Specify + build the #12 `nd-sched` maintenance/logging half (scheduler half is done).
5. **First runnable slice (`#27A`)** — exit criteria: fresh DB → scan a real track → authenticated API lookup →
   direct Range + forced transcode + client disconnect leaving **zero surviving child processes**. Full #27 needs
   share/playlists/artwork/external/quick-connect services (`server/nativeapi/native_api.go:38-55`); deferrable
   #32 must not gate this minimal slice.

## Hard constraints (all still binding)

- Plain Rust, house style: snake_case functions, UPPER constants, `///` doc comments citing `GoFile.go:LINE-LINE`
  for every ported function, `///` on every public struct field.
- Errors: `thiserror` in libraries, `anyhow` only at the `nd-cli` boundary. Async: tokio v1. DI: explicit
  construction in `nd-cli/src/main.rs` — no ambient-context parameter in the SQL layer (Go's `ctx` becomes the
  caller's).
- rusqlite 0.32 (bundled/collation/functions/backup) is the schema-contract binding. sqlx is dead — never reintroduce it.
- rusqlite `Connection` is `Send` but not `Sync`: the pool owns N = `max(4, available_parallelism())` connections
  behind one `Mutex`; no cross-thread leasing. Blocking rusqlite calls go through `tokio::task::spawn_blocking`
  at the store boundary.
- `PRAGMA has('fts5')`/`has('json1')` do not exist in SQLite; macOS `/usr/bin/sqlite3` lacks the FTS5 amalgamation
  — rusqlite bundled does have it.
- rustc rejects Go-flavored pseudo-code: no adjacent string literals (use `concat!`), no `let Ok(..)` patterns, no
  `+` string concat, no map literals as struct constructors, patterns bind by value (`Some(ref x)`), `.to_string()`
  required at `String` call sites. Per-module loop: write → `cargo build -p CRATE` → fix from compiler messages.
- Clippy traps already hit: no `#[must_use]` on a function returning `Result`, `x == None` → `x.is_none()`,
  `(b'0'..=b'9').contains(&b)` → `b.is_ascii_digit()`, no `format!` without args, no needless borrows, no collapsible
  match, no dead helpers. `cargo clippy --fix` applies nothing under `-D warnings`; fix by hand. Never autoformat.
- **No absolute developer paths in source or tests.** Resolve binaries via `PATH`, fixtures via
  `CARGO_MANIFEST_DIR`/repo-root discovery. Hardcoded paths made 4 tests unrunnable on every other host — that is
  how a lane can be green locally and broken everywhere else.
- `unsafe_code` is denied workspace-wide. If a safe wrapper for a syscall (e.g. `killpg`) is genuinely unavailable,
  stop and report — do not add `unsafe`, a per-crate override, or a new dependency to work around it.
- Never run filesystem-wide scans (`find /`, `locate`, `updatedb`); that hung a worker at 122s and killed it. Scope
  discovery to the project tree.
- Security gate is part of done: every repository consuming `sql_restful.rs` `QueryOptions` must call
  `with_field_white_list` with names from the `nd-model/src/fields.rs` registry. Parameterized values only;
  order-by identifiers through an allowlist.
- Validation gate per crate: `cargo build -p CRATE` && `cargo fmt -p CRATE -- --check` &&
  `cargo clippy -p CRATE --all-targets --all-features -- -D warnings` && `cargo test -p CRATE`. Workspace-wide at
  each phase boundary — per-crate gates miss cross-crate feature interactions. No LSP diagnostics exist: the CLI
  toolchain is the gate; use `ast_grep` for structural searches. Report actual output, including failures.
- Tests use Arrange/Act/Assert and pin rendered SQL **and** bound args, so the Go oracle stays visible. A test that
  asserts on an executed statement is worth more than one that asserts on a rendered string; prefer execution.
- One writer per file and per worktree. `album_repo.rs` was once damaged by a runaway write loop (29MB,
  `cursor_chunk_sql` duplicated 31,268 times); re-verify each landed file for duplication
  (`ls -l`, `grep -c 'mod tests'`) before trusting any "completed" report.
- Commits GPG-signed (global config supplies it), no `-c commit.gpgsign=false`, no AI co-author trailers. Verify
  with `git cat-file commit HEAD | grep -q '^gpgsig'`. Do not push.

- **Fabricated completion reports.** Three lanes cited commits that exist on no branch (`5d1c207f`, `8842a742`,
  `6b289012`, `2691e94c`, `74683816`), one cited a module (`procgroup.rs`) that was never written, one claimed it
  had not written two files that were sitting uncommitted in its own worktree, and one reported work in a
  workspace that was never created. One reported 125 nd-db lib tests where the tree has 317. Treat a report as a
  claim: confirm `git log`/`git status`/`git worktree list` and re-run the gate yourself before integrating.

## Lifecycle hazards (learned 2026-11-10)

- Lanes die mid-milestone: provider usage limits, provider `429 max connections reached: 4`, and a daemon
  `spawn pi ENOENT`. Bank progress in **commits per milestone**; a dead lane should lose a milestone, not a phase.
- **Never archive a lane whose worktree holds uncommitted work** — archiving removes the worktree, and on 2026-11-10
  that destroyed three lanes' uncommitted files.
- Keep tests portable: temp fixtures under `target/` or the OS temp dir, never inside the repo tree, and never
  referencing another developer's checkout.

## Also still open (do not forget)

- `nd-dbschema/src/repair.rs` (367L FTS5 verify/repair) — deferred lane, still not landed.
- `nd-sched` #12 maintenance/logging half unspecified; scheduler half DONE. `crontab.rs` orphan was closed earlier.
- Settled decisions — do not re-open: fresh service (no compatibility layer), canonical API only, Subsonic shim
  (#25), plugin host (#29), Jellyfin API (#30), sonic similarity (#31) CANCELLED, JS UI and plugin host dropped,
  root path 404 by design.
- `Cargo.lock` gains deps at each crate addition; fold it into the relevant commit, not a separate one.

Start by confirming state with `git log --oneline -3`, `git status --short`, `ls rust/crates`, and one
`cargo test --all-targets --all-features` run in `rust/`. Report what differs from this brief before acting on it.

===PROMPT END===
