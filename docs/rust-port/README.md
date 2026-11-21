# Rust port — entry point

One read order, one authority hierarchy. If two docs disagree, the hierarchy below wins, and the pinned source wins over
both. Nothing in this directory authorizes implementation, commits, new workspaces, or completion claims.

## Read order

1. [`current-status.md`](current-status.md) — what is actually measured and landed, capability axes, remaining blockers.
   Start here for "where are we".
2. [`next-work.md`](next-work.md) — **current section only**: milestone order, the live wave, next dependencies, and the
   one open scope ask. Everything below its historical separator is a dated index, not a scheduler.
3. [`decisions.md`](decisions.md) — binding decisions and durable contracts (JSON codecs, ordering rules, timestamps,
   D-OPEN/D-ERROR, write-map invariants, lowercasing, scanner scope, accepted deviations, harness recipes). This is the
   stable home for citations from Rust sources and test comments.
4. [`subplans/README.md`](subplans/README.md) — the single task/dependency index: package IDs, cross-lane prerequisite
   aliases, decision gates, ownership. Then read the assigned package's subplan file.
5. [`HANDOFF.md`](HANDOFF.md) — launch/execution/review/report rules for a bounded task. Bounded-task briefs are written
   from [`assignment-template.md`](assignment-template.md) (the parent's standard section format; it carries no authorization).
6. Source contracts as needed: [`schema-contract.md`](schema-contract.md),
   [`config-contract.md`](config-contract.md), [`credentials.md`](credentials.md),
   [`entity-write-contract.md`](entity-write-contract.md), [`hydrate-contract.md`](hydrate-contract.md),
   [`hydrate-scan-contract.md`](hydrate-scan-contract.md). Scope/ownership: [`issues.md`](issues.md),
   [`component-map.md`](component-map.md).

## Authority hierarchy

| Question | Authority |
|---|---|
| What is measured/landed right now | `current-status.md` (dated measurements at the pinned baseline; latest parent integration gates at `6d6938b1` (2363/0/1 across 98 suites); earlier `7d022a02`/`632271a9`/`de0f49ea`/`96c495e7`/`e3e0770a`/`272c9c66`/`f80b9704` runs retained as history) |
| Order of work, live wave, what is next | `next-work.md` current section |
| A settled contract or decision (time, JSON, errors, pool, scope) | `decisions.md`, then the named subplan decision |
| Task IDs, prerequisites, ownership, READY/BLOCKED | `subplans/README.md` + the assigned subplan |
| Product scope and component ownership | `issues.md` (§Revision, Part C) + `component-map.md` |
| How to run a task, review it, report it | `HANDOFF.md` |
| Final truth | The pinned source at the cited commit |

Rules that keep the docs from fighting each other:

- Dated status blocks, lane tables, test totals and "remaining" labels in historical sections are records, not current
  readiness. Do not schedule from them.
- **State axes are separate** (rendered SQL / decoded records / executed reads / executed writes / mounted routes /
  real end-to-end behavior). Never infer one from another or from a test count; a green suite is not server readiness.
- Contract files keep long source-evidence sections; their **current amendments** override older scout claims.
- Never mark a package DONE from an agent message — only from a reviewed, verified package on an integrated tree with
  parent-measured gates. At `96c495e7`, DB-10, the DB-12 executed subset, DB-22, the **SC-02 partial**, **DB-11
  `52b04064`** and **DB-15's runtime half `96c495e7`** are landed; the SC-02 slice is partial, not SC-02 completion or
  MusicFS readiness, and DB-11/DB-15 stay partial on their own scopes. One writer per checkout; parent controls
  integration.
- Labels `EXECUTED` / `SOURCE-VERIFIED` / `INFERRED` / `UNVERIFIED` are mandatory on consequential claims.

## Current one-line status

Source baseline integrated `master` code HEAD `e3e0770a` (the **SC-02 partial** — local walker, `.ndignore`, folder hash
and cancellation — fast-forwarded onto `2e7026b2`, docs HEAD `795e8986`) plus **DB-11 LANDED `52b04064`** and **DB-15's
runtime half LANDED `96c495e7`**, which extend `272c9c66` (DB-22) and the `f80b9704` boundary before it
(**amended 2026-11-21:** this branch line now sits at `6d6938b1` — DB-14 `de0f49ea`, DB-21(P1-DB) `632271a9`, DB-16 `7d022a02`,
DB-17 `b3dee0a6` and BOOT-00 `6d6938b1` on top; the `96c495e7` gate quoted below is dated history)
(`74ebd81c` + docs consolidation `f03aff98`/merge `3b5e6c76` + the executed write lanes DB-04 `c791dbc1`, DB-05
`60be7534`, DB-06 `bfdcfeca`, DB-07 `2911b9ab`/merge `9be6ad0e`, DB-08 `32c4fa5b`/merge `320b8b03`, DB-09
`d938d159`/merge `842c32a2`, DB-13 `eae4ba1d`/merge `4ab63cd5`, BOOT-02 native expiring login sessions
`c6e220eb`/merge `162fae1d`, DB-10 `65072b89`/merge `6e5ca97d`, DB-12 `e5d0592d` and rustdoc fix `f80b9704`): **parent
gates at `96c495e7` (cargo/rustc 1.99.1, EXECUTED)** pass fmt/clippy/tests and workspace tests at **2302 / 0 / 1 across
94 suites** (the ignore is the pre-existing `dump_divergences`), with `nd-db` rustdoc at **22 warnings = baseline** and
zero for `sql_annotations.rs`; the prior `e3e0770a` block reported **2276 / 0 / 1 across 92 suites** and `nd-scanner`
docs at 4 pre-existing warnings. Earlier standalone DB/doctest counts are
historical, not re-runs at this HEAD; substantial executed reads **and executed transaction-bound writes** landed, **no
runnable server or scanner**. None of the executed `*_in_tx` primitives — and none of DB-11's executed scanner reads —
has a production caller (`nd-auth` calls
`UserRepo` methods outside them, mounted only in crate tests), and the partial scopes above are **not** full DB-15, not
SC-01/SC-02, not full auth/API integration, and not the DB lane. **DB-10 and the DB-12 invalidation/enqueue subset are
LANDED**, not a worker/full scanner; **DB-22 LANDED `272c9c66`** (executed genre/scrobble/playqueue/artwork reads; C5
narrow boundary). **SC-02 partial LANDED `e3e0770a`** (rebase base `2e7026b2`; S9 dependency set recorded `b1aeea2c`):
walker + ignore policy + folder hash + cancellation only — **not** SC-01 ratification, not SC-02 completion, not MusicFS
readiness, and no scanner entrypoint or tag-to-DB pipeline. **DB-11 LANDED `52b04064`** (executed typed scanner reads +
the D-CURSOR cursor contract + `ScannerScope`/`UserScope` + `get_visible_media`; not the full cursor/REST surface, no
production scanner caller) and **DB-15 runtime half LANDED `96c495e7`** (annotation transfer + average-rating recalc;
its durable Go oracle is deferred to the EVID lane, `decisions.md` H6, so DB-15 is not fully DONE). `D-CURSOR`/`D-SCOPE`
(`decisions.md`, PARENT DECIDED 2026-11-20, corrected at `8b5b3925`) are now **consumed by executed code**, and
**D-PLUGIN** decided the same day (delete `plugin_repo.rs` as out-of-scope dead code; **deletion EXECUTED at `7d022a02`**). **DB-14 LANDED
`de0f49ea`** — `SC.folder-write-batch-contract` resolved as D-BATCH and consumed (`decisions.md` "Folder-write batch contract") —
and **DB-16 LANDED `7d022a02`** (executed move/reference-remap/delete primitives, A8 share typed rejection, D-PLUGIN deletion; 18 executed
tests; gates 2343/0/1 across 97 suites), **DB-17 LANDED `b3dee0a6`** (four caller-tx count-refresh helpers — album/artist playcounts, tag
`update_counts_in_tx`, library 8-probe + 10-bind `refresh_library_totals_in_tx`; 20 executed tests in NEW `tests/runtime_count_refresh.rs`; no
production caller — SC's phase-3 triggers are the consumers) and **BOOT-00 LANDED `6d6938b1`** (D3 manifest pin repair with zero new lock entries
+ the D1 `nd-http` dependency boundary with the executed `dependency_graph_has_no_http_api_cycle`; **gate work only, no production behavior**).
Latest merged-tree gates at `6d6938b1`: **2363 / 0 / 1 across 98 suites**, `-p nd-db` **1073 / 0 / 0 across 49 suites**, `nd-http` 1/1, `nd-db`
rustdoc 21 = post-DB-16 baseline (arithmetic: `2343` at `7d022a02` `+ 20` DB-17 `+ 0` BOOT-00, suites `97 → 98`).
The spine's next package is therefore **DB-18** (forward spine `DB-18 → 19 → 20 → DB-21(FULL-SCANNER-DB)`; DB-18 is gated on `D-STATS`, still without a
canonical record), since **DB-21(P1-DB) LANDED `632271a9`** as the DB-side-only P1 exit contract (NEW `nd-db/tests/runtime_scanner_persistence_contract.rs`, 4206 lines, 7 executed tests; parent scope narrowing 2026-11-21, `decisions.md`). See `current-status.md` and
`next-work.md` for the details. Two parent decision records landed the same day (`decisions.md`): **SC-01 ratified NARROW**
(the five SC-05 seams plus the authorized `nd-scanner` `nd-db` manifest delta; progress sink, watcher, remote storage and
`MusicFS`/`ReadTags` stay unratified) and **BOOT-00 D1-D3** recorded (D4 already B3-ratified) and **EXECUTED at `6d6938b1`**, so BOOT-01 is the
next launchable boot package.
