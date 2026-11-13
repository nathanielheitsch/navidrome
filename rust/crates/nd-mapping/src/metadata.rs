//! `model/metadata/metadata.go` — the `Metadata` value produced from an
//! [`nd_tagreader::Info`], its tag-cleaning pipeline and its typed accessors.
//!
//! Go's `Info` also carries a `FileInfo` (`metadata.go:22`, `:28-31`) that
//! storage fills in (`core/storage/local/local.go:90-109`). That is a #17
//! concern; here file stat data arrives through [`FileStat`] and every field
//! that Go derives from it is `Option`, so a missing value is never persisted
//! as a fabricated zero.

use std::collections::BTreeMap;

use regex::Regex;

use crate::config::{MappingContext, MappingOptions};
use crate::mappings::{compile_exceptions_regex, Mappings, TagConf, TagType};

/// U+200B ZERO WIDTH SPACE, the pair separator (`consts.Zwsp`,
/// `consts/consts.go:113`; `metadata.go:52-68`).
pub const ZWSP: char = '\u{200b}';

/// File stat data Go reads through `Info.FileInfo` (`metadata.go:22`, `:28-31`,
/// filled by `core/storage/local/local.go:90-109`).
///
/// `None` fields are surfaced as absent, never defaulted: `Size`/`ModTime`
/// become 0/None in the mapped entity and #17 must fill them from the real
/// storage layer before persisting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileStat {
    /// Size in bytes (Go `FileInfo.Size()`).
    pub size: Option<i64>,
    /// Modification time, RFC 3339 (Go `FileInfo.ModTime()`, mapped to
    /// `MediaFile.UpdatedAt`, `map_mediafile.go:82`).
    pub mod_time: Option<String>,
    /// Birth time, RFC 3339 (Go `FileInfo.BirthTime()`, mapped to
    /// `MediaFile.BirthTime`, `map_mediafile.go:81`).
    pub birth_time: Option<String>,
}

/// A key/value pair (Go `metadata.Pair`, `metadata.go:52-68`), encoded as
/// `key + U+200B + value`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pair(String);

impl Pair {
    /// `Pair.Key()` (`metadata.go:54`).
    #[must_use]
    pub fn key(&self) -> &str {
        self.split_at(0)
    }

    /// `Pair.Value()` (`metadata.go:55`).
    #[must_use]
    pub fn value(&self) -> &str {
        self.split_at(1)
    }

    fn split_at(&self, i: usize) -> &str {
        match self.0.split_once(ZWSP) {
            Some((k, _)) if i == 0 => k,
            Some((_, v)) => v,
            None if i == 0 => self.0.as_str(),
            None => "",
        }
    }
}

impl std::fmt::Display for Pair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// `metadata.NewPair` (`metadata.go:66-68`).
#[must_use]
pub fn new_pair(key: &str, value: &str) -> String {
    format!("{key}{ZWSP}{value}")
}

/// Cleaned tags, keyed by canonical lowercase name (Go `model.Tags`,
/// `model/tag.go:79`).
pub type Tags = BTreeMap<String, Vec<String>>;

/// Empty `Info` used only by the `albumartistid` artist-id path, which reads no
/// tags (`persistent_ids.go:124-127`).
fn empty_info() -> nd_tagreader::Info {
    nd_tagreader::Info {
        tags: BTreeMap::new(),
        audio_properties: nd_tagreader::AudioProperties {
            duration: 0.0,
            bit_rate: 0,
            channels: 0,
            sample_rate: 0,
            bit_depth: 0,
            codec: String::new(),
        },
        has_picture: false,
    }
}

/// The parsed, cleaned metadata for one file (Go `metadata.Metadata`,
/// `metadata.go:80-86`).
#[derive(Debug, Clone)]
pub struct Metadata {
    file_path: String,
    file_stat: FileStat,
    tags: Tags,
    audio_props: nd_tagreader::AudioProperties,
    has_picture: bool,
    ctx: MappingContext,
    /// Pre-compiled artist split exceptions, from the context options.
    exceptions: Option<Regex>,
}

