# Binding decisions and durable contracts (extracted 2026-11-19)

Entrypoint: [`README.md`](README.md). Authority: [`next-work.md`](next-work.md) current section for milestone order,
[`current-status.md`](current-status.md) for measured evidence. This file is the **stable home for decisions that Rust
source comments, test comments, and oracle READMEs cite**; it exists so those citations keep resolving when the dated
plan and lane transcripts in `next-work.md` are trimmed. Do not re-derive or reverse anything here without the parent.

Every entry is a settled decision or a recorded measurement/limit, with the evidence label the port uses:
EXECUTED (something was run), SOURCE-VERIFIED (read from pinned source), UNVERIFIED (asserted, not checked).
Commit/branch IDs are pinned boundaries, not claims that a later diff behaves the same.

## JSON column codecs (cited as `next-work.md` §1b traps 1-3)

- **J1 — Tags are not a plain map dump** (SOURCE-VERIFIED). Go `marshalTags` (`persistence/sql_tags.go:37-47`) emits
  role-keyed arrays of `{id, value}` with `id = model.NewTag(name, value).ID`, drops empty value lists, and serializes
  nil/empty as `{}`. `sql_tags.rs::marshal_tags` is the faithful port using `nd_id::new_tag_id`; the participant
  counterpart is `sql_participations.rs` (Go `sql_participations.go:50-65`, which also errors on an empty participant
  string). Note `sql_tags.go:15-20`'s `dbTag` is the **JSON column representation**, not the tag repository's row.
- **J2 — Integer-key ordering** (EXECUTED, Go oracle). Go marshals `map[int]string` with keys sorted **as strings**:
  `{"1":"a","10":"j","2":"b"}`. A Rust `BTreeMap<i32,String>` would emit `"1","2","10"` — different bytes. Keys must be
  stringified before sorting; pinned by `sql_json.rs` against `nd-db/tests/testdata/go_sql_json/`.
- **J3 — Escaping is a deliberate divergence** (EXECUTED). Go's `json.Marshal` escapes `<`, `>`, `&`, U+2028, U+2029;
  `serde_json` does not. Given the no-migration rule (C1/C2 below), the contract is **Go's shape and round-trip
  fidelity, not its escaped bytes**: the columns are internal, self-consistent, and every reader parses them (the
  `nd-mapping` goldens are parsed, not byte-compared). Rust decoders accept Go-written escaped bytes, so reading a
  Go-written column is safe — **carried** claim (also recorded in `hydrate-scan-contract.md`); the EXECUTED evidence in
  this repo is the golden-vs-Rust escaping comparison (`participants_json.rs`) and the `sql_json.rs` round-trip, not a
  dedicated Go-escaped-bytes decode test. Document the divergence; never silently diverge.

## Ordering rules (the "next-work.md rule" cited by participant/feeder code)

- **O1 — Persist no ordering inherited from either side** (role keys sorted; entry order = input order, never treated as
  meaningful) (EXECUTED). The heading phrase means "nothing may *depend* on order", not "no order reaches the column":
  entry order inside a role *is* the input order, it is simply not reproducible or meaningful. Go's `Participants` is a map and
  `Participants.AllNames` (`model/participants.go:174-185`) walks it directly: order is not reproducible across runs
  (executed: 40 calls over one input gave 3 distinct orders). Go's `encoding/json` **does** sort map keys
  (`encode.go:781-794`, `:184-186`), so the participant **column bytes** are stable and the Rust `BTreeMap` key order
  reproduces them. Consequences: compare oracle entries as **multisets**, never positions; role keys are string-sorted
  on both sides; entry order is input order. `search_participants` sorts and re-dedupes the names
  (`sql_feeders.rs::search_participants`), which is why `AllNames` nondeterminism stays unobservable. `full_text` is
  unaffected because `SanitizeStrings` sorts and dedupes the words. Related, from the §1b pairing note: Go's
  `NormalizeForFTS` (`utils/str/normalize_fts.go:10-44`) **preserves input order** while adding transliterated/NFD
  variants, so search-normalized text is order-sensitive by design — the ordering rule above applies to the participant
  and pair-tag maps, not to that normalizer.
- **O2 — Same rule applies to #16 pair tags** (EXECUTED, recorded as the §Then-7 hazard). Go's `parseID3Pairs`
  (`model/metadata/metadata.go:276-290`) iterates a map, so pair-tag and sub-role ordering is unreproducible across
  runs of the same binary; Rust is `BTreeMap`-ordered and matches Go's multiset, not its order. `nd-mapping`'s oracle
  compares the affected fields as multisets.
- **O3 — Neither Go golden is wholly byte-stable** (EXECUTED, review-reproduced). `go_participants` reorders entries
  and flat rows across runs; `go_feeders` is stable except `search_participants`. All other feeder fields are
  byte-stable after regeneration.

## Role representations

- **R1 — One conversion seam, never a fourth representation** (SOURCE-VERIFIED, pinned in `c7f8113b`). Three
  equivalent representations coexist: `sql_participations::Role` (owned `String`), `artist_repo::ALL_ROLES`
  (`&[&str; 14]`), and `nd_id::Role` (`&'static str`). `sql_participations`' bridge is the seam between the first two;
  all 14 keys must stay provably equivalent and no new representation may be invented.

## Timestamps (T-series)

