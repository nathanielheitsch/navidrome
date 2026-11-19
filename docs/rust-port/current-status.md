# Rust port — audited capability snapshot

**Authoritative baseline: integrated `master` at `74ebd81c`, dated 2026-11-19** (`ff900d3e` + `ab466928` DB-15
render-only rating repair + `e9dd8230`/`ff5b9c92` scanner local-storage subset + `48e65806`/`ec84c176` DB-00 typed
transaction seam + `6d9d984d`/`74ebd81c` DB-02 scanner timestamp contract), sitting on the earlier audited boundaries `4a8aac9f` (DB-03), `f57ae050` (DB-01), `62789ba2` (sort
gate), `f8ceeb8f` (W4+W6), `60cdd184` (S-D3g2 follow-up). This docs lane's own worktree is cut from `ff900d3e` and does
**not** contain that code, so claims about integrated behavior below are SOURCE-VERIFIED against `master` or dated
parent measurements — never measured in this worktree. This document records **evidence**, not implementation
authorization. Execution order lives in [`next-work.md`](next-work.md); binding decisions in
[`decisions.md`](decisions.md); bounded assignments in `subplans/`. Older dated blocks are history, not current
readiness. Resolve contradictions against the pinned source and the current contract amendments, never against a larger
test total. Entry point: [`README.md`](README.md).

## Verdict

**The Rust workspace passes its present regression gates; it is not a runnable server or scanner.** Shared codecs,
record marshaling, all eight `PostScan`-equivalent hooks, and substantial executed reads have landed. **Scanner
orchestration, HTTP, API and CLI remain scaffolds**: `nd-http` and `nd-api` are scaffolds, `nd-cli/src/main.rs` is empty,
and `nd-scanner` currently holds only tested local-storage primitives (`storage.rs`). There is **no end-to-end/server
readiness**, no mounted router, no production scan, and no integrated stream HTTP adapter. A scaffold test asserting an issue number is
not runtime coverage, and executed test helpers are not production repository methods.

## Latest verified gates (parent-measured at integrated `master` `74ebd81c`, 2026-11-19)

Measured by the parent on a clean integrated tree from `rust/` with its default `rust/target`; the test runs used
`--locked --all-targets --all-features` as recorded per row below, and every command exited **0**. These are **dated
parent measurements**, not freshly executed by this docs lane,
and not proof for a later diff. This lane re-tallied the parent logs itself and reproduces the totals exactly
(`/tmp/wave1-db02-integrated-tests.log`: 78 `test result:` lines, 2009 passed, 0 failed, 1 ignored;
`/tmp/wave1-final-db.log`: 33 lines, 841 passed, 0 failed; `/tmp/wave1-db02-integrated-doctests.log`: 5 passed).

| Command | Actual result | Source log |
|---|---|---|
| `cargo fmt --all -- --check` | Pass (diff 0) | parent |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | Pass | parent |
| `cargo test --locked -p nd-db --all-targets --all-features` | **841 passed / 0 failed / 0 ignored** across **33** suites | `/tmp/wave1-final-db.log` |
| `cargo test --locked --workspace --all-targets --all-features` | **2009 passed / 0 failed / 1 ignored** across **78** suites | `/tmp/wave1-db02-integrated-tests.log` |
| `cargo test --locked -p nd-db --doc` | **5 passed / 0 failed / 0 ignored** — a **separate axis**, never added to the 841 | `/tmp/wave1-db02-integrated-doctests.log` |
| `cargo doc --locked -p nd-db --all-features --no-deps` | Pass, **22 warnings** (log's own summary line says 22; `grep -c '^warning:'` returns 23 because it counts that summary line) | `/tmp/wave1-db02-integrated-doc.log` |
| `cargo test --locked -p nd-dbschema` | **61 passed / 0 failed / 0 ignored** — **carried** baseline, not freshly re-measured at `74ebd81c` by this lane | prior boundary |
| `cargo test --locked -p nd-auth --all-targets --all-features` | **90 passed / 0 failed / 0 ignored** — **carried** baseline, same caveat | prior boundary |
| `git diff --check` | Pass | parent |

**Axis discipline:** `--all-targets` totals **exclude doctests**; the `5` doctests are their own run. Do not reconcile
totals across axes or against an earlier head.

Earlier dated boundaries on this line of development (historical evidence, not current): `ec84c176` 834 DB / 2002
workspace across 77 suites; `4a8aac9f`/`ff900d3e` 813 DB / 1938 workspace across 74 suites; `ab466928` 819 DB across 31
suites; `f57ae050` 778 DB / 1857 workspace; `62789ba2` 771 DB /
1850 workspace; `f8ceeb8f` 765 DB; `60cdd184` 737 DB / 1816 workspace (69 groups). Toolchain at the `60cdd184` audit:
rustc/cargo **1.99.1**, Go **1.27.4**, macOS arm64, ffmpeg/ffprobe **9.0.2** available. Some process tests return early
when tools/codecs are unavailable; future lifecycle gates must require their prerequisites and record what was actually
exercised. No run above booted a server or scanned a real library.

**Rustdoc debt:** the `nd-db` doc build succeeds with **22 warnings** (8 unresolved links + 14 private-item links),
byte-identical in set to the earlier baseline. Do not add rustdoc `-D warnings` as a gate without resolving that debt,
and do not call a successful doc build warning-free. Measurement pitfall recorded so it is not repeated:
`grep -c '^warning:'` also counts rustdoc's own summary line, which produced a transient 23-warning report.

**Known flake (non-DB, tracked):** `nd-stream` lib test
`the_poller_exits_on_teardown_even_when_the_client_never_hangs_up` took ~30s against its 3s deadline in one
full-workspace run (1837 passed / 1 failed); later runs and four isolated binary runs were clean — flaky under load, not
a regression. `nd-stream`'s `transcode_body::tests::early_release_defeats_the_cap_and_accumulates_processes` (counts
live `sleep` stub PIDs via `ps`) is the other timing-sensitive one. The ignored test is `dump_divergences`
(`nd-mapping/tests/go_oracle.rs`). `--all-targets` totals are not a claim that doctests were run.

