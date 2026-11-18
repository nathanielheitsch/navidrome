# Rust port — audited capability snapshot

**Baseline:** `master` at `60cdd18402c0fd4b09b65e0b64ca494330501c02`, audited 2026-11-18.
This document records evidence, not implementation authorization. The current execution order lives in
[`next-work.md`](next-work.md); technical assignments live in `subplans/`. Older dated status blocks are history,
not current readiness. Resolve contradictions against the pinned source and current amendments, not a larger test total.

## Verdict

**The Rust workspace passes its present regression gates; it is not a runnable server or scanner.**
Shared codecs, record marshaling, all eight PostScan-equivalent hooks, and substantial executed reads have landed.
The next critical work is safe executed DB mutations and phase-specific queries, not another blanket hydration port.
`nd-scanner`, `nd-http`, and `nd-api` are still scaffolds; `nd-cli/src/main.rs:8` is empty.
A scaffold test asserting an issue number is not runtime coverage.

## Fresh parent-measured gates

Measured starting **2026-11-18T14:28:28Z**, from `rust/`, at the baseline above. Every listed command exited **0**.
The checkout was clean before and after; no formatter, auto-fix, oracle regeneration, commit, or push was run.

| Command | Actual result |
|---|---|
| `cargo fmt --all -- --check` | Pass |
| `cargo build --locked --workspace` | Pass |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | Pass |
| `cargo test --locked -p nd-db --all-targets --all-features --no-fail-fast` | **737 passed / 0 failed / 0 ignored**, 26 result groups |
| `cargo test --locked --workspace --all-targets --all-features --no-fail-fast` | **1816 passed / 0 failed / 1 ignored**, 69 result groups |
| `cargo doc --locked -p nd-db --all-features --no-deps` | Pass, **22 warnings: 8 unresolved links + 14 private links** |
| `git diff --check` (repository root) | Pass |

The ignored test is `dump_divergences`. These `--all-targets` totals are not a claim that doctests were run.
Logs: `/tmp/navidrome-plan-gates.5F9XLv/` (`head.txt`, `measured-at.txt`, `exits.txt`, per-command logs, status before/after).
Those logs are local audit artifacts, not clone-reproducible fixtures; the commands and measured boundary are recorded here.
Toolchain: rustc/cargo **1.99.1**, Go **1.27.4**, macOS arm64; ffmpeg/ffprobe **9.0.2** were available.
Some process tests return early when tools/codecs are unavailable; future network lifecycle gates must require their
prerequisites and record actual exercised paths. This run did not boot a server or scan a real library.

The earlier 733 DB / 1812 workspace boundary belongs to S-D3g2 before its follow-up. The transient report of
23 current rustdoc warnings is superseded by this actual 22-warning measurement, not by arithmetic over source tests.
Documentation is **not warning-free**; do not silently add rustdoc `-D warnings` as a gate without resolving that debt.

## Landed history and naming

| Immutable implementation label | Commit | Actual scope |
|---|---|---|
| S-D3a | `3ba10ee1` | Shared hydration seam, folder/user hooks |
| S-D3b | `6cd6c8d2` | Album/media-file hydration |
| S-D3c | `1c215a7e` | Album/media-file repository reads and artwork hydration core |
| S-D3d | `0d2dc89d` | Artist stats/similar-artists hydration and reads |
| S-D3e + S-D3f | `6a201f4a` | Track/scrobble delegation, playlist/radio reads and artwork wrappers |
| S-D3g1 | `f210431f` | Executed library/property/tag reads |
| S-D3g2 | `a8e4d57c` | Executed folder/artwork-queue reads and narrowed folder drains |
| S-D3g2 follow-up | `60cdd184` | Single-input FolderID hash; shared parenthesis/whitespace-aware sort splitting; Go-shaped string conversion; per-row folder hook/error ordering; regression tests/evidence corrections |

The old scout's S-D3b meant folder/playlist; its S-D3c meant album/media-file. Do not re-use those aliases to
schedule work. Lane names are not one-to-one commits. All rows above are already in this baseline; do not cherry-pick
or repeat them. S-D3b is committed, and playlist rules decoding is wired; old labels saying otherwise are stale.

## Capability axes (source-inspected, not a completeness percentage)

