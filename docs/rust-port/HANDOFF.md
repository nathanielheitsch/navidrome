# Handoff — audited Rust port, bounded-task workflow

This replaces the obsolete worktree/model/phase-completion handoff. Read it with the assigned technical subplan;
it does not authorize implementation, commits, new workspaces, or completion claims.

## Locate and verify

- Repository: `/Users/nathanielheitsch/projects/barnstorm/navidrome`; Go sources at the root; Rust workspace `rust/`.
- Audited code baseline: `master`, `60cdd18402c0fd4b09b65e0b64ca494330501c02` (2026-11-18).
- Plan order: [`next-work.md`](next-work.md), current section only.
- Measured capabilities/evidence: [`current-status.md`](current-status.md).
- Assignment contracts and launch decisions: [`subplans/`](subplans/). Read the index, required task template,
  selected package, and its prerequisites before editing. Proposed API signatures are not automatically frozen contracts.
- Product scope/ownership: [`issues.md`](issues.md) and [`component-map.md`](component-map.md), as amended by the
  current plan. Dated historical status/lane tables do not override the current snapshot or orchestration rules.
- Source contracts: `schema-contract.md`, `config-contract.md`, `credentials.md`, `entity-write-contract.md`,
  `hydrate-contract.md`, `hydrate-scan-contract.md`. Their current amendments override historical scout assertions.

Start with `git log -5 --oneline`, `git status --porcelain --untracked-files=all`, and the assigned method/test sources.
Report baseline drift, existing changes, unmet prerequisites or missing decisions before editing. Never discard another
writer's work. Resolve fixtures from `CARGO_MANIFEST_DIR`/repository discovery, not a developer's absolute paths.

## Current reality

At the audited baseline the parent ran formatting, locked workspace build/clippy, DB and workspace tests, docs,
and diff checks: all exited 0. Workspace **1816 passed / 0 failed / 1 ignored** (69 groups); DB **737 / 0 / 0**
(26 groups). `cargo doc -p nd-db` succeeded with **22 warnings**, not warning-free. Exact commands and toolchain
are in `current-status.md`; these are dated results, not proof for a later diff.

- S-D3g1 `f210431f`, S-D3g2 `a8e4d57c`, follow-up `60cdd184` are in this baseline. Do not re-port/cherry-pick them.
- Codecs, typed write builders/records, eight row hooks and many read methods exist. Scanner transactional entity
  writes and specialized queries/GC do not. Tests manually executing SQL bundles are not production repository methods.
- Wrong missing-update bind ordering, mixed timestamp/idle semantics and pool configuration contracts are explicit
  pre-scanner gates. Do not connect existing renderers unchanged. Missing-state, move, annotation and cleanup operations
  require executed read-back, rollback/retry and non-target/user-data preservation tests.
- Tagreader/mapping and auth/stream components work at crate level. Scanner/HTTP/API are scaffolds; CLI main is empty.
  Login/native-audience and HTTP key integration, plus stream prepare/header/Send/body ownership, need bounded packages.
- First runnable slice is fresh DB -> configured library -> real track scan -> canonical authenticated lookup ->
  direct Range/forced transcode/disconnect cleanup. Phase-1-only ingest is not complete #17. Repair, full maintenance,
  metrics and every full-API service are not automatic prerequisites for this deliberately limited slice.
- Go external scanner gob carries progress events, not metadata/tag-reader results. In-process fixture ingest can
  explicitly defer watcher/process protocol. Complete scanner claims still require its later lifecycle packages.

## Non-negotiable task boundaries

1. The parent owns decomposition, decisions, integration and final verification. Children do not delegate unless
   expressly authorized. Each implementation assignment must specify exact task ID, baseline, file ownership,
   prerequisite APIs/decisions, acceptance tests, exclusions and report format. Stop when scope must expand.
2. Use the installed **Paseo** skill. Discover tools/hosts/profiles, read all profile notes, materialize the selected
   provider/model/mode/thinking/features. Do not hard-code a model from this document or substitute Pi-local agents.
3. Keep the issue in the parent's **verified existing workspace**. Prefer inherited agent-scoped creation; verify
   returned workspace ID. Same directory is not proof of workspace identity. No new worktree/workspace, remote
   placement, archive/delete/cleanup or isolation without explicit user approval. No blind expansion of unset IDs.
4. **One writer for the entire shared checkout**, even for disjoint files or different workspace IDs. Parallel work
   is read-only by default. Serialize writers and tests that mutate fixtures. Scratch/probe artifacts only in `/tmp`.
