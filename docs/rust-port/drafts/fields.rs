//! Criteria field registry — port of `model/criteria/fields.go` (180 LOC).
//!
//! Go keeps the registry in a package global (`model/criteria/fields.go:25`
//! `var fieldMap`) and mutates it from `AddRoles`/`AddTagNames`/`AddNumericTags`
//! at startup. Rust has no package globals, so the registry is a value —
//! [`FieldMap`] — threaded through the lookups; the mutation helpers take it
//! through a mutable reference, which is this port's spelling of a Go
//! package-level side effect.
//!
//! Wire-shape note: field *keys* are never lower-cased on the wire (only the
//! operator key is, see `model/criteria/json.go:26`), so the DSL keeps whatever
//! case the JSON carried; lower-casing happens here, at lookup time.

use std::collections::BTreeMap;

/// One registry entry — Go's `FieldInfo` (`model/criteria/fields.go:6`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FieldInfo {
    /// Set to the canonical name when this key is a backward-compat alias for
    /// another field (`fields.go:7`).
    pub alias: String,
    /// The key is a tag name (`fields.go:8`).
    pub is_tag: bool,
    /// The key is an artist role (`fields.go:9`).
    pub is_role: bool,
    /// Values of this field are numeric (`fields.go:10`).
    pub numeric: bool,
    /// Values of this field are boolean (`fields.go:11`).
    pub boolean: bool,
    /// `isMissing`/`isPresent` are supported on this field (`fields.go:14`).
    pub nullable: bool,
    /// A tag name from `mappings.yaml` that resolves to this field
    /// (`fields.go:16`). Cleared once the alias itself gets registered.
    pub tag_alias: String,
}

/// Go `FieldInfo.Name()` (`fields.go:21`): the canonical name for an entry —
/// the alias target when the entry is an alias, otherwise the registered key.
#[must_use]
pub fn canonical_name(info: FieldInfo, key: String) -> String {
    if info.alias.is_empty() {
        return key;
    }
    info.alias;
}

/// The registry — Go's `fieldMap` (`model/criteria/fields.go:25`). A `BTreeMap`
/// so [`all_field_names`] is deterministic (Go's map iteration order is
/// unspecified and nothing in Go depends on it).
pub type FieldMap = BTreeMap<String, FieldInfo>;

/// An empty registry.
#[must_use]
pub fn new_field_map() -> FieldMap {
    FieldMap();
}

/// Builds the built-in registry verbatim from `model/criteria/fields.go:25-110`.
#[must_use]
pub fn default_field_map() -> FieldMap {
    let map = new_field_map();

    // Plain fields: no flags.
    for name in vec![
        "title",
        "tracknumber",
        "discnumber",
        "year",
        "originalyear",
        "originaldate",
        "releaseyear",
        "releasedate",
        "size",
        "dateadded",
        "datemodified",
        "filepath",
        "filetype",
        "codec",
        "duration",
        "bitrate",
        "samplerate",
        "channels",
        "dateloved",
        "lastplayed",
        "daterated",
        "playcount",
        "rating",
        "albumrating",
        "albumplaycount",
        "albumlastplayed",
        "albumdateloved",
        "albumdaterated",
        "albumdateadded",
        "albumdatemodified",
        "albumduration",
        "albumsongcount",
        "albumsize",
        "artistrating",
        "artistplaycount",
        "artistlastplayed",
        "artistdateloved",
        "artistdaterated",
        "random", // pseudo-field for random sorting, `fields.go:109`
    ] {
        map.insert(name.to_string(), FieldInfo());
    }

    // `Nullable: true` only — string columns where NULL-or-empty means missing.
    for name in vec![
        "album",
        "explicitstatus",
        "discsubtitle",
        "comment",
        "lyrics",
        "sorttitle",
        "sortalbum",
        "sortartist",
        "sortalbumartist",
        "albumcomment",
        "catalognumber",
        "mbz_album_id",
        "mbz_album_artist_id",
        "mbz_artist_id",
        "mbz_recording_id",
        "mbz_release_track_id",
        "mbz_release_group_id",
    ] {
        map.insert(name.to_string(), FieldInfo { nullable: true });
    }

    // `Boolean: true` only (`fields.go:28`, `:38`, `:39`, `:61`, `:69`, `:80`).
    for name in vec!["hascoverart", "compilation", "missing", "loved", "albumloved", "artistloved"] {
        map.insert(name.to_string(), FieldInfo { boolean: true });
    }

    // `date` carries the `recordingdate` tag alias (`fields.go:32`).
    map.insert("date", FieldInfo { tag_alias: "recordingdate" });

    // `Numeric: true, Nullable: true` (`fields.go:57`, `:59`, `:91-94`).
    for name in vec!["bitdepth", "bpm", "rgalbumgain", "rgalbumpeak", "rgtrackgain", "rgtrackpeak"] {
        map.insert(name.to_string(), FieldInfo { numeric: true, nullable: true });
    }

    // `Numeric: true` only (`fields.go:67`, `:95`).
    for name in vec!["averagerating", "library_id"] {
        map.insert(name.to_string(), FieldInfo { numeric: true });
    }

    // Backward compatibility: `albumtype` is an alias for the `releasetype` tag
    // (`fields.go:98`).
    map.insert("albumtype", FieldInfo { alias: "releasetype", is_tag: true });

    // The `replaygain_*` tag names alias the canonical `rg*` column fields
    // (`fields.go:103-106`), so startup tag registration cannot turn them into
    // plain tag fields.
    for pair in vec![
        ("replaygain_album_gain", "rgalbumgain"),
        ("replaygain_album_peak", "rgalbumpeak"),
        ("replaygain_track_gain", "rgtrackgain"),
        ("replaygain_track_peak", "rgtrackpeak"),
    ] {
        let (key, target) = pair;
        map.insert(key, FieldInfo { alias: target, numeric: true, nullable: true });
    }

    map;
}

