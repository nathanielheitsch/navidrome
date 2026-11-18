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
blocked by the `D-PLAYER26` scope decision below (recommend: retain player-session service, drop mpv/jukebox).

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
| `D-PLAYER26` | **Split #26 into a retained player-session service and a DROPPED mpv/jukebox service — recommendation, awaiting user sign-off.** See "#26 scope analysis" below. Neither half gates #27A. | `FRONT-26`, API-F5, player wiring | parent/user |
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
`BOOT-04` minimal CLI/service graph → `API-01` principal/key middleware → `API-02` scoped
lookup (DB-11) → `API-03` key/revoke (DB-13) → `API-05` stream route + Axum body bridge →
**`API-06` = #27A gate**, which stays BLOCKED until `SC-05`/`EXT-SC-REAL-SCAN` and real
ffmpeg disconnect evidence exist. `API-F1…F7`, `BOOT-F1` are later full-service expansion.

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

**1. Player-session service — genuinely used, port it.** `core/players.go` (`Get`/
`Register`/`Touch`) plus `model/player.go` record fields. Registered and touched by every
authenticated API client, not just the legacy wire, and its fields feed surviving services:

- `MaxBitRate` / `TranscodingId` → the transcode decision. Already ported and tested in
  `nd-stream/src/legacy_client.rs` (`player_max_bit_rate` forces downsampling,
  `decider.go:159-161` precedence), but **nothing registers or persists the player row**, so
  the cap is dead in practice — an integration gap, not a missing feature.
- `ScrobbleEnabled` → the scrobbler (#20) gates Now Playing and scrobbles on it.
- `ReportRealPath` → reported playback path semantics.
- The `player` table, `PlayerRepo` CRUD, `find_match`, API-key hash/`find_by_api_key` are
  already DONE in `nd-db/src/player_repo.rs`; only the service lifecycle above it is missing.

It also has **no mpv dependency**. In Go the middleware registers a player on every
authenticated request, so API-key issuance (API-03) and stream decisions need this service
whether or not jukebox exists.

**2. mpv / server-side playback device — a genuinely optional niche feature, recommend
DROPPING.** `core/playback/**` (`playbackserver`, `device`, `queue`, mpv IPC) spawns a real
`mpv` process on the **server host** and drives it over a unix/named pipe — the Subsonic
`jukeboxControl` feature. Evidence for dropping:

- Its **only entry point is Subsonic**, which C20 cancels (`server/subsonic/api.go:220-226`,
  `jukebox.go`). No canonical native route exposes it and no replacement exists elsewhere.
- It is **off by default** (`jukebox.enabled` false, admin-only true,
  `conf/configuration.go:1058-1061`), and `cmd/root.go:372-380` no-ops when disabled.
- The settled config contract already drops `Jukebox.*`, `MPVPath`, `MPVCmdTemplate`
  (`config-contract.md:223`; `nd-conf` `keys.rs`/`config.rs` mark them absent).
- The bundled JS UI — the main way anyone reached it — is DROPPED (C25/C32).
- It spawns processes on the host and needs OS-specific socket handling plus process reap;
  `playbackserver.go:18-53` leaves subprocess termination as a TODO, so even upstream it is
  incomplete. It also needs an external `mpv` binary nobody else in the port requires.

Nothing else consumes `core/playback/**`; `GetDeviceForUser` is only "the default device" and
is called solely from `jukebox.go`. So dropping it removes no capability any surviving
service needs.

**Recommendation (`D-PLAYER26`)**: split #26 — retain the player-session service as scoped
work (player register/touch + persistence, feeding stream decision, API-key issuance and
scrobbler), and mark the mpv/jukebox device service **DROPPED** with a short justification.
Record it as a user decision; neither half gates #27A, and the player-session half lands as
API/BOOT-adjacent work rather than a monolithic `nd-players` crate. If the user wants jukebox
kept, it needs a dedicated platform/socket/process-reap contract and a canonical
(non-Subsonic) route surface before it can be scoped.
