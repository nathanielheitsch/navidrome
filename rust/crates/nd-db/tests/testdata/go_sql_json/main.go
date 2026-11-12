// Oracle harness for the Rust port's remaining hook JSON columns (Slice
// S-D1).
//
// rust/crates/nd-db/tests/sql_json.rs asserts that the Rust codecs
// (nd-db/src/sql_json.rs) marshal/unmarshal the album `discs`
// (map[int]string), album `folder_ids` ([]string), and folder
// `image_files` ([]string, special nil -> [] PostMapArgs) columns exactly
// as the Go repositories do:
//
//   - persistence/album_repository.go:71-97 (PostMapArgs) marshals
//     `json.Marshal(a.Album.Discs)` and `json.Marshal(a.Album.FolderIDs)`;
//   - persistence/album_repository.go:41-69 (PostScan) unmarshals both
//     back into `model.Discs` (map[int]string, model/album.go:124) and
//     `[]string`;
//   - persistence/folder_repository.go:48-59 (PostMapArgs) special-cases
//     `ImageFiles == nil` to the literal "[]"; :38-46 (PostScan)
//     unmarshals into `[]string`.
//
// The shapes are plain Go types with no unexported wire structs involved
// (unlike the S-A participant oracle), so nothing is replicated here:
//
//   - the write cases drive the real `json.Marshal` over a real
//     `map[int]string` and a real `[]string`, pinning Go's integer-key
//     string-sort order (`"1", "10", "2"`), the nil -> `null` / non-nil
//     empty -> `{}` / `[]` spellings, and the escaping;
//   - the folder `image_files` write cases execute the
//     folder_repository.go:48-59 expression verbatim (the nil arm stores
//     the literal "[]" instead of marshalling), so the special case is
//     pinned empirically, not by re-stating it;
//   - the read cases drive the real `json.Unmarshal` into
//     `map[int]string` / `[]string`, pinning Go's error behaviour for
//     non-integer keys, non-string values, and the wrong JSON type.
//
// Every output here is byte-stable: the golden is deterministic across
// regenerations (unlike the go_participants golden, which carries Go
// map-iteration order). Regenerating twice must produce identical bytes.

package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
)

// discsIn is one write case: `nil: true` marks the Go nil map (whose
// json.Marshal spelling is `null`); otherwise `raw` is the Go object
// literal the case wants marshalled.
type discsIn struct {
	IsNil bool            `json:"nil"`
	Raw   json.RawMessage `json:"raw"`
}

// listIn is one []string write case, with the same nil marker.
type listIn struct {
	IsNil bool            `json:"nil"`
	Raw   json.RawMessage `json:"raw"`
}

// readCase is one read case: `json` is the column text the PostScan hook
// would receive (`nil: true` instead unmarshals the literal `null`), and
// `kind` selects the destination type — "discs" (map[int]string) or
// "list" ([]string).
type readCase struct {
	JSON  string `json:"json"`
	Kind  string `json:"kind"`
	IsNil bool   `json:"nil"`
}

// readResult records what the real json.Unmarshal did.
type readResult struct {
	OK    bool              `json:"ok"`
	Error string            `json:"error"`
	Raw   string            `json:"raw,omitempty"`  // re-marshalled result, in Go's (key-sorted) order
	Keys  []string          `json:"keys,omitempty"` // the input text's key order (stable; the string-sort contract)
	Array bool              `json:"is_array,omitempty"`
	Items []json.RawMessage `json:"items,omitempty"` // list elements, in input order
}

type row struct {
	Name       string      `json:"name"`
	Note       string      `json:"note"`
	DiscsIn    *discsIn    `json:"discs_in,omitempty"`
	DiscsOut   string      `json:"discs_out,omitempty"`
	ListIn     *listIn     `json:"string_list_in,omitempty"`
	ListOut    string      `json:"string_list_out,omitempty"`
	ImageFiles *listIn     `json:"image_files_in,omitempty"`
	ImageOut   string      `json:"image_files_out,omitempty"`
	ReadCase   *readCase   `json:"read_case,omitempty"`
	ReadResult *readResult `json:"read,omitempty"`
}

func main() {
	if len(os.Args) != 3 {
		fmt.Fprintln(os.Stderr, "usage: go_sql_json <cases.json> <output.json>")
		os.Exit(2)
	}
	casesRaw, err := os.ReadFile(os.Args[1])
	if err != nil {
		panic(err)
	}
	rows, err := run(casesRaw)
	if err != nil {
		panic(err)
	}
	out, err := json.MarshalIndent(rows, "", "  ")
	if err != nil {
		panic(err)
	}
	target := os.Args[2]
	if err := os.MkdirAll(filepath.Dir(target), 0o755); err != nil {
		panic(err)
	}
	if err := os.WriteFile(target, append(out, '\n'), 0o644); err != nil {
		panic(err)
	}
	fmt.Println("wrote", target)
}

