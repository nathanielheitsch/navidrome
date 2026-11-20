# Rust port — audited capability snapshot

**Authoritative baseline: integrated `master` at `e3e0770a` (SC-02 partial: local walker, `.ndignore`, folder hash,
cancellation), dated 2026-11-20.** This extends the previously audited `272c9c66` (DB-22) boundary — and the
`f80b9704` boundary before it (DB-04…DB-10/12/13 and BOOT-02; DB-12 signed `e5d0592d`, rustdoc fix `f80b9704`; DB-10 signed
`65072b89` / merge `6e5ca97d`) — with **SC-02 partial landed at `e3e0770a`** (fast-forwarded onto `2e7026b2`, the S9b
docs HEAD), parent-validated (92 suites, 2276/0/1). Decision records: S9 (SC-02 dependency set + close-out, `b1aeea2c`),
D-CURSOR/D-SCOPE and D-PLUGIN (all in `decisions.md`). Both executed DB subsets and the SC-02 slice are LANDED; the
**SC-02 partial** is a scoped slice, **not** SC-01 ratification, SC-02 completion, or MusicFS readiness.
Latest gates below are **parent integration evidence**, with log totals independently checked by this docs lane; this
lane did not rerun Cargo at the new HEAD. Its earlier direct measurements at `4ab63cd5` remain historical evidence.
This document records **evidence**, not implementation authorization. Execution order lives in [`next-work.md`](next-work.md); binding decisions in
[`decisions.md`](decisions.md); bounded assignments in `subplans/`. Older dated blocks are history, not current
readiness. Resolve contradictions against the pinned source and the current contract amendments, never against a larger
test total. Entry point: [`README.md`](README.md).

## Verdict

**The Rust workspace passes its present regression gates; it is not a runnable server or scanner.** Shared codecs,
record marshaling, all eight `PostScan`-equivalent hooks, substantial executed reads, and — since `4ab63cd5` —
**executed transaction-bound leaf and entity writes** (property, library incl. scan lifecycle, tag, folder,
media-file identity/base/junction writes, artist, album, user epoch revoke, artwork invalidation/enqueue) have landed. **Scanner orchestration, HTTP, API and CLI
remain scaffolds**: `nd-http` and `nd-api` are scaffolds (`nd-cli/src/main.rs` is an empty `main`); `nd-scanner` now
holds the tested local-FS walker slice (`storage.rs` + `.ndignore`/folder-hash/cancellation, SC-02 partial at `e3e0770a`)
but **no scanner entrypoint, tag-to-DB pipeline, or full-scan orchestration**. There is **no end-to-end/server readiness**, no
mounted router, no production scan, and no integrated stream HTTP adapter. None of the executed transaction-bound write
primitives has a **production caller** at `f80b9704` (SOURCE-VERIFIED: `_in_tx` references outside repository definitions
are tests/doctests or internal helpers, not scanner/bootstrap/route/CLI entrypoints). DB-12 is only the executed artwork
invalidation/enqueue subset, not a worker or full scanner. That is narrower than "no repository method is ever called":
`nd-auth` **does** call
`UserRepo` methods from production code (`login.rs:95,108,227,247,308,332,337,355,356,374`, `setup_store.rs:66-86`,
`middleware.rs:93` — `find_by_username`, `count_all`, `count_by_username`, `put`, `update_last_login_at` through
`SQLStore::with_connection`/`with_tx_retry`), but that router is mounted only in crate tests, not in a server. A scaffold
test asserting an issue number is not runtime coverage, and executed test helpers are not production repository methods.

## Latest integrated gates (parent, code HEAD `e3e0770a`, 2026-11-20)

SC-02 partial (local walker + `.ndignore` + folder hash + cancellation, S7/S8/S9/S9b) fast-forwarded onto
`2e7026b2` at `e3e0770a`. Parent re-ran the authoritative gates on the integrated main checkout:
workspace tests **92 suites, 2276 passed / 0 failed / 1 ignored**; `cargo fmt --all -- --check` clean; workspace clippy
(`--all-targets --all-features -- -D warnings`) clean; `git diff --check` clean; `nd-scanner` rustdoc = 4
pre-existing warnings (none new from this lane). This is a partial SC-02, NOT SC-01/SC-02 completion or MusicFS
readiness, and NOT the runnable-milestone scanner entrypoint. Prior dated block below.

## Prior integrated gates (code HEAD `272c9c66` / DB-22, 2026-11-20)

