// Go oracle harness for the #15 archiver slice: it pins the **zip-layout
// contract** of `core/archiver.go` — the exact bytes `archive/zip` produces for
// the headers the archiver writes — plus the side contracts the archiver
// depends on: `http.DetectContentType` → file extension, `crc32.ChecksumIEEE`,
// the MS-DOS date/time encoding, and `filepath.Join` path cleaning.
//
// It is a standalone `package main` inside the Navidrome module, so it imports
// the same `archive/zip` and `net/http` packages `core/archiver.go` uses and
// writes entries the same way the archiver does
// (`core/archiver.go:102-111,300-320,332-360`): `CreateHeader` with
// `zip.FileHeader{Name, Modified, Method: zip.Store}`, one entry per track, the
// cover after the tracks, and the comment from `createZipWriter`.
//
// Run it from the repo root:
//
//	go run ./rust/crates/nd-stream/tests/testdata/go_zip \
//	  "$PWD/rust/crates/nd-stream/tests/data/go_zip"
//
// It rewrites `manifest.json` and one `.zip` per golden in the output
// directory. Commit those with any change to `core/archiver.go` or the
// `archive/zip` standard library version.
package main

import (
	"archive/zip"
	"bytes"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"hash/crc32"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"time"
)

// fixedTime is the timestamp the goldens use for entries whose Modified is
// deterministic. It is a UTC instant, so the MS-DOS fields do not depend on the
// machine's local zone; the Rust port encodes UTC (see its divergence note).
var fixedTime = time.Date(2021, 7, 14, 12, 34, 56, 0, time.UTC)

type entrySpec struct {
	name     string
	modified string // "fixed", "zero" or "now"
	data     []byte
}

type golden struct {
	name    string
	comment string
	entries []entrySpec
}

type manifestEntry struct {
	Name       string `json:"name"`
	Method     uint16 `json:"method"`
	Flags      uint16 `json:"flags"`
	CRC32      uint32 `json:"crc32"`
	CompSize   uint64 `json:"comp_size"`
	UncompSize uint64 `json:"uncomp_size"`
	ModDate    uint16 `json:"mod_date"`
	ModTime    uint16 `json:"mod_time"`
	ExtraHex   string `json:"extra_hex"`
	Offset     int64  `json:"local_offset"`
	DataHex    string `json:"data_hex"`
	Modified   string `json:"modified"`
}

type manifestGolden struct {
	Name    string          `json:"name"`
	Comment string          `json:"comment"`
	ZipFile string          `json:"zip_file"`
	FixedAt int64           `json:"fixed_unix"`
	Entries []manifestEntry `json:"entries"`
	// DeterministicHex is the archive prefix written before any entry whose
	// Modified is "now" (Go's cover art uses time.Now(), whose MS-DOS fields and
	// extended-timestamp seconds are not reproducible). Empty when the golden has
	// no "now" entry, in which case BytesHex covers the whole archive.
	DeterministicHex string `json:"deterministic_prefix_hex"`
	BytesHex         string `json:"bytes_hex"`
	Deterministic    bool   `json:"deterministic"`
}

type coverExtCase struct {
	Name        string `json:"name"`
	PayloadHex  string `json:"payload_hex"`
	ContentType string `json:"content_type"`
	Ext         string `json:"ext"`
}

type msdosCase struct {
	Unix int64  `json:"unix"`
	Date uint16 `json:"date"`
	Time uint16 `json:"time"`
}

type crcCase struct {
	InputHex string `json:"input_hex"`
	CRC      uint32 `json:"crc"`
}

type joinCase struct {
	Library string `json:"library"`
	Path    string `json:"path"`
	Joined  string `json:"joined"`
}

type manifest struct {
	FixedUnix   int64            `json:"fixed_unix"`
	Goldens     []manifestGolden `json:"goldens"`
	CoverExt    []coverExtCase   `json:"cover_ext_cases"`
	MsDos       []msdosCase      `json:"msdos_cases"`
	CRC32       []crcCase        `json:"crc32_cases"`
	Join        []joinCase       `json:"join_cases"`
	M3UDuration []m3uCase        `json:"m3u_duration_cases"`
}

