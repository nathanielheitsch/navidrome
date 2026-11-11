//! Tag mapping table, ported from `resources/mappings.yaml` +
//! `model/tag_mappings.go`.
//!
//! Go loads the YAML once (`model/tag_mappings.go:326-335`) and normalizes it
//! (`collectTags`, `:259-277`): aliases are lowercased and the tag name itself
//! becomes the lowercased key. This port parses the *actual* embedded YAML at
//! build time ([`MAPPINGS_YAML`]), so the table cannot drift from the Go source.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;

/// The Go `resources/mappings.yaml`, embedded verbatim so the Rust table is
/// generated from the same file the Go server reads.
pub const MAPPINGS_YAML: &str = include_str!("../../../../resources/mappings.yaml");

/// The Go `resources/mime_types.yaml`, embedded for [`crate::file_types`].
pub const MIME_TYPES_YAML: &str = include_str!("../../../../resources/mime_types.yaml");

/// Merged/canonical mapping table keyed by lowercase tag name.
///
/// Go iterates this map in unspecified order (`clean`,
/// `model/metadata/metadata.go:211-220`), so the map's own order is not
/// observable: each tag's value order comes from its conf's `Aliases` vector,
/// which is kept in YAML order. `BTreeMap` is therefore a faithful, and
/// deterministic, stand-in.
pub type Table = BTreeMap<String, TagConf>;

/// Alias → canonical name index (Go `tagNameIndex`).
pub type AliasIndex = BTreeMap<String, String>;

/// Tag value type (Go `model.TagType`, `model/tag_mappings.go:182-191`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagType {
    /// `TagTypeString` (also Go's zero value for an unset `type:`).
    String,
    /// `TagTypeInteger` ("int").
    Integer,
    /// `TagTypeFloat` ("float").
    Float,
    /// `TagTypeDate` ("date").
    Date,
    /// `TagTypeUUID` ("uuid").
    Uuid,
    /// `TagTypePair` ("pair").
    Pair,
}

impl TagType {
    /// Parses a YAML `type:` value; unknown/empty yields `String`, matching
    /// Go's zero `TagConf.Type`.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "int" => Self::Integer,
            "float" => Self::Float,
            "date" => Self::Date,
            "uuid" => Self::Uuid,
            "pair" => Self::Pair,
            _ => Self::String,
        }
    }
}

/// One tag's configuration (Go `model.TagConf`, `model/tag_mappings.go:30-38`).
#[derive(Debug, Clone)]
pub struct TagConf {
    /// Lowercased aliases (`model/tag_mappings.go:261-264`).
    pub aliases: Vec<String>,
    /// Value type.
    pub type_name: TagType,
    /// Maximum value length in bytes; 0 means "use
    /// [`crate::config::DEFAULT_MAX_TAG_LENGTH`]" (`model/metadata/metadata.go:367-371`).
    pub max_length: usize,
    /// Literal split separators in YAML order (`TagConf.Split`).
    pub split: Vec<String>,
    /// Album-level flag (`TagConf.Album`): decides which tags survive the
    /// first-class-field pruning (`model/metadata/map_mediafile.go:104-112`).
    pub album: bool,
    /// Compiled alternation of the separators (Go `TagConf.SplitRx`,
    /// `model/tag_mappings.go:279-304`).
    split_rx: Option<Regex>,
    /// Attached artist-split-exceptions regex (Go `TagConf.ExceptionsRx`,
    /// set by `WithParticipantExceptions`, `model/tag_mappings.go:175-180`).
    pub(crate) exceptions: Option<Regex>,
}

/// Compares the declarative fields only; the compiled regexes are derived from
/// them, so comparing them would be redundant.
impl PartialEq for TagConf {
    fn eq(&self, other: &Self) -> bool {
        self.aliases == other.aliases
            && self.type_name == other.type_name
            && self.max_length == other.max_length
            && self.split == other.split
            && self.album == other.album
    }
}

