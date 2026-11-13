// Oracle harness for the Rust port's #16 mapping layer. This is NOT part of the
// port and NOT part of Navidrome's runtime: it drives the real Go
// model/metadata pipeline and writes rust/crates/nd-mapping/tests/data/go_oracle.json,
// which rust/crates/nd-mapping/tests/go_oracle.rs asserts against. See README.md
// in this directory for how to regenerate.
//
// It lives inside the module on purpose: importing the real packages is the
// whole point, so the goldens cannot drift from the code the scanner runs.
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	_ "github.com/navidrome/navidrome/adapters/gotaglib"
	"github.com/navidrome/navidrome/conf"
	_ "github.com/navidrome/navidrome/conf/mime"
	"github.com/navidrome/navidrome/core/storage"
	"github.com/navidrome/navidrome/model"
	"github.com/navidrome/navidrome/model/metadata"
)

type pair struct {
	Key   string `json:"key"`
	Value string `json:"value"`
}

type participant struct {
	ID              string `json:"id"`
	Name            string `json:"name"`
	SortArtistName  string `json:"sort_artist_name"`
	OrderArtistName string `json:"order_artist_name"`
	MbzArtistID     string `json:"mbz_artist_id"`
	SubRole         string `json:"sub_role"`
}

type row struct {
	Source       string                   `json:"source"`
	FilePath     string                   `json:"file_path"`
	InfoTags     map[string][]string      `json:"info_tags"`
	AudioProps   map[string]any           `json:"audio_properties"`
	HasPicture   bool                     `json:"has_picture"`
	CleanedTags  map[string][]string      `json:"cleaned_tags"`
	Pairs        map[string][]pair        `json:"pairs"`
	MediaFile    map[string]any           `json:"media_file"`
	Mapped       map[string]any           `json:"mapped"`
	Participants map[string][]participant `json:"participants"`
	TagIDs       []string                 `json:"tag_ids"`
	Size         int64                    `json:"size"`
	Suffix       string                   `json:"suffix"`
	PidAlbum     string                   `json:"pid_album"`
	PidTrack     string                   `json:"pid_track"`
	LibraryID    int                      `json:"library_id"`
}

// idsOf normalizes Go's nil slice to an empty one, so the JSON is always an
// array (Go marshals a nil []string as `null`).
func idsOf(t model.Tags) []string {
	ids := t.IDs()
	if ids == nil {
		return []string{}
	}
	return ids
}

func toStrMap(t model.Tags) map[string][]string {
	out := map[string][]string{}
	for k, v := range t {
		out[string(k)] = v
	}
	return out
}

func mapMF(mf model.MediaFile) map[string]any {
	b, err := json.Marshal(mf)
	if err != nil {
		panic(err)
	}
	var m map[string]any
	if err := json.Unmarshal(b, &m); err != nil {
		panic(err)
	}
	// unstable / filesystem-dependent: excluded from the oracle
	delete(m, "birthTime")
	delete(m, "createdAt")
	delete(m, "updatedAt")
	delete(m, "mediaFileId")
	return m
}