impl Metadata {
    /// `metadata.New` (`metadata.go:70-78`): cleans the raw tags and keeps the
    /// audio properties/has-picture verbatim.
    #[must_use]
    pub fn new(
        file_path: impl Into<String>,
        info: &nd_tagreader::Info,
        file_stat: FileStat,
        ctx: &MappingContext,
    ) -> Self {
        let file_path = file_path.into();
        let exceptions = compile_exceptions_regex(&ctx.options().artist_split_exceptions);
        let tags = clean(&file_path, &info.tags, ctx.mappings(), &exceptions);
        Self {
            file_path,
            file_stat,
            tags,
            audio_props: info.audio_properties.clone(),
            has_picture: info.has_picture,
            ctx: ctx.clone(),
            exceptions,
        }
    }

    /// The path the metadata was read for (Go `Metadata.FilePath()`, `:88`).
    #[must_use]
    pub fn file_path(&self) -> &str {
        &self.file_path
    }

    /// `Metadata.Suffix()` (`metadata.go:92-94`): lowercased extension without
    /// the dot.
    #[must_use]
    pub fn suffix(&self) -> String {
        crate::file_types::extension(&self.file_path)
            .strip_prefix('.')
            .unwrap_or("")
            .to_lowercase()
    }

    /// `Metadata.Size()` (`metadata.go:91`).
    #[must_use]
    pub fn size(&self) -> Option<i64> {
        self.file_stat.size
    }

    /// `Metadata.ModTime()` (`metadata.go:89`).
    #[must_use]
    pub fn mod_time(&self) -> Option<&str> {
        self.file_stat.mod_time.as_deref()
    }

    /// `Metadata.BirthTime()` (`metadata.go:90`).
    #[must_use]
    pub fn birth_time(&self) -> Option<&str> {
        self.file_stat.birth_time.as_deref()
    }

    /// `Metadata.AudioProperties()` (`metadata.go:95`).
    #[must_use]
    pub fn audio_properties(&self) -> &nd_tagreader::AudioProperties {
        &self.audio_props
    }

    /// `Metadata.Length()` (`metadata.go:96`): `float32(duration.Milliseconds()) / 1000`.
    ///
    /// Go truncates the millisecond count to a `float32` before dividing, so
    /// long durations lose precision; the port reproduces that truncation
    /// rather than keeping full `f64` precision.
    #[must_use]
    pub fn length(&self) -> f32 {
        // Go: `float32(duration.Milliseconds()) / 1000`. The backend's `f64`
        // seconds become an int64 nanosecond count, then `Milliseconds()`
        // truncates to whole milliseconds; the float32 conversion loses
        // precision for long durations, which this reproduces rather than
        // keeping full `f64` precision.
        let nanos = (self.audio_props.duration * 1e9) as i64;
        ((nanos / 1_000_000) as f32) / 1000.0
    }

    /// `Metadata.HasPicture()` (`metadata.go:97`).
    #[must_use]
    pub fn has_picture(&self) -> bool {
        self.has_picture
    }

    /// `Metadata.All()` (`metadata.go:98`).
    #[must_use]
    pub fn all(&self) -> &Tags {
        &self.tags
    }

    /// `Metadata.Strings()` (`metadata.go:99`).
    #[must_use]
    pub fn strings(&self, name: &str) -> Vec<String> {
        self.tags.get(name).cloned().unwrap_or_default()
    }

    /// `Metadata.String()` (`metadata.go:100` via `first`, `:118-123`).
    #[must_use]
    pub fn string(&self, name: &str) -> String {
        self.first(name)
    }

    /// `Metadata.Int()` (`metadata.go:101`): unparseable values yield 0.
    #[must_use]
    pub fn int(&self, name: &str) -> i64 {
        parse_go_i64(&self.first(name)).unwrap_or(0)
    }

    /// `Metadata.Bool()` (`metadata.go:102`): `strconv.ParseBool` semantics,
    /// which accepts only `1`, `t`, `T`, `TRUE`, `true`, `True` as true; every
    /// other value (including `yes`/`on`) yields false with an error, and Go
    /// ignores the error.
    #[must_use]
    pub fn bool(&self, name: &str) -> bool {
        matches!(
            self.first(name).as_str(),
            "1" | "t" | "T" | "TRUE" | "true" | "True"
        )
    }