func run(casesRaw []byte) ([]*row, error) {
	var cases []struct {
		Name       string          `json:"name"`
		Note       string          `json:"note"`
		Discs      json.RawMessage `json:"discs"`
		List       json.RawMessage `json:"string_list"`
		ImageFiles json.RawMessage `json:"image_files"`
		Read       json.RawMessage `json:"read"`
	}
	// Absent optional fields decode to an empty RawMessage, so the
	// `string(...) != "null"` presence checks below must also treat an
	// empty message as absent.
	if err := json.Unmarshal(casesRaw, &cases); err != nil {
		return nil, err
	}

	rows := make([]*row, 0, len(cases))
	for _, c := range cases {
		r := &row{Name: c.Name, Note: c.Note}

		presence := func(r json.RawMessage) bool {
			return string(r) != "null" && string(r) != ""
		}

		if presence(c.Discs) {
			var in discsIn
			if err := json.Unmarshal(c.Discs, &in); err != nil {
				return nil, fmt.Errorf("%s: discs: %w", c.Name, err)
			}
			r.DiscsIn = &in
			// persistence/album_repository.go:90-93: json.Marshal(a.Album.Discs)
			m := map[int]string(nil)
			if !in.IsNil {
				if err := json.Unmarshal(in.Raw, &m); err != nil {
					return nil, fmt.Errorf("%s: discs raw: %w", c.Name, err)
				}
			}
			b, err := json.Marshal(m)
			if err != nil {
				return nil, fmt.Errorf("%s: discs marshal: %w", c.Name, err)
			}
			r.DiscsOut = string(b)
		}

		if presence(c.List) {
			var in listIn
			if err := json.Unmarshal(c.List, &in); err != nil {
				return nil, fmt.Errorf("%s: string_list: %w", c.Name, err)
			}
			r.ListIn = &in
			// persistence/album_repository.go:86-89: json.Marshal(a.Album.FolderIDs)
			s := []string(nil)
			if !in.IsNil {
				if err := json.Unmarshal(in.Raw, &s); err != nil {
					return nil, fmt.Errorf("%s: string_list raw: %w", c.Name, err)
				}
			}
			b, err := json.Marshal(s)
			if err != nil {
				return nil, fmt.Errorf("%s: string_list marshal: %w", c.Name, err)
			}
			r.ListOut = string(b)
		}

		if presence(c.ImageFiles) {
			var in listIn
			if err := json.Unmarshal(c.ImageFiles, &in); err != nil {
				return nil, fmt.Errorf("%s: image_files: %w", c.Name, err)
			}
			r.ImageFiles = &in
			// persistence/folder_repository.go:48-58, verbatim:
			//   if f.Folder.ImageFiles == nil {
			//       args["image_files"] = "[]"
			//   } else {
			//       imgFiles, err := json.Marshal(f.Folder.ImageFiles)
			//       ...
			//       args["image_files"] = string(imgFiles)
			//   }
			s := []string(nil)
			if !in.IsNil {
				if err := json.Unmarshal(in.Raw, &s); err != nil {
					return nil, fmt.Errorf("%s: image_files raw: %w", c.Name, err)
				}
			}
			if s == nil {
				r.ImageOut = "[]"
			} else {
				b, err := json.Marshal(s)
				if err != nil {
					return nil, fmt.Errorf("%s: image_files marshal: %w", c.Name, err)
				}
				r.ImageOut = string(b)
			}
		}

		if presence(c.Read) {
			var rc readCase
			if err := json.Unmarshal(c.Read, &rc); err != nil {
				return nil, fmt.Errorf("%s: read: %w", c.Name, err)
			}
			r.ReadCase = &rc
			res := read(rc)
			r.ReadResult = &res
		}

		rows = append(rows, r)
	}
	return rows, nil
}

// read drives the real json.Unmarshal for one read case.
func read(rc readCase) readResult {
	res := readResult{}
	input := rc.JSON
	if rc.IsNil {
		input = "null"
	}
	switch rc.Kind {
	case "discs":
		// persistence/album_repository.go:43-47: json.Unmarshal into
		// map[int]string (a.Album.Discs is model.Discs = map[int]string).
		var m map[int]string
		err := json.Unmarshal([]byte(input), &m)
		res.OK = err == nil
		res.Error = errString(err)
		if err == nil {
			b, _ := json.Marshal(m)
			res.Raw = string(b)
			// The input's key order, as it appears in the text — the
			// contract for the Rust port's stringified-key sort.
			res.Keys = rawOrderKeys(input)
		}
	case "list":
		// persistence/album_repository.go:59-64 and
		// folder_repository.go:40-44: json.Unmarshal into []string.
		var s []string
		err := json.Unmarshal([]byte(input), &s)
		res.OK = err == nil
		res.Error = errString(err)
		if err == nil && s != nil {
			res.Array = true
			b, _ := json.Marshal(s)
			res.Raw = string(b)
			var arr []json.RawMessage
			_ = json.Unmarshal([]byte(input), &arr)
			res.Items = arr
		}
	default:
		panic(fmt.Sprintf("unknown read kind %q", rc.Kind))
	}
	return res
}

// rawOrderKeys returns the key order of a JSON object literal exactly as
// it appears in the text, via a streaming decode (stable, and distinct
// from Go's marshal key-sorted order).
func rawOrderKeys(s string) []string {
	var order []string
	dec := json.NewDecoder(bytes.NewReader([]byte(s)))
	tok, err := dec.Token()
	if err != nil || tok != json.Delim('{') {
		return order
	}
	for dec.More() {
		kt, err := dec.Token()
		if err != nil {
			break
		}
		key, ok := kt.(string)
		if !ok {
			break
		}
		order = append(order, key)
		var skip json.RawMessage
		if err := dec.Decode(&skip); err != nil {
			break
		}
	}
	return order
}

func errString(err error) string {
	if err == nil {
		return ""
	}
	return err.Error()
}
