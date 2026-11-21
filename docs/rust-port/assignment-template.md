_Parent's standard bounded-task brief format, distilled from the DB-11/DB-14/DB-15 lanes._
_It authorizes nothing — no implementation, no commits, no new workspaces, no completion claims_ (same caveat style as `HANDOFF.md`).

# [TASK-ID] — [one-line goal]

## Binding context (read first, in order)
HANDOFF.md → the assigned subplan section (STATUS line first; V/proposal lines are
pre-implementation history — treat them as scope evidence, not frozen contracts)
→ the decisions.md entries named below → any consumer-side spec section named below.

## Baseline
HEAD <sha> on <branch>. Before editing run: git log -5 --oneline; git status
--porcelain --untracked-files=all; git diff --check. Report drift BEFORE editing.
Never edit docs line-numbers in a code lane; docs sync is a parent/docs lane.

## Resolved contracts (do not re-litigate — parent decisions)
- [each handshake/decision with the concrete codeable shape + file:line evidence]

## Ownership (exclusive)
- NEW files: [...]
- Modified: [...]
- Parent-authorized shared edits (named, bounded): [exact helper + its Go citation]
- Forbidden: [every other file/crate; no manifests/lockfile/Go/generated/unsafe/lint-bypasses]

## HEAD-accurate reference map (grep-verified at launch)
[subplan line numbers drift as lanes land — use these, not the subplan's]

## Binding pins (numbered; each mandatory; each testable)
1. ...

## Test matrix
Named tests + non-vacuity controls: state which mutation must fail which test; a
negative control for every 'unrestricted' trap; read back stored rows/side tables,
not SQL text.

## Gates (exact commands + expected baselines)
[fmt --check / build / clippy -D warnings / focused + package + workspace test
totals with the current baseline numbers / rustdoc N-warning baseline / diff --check]

## Report (single final message, <N words)
Transport truncates long responses (~50KB) — keep the report under N words:
verdict; exact files changed; delivered API surface; per-pin compliance; actual
command exits + test totals + tool versions; evidence labels
(EXECUTED/SOURCE-VERIFIED/INFERRED); deviations closed or retained; blockers;
exact next package.

## Proof of delivery (mandatory before any verdict — parent treats claims without it as unverified)
Paste literal outputs, copied not paraphrased: `wc -l` and `ls -la` for every new/changed
artifact; `git status --porcelain --untracked-files=all`; literal `cargo --version` and
`rustc --version` output; literal exit codes plus the literal test-total lines
(`test result: …`) for each gate command. A verdict with no artifacts on disk, no tool
versions, or paraphrased totals is treated as unverified: the parent re-checks the tree
and a fresh reviewer's tree check overrides the report.
