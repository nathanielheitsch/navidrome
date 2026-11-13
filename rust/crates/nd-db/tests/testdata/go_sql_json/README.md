# Go oracle harness for the remaining hook JSON columns (Slice S-D1)

`rust/crates/nd-db/tests/sql_json.rs` asserts that the Rust codecs
(`nd-db/src/sql_json.rs`) store and read back the same `discs`,
`folder_ids`, and `image_files` JSON columns as the real Go
repositories. This directory holds the dump harness that produces the
golden `data/go_sql_json.json` (regenerable — see below).

## What it does

`main.go` is a standalone `package main` inside the Navidrome module, so
it drives the **real** `encoding/json` over the **real Go types**
(`map[int]string`, as `model.Discs` is, `model/album.go:124`; `[]string`)
for each case in `cases.json`:

- **write / discs** — `json.Marshal` of a `map[int]string`, pinning Go's
  integer-key **string-sort order** (`"1"`, `"10"`, `"100"`, `"2"` — not
  numeric order), the `nil → null` / non-nil empty → `{}` spellings, and
  the `<`/`>`/`&`/U+2028/U+2029 escaping. This is what
  `album_repository.go:90-93` (`PostMapArgs`) binds as the `discs`
  column.
- **write / string_list** — `json.Marshal` of a `[]string`
  (`folder_ids`, `album_repository.go:86-89`): `nil → null`, non-nil
  empty → `[]`, element order preserved.
- **write / image_files** — the `folder_repository.go:48-58`
  `PostMapArgs` expression **executed verbatim**: `ImageFiles == nil`
  stores the **literal `"[]"`** (never `json.Marshal(nil)`, which would
  be `null`); non-nil goes through `json.Marshal`.
- **read** — `json.Unmarshal` of a column string into
  `map[int]string` (`kind: "discs"`, `album_repository.go:43-47`) or
  `[]string` (`kind: "list"`, `album_repository.go:59-64` /
  `folder_repository.go:40-44`), recording `ok`, the Go error text, the
  re-marshalled result, and — for discs — the input's key order (stable,
  from a streaming decode).

## Regenerating

The harness takes the **output FILE path** as its second argument (not a
directory — unlike `go_participants`, which wants a directory, and unlike
`go_feeders`, which also wants a file):

```sh
cd <repo root>
go run ./rust/crates/nd-db/tests/testdata/go_sql_json \
  rust/crates/nd-db/tests/testdata/go_sql_json/cases.json \
  rust/crates/nd-db/tests/testdata/go_sql_json/data/go_sql_json.json
```

That rewrites the golden and prints `wrote <path>`. **Check the exit
status is 0 and the `wrote` line before trusting any diff** — a failed
run writes nothing, and a diff against the old golden then matches
trivially and looks green. Commit the JSON with any change to
`persistence/album_repository.go` / `persistence/folder_repository.go`
(the discs/folder_ids/image_files arms of `PostScan`/`PostMapArgs`).

## Stability

Unlike the `go_participants` golden (whose entry order carries Go
map-iteration order), this golden is **byte-stable**: the harness never
iterates a Go map for output order — key order is recorded by a
streaming decode that preserves the input order `cases.json` declared.
Regenerating twice must produce identical bytes (verified with `cmp`).
The Rust test may therefore assert object key order positionally where
Go's string-key sorting is the contract.

## What the golden does NOT pin

- **Escaped bytes.** Go's `json.Marshal` escapes `<`, `>`, `&`, U+2028
  and U+2029 (visible in the golden); `serde_json` does not. Settled
  decision (next-work.md §1b): match Go's *shape and round-trip
  fidelity*, not its escaped bytes. The Rust test compares those cases
  parsed (structure + key order), never byte-for-byte.
- **Structural-error wording.** The golden records Go's
  `json.Unmarshal` error *text* for the malformed cases; the Rust
  `JsonColumnError` carries the raw `serde_json` source instead.
  Callers (S-D3) may only depend on the `column:` context plus the
  Go-parity prefix the entity hook adds (`parsing album discs from db:`
  etc.), never on the encoder's wording.
- **Nil-vs-empty on the write side of `image_files`** is collapsed by
  Go itself (`nil → []`); only the read side can distinguish `null`
  (nil slice) from `[]` (empty non-nil slice), and both spellings are
  pinned in the `read` cases.

## No replication

Unlike the S-A participant oracle, nothing is replicated here: the
column shapes are plain Go types (`map[int]string`, `[]string`) with no
unexported wire structs, so the harness calls the real `encoding/json`
on real values. The only Go-side code reproduced is the
`folder_repository.go:48-58` nil-branch expression, quoted verbatim in
the harness.
