# Subplan index — the one dependency map (baseline `60cdd184`, 2026-11-18)

**Planning authority, NOT implementation authorization.** This file is the parent-owned
index that reconciles the four technical subplans into a single task graph, resolves the
semantic prerequisite aliases each lane invented into actual package IDs, and records the
launch order and the open decisions. `[planned]` everywhere; nothing here has been built,
tested, or launched. Live facts and gates are in [`../current-status.md`](../current-status.md).

Lane files (verbatim planner output, parent-published):

| File | Covers | Packages | Verdict |
| --- | --- | --- | --- |
| [`db-runtime.md`](db-runtime.md) | #04/#05 executed persistence | DB-00 … DB-21 | READY for decisions/DB-01; most BLOCKED |
| [`scanner.md`](scanner.md) | #17 staged scanner | SC-00 … SC-14 | BLOCKED for code; SC-00 prep READY |
| [`boot-api.md`](boot-api.md) | #24, #33 graph, #27A | BOOT-00…04, API-00…06, API-F1…F7, BOOT-F1 | spec-first; #27A gated |
| [`evidence-ops.md`](evidence-ops.md) | parity residual + #03/#31 | EVID-01…08, OPS-01…07, FRONT-* | mostly BLOCKED/decision |

Rules that override any historical lane text: one writer per shared checkout; a child
inherits the parent's verified workspace; review never races edits; every package gets a
fresh review and parent CLI verification before it is claimed done; a green crate test is
not proof of an integration.

---

## Cross-lane prerequisite alias resolution

The `boot-api` lane invented `EXT-*` semantic aliases and the `db-runtime`/`scanner` lanes
invented `SC.*` / `BOOT.*` / `API.*` handshake names. They bind to concrete packages here.
**No implementation may launch while its prerequisite is unresolved or reported only as an
agent message — the dependency is met only by a verified, reviewed package.**

### `EXT-*` (boot-api → DB/SC)