Parent validated on the DB-22 branch after rebase onto `276eec6c` (fast-forward, no merge commit): fmt check,
workspace clippy, and workspace tests all succeed: **88 suites, 2171 passed / 0 failed / 1 ignored**. `nd-db` docs
report **23 rustdoc warnings**, all pre-existing (one new in `folder_repo.rs` came from already-integrated DB-07
work, not DB-22); no warning in a DB-22-touched file. Dated code-HEAD results, not proof for later diffs.

## Integrated gates history (code HEAD `f80b9704`, 2026-11-19)

Parent reports fmt check, workspace clippy, workspace tests and workspace docs all succeed at `f80b9704`. This lane
independently tallied `/tmp/nav-db10-integrated-tests.log`: **87 suites, 2159 passed / 0 failed / 1 ignored**.
Clippy log: `/tmp/nav-db10-integrated-clippy.log`; docs: `/tmp/nav-db10-integrated-doc.log`. `nd-db` still has
**22 rustdoc warnings** (pre-existing; no net increase after `f80b9704` fixed the DB-12 HTML-tag warning). Workspace docs
success does not mean warning-free. No new standalone DB/doctest run is claimed. These are dated code-HEAD results, not
proof for later diffs or mounted/end-to-end behavior.
## Earlier direct docs-lane gates (`4ab63cd5`, historical, 2026-11-19)

Measured by **this docs lane** on a clean integrated tree from `rust/` with its default `rust/target` (`cargo 1.99.1`,
`rustc 1.99.1`, Homebrew, macOS arm64); the test runs used `--locked --all-targets --all-features` as recorded per row
below, and every command exited **0**. `git diff --check` and `git status --porcelain` were clean before and after. These
are **dated measurements at `4ab63cd5`**, not proof for a later diff. Totals were re-tallied from the logs themselves
(`grep -c '^test result:'` plus a sum of the passed/failed/ignored fields): `/tmp/docs-sd3f-workspace-tests.log` — 85
`test result:` lines, 2114 passed, 0 failed, 1 ignored; `/tmp/docs-sd3f-nd-db-tests.log` — 40 lines, 929 passed, 0 failed;
`/tmp/docs-sd3f-doctests.log` — 6 passed.