- **T1 — Convention decision (user, 2026-11-11): Rust standard, no Go-migration compatibility.** This port is not a
  migration target, so matching Go's stored representation is explicitly not a goal. Storage uses rusqlite's `chrono`
  feature with `DateTime<Utc>` (`rusqlite-0.32.1/src/types/chrono.rs:88-93` binds `%F %T%.f%:z`; `:117-136` parses
  RFC3339 with a naive-`NaiveDateTime` fallback, so it also reads SQLite's `current_timestamp` default). Set
  `created_at`/`updated_at` explicitly instead of relying on the DDL default — Go sets them explicitly too. Pin with
  an executed write→read→compare round-trip, never a string assertion. LANDED `e13bd31a`…`a7afda2a` for
  `share_repo`, `radio_repo`, `artist_repo`, `artwork_queue_repo` via `nd-db/src/time.rs`.
- **T2 — Why it was a correctness bug, not a nit** (EXECUTED against SQLite). A declared `datetime` column type has
  **NUMERIC affinity** (SQLite §3.1 rule 3 — only TEXT/BLOB/REAL or no type give non-numeric affinity), so an
  epoch-integer bind keeps `typeof() = integer` rather than being coerced to text, and INTEGER sorts before TEXT. A TEXT-bound `Gt`/`Lt`
  predicate silently excludes integer rows, and `ORDER BY` is non-chronological in both directions (INTEGER first in
  ASC, last in DESC).
- **T3 — D-TIME (parent, 2026-11-18): scanner stamps are TEXT via `time::value`; Go's `<> canonical zero` idle
  predicate is kept verbatim.** Measured with a Go oracle: `time.Time{}` binds as `0001-01-01 00:00:00+00:00`, and the
  DDL sentinel `0000-00-00 00:00:00` makes Go's own `ScanInProgress` count a fresh library as in-progress — inherited
  behavior, pinned rather than "fixed". The real defect was INTEGER affinity, which T1/T2 removed. Writers, bounds,
  defaults, the idle predicate, and the decoded model must be tested together. DB-02 is **LANDED `6d9d984d`/`74ebd81c`**:
  scanner-facing stamps/cutoffs bind canonical TEXT via `time::value` and the idle predicate uses
`library_repo::zero_time_value()`. Known residual resolved by **T6**: `scrobble_buffer_repo` still binds
  `play_time`/`enqueue_time` as integers (decision recorded; lane pending). `find_recent_files_by_properties_sql`
  still has no executed regression pin (separate residual).
- **T4 — Sentinel decode rule** (EXECUTED). `library_repo`'s `0000-00-00 00:00:00` and SQLite's naive
  `CURRENT_TIMESTAMP` decode to `None`, mirroring Go discarding its parse error into a zero `time.Time` that callers
  test with `!LastScanAt.IsZero()` (`model/library.go:64`, `scanner/controller.go:124`); a genuinely malformed date
  stays an error. `time::optional_sentinel()`.
- **T5 — Record-shape divergence, documented not "fixed".** A cell that scans as Go's zero time is returned as `None`
  rather than a zero stamp (`hydrate.rs::go_zero_or_none`), consistent with the write path treating `None` as "never
  stamped". Annotation stamps keep the stored zone while record stamps render as `Z`.
- **T6 — Scrobble-table timestamp boundary (PARENT DECIDED 2026-11-20):** `scrobble_buffer` is a **TEXT-timestamp
  table**; `scrobbles` is an **epoch-seconds table**. T1-T5's scope was scanner-facing stamps, so neither table was inside D-TIME's
  covered set — this closes that gap rather than exempting it. Parent re-verified the load-bearing Go source:
  `scrobble_buffer.play_time`/`enqueue_time` are declared `datetime` (`db/migrations/20260405124200_fix_schema_inconsistencies.sql:
  39-40`) with `UNIQUE (user_id, service, media_file_id, play_time)` (:42), and Go `Enqueue` binds `play_time`/`enqueue_time` as
  `time.Time`/`time.Now()` (`persistence/scrobble_buffer_repository.go:54-63`), which go-sqlite3 stores as TEXT; `scrobbles.
  submission_time` is declared `INTEGER` (`20251206013022_create_scrobbles_table.sql:12`) and Go writes `submissionTime.Unix()`
  (`persistence/scrobble_repository.go:52`), so it stays integer seconds. Decision: Rust binds `scrobble_buffer` TEXT via `time::value`
  (the current integer bind at `scrobble_buffer_repo.rs:206-237` + `now_unix` second-truncation is a DEFECT to replace, not a
  deviation): (i) under T2's NUMERIC-affinity rule a mixed INTEGER/TEXT `play_time` column makes `ORDER BY play_time, s.rowid`
  (`:74`) non-chronological; (ii) second-truncation collapses sub-second-distinct plays that Go's ns-precision TEXT keeps, and
  `play_time` is in the UNIQUE key, so a buffered scrobble Go keeps is silently rejected (also through the `OR IGNORE` reassign
  leg, `mediafile_repository.go:421-428`). `scrobbles.submission_time` stays INTEGER — converting it would BE the divergence; every
  consumer is integer-native (`scrobble_repository.go:17-22`, `plugins/host_scrobbleretriever.go:150-157`). No Go code compares either
  table's stamps against a TEXT timestamp (verified across `persistence/` and `core/scrobbler/`), so there is no cross-table parity
  risk. The read side keeps the DUAL decode (`hydrate.rs::get_dt` + `driver_integer_dt`), because Go-written and `CURRENT_TIMESTAMP`-
  default-written naive-TEXT cells both occur; that is pinned, not normalized. Lane: fold the write-shape change + an executed
  round-trip pin (`typeof(play_time)='text'`, chronological order over mixed fixtures, sub-second dedup, an INTEGER-bound
  non-vacuity control) into #20 `nd-scrobbler`'s first commit, or run as a ~30-min DB-02b filler; it must NOT run concurrently with
  another DB writer (whole-workspace gates in one checkout). Evidence: read-only audit `e17a161b` (2026-11-20) + parent re-verification
  of the three Go citations above.

## DB open/configuration (D-OPEN, landed `4a8aac9f`)

- **P1 — Effective per-connection configuration is explicit, not inferred** (EXECUTED by the parent, 2026-11-18).
  rusqlite honors real SQLite URI params (`mode=`, `cache=`) but **not** go-sqlite3's `_journal_mode`,
  `_busy_timeout`, `_foreign_keys`, `synchronous=` conventions, so every open previously kept `journal=delete`,
  `busy_timeout=5000`, `synchronous=2`. Chosen (user option a): typed `DbOpenOptions` applied as explicit
  per-connection PRAGMA statements with **effective-state read-back before pooling**, defaults matching Go
  (WAL for file DBs, 15000 ms, NORMAL, FK on; `:memory:` keeps the memory journal), while the config string stays
  Go-compatible through a DSN parser that keeps genuine URI params and rejects unknown ones.
  - Baseline facts to keep: bundled SQLite enables FK (`libsqlite3-sys 0.30.1/build.rs:123`); rusqlite sets a 5000 ms
    busy timeout (`inner_connection.rs:119`). FK-off / no-busy-handler claims are therefore unproven. Assertions must
    cover the effective path, FK, journal, timeout, synchronous, and extensions on **every simultaneously held pool
    connection**.
  - Bad-citation correction: Go has no `db.rs::NewSqliteDb` symbol here; Go uses the `db.Db()` singleton
    (`db/db.go:41-68`).
  - Go's WAL/15000 ms/synchronous-normal defaults are a source reference, not permission to silently change Rust
    configuration.

## Errors and transactions (D-ERROR, DB-00; LANDED `48e65806` + merge `ec84c176`)

- **E1 — Typed seam, no string loss** (parent decision option B, reviewed and LANDED `48e65806`/`ec84c176`). Additive
  `try_with_tx` / `try_with_tx_retry` live **beside** the existing `with_tx*` (no signature churn): callback failures use the
  typed `DbOperationError<E>` (`Domain(E)` / `Sqlite { stage, source }`), and the runtime output is `DbRuntimeError<E>`
  (`Domain(E) / Sqlite { stage, source } / BusyExhausted { attempts, stage, source } / RollbackFailed { original, rollback } /
  Pool(StoreError)`), keeping typed SQLite causes reachable through `Error::source()`. `Pool(StoreError)` keeps pool-checkout
  and blocking-`JoinError` failures unstringified. Safety landings that are now **landed**, not pending: `RollbackFailed`
  keeps both the original failure and the concrete ROLLBACK failure reachable (in `StoreError` and `DbRuntimeError`), so a
  failed cleanup never discards the original; exhausted busy retries surface `BusyExhausted { attempts, stage, source }`
  rather than a string; and pool checkout reports `StoreError::Depleted` (no live connections) and `StoreError::Poisoned`
  distinctly instead of waiting forever (`pool.rs:389-403`). `#[error(transparent)]` skipping one chain node was
  independently reproduced at `608f8784`. **Scope limit:** DB-00 is the seam only — compound writes (DB-04 onward) consume
  it later, and the retryable set is H2.

- **E2 — Retry schedule is Go's, and only for busy** (SOURCE-VERIFIED; retryable set pinned by executed
  `busy_retry_mapping.rs`). `WithTxRetry` (`persistence/persistence.go:207-228`) retries only `db.IsBusy` — primary
  `SQLITE_BUSY` 5 only, see H2 — on a 5s/10s/15s schedule (`:203,:205,:225`) and never retries a domain error.
  Retry closures may run again: no repeated filesystem/network/event side effects inside them; inputs are immutable or
  preplanned per retry. Dropping an async await does not cancel an already-started blocking SQLite transaction.
- **E3 — COMMIT-stage busy is reachable** (EXECUTED): a reader transaction on one handle makes another handle's COMMIT
  fail with extended code 5, so the Commit branch must not issue a second ROLLBACK.

## Missing-state writes (DB-01, landed `f57ae050`)

- **W0 — Bind order follows Go's `.Set`/`.Where` order** (`persistence/mediafile_repository.go:459-461`). The
  previous `mark_missing_sql` was a silent no-op (0 rows affected) and, with numeric-looking ids, could overwrite the
  wrong row; `runtime_missing_state.rs` executes 7 real-schema tests plus a pre-fix control (reverting the fix fails
  5 of 7). Lesson for the whole write layer: **renderer tests cannot catch this class — every write path needs an
  executed write→read-back test.** Assert SQL **and ordered binds**, then execute on a fresh schema and read stored
  rows and side tables back.

## Write-map and hydration invariants

- **W1 — Columns and args are one unit** (the pre-`8c89aeb9` defect class, cited as `next-work.md` §Current-state).
  The three caller-column `put` renderers built independent `columns`/`values` vectors and could desync; squirrel
  `SetMap` sorts column names and emits values positionally against that order (`squirrel@v1.5.4/insert.go:271-283`).
  The invariant is **columns and args emitted together** — not "id binds first", which was an invented rule that
  encoded the bug. `MarshalledRow` is the only shape a repo adapter receives.