5. Freeze writes before a fresh read-only review. A parent source check is useful but is not a substitute for the
   required independent reviewer. On provider failure, report incomplete coverage and resume/replace the review;
   do not silently declare approval. Use completion notifications; do not poll or send hurry-ups.
6. No existing Go-source, Cargo manifest/lockfile or generated-file changes unless the particular assignment explicitly
   authorizes them. Plans needing new dependencies or tracked Go harnesses must resolve that gate first. No `unsafe`,
   lint bypasses, auto-formatting or auto-fixes. Apply deliberate minimal edits only.
7. No commits or push without fresh explicit authorization. Any authorized commit must be GPG signed; never disable
   signing or add AI attribution trailers. Verify `git cat-file commit HEAD | grep -q '^gpgsig'`. Pushing needs
   separate authorization. Do not archive a workspace/agent merely because its work was committed.
8. Destructive/irreversible changes, credentials/secrets, production DB/file mutations and ambiguous cleanup require
   user confirmation. Test only fresh synthetic fixtures; do not read or expose real credentials to build fixtures.

## Technical rules and acceptance

- Rust edition/workspace conventions: snake_case functions, documented public types/fields/functions and pinned Go
  symbol/path citations where behavior is ported. `thiserror` in libraries, `anyhow` at CLI boundary, Tokio async,
  explicit typed DI in the CLI. DTO ownership stays as assigned; no opportunistic cross-crate model refactor.
- SQLite uses pinned rusqlite (not sqlx); connections are single-owner and blocking work goes through SQLStore's
  `spawn_blocking` boundary. Transaction callbacks share one leased connection; never recursively check out the pool.
  Retry closures may run again: no repeated filesystem/network/event side effects inside them. Dropping an async
  await does not cancel an already-started blocking SQLite transaction. Compose domain errors without string loss.
- Values are parameterized; sort identifiers/options use allowlists. Library/user restrictions are deliberate typed
  inputs, never raw request SQL. Assert SQL **and ordered binds**, then execute on a fresh schema and read stored
  rows/side tables/FTS back. Renderer-only comparisons missed real data-corruption and syntax defects before.
- Pin zero/NULL/malformed/default behavior, transaction rollback/retry, empty/one/many and chunk boundaries, stable
  identity, created/birth immutability, first-error order and cross-user/library non-target behavior as relevant.
- Do not weaken strict native audience, session expiry/refresh/epoch, key hashing/ownership or trusted-proxy boundaries.
  First-admin atomicity is a retained deliberate improvement over Go, not parity debt to undo.
- Stream/network tests must exercise real body backpressure/drop, child cleanup and recovered slots. Require external
  tool/codecs for acceptance; an early-returning test is not lifecycle evidence. Keep post-header failures distinct
  from pre-header error responses and preserve cache/lease/cancellation ownership.
- Label evidence EXECUTED / SOURCE-VERIFIED / INFERRED / UNVERIFIED. Verify reports against files/history/logs.
  Use real pinned Go behavior, not a copied algorithm or Rust-produced expected output. Record nondeterministic set
  normalization, output hashes, exact commands/exits and toolchain versions. `/tmp`-only oracles are local evidence,
  not clean-clone reproducibility. Do not regenerate expectations to make a failing implementation pass.
- Product decisions stay settled: fresh service/no Go upgrade path, canonical API only, no reversible passwords;
  Subsonic/Jellyfin/plugin/sonic cancelled; bundled JS UI/backgrounds dropped; `/` is 404; Quick-Connect belongs to
  later canonical API work. Borrow Go behavior without resurrecting cancelled wire surfaces.

## Validation and completion report

From `rust/`, use focused build/tests after small steps and the package's full acceptance matrix. Integrated gates:

```sh
cargo fmt --all -- --check
cargo build --locked --workspace
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked -p nd-db --all-targets --all-features --no-fail-fast
cargo test --locked --workspace --all-targets --all-features --no-fail-fast
cargo doc --locked -p nd-db --all-features --no-deps
```

At the repository root run `git diff --check` and `git status --porcelain --untracked-files=all`. Compare rustdoc
warning messages to the baseline; do not call a successful build warning-free. Record failures/flakes and coverage
limits, not just the last green rerun. Parent validates the integrated result after selected review fixes.

Report: **verdict; exact files changed; actual commands/exits/test totals/ignored and tool versions; evidence and
non-vacuity; deviations closed/retained; blockers; exact next package**. No implementation is complete merely because
a child says it is or the current suite passes.