## Landed history (labels are immutable; do not re-use them to schedule)

| Label | Commit | Actual scope |
|---|---|---|
| S-D3a | `3ba10ee1` | Shared hydration seam, folder/user hooks |
| S-D3b | `6cd6c8d2` | Album/media-file hydration |
| S-D3c | `1c215a7e` | Album/media-file repository reads, artwork hydration core |
| S-D3d | `0d2dc89d` | Artist stats/similar-artists hydration and reads |
| S-D3e + S-D3f | `6a201f4a` | Track/scrobble delegation, playlist/radio reads and artwork wrappers |
| S-D3g1 | `f210431f` | Executed library/property/tag reads |
| S-D3g2 | `a8e4d57c` | Executed folder/artwork-queue reads, narrowed folder drains |
| S-D3g2 follow-up | `60cdd184` | Single-input FolderID hash, parenthesis/whitespace-aware sort splitting, Go-shaped string conversion, per-row folder hook/error ordering, tests/evidence corrections |
| W4+W6 | `f8ceeb8f` | Executed reads incl. **transcoding `read_all`/`count_all` with `command` masking** |
| W7 sort gate | `62789ba2` | `RestQueryOptions` parity on executed REST reads |
| DB-01 | `f57ae050` | Missing-state bind order + executed regression gate |
| DB-03 / D-OPEN | `4a8aac9f` | Go-DSN parity with pool-bound initializer tokens, per-connection effective-state read-back |
| DB-15 **render-only** rating repair | `ab466928` | `update_avg_rating_sql` embeds the average-rating aggregate with 3 ordered binds, executed against a real schema by `runtime_annotation_rating.rs`; the three public renderers were kept and redocumented with their prepare failure pinned — **not** the full DB-15 annotation reassignment service, whose transactions and read APIs are unimplemented |
| Scanner local-FS storage subset | `e9dd8230` + merge `ff5b9c92` | `storage.rs` only: std-`fs` `list`/`stat`/`read`/`read_link`/`resolve`/`exists`, logical paths, `file://` URI parsing, explicit symlink classification. Walker, `.ndignore`/ignore policy, folder hashing and cancellation are **not implemented** — **not** SC-01 ratification or SC-02 completion |
| DB-00 typed transaction/error seam | `48e65806` + merge `ec84c176` | Additive `try_with_tx`/`try_with_tx_retry`, `DbOperationError`/`DbRuntimeError`, safe rollback cleanup, BUSY-only retry (H2 amended) — the seam only, not the DB lane |
| DB-02 scanner timestamp contract | `6d9d984d` + merge `74ebd81c` | Scanner-facing stamps/cutoffs bound as canonical TEXT via `time::value`, `library_repo::zero_time_value()` replacing raw `ZERO_TIME` integers, artwork-purge cutoff fixed to a `DateTime` bound, `runtime_scan_clock.rs` executed coverage. **Not** full scanner writes: `scrobble_buffer` `play_time`/`enqueue_time` still bind integers (see Residuals) |