    /// `Metadata.Date()` (`metadata.go:103` via `date`, `:169-171`).
    #[must_use]
    pub fn date(&self, name: &str) -> String {
        self.first(name)
    }

    /// `Metadata.Float()` (`metadata.go:105-107` with `float`, `:125-134`).
    #[must_use]
    pub fn float(&self, name: &str, default: f64) -> f64 {
        self.nullable_float(name).unwrap_or(default)
    }

    /// `Metadata.NullableFloat()` (`metadata.go:108` via `nullableFloat`,
    /// `:136-142`): inf and NaN are rejected.
    #[must_use]
    pub fn nullable_float(&self, name: &str) -> Option<f64> {
        nullable_float(&self.first(name))
    }

    /// `Metadata.Gain()` (`metadata.go:110-113`): strips one `dB` anywhere in
    /// the value, then parses.
    #[must_use]
    pub fn gain(&self, name: &str) -> Option<f64> {
        let v = self.first(name).replace("dB", "");
        nullable_float(v.trim())
    }

    /// `Metadata.NumAndTotal()` (`metadata.go:104` via `tuple`, `:145-156`).
    ///
    /// The total falls back to `<name>total`; a two-part `n/total` overrides
    /// it. Values outside int32 or negative become 0 (`tagNumber`, `:158-165`).
    #[must_use]
    pub fn num_and_total(&self, name: &str) -> (i32, i32) {
        let tag = self.first(name);
        if tag.is_empty() {
            return (0, 0);
        }
        let parts: Vec<&str> = tag.split('/').collect();
        let mut total = self.first(&format!("{name}total"));
        if parts.len() > 1 {
            total = parts[1].to_string();
        }
        (tag_number(parts[0]), tag_number(&total))
    }

    /// `Metadata.Pairs()` (`metadata.go:114-117`).
    #[must_use]
    pub fn pairs(&self, name: &str) -> Vec<Pair> {
        self.tags
            .get(name)
            .map(|vs| vs.iter().map(|v| Pair(v.clone())).collect())
            .unwrap_or_default()
    }

    /// The mapping table in effect (Go reads the global `model.TagMappings()`).
    #[must_use]
    pub fn mappings(&self) -> &Mappings {
        self.ctx.mappings()
    }

    /// Options in effect.
    #[must_use]
    pub fn options(&self) -> &MappingOptions {
        self.ctx.options()
    }

    /// The full mapping context (options + table).
    #[must_use]
    pub fn context(&self) -> &MappingContext {
        &self.ctx
    }

    /// Artist-split-exception regex, if configured
    /// (`model/tag_mappings.go:145-153`).
    #[must_use]
    pub fn exceptions(&self) -> Option<&Regex> {
        self.exceptions.as_ref()
    }

    /// A process-wide empty metadata, for the tag-free `albumartistid` artist
    /// id path (`persistent_ids.go:124-127`).
    #[must_use]
    pub fn empty() -> &'static Metadata {
        static EMPTY: std::sync::OnceLock<Metadata> = std::sync::OnceLock::new();
        EMPTY.get_or_init(|| {
            Metadata::new(
                "",
                &empty_info(),
                FileStat::default(),
                &MappingContext::default(),
            )
        })
    }

    pub(crate) fn first(&self, name: &str) -> String {
        self.tags
            .get(name)
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_default()
    }
}

/// `metadata.clean` (`metadata.go:205-227`): lowercase raw keys, collect every
/// mapped tag's values in alias order, drop empty/duplicate values, and
/// sanitize each surviving value.
pub(crate) fn clean(
    file_path: &str,
    raw: &BTreeMap<String, Vec<String>>,
    table: &Mappings,
    exceptions: &Option<Regex>,
) -> Tags {
    let lowered = lower_tags(raw);
    let mut cleaned: Tags = BTreeMap::new();

    for (name, mapping) in table.all() {
        // `WithParticipantExceptions` (`model/tag_mappings.go:175-180`).
        let conf = mapping.with_participant_exceptions(name, exceptions.as_ref());
        let values = match conf.type_name {
            TagType::Pair => process_pair_mapping(name, &conf, &lowered),
            _ => process_regular_mapping(&conf, &lowered),
        };
        cleaned.insert(name.to_string(), values);
    }

    let cleaned = filter_empty_tags(cleaned);
    sanitize_all(file_path, &cleaned, table)
}