| Command | Actual result | Source log |
|---|---|---|
| `cargo fmt --all -- --check` | Pass (diff 0, exit 0) | `/tmp/docs-sd3f-fmt.log` |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | Pass (exit 0) | `/tmp/docs-sd3f-clippy.log` |
| `cargo test --locked -p nd-db --all-targets --all-features` | **929 passed / 0 failed / 0 ignored** across **40** suites | `/tmp/docs-sd3f-nd-db-tests.log` |
| `cargo test --locked --workspace --all-targets --all-features` | **2114 passed / 0 failed / 1 ignored** across **85** suites | `/tmp/docs-sd3f-workspace-tests.log` |
| `cargo test --locked -p nd-db --doc` | **6 passed / 0 failed / 0 ignored** — a **separate axis**, never added to the 929 (the new one is `user_repo::revoke_tokens_in_tx`) | `/tmp/docs-sd3f-doctests.log` |
| `cargo doc --locked -p nd-db --all-features --no-deps` | Pass (exit 0), **22 warnings** (8 unresolved links + 14 private-item links; `grep -c '^warning:'` returns 23 because it also counts rustdoc's own summary line) | `/tmp/docs-sd3f-doc.log` |
| `cargo test --locked -p nd-dbschema --all-targets --all-features` | **61 passed / 0 failed / 0 ignored** across **3** suites | `/tmp/docs-sd3f-dbschema.log` |
| `cargo test --locked -p nd-auth --all-targets --all-features` | **107 passed / 0 failed / 0 ignored** across **3** suites (lib 79, `http_auth` 14, `login_http` 14) | `/tmp/docs-sd3f-auth.log` |
| `git diff --check` | Pass (exit 0) | this lane |

**Axis discipline:** `--all-targets` totals **exclude doctests**; the `6` doctests are their own run. Do not reconcile
totals across axes or against an earlier head.

Dated boundaries on this line of development (historical evidence, not current): `4ab63cd5` 929 DB / 2114 workspace
across 85 suites (this lane's re-measurement; the parent's integration runs recorded the same workspace total in
`/tmp/nav-db13-integrated-tests.log`); `842c32a2` 2096 workspace across 84 suites (`/tmp/nav-db09-integrated-tests.log`);
`320b8b03` 2078 across 83 (`/tmp/nav-db08-integrated-tests.log`); `9be6ad0e` 2065 across 82
(`/tmp/nav-db07-integrated-tests.log`); `162fae1d` 2055 across 81 (`/tmp/nav-boot02-integrated-tests.log`); `bfdcfeca` 2038
across 81 (`/tmp/nav-db06-parent-tests.log`); `60be7534` 2029 across 80 (`/tmp/nav-db05-parent-tests.log`); `c791dbc1` 2021
across 79 (`/tmp/nav-db04-parent-tests.log`); `74ebd81c` 841 DB across 33 suites / 2009 workspace across 78
suites; `ec84c176` 834 DB / 2002 workspace across 77 suites; `4a8aac9f`/`ff900d3e` 813 DB / 1938 workspace across 74
suites; `ab466928` 819 DB across 31 suites; `f57ae050` 778 DB / 1857 workspace; `62789ba2` 771 DB / 1850 workspace;
`f8ceeb8f` 765 DB; `60cdd184` 737 DB / 1816 workspace (69 groups). Those intermediate totals come from the parent's named
integration logs (`/tmp/nav-db04-parent-tests.log`, `/tmp/nav-db05-parent-tests.log`, `/tmp/nav-db06-parent-tests.log`,
`/tmp/nav-boot02-integrated-tests.log`, `/tmp/nav-db07-integrated-tests.log`, `/tmp/nav-db08-integrated-tests.log`,
`/tmp/nav-db09-integrated-tests.log`, `/tmp/nav-db13-integrated-tests.log`) and are re-tallied here from the logs, not
re-executed. Toolchain at the `60cdd184` audit: rustc/cargo **1.99.1**, Go **1.27.4**, macOS arm64,
ffmpeg/ffprobe **9.0.2** available. Some process tests return early when tools/codecs are unavailable; future lifecycle
gates must require their prerequisites and record what was actually exercised. No run above booted a server or scanned
a real library.

**Rustdoc debt:** the `nd-db` doc build succeeds with **22 warnings** (8 unresolved links + 14 private-item links),
re-measured at `4ab63cd5` by this lane; the set is unchanged from the earlier baseline. Do not add rustdoc
`-D warnings` as a gate without resolving that debt, and do not call a successful doc build warning-free. Measurement
pitfall recorded so it is not repeated: `grep -c '^warning:'` also counts rustdoc's own summary line, which produced a
transient 23-warning report.

**Known flake (non-DB, tracked):** `nd-stream` lib test
`the_poller_exits_on_teardown_even_when_the_client_never_hangs_up` took ~30s against its 3s deadline in one
full-workspace run (1837 passed / 1 failed); later runs, four isolated binary runs, and this lane's full-workspace run
at `4ab63cd5` were clean — flaky under load, not a regression. `nd-stream`'s
`transcode_body::tests::early_release_defeats_the_cap_and_accumulates_processes` (counts live `sleep` stub PIDs via
`ps`) is the other timing-sensitive one. The ignored test is `dump_divergences` (`nd-mapping/tests/go_oracle.rs`).
`--all-targets` totals are not a claim that doctests were run.

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
| Scanner local-FS storage subset | `e9dd8230` + merge `ff5b9c92` | `storage.rs` only: std-`fs` `list`/`stat`/`read`/`read_link`/`resolve`/`exists`, logical paths, `file://` URI parsing, explicit symlink classification. At that commit the walker, `.ndignore`/ignore policy, folder hashing and cancellation were **not implemented** — **not** SC-01 ratification or SC-02 completion. **Superseded on those four items by the `e3e0770a` row below.** |
| Scanner **SC-02 partial** (local walker + `.ndignore` + folder hash + cancellation) | `e3e0770a` (fast-forwarded onto `2e7026b2`) | `nd-scanner`: `contract.rs`/`walk.rs`/`ignore.rs`/`folder.rs` plus the `lib.rs`/`storage.rs` seam, and executed `tests/{folder_hash,ignore_policy,storage_seam,walk_policy}.rs`; the six user-approved dependencies are recorded in `decisions.md` S9 and the manifest/lock delta is limited to that approved dep set. Review findings through S9b closed. **Partial scope**: no scanner entrypoint, `MusicFS`/`ReadTags` registry dispatch, tag-to-DB ingest, full-scan orchestration, watcher or progress protocol — **not** SC-01 ratification, SC-02 completion, or MusicFS readiness, and no `*_in_tx` write has a scanner caller. |
| DB-02 scanner timestamp contract | `6d9d984d` + merge `74ebd81c` | Scanner-facing stamps/cutoffs bound as canonical TEXT via `time::value`, `library_repo::zero_time_value()` replacing raw `ZERO_TIME` integers, artwork-purge cutoff fixed to a `DateTime` bound, `runtime_scan_clock.rs` executed coverage. **Not** full scanner writes: `scrobble_buffer` `play_time`/`enqueue_time` still bind integers (see Residuals) |
| Docs consolidation | `f03aff98` + merge `3b5e6c76` | `current-status.md`/`next-work.md`/`decisions.md` consolidated to one authority chain; historical blocks marked superseded |
| DB-04 executed **property** writes | `c791dbc1` | `property_repo::put_in_tx`/`delete_in_tx` executed on the caller's transaction through `execution::execute`, update-first/insert-on-zero-row per Go, `DbOperationError::Sqlite` arm retained for busy classification; `tests/runtime_property_writes.rs` (12 executed tests). No `PropertyRepo::put`/`delete` connection-bound method and no production caller |
| DB-05 executed **library** writes + scan lifecycle | `60be7534` | `library_repo::put_in_tx` (typed `LibraryColumn` set, id-1 path-immutability guard, autoincrement + predefined-id arms, admin assignment in the same tx), `scan_begin_in_tx`/`scan_end_in_tx`/`set_scanned_pid_in_tx`/`add_artist_in_tx`; `tests/runtime_library_writes.rs` (8 executed tests). **Not** the bootstrap caller: no code creates a configured library row |
| DB-06 executed **tag** dictionary + library links | `bfdcfeca` | `tag_repo::add_in_tx` runs each 200-id chunk's dictionary INSERT then `library_tag` link INSERT on the caller's tx (`ON CONFLICT DO NOTHING` preserved); `tests/runtime_tag_writes.rs` (9 executed tests). Counts/purge (`update_counts_sql`, `purge_unused_sql`) remain renderers — DB-17/20 |
| DB-07 folder missing-state + album touch primitives | `2911b9ab` + merge `9be6ad0e` | `folder_repo::put_folder_in_tx`, `mark_folders_missing_in_tx`, `mediafile_repo::mark_media_missing_in_tx`/`mark_media_missing_by_folder_in_tx`, `album_repo::touch_albums_in_tx` and `touch_albums_with_missing_folders_globally_in_tx` (deliberately GLOBAL, Go predicate retained); `tests/runtime_folder_writes.rs` (10 executed tests). `put_folder_in_tx` takes a caller-supplied id and refuses an empty one as `FolderWriteError::EmptyId` rather than giving Go's random-id arm; the mark-list primitives take id lists and run NO statement on an empty list (Go's own chunk loop), so they are not an empty-id refusal |
| DB-08 artist writes + exact partial timestamp selection | `32c4fa5b` + merge `320b8b03` | `artist_repo::put_artist_in_tx` with the typed `ArtistColumn` set; the pre-existing defect where a filtered update force-appended `updated_at` was **corrected** to Go's `selectUpdateColumns` semantics (`runtime_timestamp_convention.rs` re-pinned). `library_artist` link is NOT written here — it stays `library_repo::add_artist_in_tx` |
| DB-09 atomic album base + participant + tag writes | `d938d159` + merge `842c32a2` | `album_repo::put_album_in_tx` executes base upsert then unconditional participant delete + optional insert, then unconditional tag delete + optional insert, all on the caller's tx. **FTS is not issued by any statement**: the fresh schema's own `album_fts_ai`/`_au`/`_ad` triggers maintain `album_fts`, and `runtime_album_writes.rs` (18 executed tests) observes it through `MATCH` and the contentless row count. Album aggregation/`equals` remain SC-owned (`decisions.md` S4) |
| DB-10 identity/base/junction writes | `65072b89` + signed merge `6e5ca97d` | `put_mediafile_in_tx`, 31 executed tests. Empty-ID Match miss mints inside each tx attempt; retries may mint different GUIDs, failed attempts publish no ID, only committed result returns ID. BUSY test fails initial INSERT before junction/FTS, not commit failure of a completed bundle. Review APPROVE, follow-up P3s fixed (parent report); no production scanner caller |
| DB-12 artwork invalidation/enqueue subset | `e5d0592d` (fast-forward), rustdoc fix `f80b9704` | Transaction-bound delete-for-items and three enqueue variants; 14 executed tests; review APPROVE after follow-up test fixes (parent report). Not worker/full scanner; no production scanner caller |
| DB-13 epoch-only token revocation | `eae4ba1d` + merge `4ab63cd5` | `user_repo::increment_token_epoch_sql` (guarded `token_epoch = token_epoch + 1 … RETURNING token_epoch` with `typeof(token_epoch) = 'integer'` and `< i64::MAX`) executed by `revoke_tokens_in_tx` on the caller's tx, with `RevokeError::NotFound`/`Exhausted` classified by a same-tx existence read; `tests/runtime_token_revoke.rs` (18 executed tests). Epoch-only revoke is a **Rust-side addition** — Go bumps the epoch only inside the password UPDATE. No route/BOOT consumer exists |
| BOOT-02 native expiring login sessions | `c6e220eb` + merge `162fae1d` | `login.rs` mints via `Authenticator::create_native_session_token` (native audience + checked positive `exp`), replacing the audience-less `create_token` call that made the login token unusable at `AuthGate::Api`; `tests/login_http.rs` (14) and `tests/http_auth.rs` (14) executed, including a legacy audience-less **negative control** and an epoch-revoke round-trip. The remaining HTTP API-key extraction/issuance middleware is **not** part of this scope |

