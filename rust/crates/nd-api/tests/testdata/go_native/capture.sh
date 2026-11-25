#!/bin/sh
# capture.sh — API-00 in-process Go native oracle capture (test-only).
#
# ORIGIN: in-process Go native router capture via net/http/httptest. This script starts NO
# production binary, opens NO listener, makes NO network request, and never removes or
# overwrites anything inside a caller-provided directory.
#
# Usage:
#   sh rust/crates/nd-api/tests/testdata/go_native/capture.sh [OUTPUT_DIR]
#
# Lifecycle rules (API-00 review):
#   - No rm -rf of any caller path; only the script's own mktemp staging is removed.
#   - If OUTPUT_DIR is supplied it must NOT already contain run1, run2, captured, cases or
#     manifest.json (as file, directory, or symlink, dangling included); the script refuses
#     and leaves all existing content intact.
#   - All staging lives in script-owned mktemp directories (two distinct ones, so the two
#     runs use different temp roots), removed by an EXIT trap on normal and error exit; a
#     hard kill (SIGKILL) or crash cannot run that trap, so staging can survive those. No
#     pre-existing or older temp directory is cleaned up, and the caller directory receives
#     only the promoted `captured/` artifact.
#
# Exit codes: 0 reproducible, 1 harness failure, 2 usage, 3 not reproducible / verdict
# mismatch, 4 refused destination or missing precondition, 5 source-pin mismatch.
set -eu

die() {
  printf 'capture.sh: %s\n' "$1" >&2
  exit "$2"
}

OUT_ARG="${1:-}"
REFUSED=""

# An argument starting with '-' is a mistyped flag, not a directory: refusing here stops a
# common `capture.sh --out DIR` mistake from promoting an artifact into the working tree.
case "$OUT_ARG" in
  -*) die "OUTPUT_DIR must not start with '-' (pass the directory itself, not --out): $OUT_ARG" 2 ;;
esac
if [ "$#" -gt 1 ]; then
  die "usage: capture.sh [OUTPUT_DIR] (got $# arguments)" 2
fi


command -v go >/dev/null 2>&1 || die "go toolchain not found" 4
command -v git >/dev/null 2>&1 || die "git not found (needed for source-pin validation)" 4

# Resolve the repository root from this script's own path, not from $PWD.
# go_native -> testdata -> tests -> nd-api -> crates -> rust -> repo root = six levels up.
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../../../../../../" && pwd) || die "cannot resolve repo root" 4
[ -f "$REPO_ROOT/go.mod" ] || die "go.mod not found at resolved repo root" 4

CASES="$REPO_ROOT/rust/crates/nd-api/tests/testdata/go_native/requests.json"
[ -f "$CASES" ] || die "requests.json not found at $CASES" 4

# Refuse before writing anything if the caller's destination already holds capture artifacts
# or anything else we would clobber. Nothing in the caller directory is ever removed.
if [ -n "$OUT_ARG" ]; then
  for name in run1 run2 captured manifest.json cases; do
    if [ -e "$OUT_ARG/$name" ] || [ -L "$OUT_ARG/$name" ]; then
      REFUSED="$OUT_ARG/$name"
      break
    fi
  done
  if [ -n "$REFUSED" ]; then
    printf 'capture.sh: refusing to touch existing destination entry %s (nothing was modified)\n' "$REFUSED" >&2
    exit 4
  fi
  OUT_BASE="$OUT_ARG"
else
  OUT_BASE=$(mktemp -d "${TMPDIR:-/tmp}/api00-native-corpus.XXXXXX")
fi

# All staging is script-owned mktemp (removed on exit); the caller directory receives only
# the promoted artifact at the very end. Two DIFFERENT staging roots are used so the two
# runs genuinely run under different temp roots.
STAGE1=$(mktemp -d "${TMPDIR:-/tmp}/api00-native-stage1.XXXXXX")
STAGE2=$(mktemp -d "${TMPDIR:-/tmp}/api00-native-stage2.XXXXXX")
LOGS=$(mktemp -d "${TMPDIR:-/tmp}/api00-native-logs.XXXXXX")
trap 'rm -rf "$STAGE1" "$STAGE2" "$LOGS"' EXIT

