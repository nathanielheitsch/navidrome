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

Measured at `38b0b812` (2026-11-11), `cd rust`:

- `cargo fmt --all -- --check` — clean
- `cargo build --locked --workspace` — clean
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` — clean
- `cargo test --locked --workspace --all-targets --all-features --no-fail-fast` — **1453 passed / 0 FAILED** (1 ignored)
  across 53 test binaries; zero surviving `ffmpeg` processes afterwards
- Branch `issue/planning-rust-sdhnn` is **pushed** to `origin` (`git@github.com:nathanielheitsch/navidrome.git`);
  76 commits ahead of the remote branch before the push, all GPG-signed, no AI co-author trailers

Landed since the 1067/4 measurement: `dee593b5` (execution seam + `SQLStore` + executed `user_repo` with
hydration), `5e56f7e8` (portable fixture resolution from `CARGO_MANIFEST_DIR` to the repo root — the 4 failures were
hardcoded paths from a retired worktree), `8152ac5a` (`process_group(0)` spawn + `kill_process_group` SIGKILL,
`STDERR_CAP` with overflow still consumed, `TEARDOWN_TIMEOUT_MS` bounded drain/reap), `aa9b280e`/`a462b1f2`
(Range/conditional matrix, bounded direct adapter, probe adapter, process-backed transcode body),
`c8c71f8d` (player repository + API-key path, 19 executed tests), `15426bbc` (80 secondary indexes +
the 8 dropped columns + index/column drift guards), `0fad9985` (auth wiring merged: mountable
`nd-auth` middleware, `verifier.rs`, `login.rs`, `refresh.rs`, `setup_store.rs`, `guarded_limiter.rs`),
`beaf63d7` (real `lofty` tag backend), `4463670b`/`5d12b5ad`/`b419472b` (#16 mapping + Go oracle),
`164780ee`/`fc48ff3f`/`18f6bc5e` (transcode semaphore wired), `8b9f48cf`…`b5c317f8` (#15 archiver +
Go zip oracle), and `db9f1e42`…`38b0b812` (**#15 HTTP handler**, 35 integration tests incl. the
zero-surviving-child criterion).

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
- `nd-auth`: **wired** (`bd8f7a84..cfea1e08`, merged `0fad9985`) — request middleware, credential verifier,
  login with transactional first-admin, post-handler token refresh, trusted-header provisioning, and
  `GuardedLimiter` denying on a non-positive limit at the boundary. The reviewed `limiter.rs` primitive
  keeps Go parity (non-positive => allow-all, `server/subsonic/auth_limiter.go:39-48,54-56`); the deny
  lives at the boundary. Reviewed by a fresh read-only reviewer (A16): **approve-with-fixes**.
  On the security questions: atomicity is sound (count+insert in one retryable tx, login.rs:222-249;
  Go's createAdmin really is non-atomic, server/auth.go:122-160), the Go-parity allow-all limiter is
  NOT reachable from a first-party mountable route (every login handler builds LoginState.limiter
  through GuardedLimiter), and no token/password/secret leak was found. Two Go-parity BUGS were
  confirmed and are being fixed on rust/auth-fixes: (1) refresh.rs:40-43 skips empty/204 bodies where
  Go refreshes on WriteHeader and post-handler (server/auth.go:332-368, test auth_test.go:513-525), so
  a password change returning an empty body leaves the client holding a revoked token; (2)
  setup_store.rs:66-83 gates dev-admin creation on count_all() == 0 instead of checking the `admin`
  username specifically (server/initial_setup.go:45-64), so a DB with a regular user and no admin skips
  creation yet commits the setup flag permanently. Credential precedence is NOT a bug - it matches Go
  (server/auth.go:186-197,257-276); neither implementation rejects conflicting sources. Known gap:
  TokenEpochHolder::record_put has no production call site — see the A3 row in issues.md: the blocker is the missing native-API CRUD route layer (#27), not nd-auth
  until a real user-update handler calls it.
- `nd-dbschema`: `schema_ddl.rs` now carries the **80 secondary indexes** (4 UNIQUE) and the
  8 late-window columns that were missing (`album.rg_album_gain`/`rg_album_peak`,
  `artist.uploaded_image`, `media_file.codec`/`probe_data`, `playlist.uploaded_image`/
  `external_image_url`, `radio.uploaded_image`) - landed `15426bbc`. `player_api_key_hash`
  UNIQUE therefore backs `PlayerRepo::save`'s race settlement, and `runtime_player_repo.rs`
  no longer hand-creates it. `schema_diff.rs` now diffs index rows (name + uniqueness) and
  per-table declared column sets, both verified non-vacuous by mutation; keep that coverage
  when the schema changes.
- `nd-stream`: Range/conditional matrix, bounded direct adapter, probe adapter, transcode body, semaphore,
  archiver, the **HTTP handler** (`StreamHandler<D>`, `handler.rs`), and the transcoding-file **cache**
  (`8abefec1`/`46b2a1ca`/`d19526e3`) are landed — **#15 is complete**. The cache took one new third-party crate,
  `sha1` 0.10, and changed the seam by agreement: `TranscodeLease::Cached` gained `signal` + `expected_size` and
  the pump arm takes the signal from the lease, because a writer-side lease is `Cached=true` but non-seekable
  and the old `Cached => None` would have served a 200 with 0 bytes. The cache plugs into the same
  `TranscodeSource` seam and `TranscodeLease::{Process, Cached}` the handler already used.
  The limiter is **wired** (no longer an unbounded ffmpeg count), and `FFCmd`/`DetachedChild` now group-kill in
  `Drop` and borrow the child across every await, so a cancelled or dropped future cannot orphan a live child —
  `dropping_the_serve_future_leaves_no_child` proves it by pid.
  The earlier leak bug (no `impl Drop` on `FFCmd`, and `teardown` taking the child before its
  awaits) was fixed in `7266a183`/`72945411` (merged `5697d089`) and is now exercised through the
  handler: Drop group-kills with a direct fallback, teardown borrows the child across every await,
  `child` is private and a second `start()` while one is owned is rejected, and the process tests
  distinguish zombies from live processes. Honest limits: Drop kills but CANNOT reap (Tokio's orphan
  queue is driver-polled with no timing guarantee), the reap timeout stays `Err` and retains the child
  for retry, and the stderr-stall hazard is documented, not fixed. The cleanup owner is fail-closed —
  a slot is released only on a **confirmed** reap, which is a deliberate deviation from Go
  (`core/stream/limiter.go:63-66` releases after closing the reader without waiting). `nd-cli/src/main.rs`
  `main()` is empty and `nd-http`/`nd-api`/`nd-scanner` are scaffold-only, so nothing boots.

## Active lanes (user-approved 2026-11-10 — IN PROGRESS, not done)

| lane | branch | scope | asks | state |
|---|---|---|---|---|
| DB runtime spine | `rust/db-runtime` | execution seam, `SQLStore`, corrected user SQL, user hydration | A7, A8, A11 | **landed** `3ce1b154`, `dee593b5` |
| FFmpeg readiness | `rust/ffmpeg-readiness` | portable fixtures, process-group cancellation, bounded teardown | A2, A3, A4, A5 | **landed** `5e56f7e8`, `8152ac5a` |
| Stream serving | `rust/stream-serving` | Range/conditional matrix, direct adapter, probe adapter, transcode body | A12 | **landed** `aa9b280e`, `a462b1f2`; semaphore, archiver, and HTTP handler landed after it — cache only remains |
| Ledger correction | `rust/ledger-correction` | this document + `issues.md` + `next-work.md` | A6 | **landed** `0309b5aa` |
| Player repository | next | `persistence/player_repository.go` incl. owner-only API-key rules | A9 | **landed** `c8c71f8d`, integrated `f198aa66` |
| Auth wiring | after player repo | middleware, `AuthResult` → claims, `/player`, API-key issuance | A10 | **landed** `bd8f7a84..cfea1e08`, merged `0fad9985` |

All lanes above are finished or dead (provider limits: `429 max connections reached: 4`, Codex usage limit,
OpenRouter budget). **Two of them reported commits that do not exist** (`5d1c207f`, `8842a742`, `6b289012`, and a
`procgroup.rs` module absent from every branch); one wrote files without committing them. Always verify a report
against `git log`, `git status`, and the actual files before integrating.

Before consuming another lane's output, read its current code. Streaming consumes `nd-ffmpeg`'s `new_ffmpeg_cmd()`
and its `cancel()`/`wait()` verbs only; `FFCmd`'s `pgid`/`closed`/`child` fields stay private.

## Then, in order (updated 2026-11-11 at `d19526e3`, gate 1442/0)

1. ~~#15 transcode cache~~ — **landed** `8abefec1`/`46b2a1ca`/`d19526e3`. #15 is complete.
2. **`nd-db` shared marshal + hydration layer** — **the critical path**.
   **Timestamp convention is DECIDED and PARTIALLY LANDED** (2026-11-11): rusqlite's `chrono` feature with
   `DateTime<Utc>`, one shared helper `nd-db/src/time.rs`; `e13bd31a`…`a7afda2a`, gate 1453/0. Converted: `share_repo`,
   `radio_repo`, `artist_repo`, `artwork_queue_repo`. **Still binding epoch integers** (listed in `time.rs:42-49`):
   `library_repo`, `mediafile_repo`, `folder_repo`, `album_repo`, `playqueue_repo`, `artwork_repo`, `plugin_repo` —
   `library_repo` must move together with `artist`'s `RefreshStats` (`artist_repository.go:479`), so it belongs to the
   hydration/#17 lane below. The bug class: INTEGER affinity beats `datetime`, so a TEXT-bound range predicate
   (`mediafile_repository.go:533,559`) silently drops those rows and `ORDER BY` is non-chronological both ways.
 Go routes every read through one
   reflection scanner (`dbx@v1.12.0/rows.go:65-103` via `sql_base_repository.go:381,429,450,466`) and every write
   through `structs.Map` + `PostMapArgs` (`sql_base_repository.go:445,480`); Rust has **neither**, only three
   hand-written `row_to_*` mappers, and **nothing computes the media_file/album FTS columns**
   (`mediafile_repo.rs:459-469` makes the caller supply them; only `artist_repo.rs:418` does). A scanner writing
   rows today would persist empty search text and break search silently. Also decide the **timestamp convention**
   (Go CGO sqlite3 emits offset-bearing UTC; the port's goldens are naive UTC) and pin it with a Go oracle.
   Keep DTOs local to `nd-db`; do not touch `nd-model`. Full brief: `next-work.md` §Then item 1.
3. **#17 scanner** (`nd-scanner`) — XL, **gated by item 2**. Minimum executed surface is 8 repos + 1 mapper, not
   ~14; a fresh scan reads back what it wrote, so executed reads are unavoidable. Split it into lanes: phase 1
   over a fixture tree, tag-reader IPC (Go uses `gob` — needs a wire-format decision, not a translation), symlink
   semantics, watcher (Go's `rjeczalik/notify` is MIT, so licensing is not a blocker), GC + finalize, idempotent
   second pass. Respect the declared #08/#16 gaps (ASF/WMA, bit depth, bitrate estimates, pair-tag ordering:
   persist no ordering from either side). Full brief: `next-work.md` §Then item 2.
4. **`nd-http` shell + `nd-cli` bootstrap** (#24) — `main()` is empty, so nothing boots. Wire auth middleware →
   router → `StreamHandler` + the two stream callers.
5. **First runnable slice (`#27A`)** — fresh DB → scan a real track → authenticated API lookup → direct Range +
   forced transcode + client disconnect leaving **zero surviving child processes**. Steps 2-4 are its prerequisites;
   the disconnect half is already proven at the handler and cache level, so #27A mostly needs items 2 and 4. Full
   #27 needs share/playlists/artwork/external/quick-connect services
   (`server/nativeapi/native_api.go:38-55`); deferrable #32 must not gate this minimal slice.