Executed write primitives are **transaction-bound and caller-trusted** (`WHERE id = ?` and nothing else, exactly like
Go): they own no scope filter, no auth check, and no commit. None has a production caller at `f80b9704`.

The old scout's S-D3b meant folder/playlist and its S-D3c meant album/media-file; `hydrate-scan-contract.md` numbers
the same lanes differently (see the numbering-drift note in `next-work.md`'s historical index). All rows above are in
this baseline — do not cherry-pick or repeat them. Playlist rules decoding is wired; old labels saying otherwise are
stale.

## Capability axes (source-inspected, not a completeness percentage)

| Surface | Rendered / decoded | Production execution | Mounted / end-to-end |
|---|---|---|---|
| Shared marshal, JSON/role/FTS feeders | Present; eight row hooks exist | Consumers below; marshal tests also manually execute SQL bundles | Not a full entity-write or scanner contract |
| User/player | Read and mutation builders/records | Substantial reads and writes execute, incl. DB-13 `user_repo::revoke_tokens_in_tx` (`eae4ba1d`) | Auth consumers exist (login mints a native expiring session at `c6e220eb`); no server bootstrap, no revoke route |
| Share/user properties | Present | Raw-connection reads/writes execute, outside the shared execution helper | Full share service not implemented |
| Album/media-file/artist/folder/playlist/radio | Record/hook/read builders present | Substantial reads; executed entity writes landed for **artist** (`32c4fa5b`), **album** (`d938d159`), **folder** (`2911b9ab`) and media-file **missing-state plus identity/base/junction writes** (`put_mediafile_in_tx`, DB-10 `65072b89`) | No ingest or mounted API |
| Library/property/tag | Plain mappers and builders present | Reads **and** executed writes landed (property `c791dbc1`, library + scan lifecycle `60be7534`, tag `bfdcfeca`) | No library bootstrap/scan-lifecycle caller |
| Playlist tracks/scrobble buffer | Delegating hooks and builders present | Partial reads; buffer dequeue executes | Not full playlist/scrobbler services |
| Artwork queue | Builders and plain mapper present | Reads/counts, `purge_queued`, transaction-bound enqueue variants and artwork delete-for-items execute (DB-12 `e5d0592d`) | No production scanner caller; worker lifecycle not implemented |
| Transcoding module | Builders present | **Executed reads landed at `f8ceeb8f`** (`read_all`/`count_all`, `command` masked for a non-admin) | No route, no default-profile seed; profile **editing** out of scope |
| Artwork/genre/playqueue/scrobble/plugin/smart_playlist | genre/playqueue/scrobble executed reads **landed at `272c9c66` (DB-22)**; artwork item-image lookup and invalidation execute; plugin/smart_playlist remain render-only | Artwork item-image lookup separately executes in `artwork_hydration.rs`; artwork `delete_for_items_in_tx` executes | Not full repository/service completion; `plugin` decided for deletion by D-PLUGIN (`decisions.md`, `89b72894`), not yet executed; `smart_playlist` not a separate Go repo |
| DB store/pool | Sync/async lease and transaction/retry APIs exist | SQLite work runs on the blocking boundary | Effective per-connection configuration **landed** (DB-03); typed seam **landed** (`48e65806`/`ec84c176`, DB-00) and DB-04…DB-10/12/13 consume it |
| Schema | Fresh DDL, migrations/marker, extensions, indexes, backup/optimize | Executed schema tests (61 at `4ab63cd5`) | No default library row; repair deferred; no bootstrap caller |
| Tagreader/mapping | Real lofty backend and metadata mapping | Crate-level implementation/tests | No storage→record→DB ingest caller |
| Auth | Credential/JWT/login/refresh/middleware components | Crate-level mounted test routers (107 executed at `4ab63cd5`) | Login→native-audience round-trip **closed** at `c6e220eb`; HTTP API-key extraction/issuance middleware still absent; no production router |
| Stream #15 | Handler/cache/limiter/archiver implemented | Crate-level serving/lifecycle tests | **Component-level complete, not HTTP-ready**; header commitment/Send adapter gaps |
| Scanner/HTTP/API/CLI | Scaffolds / empty `main` | No production scan/listener/router | **Not started at the integration boundary**; local-FS storage subset `e9dd8230`/`ff5b9c92` then SC-02 partial walker landed `e3e0770a` (**not** SC-01/SC-02 completion) |
| Scheduler | Scheduling primitives implemented | Crate tests | Full maintenance/logging and bootstrap remain |

Repository sources: `rust/crates/nd-db/src/*_repo.rs`, `hydrate.rs`, `marshal.rs`, `store.rs`, `pool.rs`, `execution.rs`.
Executed reads do not prove all methods execute. Re-counted at integrated `master` `272c9c66` (DB-22):
(SOURCE-VERIFIED: grep for `execution::query`/`execution::execute`/`read_hydrated`/`prepare_and_then` across the 23
`*_repo.rs` modules): **19 modules execute through the shared helper** (artwork, property, playlist_track, radio,
scrobble_buffer, transcoding, album, artist, tag, folder, artwork_queue, mediafile, playlist, library, player, user,
**genre, scrobble, playqueue** — the last three added by DB-22 `272c9c66`),
**2 execute on raw connections** outside it (`share`, `user_props`), and **two are render-only** (`plugin`,
`smart_playlist` — `smart_playlist` is not a separate Go repository; `plugin` is decided for deletion by D-PLUGIN
`decisions.md`, record at `89b72894`, not yet executed).
Twenty-two executed transaction-bound write functions exist (`grep 'pub fn .*_in_tx'`):
property `put_in_tx`/`delete_in_tx`, library `put_in_tx`/`scan_begin_in_tx`/`scan_end_in_tx`/`set_scanned_pid_in_tx`/
`add_artist_in_tx`, tag `add_in_tx`, folder `put_folder_in_tx`/`mark_folders_missing_in_tx`, mediafile
`mark_media_missing_in_tx`/`mark_media_missing_by_folder_in_tx`, artist `put_artist_in_tx`, album
`put_album_in_tx`/`touch_albums_in_tx`/`touch_albums_with_missing_folders_globally_in_tx`, user
`revoke_tokens_in_tx`, plus DB-10 `put_mediafile_in_tx`, artwork `delete_for_items_in_tx`, and queue
`enqueue_in_tx`/`enqueue_all_missing_in_tx`/`enqueue_if_missing_in_tx`. `annotation_repo.rs` does not exist because annotations ride album/media-file (DB-15), so that
is not a gap.

## Landed safety gates (former open blockers, now closed with executed regressions)

- **Missing-update bind order — CLOSED at `f57ae050`** (DB-01): `mark_missing_sql` args follow Go's `.Set`/`.Where`
  order; `runtime_missing_state.rs` executes 7 real-schema tests plus a pre-fix control. Details: `decisions.md` W0.
  **Other** latent write defects remain — DB-08 found and fixed one of them (filtered artist updates force-appending
  `updated_at`, corrected at `32c4fa5b`); do not connect existing renderers unchanged.
- **Timestamp/idle semantics — CLOSED at `6d9d984d`/`74ebd81c`** (DB-02/D-TIME, `decisions.md` T1-T5): scanner-facing
  stamps and cutoffs bind canonical TEXT through `time::value`; the idle/scan-end comparison uses the deliberate
  `library_repo::zero_time_value()` API instead of raw `ZERO_TIME` integers; year-one `time::zero()` and the fresh-DDL
  `0000-00-00 00:00:00` sentinel behavior are preserved as inherited Go semantics (pinned, not "normalized"), and the
  artwork-purge integer-cutoff regression is fixed with executed coverage in `runtime_scan_clock.rs`. Not a demand for
  Go database migration support. The DB-04…DB-09 write lanes consume this convention for their stamps. **Known
  residual:** `scrobble_buffer.play_time`/`enqueue_time` still bind integers.
- **Pool configuration — CLOSED at `4a8aac9f`** (DB-03/D-OPEN): explicit typed options, per-connection PRAGMA,
  effective-state read-back before pooling, Go-compatible DSN parser (`decisions.md` P1). FK-on and the 5000 ms rusqlite
  default are pinned facts, not assumptions.
- **Typed repository/transaction errors — CLOSED at `48e65806`/`ec84c176`** (DB-00/D-ERROR, `decisions.md` E1-E3,
  retryable set H2): additive seam beside `with_tx*`, `RollbackFailed` keeps original + rollback errors reachable,
  `BusyExhausted` replaces strings, `Depleted`/`Poisoned` checkout states are distinct. Consumers adopted it lane by lane:
  DB-04…DB-09 and DB-13 all return `DbOperationError` with the concrete `rusqlite::Error` (extended code intact) on the
  `Sqlite` arm, so `store::is_busy` still classifies retryable failures.

First-wave lanes (DB-01, DB-03, DB-00, DB-02, plus the DB-15 render-only repair and the scanner local-FS storage
subset) are merged into `74ebd81c`, and DB-04 through DB-09 plus DB-13 and BOOT-02 are merged into `4ab63cd5`. DB-10 and
DB-12's executed subset are merged into `f80b9704`; **the SC-02 partial (local walker, `.ndignore`, folder hash,
cancellation) is LANDED at `e3e0770a`** — a partial slice, not SC-02 completion. DB-22's former DB-12 ownership block is
released; it is ready subject to parent launch and exclusive checkout ownership. `D-CURSOR`/`D-SCOPE` are recorded in
`decisions.md` (PARENT DECIDED 2026-11-20, corrected record `8b5b3925`), so DB-11 is the spine's next launchable package.

