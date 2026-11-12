// Oracle harness for the Rust port's nd-db FTS write columns (slice S-B).
// This is NOT part of the port and NOT part of Navidrome's runtime: it
// drives the real Go `str` (SanitizeStrings, NormalizeForFTS) and `model`
// (MediaFile.FullTitle, Participants.AllNames, Tags.First) packages and
// replicates the three FTS lines of `dbAlbum.PostMapArgs`
// (persistence/album_repository.go:71-96) and `dbMediaFile.PostMapArgs`
// (persistence/mediafile_repository.go:69-79), then writes
// go_feeders.json, which rust/crates/nd-db/tests/search_feeders.rs
// asserts against. See README.md in this directory for regeneration.
//
// Why the two `PostMapArgs` are replicated rather than called:
// `dbAlbum`/`dbMediaFile` and `formatFullText` are unexported and only
// reachable from inside `package persistence` (this directory is a
// separate package); everything below them — the input lists,
// `str.SanitizeStrings`, `str.NormalizeForFTS`, `model.MediaFile.FullTitle`
// (with the real `conf.Server.Subsonic.AppendSubtitle`) and
// `Participants.AllNames` — is driven by the real code. The replicated
// lines are cited verbatim in the function docs below; the tags/
// participants JSON marshalling is out of scope (S-A / S-D own it).
package main

import (
	"encoding/json"
	"fmt"
	"maps"
	"os"
	"slices"
	"strconv"
	"strings"

	"github.com/navidrome/navidrome/conf"
	"github.com/navidrome/navidrome/model"
	"github.com/navidrome/navidrome/utils/str"
)

// Verbatim copy of `formatFullText` (persistence/sql_search.go:15-18);
// unexported, so it cannot be imported.
func formatFullText(text ...string) string {
	fullText := str.SanitizeStrings(text...)
	return " " + fullText
}

// The three FTS columns of `dbAlbum.PostMapArgs`
// (persistence/album_repository.go:71-96), verbatim:
//
//	fullText := []string{a.Name, a.SortAlbumName, a.AlbumArtist}
//	participantNames := a.Album.Participants.AllNames()
//	fullText = append(fullText, participantNames...)
//	fullText = append(fullText, slices.Collect(maps.Values(a.Album.Discs))...)
//	fullText = append(fullText, a.Album.Tags[model.TagAlbumVersion]...)
//	fullText = append(fullText, a.Album.Tags[model.TagCatalogNumber]...)
//	args["full_text"] = formatFullText(fullText...)
//	args["search_participants"] = strings.Join(participantNames, " ")
//	args["search_normalized"] = str.NormalizeForFTS(a.Name, a.AlbumArtist)
func albumFTS(a *model.Album) (string, string, string) {
	fullText := []string{a.Name, a.SortAlbumName, a.AlbumArtist}
	participantNames := a.Participants.AllNames()
	fullText = append(fullText, participantNames...)
	fullText = append(fullText, slices.Collect(maps.Values(a.Discs))...)
	fullText = append(fullText, a.Tags[model.TagAlbumVersion]...)
	fullText = append(fullText, a.Tags[model.TagCatalogNumber]...)
	return formatFullText(fullText...),
		strings.Join(participantNames, " "),
		str.NormalizeForFTS(a.Name, a.AlbumArtist)
}

// The three FTS columns of `dbMediaFile.PostMapArgs`
// (persistence/mediafile_repository.go:69-79), verbatim:
//
//	fullText := []string{m.FullTitle(), m.Album, m.Artist, m.AlbumArtist,
//		m.SortTitle, m.SortAlbumName, m.SortArtistName, m.SortAlbumArtistName, m.DiscSubtitle}
//	participantNames := m.MediaFile.Participants.AllNames()
//	fullText = append(fullText, participantNames...)
//	args["full_text"] = formatFullText(fullText...)
//	args["search_participants"] = strings.Join(participantNames, " ")
//	args["search_normalized"] = str.NormalizeForFTS(m.FullTitle(), m.Album, m.Artist, m.AlbumArtist)
//
// `m.FullTitle()` is the REAL model.MediaFile.FullTitle
// (model/mediafile.go:109-113): it reads
// `conf.Server.Subsonic.AppendSubtitle` and `Tags.First(TagSubtitle)`
// (model/tag.go:85-90) at call time.
func mediaFileFTS(mf *model.MediaFile) (string, string, string, string) {
	fullText := []string{mf.FullTitle(), mf.Album, mf.Artist, mf.AlbumArtist,
		mf.SortTitle, mf.SortAlbumName, mf.SortArtistName, mf.SortAlbumArtistName, mf.DiscSubtitle}
	participantNames := mf.Participants.AllNames()
	fullText = append(fullText, participantNames...)
	return formatFullText(fullText...),
		strings.Join(participantNames, " "),
		str.NormalizeForFTS(mf.FullTitle(), mf.Album, mf.Artist, mf.AlbumArtist),
		mf.FullTitle()
}

type participant struct {
	Name           string `json:"name"`
	SortArtistName string `json:"sort_artist_name"`
}