The old scout's S-D3b meant folder/playlist and its S-D3c meant album/media-file; `hydrate-scan-contract.md` numbers
the same lanes differently (see the numbering-drift note in `next-work.md`'s historical index). All rows above are in
this baseline — do not cherry-pick or repeat them. Playlist rules decoding is wired; old labels saying otherwise are
stale.

## Capability axes (source-inspected, not a completeness percentage)

| Surface | Rendered / decoded | Production execution | Mounted / end-to-end |
|---|---|---|---|
| Shared marshal, JSON/role/FTS feeders | Present; eight row hooks exist | Consumers below; marshal tests also manually execute SQL bundles | Not a full entity-write or scanner contract |
| User/player | Read and mutation builders/records | Substantial reads and writes execute | Auth consumers exist; no server bootstrap |
| Share/user properties | Present | Raw-connection reads/writes execute, outside the shared execution helper | Full share service not implemented |
| Album/media-file/artist/folder/playlist/radio | Record/hook/read builders present | Substantial reads; **no production executed entity `Put` lifecycle** | No ingest or mounted API |
| Library/property/tag | Plain mappers and builders present | Reads execute, including `PropertyRepo::default_get`; scanner writes do not | No library bootstrap/scan lifecycle |
| Playlist tracks/scrobble buffer | Delegating hooks and builders present | Partial reads; buffer dequeue executes | Not full playlist/scrobbler services |
| Artwork queue | Builders and plain mapper present | Reads/counts and `purge_queued` execute | Enqueue/worker lifecycle not implemented |
| Transcoding module | Builders present | **Executed reads landed at `f8ceeb8f`** (`read_all`/`count_all`, `command` masked for a non-admin) | No route, no default-profile seed; profile **editing** out of scope |
| Artwork/genre/playqueue/plugin/scrobble/smart_playlist | Standalone repository operations are builder-only | Artwork item-image lookup separately executes in `artwork_hydration.rs` | Not full repository/service completion; `plugin` scope decision pending (`next-work.md` §"Decision ask (scope)") |
| DB store/pool | Sync/async lease and transaction/retry APIs exist | SQLite work runs on the blocking boundary | Effective per-connection configuration **landed** (DB-03); typed seam **landed** (`48e65806`/`ec84c176`, DB-00) — compound writes (DB-04+) still consume it |
| Schema | Fresh DDL, migrations/marker, extensions, indexes, backup/optimize | Executed schema tests | No default library row; repair deferred; no bootstrap caller |
| Tagreader/mapping | Real lofty backend and metadata mapping | Crate-level implementation/tests | No storage→record→DB ingest caller |
| Auth | Credential/JWT/login/refresh/middleware components | Crate-level mounted test routers | Login-audience and HTTP API-key extraction integration gaps; no production router |
| Stream #15 | Handler/cache/limiter/archiver implemented | Crate-level serving/lifecycle tests | **Component-level complete, not HTTP-ready**; header commitment/Send adapter gaps |
| Scanner/HTTP/API/CLI | Scaffolds / empty main | No production scan/listener/router | **Not started at the integration boundary**; a local-FS storage subset landed at `e9dd8230`/`ff5b9c92` (**not** SC-01/SC-02 completion) |
| Scheduler | Scheduling primitives implemented | Crate tests | Full maintenance/logging and bootstrap remain |

Repository sources: `rust/crates/nd-db/src/*_repo.rs`, `hydrate.rs`, `marshal.rs`, `store.rs`, `pool.rs`. Executed reads
do not prove all methods execute. Re-counted at integrated `master` `74ebd81c` by this lane (SOURCE-VERIFIED: grep for
`execution::query`/`read_hydrated`/`prepare_and_then` across the 23 `*_repo.rs` modules): **15 modules execute through the
shared helper** (property, playlist_track, radio, scrobble_buffer, transcoding, album, artist, tag, folder,
artwork_queue, mediafile, playlist, library, player, user), **2 execute on raw connections** outside it (`share`,
`user_props`), and **six are render-only** (`artwork`, `genre`, `playqueue`, `plugin`, `scrobble`, `smart_playlist` —
`smart_playlist` is not a separate Go repository; `plugin` scope decision pending). `annotation_repo.rs` does not exist
because annotations ride album/media-file (DB-15), so that is not a gap.

## Landed safety gates (former open blockers, now closed with executed regressions)

- **Missing-update bind order — CLOSED at `f57ae050`** (DB-01): `mark_missing_sql` args follow Go's `.Set`/`.Where`
  order; `runtime_missing_state.rs` executes 7 real-schema tests plus a pre-fix control. Details: `decisions.md` W0.
  **Other** latent write defects remain — do not connect existing renderers unchanged.
- **Timestamp/idle semantics — CLOSED at `6d9d984d`/`74ebd81c`** (DB-02/D-TIME, `decisions.md` T1-T5): scanner-facing
  stamps and cutoffs bind canonical TEXT through `time::value`; the idle/scan-end comparison uses the deliberate
  `library_repo::zero_time_value()` API instead of raw `ZERO_TIME` integers; year-one `time::zero()` and the fresh-DDL
  `0000-00-00 00:00:00` sentinel behavior are preserved as inherited Go semantics (pinned, not "normalized"), and the
  artwork-purge integer-cutoff regression is fixed with executed coverage in `runtime_scan_clock.rs`. Not a demand for
  Go database migration support. **Known residual:** `scrobble_buffer.play_time`/`enqueue_time` still bind integers.
- **Pool configuration — CLOSED at `4a8aac9f`** (DB-03/D-OPEN): explicit typed options, per-connection PRAGMA,
  effective-state read-back before pooling, Go-compatible DSN parser (`decisions.md` P1). FK-on and the 5000 ms rusqlite
  default are pinned facts, not assumptions.
- **Typed repository/transaction errors — CLOSED at `48e65806`/`ec84c176`** (DB-00/D-ERROR, `decisions.md` E1-E3,
  retryable set H2): additive seam beside `with_tx*`, `RollbackFailed` keeps original + rollback errors reachable,
  `BusyExhausted` replaces strings, `Depleted`/`Poisoned` checkout states are distinct. Consumers (DB-04 onward) still
  adopt it lane by lane.

All four first-wave implementation lanes (DB-01, DB-03, DB-00, DB-02, plus the DB-15 render-only repair and the scanner
local-FS storage subset) are merged into `74ebd81c`; **no implementation branch is currently in flight.** The next lane is
assigned work, not a pending merge.

## Remaining blockers and integration obligations (SOURCE-VERIFIED)

1. **Executed scanner writes and queries are missing** — conn-bound `Put`/`PutByMatch` plus base/FTS/participant/tag/link
   atomicity; leaf property/library/tag/folder writes; typed folder/album-filtered media reads;
   missing/recent/touched queries; queue enqueue and artwork invalidation; move/reference/annotation transactions;
   scoped eleven-operation GC. `MediaFileRepo::get_all` does not forward arbitrary scanner filters, and a raw
   user-supplied SQL fragment is not an acceptable substitute.
2. **Scanner mapping/equality seams** — `nd_mapping::map_file` (`nd-mapping/src/lib.rs:78-86`) forces library 1 and an
   empty folder id; `nd_model::album::equals` (`album.rs:174-176`) compares only ids. The scanner plan must own a tested
   aggregate/equality bridge or obtain the domain-owner decision (`decisions.md` S4). IPC correction: Go's external
   scanner gob carries `ProgressInfo` (`scanner/external.go:86-96`, `cmd/scan.go:71-76`), not tag-reader results;
   watcher/progress-protocol are later parity packages, not gates for a limited in-process fixture scan.
3. **Auth integration** — login calls the audience-less `create_token` (`login.rs:143`, `credentials.rs:126-135`) while
   `AuthGate::Api` requires the native audience (`middleware.rs:126-130`, `credentials.rs:288-300`); existing tests
   assert audience-less rejection and no mounted login→canonical-API round-trip closes this. Preserve expiring sessions,
   refresh/epoch checks and strict native audience; do not replace sessions with non-expiring API tokens or weaken gates.
   Hashed-key primitives do not supply canonical HTTP key extraction/issuance middleware.
4. **Stream integration** — `StreamHandler::serve` (`handler.rs:1302-1507`) returns status/headers after pumping and
   teardown, `prepare` is private and may change the body kind, and the boxed-future/trait-object `Send` contract needs a
   deliberate network adapter seam. Preserve bounded backpressure, cancellation, lease/slot ownership, cache semantics,
   and post-header versus pre-header error behavior. Network tests must exercise real body backpressure/drop, child
   cleanup and recovered slots with real tools/codecs; an early-returning test is not lifecycle evidence.
5. **No library row** — fresh DDL has none; configured-library creation belongs to an explicit bootstrap
   transaction/caller using typed executed DB methods, not an invented schema-golden seed.
6. **Scope decisions still open** — `plugin_repo.rs` (`next-work.md` §"Decision ask (scope)", `decisions.md` open
   list): the `plugin` table is absent from the fresh schema, so every statement that module renders fails, and the
   scope decision is the parent's call. Part C per-key config scopes; oracle-harness authorization for new tracked
   Go harnesses.

## Bounded source-level residuals (measured at `74ebd81c`; this list is a boundary, not a claim that everything else is done)

- `scrobble_buffer_repo` still binds `play_time`/`enqueue_time` as **integers** (`Value::Integer`) on an INTEGER-affinity
  `datetime` column, while the DB-02 contract binds TEXT elsewhere (`scrobble_buffer_repo.rs:211-225`). Distinct and
  deliberate: `scrobbles.submission_time` is intentionally an integer epoch and is not part of this convention.
- `find_recent_files_by_properties_sql` (`mediafile_repo.rs:1792`) is converted to the TEXT cutoff shape but has **no
  executed regression pin**; the sibling MBZ path is pinned (`runtime_scan_clock.rs:222`), so the properties path is the
  uncovered one.
- DB-04+ scanner mutations (property/library/tag/folder writes, entity `Put`/`PutByMatch`, FTS/junction atomicity),
  DB-13 epoch revoke, DB-14 folder-delta transaction, and full DB-15 annotation execution are **not implemented**.
- The scanner local-FS subset and the DB-15 render-only repair remain **partial scopes** (not SC-01/SC-02, not the
  annotation service); storage `Put` lifecycle, aggregation/equality bridge, controller/watcher and progress protocol are
  unstarted. There is **no server readiness**: no router, listener, library bootstrap, or integrated stream HTTP adapter.

## Residuals (parity debt vs first-boot gates, kept distinct)

Genre JOIN/filtering, smart-playlist/playlist-track OR grouping and alias hazards must close **before their consumers
are exposed**; they are not accepted behavior. Participant artist re-fetch, true lazy cursors, and per-row hook/error
ordering in sibling bulk readers need named consumer gates; sibling error precedence is source-inferred, not a freshly
executed counterexample, and folder's full-drain adapter is not a claim of universal lazy-cursor parity. Accepted
deviations (`decisions.md` A1-A7: offset-only `LIMIT -1`, JSON escaping/diagnostics ordering, Rust error shapes,
`i32` narrowing, artwork error exposure, atomic first-admin) stay distinct from deferred bugs and unverified behavior.
Do not silently fix unrelated parity details under a scanner task.

Go harnesses and traces for the hydration lanes exist locally but are **not clone-reproducible tracked fixtures**; the
previously tracked participant/feeder/plain-JSON harnesses have narrower scopes and cannot certify transaction/query
behavior they do not execute. Durable captures, pinned versions, canonicalization and regeneration recipes belong to
the EVID lane (`subplans/evidence-ops.md`), and adding or changing Go harness sources requires explicit permission.
Never derive Go expectations from Rust or commit scratch binaries/DBs.

Repair, full maintenance/logging, complete API/services, HTML shares and metrics remain separate scopes and are not
automatic first-track/first-boot prerequisites. Cancelled Subsonic/Jellyfin/plugin/sonic/UI work stays cancelled; first
playback uses the canonical API, not resurrected `/rest` routes. Player scope is settled — the player-session service
and owner-scoped editing are retained, mpv/jukebox is dropped (`D-PLAYER26`, `D-PLAYER-EDIT`; full evidence in
`subplans/README.md`), the transcoding read now executes, and only the BOOT-owned `DEFAULT_TRANSCODINGS` seed plus the
canonical routes remain. Neither half gates #27A, and #26 is not DONE: no player route is mounted (`nd-api` is a
scaffold).

## Provenance

Conclusions here come from parent-measured gate runs and source inspection at the pinned commits, plus read-only
investigation briefs whose local `/tmp` reports are supporting evidence only (agent IDs and provider failure histories
are deliberately not canonicalized in these docs). The parent independently verified consequential source claims and
reproduced the hazards recorded in the contracts. Nothing is complete merely because it is planned, and no
implementation is authorized by this snapshot.
