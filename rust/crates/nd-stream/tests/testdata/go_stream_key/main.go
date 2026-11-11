// Oracle harness for the transcoding-cache key and spread layout. It prints, per
// streamJob, the exact `streamJob.Key()` string from
// `core/stream/media_streamer.go:59-61` and the `spreadFS.KeyMapper` data path from
// `utils/cache/spread_fs.go:177-186` (sha1 hex, split 2/2/40).
//
// Regenerate with:
//
//	TZ=UTC go run ./rust/crates/nd-stream/tests/testdata/go_stream_key \
//	  "$PWD/rust/crates/nd-stream/tests/data/go_stream_key"
//
// TZ=UTC pins the zone: the Rust port formats the instant in UTC and declares that
// divergence in `src/transcode_cache.rs`.
package main

import (
	"crypto/sha1"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

type job struct {
	id         string
	epochSecs  int64
	bitRate    int
	sampleRate int
	bitDepth   int
	channels   int
	format     string
	offset     int
}

var jobs = []job{
	{"mf-1", 1700000000, 128, 44100, 0, 2, "mp3", 0},
	{"mf-2", 0, 320, 48000, 24, 5, "ogg", 10},
	{"mf-3", 1, 0, 0, 0, 0, "flac", 0},
	{"mf-4", 2147483647, 64, 8000, 16, 1, "wav", 3600},
	{"mf-5", -1, 192, 44100, 0, 2, "opus", 90},
	{"mf-6", 951782400, 128, 44100, 0, 2, "mp3", 7},
	{"mf.7_x", 1782999999, 256, 96000, 32, 8, "m4a", 3},
}

func main() {
	out := os.Args[1]
	root := filepath.Join("/cache", "transcoding")
	var lines []string
	lines = append(lines, "# id epoch_secs bitRate sampleRate bitDepth channels format offset key hash")
	for _, j := range jobs {
		key := fmt.Sprintf("%s.%s.%d.%d.%d.%d.%s.%d", j.id,
			time.Unix(j.epochSecs, 0).Format(time.RFC3339Nano),
			j.bitRate, j.sampleRate, j.bitDepth, j.channels, j.format, j.offset)
		hash := fmt.Sprintf("%x", sha1.Sum([]byte(key)))
		mapped := filepath.Join(root, hash[0:2], hash[2:4], hash)
		if !strings.HasPrefix(mapped, root) {
			panic("KeyMapper layout must live under the cache root")
		}
		lines = append(lines, fmt.Sprintf("%s\t%d\t%d\t%d\t%d\t%d\t%s\t%d\t%s\t%s",
			j.id, j.epochSecs, j.bitRate, j.sampleRate, j.bitDepth, j.channels,
			j.format, j.offset, key, hash))
	}
	if err := os.WriteFile(filepath.Join(out, "keymapper.tsv"), []byte(strings.Join(lines, "\n")+"\n"), 0644); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