/// `lowerTags` (`metadata.go:240-246`).
fn lower_tags(raw: &BTreeMap<String, Vec<String>>) -> Tags {
    raw.iter()
        .map(|(k, v)| (k.to_lowercase(), v.clone()))
        .collect()
}

/// `processRegularMapping` (`metadata.go:229-238`): collect every alias's
/// values in alias order, splitting each.
fn process_regular_mapping(mapping: &TagConf, lowered: &Tags) -> Vec<String> {
    let mut values: Vec<String> = Vec::new();
    for alias in &mapping.aliases {
        if let Some(vs) = lowered.get(alias) {
            values.extend(mapping.split_tag_value(vs));
        }
    }
    values
}

/// `processPairMapping` (`metadata.go:248-272`).
fn process_pair_mapping(name: &str, mapping: &TagConf, lowered: &Tags) -> Vec<String> {
    let mut alias_values: Vec<String> = Vec::new();
    for alias in &mapping.aliases {
        if let Some(vs) = lowered.get(alias) {
            alias_values.extend(vs.iter().cloned());
        }
    }

    // Always parse ID3 pairs; TagLib always provides `lyrics:xxx`, which is
    // preferred over format-specific tags.
    let mut pairs = parse_id3_pairs(name, lowered);

    if !alias_values.is_empty() {
        if name == crate::tag_names::TAG_LYRICS.as_str() {
            // For lyrics, do not use parseVorbisPairs: parentheses in lyric
            // content must not be read as language keys.
            for v in &alias_values {
                pairs.push(new_pair("xxx", v));
            }
        } else {
            pairs.extend(parse_vorbis_pairs(&alias_values));
        }
    }
    pairs
}

/// `parseID3Pairs` (`metadata.go:274-290`): every `<name>:<key>` tag becomes a
/// pair; `<name>:<name>` yields an empty key.
fn parse_id3_pairs(name: &str, lowered: &Tags) -> Vec<String> {
    let prefix = format!("{name}:");
    let mut pairs = Vec::new();
    for (tag_key, tag_values) in lowered {
        if let Some(key_part) = tag_key.strip_prefix(&prefix) {
            let key_part = if key_part == name { "" } else { key_part };
            for v in tag_values {
                pairs.push(new_pair(key_part, v));
            }
        }
    }
    pairs
}

/// `parseVorbisPairs` (`metadata.go:292-315`): `"Name (role)"` becomes
/// `role -> Name`, with nested parentheses allowed in the role.
fn parse_vorbis_pairs(values: &[String]) -> Vec<String> {
    static RX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let rx = RX.get_or_init(|| {
        Regex::new(r"\(([^()]+(?:\([^()]*\)[^()]*)*)\)").expect("valid vorbis pair regex")
    });
    let mut pairs = Vec::with_capacity(values.len());
    for value in values {
        let Some(cap) = rx.captures(value) else {
            pairs.push(new_pair("", value));
            continue;
        };
        let inner = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let key = inner.trim().to_lowercase();
        let whole = cap.get(0).map(|m| m.as_str()).unwrap_or("");
        // `strings.Replace(value, "("+match+")", "", 1)` — the first occurrence.
        let value_without_key = value.replacen(whole, "", 1);
        pairs.push(new_pair(&key, value_without_key.trim()));
    }
    pairs
}

/// `filterEmptyTags` (`metadata.go:317-327`).
fn filter_empty_tags(tags: Tags) -> Tags {
    let mut out: Tags = BTreeMap::new();
    for (k, v) in tags {
        let clean = filter_duplicated_or_empty(v);
        if !clean.is_empty() {
            out.insert(k, clean);
        }
    }
    out
}