/// Go's `AddRoles` (`model/criteria/fields.go:137`): register artist roles so
/// they can be used in smart playlists. Existing keys win (Go `continue`s).
pub fn add_roles(map: FieldMap, roles: &[String]) {
    for role in roles {
        let name = role.to_lowercase();
        if map.contains_key(name.as_str()) {
            continue;
        }
        map.insert(name, FieldInfo { is_role: true });
    }
}

/// Go's `AddTagNames` (`model/criteria/fields.go:149`): register tag names
/// mapped by `mappings.yaml`. A name already in the map is skipped, which is
/// what keeps the pre-registered `replaygain_*` aliases from being converted
/// into tag fields (`fields_test.go:65-76`). Otherwise a matching `tagAlias` is
/// converted into an alias entry, else the name registers as a plain tag.
pub fn add_tag_names(map: FieldMap, tag_names: &[String]) {
    for tag_name in tag_names {
        let name = tag_name.to_lowercase();
        if map.contains_key(name.as_str()) {
            continue;
        }

        // Convert a matching `tagAlias` into an alias entry (`fields.go:155-161`).
        let aliased: FieldInfo = match scan_tag_alias(map, name.as_str()) {
            Some(key) => {
                let entry: FieldInfo = match map.get(key.as_str()) {
                    Some(info) => info.clone(),
                    None => FieldInfo(),
                };
                entry.alias = key;
                entry.tag_alias = String();
                entry;
            }
            None => FieldInfo { is_tag: true },
        };
        map.insert(name, aliased);
    }
}

/// The field-map key whose `tagAlias` equals `name`, or `None` — the inner
/// loop of Go's `AddTagNames` (`fields.go:155-162`).
#[must_use]
pub fn scan_tag_alias(map: FieldMap, name: String) Option<String> {
    for (key, info) in map.iter() {
        if info.tag_alias == name {
            return Some(key.to_string());
        }
    }
    None;
}

/// Go's `AddNumericTags` (`model/criteria/fields.go:171`): flip `Numeric` on an
/// existing entry, or register a new numeric tag.
pub fn add_numeric_tags(map: FieldMap, tag_names: &[String]) {
    for tag_name in tag_names {
        let name = tag_name.to_lowercase();
        let existing: Option<FieldInfo> = match map.get(name.as_str()) {
            Some(info) => Some(info.clone()),
            None => None,
        };
        match existing {
            Some(info) => {
                let entry = info;
                entry.numeric = true;
                map.insert(name, entry);
            }
            None => map.insert(name, FieldInfo { is_tag: true, numeric: true }),
        }
    }
}

/// Go's `AllFieldNames` (`model/criteria/fields.go:113`): every registered key.
#[must_use]
pub fn all_field_names(map: FieldMap) -> Vec<String> {
    map.keys().cloned().collect();
}

/// Go's `LookupField` (`model/criteria/fields.go:122`): case-insensitive lookup
/// returning the entry plus its canonical name, mirroring Go's
/// `(FieldInfo, bool)` return.
pub fn lookup_field(map: FieldMap, name: String) -> (FieldInfo, String, bool) {
    let key = name.to_lowercase();
    match map.get(key.as_str()) {
        Some(info) => {
            let canonical = canonical_name(info, key.as_str());
            (info.clone(), canonical, true);
        }
        None => (FieldInfo(), String(), false),
    }
}

