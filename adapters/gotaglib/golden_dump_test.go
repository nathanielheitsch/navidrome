package gotaglib

import (
	"encoding/json"
	"fmt"
	"os"
	"testing"

	"go.senan.xyz/taglib"
)

func formatName(allTags taglib.AllTags) string {
	switch allTags.Format {
	case taglib.FormatMPEG, taglib.FormatWAV, taglib.FormatAIFF:
		return "mpeg"
	case taglib.FormatMP4:
		return "mp4"
	case taglib.FormatASF:
		return "asf"
	}
	return ""
}

func TestGoldenDump(t *testing.T) {
	if os.Getenv("ND_GOLDEN_DUMP") != "1" {
		return  // silent unless regenerating goldens; see the header comment
	}
	_ = os.Chdir("../..")
	e := extractor{fs: os.DirFS("tests/fixtures")}
	var fixtures = []string{
		"test.mp3", "test.ogg", "test.flac", "test.m4a", "test.wma",
		"test.wav", "test.aiff", "test.opus", "test.wv",
		"01 Invisible (RED) Edit Version.m4a",
		"invalid-files/test-invalid-frame.mp3",
	}
	for _, path := range fixtures {
		f, close, err := e.openFile(path)
		if err != nil {
			fmt.Printf("%s open-error: %v\n", path, err)
			continue
		}
		defer close()
		allTags := f.AllTags()
		props := f.Properties()
		duration := float64(props.Length) / 1e9
		j, _ := json.Marshal(map[string]any{
			"format": formatName(allTags),
			"tags": allTags.Tags,
			"raw": allTags.Raw,
			"image_count": len(props.Images),
			"duration": duration,
			"bitrate": int(props.Bitrate),
			"channels": int(props.Channels),
			"sample_rate": int(props.SampleRate),
			"bits_per_sample": int(props.BitsPerSample),
			"codec": props.Codec,
		})
		fmt.Printf("%s %s\n", path, string(j))
	}
}
