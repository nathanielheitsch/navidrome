# Go oracle harness for #16 (`nd-mapping`)

`rust/crates/nd-mapping/tests/go_oracle.rs` compares the Rust
`Info → Metadata → MediaFile` mapping against a dump produced by **the real Go
code** in this repo. This directory holds that dump harness.

## What it does

`main.go` is a standalone `package main` inside the Navidrome module, so it
imports the same `model/metadata`, `model`, and `adapters/gotaglib` packages the
scanner uses. For every case it:

1. builds a `metadata.Info` (raw tags + `AudioProperties` + `HasPicture`),
2. calls `metadata.New(...)` and `md.ToMediaFile(library, folderID)`,
3. serializes the resulting `model.MediaFile`, the cleaned `Metadata` tags, the
   pair tags, and the participant credits to JSON.

Two kinds of rows are emitted:

- **real fixtures** — the audio files under `tests/fixtures/` and
  `rust/crates/nd-tagreader/tests/fixtures/real/`, read through the Go TagLib
  extractor (`adapters/gotaglib`), exactly as Go's scanner reads them.
- **synthetic cases** — `cases.json` tag tables covering Go's precedence rules:
  artist/album-artist singular-vs-plural, `ArtistSplitScripts`, `ArtistJoiner`,
  participant exceptions, performer sub-roles and MBID sync, `PID` specs,
  truncation, pair tags, compilation/`Various Artists`, and the date/year
  cascade.

## Regenerating

```sh
cd <repo root>
go run ./rust/crates/nd-mapping/tests/testdata/go_oracle \
  "$PWD" \
  "$PWD/rust/crates/nd-mapping/tests/data" \
  ./rust/crates/nd-mapping/tests/testdata/go_oracle/cases.json
```

That rewrites `rust/crates/nd-mapping/tests/data/go_oracle.json`. Commit the
JSON with any change to `model/metadata/**`, `resources/mappings.yaml`,
`resources/mime_types.yaml`, or `cases.json`.

## Order-sensitive fields

Go's `parseID3Pairs` (`model/metadata/metadata.go:276-290`) iterates a map, so
the ordering of pair-typed tags (`lyrics`, `performer`,
`musicbrainz_performerid`) — and therefore the sub-role group order in
performer credits — is **not reproducible**: regenerating the oracle four times
produced four different `performer` orderings for `tests/fixtures/test.wav`. The
Rust side uses a `BTreeMap` and is deterministic. The oracle test therefore
compares those fields as multisets and additionally by a stable sort on the pair
key, which pins the ordering Go *does* control. #17 must not persist any
ordering that depends on Go's map iteration.

## `case_probe/`

Empty by design: `go run` needs a directory, and `main.go` refuses to run
without arguments. Nothing is generated there; the harness writes only into the
output directory passed to it.
