//! Injected configuration for the mapping layer (#S1 explicit-construction
//! freeze: no `conf.Server` global).
//!
//! Every field corresponds to a Go option read by `model/metadata` and
//! `model/tag_mappings.go`, and defaults to the Go default in
//! `conf/configuration.go`, so a caller that supplies
//! [`MappingOptions::default()`] gets Go's shipped behaviour.

use std::collections::BTreeMap;

/// Defaults copied from `conf/configuration.go:1013` (`ignoredarticles`).
pub const DEFAULT_IGNORED_ARTICLES: &str = "The El La Los Las Le Les Os As O A";

/// Defaults copied from `consts/consts.go:221` (`ArtistJoiner`).
pub const DEFAULT_ARTIST_JOINER: &str = " • ";

/// Defaults copied from `model/tag_mappings.go:367` (`defaultMaxTagLength`).
pub const DEFAULT_MAX_TAG_LENGTH: usize = 1024;

/// Per-tag configuration a user may override (Go `conf.Server.Tags`,
/// `model/tag_mappings.go:350-381`). An absent entry keeps the YAML values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagOverride {
    /// Replace the alias list. Empty means "keep the YAML list"
    /// (`model/tag_mappings.go:361-363`).
    pub aliases: Vec<String>,
    /// Replace the split separators. Empty means "keep the YAML list"
    /// (`model/tag_mappings.go:364-367`).
    pub split: Vec<String>,
    /// Override the type; empty string keeps the YAML type
    /// (`model/tag_mappings.go:371`).
    pub type_name: String,
    /// Override the maximum length; 0 keeps the YAML value
    /// (`model/tag_mappings.go:372`).
    pub max_length: usize,
    /// Override the album-level flag; false keeps the YAML value
    /// (`model/tag_mappings.go:373`).
    pub album: bool,
    /// Drop the tag entirely (`model/tag_mappings.go:351-354`).
    pub ignore: bool,
}

/// Scanner/mapping options the mapping layer reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingOptions {
    /// `Scanner.ArtistJoiner` (`consts/consts.go:221`), used by
    /// `mapDisplayName` (`model/metadata/map_participants.go:218-223`).
    pub artist_joiner: String,
    /// `IgnoredArticles` (`conf/configuration.go:1013`), used by
    /// `str.SanitizeFieldForSortingNoArticle`.
    pub ignored_articles: String,
    /// `Scanner.ArtistSplitExceptions` (`model/tag_mappings.go:145-153`):
    /// names that must not be split by the participant separators.
    pub artist_split_exceptions: Vec<String>,
    /// Deprecated `Scanner.GenreSeparators` (`model/tag_mappings.go:341-347`):
    /// each character becomes a genre split separator when the genre tag has
    /// no configured aliases.
    pub genre_separators: String,
    /// `Scanner.GroupAlbumReleases` (`model/metadata/legacy_ids.go:30`):
    /// when false, the legacy album id includes the release date.
    pub group_album_releases: bool,
    /// Per-tag overrides keyed by canonical (lowercase) tag name.
    pub tags: BTreeMap<String, TagOverride>,
}

impl Default for MappingOptions {
    fn default() -> Self {
        Self {
            artist_joiner: DEFAULT_ARTIST_JOINER.to_string(),
            ignored_articles: DEFAULT_IGNORED_ARTICLES.to_string(),
            artist_split_exceptions: Vec::new(),
            genre_separators: String::new(),
            group_album_releases: false,
            tags: BTreeMap::new(),
        }
    }
}

/// PID specs in effect (Go `model.PIDConfig`, `model/library.go:39-52`).
///
/// Empty strings are resolved to the Go defaults by
/// [`PidOptions::effective`] (`model/library.go:47-52` falls back to
/// `conf.Server.PID`, whose defaults are `consts.DefaultAlbumPID`/
/// `consts.DefaultTrackPID` at `conf/configuration.go:1105-1106`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PidOptions {
    /// Per-library album spec (Go `Library.PIDAlbum`).
    pub album: String,
    /// Per-library track spec (Go `Library.PIDTrack`).
    pub track: String,
}

/// The mapping context: injected options plus the mapping table derived from
/// them (Go's global `conf.Server` + `model.TagMappings()` pair). Building it
/// once per scanner run mirrors Go's `sync.OnceValues` load.
#[derive(Debug, Clone)]
pub struct MappingContext {
    options: MappingOptions,
    mappings: std::sync::Arc<crate::mappings::Mappings>,
}

impl MappingContext {
    /// Builds the table from the options (Go `parseMappings` applies the `Tags`
    /// overrides to the loaded YAML, `model/tag_mappings.go:341-381`).
    #[must_use]
    pub fn new(options: MappingOptions) -> Self {
        let mappings = std::sync::Arc::new(crate::mappings::Mappings::parse_with(
            crate::mappings::MAPPINGS_YAML,
            &options,
        ));
        Self { options, mappings }
    }

    /// The options in effect.
    #[must_use]
    pub fn options(&self) -> &MappingOptions {
        &self.options
    }

    /// The tag mapping table in effect.
    #[must_use]
    pub fn mappings(&self) -> &crate::mappings::Mappings {
        &self.mappings
    }
}

impl Default for MappingContext {
    /// Go's shipped defaults: default options + the YAML table unmodified.
    fn default() -> Self {
        Self {
            options: MappingOptions::default(),
            mappings: std::sync::Arc::new(crate::mappings::mappings().clone()),
        }
    }
}

/// Resolved PID specs (Go `Library.EffectivePID`, `model/library.go:47-52`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PidConfig<'a> {
    /// The track spec in effect.
    pub track: &'a str,
    /// The album spec in effect.
    pub album: &'a str,
}

impl PidOptions {
    /// `Library.EffectivePID`: a library override wins, else the defaults.
    ///
    /// Go falls back to `conf.Server.PID`, which itself defaults to
    /// `consts.DefaultAlbumPID`/`consts.DefaultTrackPID`
    /// (`conf/configuration.go:502-503`, `:1105-1106`); use
    /// [`PidOptions::effective_with`] when the global config differs.
    #[must_use]
    pub fn effective(&self) -> PidConfig<'_> {
        PidConfig {
            track: if self.track.is_empty() {
                nd_id::DEFAULT_TRACK_PID
            } else {
                &self.track
            },
            album: if self.album.is_empty() {
                nd_id::DEFAULT_ALBUM_PID
            } else {
                &self.album
            },
        }
    }

    /// `Library.EffectivePID` with an explicit global fallback (the
    /// `conf.Server.PID` half of `model/library.go:47-52`). An empty global
    /// falls back to the Go constants.
    #[must_use]
    pub fn effective_with<'a>(&'a self, global: &'a Self) -> PidConfig<'a> {
        let global_track = if global.track.is_empty() {
            nd_id::DEFAULT_TRACK_PID
        } else {
            global.track.as_str()
        };
        let global_album = if global.album.is_empty() {
            nd_id::DEFAULT_ALBUM_PID
        } else {
            global.album.as_str()
        };
        PidConfig {
            track: if self.track.is_empty() {
                global_track
            } else {
                &self.track
            },
            album: if self.album.is_empty() {
                global_album
            } else {
                &self.album
            },
        }
    }
}