type m3uCase struct {
	Duration float32 `json:"duration"`
	Line     string  `json:"line"`
}

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: go_zip <output-dir>")
		os.Exit(1)
	}
	outDir := os.Args[1]
	if err := os.MkdirAll(outDir, 0o755); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}

	m := manifest{FixedUnix: fixedTime.Unix()}
	for _, g := range goldens() {
		mg, err := buildGolden(outDir, g)
		if err != nil {
			fmt.Fprintln(os.Stderr, g.name, err)
			os.Exit(1)
		}
		m.Goldens = append(m.Goldens, mg)
	}
	m.CoverExt = coverExtCases()
	m.MsDos = msdosCases()
	m.CRC32 = crcCases()
	m.Join = joinCases()
	m.M3UDuration = m3uCases()

	data, err := json.MarshalIndent(m, "", "  ")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	if err := os.WriteFile(filepath.Join(outDir, "manifest.json"), data, 0o644); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	fmt.Printf("wrote %d goldens to %s\n", len(m.Goldens), outDir)
}

// goldens mirrors the entry shapes core/archiver.go emits.
func goldens() []golden {
	return []golden{
		{
			// zipAlbums with format=mp3/128: two tracks in one folder, cover after
			// the tracks (core/archiver.go:76-96).
			name:    "album_transcoded",
			comment: "Downloaded from Navidrome, transcoded to mp3 128bps",
			entries: []entrySpec{
				{name: "Album_Promo/01 - track1.mp3", modified: "fixed", data: []byte("test")},
				{name: "Album_Promo/02 - track2.mp3", modified: "fixed", data: []byte("test")},
				{name: "Album_Promo/folder.jpg", modified: "now", data: []byte("\xff\xd8\xff\xe0 fake jpeg")},
			},
		},
		{
			// zipAlbums with format=raw: plain comment, zero-value UpdatedAt
			// (Go's time.Time zero → no MS-DOS fields, no extended timestamp).
			name:    "album_raw_zero_time",
			comment: "Downloaded from Navidrome",
			entries: []entrySpec{
				{name: "Album/01 - track1.mp3", modified: "zero", data: []byte("AAAA")},
				{name: "Album/folder.png", modified: "zero", data: []byte("\x89PNG\r\n\x1a\n fake png")},
			},
		},
		{
			// zipArtist: each album's cover follows that album's tracks, and the
			// artist image goes at the root last (core/archiver.go:76-97).
			name:    "artist_root_and_albums",
			comment: "Downloaded from Navidrome, transcoded to mp3 128bps",
			entries: []entrySpec{
				{name: "Album 1/01 - track1.mp3", modified: "fixed", data: []byte("test")},
				{name: "Album 1/folder.jpg", modified: "now", data: []byte("jpeg-bytes")},
				{name: "Album 2/02 - track2.mp3", modified: "fixed", data: []byte("test")},
				{name: "Album 2/folder.jpg", modified: "now", data: []byte("jpeg-bytes")},
				{name: "folder.png", modified: "now", data: []byte("\x89PNG\r\n\x1a\n png")},
			},
		},
		{
			// zipMediaFiles: playlist numbering, cover at the root, M3U last with
			// the first track's UpdatedAt (core/archiver.go:176-215).
			name:    "playlist_with_m3u",
			comment: "Downloaded from Navidrome, transcoded to mp3 128bps",
			entries: []entrySpec{
				{name: "01 - AC_DC - track1.mp3", modified: "fixed", data: []byte("test")},
				{name: "02 - Artist 2 - track2.mp3", modified: "fixed", data: []byte("test")},
				{name: "folder.jpg", modified: "now", data: []byte("jpeg-bytes")},
				{
					name:     "Test Playlist.m3u",
					modified: "fixed",
					data:     []byte("#EXTM3U\n#PLAYLIST:Test Playlist\n#EXTINF:0,AC/DC - track1\n01 - AC_DC - track1.mp3\n#EXTINF:0,Artist 2 - track2\n02 - Artist 2 - track2.mp3\n"),
				},
			},
		},
		{
			// Multi-disc names, all deterministic (core/archiver.go:146-155).
			name:    "multi_disc",
			comment: "Downloaded from Navidrome, transcoded to opus 64bps",
			entries: []entrySpec{
				{name: "Album/Disc 01/01 - one.opus", modified: "fixed", data: []byte("a")},
				{name: "Album/Disc 02/02 - two.opus", modified: "fixed", data: []byte("bb")},
				{name: "Album/Disc 02/03 - three.opus", modified: "fixed", data: []byte("ccc")},
			},
		},
		{
			// UTF-8 flag: CJK requires it, "~" (0x7e) and "\" (0x5c) also do per
			// archive/zip's detectUTF8, which forbids those two code points.
			name:    "utf8_names",
			comment: "Downloaded from Navidrome",
			entries: []entrySpec{
				{name: "Album 東京/01.mp3", modified: "fixed", data: []byte("1")},
				{name: "Hits~/02.mp3", modified: "fixed", data: []byte("2")},
				{name: "Plain/03.mp3", modified: "fixed", data: []byte("3")},
				{name: "Sigur Rós_Live_ Heima/04.mp3", modified: "fixed", data: []byte("4")},
			},
		},
		{
			// Empty archive: comment-only EOCD.
			name:    "empty",
			comment: "Downloaded from Navidrome, transcoded to mp3 320bps",
			entries: nil,
		},
		{
			// Share: no M3U, entries numbered from 1 (core/archiver.go:160-174).
			name:    "share",
			comment: "Downloaded from Navidrome, transcoded to mp3 128bps",
			entries: []entrySpec{
				{name: "01 - Artist 1 - track1.mp3", modified: "zero", data: []byte("t1")},
				{name: "02 - Artist 2 - track2.mp3", modified: "zero", data: []byte("t2")},
			},
		},
	}
}

