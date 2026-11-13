# Go oracle harness for the participant JSON column (Slice S-A)

`rust/crates/nd-db/tests/participants_json.rs` asserts that the Rust
`marshal_participants` / `unmarshal_participants` port
(`nd-db/src/sql_participations.rs`) stores and reads back the same
`participants` column as the real Go code. This directory holds the dump
harness that produces the golden
`data/go_participants.json` (regenerable — see below).

## What it does

`main.go` is a standalone `package main` inside the Navidrome module, so it
drives the **real** `model.Participants` / `model.Role` types over
`cases.json` and dumps, per case:

- `raw` — the bytes `marshalParticipants` (`persistence/sql_participations.go:50-59`)
  would store: the harness builds the same `map[model.Role][]participant` and
  `json.Marshal`s it, so role keys go through the real `Role.MarshalText`.
- `raw_direct` — (nil case only) the real `model.Participants` nil marshalled
  directly, pinning Go's `null` spelling.
- `unmarshal_ok` / `unmarshal_error` — the result of unmarshalling `raw` back
  into `map[model.Role][]participant`, i.e. through the real
  `Role.UnmarshalText` validation.
- `roundtrip` — the re-marshalled bytes (parsed object), Go's
  round-trip fidelity.
- `flat` — the `flatParticipant` rows (`:19-24, :91-105`) the
  `updateParticipants` `json_each` INSERT consumes, in Go's map-iteration order.

## Regenerating

```sh
cd <repo root>
go run ./rust/crates/nd-db/tests/testdata/go_participants \
  rust/crates/nd-db/tests/testdata/go_participants/cases.json \
  rust/crates/nd-db/tests/testdata/go_participants
```

That rewrites `data/go_participants.json`. Commit the JSON with any change to
`persistence/sql_participations.go` (`marshalParticipants`,
`unmarshalParticipants`, `participant`/`flatParticipant`) or
`model/participants.go` (roles, `Role` (un)marshalling).

## Deliberate limitation: the wire struct is replicated

`marshalParticipants` / `unmarshalParticipants` and their `participant`
wire struct are **unexported** in `persistence`, so a standalone harness
cannot import them. The oracle therefore:

- replicates the `participant` and `flatParticipant` structs
  byte-for-byte (same fields, same `json:` tags) and reproduces
  `marshalParticipants` exactly as written;
- tests everything the real types add **against the real code**: role keys
  marshal/unmarshal via the real `model.Role` (so the oracle's
  `unmarshal_error` is the real `Role.UnmarshalText` behaviour), and the
  nil-vs-empty case goes through the real `model.Participants`.

If `participant`/`flatParticipant` ever change tags, this replication and
`data/go_participants.json` must be re-checked against
`persistence/sql_participations.go:13-24`.

## Ordering and escaping: what the golden records

- **Within-role / role-group order** in `raw` and `flat` comes from Go map
  iteration for multi-entry inputs and is **not reproducible** across
  regenerations (same rule as the `nd-mapping` oracle). The Rust test
  compares role keys in string order (both sides sort keys as strings) and
  entries per role as **multisets** — persist no ordering inherited from
  either side.
- **Escaping**: Go's `json.Marshal` escapes `<`, `>`, `&`, U+2028, U+2029
  (visible in `data/go_participants.json`, e.g. `u003c`); `serde_json`
  does not. Settled decision (next-work.md): match Go's shape and
  round-trip fidelity, not its escaped bytes. The golden is **parsed**,
  never byte-compared.
