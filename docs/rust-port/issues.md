# Rust port — issue plan (paste-onto-your-tracker edition)

Repo: this navidrome fork (Go server, GPL-3.0). Companion doc: `docs/rust-port/component-map.md`
(component graph, IDs `C01`–`C33`). This file is the build-order issue list: 4 design issues
(`#S1`–`#S4`) that freeze shared contracts, then the numbered implementation issues `#01`–`#34`
(of which #25 Subsonic shim, #29 plugin host, #30 Jellyfin API, #31 sonic similarity are CANCELLED and #34 backgrounds
is DROPPED — 29 live issues), in build order. Every claim is a path/line citation from a read-only pass
over this checkout; [`current-status.md`](current-status.md) and the scope table below record what has since been
executed and gate-verified.

## Revision (post-milestone-0) — settled decisions

1. **ONE canonical API** = the existing native surface (`server/nativeapi/**`, mounted at `consts/consts.go:58` `/api`). Reason: the owners' Flutter client speaks it; Subsonic becomes a baseline reference, not the trunk.
2. **Subsonic wire layer CANCELLED — clean API break** (#25). The canonical API is *inspired by* Subsonic, not bound to it: keep its sound semantics (typed-ID prefixes `al-`/`mf-`/`pl-`, artwork-ID composition, transcode-decision split), drop the wire anachronisms (XML, auth-in-query, `u/t/s`, `.view`, camelCase verbs). Reason: a faithful `t = md5(password+salt)` shim requires the reversible password store back — re-importing model B's removed debt — and the `apiKey` alternative works only for clients that can send `apiKey`, which major Subsonic clients cannot. Third-party Subsonic clients will not work against this server; that is a deliberate product decision. The 126 goldens become reference data only, and a future community adapter would consume the published OpenAPI spec, not server internals.
3. **WASM plugin host cancelled** (#29). Reason: the Extism/wazero host ABI is the largest single lane and delivers nothing the canonical API or clients need; `plugins/capabilities/*.yaml` remain as the trait spec for a future Rust-native layer.
4. **Credential model B is the settled default** (#S4): hashed passwords (argon2id) + long-lived hashed API keys + JWT sessions with `user.token_epoch` revocation. There is no reversible AES-GCM store anywhere on the trunk and **no `migrate-credentials` path exists at all** — see item 6 (fresh service, no upgrade path), which supersedes the earlier wording that treated decryption as a one-time migration step. With #25 cancelled, model A has no host at all.
5. **JS UI dropped unconditionally** (#28): the bundled react-admin UI is deleted (the Flutter client comes after the server translation); only the share-page HTML contract survives.
6. **No upgrade path, fresh service**: there are NO existing installs to migrate. The trunk ships argon2id from day one; no `migrate-credentials` exists — first-run bootstrap (initial admin creation, `server/initial_setup.go` pattern) replaces it. The whole byte-compat apparatus is dropped: DB schema is a free set (one fresh migration, edited as the schema evolves), config keys are free (clean TOML, no deprecation table), and the ID scheme inherits the 22-char base62 + typed-prefixes shape because it is good design, not for compat.
7. **Jellyfin API dropped** (#30 CANCELLED): `server/jellyfin/**` and the `jellyfin` JWT audience are not ported; Quick-Connect survives re-owned by #27 (device pairing, not Jellyfin-specific).

Fields per issue: **Build** (what to write, in order) · **Test** (strategy + Go oracle) · **Risk**
(`CRITICAL` byte-exact wire oracle; `HIGH` compat/process/FFI/ABI; `MED` behavior-preserving;
`LOW` free; `DEFERRABLE` stub, don't block) · **dependsOn** (issue numbers here — start only when all
are green) · **Owns** (Rust paths this issue may create/edit) · **Replaces** (Go paths retired).

Global rules:
- Crate boundary = ownership unit. No path appears under two issues; if you want another issue's
  path, you are consuming it — file a dependency, do not edit.
- Every crate is GPL-3.0 (`LICENSE:1-4`). Third-party license planes stay separate. The extism/wazero plane (`extism-pdk = "1.2"` at `plugins/pdk/rust/nd-pdk/Cargo.toml:17`, `go.mod:20,58`) is no longer ported at all — #29 is CANCELLED; the taglib fork stays pinned at `go.mod:6`.
- Wire goldens are **data** and, post-clean-break, gate **nothing** (#25 is CANCELLED): `server/subsonic/responses/.snapshots/` stays in-tree as reference data — a recorded behavioral spec (field shapes, streaming/Range/ETag semantics) to consult while authoring the canonical API. Regeneration stays Go-only (`Makefile:167-169`).
- The trunk's oracle is **native-API response-shape goldens captured from the Go binary before the issue opens** (#27, and likewise #28); nothing on the trunk is gated by the Subsonic goldens.
- Gate #S1–#S4 with an approval step: they change contracts nothing else absorbs.

---

## Current progress authority (baseline integrated `master` code HEAD `e3e0770a` plus DB-11 `52b04064`, DB-15 runtime `96c495e7`, DB-14 `de0f49ea` and DB-21(P1-DB) `632271a9`, updated 2026-11-21)

Use [`README.md`](README.md) for the read order, [`current-status.md`](current-status.md) for measured capability axes,
[`next-work.md`](next-work.md) for the current milestone order and live wave, [`decisions.md`](decisions.md) for settled
decisions, and [`subplans/`](subplans/) for bounded technical assignments. The issue descriptions below retain
ownership/product scope, **not** current implementation readiness or permission to launch all dependencies.

- Latest parent gates at `6d6938b1` (**DB-17 LANDED `b3dee0a6`** — four caller-tx count-refresh helpers, 20 executed tests — and **BOOT-00 LANDED `6d6938b1`** — manifest pin repair + the D1 dependency boundary, gate work only with no production behavior; this machine's
  toolchain is **Homebrew cargo/rustc 1.99.1** (`/opt/homebrew/bin`, rustup stable default), EXECUTED on the merged tree): workspace tests
  **2363 / 0 / 1 across 98 suites** (the ignore is the pre-existing
  `dump_divergences`), `-p nd-db` all-targets **1073 / 0 / 0 across 49 suites**, `nd-http` **1 / 1** (the BOOT-00 cycle test), `cargo fmt --all -- --check`, workspace clippy
  (`--all-targets --all-features -- -D warnings`) and `git diff --check` clean, `nd-db` rustdoc **21 warnings =
  post-DB-16 baseline** (arithmetic: `2343` at `7d022a02` `+ 20` DB-17 `+ 0` BOOT-00, suites `97 → 98`). The prior `7d022a02` block (**DB-16 LANDED `7d022a02`** — executed move/reference-remap/delete primitives, A8 share typed rejection, D-PLUGIN deletion;
  **2343 / 0 / 1 across 97 suites**, `-p nd-db` **1053 / 0 / 0 across 48 suites**, rustdoc **21 = post-DB-16 baseline**), the prior `632271a9` block (**DB-21(P1-DB) LANDED `632271a9`** — the
  DB-side-only P1 exit contract; **2326 / 0 / 1 across 96 suites**, `-p nd-db` **1036 / 0 / 0 across 47 suites**, rustdoc **22 = baseline**)
  is dated history. The prior `de0f49ea` block (**DB-14 LANDED `de0f49ea`**;
  **2319 / 0 / 1 across 95 suites**), the prior `96c495e7` block (**DB-11 LANDED `52b04064`**, **DB-15 runtime half
  LANDED `96c495e7`**; **2302 / 0 / 1 across 94 suites**), the intermediate `52b04064` block (**2297 / 0 / 1**) and the prior
  `e3e0770a` block (**SC-02 partial** — local walker, `.ndignore`, folder hash, cancellation — fast-forwarded
  onto `2e7026b2`: **2276 / 0 / 1 across 92 suites**, `nd-scanner` rustdoc **4 pre-existing** warnings) are dated
  history. These are parent integration results; this docs lane
  did not re-run Cargo. Prior dated block at `272c9c66` (DB-22): **2171 / 0 / 1 across 88 suites**, `nd-db` docs **23**
  pre-existing rustdoc warnings. Older dated blocks below are history, not current readiness.
- Earlier parent gates at `f80b9704`: fmt/workspace clippy/tests/docs pass, **2159 / 0 / 1 across 87 suites**, `nd-db`
  retains **22 rustdoc warnings** (no net increase after `f80b9704`). Logs `/tmp/nav-db10-integrated-{clippy,tests,doc}.log`;
  workspace test total independently tallied here. Historical direct gates at `4ab63cd5`: workspace **2114 / 0 / 1** across
  **85** suites, `nd-db --all-targets` **929 / 0 / 0** across **40** suites (doctests **6** on a separate axis, not
  included), `nd-dbschema` **61 / 0 / 0**, `nd-auth` **107 / 0 / 0**, fmt/clippy/diff clean, doc build **22 warnings** (not
  warning-free). At the `ec84c176` DB-00 boundary: workspace **2002 / 0 / 1** across **77** suites, `nd-db --all-targets`
  **834 / 0 / 0** across **32** suites, `nd-auth` **90 / 0 / 0**. At the later DB-02 boundary `74ebd81c`, `nd-db` measured
  **841 / 0 / 0** across **33** suites. A green suite is not scanner/server readiness; re-measure on any integrated tree.
- Landed and reviewed: sort gate `62789ba2`, DB-01 `f57ae050`, DB-03/D-OPEN `4a8aac9f`, executed transcoding reads
  `f8ceeb8f`, and the S-D3a…S-D3g2 hydration/executed-read lanes (`3ba10ee1`, `6cd6c8d2`, `1c215a7e`, `0d2dc89d`,
  `6a201f4a`, `f210431f`, `a8e4d57c`, `60cdd184`). All eight row hooks and substantial reads execute, and since `4ab63cd5`
  so do executed transaction-bound writes; typed specialized scanner reads execute since DB-11 `52b04064`, while
  GC (DB-20) and move/reference transactions (DB-16) do not.
- LANDED since `ff900d3e` with **partial** scopes: DB-15 render-only rating repair `ab466928` **and its runtime half
  `96c495e7`** (annotation transfer + average-rating recalc executed; DB-15 is still **not** fully DONE — the durable
  Go-harness oracle is deferred to the EVID lane, `decisions.md` H6, and no annotation service/route caller exists),
  **DB-11 `52b04064`** (executed typed scanner reads, `D-CURSOR` cursor contract, `ScannerScope`/`UserScope`,
  `get_visible_media`; not the full cursor/REST surface, no production scanner caller, no `nd-scanner` consumption),
  scanner local-FS storage subset `e9dd8230`/`ff5b9c92`, and the **SC-02 partial `e3e0770a`** (bounded
  local walker, `.ndignore`/ignore policy, folder hash, cancellation with executed walker/ignore/hash/seam tests; the six
  approved dependencies recorded in `decisions.md` S9 at `b1aeea2c`, corrected at `2e7026b2`). Neither scanner slice is
  SC-01 ratification or SC-02 completion: there is still no scanner entrypoint, `MusicFS`/`ReadTags` dispatch, tag-to-DB
  ingest, full-scan orchestration or watcher. DB-00 typed
  transaction/error seam `48e65806`/`ec84c176` (seam only, not the DB lane), DB-02 `6d9d984d`/`74ebd81c`, and the executed
  write lanes DB-04 `c791dbc1`, DB-05 `60be7534`, DB-06 `bfdcfeca`, DB-07 `2911b9ab`/`9be6ad0e`, DB-08
  `32c4fa5b`/`320b8b03`, DB-09 `d938d159`/`842c32a2`, DB-13 `eae4ba1d`/`4ab63cd5`. None of the `*_in_tx` primitives has a
  **production** caller; `nd-auth` calls `UserRepo` methods (`find_by_username`, `count_all`, `count_by_username`, `put`,
  `update_last_login_at`) from its own production code, but that router is mounted only in crate tests, not a server.
  DB-02 left two known residuals: scrobble-buffer integer stamps and no executed pin for
  `find_recent_files_by_properties_sql`.
- Real tag parsing and mapping, auth components, and #15 handler/cache/limiter/archiver exist at crate level.
  Scanner holds the SC-02 partial walker slice (`e3e0770a`) but no entrypoint or ingest; HTTP/API remain scaffolds and the
  CLI main is empty. **BOOT-02 landed** `c6e220eb`/`162fae1d`: login mints a
  native expiring session token, so the login-audience gap is closed — but that is **not** full auth/API integration. HTTP
  API-key extraction/issuance middleware, a mounted router, and stream HTTP commitment/Send still need explicit packages
  (BOOT-00/01/03/04, API-00…06); do not call those components absent or assume they mount unchanged.
- Pre-scanner safety gates: missing-update bind repair (DB-01), every-pool-handle effective configuration (DB-03),
  typed transaction/error seam (DB-00, `48e65806`/`ec84c176`), timestamp/idle lifecycle (DB-02, `6d9d984d`/`74ebd81c`) —
  **all four landed**, and DB-04…DB-10/12/13 are landed too. DB-10 signed `65072b89` / signed merge `6e5ca97d` executes
  identity/base/junction writes; DB-12 signed `e5d0592d` (fast-forward, rustdoc fix `f80b9704`) executes only artwork
  invalidation/enqueue, not worker/full scanner. Neither has a production scanner caller. **DB-11 LANDED `52b04064`**
  and **DB-15's runtime half LANDED `96c495e7`**, so `D-CURSOR`/`D-SCOPE` (recorded in `decisions.md`, PARENT DECIDED
  2026-11-20, corrected record `8b5b3925`) are consumed by executed code, and **DB-14 LANDED `de0f49ea`** consumes
  `D-BATCH`; **DB-21(P1-DB) LANDED `632271a9`** as the DB-side-only P1 exit
  contract (parent scope narrowing 2026-11-21, `decisions.md` "DB-21 exit scope and prepared-statement gates"; no production caller,
  not the full exit) and **DB-16 LANDED `7d022a02`** (executed move/reference-remap/delete primitives, A8 share typed rejection,
  D-PLUGIN deletion executed; 18 executed tests; no production caller — SC-06/BOOT maintenance are the consumers), **DB-17 LANDED `b3dee0a6`**
  (four caller-tx count-refresh helpers — album/artist playcounts, tag `update_counts_in_tx`, library 8-probe + 10-bind
  `refresh_library_totals_in_tx`; 20 executed tests; no production caller — SC's phase-3 `:138`/`:150` and `scanner.go:316`/`:344` triggers
  are the consumers) and **BOOT-00 LANDED `6d6938b1`** (D3 manifest pins repaired with zero new lock entries + the D1 `nd-http` dependency
  boundary with the executed `dependency_graph_has_no_http_api_cycle`; gate work only, nothing listens), so the remaining
  forward spine is `DB-18 → 19 → 20 → DB-21(FULL-SCANNER-DB)` (DB-18 gated on `D-STATS`, still without a canonical record). DB-22 LANDED `272c9c66` (executed genre/scrobble/
  playqueue/artwork reads; C5 narrow-read boundary).
  The **SC-02 partial is LANDED at `e3e0770a`** (rebase base `2e7026b2`; S9 gate `b1aeea2c`), a partial slice rather than
  SC-02 completion; the former `rust-sc02-local-walker` worktree/branch are removed. Full #17 also requires
  a scanner entrypoint, ingest/aggregation, moves/stats/playlists/GC/lifecycle.
- #27A is a deliberately scoped first runnable slice. Repair, full maintenance, metrics and all full-API services do not
  automatically gate it. Cancelled routes/services stay cancelled. The old build-order list is historical.
- Decision 6 (fresh service/no upgrade path) supersedes decision 4's historical `migrate-credentials` wording: no
  migration command ever exists; first-run bootstrap replaces it.

### Explicit deviation register entry: AUTH-ATOMIC-ADMIN

**Retained intentional divergence, not an open implementation task.** First-admin creation is atomic in a retryable
transaction (`nd-auth/src/login.rs:197-256`, `create_admin`/`create_admin_inner`; the trusted-header variant shares the
same rule at `:279-390`), unlike Go's non-atomic count/insert (`server/auth.go:122-160`). These line references are as of
`4ab63cd5` and shift with every edit to that file.
Do not restore the Go race for parity. Existing auth tests cover concurrent creation; the future bootstrap/API plan
must preserve the same transaction and separately prove its mounted setup flow. Audit confidence: source/test-body
inspection plus the parent workspace gate above; no new isolated concurrency run is claimed here.

## Current issue scope (updated 2026-11-20, baseline integrated `master` code HEAD `e3e0770a` plus DB-11 `52b04064` and DB-15 runtime `96c495e7`)

Dated crate states below are **scope snapshots**, not launch permission. Measured gates, capability axes and remaining
blockers live in [`current-status.md`](current-status.md); order and the live wave in [`next-work.md`](next-work.md);
settled decisions in [`decisions.md`](decisions.md); bounded packages in [`subplans/`](subplans/). The former 2026-11-10
progress ledger (lane-by-lane transcripts, per-crate DONE prose and superseded test totals) was removed as a competing
schedule; the durable facts it held are recorded in `decisions.md`, `current-status.md` and `next-work.md`'s historical
index. Long-running truth worth keeping per issue is summarized in one row each.

| Issue | Crate | Current scope state (dated, SOURCE-VERIFIED unless noted) |
|---|---|---|
| #S1-#S4 | workspace manifest + contracts | DONE — workspace/crate graph, schema, config and credential contracts frozen (`schema-contract.md`, `config-contract.md`, `credentials.md`) |
| #01 | `nd-utils` | DONE (22 modules), incl. Go-exact `str::to_lower` (`decisions.md` L1/L2) |
| #02 | `nd-id` | DONE — id/artwork/participant schemes |
| #03 | `nd-conf` | DONE — keys/config/dir/bytes/duration/crontab/consts; `EnableTranscodingConfig` carried-but-unused |
| #04 | `nd-dbschema` | PARTIAL — fresh DDL (all 80 index rows, 8 recovered columns via `15426bbc`), marker-guard migration runner, `optimize.rs`/`backup.rs` green; `db/repair.go` (367L FTS5 verify/repair) **explicitly deferred** as its own lane; golden DDL re-derivation pending (`OPS-01`); DB-03 landed here (`4a8aac9f`) |
| #05 | `nd-db` | PARTIAL — execution seam (`3ce1b154`), store/pool, all 23 repositories shaped, `criteria_sql.rs` (2536 lines porting the 904-line Go `persistence/criteria_sql.go`), shared marshal + hydration + 8 hooks landed; substantial executed reads incl. transcoding (`f8ceeb8f`) **and thirty-six executed transaction-bound `*_in_tx` writes landed** (33 `pub`, plus three `pub(crate)`: DB-14's folder-delta seam and DB-16's `move_matched_mediafile_in_tx`/`remap_missing_mediafile_in_tx`; amended at `b3dee0a6` — DB-17 added four `pub` refresh helpers) (property `c791dbc1`, library + scan lifecycle `60be7534`, tag `bfdcfeca`, folder/media missing-state + album touch `2911b9ab`/`9be6ad0e`, artist `32c4fa5b`/`320b8b03`, album `d938d159`/`842c32a2`, user epoch revoke `eae4ba1d`/`4ab63cd5`) with **no production caller**; DB-10 `put_mediafile_in_tx` LANDED `65072b89`/`6e5ca97d`; DB-12 artwork invalidation/enqueue subset LANDED `e5d0592d` (rustdoc fix `f80b9704`), no production scanner callers; **DB-11 LANDED `52b04064`** (executed typed scanner reads + `D-CURSOR` cursor contract + `ScannerScope`/`UserScope` + `get_visible_media`, NEW `scanner_queries.rs`, 21 executed tests), **DB-15's runtime half LANDED `96c495e7`** (`AnnotationEntityKind`, `update_avg_rating_in_tx`, `reassign_annotation_in_tx`, 5 executed tests; durable Go oracle deferred to EVID — `decisions.md` H6) and **DB-14 LANDED `de0f49ea`** (NEW `scanner_writes.rs`: `FolderWriteBatch`, `persist_folder_delta`, `CommittedFolderOutcome`, `scan_artwork_item`, plus `album_repo::copy_album_created_at_in_tx`; 17 executed tests), each with no production caller; **DB-21(P1-DB) LANDED `632271a9`** (NEW `tests/runtime_scanner_persistence_contract.rs`, 4206 lines, 7 executed tests — composed phase-1 regression through the real pool, a 51-pin API freeze, and an executed U-ledger pinning `SC.mapping-bridge`/`SC.phase1-fixture`/`BOOT.configured-library-caller`/`production_full_finalize_db_contract` as Unmet; the collection/count API for later API-F* still belongs to DB-21(FULL)); GC (DB-20) and playlist orphan cleanup (DB-19/20) remain; move/reference transactions **landed at `7d022a02` (DB-16)** and count refresh **landed at `b3dee0a6` (DB-17)**; `tag_repo::purge_unused_sql` is still a renderer. `DB-01` landed `f57ae050`; `DB-00` `48e65806`/`ec84c176` and `DB-02` `6d9d984d`/`74ebd81c` are **landed**, not in flight; `DB-22` `272c9c66` executed genre/scrobble/playqueue/artwork reads, so **one** module is render-only (`smart_playlist` — not a separate Go repository; `plugin` was deleted by D-PLUGIN at `7d022a02`, DB-16); artwork item-image lookup and invalidation execute, and `share`/`user_props` execute on raw connections outside the shared helper |
| #06 | `nd-model` | DONE — DTOs + criteria DSL (`fields.rs`, `operators.rs`, `criteria.rs`, 35 crate tests); the abandoned `docs/rust-port/drafts/` copies were deleted 2026-11-19 |
| #07 | `nd-ffmpeg` | PARTIAL — args tables byte-exact, ffprobe JSON parser, FLAC STREAMINFO patcher, process-group spawn/SIGKILL teardown (`8152ac5a`), `MockFFmpeg` double, live container fixtures; symlink-chain and `invalid-files` fixture cases uncovered; `patchFLACDuration` deferred to the consumer |
| #08 | `nd-tagreader` | DONE at crate level — real lofty `=0.25.5` parsing (`beaf63d7`) replaced the sidecar-only reader, so #16→#17 is unblocked upstream. Declared gaps: ASF/WMA unsupported (error, never a fabricated success), AAC-in-MP4 bit depth 0 vs Go's 16, bitrate estimates differ, MPEG-frame-timestamp SYLT skipped |
| #09/#10 | `nd-lyrics`/`nd-playlists` parsers | Parsers landed; fetch/import service halves belong to later packages |
| #11 | `nd-events` | DONE at crate level; no mounted SSE route |
| #12 | `nd-sched` | PARTIAL — scheduler/crontab half DONE (validator agreement and robfig oracle pinned); maintenance/logging half unspecified (`OPS-04/05/06`) |
| #13 | `nd-auth` | DONE (credential + claim model) — `AuthGate` middleware exists and is crate-test mountable, but no production server mounts it. HTTP API-key extraction/issuance and production router integration remain open (API-01); native-only audience, dual secrets, epoch revocation, fail-closed `GuardedLimiter`, CIDR/`@` trusted sources. **The login-audience defect is CLOSED at `c6e220eb`/merge `162fae1d`** (login mints `create_native_session_token`, native audience + checked positive `exp`; crate tests pass it through `AuthGate::Api`); AUTH-ATOMIC-ADMIN deviation retained below |
| #14 | `nd-stream` decision half | DONE — decision tables, HS256 stream token, `ProbeInput` keeps `make_decision` infallible |
| #15 | `nd-stream` serving half | **COMPLETE at crate level** (`d19526e3`) — Range/conditional matrix, bounded direct adapter, probe adapter, process-backed transcode body, limiter, archiver (Go `archive/zip` oracle), HTTP handler, transcoding-file cache. **Not HTTP-ready**: `prepare` privacy, header-commitment and `Send` adapter seams need API-04/API-05 |
| #16 | `nd-mapping` | DONE at crate level (`b419472b`) — Go oracle-pinned `Info → Metadata → MediaFile`; 11 declared divergences; pair-tag map-order hazard (`decisions.md` O2). No ingest caller |
| #17 | `nd-scanner` | SCAFFOLD with two landed partial slices: local-FS storage subset `e9dd8230`/`ff5b9c92`, then the **SC-02 partial LANDED `e3e0770a`** (fast-forwarded onto `2e7026b2`) — bounded local walker, `.ndignore`/ignore policy, folder hash and cancellation with executed `tests/{folder_hash,ignore_policy,storage_seam,walk_policy}.rs` and the six approved dependencies recorded in `decisions.md` S9 (`b1aeea2c`, corrected `2e7026b2`). Still **not** SC-01 ratification or SC-02 completion: no scanner entrypoint, `MusicFS`/`ReadTags` registry dispatch, tag-to-DB ingest, full-scan orchestration or watcher. Minimum executed surface is 8 repos + 1 mapper (`decisions.md` S1); gob carries `ProgressInfo` only. DB-11 `52b04064` released the typed scanner reads/cursors this crate must consume, but nothing calls them yet |
| #18 | `nd-http` | BOOT-00 dependency-boundary module (`RouterInjection` + the executed `dependency_graph_has_no_http_api_cycle`; deps `nd-conf`/`nd-auth`/`axum` only) — **still no router, middleware or listener**; BOOT-03 owns the HTTP shell |
| #19/#20/#21/#22/#23 | artwork, scrobbler, external metadata, lyrics fetch, playlists service | Not started; each needs its own plan when scoped (`FRONT-*`) |
| #24/#33 | boot + service graph | BOOT-00 **LANDED `6d6938b1`** (D1-D3 executed, D4 B3-ratified: manifest pins repaired with zero new lock entries, `nd-http` typed dependency boundary + executed cycle test). **BOOT-01 LANDED `659d66f3`…`f2289d8b` — callable initialization only, NOT a running server**: exported `preflight` + `ServiceReady::prepare` compose config resolution (explicit path → `ND_CONFIGFILE` → default), auth/stream validation, ffmpeg-config injection before filesystem setup, one blocking-worker startup/commit, auth state and shared cache. Both BOOT-01-owned gaps are closed — `ND_CONFIGFILE` resolution and **B2** ffmpeg-config injection (`845b98cb`/`f2289d8b`) — so neither is outstanding. **`main` is still `fn main() {}`; no listener/router/API/production scan caller.** BOOT-03/BOOT-04 and the actual forced-transcode gate remain BLOCKED |
| #25/#29/#30/#31 | Subsonic shim, plugin host, Jellyfin, sonic similarity | **CANCELLED** — no wire, no ABI, no host; reference goldens stay data-only |
| #26 | player-session service (`nd-players`) | Scope split `D-PLAYER26`: session service retained (API-01 placement), owner-scoped editing retained `D-PLAYER-EDIT`, mpv/jukebox **DROPPED**. Not DONE — no mounted route; transcoding read landed `f8ceeb8f`; seed + route remain |
| #27 | `nd-api` canonical surface | SCAFFOLD (765-byte `lib.rs`); scoped **#27A** is the first slice; full #27 needs share/playlists/artwork/external/quick-connect services. BOOT-02 landed `c6e220eb` gives login a native expiring token, but with no router and no API-01 key middleware nothing is mounted |
| #28 | UI contract | Bundled JS UI **DROPPED**; only the server-side share-page HTML contract survives |
| #32 | metrics/diagnostics | DEFERRABLE — must not gate #27A |
| #34 | backgrounds | **DROPPED** (sole consumer was the JS UI) |

`nd-cli/src/main.rs` is empty: no DI wiring, no bootstrap, so nothing boots. The historical claim that #26 depends on a
missing player repository is stale — `player_repo` landed at `c8c71f8d`.

## First-runnable-slice exit criteria

The smallest honest definition of "the Rust server runs", in dependency order:

1. Fresh SQLite DB via `nd-dbschema` open/init (schema + `SEEDEDRAND`/`NATSORT` registered per connection).
   **Mechanism exists** (`store::SQLStore::open`, no migrations); no caller yet.
2. Scan a real track directory: a tag backend that *produces* metadata → `nd-mapping` `Info` → entities → `nd-db` write path.
   **Blocked upstream** — #08 has no producer, and only `user_repo` executes so writes need the mediafile path too.
3. Authenticated API lookup: initial admin setup → claims → mounted middleware → one entity read.
   **Blocked** — needs the player repository (A9) for `/player` + API-key issuance, then auth wiring (A10).
4. Stream one track three ways: direct Range, forced transcode, and client disconnect.
   **Half-built** — Range matrix, bounded direct adapter, and transcode body exist; no HTTP handler, cache, or limiter.
5. Disconnect leaves **zero surviving child processes**. **Satisfied at the crate level** by `8152ac5a`
   (`process_group(0)` + `kill_process_group`, verified by a grandchild-survival test that fails without it); still
   needs asserting end-to-end through the handler.

Dependency arithmetic the row labels hide: runtime ingest needs a real #08 tag producer, then #16, then #17.
The first boot needs only scoped HTTP/API plus minimal serve wiring (`#27A`); full #27 needs share, playlists,
artwork, external metadata, and quick-connect services (`server/nativeapi/native_api.go:38-55`), so a scoped
`#27A` over landed auth + DB is the correct first slice. Deferrable #32 must NOT gate it.

Next in build order (2026-11-10, after integration): **player repository** (A9) → **auth wiring** (A10) →
**#15 remainder** (transcode cache, `core/stream/limiter.go` semaphore, archive, HTTP handler) → a real #08 tag
backend → #16 → #17 → `repair.rs` → the unspecified #12 maintenance/logging half → scoped `#27A`. Criterion 5
(disconnect leaves zero surviving children) is now satisfiable: `8152ac5a` kills the process group. Criteria 1-4
still need the player repo, auth middleware, and a tag producer. The historical "#07 test-plane tail" note below
is superseded by the `5e56f7e8` fixture work.

---
## Part A — shared artifacts first (design issues, blocking)

### #S1 — Fix the Rust workspace and crate graph

- **Build**: one `nd-*` workspace member per owning issue below: `nd-utils nd-id nd-conf nd-dbschema
  nd-db nd-model nd-ffmpeg nd-tagreader nd-lyrics nd-playlists nd-events nd-sched nd-auth nd-stream
  nd-mapping nd-scanner nd-artwork nd-public nd-api nd-players nd-ui-contract
  nd-cli nd-metrics nd-agents nd-http nd-scrobbler` — **26 owned crates** after the
  clean break and no-upgrade-path decisions (`nd-native-api` → `nd-api`; `nd-subsonic-compat`, `nd-sonic`, `nd-plugin-host`, `nd-backgrounds`, `nd-jellyfin` all removed).
  Crate-vs-issue arithmetic, frozen here: 34 numbered issues − #25/#29/#30/#31 CANCELLED − #34 DROPPED = 29 live issues,
  and 26 crates because three pairs share a crate: #14 inside `nd-stream` (#15), #09 inside `nd-lyrics` (#22), #10 inside
  `nd-playlists` (#23). 29 − 3 = 26. ✓
  `nd-mcp` is deliberately NOT a workspace member — it is a separate client artifact talking to the documented canonical API (see "Not ported"). Freeze the DI replacement
  once: Go builds its graph in `cmd/wire_gen.go` / `cmd/wire_injectors.go`, with the datastore
  singleton at `model/datastore.go` and provider selection at `core/wire_providers.go`
  (`persistence/persistence.go:17-40` declares repository interfaces); Rust replaces codegen with
  explicit construction (typed service struct per crate, wired in `nd-cli`'s `main.rs`).
  Decide here, once: crate naming/granularity, error type, async runtime, trait-object vs generic
  services — later issues inherit, they may not diverge.
- **Test**: compile-only — `cargo build`, `cargo fmt --all -- --check`,
  `cargo clippy --all-targets --all-features -D warnings`; one Arrange-Act-Assert stub per crate
  proving it instantiates without pulling the world in.
- **Risk**: MED (design freeze, no behavior).
- **dependsOn**: —
- **Owns**: `Cargo.toml` (workspace manifest), `docs/rust-port/component-map.md`, `docs/rust-port/issues.md`
- **Replaces**: DI codegen — `cmd/wire_gen.go`, `cmd/wire_injectors.go`, `core/wire_providers.go`,
  `model/datastore.go`

### #S2 — Freeze the DB schema contract

- **Build**: a written contract (no code): target table set, FK graph, registered SQL funcs, and the
  MUST-RESOLVE strategy question. Hard set, from `db/db.go:27` (`Dialect="sqlite3"`), `db/db.go:28`
  custom driver, `db/db.go:85-89` pragmas, `db/db.go:54-59` `:memory:` rewrite, `db/db.go:62`
  `SetMaxOpenConns`: table list minus dropped `search` (`20200319211049`) and `genre*` (dropped by
  `20241026183640:85-88`); FK graph incl. `library_id` FKs and `on delete cascade`
  (`20240511220020:17-56`, `20241026183640:40-125`); 22-char varchar entity ids
  (`model/id/id.go:1-48`); FTS5 tables `media_file_fts/album_fts/artist_fts` with `_ai/_ad/_au`
  triggers (`db/migrations/20260220173400_add_fts5_search.go:24-`, repaired by `db/repair.go:12,14`);
  `WITHOUT ROWID` + composite PKs + covering index
  `ix_artwork_queue_drain(item_kind, priority DESC, enqueued_at, retry_at)`
  (`db/migrations/20260722023032_add_artwork_tables.sql:1-46`); canonical-id column set
  (`20260720015443_uniform_canonical_ids.go:55`); `property` keys
  (`consts/consts.go:15-29,33-34,49-51,65-67,162`). Registered SQL funcs must be re-registered per
  connection in the Rust SQLite binding: `SEEDEDRAND` (`db/db.go:46`, from `utils/hasher`), `NATSORT`
  (`db/db.go:49`, `utils/natural.CompareFold`).
- **Fresh service — no compat set**: there are NO existing installs, so nothing is byte-frozen. The
  Go schema's FINAL shape is the starting point (derive from the repos + the last schema-shaping
  migrations, not the 138-file history: `.go` migrations carry Go logic, `20241026183640.go:106`,
  `db/db.go:32-35` embeds only `migrations/*.sql`); edit freely from there. Exactly ONE fresh Rust
  migration ships as the basis and is edited in place as the schema evolves. Internal PK/column
  names, ANALYZE scheduling (`db/optimize.go:19-40`), backup naming (`db/backup.go`) are all free.
- **Credential columns** (#S4, settled): `user.password` is argon2id from day one (no reversible
  `enc:` payload, no `PasswordsEncryptedKey`); `user.token_epoch`
  (`db/migrations/20260822062750_add_user_token_epoch.sql`) and `player.api_key_hash` + unique index
  (`db/migrations/20260924010054_add_player_api_key_hash.sql`) are load-bearing.
- **Test**: none — the artifact must cite every table to its source (Go schema or explicit change)
  so #04 can act on it.
- **Risk**: design issue, MED (a regeneration, not a compat port).
- **dependsOn**: #S1 · **Owns**: `docs/rust-port/schema.md` · **Replaces**: — (consumed by
  #04, #05, #16, #17, #18, #20, #23)

### #S3 — Freeze the config-format contract

- **Build**: enumerate keys verbatim from `conf/configuration.go:33-165` (`configOptions`): root keys
  `Address Port UnixSocketPerm EnforceNonRootUser MusicFolder DataFolder CacheFolder DbPath LogLevel
  BaseURL/BasePath/BaseHost/BaseScheme TLSCert/Key Enable* TranscodingCacheSize ImageCacheSize
  FFmpegPath MPVPath MPVCmdTemplate *ArtPriority LyricsPriority MaxImage(Upload)Size DefaultTheme
  AuthRequestLimit PasswordEncryptionKey Agents …` plus nested sections `Search Matcher ExtAuth
  Plugins HTTPHeaders Prometheus Scanner Jukebox Backup PID Inspect Subsonic Transcoding LastFM Deezer
  ListenBrainz Jellyfin Tags` and `Dev*` (`:134-164`, incl. `DevAPIv1`), free-form prefixes `Tags.`
  and `DevLogLevels.` (`:766-768`), non-settable marker (`configuration.go:34` `ConfigFile`).
  **Carve-out (Part C item 12 + `D-PLAYER26`):** this enumeration is the *Go* inventory to transcribe and classify, not a
  port list. `MPVPath`, `MPVCmdTemplate`, `Jukebox.*`, `Subsonic.*`, `Plugins.*`, `PasswordEncryptionKey`, the compat
  toggle and the cancelled Jellyfin/Inspect sections are **absent from the fresh TOML** — see `config-contract.md` §4 for
  the complete dropped-key list with citations; unknown keys are a startup error, not a warning. Do not reintroduce them
  for flag or inventory parity.
  Verbatim is required because `release/linux/navidrome.toml`, `contrib/*`, `Dockerfile:208`
  (`ND_CONFIGFILE`) are artifacts users already have. Record the deprecation table to implement, not
  skip: 10 renames (`conf/configuration.go:539-549`) — `Scanner.GenreSeparators`,
  `Scanner.GroupAlbumReleases`, `DevEnableBufferedScrobble`, `SearchFullString→Search.FullString`,
  `ReverseProxyWhitelist→ExtAuth.TrustedSources`, `ReverseProxyUserHeader→ExtAuth.UserHeader`,
  `HTTPSecurityHeaders.CustomFrameOptionsValue→HTTPHeaders.FrameOptions`,
  `CoverJpegQuality→CoverArtQuality`, `SimilarSongsMatchThreshold→Matcher.FuzzyThreshold`,
  `EnableTranscodingCancellation→Transcoding.EnableCancellation` — and removals `Spotify.ID`,
  `Spotify.Secret` (`:552`), with "explicit new name wins" precedence (`:623-628`, `:630`).
  Record the semantics layer: case-insensitive keys (`:707-780`), INI `[default]` merge (`:784-800`,
  `conf/testdata/cfg.ini`), ND_-env remap with duplicate hard error (`:589-617`, `:690-692`),
  unknown-key reporting (`:676-690`), value types — `Dir` lazy `MkdirAll` (`conf/dir.go:1-77`),
  byte sizes (`:524-526`, `:857-867`), durations, crontab validation (`:905`),
  `Search.Backend ∈ {fts, legacy}` fallback (`:933-940`).
- **Test**: none — contract must be checkable line-range by line-range against `conf/configuration.go`.
- **Risk**: design issue, HIGH stakes for existing installs.
- **dependsOn**: #S1 · **Owns**: `docs/rust-port/config.md` · **Replaces**: — (consumed by #03, #14,
  #17, #18, #24, #32)

### #S4 — Credential model (settled: B — clean break, no model A anywhere)

- **Build**: a settled decision record — model B is the trunk credential model; there is no open A-vs-B question left.
  - **Trunk credentials (B)**: long-lived hashed API keys + JWT session tokens, revoked per user through `token_epoch` (`db/migrations/20260822062750_add_user_token_epoch.sql`, `persistence/user_repository.go:129-146`, `server/auth.go:320`).
  - Passwords are hashed (argon2id) by default. There is **no reversible AES-GCM password store on the trunk**.
  - **Nothing of model A exists.** With #25 CANCELLED and a fresh service (no existing installs), `t = md5(password+salt)` (`server/subsonic/middlewares.go:281`), `enc:` passwords (`:243`), the `subsonic`/`jellyfin` JWT audiences, and the AES-GCM reversible store keyed `sha256(PasswordEncryptionKey | "just for obfuscation")` (`utils/encrypt.go:15,40`, `core/auth/auth.go:198-205`, `consts/consts.go:47-50`, `persistence/user_repository.go:389,394-450,453,464`) are **never ported**. Passwords are argon2id from day one; no migration command exists. Prefer vetted Rust crates over hand-rolled crypto (`argon2`, `jsonwebtoken`/`jose`, `rand`/`getrandom`) — nothing here is novel cryptography.
  - **First-run bootstrap** replaces any migration: initial admin creation follows the `server/initial_setup.go` pattern. There is no upgrade path and no stored-ciphertext question.
  - Sub-decisions that are not A-specific and must not be silently relaxed: property keys `JWTSecret/JWTPublicSecret/PasswordsEncryptedKey` (`consts/consts.go:33,34,50`); two signing secrets — `TokenAuth` rotation vs `PublicTokenAuth` survival (`core/auth/auth.go:23-29,50,172`); audiences `jellyfin|subsonic|native` (`core/auth/auth.go:33-36`); claim shape (`core/auth/claims.go:11,31-70`); `ExtAuth.TrustedSources` CIDR + unix-`@` case (`server/auth.go:212,418,427`); rate-limit semantics (`server/auth.go:100` 8KiB body, `server/server.go:212`, `server/subsonic/auth_limiter.go:15`, form-body cap 10MB `middlewares.go:34`).
  - `PasswordEncryptionKey` (`consts/consts.go:50`) is dead config — not carried into the fresh TOML.
- **Test**: none (decision record) — but it must name the gated consumers so the tracker can block them.
- **Risk**: design issue, HIGH — gates #13, #14, #18, #19, #24, #26, #27, #28 and DB import (#25/#29/#30/#31 cancelled and own nothing).
- **dependsOn**: #S1 · **Owns**: `docs/rust-port/credentials.md` · **Replaces**: —

Already-fixed shared artifacts needing no design issue: the **ID scheme** (`model/id/id.go` byte-exact
base62/MD5, sentinels `consts/consts.go:214,218`, PID rules `consts/consts.go:157-159`) — owned by
#02; **wire fixtures** (126 goldens, 63 shape variants × JSON/XML, `.gitattributes:1 * eol=lf`) —
reference data only — nothing gates on them (#25 is CANCELLED); #27 and #28 capture their own goldens from the Go binary; **tag vocabulary** (`resources/mappings.yaml`, `model/tag_mappings.go`)
— shipped with #03/#16; **test mocks as spec** (`tests/mock_ffmpeg.go`, `tests/mock_*_repo.go`,
`tests/test_helpers.go`, `tests/navidrome-test.toml`) — consumed by #07, #05, #15.

---

## Part B — implementation issues in build order

### #01 — Pure primitives (`nd-utils`)
- **Build**: pure first — `slice str hasher natural collation shellquote nanoid number gg chrono merge
  jsoncommentstrip index_group_parser` — then effectful `run singleton cache httpclient netguard
  limiter files gravatar encrypt req ioutils pl random context`. Compat traps: `utils/hasher` backs
  the `SEEDEDRAND` SQLite func (`db/db.go:46`), `utils/natural.CompareFold` backs `NATSORT`
  (`db/db.go:49`) — byte-identical or old queries change meaning.
- **Test**: port each `utils/*_test.go` (`utils/str/str_test.go` 14K, `utils/slice/slice_test.go`,
  `utils/shellquote/shellquote_test.go`); property tests for sort/collation over a Go string corpus.
- **Risk**: LOW–MED (the two registered funcs HIGH).
- **dependsOn**: #S1 · **Owns**: `nd-utils/**` · **Replaces**: `utils/**` (21 dirs)

### #02 — ID scheme (`nd-id`)
- **Build**: 22-char base62 ids; `NewHash` = MD5 over args joined by U+200B (`model/id/id.go:1-48`);
  `model/artwork_id.go`, `model/participants.go`; PID rules `consts/consts.go:157-159` and
  `PIDTrack`/`PIDAlbum` overrides (`20241026183640.go:96-97`). Emit sentinels as constants, not
  runtime derivations: `VariousArtistsID = "63sqASlAfjbGMuLP4JhnZU"` (`consts/consts.go:214`),
  `UnknownArtistID` (`consts/consts.go:218`) — legal only while `NewHash` stays byte-exact.
- **Test**: golden round-trips against ids produced by the Go build; length/alphabet assertions.
- **Risk**: HIGH (every stored id in existing DBs).
- **dependsOn**: #S1 · **Owns**: `nd-id/**` · **Replaces**: `model/id/id.go`, `model/artwork_id.go`,
  `model/participants.go`

### #03 — Config loader and constants (`nd-conf`)
- **Build**: TOML + INI loader with the precedence ladder from #S3 (file → ND_-env remap with
  duplicate hard error `conf/configuration.go:589-617`/`:690-692` → INI `[default]` merge `:784-800`),
  case-insensitive keys, unknown-key reporting (`:676-690`); the whole `configOptions` key set; the
  deprecation/removal table; value types (`Dir` `conf/dir.go:1-77`, byte sizes, durations, crontab
  validation `:905`, `Search.Backend` fallback `:933-940`); constants as a typed table
  (`consts/consts.go`, 234 LOC); build-tag equivalents (`conf/buildtags/{netgo,sqlite_fts5,doc}.go`)
  and MIME gating (`conf/mime/mime_types.go`, `resources/mime_types.yaml`).
- **Test**: port `conf/configtest/`; key-set snapshot diff against `consts/consts.go`; assert
  `release/linux/navidrome.toml` and `contrib/*` load unchanged; deprecation-pair table.
- **Risk**: MED (key spelling HIGH).
- **dependsOn**: #S1, #S3 · **Owns**: `nd-conf/**` · **Replaces**: `conf/**`, `consts/consts.go`

### #04 — Schema, migrations, DB maintenance (`nd-dbschema`)
- **Build**: the ~25 schema-shaping files of ~138 in `db/migrations/**` —
  `20200130083147_create_schema.go`, `20240511220020_add_library_table.go`,
  `20241026183640_support_new_scanner.go`, `20260220173400_add_fts5_search.go`,
  `20260722023032_add_artwork_tables.sql`, `20260720015443_uniform_canonical_ids.go` — plus runner,
  open/init (`db/db.go:27,28,32-35,54-59,62,85-89`), `db/optimize.go` ANALYZE scheduling, `db/backup.go`,
  `db/repair.go` (`:12,14` FTS5 repair). Register `SEEDEDRAND` (`db/db.go:46`) and `NATSORT`
  (`db/db.go:49`) per connection. Resolve #S2's strategy question before writing.
- **Test**: DB byte-compat harness — build a DB with the Go binary, open it in Rust, assert table set /
  FK graph / FTS5 triggers / `property` keys survive; per-migration shape goldens;
  `navidrome-test.toml` `DbPath=:memory:` counterpart for unit runs.
- **Risk**: HIGH.
- **dependsOn**: #S1, #S2, #S3, #S4, #01, #02 · **Owns**: `nd-dbschema/**` · **Replaces**: `db/db.go`,
  `db/migrations/**`, `db/optimize.go`, `db/backup.go`, `db/repair.go`

### #05 — Repository layer (`nd-db`)
- **Build**: `persistence/persistence.go` interface layer (`:17-40`), `sql_base_repository.go:28-49`
  builder, 24 `*_repository.go`, `sql_{search,search_fts,search_like,bookmarks,participations,tags,
  annotations,restful}.go`, `criteria_sql.go` (904 LOC), `plugin_cleanup.go`; keep `WithTx/WithTxRetry`
  retry-on-contention and the `SQLStore` indirection the 24 repository interfaces expose. Deliberate
  split: `persistence/criteria_sql.go` lives here, `model/criteria/` (the DSL) lives in #06 — the
  DSL↔SQL bridge is the only touch point.
- **Test**: port `tests/mock_*_repo.go` behavioral specs (`mock_user_repo.go`, `mock_scrobble_buffer_repo.go`,
  `mock_tag_repo.go`, `mock_transcoding_repo.go`, `mock_share_repo.go`, `mock_user_props_repo.go`) as
  Rust fixtures; query tests against a SQLite harness modeled on
  `server/subsonic/e2e/e2e_suite_test.go:417`.
- **Risk**: HIGH (this issue gates ~20 later ones).
- **dependsOn**: #04, #02 · **Owns**: `nd-db/**` · **Replaces**: `persistence/**`

### #06 — Model / DTO layer (`nd-model`)
- **Build**: interface-only structs mirroring `model/*.go`: entity DTOs, `model/criteria/*` (search DSL +
  JSON form), `model/{searchable,get_entity,library_matcher}.go`, `model/tag.go`, `model/annotation.go`,
  `model/scanner.go`, lyric DTOs; tag-vocabulary loader contract (`model/tag_mappings.go`, also consumed
  at `model/criteria/fields.go:147`). Pure transforms only — the SQL half is #05's.
- **Test**: JSON round-trips against shapes pinned in `server/subsonic/responses/.snapshots/`; port
  `model/criteria/*_test.go`; field-order tests where the wire depends on order.
- **Risk**: MED.
- **dependsOn**: #S1, #02, #03 · **Owns**: `nd-model/**` · **Replaces**: `model/*` (DTO half),
  `model/criteria/*`, `model/{searchable,get_entity,library_matcher,errors}.go`

### #07 — ffmpeg process adapter (`nd-ffmpeg`)
- **Build**: `core/ffmpeg/ffmpeg.go` — `Transcode / ExtractImage / ConvertAnimatedImage / ProbeAudioStream /
  CmdPath / IsAvailable`; FLAC `STREAMINFO` patching (`core/ffmpeg/flac_streaminfo.go`). Hard requirement:
  cancel kills the child process group, no zombies, no orphaned stdout.
- **Test**: `tests/mock_ffmpeg.go` rebuilt as a Rust mock binary; `tests/fixtures/**` containers
  (`test.aiff m4a ogg opus wav wma`, invalid-frame, symlink chains); golden patched FLAC header; cancel-soak
  asserting zero surviving PIDs.
- **Risk**: HIGH.
- **dependsOn**: #S1, #01 · **Owns**: `nd-ffmpeg/**` · **Replaces**: `core/ffmpeg/**`

### #08 — Tag extractor (`nd-tagreader`)
- **Build**: `adapters/gotaglib/gotaglib.go`, `core/storage/**`, `core/storage/local/extractors.go` —
  `Extractor{Parse → map[string]Info, Version}`, `RegisterExtractor("taglib")`. Go pins the fork
  `github.com/deluan/go-taglib` (`go.mod:5-6`); choose once here: C bindings, pure Rust, or a shim —
  the contract above must survive whichever you pick.
- **Test**: per-container expected `Info` maps over `tests/fixtures/**`; diff against output produced by the
  Go build (Go tags are the oracle).
- **Risk**: HIGH (FFI/WASM equivalence unresolved).
- **dependsOn**: #S1, #01 · **Owns**: `nd-tagreader/**` · **Replaces**: `adapters/gotaglib/**`, `core/storage/**`

### #09 — Lyrics parsers, pure half (`nd-lyrics/src/parsers`)
- **Build**: `model/lyrics_{lrc,srt,ttml}.go`, `model/lyrics_normalize.go`, `model/lyrics_parse.go` — parse +
  normalize only. BOM and time-axis normalization are the compat traps. Fetch/merge is #22.
- **Test**: parser goldens from `tests/fixtures/` (LRC BOM case); normalize round-trip idempotence.
- **Risk**: MED.
- **dependsOn**: #S1, #01 · **Owns**: `nd-lyrics/src/parsers/**` · **Replaces**: `model/lyrics_*.go`

### #10 — Playlist parsers (`nd-playlists/src/parsers`)
- **Build**: `core/playlists/{parse_m3u,parse_nsp}.go` and import semantics (`PlaylistsImportPending` key,
  `consts/consts.go:19`); the `rest_adapter.go` shape. Repo wiring lands in #23.
- **Test**: parser table (extended M3U directives, BOM, CRLF, nested playlists, comments); NSP branch golden;
  `model/playlist.go` DTO round-trip.
- **Risk**: MED.
- **dependsOn**: #S1, #01, #06 · **Owns**: `nd-playlists/src/parsers/**` · **Replaces**: `core/playlists/{parse_m3u,parse_nsp,import}.go`

### #11 — Events / SSE (`nd-events`)
- **Build**: `server/events/*` — SSE `/api/events`, `Event`/`Broker` broadcast, drop-on-disconnect.
- **Test**: frame-shape test for event names scanner/artwork workers emit later; broker fan-out table;
  disconnect cancellation.
- **Risk**: LOW.
- **dependsOn**: #S1, #03 · **Owns**: `nd-events/**` · **Replaces**: `server/events/*`

### #12 — Scheduler, logging, maintenance (`nd-sched`)
- **Build**: `scheduler/{scheduler,crontab_schedule,log_adapter}.go`, `log/{log,formatters,journal,redactrus}.go`,
  `core/maintenance.go`. Crontab parsing must agree with the `ScanSchedule` validator path
  (`conf/configuration.go:905`). Keep the redaction set (`log/redactrus.go`) — user-visible.
- **Test**: port `scheduler/scheduler_test.go` and `crontab_schedule_test.go` (5.8K); redaction table;
  maintenance-op exit codes (CLI wiring deferred to #33).
- **Risk**: LOW.
- **dependsOn**: #S1, #03 · **Owns**: `nd-sched/**` · **Replaces**: `scheduler/**`, `log/**`, `core/maintenance.go`

### #13 — Credential and claim model (`nd-auth`)
- **Build**: implement #S4's settled choice (model B; the `p`/`t+s`/`enc:` arms of the Go credential switch are NOT ported at all — #25 is CANCELLED). `core/auth/auth.go`, `core/auth/claims.go:11,31-70`,
  `server/auth.go`, `server/initial_setup.go`, the `apiKey`-JWT-internal arms of the credential switch in
  `server/subsonic/middlewares.go` (the `p` and `t+s` arms are NOT ported — #25 CANCELLED), `server/subsonic/auth_limiter.go:15`
  (fail-closed limiter), `core/publicurl/publicurl.go`. Modern-Rust auth: vetted crates only (`argon2`, `jsonwebtoken`/`jose`,
  `rand`/`getrandom`) — dual signing secrets (`core/auth/auth.go:23-29,50,172`), epoch revocation
  (`persistence/user_repository.go:129-144`), audience discipline `native` only (Part C #10; `jellyfin` audience removed with #30,
  `subsonic` never minted), the 3-way credential switch (jwt / apiKey / internal),
  `X-ND-Authorization` (`consts/consts.go:31`), `ExtAuth.TrustedSources` CIDR + unix-`@`
  (`server/auth.go:212,418,427`), body caps (`server/auth.go:100` 8KiB, `middlewares.go:34` 10MB).
- **Test**: claim/verdict table across the 3 credential forms (aud-less tokens REJECTED once the
  discipline settles); cross-audience rejection cases; epoch
  invalidation; rate-limit window tests; initial-setup flow driven from `conf/testdata`.
- **Risk**: HIGH (security) — blocks every wire surface.
- **dependsOn**: #05, #03, #01, #S4 · **Owns**: `nd-auth/**` · **Replaces**: `core/auth/**`, `server/auth.go`,
  `server/initial_setup.go`, auth slice of `server/subsonic/middlewares.go`, `server/subsonic/auth_limiter.go`,
  `core/publicurl/**`

### #14 — Transcode decision tables (`nd-stream/src/decision`)
- **Build**: `core/stream/{types,decider,codec,limitations,aliases,legacy_client,token}.go` —
  `TranscodeDecider{MakeDecision, CreateTranscodeParams, ResolveRequestFromToken}`; defaults at
  `consts/consts.go:176 DefaultTranscodings`; DB history `20200706231659_*`, `20260309203355_*`,
  `20260618120509_*`. Pure tables first; the token half needs #13 — that is why #14 waits for it.
- **Test**: `core/stream/decider_test.go` (~1300 LOC) is the spec, port it as the decision table;
  `codec_test.go`, `aliases_test.go`, `token_test.go` as branch tables.
- **Risk**: HIGH (protocol tables).
- **dependsOn**: #07, #13, #06, #05 · **Owns**: `nd-stream/src/decision/**` · **Replaces**: `core/stream/{types,decider,codec,limitations,aliases,legacy_client,token}.go`,
  `model/transcoding.go`

### #15 — Stream serving, Range, archiver (`nd-stream`)
- **Build**: `core/stream/media_streamer.go` (`NewStream/Serve`), `core/stream/limiter.go`, `core/archiver.go`,
  `server/public/handle_streams.go`. Cancellation end-to-end (client hangup kills #07's child) and the
  transcoding-file cache singleton's eviction contract.
- **Test**: Range matrix (partial, beyond-EOF, zero-length, malformed header); cancellation soak with the
  ffmpeg mock; zip-layout goldens for the archiver.
- **Risk**: HIGH.
- **dependsOn**: #14, #07 · **Owns**: `nd-stream/**` minus `src/decision/**` · **Replaces**: `core/stream/media_streamer.go`,
  `core/stream/limiter.go`, `core/archiver.go`, `server/public/handle_streams.go`

### #16 — Tag/metadata mapping, `Info` → entities (`nd-mapping`)
- **Build**: `model/metadata/*` — `map_mediafile`, `map_participants`, `persistent_ids`, `legacy_ids`;
  `model/tag_mappings.go`, `model/file_types.go`, format branches via `adapters/gotaglib`. This is the ID
  canonicalization step: wrong here, DBs diverge silently.
- **Test**: golden `Info → MediaFile/ArtistRole` round-trips over the fixture corpus; PID rules and legacy-id
  fallback assertions; multi-valued tag split table.
- **Risk**: HIGH.
- **dependsOn**: #02, #08, #03, #06 · **Owns**: `nd-mapping/**` · **Replaces**: `model/metadata/*`, `model/tag_mappings.go`, `model/file_types.go`

### #17 — Scanner (`nd-scanner`)
- **Build**: `scanner/` 12 src files — `controller.go`, `scanner.go`, `walk_dir_tree.go`, `ignore_checker.go`,
  `watcher.go`, `phase_1_folders.go`–`phase_4_playlists.go`, `image_changes.go`, `folder_entry.go`, `external.go`;
  contract `ScanAll/ScanFolders/Status`, `New/CallScan/IsScanning/LockForMaintenance`, `ProgressInfo` events
  (consumed by #11). Read `scanner/README.md` first. Three hard surfaces: symlink semantics
  (`ignore_checker.go` + `walk_dir_tree.go`), subprocess IPC (external calls), fsnotify watching — plus phase
  ordering and rescan idempotence. It feeds the `artwork_queue` drain in #18 and the FTS5 triggers in #04.
- **Test**: port `scanner/scanner_test.go` (50K), `scanner_multilibrary_test.go` (39K), `walk_dir_tree_test.go`
  (27K), `watcher_test.go` (17K), `scanner_selective_test.go` (12K); symlink-chain fixtures from `tests/fixtures/`;
  assert an idempotent second pass mutates no ids.
- **Risk**: HIGH (XL, ~3.4k src / 7k test LOC).
- **dependsOn**: #05, #06, #16, #08, #11 · **Owns**: `nd-scanner/**` · **Replaces**: `scanner/**`, `model/scanner.go`

### #18 — Artwork pipeline (`nd-artwork`)
- **Build**: `core/artwork/**` — `artwork.go`, `worker.go`, `image_cache.go`, `image_store.go`, `sources.go`,
  `processor.go`, `resize.go`, `resolve.go`, `gate.go`, `housekeeping.go`, `prune.go`, `uploader.go`, `disc.go`,
  `animation.go`, `blurhash/`, `thumbhash/`, `dominant/`, `wire_providers.go`, `folders_album.go`, `folders_artist.go`,
  `agent_images.go`, `library_fs.go`, `trace.go`; plus `model/artwork.go` and the `artwork_queue` drain contract
  (`db/migrations/20260722023032_add_artwork_tables.sql:1-46`, covering index
  `ix_artwork_queue_drain(item_kind, priority DESC, enqueued_at, retry_at)`). `thumbhash`/`blurhash` must be byte-compatible
  with `ui/src/utils/thumbhash.js` — `core/artwork/thumbhash/testdata/{golden.json,generated.json,thumbhash.js,gen_*.mjs}`
  is the oracle.
- **Test**: thumbhash/blurhash goldens first (deterministic, cheap); queue-drain ordering against the index columns;
  resolution/gate tests from `resolve_test.go`/`sources_internal_test.go`/`gate_test.go`; drain throughput modeled on
  `worker_soak_test.go`/`worker_timing_test.go`.
- **Risk**: HIGH.
- **dependsOn**: #05, #07, #08, #17 · **Owns**: `nd-artwork/**` · **Replaces**: `core/artwork/**`, `model/artwork.go`

### #19 — Public share pages (`nd-public`)
- **Build**: `server/public/{public,handle_shares,handle_images,handle_downloads}.go`, `core/share.go`,
  `core/publicurl`, `model/share.go`, `persistence/share_repository.go`. Unauthenticated surface: token handling,
  `Content-Disposition` quoting (`utils/str/content_disposition.go` contract), share-image/stream handlers, the
  `ShareInfo` template payload for `ui/index.html`.
- **Test**: token matrix (expired / revoked / malformed / replay); disposition quoting cases; string sanitization against
  `utils/str/sanitize_strings.go` and `utils/str/normalize_fts.go` behavior.
- **Risk**: HIGH (public unauth surface).
- **dependsOn**: #05, #13, #15, #18 · **Owns**: `nd-public/**` · **Replaces**: `server/public/{public,handle_shares,handle_images,handle_downloads}.go`,
  `core/share.go`, `core/publicurl/**`, `model/share.go`

### #20 — Scrobbler (`nd-scrobbler`)
- **Build**: `core/scrobbler/{interfaces,play_tracker,buffered_scrobbler,nowplaying_worker,playbackreport_worker}.go`,
  `model/scrobble.go`, `model/scrobble_buffer.go`, `persistence/{scrobble,scrobble_buffer}_repository.go`. Preserve
  `ErrRetryLater` *identity* (callers branch on the error value) and the buffered flush window
  (`DevEnableBufferedScrobble` is a deprecated key — #S3).
- **Test**: port `tests/mock_scrobble_buffer_repo.go` / `mock_scrobble_repo.go` semantics; now-playing expiry clock table;
  retry-later round-trip.
- **Risk**: MED.
- **dependsOn**: #05 · **Owns**: `nd-scrobbler/**` · **Replaces**: `core/scrobbler/**`, `model/scrobble.go`, `model/scrobble_buffer.go`

### #21 — External metadata agents + matcher (`nd-agents`)
- **Build**: `core/agents/*` capability structs, `core/external/{provider,provider_similarsongs}.go`, `core/matcher/matcher.go`,
  `adapters/{lastfm,listenbrainz,deezer}/*`, `core/wire_providers.go` DI selection. The matcher is flagged red in the graph —
  budget real work, not a straight port.
- **Test**: canned HTTP from `tests/fixtures/**` provider responses (lastfm/deezer/listenbrainz JSON + HTML); matcher fuzz
  table against `Matcher.FuzzyThreshold` (renamed key, #S3); provider-selection tests for the `Agents` config key.
- **Risk**: MED–HIGH.
- **dependsOn**: #05, #01, #13 · **Owns**: `nd-agents/**` · **Replaces**: `core/agents/*`, `core/external/**`, `core/matcher/matcher.go`, `adapters/{lastfm,listenbrainz,deezer}/*`, `core/wire_providers.go`

### #22 — Lyrics fetch + merge (`nd-lyrics`)
- **Build**: `core/lyrics/{lyrics,sources}.go` — source resolution and merge precedence against `LyricsPriority`; parsers come
  from #09. Handler lives in #27 (no #25 exists).
- **Test**: merge-precedence table; canned provider responses; BOM/time-axis cases from `tests/fixtures/`.
- **Risk**: MED.
- **dependsOn**: #06, #09, #05, #21 · **Owns**: `nd-lyrics/**` minus `src/parsers/**` · **Replaces**: `core/lyrics/**`

### #23 — Playlists, store + import (`nd-playlists`)
- **Build**: `core/playlists/{playlists,rest_adapter}.go` + repository layer (`persistence/{playlist,playlist_track}_repository.go`),
  `model/playlist.go`; import semantics and the pending flag from #10.
- **Test**: import round-trip through the repo layer; smart-playlist refresh (`SmartPlaylistRefreshDelay` key); DTO round-trip.
- **Risk**: MED.
- **dependsOn**: #05, #06, #10 · **Owns**: `nd-playlists/**` minus `src/parsers/**` · **Replaces**: `core/playlists/{playlists,rest_adapter}.go`, `model/playlist.go`

### #24 — HTTP shell and middleware pipeline (`nd-http`)
- **Build**: `server/server.go`, `server/middlewares.go`, `server/serve_index.go`, `server/throttle_backlog.go`, `server/filter/filters.go`,
  `server/imghttp/headers.go`, `consts/consts.go:57-63`. BasePath handling, HTML template injection, image ETag/304, backlog
  throttling. Mount order and header-merge order are byte-visible.
- **Test**: middleware-order table; BasePath prefix cases; ETag/304 conditional matrix; port `server/throttle_backlog_test.go` (7.5K) as the
  queueing spec.
- **Risk**: MED.
- **dependsOn**: #13, #03 · **Owns**: `nd-http/**` · **Replaces**: `server/{server,middlewares,serve_index,throttle_backlog}.go`, `server/filter/filters.go`, `server/imghttp/headers.go`

### #25 — Subsonic/OpenSubsonic wire layer — CANCELLED (clean API break, not ported) (`nd-subsonic-compat`)

- **Cancelled**: no compat shim, no wire translation; `server/subsonic/**` and the Subsonic-era auth surface are not ported. Rationale (settled, Revision item 2): a faithful `t+s` shim requires the reversible password store back (`server/subsonic/middlewares.go:281`), re-importing the security debt model B removed; `apiKey`-over-`/rest` works only for clients that can send `apiKey`, which the major Subsonic clients cannot; and the shim would permanently pull canonical design toward Subsonic's lowest-common-denominator wire shapes. Third-party Subsonic clients (play:Sub, Symfonium, Feishin, DSub…) will not work against this server — a deliberate product decision, to be stated in the fork README.
- **What survives**: the 126 goldens under `server/subsonic/responses/.snapshots/` stay in-tree as **reference data** (a recorded behavioral spec of field shapes and streaming/Range/ETag semantics) consulted while authoring #27 — they gate nothing; the fixture *discipline* transfers to the canonical API (#27 captures its own recorded goldens from the Go binary). Subsonic *semantics* worth borrowing are recorded in #27's Build: typed-ID prefixes `al-`/`mf-`/`pl-` (`model/artwork_id.go`), artwork-ID composition `dc-<album>:<disc>`, the transcode-decision split (`core/stream/decider.go`). A future community Subsonic adapter, if ever wanted, would consume #27's published OpenAPI spec — a separate artifact, not a server lane.
- **Risk**: — (not ported). **dependsOn**: — · **Owns**: — · **Replaces**: — (the Go paths retire unported)

### #26 — Player-session service (`nd-players`); mpv/jukebox device service **DROPPED**
- **Scope split (DECIDED 2026-11-18, user-approved `D-PLAYER26`)**: #26 bundled two unrelated things; they are now separated.
  **Retained:** the player-session service — `core/players.go` `Get`/`Register`/`Touch` plus the `model/player.go` record
  fields (`MaxBitRate`, `TranscodingId`, `ScrobbleEnabled`, `HasAPIKey`/`APIKey`, and the retained `report_real_path`
  column). Note that this lifecycle does **not** exist on the canonical surface today: Go registers/touches players only
  on the cancelled Subsonic/Jellyfin surfaces (`server/subsonic/middlewares.go:291-316` mounted on 13 of the 14 protected
  groups in `server/subsonic/api.go`; `server/jellyfin/middlewares.go:193-224`), while the native API exposed plain
  rest-CRUD `/player` (`server/nativeapi/native_api.go:75`) and never registered or touched. So building this service is a
  deliberate canonical choice (placement belongs to API-01), not inherited behavior. The fields feed surviving services:
  the transcode decision (`core/stream/decider.go:159-161`, already ported and tested in `nd-stream/src/legacy_client.rs`
  as `player_max_bit_rate` — currently dead because nothing persists the row and no production caller sets that field),
  and the scrobbler's `ScrobbleEnabled` gate (`core/scrobbler/play_tracker.go:366,436,473`). `ReportRealPath` is a retained
  column whose only non-test reader is the cancelled wire (`server/subsonic/helpers.go:227`) and whose seeding key
  `Subsonic.DefaultReportRealPath` is itself dropped (`config-contract.md:220`); its canonical meaning must be re-specified
  in FRONT-26/API-01. Key issuance/revocation consumes the repository (`set_api_key` `player_repo.rs:1305`), not
  `Register`/`Touch`. The DB half is already **DONE** in `nd-db/src/player_repo.rs` (CRUD, `find_match`, API-key
  hash/`find_by_api_key`); only the thin service lifecycle above it is missing. It has **no mpv dependency**.
  **Dropped:** the server-side playback device / mpv IPC service — `core/playback/{playbackserver,device,queue}.go`,
  `core/playback/mpv/**`, `server/subsonic/jukebox.go`. Rationale: its only entry point is Subsonic
  (`server/subsonic/api.go:220-226`), cancelled by #25; no canonical native route exposes it and no **surviving** code
  consumes `core/playback/**` (`GetDeviceForUser` is only "the default device" `playbackserver.go:117-127`, called solely
  from `jukebox.go:47`; the full consumer set — DI/boot plumbing `core/wire_providers.go:35`, `cmd/wire_injectors.go:150`,
  `cmd/wire_gen.go:22,112,227-230`, `cmd/root.go:90,372-382`, and three cancelled-surface e2e suites passing
  `PlaybackServer(nil)` — is listed with evidence in `subplans/README.md`); it is off by default
  (`conf/configuration.go:1058-1061`) and `cmd/root.go:372-382` no-ops when disabled; the settled config contract
  already drops `Jukebox.*`, `MPVPath`, `MPVCmdTemplate` (Part C item 12, `config-contract.md:222-223`); the bundled JS UI
  that reached it is dropped (C25 — the ~47k-LOC `ui/src` tree; C32 is backgrounds, a separate drop, and the only surviving
  JS mention is a `[Jukebox]` config-dump label in `ui/src/dialogs/aboutUtils.test.js:368,379`); and it spawns host
  processes needing OS-specific sockets plus process reap that upstream itself leaves unfinished
  (`playbackserver.go:54` "Should confirm all subprocess are terminated"). `Jukebox.Enabled`'s only other readers are
  non-surviving scopes: `server/subsonic/users.go:29-30` (`jukeboxRole`, cancelled) and `core/metrics/insights.go:218`
  (#32, deferred). Nothing else loses capability.
- **Per-device editing (DECIDED 2026-11-18, user-approved `D-PLAYER-EDIT`)**: owner-scoped `GET`/`PUT /api/player/{id}` is
  retained canonical scope, not an accident of Go rest-CRUD. The repository already enforces the safety shape:
  `PlayerRepo::read(id,user)` (`player_repo.rs:1129`) is the ownership-restricted read (vs unrestricted `get` :1119), and
  `update_owned` (:1345, `OwnerOrAdmin`) builds a SET that drops `user_id`/`created_at` (`:961`), mirroring Go
  `updateOwned` (`persistence/sql_base_repository.go:506-514`, which deletes `user_id` because ownership is immutable on
  update) with `ownedRow` (`:97-102`) restricting the write; the field whitelist is `:538-549`. `MaxBitRate` is already
  consumed by the ported decision, so bitrate editing is functional once a row persists. What is **missing** is the picker:
  a read-only `GET /api/transcoding` (caller applies `mask_command`/`mask_commands`, `transcoding_repo.rs:60,69`), plus
  #05 executing the transcoding read and a BOOT-owned default seed (`nd-conf` `DEFAULT_TRANSCODINGS`,
  `consts.rs:242`, 4 entries; fresh DDL seeds nothing, `schema_ddl.rs:56`). Profile **editing** stays dropped (Part C
  item 1); `EnableTranscodingConfig` stays **carried but unused** (`keys.rs:189-193`, `config.rs:435,:662` — parsed,
  no consumer, no editor route); `report_real_path` is not exposed as a knob until re-specified; `apiKey`/hash is never
  serialized. Status parity is Go’s: cross-owner read 404 (`player_repository_test.go:163-167`), cross-owner write 403
  (`classifyOwnedWriteMiss`, `sql_base_repository.go:554-563`). No player route is mounted today — scope, not delivered
  behavior.
  **Edge guardrails (audit-verified 2026-11-18):** `PUT` must call `update_owned` with an explicit non-empty column list and
  never `PlayerRepo::put` (`player_repo.rs:922-947` re-owns a foreign row: `UPDATE player SET user_id=? WHERE id=?`, no
  ownership predicate; measured `Ok(1)` + `user_id` changed in `/tmp/fkprobe2`, Go parity identical); never
  `LoggedUser::headless()` for a user request; never persist a client-supplied `lastSeen`; never `{:?}`-log a `PlayerRecord`
  (`Debug` at `:206` + `api_key: Option<String>` at `:235` leaks the plaintext key). Clamp `maxBitRate`, validate that
  `transcoding_id` exists (no FK: `schema_ddl.rs:65`), require non-empty `name`/`client`, and never pass request text into
  `Options::filters` (raw SQL fragment, `:155`).
- **Build**: player register/touch/get lifecycle + write-throttled persistence over the existing `PlayerRepo` — Go has no
  expiry mechanism, only `LastSeen = time.Now()` stamping (`core/players.go:78`) and a write limiter at
  `consts.UpdatePlayerFrequency` = 1 minute (`core/players.go:26`, `consts/consts.go:89`) — plus the `model/player.go`
  field shapes and a canonical decision on `report_real_path`. Deliberately **not** a monolithic crate needing all of
  #05/#13/#15: it lands as API/BOOT-adjacent work, with the resolved player/transcoding pair feeding stream decisions and
  the scrobbler (key issuance itself only needs `PlayerRepo::set_api_key`, Part C item 4).
- **Test**: `Players.Get/Register/Touch` write-throttle (limiter) + `LastSeen`-stamping tests; register reuse rules (client/user
  mismatch re-registers, `FindMatch` reuse), owner-scoped persistence, and the stream decision actually receiving a persisted
  `MaxBitRate` end-to-end. No mpv/fake-device harness — that scope is dropped.
- **Risk**: LOW–MED (was MED with mpv). The retained half is small and mostly sits on already-landed player-repo work,
  but it inherits the pending transcoding-read/seed dependency above.
- **dependsOn**: #05 (player repo — landed `c8c71f8d`, 2127 lines; **plus** the still-pending executed `transcoding_repo`
  get and seeded default transcodings, since Go's `refresh` loads the transcoding row — `core/players.go:87-91`, and now a
  hard prerequisite of `D-PLAYER-EDIT`'s profile picker), #13 ·
  **Owns**: `nd-players/**` · **Replaces**: `core/players.go`, `model/player.go` (mpv/playback paths retire unported)

### #27 — Canonical API surface (`nd-api`) (TRUNK)

- **Build**: this is the trunk — the surface the owners' Flutter client and the MCP client artifact speak. `server/nativeapi/{native_api,plugin}.go`, `server/apiv1/api.go`, `api/bundled/openapi.{json,yaml}`, `api/embed.go`; mounted at `consts/consts.go:58` `/api`. The chi handlers are the source of truth; the OpenAPI document is **regenerated in Rust from the registered routes**, not shipped verbatim — today `api/bundled/openapi.json` documents only `/server`, `/openapi.json`, `/openapi.yaml`, a known gap, so "served byte-for-byte" is not the contract. `/api/plugin*` routes (`server/nativeapi/plugin.go:16-30`, DI binds `cmd/wire_gen.go:271`) become 404-or-empty per the Decisions section (Part C, item 14).
- **Test**: response-shape goldens captured from the Go binary **before** this issue starts (as today) + OpenAPI self-consistency against the registered route set; error-envelope table. The 126 Subsonic goldens are reference data only — they gate nothing.
- **Risk**: HIGH (external-client contract).
- **dependsOn**: #13, #05 · **Owns**: `nd-api/**` · **Replaces**: `server/nativeapi/**`, `server/apiv1/api.go`, `api/**`

### #28 — Share-page + root HTML contract (`nd-ui-contract`)

- **Build**: server side only — `ui/index.html` placeholders `{{ .AppConfig }}` (`:35`), `{{ .ShareInfo }}` (`:38`), rendered at `server/serve_index.go:122-136` and `:183`. The bundled react-admin JS UI (~47k LOC under `ui/src`) is **DROPPED** by default: a Flutter client replaces it, so nothing here may depend on it.
- **Test**: template-variable presence test for `AppConfig`/`ShareInfo`; base-path/URL assembly for the share URLs the payloads carry. (No JS-client test remains — the `t+s` token flow it exercised has no client any more.)
- **Open decision**: once the JS UI is dropped, what is `/` (root mount `server/server.go:161-167`, `:61`) — redirect, 404, or a landing page? Unresolved; see Part C, item 13. Share pages reuse the same template (`serve_index.go:183 ShareInfo`), so the choice must not break them.
- **Risk**: HIGH (compat).
- **dependsOn**: #27, #24, #19, #11 · **Owns**: `nd-ui-contract/**` · **Replaces**: the server half of `ui/**` (`ui/embed.go`, `ui/index.html` template data)

### #29 — Plugin host — CANCELLED (not ported)

- **Cancelled**: the Extism/wazero host ABI + sandbox (`plugins/{manager,manager_loader,manager_call}.go`, `capabilities*`, `host*`, `sandbox_fs.go`, `host_netguard.go`, `manifest*`, `package.go`, `plugins/cmd/ndpgen/*` codegen, `go.mod:20,58` — 14 host services, ~250 files) was the largest single lane and serves nothing the canonical API (#27) or the clients need. The capability model survives only as data: `plugins/capabilities/*.yaml` is a trait spec for a future Rust-native provider layer. The `PluginLoader`/`PluginManager` DI binds in `cmd/wire_gen.go:271` are simply absent. The GPL-3.0 Rust PDK crates `plugins/pdk/rust/nd-pdk*` and the `plugins/cmd/ndpgen` codegen are not ported.
- **Owns**: — (no crate) · **dependsOn**: — · **Replaces**: — (server-side plugin routes are handled by #27 as 404)

### #30 — Jellyfin API — CANCELLED (not ported) (`nd-jellyfin`)
- **Cancelled** (user decision, Part C #15 resolved): the Jellyfin-compatible surface is not ported. Rationale: it serves only third-party Jellyfin clients — the same ecosystem argument as the cancelled Subsonic shim — and its in/out flow was always DEFERRABLE, never on the spine. The `jellyfin` JWT audience is removed from #13/#S4 (mint and accept `native` only). **Quick-Connect survives**: `core/quickconnect/**` is not Jellyfin-specific and is re-owned by #27 (`nd-api`), which implements `/api/quickconnect` (`server/nativeapi/quickconnect.go:24-33` lookup/authorize device-pairing flow) with its own recorded fixtures.
- **Owns**: nothing · **Replaces**: — (`server/jellyfin/**` not ported; `core/quickconnect/**` → #27)

### #31 — Sonic similarity — CANCELLED (not ported) (`nd-sonic`)

- **Cancelled with #25**: its only consumers were `server/subsonic/api.go:129` (`getSonicSimilarTracks`) and the dropped JS UI (`ui/src/subsonic/index.js:126`) — both gone post-clean-break. Archive with a revival path if a canonical similar-tracks endpoint is ever wanted.
- **Risk**: — (not ported). **dependsOn**: — · **Owns**: — · **Replaces**: — (`core/sonic/sonic.go`, `server/subsonic/sonic_similarity.go` retire unported)

### #32 — Diagnostics / metrics (DEFERRABLE) (`nd-metrics`)
- **Build**: `core/metrics/{insights,prometheus,insights_linux,data}.go`, `core/inspect.go` — Prometheus text + library insights. Tied to DB keys `LastDBAnalyzeAt`,
  `LastDBAnalyzeAttemptAt`, `DBAnalyzePending`, `DBAnalyzeFailureCount` (`consts/consts.go:23-26`) and the insights key (#S3).
- **Test**: Prometheus text golden per metric group; port `core/inspect_test.go` for the insights shape.
- **Risk**: MED, DEFERRABLE.
- **dependsOn**: #05, #03 · **Owns**: `nd-metrics/**` · **Replaces**: `core/metrics/**`, `core/inspect.go`

### #33 — CLI and service entrypoints (`nd-cli`)
- **Build**: `cmd/*.go` (root/scan/inspect/doctor/backup/pls/missing/artwork/plugin/svc/user/search) plus the hand-written replacement for `cmd/wire_gen.go` +
  `cmd/wire_injectors.go` DI codegen — there is no Rust analogue, so write explicit construction in `nd-cli/src/main.rs`. Subcommand surface stays flag-compatible;
  DI scaffolding may stay stubbed until the lanes stabilize (flagged in the graph as scaffold-not-final).
- **Test**: `--help` and exit-code tables per subcommand; arg-parse round-trip against Go; registry snapshot diff against `cmd/`.
- **Risk**: HIGH.
- **dependsOn**: #04, #05, #03, #17, #12, #32 · **Owns**: `nd-cli/**` · **Replaces**: `cmd/*.go`

### #34 — Backgrounds / wallpaper — DROPPED (not ported) (`nd-backgrounds`)

- **Dropped** (Part C #17 resolved): its sole consumer was the JS UI's login background fetch (`cmd/root.go:153` mount of `conf.Server.UILoginBackgroundURL`, `consts/consts.go:71`) — dropped with #28. Re-open only if the Flutter client asks for it.
- **Risk**: — (not ported). **dependsOn**: — · **Owns**: — · **Replaces**: — (`server/backgrounds/handler.go` retires unported)

## Dependency spine (for the tracker's `blockedBy` fields)

- **Spine gate**: #S1 → {#S2,#S3,#S4} → #04 → #05. ~20 issues are `blockedBy` #05; nothing runtime-bearing compiles before it lands.
- **Leaf lane** (parallel with the spine): #01, #02, #03, #06, #11, #12, #09, #10 — no DB writes.
- **Media adapters** (parallel with the spine): #07, #08, #09 — consume #01 only.
- **Auth**: #13 gates #14, #15, #19, #24, #26, #27, #28 (#25/#29/#30 cancelled and own nothing).
- **Stream chain** (strict): #07 → #14 → #15 → #19. Streaming reaches the wire only through #27's canonical endpoints. #26's
  retained player-session service feeds the stream decision (persisted `MaxBitRate`) and key issuance but is not a
  prerequisite for streaming itself; its dropped mpv half gated nothing.
- **Scan pipeline**: #08 → #16 → #17 → #18. #17 feeds the `artwork_queue` drain in #18 and the FTS5 triggers in #04.
- **Wire lane** (last): #24 → #27 → #28 — canonical surface only.
- **Satellites** (last, concurrent): #20, #21, #22, #23, #32, #33.
- **Deferred tail**: none (#30 CANCELLED).

## Not ported

CANCELLED / DROPPED issues (kept for number stability; each retires its Go paths unported): #25 Subsonic wire layer, #29 WASM plugin host, #31 sonic similarity, #34 backgrounds. State the Subsonic ecosystem loss (third-party Subsonic clients will not work) in the fork README.
No issue needed: `git/pre-commit`, `git/pre-push`, `.github/`, `release/`, `contrib/`, `.devcontainer/`, `scripts/setup-worktree.sh`, `bench/` — repo/CI tooling only.
Re-check `release/linux/navidrome.toml` and `Dockerfile:208` whenever config keys change: #03 owns that coupling.
MCP is a separate client artifact, not part of this Rust workspace: it speaks the canonical API (#27) with one API key, is not GPL-encumbered, and needs no crate.

---

## Part C — Contract decisions (settled at #S1; all items decided)

These are the open contract questions the revision did not settle. Each names the issues it blocks; none of them may be resolved silently inside a later issue.

1. **Canonical API KEEP/DROP per route — DECIDED**: keep CRUD + `search` + share + admin/account + `quickconnect` (re-owned per item 15); drop `/inspect`, `/config/*`, `/plugin*` (item 14), `/transcoding` config editor (#32 dropped); `/share` kept only as share-page HTML (#28). Blocks #27.
2. **Native equivalents for Subsonic-only features — DECIDED (PRIORITY #1: add them to the canonical surface)** — with #25 CANCELLED, playback has no path at all. Decisions (endpoint proposals, not validated designs): `GET /api/stream`, `GET /api/download`, `GET /api/artwork`, `GET /api/search` modelled on the Go `/rest` implementations (`server/subsonic/stream.go`, `download.go`, `artwork.go`, `search3.go`) and the native CRUD shapes in `server/nativeapi/native_api.go:59-104`; bookmarks/radio/play-queues/lyrics keep their semantics (they are Subsonic-only *features* in the code, not a wire contract). Their fixtures are **authored by transcribing the Go `/rest` behavior** (Range/ETag headers, `server/subsonic/stream.go` + `core/stream/token.go:106,111` transcode decision) BEFORE #27/#15 open. Blocks #27, #15, #19.
3. **Streaming URL contract — DECIDED (PRIORITY #2: canonical `GET /api/stream?...` forms)** — the Flutter client (built after the server translation) speaks the canonical forms from item 2, not `/rest/stream?u&t&s` (`ui/src/subsonic/index.js:17-22`) or `getTranscodeStream?...transcodeParams=<jwt>&offset=` (`ui/src/transcode/decisionService.js:73-83`). Share URLs keep the share-page HTML contract (#28, `ui/src/utils/urls.js:28-40`).
4. **API-key issuance on the canonical surface — DECIDED (PRIORITY #3: first-class `POST /api/player/:id/key`)** (re-issue rotates the hash; owner is the player). Implementation moves from the Subsonic middleware (`server/subsonic/middlewares.go:191-222`) into #27's route, hashing per `persistence/player_repository.go:120,127,154-158,200-207` (`^nds_[0-9A-Za-z]{22}$`). Blocks #27, #13, #26.
5. **Per-key scopes/labels — DECIDED: keep one-hash-per-`player`, no scopes** (owner's minimal default; unique index `db/migrations/20260924010054_add_player_api_key_hash.sql:3,4` stands). A player row ≈ a device with one key; revocation = null the hash. If per-key scopes ever appear, they are a deliberate #S2 schema delta, reopened then.
6. **Existing-install password migration — MOOT (fresh service, no upgrade path)**: no legacy installs exist; passwords are argon2id from day one; the AES-GCM code and `PasswordEncryptionKey` config key are never ported. Blocks nothing.
7. **`subsonicSalt`/`subsonicToken` in the NATIVE login payload — DECIDED: drop entirely** — `server/auth.go:84-97 buildAuthPayload` (called `:68`, `:415`) is removed from the login payload; its only consumers are the dropped legacy JS player (`ui/src/utils/tokens.js:14-17`, `ui/src/subsonic/index.js:5-9`, `ui/src/subsonic/albumList.js:105-109`, `ui/src/multipleInheritance.js:26`). Blocks #13, #27.
8. **Public-token surface — DECIDED: keep** (share pages are the only HTML surface). SECURITY-CRITICAL: Go verifies with HMAC (`core/auth/auth.go:125-131`); the Rust port must verify signatures, never decode — `server/public/handle_images.go:76-93` calls `PublicTokenAuth.Decode` (signature-bypassing) while granting `IsAdmin:true` (`:42-44`), a real bug to fix not copy. Blocks #19, #28.
9. **Auth limiter for the trunk — DECIDED: apply on login + key-issuance paths, fail-closed** (unmatched route = rate-limited); do NOT copy the `newAuthLimiter` allows-all at `limit<=0` (`server/subsonic/auth_limiter.go:39-48,54-56`) — `limit<=0` means misconfigured, not unguarded. Blocks #27, #13.
10. **JWT audience discipline — DECIDED: mint and accept only `native`** — `jellyfin`/`subsonic` audiences are never minted or accepted; aud-less acceptance is closed from day one (no legacy tokens to preserve). Use a vetted Rust JWT crate (`jsonwebtoken`) with explicit aud/exp checks plus token_epoch. Blocks #13.
11. **Per-key revocation — DECIDED: `token_epoch` bump revokes all of a user's sessions; nulling `player.api_key_hash` revokes only that key** (`persistence/user_repository.go:129-146`, `core/auth/auth.go:147-149`, `persistence/player_repository.go:37-39,209+`), with a user-visible `POST /api/sessions/revoke`. Blocks #13.
12. **Config contract — DECIDED: clean TOML, no deprecation table** — fresh service, so no user TOML exists to pin: `Subsonic.*`, `Plugins.*`, `Jukebox.*` (`conf/configuration.go:188-196`, `:115`, `:119`) and the compat toggle are simply absent from the fresh TOML; unknown keys in a config file are a startup **error**, not a warning (`conf/configuration.go:676-690`). Blocks #S3, #03.
13. **Root path once the JS UI is dropped — DECIDED: `GET /` → 404** — `server/server.go:161-167`, `server/serve_index.go:122-136`, `ui/index.html:35,38` are not ported; the share-page HTML (#28, share template `serve_index.go:183 ShareInfo`) is the only HTML surface. Blocks #28, #24, #19.
14. **`/api/plugin*` with the host cancelled — DECIDED: route removed, 404** — `server/nativeapi/plugin.go:16-30`, DI `cmd/wire_gen.go:271`, `persistence/plugin_cleanup.go` are not ported. Blocks #27.
15. **Jellyfin drop — DECIDED: drop** (user decision). `server/jellyfin/**` is not ported (#30 CANCELLED); the `jellyfin` JWT audience is removed (mint/accept `native` only, item 10). Quick-Connect is NOT Jellyfin-specific: `core/quickconnect/**` is re-owned by #27 (`nd-api`) and kept, backing `/api/quickconnect` (`server/nativeapi/quickconnect.go:24-33`).
16. **DB migration strategy — DECIDED: regenerate** (no-upgrade-path decision). One fresh Rust migration is the basis, edited in place as the schema evolves; the 138-file Go migration history is never replayed. Blocks nothing.
17. **Zero-consumer crates — DECIDED**: `nd-backgrounds` (#34) and `nd-sonic` (#31) are out of the workspace; re-open individually if a Flutter need appears.
18. **Player scope — DECIDED 2026-11-18 (`D-PLAYER26` + `D-PLAYER-EDIT`, user-approved in-session)**: two decisions, both recorded with evidence in `subplans/README.md`.
    (a) **#26 splits.** Retain the player-session service (`Get`/`Register`/`Touch` over the landed `nd-db` `PlayerRepo`); **drop** the server-side mpv/jukebox playback device (`core/playback/**`, mpv IPC, `server/subsonic/jukebox.go`, `Jukebox.*`/`MPVPath`/`MPVCmdTemplate`) — Subsonic-only entry point, default-off, config keys already absent, JS UI dropped, unfinished subprocess reap upstream, no surviving non-test consumer.
    (b) **Per-device preference editing is retained product scope** — repository legs landed, no route mounted yet. `GET`/`PUT /api/player/{id}` stays canonical with ownership-restricted writes (`updateOwned` drops `user_id` — `persistence/sql_base_repository.go:506-514` — and `ownedRow` (`:97-102`) restricts the write; whitelist per `registerModelWhiteList`), and a **read-only** `GET /api/transcoding` profile list is required so a device can pick `transcoding_id` (caller-applied masking; Go status parity read404/write403). Transcoding profile **editing** stays dropped (item 1), `EnableTranscodingConfig` stays absent from the fresh TOML, `report_real_path` must not be exposed as an editable knob until its canonical meaning is defined, and no key/hash is ever serialized. Requires #05's executing `transcoding_repo` read and a BOOT-owned default seed from `nd-conf` `DEFAULT_TRANSCODINGS`. Blocks #27, #26, #05.