| Surface | Rendered / decoded | Production execution | Mounted / end-to-end |
|---|---|---|---|
| Shared marshal, JSON/role/FTS feeders | Present; eight row hooks exist | Consumers below; marshal tests also manually execute SQL bundles | Not a full entity-write or scanner contract |
| User/player | Read and mutation builders/records | Substantial reads and writes execute | Auth consumers exist; no server bootstrap |
| Share/user properties | Present | Raw-connection reads/writes execute, outside shared execution helper | Full share service is not implemented |
| Album/media-file/artist/folder/playlist/radio | Record/hook/read builders present | Substantial reads; **no production executed entity Put lifecycle** | No ingest or mounted API |
| Library/property/tag | Plain mappers and builders present | Reads execute, including `PropertyRepo::default_get`; scanner writes do not | No library bootstrap/scan lifecycle |
| Playlist tracks/scrobble buffer | Delegating hooks and builders present | Partial reads; buffer dequeue executes | Not full playlist/scrobble services |
| Artwork queue | Builders and plain mapper present | Reads/counts and `purge_queued` execute | Enqueue/worker lifecycle is not implemented |
| Artwork/genre/playqueue/plugin/scrobble/smart-playlist/transcoding modules | Standalone repository operations are builder-only | Artwork item-image lookup separately executes in `artwork_hydration.rs` | Not full repository/service completion |
| DB store/pool | Sync/async lease and transaction/retry APIs exist | SQLite work runs on blocking boundary | Settings on every connection and typed error composition need explicit gates |
| Schema | Fresh DDL, migrations/marker, extensions, indexes, backup/optimize | Executed schema tests | No default library row; repair deferred; bootstrap caller absent |
| Tagreader/mapping | Real lofty backend and metadata mapping | Crate-level implementation/tests | No storage-to-record-to-DB ingest caller |
| Auth | Credential/JWT/login/refresh/middleware components | Crate-level mounted test routers | Login audience/API-key extraction integration gaps remain; no production router |
| Stream #15 | Handler/cache/limiter/archiver implemented | Crate-level serving/lifecycle tests | **Component-level complete, not HTTP-ready**; header commitment/Send adapter gaps remain |
| Scanner/HTTP/API/CLI | Scaffolds/empty main | No production scan/listener/router | **Not started at integration boundary** |
| Scheduler | Scheduling primitives implemented | Crate tests | Full maintenance/logging and bootstrap remain |

Repository sources: `rust/crates/nd-db/src/*_repo.rs`, `hydrate.rs`, `marshal.rs`, `store.rs`, `pool.rs`.
Executed test helpers are not production methods. Sixteen conn-bound repository impls or eight decoded hooks do not
prove all methods execute. Seven `*_repo.rs` modules remain builder-only (including smart-playlist helper, not a separate Go repository).

## Verified blockers and integration obligations

### DB safety before enabling scanner mutations

1. **Wrong missing-update bind order** — `mediafile_repo.rs:1587-1605` renders SET placeholders before IDs, but
   binds IDs first. Its unit test at `:2377-2398` pins the wrong order. Parent inspected source and reproduced the
   emitted SQL in in-memory SQLite: intended `mf-1` unchanged, decoy ID `99` overwritten. That probe did **not** call
   the Rust function; the required regression is production-Rust-builder -> fresh-schema execution -> read-back.
   The absent scanner is not currently corrupting a user's DB; do not activate this renderer unchanged.
2. **Mixed timestamp and idle-sentinel contracts** — library scan begin/end/count (`library_repo.rs:622-709`),
   album touch, folder/media missing updates, and recent-match bounds still use integers alongside TEXT record stamps.
   `ScanEnd` writes integer `0`; `row_to_library` (`hydrate.rs:3608-3616`) decodes it as 1970, not the zero time used
   by scan preparation (`scanner/scanner.go:209-215`). Fresh DDL zero-date TEXT also disagrees with `<> INTEGER 0`.
   Writers, bounds, defaults, idle predicate and decoded model must be tested together. This is fresh-Rust correctness,
   **not a demand for Go database migration support**.
3. **Pool configuration needs measurement, not guessed failures** — `schema::open` registers extensions but has no
   explicit Go-DSN translation/settings layer; pool opens new handles. Pinned bundled SQLite enables foreign keys
   (`libsqlite3-sys 0.30.1/build.rs:123`); rusqlite sets a 5000ms busy timeout (`inner_connection.rs:119`). Therefore
   FK-off/no-busy-handler claims are unproven. Intentionally select and assert effective path, FK, journal, timeout,
   synchronous settings and extensions on **every simultaneously held pool connection**. Go defaults (WAL/15000ms/
   synchronous-normal) are source reference, not permission to silently change Rust configuration.
4. **Executed scanner writes and queries are missing** — conn-bound Put/PutByMatch plus base/FTS/participant/tag/link
   atomicity; leaf property/library/tag/folder writes; typed folder/album-filtered media reads; missing/recent/touched
   queries; queue enqueue/artwork invalidation; move/reference/annotation transactions; scoped eleven-operation GC.
   Current `MediaFileRepo::get_all` does not forward arbitrary scanner filters. A raw user-supplied SQL fragment is
   not an acceptable substitute. Compose repository errors with transaction/retry errors without string loss.

### Scanner and network boundaries

- The historical **eight repositories + one mapper** list is a core-read inventory, not the full scanner gate:
  phase 1 invalidates artwork; phase 4 needs playlist service/CRUD; GC cleans playlist orphans/annotations.
- `nd_mapping::map_file` (`nd-mapping/src/lib.rs:78-86`) forces library 1 and empty folder ID. Scanner must provide
  actual library/folder and storage facts through the correct mapping seam, not persist these convenience defaults.
  `nd-model::album::equals` (`album.rs:174-176`) compares only IDs; it is not a substitute for Go aggregation/change
  detection. The scanner plan must own a tested aggregate/equality bridge or obtain the domain-owner change decision.
