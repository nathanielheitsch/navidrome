#!/bin/sh
# Capture authenticated Go-native song wire cases twice, offline, in one private root.
set -eu
umask 077

if [ "$#" -ne 1 ]; then
  printf 'usage: capture.sh NEW_OUTPUT_DIR\n' >&2
  exit 2
fi
case "$1" in
  -*) printf 'output path must not begin with -\n' >&2; exit 2 ;;
esac
# Resolve before any cd so relative destinations remain relative to the caller.
case "$1" in
  /*) OUT=$1 ;;
  *) OUT=$(pwd -P)/$1 ;;
esac

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../../../../../../" && pwd -P)
[ -f "$ROOT/go.mod" ] || { printf 'could not resolve repository root\n' >&2; exit 4; }

# Resolve TMPDIR to an absolute path before changing directories.
TMP_ROOT=${TMPDIR:-/tmp}
case "$TMP_ROOT" in
  /*) ;;
  *) TMP_ROOT=$(CDPATH= cd -- "$TMP_ROOT" && pwd -P) ;;
esac
STAGE=$(mktemp -d "$TMP_ROOT/go-native-song.XXXXXX")
STAGE_OWNED=1
OUT_RESERVED=0
SUCCESS=0
cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  if [ "$OUT_RESERVED" -eq 1 ] && [ "$SUCCESS" -ne 1 ]; then
    if ! rm -rf "$OUT"; then
      printf 'failed to remove owned output reservation\n' >&2
      status=1
    fi
  fi
  if [ "$STAGE_OWNED" -eq 1 ]; then
    if ! rm -rf "$STAGE"; then
      printf 'failed to clean private staging root\n' >&2
      status=1
    fi
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
chmod 700 "$STAGE"
mkdir "$STAGE/tmp" "$STAGE/gocache" "$STAGE/run1" "$STAGE/run2"

# Atomic reservation refuses files, directories, symlinks, and races; never merge.
if ! mkdir "$OUT" 2>/dev/null; then
  printf 'refusing existing or unavailable output path; nothing changed\n' >&2
  exit 4
fi
OUT_RESERVED=1

export TMPDIR="$STAGE/tmp"
export GOCACHE="$STAGE/gocache"
export GOTOOLCHAIN=local
export GOENV=off
export GOWORK=off
export GOPROXY=off
export GOSUMDB=off
export GOPRIVATE=
export GONOPROXY=
export GONOSUMDB=
export GOINSECURE=
export GOVCS='*:off'
export GOFLAGS=-mod=readonly

# Inspect only local Go configuration; never issue a network probe.
[ "$GOENV" = off ]
case "$(go env GOENV)" in ''|off) ;; *) exit 1 ;; esac
[ "$GOWORK" = off ]
case "$(go env GOWORK)" in ''|off) ;; *) exit 1 ;; esac
[ "$(go env GOTOOLCHAIN)" = local ]
[ "$(go env GOPROXY)" = off ]
[ "$(go env GOSUMDB)" = off ]
[ "$(go env GOPRIVATE)" = "" ]
[ "$(go env GONOPROXY)" = "" ]
[ "$(go env GONOSUMDB)" = "" ]
[ "$(go env GOINSECURE)" = "" ]
[ "$(go env GOVCS)" = '*:off' ]
[ "$(go env GOFLAGS)" = -mod=readonly ]

cd "$ROOT"
HARNESS=./rust/crates/nd-api/tests/testdata/go_native_song
GO_DEPS="$STAGE/local-deps.json"
go list -deps -json -tags 'netgo sqlite_fts5' "$HARNESS" > "$GO_DEPS"
go build -mod=readonly -tags 'netgo sqlite_fts5' -o "$STAGE/go_native_song" "$HARNESS"
"$STAGE/go_native_song" --repo-root "$ROOT" --compiled-inputs "$GO_DEPS" --out "$STAGE/run1/captured"
"$STAGE/go_native_song" --repo-root "$ROOT" --compiled-inputs "$GO_DEPS" --out "$STAGE/run2/captured"
if ! diff -r "$STAGE/run1/captured" "$STAGE/run2/captured" >/dev/null; then
  printf 'two fresh captures differ; refusing promotion\n' >&2
  exit 3
fi
printf 'REPRODUCIBLE: two fresh scanner/auth captures are byte-identical\n'
cp -R "$STAGE/run2/captured/." "$OUT/"
SUCCESS=1
printf 'ARTIFACT: new output populated\n'
find "$OUT/cases" -maxdepth 1 -type f -print | sed 's|.*/||' | sort