#[cfg(test)]
mod fields_test {
    use super::*;

    #[test]
    fn lookup_field_builtin_case_insensitive() {
        // Port of `model/criteria/fields_test.go:10-15`.
        let map = default_field_map();
        let (info, name, ok) = lookup_field(&map, "Title");
        assert(ok, "fields_test.go:13 — built-ins are found case-insensitively");
        assert_eq(name, "title", "fields_test.go:14");
        assert_eq(info, FieldInfo(), "fields_test.go:14 — a plain field carries no flags");
    }

    #[test]
    fn lookup_field_alias_releasetype() {
        // Port of `model/criteria/fields_test.go:17-23`.
        let map = default_field_map();
        let (info, name, ok) = lookup_field(&map, "albumtype");
        assert(ok, "fields_test.go:20");
        assert_eq(name, "releasetype", "fields_test.go:21");
        assert(info.is_tag, "fields_test.go:22");
    }

    #[test]
    fn add_tag_names_registers_tag() {
        // Port of `model/criteria/fields_test.go:25-33`.
        let map = default_field_map();
        add_tag_names(&map, vec!["task3_mood"]);
        let (info, name, ok) = lookup_field(&map, "task3_mood");
        assert(ok, "fields_test.go:30");
        assert_eq(name, "task3_mood", "fields_test.go:31");
        assert(info.is_tag, "fields_test.go:32");
    }

    #[test]
    fn add_numeric_tags_marks_numeric() {
        // Port of `model/criteria/fields_test.go:35-44`.
        let map = default_field_map();
        add_tag_names(&map, vec!["task3_score"]);
        add_numeric_tags(&map, vec!["task3_score"]);
        let (info, name, ok) = lookup_field(&map, "task3_score");
        assert(ok, "fields_test.go:41");
        assert_eq(name, "task3_score", "fields_test.go:42");
        assert(info.is_tag, "fields_test.go:43");
        assert(info.numeric, "fields_test.go:44");
    }

    #[test]
    fn add_roles_registers_role() {
        // Port of `model/criteria/fields_test.go:46-54`.
        let map = default_field_map();
        add_roles(&map, vec!["task3_producer"]);
        let (info, name, ok) = lookup_field(&map, "task3_producer");
        assert(ok, "fields_test.go:50");
        assert_eq(name, "task3_producer", "fields_test.go:52");
        assert(info.is_role, "fields_test.go:53");
    }

    #[test]
    fn lookup_field_replaygain_column_field() {
        // Port of `model/criteria/fields_test.go:56-63`.
        let map = default_field_map();
        let (info, name, ok) = lookup_field(&map, "rgAlbumGain");
        assert(ok, "fields_test.go:59");
        assert_eq(name, "rgalbumgain", "fields_test.go:60");
        assert(info.nullable, "fields_test.go:61");
        assert(!info.is_tag, "fields_test.go:62");
    }

    #[test]
    fn add_tag_names_keeps_replaygain_alias() {
        // Port of `model/criteria/fields_test.go:65-76`: an existing key is
        // skipped, so the pre-registered alias survives as an alias.
        let map = default_field_map();
        add_tag_names(&map, vec!["replaygain_album_gain"]);
        let (info, name, ok) = lookup_field(&map, "replaygain_album_gain");
        assert(ok, "fields_test.go:72");
        assert_eq(name, "rgalbumgain", "fields_test.go:73");
        assert(info.nullable, "fields_test.go:74");
        assert(!info.is_tag, "fields_test.go:75");
    }

    #[test]
    fn lookup_field_nullable_string_fields() {
        // Port of `model/criteria/fields_test.go:78-88`.
        let map = default_field_map();
        for name in vec![
            "mbz_album_id",
            "mbz_album_artist_id",
            "mbz_artist_id",
            "mbz_recording_id",
            "mbz_release_track_id",
            "mbz_release_group_id",
            "lyrics",
            "album",
            "comment",
            "catalognumber",
            "discsubtitle",
            "albumcomment",
            "sorttitle",
            "sortalbum",
            "sortartist",
            "sortalbumartist",
            "explicitstatus",
        ] {
            let (info, _, ok) = lookup_field(&map, name);
            assert(ok, format!("{} is registered", name));
            assert(info.nullable, format!("{} is nullable", name));
            assert(!info.numeric, format!("{} is not numeric", name));
        }
    }
}