impl TagConf {
    /// Builds a conf, compiling the split separators like Go's
    /// `compileSplitRegex`.
    #[must_use]
    pub fn new(
        aliases: Vec<String>,
        type_name: TagType,
        max_length: usize,
        split: Vec<String>,
        album: bool,
    ) -> Self {
        Self {
            aliases,
            type_name,
            max_length,
            split_rx: compile_split_regex(&split),
            split,
            album,
            exceptions: None,
        }
    }

    /// `TagConf.WithParticipantExceptions` (`model/tag_mappings.go:175-180`):
    /// attaches the global artist-split exceptions, but only for participant
    /// (artist/role) tag names — so genre-style splits never see them.
    #[must_use]
    pub fn with_participant_exceptions(&self, name: &str, exceptions: Option<&Regex>) -> Self {
        let mut conf = self.clone();
        if is_participant_tag_name(name) {
            conf.exceptions = exceptions.cloned();
        }
        conf
    }

    /// The compiled split regex, if any (Go `TagConf.SplitRx`).
    #[must_use]
    pub fn split_rx(&self) -> Option<&Regex> {
        self.split_rx.as_ref()
    }

    /// `TagConf.SplitTagValue` (`model/tag_mappings.go:42-52`): split every
    /// value by the separators and trim each part, skipping separators that
    /// fall inside an exception span. Returns the input unchanged when no
    /// separators are configured.
    #[must_use]
    pub fn split_tag_value(&self, values: &[String]) -> Vec<String> {
        let Some(rx) = self.split_rx.as_ref() else {
            return values.to_vec();
        };
        if values.is_empty() {
            return values.to_vec();
        }
        let mut out = Vec::with_capacity(values.len());
        for value in values {
            out.extend(split_value(value, rx, self.exceptions.as_ref()));
        }
        out
    }

    /// `splitParticipantValues` (`model/metadata/map_participants.go:179-184`):
    /// split, then drop empty and duplicated values. Values are returned
    /// unchanged when the conf has no separators.
    #[must_use]
    pub fn split_participant_values(&self, values: &[String]) -> Vec<String> {
        if self.split.is_empty() {
            return values.to_vec();
        }
        filter_duplicated_or_empty(self.split_tag_value(values))
    }
}

/// Splits one value, skipping separator matches inside protected spans
/// (`model/tag_mappings.go:54-66`).
pub(crate) fn split_value(tag: &str, rx: &Regex, exceptions: Option<&Regex>) -> Vec<String> {
    let protected = protected_spans(tag, exceptions);
    let mut parts: Vec<String> = Vec::new();
    let mut start = 0usize;
    for m in rx.find_iter(tag) {
        if overlaps_any(m.start(), m.end(), &protected) {
            continue;
        }
        parts.push(tag[start..m.start()].trim().to_string());
        start = m.end();
    }
    parts.push(tag[start..].trim().to_string());
    parts
}

/// `protectedSpans` (`model/tag_mappings.go:71-82`): matches sitting on
/// non-word boundaries. Go checks this rune-aware because RE2 `\b` is
/// ASCII-only and would never match names with accented letters.
fn protected_spans(tag: &str, rx: Option<&Regex>) -> Vec<(usize, usize)> {
    let Some(rx) = rx else {
        return Vec::new();
    };
    rx.find_iter(tag)
        .filter(|m| is_word_bounded(tag, m.start(), m.end()))
        .map(|m| (m.start(), m.end()))
        .collect()
}

/// `isWordBounded` (`model/tag_mappings.go:84-89`).
fn is_word_bounded(s: &str, start: usize, end: usize) -> bool {
    let is_word = |r: char| r.is_alphabetic() || r.is_numeric();
    let before = s[..start].chars().next_back();
    let after = s[end..].chars().next();
    !before.is_some_and(is_word) && !after.is_some_and(is_word)
}