type caseInput struct {
	Kind string `json:"kind"`
	ID   string `json:"id"`

	// album (persistence/album_repository.go:71-96)
	Name            string               `json:"name"`
	SortAlbumName   string               `json:"sort_album_name"`
	AlbumArtist     string               `json:"album_artist"`
	Participants    map[string][]participant `json:"participants"`
	Discs           map[string]string    `json:"discs"`
	Tags            map[string][]string  `json:"tags"`

	// mediafile (persistence/mediafile_repository.go:69-79)
	Title               string `json:"title"`
	Subtitle            string `json:"subtitle"`
	AppendSubtitle      bool   `json:"append_subtitle"`
	Album               string `json:"album"`
	Artist              string `json:"artist"`
	SortTitle           string `json:"sort_title"`
	SortArtistName      string `json:"sort_artist_name"`
	SortAlbumArtistName string `json:"sort_album_artist_name"`
	DiscSubtitle        string `json:"disc_subtitle"`
}

type row struct {
	ID                 string `json:"id"`
	Kind               string `json:"kind"`
	FullText           string `json:"full_text"`
	SearchParticipants string `json:"search_participants"`
	SearchNormalized   string `json:"search_normalized"`
	FullTitle          string `json:"full_title,omitempty"`
}

func buildParticipants(raw map[string][]participant) model.Participants {
	p := model.Participants{}
	for role, list := range raw {
		r := model.RoleFromString(role)
		if r == model.RoleInvalid {
			panic(fmt.Sprintf("unknown role %q", role))
		}
		for _, a := range list {
			// The ID is irrelevant to AllNames; a deterministic placeholder
			// keeps the dump stable.
			p.Add(r, model.Artist{ID: "id:" + a.Name, Name: a.Name, SortArtistName: a.SortArtistName})
		}
	}
	return p
}

func buildTags(raw map[string][]string) model.Tags {
	tags := model.Tags{}
	for k, v := range raw {
		tags[model.TagName(k)] = v
	}
	return tags
}

func buildDiscs(raw map[string]string) model.Discs {
	d := model.Discs{}
	for k, v := range raw {
		n, err := strconv.Atoi(k)
		if err != nil {
			panic(fmt.Sprintf("disc key %q is not an int: %v", k, err))
		}
		d[n] = v
	}
	return d
}

func run(c caseInput) row {
	switch c.Kind {
	case "album":
		album := model.Album{
			Name:            c.Name,
			SortAlbumName:   c.SortAlbumName,
			AlbumArtist:     c.AlbumArtist,
			Participants:    buildParticipants(c.Participants),
			Discs:           buildDiscs(c.Discs),
			Tags:            buildTags(c.Tags),
		}
		ft, sp, sn := albumFTS(&album)
		return row{ID: c.ID, Kind: c.Kind, FullText: ft, SearchParticipants: sp, SearchNormalized: sn}
	case "mediafile":
		tags := model.Tags{}
		if c.Subtitle != "" {
			tags[model.TagSubtitle] = []string{c.Subtitle}
		}
		// FullTitle reads the conf at call time (model/mediafile.go:110).
		conf.Server.Subsonic.AppendSubtitle = c.AppendSubtitle
		mf := model.MediaFile{
			Title:               c.Title,
			Album:               c.Album,
			Artist:              c.Artist,
			AlbumArtist:         c.AlbumArtist,
			SortTitle:           c.SortTitle,
			SortAlbumName:       c.SortAlbumName,
			SortArtistName:      c.SortArtistName,
			SortAlbumArtistName: c.SortAlbumArtistName,
			DiscSubtitle:        c.DiscSubtitle,
			Participants:        buildParticipants(c.Participants),
			Tags:                tags,
		}
		ft, sp, sn, fullTitle := mediaFileFTS(&mf)
		return row{ID: c.ID, Kind: c.Kind, FullText: ft, SearchParticipants: sp, SearchNormalized: sn, FullTitle: fullTitle}
	default:
		panic(fmt.Sprintf("case %s: unknown kind %q", c.ID, c.Kind))
	}
}

func main() {
	if len(os.Args) != 3 {
		panic("usage: go_feeders <cases.json> <output.json>")
	}
	raw, err := os.ReadFile(os.Args[1])
	if err != nil {
		panic(fmt.Sprintf("reading cases: %v", err))
	}
	var cases []caseInput
	if err := json.Unmarshal(raw, &cases); err != nil {
		panic(fmt.Sprintf("parsing cases: %v", err))
	}
	rows := make([]row, 0, len(cases))
	for _, c := range cases {
		rows = append(rows, run(c))
	}
	out, err := json.MarshalIndent(rows, "", "  ")
	if err != nil {
		panic(fmt.Sprintf("marshalling rows: %v", err))
	}
	if err := os.WriteFile(os.Args[2], append(out, '\n'), 0o644); err != nil {
		panic(fmt.Sprintf("writing output: %v", err))
	}
	fmt.Printf("wrote %d rows to %s\n", len(rows), os.Args[2])
}