- **W2 — Write-map key invariance** (EXECUTED; the target of the `next-work.md` pointer
  "Write-map invariance", which `artwork_hydration.rs` and `runtime_hydrate_album_mediafile.rs` cite by line range): `album_row` stays at
  48 keys (45 base + 3 FTS) and `mediafile_row` at 66 (63 base + 3 FTS), with none of the hydration-only columns
  present, and a hydrated record re-renders byte-identical INSERT/UPDATE legs to one whose derived fields are empty.
  Forbidden write-map keys include the read-only artwork legs (`image`, `album_image`) and, for media_file,
  `blur_hash`, `thumb_hash`, `dominant_color`, `image_width`, `image_height`. Assert key sets **derived** from the live
  DDL/`put_cols`, never a guessed count.
- **W3 — Annotation play count is an expression column** (EXECUTED). `sql_annotations.go:80-83` yields REAL storage,
  so rusqlite's Integer-only `FromSql for i64` aborts; `hydrate.rs::annotation_play_count` reproduces Go's int64 scan
  arm — `asString`'s `%g` spelling (`database/sql/convert.go:497-508,570-571`) then `strconv.ParseInt`, reusing
  `criteria_sql::go_fmt_float64`. Exponent spellings (`1e+06`) return Go's conversion error naming `play_count`;
  truncation was deliberately **not** adopted because Go surfaces the failure. Pinned on go-sqlite3 v1.14.52's
  bundled SQLite (`select sqlite_version()` = 3.53.4).
- **W4 — NULL semantics are pinned, not assumed** (SOURCE-VERIFIED + EXECUTED). Go delegates to `database/sql`: NULL
  into a non-pointer numeric/string/time field **errors**, it does not zero (`convert.go:489-535`); pointer fields
  become nil. Go's nullable pointers: album RG + `external_info_updated_at` (`album.go:53-54,62`), mediafile bit
  depth/BPM/RG (`mediafile.go:67,84,95-98`), artist timestamps (`artist.go:39,45-46`), playlist evaluated
  (`playlist.go:44`), user last login/access (`user.go:16-17`), share expiry/last visited (`share.go:19-20`),
  annotation timestamps (`annotation.go:10-14`); `model/artwork.go:65-67` warns about non-pointer
  `attempted_at`/`updated_at`. `join_text` distinguishes "column absent" (Go's zero, `dbx@v1.12.0/rows.go:80-86`)
  from "present and NULL" (Go's scan failure). dbx binds refs only for result columns, so an absent column reads as
  the zero value — a residual, unreachable from any live read whose projection carries both JSON shims.
- **W5 — Eight `PostScan` hooks, five `PostMapArgs` hooks, and nothing else** (grep-verified). Hooks: album, artist,
  folder, mediafile, playlist, playlist_track, scrobble_buffer (delegates to mediafile's and overwrites
  `MediaFile.ID = media_file_id`), and **user** (`dbUser.PostScan` unmarshals `LibrariesJSON`,
  `user_repository.go:36-42`). Models with **no** hooks — library, tag, genre, radio, share, scrobble, transcoding,
  property, artwork, artwork_queue, plugin, user_props, smart_playlist — must not get invented Rust hooks;
  `smart_playlist` is not a separate repository at all (`smart_playlist_repository.go:15-23`). DTOs stay local to
  `nd-db`; `nd-model` is #06's crate and shared — do not touch it for hydration.
- **W6 — Artwork hydration is a second layer, outside `hydrate.rs`** (SOURCE-VERIFIED,
  `hydrate-scan-contract.md`): the eight PostScan hooks are pure-column and none queries; `hydrateItems`' batched
  `item_artwork` lookup lives in `artwork_hydration.rs`. Kind is the **prefix** (`al`/`mf`/`ar`/`pl`/`ra`), never the
  kind name. A failed artwork lookup yields an **empty map with earlier chunks discarded** and the caller
  swallow-and-logs; the port swallows the same failure and **exposes** the text as `ReadOutcome::artwork_error`
  because `nd-db` has no logging dependency. Empty id lists issue no statement; `BATCH_SIZE = 200`; ids are collected
  in slice order **without dedup**. There is no playlist-track artwork kind (`model/artwork_id.go:29-33`), so playlist
  tracks reuse the media-file wrapper, and there is no folder wrapper in that file.

## Lowercasing (Go `strings.ToLower`, persisted identity)

- **L1 — Go's `strings.ToLower` is not Rust's `str::to_lowercase()`** (EXECUTED differential sweep, 1488 changing
  runes; goldens Go-measured). Non-ASCII Go path is `Map(unicode.ToLower, s)`, rune-independent, so it never applies
  Unicode final-sigma folding, and Go maps U+0130 to plain `i` where Rust yields `i` + U+0307. Because tag ids are
  persisted and tag values are arbitrary user text, the port would have minted different ids than a Go-written
  database for the same tag. `nd_utils::str::to_lower` replicates Go exactly and `nd_id::new_tag_id` uses it
  (`ΟΔΥΣΣ` = `0UEOhdgRzYCxNU4pOSozfz`, `ΙΣ` = `3iW1R9fgXXkzfoV7433up2`, `İstanbul Jazz` =
  `3zPshTNGcEwd2fDkQE5r0A`, `İ` = `6JztUNsZpP9q8P7zDbGeMs`).
- **L2 — Same class/severity at these callsites, all using `to_lower`**: `nd-mapping/src/persistent_ids.rs`
  (PID album-artist/album values and `legacy_album_id` are hashed into persisted `pid`/`album_id`),
  and `nd-utils/src/str.rs` `sanitize_field_for_sorting*` (persisted `sort_*`/`order_*`), `sanitize_strings`
  (feeds FTS columns), `normalize_for_fts` (lowercase dedupe decides which variants persist). Tests pin Greek final
  sigma and U+0130 through the persisted paths.
- **L3 — Deliberately deferred as safe**: criteria/map keys normalized on both sides inside the Rust process, ASCII
  configuration tag names, ASCII extension/codec comparisons, `nd-playlists` M3U keys built and looked up
  symmetrically, and the `nd-auth` username comparison (non-ASCII edge only). `nd-mapping/title_tables.rs::LOWER_SPECIAL`
  models Go's `Title()` title-casing, a different function that intentionally matches Rust-style folding — unchanged.

## Scanner scope and contracts (S-series)

- **S1 — Minimum executed surface for a fresh scan is 8 repositories + 1 shared mapper, not ~14** (SOURCE-VERIFIED):
  `library_repo`, `folder_repo`, `mediafile_repo`, `tag_repo`, `album_repo`, `artist_repo`, `property_repo`,
  `artwork_queue_repo`, plus the shared row mapper — because phases 2/3/4 and `finalize` **read back** what they wrote.
  `User().FindFirstAdmin` (`scanner/phase_4_playlists.go:56`) already executes. Treat the "8 repos + 1 mapper" list as
  a core-read inventory, **not** a full #17 completion gate: phase 1 invalidates artwork, phase 4 needs the playlist
  service/CRUD, and GC cleans playlist orphans/annotations.
- **S2 — Orchestration order** (`scanner/scanner.go:150-179`): phase 1 → 2 sequential, 3 ∥ 4, then GC →
  enqueue-missing-artwork → refresh-stats.
- **S3 — IPC correction.** Go's external scanner gob carries `ProgressInfo` (`scanner/external.go:86-96`,
  `cmd/scan.go:71-76`), **not** tag-reader results. Watcher/progress-process protocol are later parity packages, not
  gates for an explicitly limited in-process fixture scan. Watcher licensing is not a blocker: Go pins
  `github.com/rjeczalik/notify v0.9.3`, which is MIT (verified in the module cache) — choose a Rust watcher on behavior.
- **S4 — Mapping seams the scanner must own** (SOURCE-VERIFIED). `nd_mapping::map_file`
  (`nd-mapping/src/lib.rs:78-86`) forces library 1 and an empty folder id — the scanner must supply real library/folder
  and storage facts, never persist those defaults. `nd_model::album::equals` (`album.rs:174-176`) compares only ids
  and is not Go's aggregation/change detection; the scanner plan owns a tested aggregate/equality bridge or obtains the
  domain-owner decision.
- **S5 — Recorded Go divergences that are not port bugs**: `playlistTrackRepository.Count` LEFT-joins `media_file`
  (`:86`) while `CountAll` INNER-joins it (`:82`), both applying the library filter on alias `f`, so for an admin
  (filter skipped) Go's `Count` includes a track whose media file is gone and `CountAll` does not (executed
  `adm Count=4 | CountAll=2`). The port's `count_query` uses `inner_join`, pre-dating that lane; `Count` belongs to
  the REST lane. Also: `library_artist.stats`-backed sort keys reference `artist.stats`, which
  `db/migrations/20250701010108_add_multi_library_support.go:59` dropped and the fresh DDL does not carry
  (`schema_ddl.rs:48`), so those keys are broken upstream too; a future wiring sorts on
  `library_artist.stats`.