/// `overlapsAny` (`model/tag_mappings.go:91-98`).
fn overlaps_any(span_start: usize, span_end: usize, spans: &[(usize, usize)]) -> bool {
    spans.iter().any(|(s, e)| span_start < *e && *s < span_end)
}

/// `compileExceptionsRegex` (`model/tag_mappings.go:102-131`): a
/// case-insensitive alternation of literal names, longest-first so overlapping
/// entries win (ties broken lexicographically).
pub(crate) fn compile_exceptions_regex(names: &[String]) -> Option<Regex> {
    let mut names: Vec<&str> = names
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if names.is_empty() {
        return None;
    }
    names.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    let pattern = format!(
        "(?i)({})",
        names
            .iter()
            .map(|n| regex::escape(n))
            .collect::<Vec<_>>()
            .join("|")
    );
    Regex::new(&pattern).ok()
}

/// `compileSplitRegex` (`model/tag_mappings.go:279-304`): one case-insensitive
/// alternation of the escaped separators. Empty entries are dropped, and a
/// list containing only empty entries yields no regex (value returned
/// unchanged, as in Go).
fn compile_split_regex(split: &[String]) -> Option<Regex> {
    let escaped: Vec<String> = split
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| regex::escape(s))
        .collect();
    if escaped.is_empty() {
        return None;
    }
    Regex::new(&format!("(?i)({})", escaped.join("|"))).ok()
}

/// Drops empty values and later duplicates, preserving order
/// (`filterDuplicatedOrEmptyValues`, `model/metadata/metadata.go:329-343`).
pub(crate) fn filter_duplicated_or_empty(values: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::with_capacity(values.len());
    let mut result = Vec::with_capacity(values.len());
    for v in values {
        if v.is_empty() || !seen.insert(v.clone()) {
            continue;
        }
        result.push(v);
    }
    result
}

/// The normalized mapping table plus the two conf blocks (Go `mappingsConf` +
/// `parseMappings`, `model/tag_mappings.go:21-26`, `:237-257`).
#[derive(Debug, Clone)]
pub struct Mappings {
    all: Table,
    main: Table,
    roles: TagConf,
    artists: TagConf,
    index: AliasIndex,
}

impl Mappings {
    /// Parses the YAML with Go defaults (no user `Tags` overrides).
    #[must_use]
    pub fn parse(yaml: &str) -> Self {
        Self::parse_with(yaml, &crate::config::MappingOptions::default())
    }

