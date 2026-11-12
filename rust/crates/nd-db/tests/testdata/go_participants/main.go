// Oracle harness for the Rust port's participant JSON column (Slice S-A).
//
// `rust/crates/nd-db/tests/participants_json.rs` asserts that the Rust
// `marshal_participants`/`unmarshal_participants` port reproduces what the
// real Go code stores. This harness dumps that Go truth to
// `tests/data/go_participants.json`. See README.md in this directory for how
// to regenerate.
//
// LIMITATION (deliberate, documented in the README and the Rust test): the
// functions under test — `marshalParticipants`/`unmarshalParticipants`
// (`persistence/sql_participations.go:50-76`) and its `participant` wire
// struct (`:13-17`) — are unexported, so a standalone harness cannot import
// them. The oracle therefore REPLICATES the wire struct byte-for-byte and
// reproduces `marshalParticipants` exactly as written (builds the same
// `map[model.Role][]participant`, `json.Marshal`s it). What is NOT
// replicated is the validation the real types add, and that is tested
// against the REAL code instead:
//
//   - Role keys go through the real `model.Role`: `map[model.Role][]…`
//     marshals via `Role.MarshalText` and unmarshals via
//     `Role.UnmarshalText`, so an invalid role key errors exactly as in
//     production (`invalid role: <key>`).
//   - The `nil_participants` case marshals and unmarshals the real
//     `model.Participants` nil, pinning Go's `null` spelling and its
//     empty-non-nil round-trip.
//
// Ordering: Go's `marshalParticipants` iterates `model.Participants` (a map),
// so the within-role and role-group order it emits depends on map iteration
// for multi-entry inputs. The oracle records Go's bytes as-is (with their
// map-iteration order) and the Rust test compares role keys in string order
// and entries per role as multisets — the next-work.md rule: persist no
// ordering inherited from either side.

package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"github.com/navidrome/navidrome/model"
)

// participant is a byte-for-byte copy of the unexported wire struct at
// persistence/sql_participations.go:13-17.
type participant struct {
	ID      string `json:"id"`
	Name    string `json:"name"`
	SubRole string `json:"subRole,omitempty"`
}

// flatParticipant is a copy of persistence/sql_participations.go:19-24.
type flatParticipant struct {
	ArtistID string `json:"artist_id"`
	Role     string `json:"role"`
	SubRole  string `json:"sub_role,omitempty"`
}

// inputEntry is one case input row (cases.json `inputs` value).
type inputEntry struct {
	ID      string `json:"id"`
	Name    string `json:"name"`
	SubRole string `json:"subRole"`
}

type row struct {
	Name         string            `json:"name"`
	Note         string            `json:"note"`
	Raw          any               `json:"raw"`
	RawDirect    any               `json:"raw_direct,omitempty"`
	UnmarshalOK  bool              `json:"unmarshal_ok"`
	UnmarshalErr string            `json:"unmarshal_error"`
	DirectOK     bool              `json:"direct_ok,omitempty"`
	Roundtrip    map[string]any    `json:"roundtrip"`
	Flat         []flatParticipant `json:"flat"`
}

func main() {
	if len(os.Args) != 3 {
		fmt.Fprintln(os.Stderr, "usage: go_participants <cases.json> <output dir>")
		os.Exit(2)
	}
	casesRaw, err := os.ReadFile(os.Args[1])
	if err != nil {
		panic(err)
	}
	var cases []struct {
		Name   string          `json:"name"`
		Note   string          `json:"note"`
		Inputs json.RawMessage `json:"inputs"`
	}
	if err := json.Unmarshal(casesRaw, &cases); err != nil {
		panic(err)
	}

	rows := make([]row, 0, len(cases))
	for _, c := range cases {
		var participants model.Participants
		isNil := false
		switch {
		case c.Inputs == nil || string(c.Inputs) == "null":
			participants = model.Participants{}
		case string(c.Inputs) == `"nil"`:
			participants = nil
			isNil = true
		default:
			participants = make(model.Participants)
			var byRole map[string][]inputEntry
			if err := json.Unmarshal(c.Inputs, &byRole); err != nil {
				panic(fmt.Sprintf("%s: %v", c.Name, err))
			}
			for role, entries := range byRole {
				for _, e := range entries {
					participants.AddWithSubRole(model.RoleFromString(role), e.SubRole, model.Artist{ID: e.ID, Name: e.Name})
				}
			}
		}

		row := row{Name: c.Name, Note: c.Note}

		// marshalParticipants (sql_participations.go:50-59), reproduced
		// exactly: same map build, same json.Marshal (Role keys via
		// Role.MarshalText). Even a nil input yields `{}` here — Go's
		// `make(map…)` at :51 means production never stores `null` through
		// this function.
		dbParticipants := make(map[model.Role][]participant)
		for role, artists := range participants {
			for _, artist := range artists {
				dbParticipants[role] = append(dbParticipants[role], participant{
					ID:      artist.ID,
					Name:    artist.Name,
					SubRole: artist.SubRole,
				})
			}
		}
		raw, err := json.Marshal(dbParticipants)
		if err != nil {
			panic(err)
		}
		row.Raw = json.RawMessage(raw)

		// nil-vs-empty: the REAL type marshalled directly. `null` is the
		// spelling other Go code paths (struct embedding, dbx) can write
		// into the column; the port must accept it back.
		if isNil {
			direct, err := json.Marshal(model.Participants(nil))
			if err != nil {
				panic(err)
			}
			row.RawDirect = json.RawMessage(direct)
		}

		// unmarshalParticipants (sql_participations.go:61-76): unmarshal into
		// the real-keyed map, then re-marshal (the round-trip bytes), with
		// the real Role.UnmarshalText validation in the way.
		var dbOut map[model.Role][]participant
		unErr := json.Unmarshal(raw, &dbOut)
		row.UnmarshalOK = unErr == nil
		row.UnmarshalErr = errString(unErr)
		if unErr == nil {
			rt, err := json.Marshal(dbOut)
			if err != nil {
				panic(err)
			}
			if err := json.Unmarshal(rt, &row.Roundtrip); err != nil {
				panic(err)
			}
		}
		// And, for the nil case, unmarshal the direct `null` spelling: Go
		// yields no error and a zero-length map (note: `json.Unmarshal` of
		// `null` does not mutate its map destination — a nil stays nil —
		// so the production-meaningful assertion is no error + zero entries).
		if isNil {
			var out model.Participants
			err := json.Unmarshal(row.RawDirect.(json.RawMessage), &out)
			row.DirectOK = err == nil && len(out) == 0
		}

		// Flat form (sql_participations.go:91-105): Go iterates the map, so
		// order is map-iteration order; the Rust port emits role-sorted
		// order and the test compares as a multiset.
		flat := make([]flatParticipant, 0)
		for role, artists := range participants {
			for _, artist := range artists {
				flat = append(flat, flatParticipant{
					ArtistID: artist.ID,
					Role:     role.String(),
					SubRole:  artist.SubRole,
				})
			}
		}
		row.Flat = flat

		rows = append(rows, row)
	}

	out, err := json.MarshalIndent(rows, "", "  ")
	if err != nil {
		panic(err)
	}
	target := filepath.Join(os.Args[2], "data", "go_participants.json")
	if err := os.MkdirAll(filepath.Dir(target), 0o755); err != nil {
		panic(err)
	}
	if err := os.WriteFile(target, append(out, '\n'), 0o644); err != nil {
		panic(err)
	}
	fmt.Println("wrote", target)
}

func errString(err error) string {
	if err == nil {
		return ""
	}
	return err.Error()
}