// fullMF lists every field the #16 mapping writes, explicitly, so Rust can
// compare without guessing about Go's `omitempty` behaviour.
func fullMF(mf model.MediaFile, md metadata.Metadata) map[string]any {
	out := map[string]any{
		"pid":                  mf.PID,
		"albumId":              mf.AlbumID,
		"title":                mf.Title,
		"album":                mf.Album,
		"sortTitle":            mf.SortTitle,
		"sortAlbumName":        mf.SortAlbumName,
		"orderTitle":           mf.OrderTitle,
		"orderAlbumName":       mf.OrderAlbumName,
		"compilation":          mf.Compilation,
		"trackNumber":          mf.TrackNumber,
		"discNumber":           mf.DiscNumber,
		"discSubtitle":         mf.DiscSubtitle,
		"catalogNum":           mf.CatalogNum,
		"comment":              mf.Comment,
		"lyrics":               mf.Lyrics,
		"explicitStatus":       mf.ExplicitStatus,
		"year":                 mf.Year,
		"date":                 string(mf.Date),
		"originalYear":         mf.OriginalYear,
		"originalDate":         string(mf.OriginalDate),
		"releaseYear":          mf.ReleaseYear,
		"releaseDate":          string(mf.ReleaseDate),
		"mbzRecordingId":       mf.MbzRecordingID,
		"mbzReleaseTrackId":    mf.MbzReleaseTrackID,
		"mbzAlbumId":           mf.MbzAlbumID,
		"mbzReleaseGroupId":    mf.MbzReleaseGroupID,
		"mbzAlbumType":         mf.MbzAlbumType,
		"hasCoverArt":          mf.HasCoverArt,
		"duration":             mf.Duration,
		"bitRate":              mf.BitRate,
		"sampleRate":           mf.SampleRate,
		"channels":             mf.Channels,
		"codec":                mf.Codec,
		"path":                 mf.Path,
		"suffix":               mf.Suffix,
		"size":                 mf.Size,
		"artist":               mf.Artist,
		"albumArtist":          mf.AlbumArtist,
		"artistId":             mf.ArtistID,
		"albumArtistId":        mf.AlbumArtistID,
		"orderArtistName":      mf.OrderArtistName,
		"orderAlbumArtistName": mf.OrderAlbumArtistName,
		"sortArtistName":       mf.SortArtistName,
		"sortAlbumArtistName":  mf.SortAlbumArtistName,
		"tags":                 toStrMap(mf.Tags),
		"movements":            movementsOf(mf),
		"mdSuffix":             md.Suffix(),
	}
	if mf.BPM != nil {
		out["bpm"] = *mf.BPM
	} else {
		out["bpm"] = nil
	}
	if mf.BitDepth != nil {
		out["bitDepth"] = *mf.BitDepth
	} else {
		out["bitDepth"] = nil
	}
	if mf.RGAlbumPeak != nil {
		out["rgAlbumPeak"] = *mf.RGAlbumPeak
	} else {
		out["rgAlbumPeak"] = nil
	}
	if mf.RGAlbumGain != nil {
		out["rgAlbumGain"] = *mf.RGAlbumGain
	} else {
		out["rgAlbumGain"] = nil
	}
	if mf.RGTrackPeak != nil {
		out["rgTrackPeak"] = *mf.RGTrackPeak
	} else {
		out["rgTrackPeak"] = nil
	}
	if mf.RGTrackGain != nil {
		out["rgTrackGain"] = *mf.RGTrackGain
	} else {
		out["rgTrackGain"] = nil
	}
	return out
}

func movementsOf(mf model.MediaFile) []map[string]any {
	out := []map[string]any{}
	for _, m := range mf.Movements() {
		out = append(out, map[string]any{"name": m.Name, "number": m.Number, "count": m.Count})
	}
	return out
}

func participantsOf(mf model.MediaFile) map[string][]participant {
	out := map[string][]participant{}
	for role, list := range mf.Participants {
		l := make([]participant, 0, len(list))
		for _, p := range list {
			l = append(l, participant{ID: p.ID, Name: p.Name, SortArtistName: p.SortArtistName, OrderArtistName: p.OrderArtistName, MbzArtistID: p.MbzArtistID, SubRole: p.SubRole})
		}
		out[role.String()] = l
	}
	return out
}

func pairsOf(md metadata.Metadata) map[string][]pair {
	out := map[string][]pair{}
	for _, name := range []model.TagName{"lyrics", "performer", "musicbrainz_performerid"} {
		ps := md.Pairs(name)
		if len(ps) == 0 {
			continue
		}
		l := make([]pair, 0, len(ps))
		for _, p := range ps {
			l = append(l, pair{Key: p.Key(), Value: p.Value()})
		}
		out[string(name)] = l
	}
	return out
}

func buildRow(source, filePath string, info metadata.Info, size int64) row {
	md := metadata.New(filePath, info)
	mf := md.ToMediaFile(model.Library{ID: 1}, "FOLDERID")
	return row{
		Source:       source,
		FilePath:     filePath,
		InfoTags:     info.Tags,
		AudioProps:   map[string]any{"duration_seconds": info.AudioProperties.Duration.Seconds(), "bit_rate": info.AudioProperties.BitRate, "bit_depth": info.AudioProperties.BitDepth, "sample_rate": info.AudioProperties.SampleRate, "channels": info.AudioProperties.Channels, "codec": info.AudioProperties.Codec},
		HasPicture:   info.HasPicture,
		CleanedTags:  toStrMap(md.All()),
		Pairs:        pairsOf(md),
		MediaFile:    mapMF(mf),
		Mapped:       fullMF(mf, md),
		Participants: participantsOf(mf),
		TagIDs:       idsOf(mf.Tags),
		Size:         size,
		Suffix:       md.Suffix(),
		PidAlbum:     "",
		PidTrack:     "",
		LibraryID:    1,
	}
}

type syntheticCase struct {
	Name      string              `json:"name"`
	Repeat    map[string]int      `json:"repeat"`
	FilePath  string              `json:"file_path"`
	Tags      map[string][]string `json:"tags"`
	Props     map[string]any      `json:"props"`
	PIDAlbum  string              `json:"pid_album"`
	PIDTrack  string              `json:"pid_track"`
	LibraryID int                 `json:"library_id"`
}