- **IPC correction:** Go external scanner gob carries `ProgressInfo` (`scanner/external.go:86-96`, `cmd/scan.go:71-76`),
  not tag-reader results. Watcher/progress-process protocol are later parity packages, not automatic gates for an
  explicitly limited in-process fixture scan. A complete #17 claim still requires all advertised phases/lifecycle.
- **Auth integration:** login calls audience-less `create_token` (`login.rs:143`, `credentials.rs:126-135`), while
  `AuthGate::Api` requires native audience (`middleware.rs:126-130`, `credentials.rs:288-300`). Existing tests assert
  audience-less rejection; no mounted login->canonical API round-trip closes this. Preserve expiring sessions,
  refresh/epoch checks and strict native audience; do not replace sessions with non-expiring API tokens or weaken gates.
  Hashed-key primitives do not supply canonical HTTP key extraction/issuance middleware by themselves.
- **Stream integration:** `StreamHandler::serve` (`handler.rs:1302-1507`) returns status/headers after pumping and
  teardown; `prepare` is private and may change the body kind. A channel alone cannot commit correct HTTP headers.
  Its boxed-future/trait-object Send contract also needs a deliberate network adapter seam. Preserve bounded
  backpressure, cancellation, lease/slot ownership, cache semantics, and post-header error behavior.
- Fresh DDL has no library row. Configured-library creation belongs to an explicit bootstrap transaction/caller,
  using typed executed DB methods—not an invented schema-golden seed.

## Residuals: separate parity debt from first-boot gates

Known genre JOIN/filtering, smart-playlist/playlist-track OR grouping and alias hazards must close **before their
consumers are exposed**; they are not accepted behavior. Participant artist re-fetch, true lazy cursors, and per-row
hook/error ordering in sibling bulk readers need named consumer gates. Sibling error precedence is source-inferred,
not a freshly executed counterexample. Folder's full-drain adapter is not a claim of universal lazy-cursor parity.
Accepted deviations (e.g. offset-only `LIMIT -1`, some JSON diagnostics/ordering/escaping, Rust error shapes) must
stay distinct from deferred bugs and unverified behavior. Do not silently fix unrelated parity details under a scanner task.

S-D3g1/g2 Go harnesses and traces exist locally but are **not clone-reproducible tracked fixtures**. Capture minimal
real-Go harness inputs/results/regeneration recipes, pinned versions and canonicalization in an authorized evidence
lane; adding/changing Go harness sources requires explicit permission. Never derive Go expectations from Rust or
commit scratch binaries/DBs. Previously tracked participant/feeder/plain-JSON harnesses have narrower scopes and
cannot certify transaction/query behavior they do not execute.

Repair, full maintenance/logging, complete API/services, HTML shares and metrics remain separate scopes. They are
not automatic first-track/first-boot prerequisites. Cancelled Subsonic/Jellyfin/plugin/sonic/UI work stays cancelled;
first playback uses the canonical API, not resurrected `/rest` routes. Full service tails require their own contracts.

Player scope (decided 2026-11-18, `D-PLAYER26`, user-approved): #26 is split. The player-session service is retained —
`Get`/`Register`/`Touch` over the landed `nd-db` `PlayerRepo`, with canonical placement chosen by API-01 — because its
`MaxBitRate`/`TranscodingId` feed the already-ported stream decision (`nd-stream/src/legacy_client.rs`, currently dead
because nothing persists the row) and `ScrobbleEnabled` gates the scrobbler. The mpv/jukebox server-side playback-device
scope (`core/playback/**`, mpv IPC, `server/subsonic/jukebox.go`, `Jukebox.*`/`MPVPath`/`MPVCmdTemplate`) is **DROPPED**
unported: Subsonic-only entry point, default-off, config keys already absent, JS UI dropped, unfinished subprocess reap
upstream, and no other consumer. Neither half gates #27A; #26 is not DONE.

## Audit provenance

Three independent read-only investigations in the existing workspace, with no test runs or repository writes:

| Assignment | Agent ID | Local report |
|---|---|---|
| DB runtime/scanner prerequisites | `eeb2bc93-8084-4e6e-b399-2ad989b16f80` | `/tmp/navidrome-plan-db-audit.md` |
| Boot/scanner/API reconnaissance | `fda0b28e-7f20-422b-b437-c9a35dea4988` | `/tmp/navidrome-plan-runtime-scout.md` |
| Plan/evidence consistency | `a6b5241d-54ea-47a2-a151-9dd25b8232a1` | `/tmp/navidrome-plan-evidence-audit.md` |

Host `srv_QsD9e1TgIsNq`; workspace `wks_7bb3344766422a39`; parent `4920ad4e-8f0f-4982-92e6-8fceee49afdd`.
Parent independently checked consequential source claims, corrected the Property DefaultGet/SQLite-default assumptions,
reproduced the missing-update emitted-SQL hazard, and ran the gates above. Reports are local supporting evidence;
this snapshot and future tracked subplans carry the durable conclusions. No future package is complete merely
because it is planned, and no implementation has been authorized by this audit.