## Remaining blockers and integration obligations (SOURCE-VERIFIED)

1. **Executed scanner writes and queries are partly missing.** Landed on the integrated tree: leaf property/library/tag
   writes, folder/album/media-file missing-state and touch primitives, artist and album entity writes with participant/tag
   junction atomicity (`c791dbc1`, `60be7534`, `bfdcfeca`, `2911b9ab`, `32c4fa5b`, `d938d159`), plus DB-13 epoch revoke
   (`eae4ba1d`), DB-10 identity/base/junction writes (`65072b89`) and DB-12 artwork invalidation/enqueue (`e5d0592d`).
   **Still unexecuted:** typed folder/album-filtered scanner reads and cursors (DB-11), the recent/touched read pins,
   move/reference/annotation transactions (DB-16), full DB-15
   annotation execution, and scoped eleven-operation GC. `MediaFileRepo::get_all` does not forward arbitrary scanner
   filters, and a raw user-supplied SQL fragment is not an acceptable substitute. Executed primitives also still have
   **no production caller**: nothing invokes them from a scanner, bootstrap, or route.
2. **Scanner mapping/equality seams** — `nd_mapping::map_file` (`nd-mapping/src/lib.rs:78-86`) forces library 1 and an
   empty folder id; `nd_model::album::equals` (`album.rs:174-176`) compares only ids. The scanner plan must own a tested
   aggregate/equality bridge or obtain the domain-owner decision (`decisions.md` S4). IPC correction: Go's external
   scanner gob carries `ProgressInfo` (`scanner/external.go:86-96`, `cmd/scan.go:71-76`), not tag-reader results;
   watcher/progress-protocol are later parity packages, not gates for a limited in-process fixture scan.