6. Independent, no dependencies: `nd-dbschema/src/repair.rs` (`db/repair.go`, 367L FTS5 verify/repair) —
   explicitly deferred, its own lane. And the #12 `nd-sched` maintenance/logging half (scheduler half is done).
7. Re-derive the `nd-dbschema` golden DDL once against the corrected shape, and give the documented `createAdmin`
   atomicity deviation its own ledger row.

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

## Verification protocol (learned 2026-11-11)

- **When a reviewer dies on provider budget, the parent reviews instead of skipping review**, and says so. On
  #08 the review agent errored on quota, so the parent ran it: Cargo.lock delta + license audit, fixture
  regeneration against `generate.sh` (sha256-compared, byte-identical), a grep proving the sidecar loader is
  reachable only from tests, and a mutation that stubs `read_metadata` (16 of 33 tagreader tests fail, so the
  real-parsing coverage is load-bearing). A parent review is not an independent one — record who did it.
- **Mutation-check every "bug fixed" claim.** On A17 the parent restored the pre-fix code and confirmed the new
  tests fail (`refresh_includes_header_for_empty_response`, `initial_setup_creates_dev_admin_with_existing_regular_user`).
  A test that cannot fail is not evidence of a fix.
- **Verify a child's claims before forwarding them.** The teardown advisor's `kill_group` criticism was stale —
  it had read the pre-fix commit — and the parent dropped it after checking the branch.
- **Parent-supplied premises are a real failure mode.** Two children were corrected because the parent asserted
  something false: a `COLLATE NOCASE` on `user.user_name` (the collation belongs to the separate
  `user_username_password` index), and that Go releases the limiter permit only after reaping (it does not —
  `core/stream/limiter.go:63-66`). Children that pushed back were right; cite files, not recollection.

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
