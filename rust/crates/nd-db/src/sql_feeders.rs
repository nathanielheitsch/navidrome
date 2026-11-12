//! FTS write-column feeders for the `album` and `media_file` repositories.
//!
//! Go computes the three full-text-search columns at `Put` time in the
//! repository's `PostMapArgs` hook:
//!
//! - `dbAlbum.PostMapArgs` (`persistence/album_repository.go:71-96`):
//!   `full_text` from `Name`, `SortAlbumName`, `AlbumArtist`, the participant
//!   names, the `Discs` map values, `Tags[TagAlbumVersion]` and
//!   `Tags[TagCatalogNumber]`; `search_participants` the space-joined
//!   participant names; `search_normalized` =
//!   `str.NormalizeForFTS(Name, AlbumArtist)`.
//! - `dbMediaFile.PostMapArgs` (`persistence/mediafile_repository.go:69-79`):
//!   `full_text` from `FullTitle()`, `Album`, `Artist`, `AlbumArtist`,
//!   `SortTitle`, `SortAlbumName`, `SortArtistName`,
//!   `SortAlbumArtistName`, `DiscSubtitle`, then the participant names;
//!   `search_participants` as above; `search_normalized` =
//!   `str.NormalizeForFTS(FullTitle(), Album, Artist, AlbumArtist)`.
//!
//! Before this module, **no production code computed any of these columns**
//! (hazard 6 in `docs/rust-port/next-work.md`); the only in-crate producer was
//! the artist-only pair in [`artist_repo`](crate::artist_repo), which cannot
//! express album's or mediafile's inputs. These functions are now the
//! producer of [`mediafile_repo::FtsColumns`]: the SQL repositories take the
//! columns already-marshalled. The participant/tag JSON marshalling is not
//! here: `sql_participations.rs` (slice S-A) and the entity-marshal lane
//! (S-D) own it — participant names arrive pre-collected as `&[String]`,
//! mirroring Go's `Participants.AllNames` (`model/participants.go:174-185`).
//!
//! **Deliberate divergence — `search_participants` order.** Go's `AllNames`
//! iterates `map[Role]ParticipantList`, so the byte order of the joined
//! string is *not reproducible* across runs of the same Go binary (same class
//! as the #16 `parseID3Pairs` hazard, `docs/rust-port/next-work.md`
//! §Then-7). Per the settled rule "persist no ordering inherited from either
//! side", [`search_participants`] sorts (and re-dedupes) the names before
//! joining. `full_text` is unaffected by name order:
//! `str.SanitizeStrings` sorts and dedupes the words.

use nd_utils::str::{normalize_for_fts, sanitize_strings};

/// `formatFullText` (`persistence/sql_search.go:15-18`):
/// `" " + str.SanitizeStrings(text...)`. The leading space is a FTS5 token
/// separator and is **invariant**: the result always starts with a space,
/// even when every input is empty (then the result is `" "`).
#[must_use]
pub fn format_full_text(text: &[&str]) -> String {
    format!(" {}", sanitize_strings(text))
}

/// The `search_normalized` column: `str.NormalizeForFTS`
/// (`utils/str/normalize_fts.go:10-44`). The output order follows the input
/// order, so the callers must keep Go's field order (4 fields for mediafile,
/// 2 for album — see [`mediafile_fts_columns`] / [`album_fts_columns`]).
#[must_use]
pub fn search_normalized(values: &[&str]) -> String {
    normalize_for_fts(values)
}