3. **Auth integration** — the login-audience gap is **CLOSED at `c6e220eb`** (BOOT-02): `login.rs:142-145` mints through
   `Authenticator::create_native_session_token` (`credentials.rs:163-175`, native audience plus a checked positive `exp`),
   so a login token now passes `AuthGate::Api` (`middleware.rs:121-129` → `check_api_claims`, `credentials.rs:356-367`);
   `tests/login_http.rs`/`tests/http_auth.rs` execute the round-trip, an epoch-revoke rejection, and a legacy audience-less
   **negative control**. Preserve expiring sessions, refresh/epoch checks and the strict native audience; do not replace
   sessions with non-expiring API tokens or weaken gates. **Still open:** no canonical HTTP API-key extraction/issuance
   middleware — `verify_api_key_hash` (`credentials.rs:395`) and `PlayerRepo::find_by_api_key` have no non-test caller, and
   `supplied_tokens`/`resolve_username` (`verifier.rs:78-108,122-140`) cover JWT header/cookie/query plus ExtAuth only.
4. **Stream integration** — `StreamHandler::serve` (`handler.rs:1302-1507`) returns status/headers after pumping and
   teardown, `prepare` is private and may change the body kind, and the boxed-future/trait-object `Send` contract needs a
   deliberate network adapter seam. Preserve bounded backpressure, cancellation, lease/slot ownership, cache semantics,
   and post-header versus pre-header error behavior. Network tests must exercise real body backpressure/drop, child
   cleanup and recovered slots with real tools/codecs; an early-returning test is not lifecycle evidence.