    /// Parses the YAML and applies the config overrides Go applies in
    /// `parseMappings`/`loadTagMappings` (`model/tag_mappings.go:237-257`,
    /// `:341-381`): the deprecated `Scanner.GenreSeparators` block, then
    /// `Tags.<name>` overrides (ignore, aliases, split, type, maxLength, album).
    #[must_use]
    pub fn parse_with(yaml: &str, opts: &crate::config::MappingOptions) -> Self {
        let doc: serde_yaml::Value =
            serde_yaml::from_str(yaml).unwrap_or_else(|e| panic!("mappings.yaml must parse ({e})"));
        let mut main = section(doc.get("main"));
        let mut additional = section(doc.get("additional"));

        // Deprecated `Scanner.GenreSeparators` (`model/tag_mappings.go:341-347`):
        // only when set and no `Tags.genre` aliases were configured.
        if !opts.genre_separators.is_empty()
            && opts
                .tags
                .get("genre")
                .map(|t| t.aliases.is_empty())
                .unwrap_or(true)
        {
            if let Some(conf) = main.iter_mut().find(|(k, _)| k == "genre").map(|(_, c)| c) {
                conf.split = opts
                    .genre_separators
                    .chars()
                    .map(|c| c.to_string())
                    .collect();
                conf.split_rx = compile_split_regex(&conf.split);
            }
        }

        // `Tags.<name>` overrides (`model/tag_mappings.go:350-381`).
        for (tag, cfg) in &opts.tags {
            if cfg.ignore {
                main.retain(|(k, _)| k != tag);
                additional.retain(|(k, _)| k != tag);
                continue;
            }
            let old = lookup(&main, tag)
                .or_else(|| lookup(&additional, tag))
                .cloned()
                .unwrap_or_else(|| TagConf::new(Vec::new(), TagType::String, 0, Vec::new(), false));
            let aliases = if cfg.aliases.is_empty() {
                old.aliases.clone()
            } else {
                cfg.aliases.clone()
            };
            let split = if cfg.split.is_empty() {
                old.split.clone()
            } else {
                cfg.split.clone()
            };
            let type_name = if cfg.type_name.is_empty() {
                old.type_name
            } else {
                TagType::parse(&cfg.type_name)
            };
            let max_length = if cfg.max_length == 0 {
                old.max_length
            } else {
                cfg.max_length
            };
            let album = cfg.album || old.album;
            let conf = TagConf::new(aliases, type_name, max_length, split, album);
            match main.iter_mut().find(|(k, _)| k == tag) {
                Some(slot) => slot.1 = conf,
                None => additional.push((tag.clone(), conf)),
            }
        }
        let roles = tag_conf("roles", doc.get("roles"));
        let artists = tag_conf("artists", doc.get("artists"));

        // `parseMappings` normalizes each section separately (`:241-247`)
        // and then merges main over additional (`:249-256`).
        let mut normalized_main: Table = Table::new();
        collect_tags(&main, &mut normalized_main);
        let mut normalized_additional: Table = Table::new();
        collect_tags(&additional, &mut normalized_additional);

        // Go merges into a plain map (`:249-256`): additional first, then main,
        // so a tag defined in both sections keeps the main conf. Iteration order
        // of that merged map does not matter for `clean`, which only needs every
        // tag present (value order comes from each conf's alias list).
        let mut all: Table = Table::new();
        for (k, v) in normalized_additional {
            all.insert(k, v);
        }
        for (k, v) in normalized_main.clone() {
            all.insert(k, v);
        }

        // `tagNameIndex` (`:204-218`): aliases first, then names, so a tag
        // name always wins over an alias pointing at another tag
        // (`musicbrainz_trackid` is both a tag and an alias of
        // `musicbrainz_recordingid`).
        let mut index: AliasIndex = BTreeMap::new();
        for (name, conf) in &all {
            for alias in &conf.aliases {
                index.insert(alias.clone(), name.clone());
            }
        }
        for name in all.keys() {
            index.insert(name.clone(), name.clone());
        }

        Self {
            all,
            main: normalized_main,
            roles,
            artists,
            index,
        }
    }

    /// The merged table (Go `model.TagMappings()`, `model/tag_mappings.go:193-196`).
    #[must_use]
    pub fn all(&self) -> &Table {
        &self.all
    }

    /// The main section (Go `model.TagMainMappings()`, `:230-233`).
    #[must_use]
    pub fn main(&self) -> &Table {
        &self.main
    }

    /// The `roles:` conf (Go `model.TagRolesConf()`, `:220-223`).
    #[must_use]
    pub fn roles(&self) -> &TagConf {
        &self.roles
    }

    /// The `artists:` conf (Go `model.TagArtistsConf()`, `:225-228`).
    #[must_use]
    pub fn artists(&self) -> &TagConf {
        &self.artists
    }

    /// `model.CanonicalTagName` (`model/tag_mappings.go:199-202`).
    #[must_use]
    pub fn canonical_tag_name(&self, name: &str) -> Option<&str> {
        self.index.get(&name.to_lowercase()).map(String::as_str)
    }
}

/// The process-wide table with Go defaults (Go's `sync.OnceValues`,
/// `model/tag_mappings.go:237`). A caller with user config builds its own via
/// [`Mappings::parse_with`] and hands it to [`crate::config::MappingContext`].
#[must_use]
pub fn mappings() -> &'static Mappings {
    static ONCE: OnceLock<Mappings> = OnceLock::new();
    ONCE.get_or_init(|| Mappings::parse(MAPPINGS_YAML))
}

