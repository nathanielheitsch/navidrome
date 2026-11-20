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
5. [`HANDOFF.md`](HANDOFF.md) — launch/execution/review/report rules for a bounded task.
6. Source contracts as needed: [`schema-contract.md`](schema-contract.md),
   [`config-contract.md`](config-contract.md), [`credentials.md`](credentials.md),
   [`entity-write-contract.md`](entity-write-contract.md), [`hydrate-contract.md`](hydrate-contract.md),
   [`hydrate-scan-contract.md`](hydrate-scan-contract.md). Scope/ownership: [`issues.md`](issues.md),
   [`component-map.md`](component-map.md).

## Authority hierarchy

| Question | Authority |
|---|---|
| What is measured/landed right now | `current-status.md` (dated measurements at the pinned baseline; latest parent integration gates at `e3e0770a`; earlier `272c9c66`/`f80b9704` runs retained as history) |
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
  parent-measured gates. At `e3e0770a`, DB-10, the DB-12 executed subset, DB-22 **and the SC-02 partial** (local walker,
  `.ndignore`, folder hash, cancellation) are merged; the SC-02 slice is partial, not SC-02 completion or MusicFS readiness.
  One writer per checkout; parent controls integration.
- Labels `EXECUTED` / `SOURCE-VERIFIED` / `INFERRED` / `UNVERIFIED` are mandatory on consequential claims.

## Current one-line status

Source baseline integrated `master` at `e3e0770a` (the **SC-02 partial** — local walker, `.ndignore`, folder hash and
cancellation — fast-forwarded onto `2e7026b2`), which extends `272c9c66` (DB-22) and the `f80b9704` boundary before it
(`74ebd81c` + docs consolidation `f03aff98`/merge `3b5e6c76` + the executed write lanes DB-04 `c791dbc1`, DB-05
`60be7534`, DB-06 `bfdcfeca`, DB-07 `2911b9ab`/merge `9be6ad0e`, DB-08 `32c4fa5b`/merge `320b8b03`, DB-09
`d938d159`/merge `842c32a2`, DB-13 `eae4ba1d`/merge `4ab63cd5`, BOOT-02 native expiring login sessions
`c6e220eb`/merge `162fae1d`, DB-10 `65072b89`/merge `6e5ca97d`, DB-12 `e5d0592d` and rustdoc fix `f80b9704`): **parent
integration gates at this code HEAD** pass fmt/clippy/tests and workspace tests at **2276 / 0 / 1 across 92 suites**;
`nd-scanner` docs report 4 pre-existing warnings (none new from that slice). Earlier standalone DB/doctest counts are
historical, not re-runs at this HEAD; substantial executed reads **and executed transaction-bound writes** landed, **no
runnable server or scanner**. None of the executed `*_in_tx` primitives has a production caller (`nd-auth` calls
`UserRepo` methods outside them, mounted only in crate tests), and the partial scopes above are **not** full DB-15, not
SC-01/SC-02, not full auth/API integration, and not the DB lane. **DB-10 and the DB-12 invalidation/enqueue subset are
LANDED**, not a worker/full scanner; **DB-22 LANDED `272c9c66`** (executed genre/scrobble/playqueue/artwork reads; C5
narrow boundary). **SC-02 partial LANDED `e3e0770a`** (rebase base `2e7026b2`; S9 dependency set recorded `b1aeea2c`):
walker + ignore policy + folder hash + cancellation only — **not** SC-01 ratification, not SC-02 completion, not MusicFS
readiness, and no scanner entrypoint or tag-to-DB pipeline. The spine resumes at **DB-11**; `D-CURSOR`/`D-SCOPE` are
**recorded** in `decisions.md` (PARENT DECIDED 2026-11-20, corrected at `8b5b3925`), and **D-PLUGIN** decided the same day
(delete `plugin_repo.rs` as out-of-scope dead code). See `current-status.md` and `next-work.md` for the details.