5. **No library row** — fresh DDL has none; configured-library creation belongs to an explicit bootstrap
   transaction/caller using typed executed DB methods, not an invented schema-golden seed.
6. **Scope decisions** — `plugin_repo.rs` is **DECIDED** (D-PLUGIN, `decisions.md`, record `89b72894`: delete the module,
   its tests and the `lib.rs` export as out-of-scope dead code) but **not yet executed**; the `plugin` table is absent from
   the fresh schema, so every statement that module renders fails until deletion lands. Still open: Part C per-key config
   scopes; oracle-harness authorization for new tracked Go harnesses.

## Bounded source-level residuals (amended at integrated `master` `f80b9704`; older line references retained, not a completeness claim; SC-02 partial landed later at `e3e0770a` did not re-check these lines)

- `scrobble_buffer_repo::enqueue_sql` still binds `play_time`/`enqueue_time` as **integers** (`Value::Integer`,
  `scrobble_buffer_repo.rs:206-226`) on an INTEGER-affinity `datetime` column, while the DB-02 contract binds TEXT
  elsewhere. Distinct and deliberate: `scrobbles.submission_time` is intentionally an integer epoch and is not part of
  this convention.
- `find_recent_files_by_properties_sql` (`mediafile_repo.rs:1903`) is converted to the TEXT cutoff shape but has **no
  executed regression pin**; the sibling MBZ path is pinned (`runtime_scan_clock.rs:222`, which executes
  `find_recent_files_by_mbz_track_id_sql`), so the properties path is the uncovered one.