- **S6 — Non-smart-playlist filter shape** (EXECUTED against pinned squirrel): a nil `Eq` renders `IS NULL` with no
  placeholder (`expr.go:182-183`, `if val == nil`), so `rules = ?` with a NULL bind excludes exactly the NULL-`rules`
  rows a non-smart `Put` writes. `Or` legs of an empty query render `1=1`, not `1=0`. Every `Or` these builders emit
  must be parenthesized (`expr.go:384-386`) because SQL binds `AND` tighter than `OR`. Four fragments still emit a
  bare `OR` and are tracked as residuals in `hydrate-contract.md` (`child_playlist_filter_sql`,
  `criteria_sql::Cond`'s `Or` join, `playlist_track_repo::add_discs_sql`, `artwork_queue_repo::source_filter_sql`).
- **S7 — Existing local walker remediation (PARENT DECIDED 2026-11-20; narrow, not SC-01 ratification).** Authorize only
  fixes to the existing SC-02 local walker slice, with no new dependencies or manifest/lock changes: use logical library-relative
  components for ancestor `.ndignore` preload; make async cancellation sticky for pre-subscription observers; report ignore-pattern
  warnings once per actual file load/push (including repeated loads); exclude unrepresentable non-UTF-8 names with raw-path warnings
  and mark the owning folder `EntriesIncomplete` without lossy identity/matcher conversion. `WalkHandle::terminal()` drains and
  discards remaining events through channel EOF before joining; `Complete` means the producer completed, not that another caller
  observed every event. Callers that need events use `next()`/`drain_and_join()`; preserve the worker's terminal error/outcome.
  Match Go's broken-link `modTime` ordering without moving all accumulation after classification. Keep conservative regex refusal as
  an explicit policy, correct its rationale/goldens; retain the typed hash error and exclusion for unavailable captured mtime and
  correct its comment. Remove the unreachable `EnumerationIncomplete`/`Ignored` coverage variants/branch. Preserve separate traversals
  for distinct overlapping targets; exact duplicate target strings remain Rust-deduplicated as an accepted normalization deviation from
  Go's literal target-list loop. SC-06 owns persistence idempotency. This approves no other scanner interface, dependency, manifest, Go-source
  or cross-package work; SC-01 remains **DECISION REQUIRED**, and these repairs do not establish SC-01 ratification, SC-02 completion
  or MusicFS readiness. Merge remains gated on implementation, independent review and parent CLI validation. Source basis:
  `/tmp/nav-sc02-final-review.md` and the read-only remediation plan; no execution is implied.
- **S8 — Second-reviewed SC-02 narrow repairs (PARENT DECIDED 2026-11-20; not ratification).** Authorize only source/test repairs in
  the existing scanner slice, without new dependencies or manifest/lock/Go changes: preflight every logical target component and reject
  non-UTF-8 targets with typed `NonUtf8Name` plus a raw-path `NameNotUtf8` summary warning; do not reject a non-UTF-8 configured root.
  Exclude a followed link whose resolved endpoint basename is unrepresentable, mark its owning folder `EntriesIncomplete`, and retain the
  raw endpoint in both folder and terminal warnings. Put `EntryStatFailed` in both warning collections; increment `folders_emitted` only
  after the event is accepted by the channel. Refuse Go-valid regexes using `\d \D \w \W \s \S \b \B` with the existing
  unsupported-pattern warning (an accepted conservative deviation; do not globally disable Unicode or attempt lowering here). For local
  ignore files, preserve a valid final partial pattern before reporting a read error, never substitute an ignore-all pattern for an empty
  error prefix, and match Go `bufio.Scanner`'s ordinary local-file buffer ceiling (M=65,536, counting raw terminator bytes); do not claim
  parity for arbitrary Reader `(n, err)` combinations. Match Go `strings.EqualFold` for this ASCII blocklist, including the long-s and
  Kelvin-sign ASCII fold orbits, without claiming general Unicode case folding. Make permission tests branch on observed access; correct
  stale regex/token-limit claims; add distinct-overlap traversal coverage without changing exact-duplicate normalization. These repairs do
  not authorize new scanner APIs or imply SC-01/SC-02/MusicFS readiness; independent review and parent CLI gates remain required.
  Source basis: the second critical-review report and the read-only reasoner audit; no execution is implied.
- **S9 — SC-02 manifest/dependency record + S8 close-out dispositions (PARENT DECIDED 2026-11-20; merge gate closed, not ratification).**
  S7/S8's "no new dependencies" clause meant *beyond the user-approved set below*; the walker cannot compile without it, and it was never
  recorded here. Durably recorded now: `rust/crates/nd-scanner/Cargo.toml` carries exactly six direct dependencies — `nd-mapping`
  (path), `chrono` 0.4, `md-5` 0.10, `regex` 1.13, `tokio` (workspace), `thiserror` (workspace) — every one user-approved for this
  lane and already pinned in `rust/Cargo.lock`; the only lock diff is the `nd-scanner` dependency edge, adding no package, version,
  or checksum. No other manifest may change. The merge commit must keep the manifest/lock diff visible so the reviewer sees it.
  S8 close-out dispositions from the second-round review (basis: `/tmp/nav-sc02-s8-review.md`): (a) the raw-byte fixtures' accidental
  lowercase-`ff` assertions are replaced with explicit `\\xFF`-style rawness checks and the lossy `LogicalPath` `Display` is noted as
  showing U+FFFD while `message`/`Debug` keep the exact `\\xFF` form (display diagnostics only; a quoted basename can
  never be a `Path`-Debug suffix, so rawness tests assert containment of the exact escape, not `ends_with` of a quoted
  name); (b) the `logical_to_string` unreachable fail-fast is noted (its order-claim was wrong — Go's `isIgnoredEntry`
  is blocklist-first like the port; S9b corrected the comment, code order unchanged);
  (c) the Linux-gated raw-name tests are retained as the real-filesystem evidence, with the portable seam tests as the
  platform-independent layer, and their platform restriction stated in the test comments; (d) the untested warning kinds
  (`DirectoryEnumerationFailed`, `DepthLimitReached`, `TimestampUnavailable`, walk-level `EntryStatFailed`) and `MAX_WALK_DEPTH` get one
  exercised walk-level regression each where deterministically reachable, plus an `as_str()` table round-trip; where a kind is not
  deterministically reachable from a walk on this platform, that is stated, not papered over. Nothing here authorizes further scanner
  scope or claims SC-01/SC-02 completion or MusicFS readiness.

## Cursor and scope (D-CURSOR, D-SCOPE; record PARENT DECIDED, consumed by DB-11 `52b04064`)

- **D-CURSOR — Two cursor kinds, leased connection, three-way stop taxonomy (PARENT DECIDED 2026-11-20; unblocks DB-11).**
  SOURCE-VERIFIED Go truth (re-verified against `scanner/*.go` and `persistence/` after the first recording's citations
  failed audit — that version's `phase_2_artists.go:64-77`/`phase_3_albums.go:78-111` citations are WITHDRAWN: those files do
  not exist and no `WHERE id > ?`/`pid > ?` re-issue exists anywhere in `scanner/**`; there is no Go resume-pointer concept
  at all): the only streaming primitives are (i) `queryWithStableResults` (`sql_base_repository.go:411-439`) — one prepared
  statement on a pool connection, `defer rows.Close()`, scan error yielded **beside the row** (:428-431, via `wrapCursor`
  :392-406), driver error yielded as zero-value + error (:434-437), clean exhaustion = no further yield; and (ii)
  `streamByIDs` (`artwork_hydration.go:19-34`) — id pre-pass then `GetAll` per chunk of `artworkChunkSize` = 600 (= `
  artworkBatchSize` 200 × 3, defined :15-17), yielding `zero T + err` on chunk failure, non-resumable. Direct-statement
  consumers: `GetMissingAndMatching` (`mediafile_repository.go:495-518`, ORDER BY pid, **library-ID-scoped, no user filter**)
, `MediaFile.GetCursor` (:309-316), `GetTouchedAlbums` (`album_repository.go:393-403`, **library-ID-scoped, no user filter**)
, folder `GetTouchedWithPlaylists`/`GetAllWithPlaylists` (`folder_repository.go:275-299`), playlist-track cursor
  (`playlist_track_repository.go:131-141`). Id-chunked consumers: album/artist/playlist/mediafile-with-artwork GetCursor
  (`album_repository.go:306-314`, `artist_repository.go:299-308`, `playlist_repository.go:225-234`,
  `mediafile_repository.go:350-358`). Scanner interpretation: phases 2/3/4 treat **any** cursor Err as a fatal producer
  error (`phase_2_missing_tracks.go:81-85`, `phase_3_refresh_albums.go:53-58`, `phase_4_playlists.go:84-88`); phase 1 aborts
  only that folder (`phase_1_folders.go:231-244`); clean exhaustion is normal completion — NOT "stop, keep progress".
  Decision: (1) DB-11 exposes **two typed kinds** — `SingleStatement` and `IdChunked` — and callers must know which they
  hold; the "stable results" comment (:409-410) has no snapshot mechanism behind it, so stability is ordinary SQLite
  statement semantics (a held read in the default `delete` journal mode blocks writers — INFERRED from
  `nd-dbschema/src/schema.rs` pragma block); the id-chunked kind intentionally allows drift between pre-pass and chunk
  re-queries; (2) a `Cursor<'c>` owns one `Lease<'c>`/`&Connection` for its whole life, never shares it across chunks, and
  never re-checks out the store inside a visitor (`pool.rs` checkout blocks — a re-entrant visitor on a small pool
  deadlocks); (3) stop taxonomy: `Err(CursorError::Sqlite)` for query/scan failures, `Err(CursorError::Closed)` for
  post-completion use, and `Ok`-None / end of iteration = normal completion; call-site abort policy belongs to SC, per
  the phase table above — folding a SQLite error into end-of-stream is the one forbidden conflation; (4) chunk constants:
  cursor id chunk **600** (fix `artist_repo.rs:157 CURSOR_CHUNK_SIZE` from 300, which currently mis-mirrors 600 as 300 —
  verified: Go :17 is 600 and nothing in Rust consumes the constant yet), artwork info batch 200, hydration flush 200,
  touch/missing 200, subtree paths 100, `RefreshStats` 1000, `jsonCondBatchSize` 350; (5) `SEEDEDRAND` random is NOT a
  cursor: Go seeds `hasher.Reseed(seedKey)` when `Offset == 0` (:334-338) and renders the full set at once (:320-332,
  `CAST(%s.id AS TEXT)` gotcha pinned :325-326 and `playlist_track_repository_test.go:50`), so the port renders the same
  full-set SQL and may page it in 600-ID chunks ONLY while preserving the seedKey (`%s|%016x` =
  lowercased `tableName|xxh3(userId)`, :313-318) and per-connection `SEEDEDRAND`/`NATSORT` registration; (6) declared port
  deviation: `folder_repo.rs` `CursorDrain` (:1194-1206, :1486-1520) collects rows and reports the failure WITHOUT the
  failed-row prefix, where Go yields the row beside the error (:428-431) — recorded as a C5-style accepted deviation,
  not a defect to fix silently. Evidence basis: scout brief 43a749bd, **corrected** by parent re-verification of
  `sql_base_repository.go:320-338,392-439`, `artwork_hydration.go:12-54`, `mediafile_repository.go:495-518,309-316,350-358`
, `album_repository.go:393-403,306-314`, `folder_repository.go:275-299`, `scanner/phase_*.go` error arms, and the `ls
  scanner/` file census. Every cursor needs an executed test before DB-11 claims executed-read state.
- **D-SCOPE — Scope is a caller-resolved predicate; system scope ≠ user scope (PARENT DECIDED 2026-11-20; unblocks DB-11).**
  SOURCE-VERIFIED Go truth (corrected: the first recording's `buildLibraryFilter`/`sql_tags.go:47-68` citations are
  WITHDRAWN — `buildLibraryFilter` does not exist and `sql_tags.go:47-68` is `updateTags`; the real contract is
  `applyLibraryFilter`): `loggedUser` (`sql_base_repository.go:51-57`) yields the request user or `ID = "-1"` (headless).
  `applyLibraryFilter` (:261-283) **omits the clause entirely** for admin and for headless; it also omits it for a
  non-admin granted every library (`userSeesAllLibraries` :288-302 — `visible.count() == library.count()`; a user with
  **zero** grants and a non-empty `library` table therefore does NOT match `1=1`, it falls through to the clause below
  and matches **nothing**); otherwise it appends `<table>.library_id IN (SELECT ul.library_id FROM user_library ul WHERE
  ul.user_id = ?)` (:271-283) — there is **no `1=1` predicate ever emitted for a user**; `1=1` appears only as
  squirrel's empty-conjunction rendering elsewhere. Applied **selectively, not universally**: `mediafile_repository.go:
  209` (via `newSelect`), `album_repository.go:231`, `folder_repository.go:77`, the id pre-passes; **NOT** applied to
  `GetMissingAndMatching` (:493-518) or `GetTouchedAlbums` (:393-403) — those are library-ID-scoped scanner/system reads
  on a headless context; artist uses a join path (`artist_repository.go:191-203` `applyLibraryFilterToArtistQuery` +
  `artistLibraryFilter` :640, whose empty-list arm renders `Eq{"1": 2}` = match nothing); tags join `user_library`
  (`sql_tags.go:165-176`). Ownership (`ownerFilter`/`addRestriction` :67-101, A7) is a separate axis from library
  scope. Scanner scope = selected **configured library IDs**, no user filter (`phase_2_missing_tracks.go:73-75`,
  `phase_3_refresh_albums.go:49-51`); phase 4 injects an admin user (`phase_4_playlists.go:57-64`) and defers via a
  property flag when no admin exists. Decision: (1) `library_filter: Option<Sql>` stays the **only** scope parameter on
  shipped builders and DB-11 cursor methods — None = the omitted-clause case (headless/admin/all-grants), Some = the
  `library_id IN (…)` clause — resolved **once** by the caller from the store's user, never re-queried per chunk, and
  never accepted as raw caller/user SQL (the parameter is DB-rendered `Sql`, matching every existing Rust builder);
  (2) two authorities stay distinct types in the scanner layer: `ScannerScope` (configured library IDs, no user) and
  `UserReadScope` (per-request grants); (3) a non-admin with an **empty** grant set against a non-empty `library` table
  must render the deny clause (not `None`), and that is pinned by an executed test — mirroring `userSeesAllLibraries`
  :296-298 (count 0 ⇒ `false` ⇒ clause applied ⇒ empty scope matches nothing). Evidence basis: scout brief 43a749bd
  (corrected register above), verified by parent against `sql_base_repository.go:51-57,261-302`, `sql_tags.go:165-176`,
  `artist_repository.go:191-203,640`, `mediafile_repository.go:205-212,493-518`, `album_repository.go:393-403`,
  `folder_repository.go:77,275-299`, and `scanner/phase_*.go` scope sites. DB-11 must pin the empty-grant deny case,
  the all-grants fast path, and the headless skip with executed tests.

## Folder-write batch contract (D-BATCH; PARENT DECIDED 2026-11-20, unblocks DB-14; record consumed by DB-14 `de0f49ea`)

- **D-BATCH — Per-folder tx opened by DB; `persist_folder_delta_in_tx` pub(crate) (PARENT DECIDED 2026-11-20; resolves `SC.folder-write-batch-contract`).**
  SOURCE-VERIFIED Go truth (scout DB-14 memo, parent re-verified): phase 1 persists **exactly one tx per folder** —
  `persistChanges` wraps `persistFolder` in `WithTxRetry` (`scanner/phase_1_folders.go:353`), persist stage
  `Concurrency(1)` `:218-219`; in-tx order `:374-475` (cloned `albumIDMap` `:378` → Folder Put `:390` → Tag Add `:396` →
  per-artist restricted Put `:403` + Library.AddArtist `:408` + artist queue item `:413` → per-album `persistAlbum` `:419`
  (Put `:481`, warn-only ReassignAnnotation `:490`, warn-only CopyAttributes `:497`, map delete `:504`) → per-track
  MediaFile.Put `:430` → warn-only Artwork.DeleteForItems `:439` → MarkMissing `:446` → Album.Touch `:456` → enqueue
  `:466-470` with `EnqueueIfMissing` only when `lib.FullScanInProgress` `:467`). Partial failure rolls back that folder's
  entire tx then `stopWalk` aborts the walk; the only multi-folder tx is the phase finalizer (`:527-551`, SC-owned,
  outside DB-14). **Decision:** (1) DB-14 exposes `persist_folder_delta(store, &FolderWriteBatch)` wrapping
  `store.try_with_tx_retry_sync` around `persist_folder_delta_in_tx(tx, batch)`, the in-tx fn **`pub(crate)`** — Go-parity
  granularity/retry/rollback radius, retry-safety by construction (immutable batch; album-ID transitions cloned per
  attempt); SC may later widen the in-tx fn to `pub` for batching — a one-line change, not a second surface now (the
  hybrid candidate was rejected: it makes Go-divergent multi-folder batching reachable, whose busy-retry replays N
  folders and whose rollback radius covers siblings Go would have committed). (2) **Warn-vs-fatal table:** fatal = Folder
  Put / Tag Add / Artist Put / AddArtist / Album Put / MediaFile Put / MarkMissing / Touch; warn-only (tx still commits)
  = media artwork invalidation, enqueue, ReassignAnnotation, CopyAttributes (NotFound ignored per `:498`); commit/abort
  errors always fatal. (3) **ID stability:** SC pre-generates all folder/artist/album IDs (DB refuses empty —
  `put_folder_in_tx`/`put_album_in_tx`/`put_artist_in_tx`); GUID churn allowed only on the media Match path (DB-09/DB-10
  per-attempt mint rule). (4) **Retry purity:** batch immutable; no clock reads in-attempt (DB-02 caller-stamped TEXT
  stamps); no side effects inside the retry closure (post-commit `imageChanges.record` per `:363-367` is SC's, on the
  published `CommittedFolderOutcome` only — DB publishes nothing pre-commit). (5) **Copy helper (parent-controlled
  shared edit, DB-13 precedent):** Go calls CopyAttributes inside the folder tx but only renderers exist in Rust
  (`album_repo.rs` `copy_attributes_read_sql`/`copy_attributes_update_sql`); DB-14 therefore adds a narrow executed
  `copy_created_at_in_tx` in `album_repo.rs` (Go semantics `album_repository.go:331-357` incl. `0001-`/empty skip);
  DB-16's planned `copy_album_created_at_in_tx` is the SAME helper — DB-14 writes it first, DB-16 reuses it (collision
  resolved). (6) **Go quirk preserved:** `mark_media_missing_in_tx` early-return on `err != nil || c == 0`
  (`mediafile_repository.go:463-467`) stays; fixtures must not expect a zero-matching chunk's siblings to be marked.
  Evidence basis: scout DB-14 launch memo (citation audit + HEAD primitive locations), parent re-verification of the
  Go arms cited above. **DB-14 landed at `de0f49ea`** and consumed this record: `persist_folder_delta`/`persist_folder_delta_in_tx`
  (pub(crate)), the `FolderWriteStage`/`ScopeRejection`/`FolderWriteWarning` table, SC-side ID assumptions, immutable batch with
  caller-stamped `FolderStamps`, and `album_repo::copy_album_created_at_in_tx` (`album_repo.rs:1566`, the same helper DB-16 reuses).
  Review findings closed (P1 tx-loss atomicity via `is_autocommit` detection in `warn_arm`; P2 consumed-transitions retry proof).
## Bootstrap and stream seams (BOOT-00, API-04, ffmpeg; drafts 2026-11-20)

- **B1 — Workspace `chrono` placeholder is dead and wrong (PARENT DECIDED 2026-11-20).** `rust/Cargo.toml:66`
  declares `chrono = "^2"`, but no crate consumes the workspace entry (every real consumer pins `0.4` directly:
  `nd-scanner`, `nd-db`, `nd-sched`, `nd-mapping`, `nd-events`) and the lock holds `chrono 0.4.45`. There is no chrono
  2.x release, so any future crate that writes `chrono = { workspace = true }` gets a non-resolvable requirement.
  Decision: correct the workspace entry to `chrono = "^0.4"` (features per consumer) in the BOOT-00 manifest pass, and
  have consumers use `workspace = true` after that. This is a one-line manifest fix, not a new package; the lock edge
  already exists. Rationale: S9-style "record it or it bites later" — a placeholder that silently disagrees with every
  real pin is a trap for the first HTTP/consumer crate.

- **B2 — ffmpeg path resolution ignores loaded config (PARENT DECIDED 2026-11-20).** `nd-ffmpeg/src/ffmpeg_cmd.rs:
  63-95` resolves the ffmpeg binary from `nd_conf::Config::default().ffmpeg_path` (:76) — the hard-coded default struct
  (`nd-conf/src/config.rs:687` `ffmpeg_path: String::new()`), NOT a loaded config — and memoizes the result in a
  process-wide `OnceLock` (`FFMPEG_ONCE` :63). So a configured `FFMPEGPath` in the real TOML is invisible at runtime, and
  even a passed-in config could not fix later calls. Go's `core/ffmpeg/ffmpeg.go:604-622` reads the ambient loaded
  `conf.Server.FFmpegPath` under `sync.Once`; Rust has no ambient config, which is the actual gap. This is a prerequisite
  for any forced-transcode / direct-stream gate (API-05/06, #27A step 5): the gate must FAIL, not silently use a
  default-path ffmpeg, when the configured binary is absent.

  Decision: replace the `Config::default()` read with config injected once at boot and held re-readable. Recommended
  shape (a): `ffmpeg_cmd_from(&Config) -> (String, bool)` plus a boot-initialized `OnceLock` set by BOOT-01 from the
  loaded config (closest to Go's `sync.Once`; keeps the existing free-function call sites in `transcode.rs`,
  `probe_exec.rs`, and the nd-stream adapters unchanged). `ND_TEST_FFMPEG_PATH` remains the test-only override. The edit
  lives in #07-owned `nd-ffmpeg` files (outside BOOT-00's authorized file list), so lane ownership is **BOOT-01** (which
  already owns "construct singletons from committed config"); a standalone #07 micro-lane is the alternative. PARENT to
  confirm the owner before the forced-transcode gate launches.

- **B3 — `nd-stream` is not `Send`; the HTTP bridge is a DRAFT pending parent decision (API-04, 2026-11-20).**
  Verified: `archiver.rs:78` `BoxFuture` has no `Send` bound; `StreamHandler`'s `source: Box<dyn TranscodeSource<D>>`
  (`handler.rs:1022`) is a non-Send trait object; `serve` only yields status/headers AFTER the whole body is pumped and
  the process torn down; and `SinkAdapter::poll_write` (`handler.rs:1570-1604`) re-creates the write future each poll,
  dropping it on `Pending` (losing a parked mid-send). Go has no Send problem because the whole request runs on one
  goroutine; its commit point is `Stream.Serve` returning with 0 bytes written (`core/stream/media_streamer.go:159-197`;
  there is **no** `core/stream/stream.go`), after which it `panic(http.ErrAbortHandler)` and never emits a second status.
  **Fixed by this record:** do NOT add `+Send` to the global `BoxFuture` alias and do NOT `unsafe impl Send` (workspace
  `unsafe_code = "deny"`), both forbidden by boot-api.md API-04. **Still PARENT DECISION REQUIRED at the API-04 record:
  the Send-boundary strategy — owner-task bridge (recommended: per-request owner holds `StreamHandler` locally and
  `.await`s `Sink::write_all` directly, removing the re-poll defect; one-shot `Start{status,headers}` + bounded `Data`
  frames + terminal `Completion` carrying the existing `StreamResponse` observability fields) vs two-phase prepared body
  vs LocalSet actor — plus LocalSet vs dedicated `std::thread` for the non-Send owner, channel capacity/chunk bound
  (recommendation: capacity 2, 32 KiB chunks), and whether the transport module lives in `nd-stream`
  vs `nd-http` (the API-04 section is `subplans/boot-api.md:246`; this record pointed at `:239` when drafted — API-04 is at `:246`). This blocks API-05/BOOT-03 stream-lifecycle claims.
## Accepted deviations (not parity debt to undo, not bugs to fix silently)

- **A1 — Offset-without-limit renders `LIMIT -1`**: squirrel's bare `OFFSET n` is a SQLite parse error and SQLite is
  the primary engine; pinned by an executed test.
- **A2 — Pagination renders decimal literals contributing zero bound args** (`squirrel select.go:376-388,140-150`);
  `b78c01a2` removed a crate-wide divergence where the port bound `LIMIT ?`/`OFFSET ?` as args. Repository
  `to_sql()` arg vectors carry no bounds — consumers must not index pagination positions.
- **A3 — First-admin creation is atomic** in a retryable transaction (`nd-auth/src/login.rs:197-249,279`), unlike
  Go's non-atomic count/insert (`server/auth.go:122-160`). Retained deliberate improvement; see the AUTH-ATOMIC-ADMIN
  register entry in `issues.md`. Bootstrap/API work must preserve the same transaction and separately prove its
  mounted setup flow.
- **A4 — Rust error shapes, JSON diagnostic ordering/escaping, and text-level parse differences** are accepted where
  documented. No parity is claimed on inner Go scanner message text; error prefixes copied byte-exactly are limited
  to Go's own `fmt.Errorf("…: %w")` prefixes (`artist_repository.go:49,:89`, mediafile/hydrate equivalents).
- **A5 — Narrowing**: Go's `int` is 64-bit, so Go's stats decode narrows nothing; the port's `i32` fields are a
  deliberate port-wide narrowing, with accumulation through `wrapping_int64`/`wrapping_add` and clamping because
  overflow checks are on in this workspace where Go wraps.
- **A6 — Artwork failure exposure** (`ReadOutcome.artwork_error`) is a deliberate shape difference from Go's
  swallow-and-log, following the `ShareRepo::load` precedent.
- **A7 — Cross-owner status parity is canonical**: 404 on cross-owner reads (ownership-restricted select matches no
  row), 403 on cross-owner writes (`classifyOwnedWriteMiss`, `sql_base_repository.go:554-563`); Rust
  `player_repo.rs:1063-1069`. Collapsing both to 404 would be a deliberate deviation API-00 must record — not parity.

## Harness and oracle recipes (cited by `schema_diff.rs` and oracle READMEs)

- **H1 — Slice-1 pool mechanism** (DECIDED 2026-11-03; the target of the `next-work.md` pointer "Slice-1 pool decision",
  cited by `store.rs` by line range): a fixed pool of N owned
  `rusqlite::Connection`s behind one `Mutex`, N = `max(4, available_parallelism())`
  (`conf/configuration.go:1213-1216` — NOT 1, which is the e2e harness's test setting
  (`e2e_suite_test.go:243`)). `Connection` is `Send`-not-`Sync`; SQLite serializes writers on one write lock, so extra
  handles buy reader parallelism while I/O-blocked. No pool crate and no async connection surface: sync rusqlite
  inside the async shell, blocking work through `spawn_blocking` at the store boundary. Reuse #04 `schema::open` per
  connection so `SEEDEDRAND`/`NATSORT` registration is per-connection (`schema-contract.md`).
- **H2 — Busy-retry mapping** (DECIDED, **amended by the DB-00 review and LANDED at `48e65806` + merge `ec84c176`**): retry
  **only the primary `SQLITE_BUSY` code 5**, which includes `SQLITE_BUSY_SNAPSHOT` and every other extended BUSY code — the
  port masks the extended code back to its primary and compares that alone (`store.rs::is_busy`, SOURCE-VERIFIED on
  integrated `master`). `SQLITE_LOCKED` (primary 6) and `SQLITE_LOCKED_SHAREDCACHE` (extended 262) are **not** retryable and
  fail on the first occurrence, as do `rusqlite::Error::Sys` wraps, domain errors, and rollback failures. Go's truth:
  `db.IsBusy` compares the **primary** code only (`db/db.go:137-140`, comment at `:127-136`) and `WithTxRetry` reruns only on
  it (`persistence/persistence.go:218`). The earlier "extended codes 5/17 plus `Sys` wrap" wording — inherited from a
  pre-DB-00 note — was wrong: **17 is `SQLITE_SCHEMA`, not `SQLITE_LOCKED`** (LOCKED's primary code is 6, extended
  `LOCKED_SHAREDCACHE` 262), and the `Sys` arm never retried. Both claims are withdrawn: the port must not retry LOCKED and
  must not retry `Sys`. Count and schedule are unchanged: up to `TX_MAX_RETRIES` = 3 reruns at `(attempt + 1) * 5s` → 5s/10s/15s
  (`persistence.go:203,:205,:225`). Go reaches this through an ambient ctx marker (`withBusyRetry`, `persistence.go:234`); the
  port classifies at the error site, so the **retry set is identical while the mechanism differs** — no ambient context.
  Pinned by `nd-db/tests/busy_retry_mapping.rs`, whose companion shared-cache test asserts LOCKED (262) fails once.
  Citations here are against integrated `master`, not this docs worktree's `ff900d3e`, whose `is_busy` still retried LOCKED —
  exactly the defect the DB-00 review blocked.
- **H3 — Schema-drift guard** (cited by `nd-db/tests/schema_diff.rs`): dump the fully-migrated Go schema
  (`go run` with `DbPath` pointed at a temp file, then `sqlite3 .schema`) via the `nd-go-dump` harness and diff it
  machine-wise against the Rust fresh DDL — index rows (name + uniqueness) and per-table column sets. Regenerate with
  the harness when the golden drifts; eyeball parity is not a gate. Go truth lives in the migrations, not a hand-written
  golden.
- **H4 — Oracle harness invocation differs per harness**: `go_participants` takes a **directory**, `go_feeders` takes
  the **output file**; invoking either wrongly exits non-zero without writing, so an unchecked `diff` against the old
  committed golden matches trivially and looks green. Run exactly as each README documents, require exit 0, and confirm
  its `wrote …` line before trusting a diff. Oracle precedent: `nd-mapping/tests/go_oracle.rs` +
  `tests/testdata/go_oracle/` (Go harness dumps parsed goldens; unstable collections compare canonically).
  New tracked Go harnesses need the oracle-authorization decision (`subplans/README.md`); never derive Go expectations
  from Rust, and `/tmp`-only oracles are local evidence, not clone-reproducible fixtures.
- **H5 — Harness precedent for executed DB tests**: `nd-db/tests/runtime_player_repo.rs:34-46`
  (`apply_fresh_schema` + `Pool` + `SQLStore`), `_foreign_keys=on` in the DSN (`db/db.go:55`), `SEEDEDRAND`/`NATSORT`
  registered per connection via `schema::open`. Fixture paths resolve from `CARGO_MANIFEST_DIR`/repository discovery,
  never a developer's absolute path. Process tests must require their external tools/codecs; an early-returning test
  when a tool is missing is not coverage.
- **H6 — DB-15 durable Go oracle deferred to the EVID lane (PARENT DECIDED 2026-11-20):** the durable tracked Go-harness
  oracle that would certify DB-15's annotation transfer/average-recalc semantics is **deferred** to the EVID lane
  (`subplans/evidence-ops.md`, EVID-02 authorization gate recorded in `subplans/README.md` "Oracle harness"), not
  invented in this lane and not waived. Until it lands, DB-15's Go behavior is pinned by source citations
  (`sql_annotations.go:185-197`) plus `/tmp`-local Go driver evidence, explicitly labeled local and not committed
  (`decisions.md` H4: `/tmp`-only oracles are local evidence, not clone-reproducible fixtures). Consequence: `96c495e7`
  plus the `ab466928` render-only repair complete DB-15's plan scope **except that oracle** — DB-15 must not be marked
  fully DONE, and EVID-02's approval (or a declined alternative) stays an open parent decision.

## Scope and label hygiene

- **C1 — Settled product scope** (canonical statement in `issues.md` §Revision, mirrored in `component-map.md`):
  fresh Rust service with **no Go upgrade/migrate path** (no `migrate-credentials` ever exists — Revision item 6
  supersedes item 4's historical wording); canonical native API only; no Subsonic/Jellyfin/plugin host/sonic
  similarity; bundled JS UI and backgrounds dropped, share-page HTML retained; player sessions and owner-scoped
  editing retained (`D-PLAYER26`, `D-PLAYER-EDIT`), mpv/jukebox dropped; no transcoding-profile creation/editing, a
  **read-only** `GET /api/transcoding` list retained; `EnableTranscodingConfig` stays carried-but-unused in the fresh
  TOML; Quick-Connect belongs to later canonical API work; `/` is 404. Borrow Go behavior without resurrecting
  cancelled wire surfaces.
- **C2 — Timestamp/JSON decisions explicitly reject Go-migration compatibility** (T1, J3): this is not a database
  users migrate into.
- **C3 — State axes are separate** (SOURCE-VERIFIED labels): rendered SQL / decoded records / executed reads /
  executed writes / mounted routes / real end-to-end behavior. Never infer one from another or from test totals.
  Executed test helpers are not production methods; a scaffold test asserting an issue number is not runtime coverage;
  a green crate suite is not an integration.
- **C4 — Label evidence EXECUTED / SOURCE-VERIFIED / INFERRED / UNVERIFIED** and verify reports against files,
  history, and logs. Fabricated citations have appeared in this project's briefs and reviews (`nd-db/src/time.rs`
  and `now_sql()` did not exist when first cited; `mediafile_repo::upsert_sql` does not exist; there is no
  `ON DUPLICATE KEY UPDATE` in `nd-db` or Go `persistence/`; no `todo!()`/`unimplemented!()` in `nd-db`; no
  `media_file_pkey` index; Go's `mediaFileRepository.Put` uses `putByMatch` — SELECT-then-`put`, not an upsert, so do
  not "fix" an upsert that does not exist). Do not promote an unverified claim. This failure has also hit a PARENT
  decision record: the first D-CURSOR/D-SCOPE recording cited `scanner/phase_2_artists.go`/`phase_3_albums.go` (nonexistent),
  `buildLibraryFilter` (nonexistent; the real fn is `applyLibraryFilter`), and a wrong chunk/end-of-stream claim — all caught
  by a later read-only scout and corrected at `8b5b3925` WITH the withdrawals stated in-text. Lesson: "PARENT DECIDED"
  does not exempt a record from citation verification; verify every Go-file/function/line before recording, and a scout's
  correction is not itself trusted until re-verified against source the same way (the 2026-11-20 correction was).
  A later scout that "confirms" a draft without independent source checks is as dangerous as a wrong one.
- **C5 — DB-22 executed-read boundary (PARENT DECIDED 2026-11-20):** playqueue mediafile-query failures deliberately return a typed
  all-or-error result instead of Go's log-and-continue/partial queue behavior. Scrobble ordering accepts only simple allowlisted columns;
  raw SQL expression sorting is rejected rather than interpolated. These are intentional narrow-read safety/consistency choices, not full
  Go parity. Seeded `random` ordering and optioned `GenreRepo::get_all` remain unimplemented gaps; offset-only pagination is already the
  separately accepted A1 `LIMIT -1` behavior. Never describe DB-22's narrow executed subset as full read parity.
## Plugin scope (D-PLUGIN)

- **D-PLUGIN — Delete `plugin_repo.rs` and its tests (PARENT DECIDED 2026-11-20).** The `plugin` table is part C
  item 14's dropped surface: `schema-contract.md` says the table is not ported, `nd-dbschema`'s fresh DDL carries no `plugin`
  table (SOURCE-VERIFIED: zero `plugin` occurrences in `schema_ddl.rs` beyond the drop note at :7), and no Rust caller exists
  (SOURCE-VERIFIED: the only mentions of `PluginRepo`/`plugin_repo` outside the file itself are the `pub mod` in `lib.rs:35` and a
  doc reference in `time.rs`). Every renderer in `plugin_repo.rs` therefore fails with `no such table: plugin` against the shipped
  schema, so the file is dead code, not a latent feature. Go's `cleanupPluginUserReferences`/`cleanupPluginLibraryReferences`
  (`persistence/plugin_cleanup.go:8,50,74`) update the `plugin` table from `userRepository.Delete()`/`libraryRepository.Delete()`
  to keep plugin permission arrays referentially intact; the Rust port must NOT port them — user/library delete simply has no
  plugin-cleanup step, and any `delete_user_artifacts`/`delete_library_artifacts`-style hook is dropped with the plugin scope.
  Decision: delete `rust/crates/nd-db/src/plugin_repo.rs`, its tests, and the `lib.rs` export in a follow-up lane (owner:
  whichever lane next touches `nd-db`'s repo modules; no executor is required since the code is unreachable). The Go migrations
  `20260106000620`/`20260228020813` are NOT ported (table dropped); do not add a `plugin` table to `nd-dbschema`.
  Evidence basis: read-only audit `94589a34` (2026-11-20) plus parent re-verification of `plugin_cleanup.go`, `schema_ddl.rs`,
  `lib.rs:35`, and the caller census. `next-work.md` §"Decision ask" option (a) is the decision recorded here; the option-
  table's `delete_user_artifacts`/`delete_library_artifacts` names did not exist in the file — only the renderer functions do.
## DB-21 exit scope and prepared-statement gates (PARENT DECIDED 2026-11-21)

- **DB-21 P1-DB scope narrowing (PARENT DECIDED 2026-11-21).** The launchable DB-21 exit is the **DB-side-only** P1 contract: composed regression through the real pool, the frozen API surface, and the CONSUMER_HANDOFF U-ledger pinning external acceptances as Unmet; `SC.mapping-bridge`, `SC.phase1-fixture` and `BOOT.configured-library-caller` stay U until their SC/BOOT lanes deliver caller evidence (`BOOT.configured-library-caller` gates on **BOOT-01 implementing**, with BOOT-00 a prerequisite — approval is not delivered caller evidence). Per `subplans/db-runtime.md:895` a narrowly-scoped exit may demonstrate only a strictly narrower scope, never the general one: the stable ID **DB-21 is not complete until FULL-SCANNER-DB**. Landed as `632271a9` (NEW `nd-db/tests/runtime_scanner_persistence_contract.rs`, 4206 lines, 7 executed tests).
- **Renderer-prepare gate deferred (PARENT DECIDED 2026-11-21).** The `subplans/db-runtime.md:628-632` recommendation — one executed gate preparing every renderer's statement — is deferred until the DB-04..DB-17 mutation renderers stabilize. A naive all-statements gate false-fails on five documented classes: `refresh_counters_sql` partial shape; share empty-cols rendering `near "WHERE": syntax error` (a syntax error, NOT no such column); `plugin_repo` `no such table: plugin` by D-PLUGIN; `annotation_count_sql` `no such column: annotation.id` (pinned at DB-15); clause fragments not standalone-preparable. The 5-class inventory now lives **executed** in `runtime_scanner_persistence_contract.rs` (`deferred_renderer_prepare_gate_classes_are_executed` asserts the two executable failures), so a future gate cannot re-derive it wrong.
- **Proof of delivery (parent process ruling, 2026-11-21).** After a false-completion incident on DB-21 — a child reported 2148 lines / 6 green tests / a fabricated Rust 1.95.1 toolchain while its own session log showed zero write calls and no artifact existed (caught by the fresh reviewer's tree check) — the assignment template (`assignment-template.md`) gained a mandatory **Proof of delivery** section: literal `wc -l`/`ls -la`, `git status --porcelain --untracked-files=all`, literal `cargo --version`/`rustc --version` output, and literal exits + test-total lines, copied not paraphrased. Completion claims without these outputs are treated as unverified by the parent.