func buildGolden(outDir string, g golden) (manifestGolden, error) {
	var buf bytes.Buffer
	z := zip.NewWriter(&buf)
	if err := z.SetComment(g.comment); err != nil {
		return manifestGolden{}, err
	}
	firstNow := -1
	for i, e := range g.entries {
		if e.modified == "now" && firstNow < 0 {
			firstNow = i
		}
		mod := time.Time{}
		switch e.modified {
		case "fixed":
			mod = fixedTime
		case "now":
			mod = time.Now()
		}
		w, err := z.CreateHeader(&zip.FileHeader{
			Name:     e.name,
			Modified: mod,
			Method:   zip.Store,
		})
		if err != nil {
			return manifestGolden{}, err
		}
		if _, err := w.Write(e.data); err != nil {
			return manifestGolden{}, err
		}
	}
	if err := z.Close(); err != nil {
		return manifestGolden{}, err
	}
	data := buf.Bytes()

	// Re-read to get the fields as the archive recorded them.
	zr, err := zip.NewReader(bytes.NewReader(data), int64(len(data)))
	if err != nil {
		return manifestGolden{}, err
	}
	mg := manifestGolden{
		Name:          g.name,
		Comment:       g.comment,
		ZipFile:       g.name + ".zip",
		FixedAt:       fixedTime.Unix(),
		Deterministic: firstNow < 0,
		BytesHex:      hex.EncodeToString(data),
	}
	// Local header offsets: zip.File exposes none, so walk the local file
	// headers ourselves. archive/zip writes them in entry order.
	localOffsets, err := entryOffsets(data, zr)
	if err != nil {
		return manifestGolden{}, err
	}
	if firstNow >= 0 {
		// Prefix = everything written before the first entry whose Modified is
		// now(): that entry's local header offset.
		mg.DeterministicHex = hex.EncodeToString(data[:localOffsets[firstNow]])
	}
	for i, f := range zr.File {
		mg.Entries = append(mg.Entries, manifestEntry{
			Name:       f.Name,
			Method:     f.Method,
			Flags:      f.Flags,
			CRC32:      f.CRC32,
			CompSize:   f.CompressedSize64,
			UncompSize: f.UncompressedSize64,
			ModDate:    f.ModifiedDate,
			ModTime:    f.ModifiedTime,
			ExtraHex:   hex.EncodeToString(f.Extra),
			Offset:     localOffsets[i],
			DataHex:    hex.EncodeToString(g.entries[i].data),
			Modified:   g.entries[i].modified,
		})
	}
	if err := os.WriteFile(filepath.Join(outDir, mg.ZipFile), data, 0o644); err != nil {
		return manifestGolden{}, err
	}
	return mg, nil
}