RUN1="$STAGE1/run"
RUN2="$STAGE2/run"

# Offline toolchain, forced unconditionally: an inherited GOPROXY/GOTOOLCHAIN pointing at
# the network must not be honored. No module downloads, no toolchain fetch, no dependency
# mutation. GOFLAGS is set (not appended) so -mod=readonly always wins.
export GOTOOLCHAIN=local
export GOPROXY=off
export GOFLAGS=-mod=readonly

# Work from the repository root BEFORE building, so the package path and any relative
# pathspec resolve against it; the harness re-isolates its own cwd at run time.
cd "$REPO_ROOT" || die "cannot enter repo root $REPO_ROOT" 4

# Build once into script-owned staging and run the binary directly: `go run` collapses any
# non-zero program exit to 1, which would hide the harness's distinct exit codes.
BIN="$LOGS/go_native"
go build -mod=readonly -tags 'netgo sqlite_fts5' -o "$BIN" ./rust/crates/nd-api/tests/testdata/go_native \
  || die "build failed" 1

# The harness refuses an existing destination, so run1/run2 are created fresh by it.

# The harness refuses an existing destination, so run1/run2 are created fresh by it.
harness() {
  "$BIN" --cases "$CASES" --repo-root "$REPO_ROOT" --out "$1"
}

harness "$RUN1" > "$LOGS/run1.log" 2>&1; c1=$?
[ "$c1" -eq 0 ] || { cat "$LOGS/run1.log" >&2; die "first capture run failed (exit $c1)" "$c1"; }
harness "$RUN2" > "$LOGS/run2.log" 2>&1; c2=$?
[ "$c2" -eq 0 ] || { cat "$LOGS/run2.log" >&2; die "second capture run failed (exit $c2)" "$c2"; }
cat "$LOGS/run1.log" "$LOGS/run2.log"

# Reproducibility is compared on the FULL artifact bytes (manifest + cases), from two runs
# that used two different staging/temp roots.
if diff -r "$RUN1" "$RUN2" >/dev/null 2>&1; then
  printf 'REPRODUCIBLE: byte-identical FULL artifact (manifest + cases) across two runs with different temp roots\n'
else
  printf 'capture.sh: two runs differ\n' >&2
  diff -r "$RUN1" "$RUN2" >&2 || true
  die "not reproducible" 3
fi

# Compare against the checked-in corpus when it exists, so drift is caught at capture time.
CHECKED="$REPO_ROOT/rust/crates/nd-api/tests/data/native"
if [ -d "$CHECKED" ]; then
  if diff -r "$RUN2" "$CHECKED" >/dev/null 2>&1; then
    printf 'MATCHES_CHECKED_IN: %s\n' "$CHECKED"
  else
    printf 'capture.sh: fresh capture differs from the checked-in corpus\n' >&2
    diff -r "$RUN2" "$CHECKED" >&2 || true
    die "checked-in corpus drift" 3
  fi
fi

FINAL="$OUT_BASE/captured"
if [ -e "$FINAL" ] || [ -L "$FINAL" ]; then
  die "refusing to overwrite $FINAL" 4
fi
if [ ! -d "$OUT_BASE" ]; then
  mkdir -p "$OUT_BASE" || die "cannot create destination $OUT_BASE" 4
fi
cp -R "$RUN2" "$FINAL" || die "promotion failed" 1

printf 'ARTIFACT: %s\n' "$FINAL"
printf 'CASES:\n'
ls "$FINAL/cases"
printf 'FULL_SHA256 (run1 / run2):\n'
grep -h '^FULL_SHA256' "$LOGS/run1.log" "$LOGS/run2.log" || true
printf 'MANIFEST:\n'
cat "$FINAL/manifest.json"
