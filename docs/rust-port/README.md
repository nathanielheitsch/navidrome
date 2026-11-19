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
| What is measured/landed right now | `current-status.md` (dated parent measurements) |
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
  parent-measured gates. As of `74ebd81c` no implementation branch is in flight; when one is opened it is the only writer
  per checkout.
- Labels `EXECUTED` / `SOURCE-VERIFIED` / `INFERRED` / `UNVERIFIED` are mandatory on consequential claims.

## Current one-line status

Source baseline integrated `master` at `74ebd81c` (`ff900d3e` + DB-15 render-only repair `ab466928` + scanner local-FS
storage subset `e9dd8230`/`ff5b9c92` + DB-00 typed transaction seam `48e65806`/`ec84c176` + DB-02 scanner timestamp
contract `6d9d984d`/`74ebd81c`): regression gates green as dated parent measurements (workspace 2009/0/1 across 78 suites,
`nd-db` 841/0/0 across 33 suites), substantial executed reads landed, **no runnable server or scanner**; the three partial
scopes above are **not** full DB-15, not SC-01/SC-02, and not the DB lane. **No implementation lane is in flight**; the
next package is DB-04. See
`current-status.md` and `next-work.md` for the details.