func main() {
	root := os.Args[1]
	outDir := os.Args[2]
	if err := os.Chdir(root); err != nil {
		panic(err)
	}
	conf.LoadFromFile("tests/navidrome-test.toml")

	rows := []row{}

	// ---- real media, read through the Go gotaglib extractor ----
	type target struct{ base, name string }
	targets := []target{
		{"rust/crates/nd-tagreader/tests/fixtures/real", "known.mp3"},
		{"rust/crates/nd-tagreader/tests/fixtures/real", "known.flac"},
		{"rust/crates/nd-tagreader/tests/fixtures/real", "known.m4a"},
		{"rust/crates/nd-tagreader/tests/fixtures/real", "known.ogg"},
		{"tests/fixtures", "test.mp3"},
		{"tests/fixtures", "test.flac"},
		{"tests/fixtures", "test.m4a"},
		{"tests/fixtures", "test.ogg"},
		{"tests/fixtures", "test.opus"},
		{"tests/fixtures", "test.wav"},
		{"tests/fixtures", "test.aiff"},
		{"tests/fixtures", "test.wv"},
		{"tests/fixtures", "test.wma"},
		{"tests/fixtures", "01 Invisible (RED) Edit Version.m4a"},
	}
	for _, t := range targets {
		st, err := storage.For("file://" + filepath.Join(root, t.base))
		if err != nil {
			panic(err)
		}
		fsys, err := st.FS()
		if err != nil {
			panic(err)
		}
		infos, err := fsys.ReadTags(t.name)
		if err != nil {
			fmt.Printf("SKIP %s: %v\n", t.name, err)
			continue
		}
		i, ok := infos[t.name]
		if !ok {
			fmt.Printf("SKIP %s (extractor returned no info)\n", t.name)
			continue
		}
		size, err := fs_Stat(fsys, t.name)
		if err != nil {
			panic(err)
		}
		rows = append(rows, buildRow(t.base+"/"+t.name, t.name, i, size))
	}

	// ---- synthetic RawTags cases (from the Go test tables) ----
	raw, err := os.ReadFile(os.Args[3])
	if err != nil {
		panic(err)
	}
	var cases []syntheticCase
	if err := json.Unmarshal(raw, &cases); err != nil {
		panic(err)
	}
	stub := stubFileInfo{}
	for _, c := range cases {
		props := metadata.AudioProperties{}
		if v, ok := c.Props["duration_seconds"]; ok {
			props.Duration = seconds(v)
		}
		if v, ok := c.Props["bit_rate"]; ok {
			props.BitRate = asInt(v)
		}
		if v, ok := c.Props["bit_depth"]; ok {
			props.BitDepth = asInt(v)
		}
		if v, ok := c.Props["sample_rate"]; ok {
			props.SampleRate = asInt(v)
		}
		if v, ok := c.Props["channels"]; ok {
			props.Channels = asInt(v)
		}
		if v, ok := c.Props["codec"]; ok {
			props.Codec = v.(string)
		}
		raw := model.RawTags{}
		for k, v := range c.Tags {
			n := c.Repeat[k]
			if n > 1 {
				base := v
				rep := make([]string, 0, n*len(base))
				for i := 0; i < n; i++ {
					rep = append(rep, base...)
				}
				v = rep
			}
			raw[k] = v
		}
		info := metadata.Info{Tags: raw, AudioProperties: props, HasPicture: c.Props["has_picture"] == true, FileInfo: stub}
		lib := model.Library{ID: c.LibraryID, PIDAlbum: c.PIDAlbum, PIDTrack: c.PIDTrack}
		md := metadata.New(c.FilePath, info)
		mf := md.ToMediaFile(lib, "FOLDERID")
		rows = append(rows, row{
			Source:       "synthetic:" + c.Name,
			FilePath:     c.FilePath,
			InfoTags:     info.Tags,
			AudioProps:   map[string]any{"duration_seconds": props.Duration.Seconds(), "bit_rate": props.BitRate, "bit_depth": props.BitDepth, "sample_rate": props.SampleRate, "channels": props.Channels, "codec": props.Codec},
			HasPicture:   info.HasPicture,
			CleanedTags:  toStrMap(md.All()),
			Pairs:        pairsOf(md),
			MediaFile:    mapMF(mf),
			Mapped:       fullMF(mf, md),
			Participants: participantsOf(mf),
			TagIDs:       idsOf(mf.Tags),
			Size:         0,
			Suffix:       md.Suffix(),
			PidAlbum:     c.PIDAlbum,
			PidTrack:     c.PIDTrack,
			LibraryID:    c.LibraryID,
		})
	}

	if err := os.MkdirAll(outDir, 0o755); err != nil {
		panic(err)
	}
	b, err := json.MarshalIndent(rows, "", "  ")
	if err != nil {
		panic(err)
	}
	if err := os.WriteFile(filepath.Join(outDir, "go_oracle.json"), b, 0o644); err != nil {
		panic(err)
	}
	fmt.Printf("wrote %d rows\n", len(rows))
}