// entryOffsets recomputes each entry's Local File Header offset from the
// header sizes archive/zip wrote: 30 + name + extra, then the stored data, then
// the 16-byte data descriptor the writer always emits for a non-directory
// entry (zip.Writer sets flag 0x8 in CreateHeader).
func entryOffsets(data []byte, zr *zip.Reader) ([]int64, error) {
	var offsets []int64
	p := 0
	for _, f := range zr.File {
		offsets = append(offsets, int64(p))
		head := 30 + len(f.Name) + len(f.Extra)
		if p+head > len(data) {
			return nil, fmt.Errorf("truncated local header for %q", f.Name)
		}
		p += head + int(f.UncompressedSize64)
		if f.Flags&0x8 != 0 {
			p += 16
		}
	}
	for i, f := range zr.File {
		if off := offsets[i]; binary.LittleEndian.Uint32(data[off:]) != 0x04034b50 {
			return nil, fmt.Errorf("offset %d for %q is not a Local File Header", off, f.Name)
		}
	}
	return offsets, nil
}

// coverExtCases replicates core/archiver.go's coverArtExtension switch.
func coverExtCases() []coverExtCase {
	payloads := []struct{ name, payload string }{
		{"jpeg", "\xff\xd8\xff\xe0 fake jpeg"},
		{"png", "\x89PNG\x0d\x0a\x1a\x0a fake png"},
		{"webp", "RIFF\x04\x00\x00\x00WEBPVP8 "},
		{"gif89a", "GIF89a\x01\x00\x01\x00"},
		{"gif87a", "GIF87a\x01\x00\x01\x00"},
		{"jpeg after whitespace", "\n\t\xff\xd8\xff\xe0 jpeg"},
		{"icon", "\x00\x00\x01\x00\x00\x00"},
		{"bmp", "BM\x00\x00\x00\x00"},
		{"riff wave", "RIFF\x04\x00\x00\x00WAVEfmt "},
		{"mp3 id3", "ID3\x03\x00\x00\x00\x00\x00\x0a"},
		{"ogg", "OggS\x00\x02\x00\x00\x00\x00\x00\x00"},
		{"webm", "\x1a\x45\xdf\xa3\x01\x00\x00\x00\x00\x00\x00\x1f"},
		{"html", "<HTML><body>jpeg\x00\x00"},
		{"zip", "PK\x03\x04\x14\x00"},
		{"pdf", "%PDF-1.7\n"},
		{"octet", "\x01\x02\x03\x04\x05\x06\x07\x08"},
		{"short gif prefix", "GIF8"},
		{"empty", ""},
		// Beyond the 512-byte sniff window: the signature must still be seen.
		{"jpeg past sniff length", "\xff\xd8\xff\xe0" + pad(600)},
	}
	out := make([]coverExtCase, 0, len(payloads))
	for _, p := range payloads {
		ct := http.DetectContentType([]byte(p.payload))
		out = append(out, coverExtCase{
			Name:        p.name,
			PayloadHex:  hex.EncodeToString([]byte(p.payload)),
			ContentType: ct,
			Ext:         ext(ct),
		})
	}
	return out
}