| Alias | Concrete package(s) | Lane | Note |
| --- | --- | --- | --- |
| `EXT-DB-CONNECTION` | **DB-03** (+ **DB-00** typed seam) | DB | Decision `D-OPEN` owned jointly with **BOOT-00**. Bundled SQLite is FK=1 / busy 5000 ms; do not assume FK-off. |
| `EXT-DB-LIBRARY` | **DB-05** | DB | BOOT-01 owns configured-library policy; DB-05 owns executed typed insert/update. |
| `EXT-DB-SCOPED-LOOKUP` | **DB-11** (API-safe point read) | DB | Needs `D-SCOPE`; static internally-built membership only; no raw user SQL. Not #27A-blocking on its own. |
| `EXT-DB-EPOCH` | **DB-13** | DB | Epoch-only increment + RETURNING; `D-REVOKE`; consumed by BOOT/API, not a route. |
| `EXT-DB-QUERY` | **DB-11** (+ later collection/count via **DB-21**) | DB | Typed executed query/count for later API-F*; not a #27A prerequisite. |
| `EXT-SC-REAL-SCAN` | **SC-05** (min for #27A) / **SC-06** (complete phase 1) | SC | In-process phase-1 entry that really reads tagged media, commits, reopens, reads back. Full watcher/subprocess not required. |

### `SC.*` handshakes (db-runtime → scanner owner)

| Handshake | Provided by | Consumed by |
| --- | --- | --- |
| `SC.query-consumption` | SC-05/SC-06 | DB-11 (cancel/backpressure boundary) |
| `SC.folder-write-batch-contract` | SC-05/SC-06 | DB-14 |
| `SC.mapping-bridge` | SC-03 | DB-21 (P1-DB exit) |
| `SC.phase1-fixture` | SC-05 | DB-21 (P1-DB exit) |
| `SC.move-selection` | SC-07 | DB-16 |
| `SC.full-phase-contract` | SC-06…SC-10 | DB-21 (FULL-SCANNER-DB exit) |
| `SC.playlist-import-contract` | SC-09 (#23 `PL-SERVICE`) | DB-19/DB-21 |
| `SC.successful-phase-coverage` / `SC.purge-policy` | SC-07/SC-10 | DB-20 (destructive GC external gate) |

### `BOOT.*` / `API.*` handshakes (db-runtime → boot-api owner)

| Handshake | Provided by | Consumed by |
| --- | --- | --- |
| `BOOT.db-open-policy` | BOOT-00 | DB-03 (`D-OPEN`) |
| `BOOT.epoch-revoke` | BOOT + **DB-13** | API-03 |
| `API.revoke-authorization` | API-03 | DB-13 |
| `BOOT.configured-library-caller` | BOOT-01 (uses DB-05) | DB-21 (P1-DB), SC-14 |
| `API.visible-media-contract` | API-02 (+ DB-11) | DB-11 |

### `FRONT-*` (evidence-ops) — full service frontier, not #27A gates

`FRONT-18/19/20/21/22/23/26/27/28/32/33` are the remaining service tails. Each needs its own
dedicated plan + review when scoped; none gates #27A or the early HTTP boot. `FRONT-26` is
blocked by `D-PLAYER26`, now **decided** (retain player-session service, drop mpv/jukebox) — see that decision below.

---

## Open decisions (must be answered before the named package launches)

| ID | Decision | Blocks | Owner |
| --- | --- | --- | --- |
| `D-ERROR` | Typed operation error + transaction seam (recommend one narrow enum, SQLite cause retained, no string loss) | DB-00 and all compound writes | DB |
| `D-TIME` | Canonical scanner timestamps + idle sentinel vs fresh-DDL zero-date normalization | DB-02, DB-05/07/08/09/10 | DB |
| `D-OPEN` | Effective per-connection path/DSN/PRAGMA policy (Go 15000 ms vs bundled 5000; WAL; FK-on) | DB-03, every runtime acceptance | BOOT-00 + DB |
| `D-CURSOR` | Borrowed visitor vs bounded-owned cursor; cancel + backpressure ownership | DB-11 | DB + SC |
| `D-SCOPE` | Scoped-query authorization / library-alias construction; deny-on-empty | DB-11, API-02 | DB + API |
| `D-REVOKE` | Epoch-only revoke semantics, overflow, post-commit token observation | DB-13, API-03 | DB + BOOT/API |
| `D-STATS` | Retain Go partial-commit/nonfatal cleanup or approve stronger atomicity | DB-18 | DB |
| `D-GC` | Selective-scope GC semantics + uploaded-image post-commit ownership | DB-20 | DB + SC |
| `SC-01` | Scanner manifest/dependency/cancellation/domain-equality placement | SC-02..14 | SC |
| `SC-06/07` | Inaccessible-vs-deleted destructive-finalization policy; selective purge/GC scope; deterministic tie order | SC-06/SC-07 | SC + parent |
| `SC-12/13` | Watcher backend + external progress-IPC mode (deferred, not first-slice gates) | SC-12/13/SC-14 mode ledger | SC + parent |
| `D-PLAYER26` | **DECIDED 2026-11-18 (user-approved): split #26.** Retain the player-session service (`Get`/`Register`/`Touch` over the landed `PlayerRepo`); **drop** the mpv/jukebox device service. Canonical register/touch placement belongs to API-01. See "#26 scope analysis" below. Neither half gates #27A. | `FRONT-26`, API-F1/F5, player wiring — resolved | parent/user |
| Oracle harness | Approve new tracked Go harness under `testdata` (no Go-source edits) or an approved alternative; toolchain availability | EVID-02/03, SC-00 durable captures | parent |
| Fixture/toolchain | Durable fixture location/recipe, synthetic-only credentials, `ffmpeg`/`ffprobe` provenance, native path disclosure/redaction | EVID-03, SC-00, API-06 | parent |

---

## Single launch order (serial writer lanes; read-only planning/review may overlap)

Two independent milestones from the boot-api lane, kept separate:

* **Track A — early HTTP:** runnable boot + fixture-backed, authenticated read/stream HTTP
  can proceed **without** the scanner.
* **Track B — real pipeline (#27A exit gate):** fresh DB → real track scan → authenticated
  lookup → direct Range → forced transcode → disconnect → zero surviving children. A fixture
  insert does **not** satisfy this.

### Step 0 — decisions & spec freeze (no code, parallel read-only)

`BOOT-00`, `API-00` (native wire-contract capture), `SC-00` (oracle/manifest prep),
`SC-01` (seam/dependency ratify), `EVID-01` (snapshot + contract amendments incl. the
`DefaultGet`/gob/ProgressInfo corrections already folded into the docs), `EVID-02` (oracle
authorization gate), and the `#26` scope decision.

### Step 1 — DB persistence spine (serial, one writer)

`DB-01` (READY) → `DB-02` → `DB-03` → `DB-00` → `DB-04` → `DB-05` → `DB-06` → `DB-07` →
`DB-08` → `DB-09` → `DB-10` → `DB-11` → `DB-12` → `DB-15` → `DB-14` → `DB-21(P1-DB)`.
`DB-13` is inserted when BOOT/API needs revoke, after `DB-00/03`. `DB-16 → 17 → 18 → 19 → 20`
extend phase 2+/GC for the full-scanner exit.

### Step 2 — scanner (serialized in same checkout, gated on DB releases)

`SC-00/01` decisions → `SC-02` → `SC-03` → **`SC-05` first vertical** (needs the DB phase-1
write subset + DB-11/typed folder query) → `SC-04` → `SC-06` complete phase 1 → `SC-07` →
`SC-08` → (`PL-SERVICE` #23, then) `SC-09` → (`DB-20` released, then) `SC-10` → `SC-11` →
`SC-14` integrated in-process release. `SC-12/13` recorded early but off the critical path.

### Step 3 — boot/API (Track A early; Track B last)

`BOOT-00` + `API-00` → independent read-only/serialized `BOOT-02` (login audience) and
`API-04` (#15 stream handshake) → `BOOT-01` (after DB-05 seam) → `BOOT-03` HTTP shell →
`BOOT-04` minimal CLI/service graph → `API-01` principal/key middleware — where the retained
`D-PLAYER26` player-session service gets its canonical placement (resolve/touch the player and its
transcoding row into the request context; provisioning a new row must use a validated
`PlayerRepo::put`, not `save` unchanged, since `save` at `player_repo.rs:1253` requires a
client-supplied key and returns `ApiKeyValidation` without one) → `API-02` scoped lookup (DB-11) →
`API-03` key/revoke (DB-13; issuance/revocation use the already-landed `set_api_key` `:1305`, not
`Register`/`Touch`) → `API-05` stream route + Axum body bridge → **`API-06` = #27A gate**, which
stays BLOCKED until `SC-05`/`EXT-SC-REAL-SCAN` and real ffmpeg disconnect evidence exist.
`API-F1…F7`, `BOOT-F1` are later full-service expansion. The full player-session contract
(write-throttle/`LastSeen` rules, default transcoding, owner rules, persisted `MaxBitRate` reaching
the stream decision and scrobbler gate, and the canonical meaning of the retained `report_real_path`
column) is `FRONT-26` plus a `player-service-contract.md` **to be authored there** — it does not exist
yet. No mpv/socket/process-reap scope belongs in any lane.

### Step 4 — evidence & operations

`EVID-01 → 02 → 03 → 04/05/06/07/08`; `OPS-01 → 02 → 03` (schema golden, integrity, FTS
rebuild), `OPS-04` logging, `OPS-05 → 06` maintenance, `OPS-07` remap dedicated plan. These
do not gate #27A; the durable Go oracle captures are the recommended evidence-strengthening
step now that code has landed.

---

## First assignable packages right now

* **`DB-01`** (READY): fix `mark_missing_sql` bind order + a real function-invoking regression
  test. Smallest, highest-value, and G0-gates DB-07/14.
* **`EVID-01`** (READY): commit the current-state snapshot and the contract amendments already
  drafted (parent-owned docs) — durable record, no code.
* **`SC-00` / `API-00`** (READY, read-only/spec): oracle manifest + native wire-contract
  capture, pending the oracle-authorization decision.

Everything else waits on a recorded decision above. Implementation assignments go one writer
at a time in the existing workspace, each with its own fresh review and parent verification.

---

## #26 scope analysis (players / playback / mpv)

#26 bundles two unrelated things. Verifying against the Go source splits them cleanly:

**1. Player-session service — worth building canonically, so port it.** `core/players.go` (`Get`/
`Register`/`Touch`) plus `model/player.go` record fields. Important: this lifecycle does **not**
exist on the canonical surface today — Go registers/touches players only on the cancelled
Subsonic/Jellyfin surfaces (see below) — so this is a capability to **choose and build**, not
behavior inherited from a live path. Its fields feed services that survive the port:

- `MaxBitRate` → the transcode decision, already ported and tested in
  `nd-stream/src/legacy_client.rs` (`player_max_bit_rate` forces downsampling,
  `decider.go:159-161` precedence). The full chain is
  `PlayerRepo::get`/`find_match`/`put` → the `Players` service resolves the player and its
  transcoding row → request context (`request.PlayerFrom`) → `decider.go:159-161` →
  `nd-stream` `DecisionOptions::player_max_bit_rate` (`types.rs:265`; `legacy_client.rs:38,55-59,110-112`).
  **Nothing registers or persists the player row and no production caller sets that field**, so the
  cap is dead in practice — an integration gap, not a missing feature.
- `ScrobbleEnabled` → the scrobbler (#20) gates Now Playing and scrobbles on it
  (`core/scrobbler/play_tracker.go:366,436,473`).
- `TranscodingId` → Go's `refresh` also loads the transcoding row alongside the player
  (`core/players.go:87-91`). This is a real second dependency: `nd-db/src/transcoding_repo.rs`
  currently **only renders SQL** (no executing `get`/`find_by_format`), and `nd-dbschema/src/schema_ddl.rs:56`
  creates the `transcoding` table without seeding Go's default profiles (Go adds them in
  `db/migrations/20200706231659_add_default_transcodings.go` and `20260309203355_ensure_default_transcodings.go`).
  Both belong to #05 / BOOT-01 scope, not to this decision — but the retained player service cannot resolve a
  transcoding profile until they land.
- `ReportRealPath` is a retained DB/preferences **column**, not an inherited consumer: its only
  non-test reader is the cancelled Subsonic wire (`server/subsonic/helpers.go:227`, absolute path
  vs fake path) and its seeding key `Subsonic.DefaultReportRealPath` is itself dropped
  (`config-contract.md:220`, sole reader `core/players.go:56`). Its canonical meaning must be
  re-specified in FRONT-26/API-01 rather than assumed to keep this half load-bearing.
- Key issuance/revocation consumes the **repository** (`PlayerRepo::set_api_key` `player_repo.rs:1305`,
  `save` `:1253`) plus a player row — it does **not** need `Players::Register`/`Touch` and does not
  depend on #15. It needs the service only if provisioning a row without a client-supplied key is
  folded into it.
- The `player` table, `PlayerRepo` CRUD (`get` :1119, `find_match` :1140, `find_by_api_key` :1156,
  `put` :1215, `save` :1253, `set_api_key` :1305, `delete` :1384) and unsalted-sha256
  `hash_api_key` :378 already execute in `nd-db/src/player_repo.rs`; only the service lifecycle
  above them is missing. Go's lifecycle has **no expiry mechanism**: `refresh` stamps
  `LastSeen = time.Now()` (`players.go:78`) and throttles writes with
  `utils.Limiter{Interval: consts.UpdatePlayerFrequency}` (`players.go:26`, `consts/consts.go:89` =
  1 minute). Nothing ages or deletes player rows; the only `last_seen` filter is
  `core/metrics/insights.go:306` (7 days, #32 deferred).

There is **no mpv dependency**. Where registration happens today: Go registers/touches players only
in the Subsonic middleware (`server/subsonic/middlewares.go:291-316`, `Touch` :305 / `Register` :307,
`WithPlayer`/`WithTranscoding` :312-314), mounted on 13 of the 14 protected groups in
`server/subsonic/api.go` at 107, 113, 133, 143, 151, 159, 169, 174, 179, 184, 199, 210, 222; the outer authenticated
group also serves routes with no player resolution — the throttled `getCoverArt` group :193-197 and the `h501`/`h410`
stubs from :230), plus Jellyfin's `withPlayer` (`server/jellyfin/middlewares.go:193-224`, `Register` :209, mounted at
`server/jellyfin/api.go:110`). Both surfaces are cancelled. The native API never registers or touches: it exposed
plain rest-CRUD `rx(r, "/player", api.ds.Player(), true)` (`server/nativeapi/native_api.go:75`), and
API-key auth only attached the key's player (`server/subsonic/middlewares.go:182`). So the canonical
surface must **deliberately choose** where register/touch runs (recommend: the API-01 principal/API-key
middleware, resolving the player plus its transcoding row into the request context). That placement is
API-01 design work, not behavior inherited from a live path; without it the stream `MaxBitRate` cap and
scrobbler gate stay dead.

**2. mpv / server-side playback device — a genuinely optional niche feature, DROPPED by
`D-PLAYER26`.** `core/playback/**` (`playbackserver`, `device`, `queue`, mpv IPC) spawns a real
`mpv` process on the **server host** and drives it over a unix/named pipe — the Subsonic
`jukeboxControl` feature. Evidence for dropping:

- Its **only entry point is Subsonic**, which C20 cancels (`server/subsonic/api.go:220-226`,
  `jukebox.go`). No canonical native route exposes it and no replacement exists elsewhere.
- It is **off by default** (`jukebox.enabled` false, admin-only true,
  `conf/configuration.go:1058-1061`), and `cmd/root.go:372-380` no-ops when disabled.
- The settled config contract already drops `Jukebox.*`, `MPVPath`, `MPVCmdTemplate`
  (`config-contract.md:223`; `nd-conf` `keys.rs`/`config.rs` mark them absent).
- The bundled JS UI — the main way anyone reached it — is DROPPED (C25; the ~47k-LOC `ui/src` tree.
  C32 is backgrounds, a separate drop. The only surviving JS mention is a `[Jukebox]` config-dump
  label in `ui/src/dialogs/aboutUtils.test.js:368,379`, not a playback path).
- It spawns processes on the host and needs OS-specific socket handling plus process reap;
  `playbackserver.go:54` leaves subprocess termination as an unfinished TODO, so even upstream it is
  incomplete. It also needs an external `mpv` binary nobody else in the port requires.

No **surviving** code consumes `core/playback/**`. The complete consumer set, for the record, so a
later sweep does not read these as contradictions: the Subsonic entry point
(`server/subsonic/api.go:19,52,61`, `jukebox.go:8,47`), DI/boot plumbing
(`core/wire_providers.go:35`, `cmd/wire_injectors.go:150`, `cmd/wire_gen.go:22,112,227-230`,
`cmd/root.go:90,372-382`), and three cancelled-surface e2e suites that pass
`playback.PlaybackServer(nil)` (`server/subsonic/e2e/e2e_suite_test.go:429`,
`subsonic_artwork_test.go:260`, `subsonic_sonic_similarity_test.go:50`). `GetDeviceForUser` is only
"the default device" (`playbackserver.go:117-127`) and is called solely from `jukebox.go:47`.
`Jukebox.Enabled` has two further readers, both on non-surviving scopes:
`server/subsonic/users.go:29-30` (`jukeboxRole`, cancelled #25) and `core/metrics/insights.go:218`
(#32, deferred). So dropping it removes no capability any surviving service needs.

**Decision (`D-PLAYER26`, user-approved 2026-11-18)**: split #26 — retain the player-session
service as scoped work (register/touch/get + write-throttled persistence, whose resolved player and
transcoding row feed the stream decision and the scrobbler gate), with its canonical placement chosen
in API-01; mark the mpv/jukebox device service **DROPPED**, as justified above. Neither half gates
#27A, and the retained half is API/BOOT-adjacent work rather than a monolithic `nd-players` crate.
Reopening jukebox would require a dedicated platform/socket/process-reap contract and a canonical
(non-Subsonic) route surface; it is not scheduled.
