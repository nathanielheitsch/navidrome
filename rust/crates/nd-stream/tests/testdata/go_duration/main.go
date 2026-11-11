// Go oracle harness for the #15 serving half: it pins `X-Content-Duration`,
// which both stream handlers write as
//
//	strconv.FormatFloat(float64(stream.Duration()), 'G', -1, 32)
//
// (`server/subsonic/stream.go:49`, `server/public/handle_streams.go:84`).
// `MediaFile.Duration` is a float32, so the digits are the float32's shortest
// round-trip decimal — including Go's exponential-form rule, whose predicate
// lives in the non-public `internal/strconv/ftoa.go`. The Rust port
// (`rust/crates/nd-stream/src/handler.rs::format_duration_header`) reimplements
// that rule, so this table is what it is checked against.
//
// Run it from the repo root:
//
//	go run ./rust/crates/nd-stream/tests/testdata/go_duration \
//	  "$PWD/rust/crates/nd-stream/tests/data/go_duration"
//
// It rewrites `format_float.txt` with one `input|output` line per value. The
// inputs are the float32 values printed by `%v`, plus the Inf/NaN arms, whose
// names the Rust test maps back to floats.
package main

import (
	"fmt"
	"math"
	"os"
	"path/filepath"
	"strconv"
)

// values covers the exponent-form boundary (Go switches to exponential when the
// decimal exponent is < -4 or >= 6), the subnormal extreme, and the sign arms.
// Negative zero is built with Copysign because the literal `-0` is the untyped
// constant zero, which converts to +0.
var values = []float32{
	0, float32(math.Copysign(0, -1)), 300, -300, 240.5, 1.026756, 0.3, 1, 10, 100, 1000, 10000, 100000,
	1e6, 1e7, 5e5, 6e6, 123456, 1234567, 999999.5, 3.14159,
	1e-3, 0.0001, 1e-5, 2.5e-6, 1e-21, 1e-45,
	1e12, 1e15, 1e16, 1e21, math.MaxFloat32,
	8388608, 16777216, 33554432, 0.0001220703125,
}

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: go_duration <output-dir>")
		os.Exit(1)
	}
	out := filepath.Join(os.Args[1], "format_float.txt")

	f, err := os.Create(out)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	defer f.Close()

	fmt.Fprintf(f, "# strconv.FormatFloat(float64(float32), 'G', -1, 32) — the exact\n")
	fmt.Fprintf(f, "# value server/subsonic/stream.go:49 writes into X-Content-Duration.\n")
	fmt.Fprintf(f, "# Regenerate: go run ./rust/crates/nd-stream/tests/testdata/go_duration\n")

	for _, v := range values {
		// The input column is the shortest decimal that round-trips the value, so
		// the Rust test's f32 parse recovers the exact same float32 — `%v` would
		// print `-0` for negative zero and lose the sign.
		input := strconv.FormatFloat(float64(v), 'g', -1, 32)
		fmt.Fprintf(f, "%s|%s\n", input, strconv.FormatFloat(float64(v), 'G', -1, 32))
	}
	// Go's fmt Inf/NaN arms, reached through http.Error-free duration values.
	fmt.Fprintf(f, "NaN|%s\n", strconv.FormatFloat(float64(math.NaN()), 'G', -1, 32))
	fmt.Fprintf(f, "Inf|%s\n", strconv.FormatFloat(float64(math.Inf(1)), 'G', -1, 32))
	fmt.Fprintf(f, "-Inf|%s\n", strconv.FormatFloat(float64(math.Inf(-1)), 'G', -1, 32))
}