- **Still unexecuted on the integrated tree:** typed scanner reads/cursors with filters (DB-11), the DB-14 folder-delta
  transaction, DB-16 move/reference transactions, and full DB-15 annotation execution. Tag count refresh and purge
  (`update_counts_sql`/`purge_unused_sql`) and `album_repo::refresh_play_counts_sql` remain renderers.
- Executed write primitives are **caller-trusted primitives, not services**: no scope filter, no auth check, no commit,
  and no production caller. Empty-id `Put` refusal — `FolderWriteError::EmptyId`, `ArtistWriteError::EmptyId` and
  `AlbumWriteError::EmptyId` in `put_folder_in_tx`/`put_artist_in_tx`/`put_album_in_tx` — is a **recorded port-side
  deviation from Go** (Go generates a random id), documented on each function rather than silently applied. The
  mark-list primitives (`mark_folders_missing_in_tx`, `mark_media_missing_in_tx`,
  `mark_media_missing_by_folder_in_tx`) are NOT part of that deviation: they take id lists, accept an empty one and
  run no statement, exactly like Go's chunk loop.
- The scanner local-FS subset, the **SC-02 partial** at `e3e0770a`, and the DB-15 render-only repair remain **partial
  scopes** (not SC-01/SC-02 completion, not the annotation service); storage→record ingest, aggregation/equality bridge,
  controller/watcher and progress protocol are unstarted. There is **no server readiness**: no router, listener, library
  bootstrap, or integrated stream HTTP adapter.

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

Conclusions here come from **this lane's own executed gate runs on the clean integrated `master` worktree at
`4ab63cd5`** (logs under `/tmp/docs-sd3f-*.log`), from source inspection at the pinned commits, and from the parent's
dated integration logs for intermediate boundaries. Read-only investigation briefs' local `/tmp` reports are supporting
evidence only (agent IDs and provider failure histories are deliberately not canonicalized in these docs); in-flight
lane worktrees were inspected read-only for **branch state only**, never treated as merged results. Nothing is complete
merely because it is planned, and no implementation is authorized by this snapshot.