/// `participantTagNames` (`model/tag_mappings.go:157-171`): the eight artist
/// tags plus every role and `<role>sort` tag.
pub(crate) fn is_participant_tag_name(name: &str) -> bool {
    const ARTIST_TAGS: &[&str] = &[
        "artist",
        "artists",
        "artistsort",
        "artistssort",
        "albumartist",
        "albumartists",
        "albumartistsort",
        "albumartistssort",
    ];
    if ARTIST_TAGS.contains(&name) {
        return true;
    }
    // Every role string and its `sort` variant (`model/participants.go:32-47`).
    nd_id::ALL_ROLES
        .iter()
        .any(|role| name == role.as_str() || name == format!("{}sort", role.as_str()))
}

/// `collectTags` (`model/tag_mappings.go:259-277`): lowercase aliases, compile
/// the split regex (only for string-typed tags), and key by the lowercased
/// tag name. Keys are already lowercased in `mappings.yaml`, so the map's
/// insertion order is preserved rather than re-sorted.
fn collect_tags(input: &Vec<(String, TagConf)>, out: &mut Table) {
    for (name, conf) in input {
        let aliases: Vec<String> = conf.aliases.iter().map(|a| a.to_lowercase()).collect();
        // Splitting is only honoured for string types (`:265-273`); Go logs an
        // error and drops the separators.
        let split = if conf.type_name != TagType::String {
            Vec::new()
        } else {
            conf.split.clone()
        };
        out.insert(
            name.to_lowercase(),
            TagConf {
                aliases,
                type_name: conf.type_name,
                max_length: conf.max_length,
                album: conf.album,
                split_rx: compile_split_regex(&split),
                split,
                exceptions: None,
            },
        );
    }
}

/// Finds a raw conf by its YAML key.
fn lookup<'a>(section: &'a [(String, TagConf)], name: &str) -> Option<&'a TagConf> {
    section.iter().find(|(k, _)| k == name).map(|(_, c)| c)
}

/// Reads a `main:`/`additional:` section into raw (un-normalized) confs,
/// preserving YAML order (Go's `clean` iterates the mapping in insertion order
/// and `processRegularMapping` iterates `Aliases` in YAML order, which decides
/// which duplicate value wins).
fn section(value: Option<&serde_yaml::Value>) -> Vec<(String, TagConf)> {
    let mut out: Vec<(String, TagConf)> = Vec::new();
    let Some(map) = value.and_then(|v| v.as_mapping()) else {
        return out;
    };
    for (key, val) in map {
        let Some(name) = key.as_str() else { continue };
        out.push((name.to_string(), tag_conf(name, Some(val))));
    }
    out
}

/// Reads one tag conf (`aliases`, `type`, `maxLength`, `split`, `album`).
fn tag_conf(tag_name: &str, value: Option<&serde_yaml::Value>) -> TagConf {
    let Some(map) = value.and_then(|v| v.as_mapping()) else {
        return TagConf::new(Vec::new(), TagType::String, 0, Vec::new(), false);
    };
    let get = |key: &str| map.get(serde_yaml::Value::String(key.to_string()));

    let aliases = get("aliases")
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let type_name = TagType::parse(get("type").and_then(|v| v.as_str()).unwrap_or(""));

    let max_length = get("maxLength")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(0);

    let split = get("split")
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .map(|v| v.as_str().unwrap_or("").to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let album = get("album").and_then(|v| v.as_bool()).unwrap_or(false);

    // Go rejects split on non-string tags at load time
    // (`model/tag_mappings.go:265-273`); keep the parsed `split` list so a
    // caller sees it, but never compile a regex for it here — `collect_tags`
    // is where Go drops it.
    let _ = tag_name;
    TagConf::new(aliases, type_name, max_length, split, album)
}