func pad(n int) string {
	b := make([]byte, n)
	for i := range b {
		b[i] = 'a'
	}
	return string(b)
}

// ext is core/archiver.go's coverArtExtension.
func ext(contentType string) string {
	switch contentType {
	case "image/jpeg":
		return "jpg"
	case "image/png":
		return "png"
	case "image/webp":
		return "webp"
	case "image/gif":
		return "gif"
	}
	return ""
}

// msdosCases replicates archive/zip's timeToMsDosTime over UTC times,
// including values that overflow the uint16 fields.
func msdosCases() []msdosCase {
	unixes := []int64{
		fixedTime.Unix(),
		0,
		1,
		-1,
		-1000000000,
		32503680000, // 2999-12-31T00:00:00Z
		2147483647,
		-2147483648,
		1626287700, // odd second → 2s resolution
		1626287696 + 59*60 + 59,
		1626287696 + 23*3600 + 59*60 + 58,
	}
	out := make([]msdosCase, 0, len(unixes))
	for _, u := range unixes {
		t := time.Unix(u, 0).UTC()
		date := uint16(t.Day() + int(t.Month())<<5 + (t.Year()-1980)<<9)
		tme := uint16(t.Second()/2 + t.Minute()<<5 + t.Hour()<<11)
		out = append(out, msdosCase{Unix: u, Date: date, Time: tme})
	}
	return out
}

func crcCases() []crcCase {
	inputs := []string{
		"", "test", "AAAA", "jpeg-bytes", "\xff\xd8\xff\xe0 fake jpeg",
		"#EXTM3U\n#PLAYLIST:Test Playlist\n",
		string(bytes.Repeat([]byte("x"), 4096)),
	}
	out := make([]crcCase, 0, len(inputs))
	for _, in := range inputs {
		out = append(out, crcCase{InputHex: hex.EncodeToString([]byte(in)), CRC: crc32.ChecksumIEEE([]byte(in))})
	}
	return out
}

// joinCases replicates model.MediaFile.AbsolutePath (filepath.Join).
func joinCases() []joinCase {
	pairs := [][2]string{
		{"/music", "Album/01.mp3"},
		{"", "Album/01.mp3"},
		{"/music/", "Album/01.mp3"},
		{"/music", "/Album/01.mp3"},
		{"/music", "./Album/01.mp3"},
		{"/music", "Album//01.mp3"},
		{"/music", "Album/./01.mp3"},
		{"/music", "Album/../Other/01.mp3"},
		{"/music/a/b", "../../c.mp3"},
		{"/music", ".."},
		{"/music", "../.."},
		{"music", "a/../b.mp3"},
		{"/music", "a//"},
		{"/music", ""},
		{"", ""},
		{"/", "a.mp3"},
	}
	out := make([]joinCase, 0, len(pairs))
	for _, p := range pairs {
		out = append(out, joinCase{Library: p[0], Path: p[1], Joined: filepath.Join(p[0], p[1])})
	}
	return out
}

// m3uDurationCases pins `fmt.Sprintf("#EXTINF:%.f,%s - %s\n", ...)` for
// durations that exercise Go's round-half-to-even formatting.
func m3uCases() []m3uCase {
	durations := []float32{0, 0.5, 1.5, 2.5, 3.5, 2.4, 2.6, 200.5, 201.5, 123.45, 30.9, 3599.999, 1e9, -0.4}
	out := make([]m3uCase, 0, len(durations))
	for _, d := range durations {
		out = append(out, m3uCase{
			Duration: d,
			Line:     fmt.Sprintf("#EXTINF:%.f,%s - %s\n", d, "Artist", "Title"),
		})
	}
	return out
}

// keep the unused-import guard honest for strconv in future edits.
var _ = strconv.Itoa