/// `AllNames` + `strings.Join(names, " ")` (`model/participants.go:174-185`,
/// `mediafile_repository.go:72-73`, `album_repository.go:84-85`).
///
/// Go's name order comes from map iteration and is non-reproducible; the port
/// **deliberately diverges** by sorting (byte order) and re-deduping before
/// joining. Input is expected already unique (`AllNames` applies
/// `slice.Unique`), but the re-dedup makes the output safe for any input.
/// Empty input yields `""` (Go: `strings.Join(nil, " ")` = `""`).
#[must_use]
pub fn search_participants(names: &[String]) -> String {
    let mut sorted: Vec<&String> = names.iter().collect();
    sorted.sort();
    sorted.dedup();
    sorted
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `model.MediaFile.FullTitle` (`model/mediafile.go:109-113` +
/// `appendSuffix` `:124-131`). Go condition:
/// `Tags.First(TagSubtitle) != "" && conf.Server.Subsonic.AppendSubtitle` —
/// here spelled as `!subtitle.is_empty() && append_subtitle`, where
/// `subtitle` is the first `subtitle` tag value (`Tags.First`,
/// `model/tag.go:85-90`; empty string when the tag is absent).
#[must_use]
pub fn full_title(title: &str, subtitle: &str, append_subtitle: bool) -> String {
    if append_subtitle && !subtitle.is_empty() {
        append_suffix(title, subtitle)
    } else {
        title.to_string()
    }
}

/// `appendSuffix` (`model/mediafile.go:124-131`), byte-exact: Go compares
/// `suffix[0]` and `suffix[len-1]` **as bytes** against
/// `{'(': ')', '[': ']', '{': '}', '<': '>'}`. The `0` default mirrors Go's
/// map zero value (only a NUL-terminated suffix could match it).
fn append_suffix(base: &str, suffix: &str) -> String {
    let suffix = suffix.trim();
    let bytes = suffix.as_bytes();
    let closer = match bytes.first().copied() {
        Some(b'(') => b')',
        Some(b'[') => b']',
        Some(b'{') => b'}',
        Some(b'<') => b'>',
        _ => 0,
    };
    if bytes.len() >= 2 && closer == bytes.last().copied().unwrap_or(0) {
        format!("{base} {suffix}")
    } else {
        format!("{base} ({suffix})")
    }
}

/// The three FTS columns `dbMediaFile.PostMapArgs` computes
/// (`persistence/mediafile_repository.go:69-79`).
///
/// `full_title` must be the result of [`full_title`] (Go calls
/// `m.FullTitle()`, which is config-dependent); `participant_names` is the
/// Go `Participants.AllNames()` list in any order (see module docs).
#[must_use]
#[allow(clippy::too_many_arguments)] // mirrors Go's 9-field input list (mediafile_repository.go:69-79)
pub fn mediafile_fts_columns(
    full_title: &str,
    album: &str,
    artist: &str,
    album_artist: &str,
    sort_title: &str,
    sort_album_name: &str,
    sort_artist_name: &str,
    sort_album_artist_name: &str,
    disc_subtitle: &str,
    participant_names: &[String],
) -> crate::mediafile_repo::FtsColumns {
    let full_text_inputs: Vec<&str> = [
        full_title,
        album,
        artist,
        album_artist,
        sort_title,
        sort_album_name,
        sort_artist_name,
        sort_album_artist_name,
        disc_subtitle,
    ]
    .into_iter()
    .chain(participant_names.iter().map(String::as_str))
    .collect();
    crate::mediafile_repo::FtsColumns {
        full_text: format_full_text(&full_text_inputs),
        search_participants: search_participants(participant_names),
        search_normalized: search_normalized(&[full_title, album, artist, album_artist]),
    }
}

/// The three FTS columns `dbAlbum.PostMapArgs` computes
/// (`persistence/album_repository.go:71-96`).
///
/// - `disc_subtitles`: the values of `Album.Discs`
///   (`map[int]string`, `model/album.go:124`). Go iterates the map (order
///   non-reproducible), but order is irrelevant: `full_text` sorts the words.
///   An empty value (a disc with no subtitle) contributes nothing.
/// - `album_version` / `catalog_number`: the values of
///   `Tags[model.TagAlbumVersion]` / `Tags[model.TagCatalogNumber]`
///   (`model/tag.go:197,206`); empty slice when the tag is absent (Go
///   appends a nil slice, which is a no-op).
#[must_use]
pub fn album_fts_columns(
    name: &str,
    sort_album_name: &str,
    album_artist: &str,
    participant_names: &[String],
    disc_subtitles: &[&str],
    album_version: &[&str],
    catalog_number: &[&str],
) -> crate::mediafile_repo::FtsColumns {
    let full_text_inputs: Vec<&str> = [name, sort_album_name, album_artist]
        .into_iter()
        .chain(participant_names.iter().map(String::as_str))
        .chain(disc_subtitles.iter().copied())
        .chain(album_version.iter().copied())
        .chain(catalog_number.iter().copied())
        .collect();
    crate::mediafile_repo::FtsColumns {
        full_text: format_full_text(&full_text_inputs),
        search_participants: search_participants(participant_names),
        search_normalized: search_normalized(&[name, album_artist]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_text_always_starts_with_a_space() {
        // The leading-space invariant: even with no searchable content at all.
        assert_eq!(format_full_text(&[]), " ");
        assert_eq!(format_full_text(&[""]), " ");
        assert_eq!(format_full_text(&["", "  ", "\t\n"]), " ");
        // ...and normal content keeps it while sanitizing:
        let out = format_full_text(&["a", "b"]);
        assert!(out.starts_with(' '));
        assert_eq!(out, " a b");
    }

    #[test]
    fn full_text_is_order_insensitive() {
        // Go appends participant names in map-iteration order (unstable);
        // `SanitizeStrings` sorts the words, so the column must not depend
        // on the order of any input.
        let fields: Vec<&str> = vec!["Deep (Live)", "Söhne der Nacht", "AC/DC"];
        let names: Vec<String> = vec!["Nils Frahm".into(), "Bjørk".into(), "R.E.M.".into()];
        let mut base: Vec<&str> = fields.to_vec();
        base.extend(names.iter().map(String::as_str));
        let expected = format_full_text(&base);

        let mut names_last: Vec<&str> = names.iter().map(String::as_str).collect();
        names_last.extend(fields.to_vec());
        assert_eq!(format_full_text(&names_last), expected, "names last");

        let mut reversed: Vec<&str> = base.to_vec();
        reversed.reverse();
        assert_eq!(format_full_text(&reversed), expected, "reversed");
    }

    #[test]
    fn search_participants_is_sorted_and_deduplicated() {
        // Byte order: 'B' < 'Z' < 'a'; the duplicate is dropped (Go's
        // AllNames is already unique — this pins the defensive re-dedup).
        let names = vec![
            "Bjørk".to_string(),
            "ac/dc".to_string(),
            "Bjørk".to_string(),
            "Zeta".to_string(),
        ];
        assert_eq!(search_participants(&names), "Bjørk Zeta ac/dc");
        let shuffled = vec![&names[2], &names[3], &names[0], &names[1]]
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            search_participants(&shuffled),
            "Bjørk Zeta ac/dc",
            "deterministic under any input order (deliberate divergence from Go's map order)"
        );
        // Empty input: Go's strings.Join(nil, " ") = "".
        assert_eq!(search_participants(&[]), "");
    }

    #[test]
    fn full_title_reproduces_append_suffix_conditions() {
        // Flag off, or no subtitle: plain title.
        assert_eq!(full_title("T", "", true), "T", "empty subtitle");
        assert_eq!(full_title("T", "[Deluxe]", false), "T", "flag off");
        // All four bracket pairs map to the bare " " + suffix form.
        assert_eq!(full_title("T", "[Deluxe]", true), "T [Deluxe]");
        assert_eq!(full_title("T", "{Deluxe}", true), "T {Deluxe}");
        assert_eq!(full_title("T", "<Deluxe>", true), "T <Deluxe>");
        assert_eq!(full_title("T", "(Deluxe)", true), "T (Deluxe)");
        // No brackets, mismatched brackets, and a 1-char suffix: parenthesized.
        assert_eq!(full_title("T", "Deluxe", true), "T (Deluxe)");
        assert_eq!(full_title("T", "[Deluxe", true), "T ([Deluxe)");
        assert_eq!(full_title("T", "s", true), "T (s)");
        // TrimSpace applied before the check (Go: suffix = strings.TrimSpace(suffix)).
        assert_eq!(full_title("T", "  [Deluxe]  ", true), "T [Deluxe]");
        assert_eq!(
            full_title("T", "   ", true),
            "T ()",
            "whitespace-only suffix trims to ()"
        );
    }

    #[test]
    fn mediafile_columns_compose_the_go_shape() {
        // Inputs chosen so the sorted word order is trivially verifiable.
        let c = mediafile_fts_columns(
            "E Track",
            "C Album",
            "B Artist",
            "D AlbumArtist",
            "A SortTitle",
            "",
            "",
            "",
            "",
            &["Nils Frahm".to_string()],
        );
        assert_eq!(
            c.full_text,
            " a album albumartist artist b c d e frahm nils sorttitle track"
        );
        assert_eq!(c.search_participants, "Nils Frahm");
        // Go: NormalizeForFTS(FullTitle, Album, Artist, AlbumArtist) — plain
        // words contribute no variant, so the result is empty.
        assert_eq!(c.search_normalized, "");
    }

    #[test]
    fn album_columns_compose_the_go_shape() {
        // Inputs chosen so the sorted word order is trivially verifiable:
        // a < b < c < d < e < f.
        let c = album_fts_columns(
            "E Name",
            "C SortName",
            "D AlbumArtist",
            &["Nils Frahm".to_string()],
            &["B Disc"],
            &["A Version"],
            &["F Catalog"],
        );
        assert_eq!(
            c.full_text,
            " a albumartist b c catalog d disc e f frahm name nils sortname version"
        );
        assert_eq!(c.search_participants, "Nils Frahm");
        // Go: NormalizeForFTS(Name, AlbumArtist) — plain words contribute
        // no variant, so the result is empty.
        assert_eq!(c.search_normalized, "");
    }
}