/// `filterDuplicatedOrEmptyValues` (`metadata.go:329-343`).
fn filter_duplicated_or_empty(values: Vec<String>) -> Vec<String> {
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

/// `sanitizeAll` (`metadata.go:345-365`): drop unknown tags and empty values.
fn sanitize_all(file_path: &str, tags: &Tags, table: &Mappings) -> Tags {
    let mut cleaned: Tags = BTreeMap::new();
    for (k, values) in tags {
        let Some(conf) = table.all().get(k) else {
            continue;
        };
        let mut out = Vec::new();
        for value in values {
            let cleaned_value = sanitize(file_path, k, conf, value);
            if !cleaned_value.is_empty() {
                out.push(cleaned_value);
            }
        }
        if !out.is_empty() {
            cleaned.insert(k.clone(), out);
        }
    }
    cleaned
}

/// `sanitize` (`metadata.go:369-414`): truncate to `MaxLength` (default 1024)
/// without splitting a multi-byte rune, then validate/coerce by type.
pub(crate) fn sanitize(file_path: &str, tag_name: &str, tag: &TagConf, value: &str) -> String {
    let max_length = if tag.max_length == 0 {
        crate::config::DEFAULT_MAX_TAG_LENGTH
    } else {
        tag.max_length
    };
    let mut value = truncate_to_bytes(value, max_length);

    match tag.type_name {
        TagType::Date => {
            value = parse_date(file_path, tag_name, &value);
        }
        TagType::Integer => {
            if parse_go_i64(&value).is_none() {
                return String::new();
            }
        }
        TagType::Float => {
            if value.parse::<f64>().is_err() {
                return String::new();
            }
        }
        TagType::Uuid => {
            match crate::uuid::parse(&value) {
                // Go stores the canonical form: `uuid.Parse` accepts braces,
                // `urn:` prefixes and two-byte wrappers, and a wrapped value
                // would never match an exact-match query (`metadata.go:409-411`).
                Ok(u) => value = u.to_string(),
                Err(_) => return String::new(),
            }
        }
        TagType::String | TagType::Pair => {}
    }
    value
}

/// Go's byte-wise truncation plus the trailing-rune cleanup
/// (`metadata.go:370-383`): cut at `max` **bytes**, then drop up to 3 trailing
/// bytes that decode as an incomplete rune (`utf8.DecodeLastRuneInString`
/// returning `RuneError` with size 1). A pre-existing invalid byte sequence
/// elsewhere in the value is never consumed, and bytes before the cut are kept
/// exactly as Go keeps them.
fn truncate_to_bytes(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut cut = max;
    let bytes = value.as_bytes();
    // `utf8.DecodeLastRuneInString` on the truncated string: invalid trailing
    // bytes report RuneError with size 1, so drop them one at a time.
    for _ in 0..3 {
        match trailing_incomplete_len(&bytes[..cut]) {
            Some(1) => cut -= 1,
            _ => break,
        }
    }
    // Go keeps the remaining bytes even if they are not valid UTF-8 (a string
    // can hold invalid bytes); the port stores `String`, so the invalid tail is
    // dropped rather than replaced.
    match std::str::from_utf8(&bytes[..cut]) {
        Ok(v) => v.to_string(),
        Err(e) => String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned(),
    }
}

/// `Some(1)` when the last byte of `s` starts an incomplete (or invalid) UTF-8
/// sequence, mirroring Go's `RuneError`+size-1 result.
fn trailing_incomplete_len(s: &[u8]) -> Option<usize> {
    if std::str::from_utf8(s).is_ok() {
        return None;
    }
    // The tail is invalid: the last rune occupies at least one byte.
    Some(1)
}

/// `parseDate` (`metadata.go:173-203`). Go logs the failures; the port keeps
/// the same return values.
pub(crate) fn parse_date(file_path: &str, tag_name: &str, tag_value: &str) -> String {
    let _ = (file_path, tag_name);
    if tag_value.chars().count() < 4 {
        return String::new();
    }
    static RX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let rx = RX.get_or_init(|| Regex::new(r"([12]\d\d\d)").expect("valid date regex"));
    let Some(year) = rx.find(tag_value).map(|m| m.as_str().to_string()) else {
        return String::new();
    };
    if tag_value.chars().count() < 5 {
        return year;
    }
    let truncated: String = tag_value.chars().take(10).collect();
    for mask in ["%Y-%m-%d", "%Y-%m"] {
        if go_parse_date(mask, &truncated).is_some() {
            return truncated;
        }
    }
    year
}

/// Minimal `time.Parse` for the two layouts Go tries (`model/metadata/metadata.go:193-205`,
/// layouts `2006-01-02` and `2006-01`). Returns the parsed date only when it is
/// a real calendar date, as Go does (e.g. `2004-00-00` fails).
///
/// `mask` selects which layout Go would validate against, but both layouts
/// accept the same shapes here: a 3-component `YYYY-MM-DD` parses under the
/// date layout and rejects the extra field under the year-month layout, and a
/// 2-component `YYYY-MM` parses under the year-month layout with Go defaulting
/// the day to 1. The one difference Go would show — a 3-component value under
/// `2006-01` — never reaches a successful parse in either branch, so the loop
/// result is identical and the mask does not change the outcome.
fn go_parse_date(_mask: &str, value: &str) -> Option<chrono::NaiveDate> {
    let mut parts = value.split('-');
    let year = parts.next()?.parse::<i32>().ok()?;
    let month = parts.next()?.parse::<u32>().ok()?;
    let day = match parts.next() {
        Some(d) => d.parse::<u32>().ok()?,
        None => 1,
    };
    if parts.next().is_some() {
        return None;
    }
    chrono::NaiveDate::from_ymd_opt(year, month, day)
}

/// `nullableFloat` (`metadata.go:136-142`): rejects inf and NaN.
fn nullable_float(value: &str) -> Option<f64> {
    let v = value.parse::<f64>().ok()?;
    if v.is_infinite() || v.is_nan() {
        return None;
    }
    Some(v)
}

/// `tagNumber` (`metadata.go:158-165`): `strconv.ParseInt(s, 10, 32)` — so
/// anything outside int32 or negative yields 0, keeping the DB readable by
/// 32-bit builds.
pub(crate) fn tag_number(s: &str) -> i32 {
    match parse_go_i64(s) {
        Some(v) if (0..=i32::MAX as i64).contains(&v) => v as i32,
        _ => 0,
    }
}

/// `number.ParseInt[int32]` (`utils/number/number.go`): `strconv.ParseInt` at
/// 64 bits, then a Go numeric conversion to `int32`, which truncates rather
/// than rejecting. Unparseable yields 0 (the error is dropped).
pub(crate) fn parse_int32_lossy(s: &str) -> i32 {
    match parse_go_i64(s) {
        Some(v) => v as i32,
        None => 0,
    }
}

/// `strconv.Atoi` with Go's error-ignored int conversion: out-of-range values
/// saturate instead of failing, matching `mapGain`'s `strconv.Atoi` + `int`
/// (`model/metadata/map_mediafile.go:128-133`).
pub(crate) fn parse_go_i64_saturating(s: &str) -> Option<i64> {
    let (sign, digits) = split_sign_digits(s)?;
    let mut v: i64 = 0;
    for b in digits.bytes() {
        let d = i64::from(b - b'0');
        v = v.saturating_mul(10).saturating_add(d);
    }
    Some(if sign < 0 { -v } else { v })
}

/// Splits an optional `+`/`-` sign from a run of ASCII digits.
fn split_sign_digits(s: &str) -> Option<(i8, &str)> {
    let (sign, digits) = match s.as_bytes().first() {
        Some(b'+') => (1, &s[1..]),
        Some(b'-') => (-1, &s[1..]),
        _ => (1, s),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((sign, digits))
}

/// `strconv.Atoi`: optional sign, ASCII digits only, no separators or
/// surrounding whitespace; out-of-range is an error.
pub(crate) fn parse_go_i64(s: &str) -> Option<i64> {
    let (sign, digits) = match s.as_bytes().first() {
        Some(b'+') => (1, &s[1..]),
        Some(b'-') => (-1, &s[1..]),
        _ => (1, s),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v: u64 = digits.parse().ok()?;
    let v = i64::try_from(v).ok()?;
    Some(v * sign)
}
