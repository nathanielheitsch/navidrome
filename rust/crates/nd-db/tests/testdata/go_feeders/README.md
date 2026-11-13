# Go oracle harness for S-B (`nd-db` FTS write columns)

`rust/crates/nd-db/tests/search_feeders.rs` compares the Rust FTS
write-column feeders (`nd_db::sql_feeders`) against a dump produced by
**the real Go code** in this repo. This directory holds that dump harness.

## What it does

`main.go` is a standalone `package main` inside the Navidrome module, so it
imports the same `str`, `model`, and `conf` packages the persistence layer
uses. For every case in `cases.json` it:

- for `kind: "album"`: builds a real `model.Album` (name, sort name,
  album artist, participants, discs, tags) and replicates the three FTS
  lines of `dbAlbum.PostMapArgs`
  (`persistence/album_repository.go:71-96`);
- for `kind: "mediafile"`: builds a real `model.MediaFile` (with the
  subtitle tag, if any), sets `conf.Server.Subsonic.AppendSubtitle` to the
  case's value, and replicates the three FTS lines of
  `dbMediaFile.PostMapArgs` (`persistence/mediafile_repository.go:69-79`),
  calling the **real** `model.MediaFile.FullTitle` (which reads the conf at
  call time, `model/mediafile.go:109-113`).

Every value flows through the real `str.SanitizeStrings`
(`utils/str/sanitize_strings.go:14-36`), `str.NormalizeForFTS`
(`utils/str/normalize_fts.go:10-44`), and `Participants.AllNames`
(`model/participants.go:174-185`). Only the 3–9 lines of the two
unexported `PostMapArgs` methods (and the 3-line `formatFullText`) are
replicated, because `dbAlbum`/`dbMediaFile`/`formatFullText` are
unexported and this directory is a separate package; the replicated lines
are cited verbatim in `main.go`'s function docs. The participant/tag JSON
marshalling is out of scope (S-A / S-D own it).

## Regenerating

```sh
cd <repo root>
go run ./rust/crates/nd-db/tests/testdata/go_feeders \
  ./rust/crates/nd-db/tests/testdata/go_feeders/cases.json \
  ./rust/crates/nd-db/tests/testdata/go_feeders/go_feeders.json
```

That rewrites `go_feeders.json`. Commit the JSON with any change to
`persistence/{album,mediafile,sql_search}_repository.go`,
`utils/str/{sanitize_strings,normalize_fts}.go`,
`model/{mediafile,participants,tag}.go`, or `cases.json`.

## Order-sensitive fields

`Participants.AllNames` iterates `map[Role]ParticipantList`, so the byte
order of Go's `search_participants` is **not reproducible** (regenerating
the oracle can change the name order). `full_text` is unaffected —
`SanitizeStrings` sorts and dedupes the words. `search_normalized` is
deterministic: it follows the fixed field order, not map order.

Per the settled rule in `docs/rust-port/next-work.md` (persist no ordering
inherited from either side), the Rust side **deliberately diverges**:
`sql_feeders::search_participants` sorts (byte order) and re-dedupes the
names before joining. The oracle test therefore compares
`search_participants` as a sorted multiset and separately asserts the Rust
output is byte-sorted.
