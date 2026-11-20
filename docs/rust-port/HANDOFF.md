# Handoff — bounded-task workflow for the Rust port

Read the entry point [`README.md`](README.md) first. This file states how a bounded task is launched, executed,
reviewed and reported. It does **not** carry status: current milestone order is the current section of
[`next-work.md`](next-work.md), measured evidence is [`current-status.md`](current-status.md), binding decisions are
[`decisions.md`](decisions.md), and bounded assignments are the [`subplans/`](subplans/) contracts. It authorizes
nothing — no implementation, no commits, no new workspaces, no completion claims.

## Locate and verify

- Repository: this navidrome fork checkout (Go sources at the root, Rust workspace `rust/`). Resolve paths from the
  working directory — never a developer's absolute path, and never a hardcoded worktree name.
- Authoritative source baseline: this branch line at `96c495e7` — **DB-11 LANDED `52b04064`** and **DB-15's runtime half
  LANDED `96c495e7`** on top of integrated `master`'s code HEAD `e3e0770a` (**SC-02 partial** — local walker, `.ndignore`,
  folder hash, cancellation — fast-forwarded onto `2e7026b2`), which extends `272c9c66` (DB-22) and `f80b9704` (prior
  `4ab63cd5` plus DB-12 signed `e5d0592d`, DB-10 signed `65072b89` / signed merge `6e5ca97d`, DB-12 rustdoc fix `f80b9704`).
  That SC-02 slice is partial and claims neither SC-01/SC-02 completion nor MusicFS readiness; DB-11 is the executed
  read/cursor contract only and DB-15 is its runtime half only (durable Go oracle deferred — `decisions.md` H6). A docs
  lane may be cut from an earlier boundary (`ff900d3e`, `74ebd81c`) — say so, and cite integrated behavior from `master`
  instead of measuring it in your own worktree. Earlier audited boundaries are listed in `current-status.md`'s
  landed-history table; older handoff baselines are history.
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
local-FS walker slice (SC-02 partial at `e3e0770a`) but **no scanner entrypoint, tag-to-DB pipeline or full-scan
orchestration**; `nd-http` and `nd-api` are scaffolds and the CLI main is empty. Shared codecs, record marshaling, the eight row
hooks, substantial
executed reads **and twenty-four executed transaction-bound `*_in_tx` writes** are landed, including DB-10 identity/base/
junction writes and DB-12 artwork invalidation/enqueue; **DB-11 LANDED `52b04064`** adds the executed typed scanner reads,
the D-CURSOR cursor contract, the `ScannerScope`/`UserScope` authorities and the `get_visible_media` point read (21
tests), and **DB-15's runtime half LANDED `96c495e7`** adds annotation transfer + average-rating recalc (5 tests). Typed
specialized scanner reads are no longer "not landed" — GC and move/reference transactions still are, and neither DB-11's
reads nor any write has a production caller. Tests that manually execute SQL bundles are not production repository methods,
and none of the `*_in_tx` primitives has a production caller (`nd-auth` calls `UserRepo` methods outside them from its own
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
consumed by executed code. DB-22
LANDED `272c9c66`. Latest parent gates at `96c495e7` (cargo/rustc 1.99.1, EXECUTED): 2302/0/1 across 94 workspace suites
(the ignore is the pre-existing `dump_divergences`), fmt check, workspace clippy and `git diff --check` clean, `nd-db`
rustdoc at 22 warnings = baseline (zero for `sql_annotations.rs`); the intermediate `52b04064` block (2297/0/1), the
`e3e0770a` block (2276/0/1 across 92 suites, `nd-scanner` docs at 4 pre-existing warnings), the earlier `272c9c66` block
(2171/0/1 across 88 suites, `nd-db` docs at 23 pre-existing warnings) and older blocks remain dated history. See
`current-status.md` for attribution/logs. **Next is DB-14, LAUNCHABLE** — its last blocker, the
`SC.folder-write-batch-contract` decision, is resolved as **D-BATCH** (PARENT DECIDED 2026-11-20, `decisions.md`
"Folder-write batch contract": per-folder tx opened by DB, `persist_folder_delta_in_tx` `pub(crate)`, warn-vs-fatal
table, SC pre-generated IDs, parent-controlled `album_repo.rs` `copy_created_at_in_tx` helper for DB-16's later reuse). BOOT-02's login-session
native-audience correction **landed** (`c6e220eb`/`162fae1d`) — HTTP key middleware, a mounted router, and stream
prepare/header/`Send`/body ownership each still need their own bounded package. Do not connect existing renderers
unchanged; missing-state, move, annotation and cleanup operations require executed read-back, rollback/retry and
non-target/user-data preservation tests.

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
