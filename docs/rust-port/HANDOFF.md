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

Measured at `29d9d9d0` (2026-11-10), `cd rust`:

- `cargo fmt --all -- --check` — clean
- `cargo clippy --locked --all-targets --all-features -- -D warnings` — clean
- `cargo test --locked --all-targets --all-features --no-fail-fast` — **1067 passed / 4 FAILED**

The 4 failures are all `nd-ffmpeg` fixture-path portability, not behavior: they hardcode absolute paths from the
retired worktree, so live `ffmpeg`/`ffprobe` cases and the container-fixture loop **execute zero fixtures** on any
other host. `cc07fa1a` on `rust/ffmpeg-readiness` restores portable resolution (PATH lookup, dev-resolved ffprobe).
A fail-closed integration test must then assert the fixtures were enumerated, not skipped.

Do **not** trust the ledger's historical "969 passed / 0 failed" (2026-11-04 boundary) as current green.

**Shape ≠ wired.** Phases 1–4 completed *decisions and SQL shaping*; runtime integration is a separate axis:

- `nd-db`: `store.rs` has only tx/retry helpers; no `SQLStore` facade, no general row hydration
  (`sql_base_repository.go:445,480` unported); only `share_repo` and `user_props_repo` execute against a
  `Connection`. **Known defect:** `user_repo::select_user_with_libraries` renders `WHERE` after `GROUP BY`
  (invalid SQLite — reproduced as `near "WHERE": syntax error`) and its unit test *asserts that invalid order*.
  Fix the SQL against the Go oracle first; change the test only to pin the corrected statement.
- `nd-tagreader`: consumes `<media>.tags.json` sidecars that **no runtime producer writes** (only the Go
  `ND_GOLDEN_DUMP=1` fixture harness does). Real tag backend is a prerequisite for #16 → #17.
- `nd-auth`: credentials/claims verified; no router middleware. `/player` and API-key issuance additionally need
  the **player repository**, which has no Rust counterpart (Go: `persistence/player_repository.go`).
- `nd-stream`: #14 decision half only; no serving/range/limiter/archiver. `nd-cli/src/main.rs` `main()` is empty
  and `nd-http`/`nd-api`/`nd-scanner`/`nd-mapping` are scaffold-only, so nothing boots.

## Active lanes (user-approved 2026-11-10 — IN PROGRESS, not done)

| lane | workspace / branch | scope | asks |
|---|---|---|---|
| DB runtime spine | `wks_e25e0d33ddcd3d79` / `rust-db-runtime` | `SQLStore` facade, row hydration, corrected user SQL + regression; then player repo; then auth wiring | A7, A8, A9, A10 |
| FFmpeg | `wks_bee2b746c3462588` / `rust/ffmpeg-readiness` | portable fixtures committed `cc07fa1a`; then `FFCmd` process-group cancellation | A11 |
| Stream serving | `wks_eb8b973e4c6c704b` / `rust/stream-serving` | Range/conditional matrix, forced transcode, limiter, cache eviction, archive | A12 |
| Ledger correction | `wks_e33a89d09f7f9c11` / `rust/ledger-correction` | this document + `issues.md` + `next-work.md` | A6 |

Before consuming another lane's output, read its current code: streaming must use `nd-ffmpeg`'s
`new_ffmpeg_cmd()` only — the lifecycle lane is adding **private** fields to `FFCmd`, so no struct literals and no
touching lifecycle fields. Descendant-process cleanup lands separately; report it as an integration gap, not verified.

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
