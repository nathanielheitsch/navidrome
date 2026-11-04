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
/// Go stores the resolved name in `FieldInfo.name` (`fields.go:17`, filled by
/// `LookupField`); the port derives it instead, so no entry can carry a stale
/// name.
#[must_use]
pub fn canonical_name(info: FieldInfo, key: String) -> String {
    if info.alias.is_empty() {
        return key;
    }
    info.alias
}

/// The registry — Go's `fieldMap` (`model/criteria/fields.go:25`). A `BTreeMap`
/// so [`all_field_names`] is deterministic (Go's map iteration order is
/// unspecified and nothing in Go depends on it).
pub type FieldMap = BTreeMap<String, FieldInfo>;

/// An empty registry.
#[must_use]
pub fn new_field_map() -> FieldMap {
    FieldMap::new()
}

/// Builds the built-in registry verbatim from `model/criteria/fields.go:25-110`.
#[must_use]
pub fn default_field_map() -> FieldMap {
    let mut map = new_field_map();

    // Plain fields: no flags.
    for name in [
        "title",
        // `album` is Nullable (fields.go:27), registered in the block below.
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
        map.insert(name.to_string(), FieldInfo::default());
    }

    // `Nullable: true` only — string columns where NULL-or-empty means missing.
    for name in [
        "album", // fields.go:27 {Nullable: true}
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
        map.insert(
            name.to_string(),
            FieldInfo {
                nullable: true,
                ..FieldInfo::default()
            },
        );
    }

    // `Boolean: true` only (`fields.go:28`, `:38`, `:39`, `:61`, `:69`, `:80`).
    for name in [
        "hascoverart",
        "compilation",
        "missing",
        "loved",
        "albumloved",
        "artistloved",
    ] {
        map.insert(
            name.to_string(),
            FieldInfo {
                boolean: true,
                ..FieldInfo::default()
            },
        );
    }

    // `date` carries the `recordingdate` tag alias (`fields.go:32`).
    map.insert(
        "date".to_string(),
        FieldInfo {
            tag_alias: "recordingdate".to_string(),
            ..FieldInfo::default()
        },
    );

    // `Numeric: true, Nullable: true` (`fields.go:57`, `:59`, `:91-94`).
    for name in [
        "bitdepth",
        "bpm",
        "rgalbumgain",
        "rgalbumpeak",
        "rgtrackgain",
        "rgtrackpeak",
    ] {
        map.insert(
            name.to_string(),
            FieldInfo {
                numeric: true,
                nullable: true,
                ..FieldInfo::default()
            },
        );
    }

    // `Numeric: true` only (`fields.go:67`, `:95`).
    for name in ["averagerating", "library_id"] {
        map.insert(
            name.to_string(),
            FieldInfo {
                numeric: true,
                ..FieldInfo::default()
            },
        );
    }

    // Backward compatibility: `albumtype` is an alias for the `releasetype` tag
    // (`fields.go:98`).
    map.insert(
        "albumtype".to_string(),
        FieldInfo {
            alias: "releasetype".to_string(),
            is_tag: true,
            ..FieldInfo::default()
        },
    );

    // The `replaygain_*` tag names alias the canonical `rg*` column fields
    // (`fields.go:103-106`), so startup tag registration cannot turn them into
    // plain tag fields.
    for (key, target) in [
        ("replaygain_album_gain", "rgalbumgain"),
        ("replaygain_album_peak", "rgalbumpeak"),
        ("replaygain_track_gain", "rgtrackgain"),
        ("replaygain_track_peak", "rgtrackpeak"),
    ] {
        map.insert(
            key.to_string(),
            FieldInfo {
                alias: target.to_string(),
                numeric: true,
                nullable: true,
                ..FieldInfo::default()
            },
        );
    }

    map
}

/// Go's `AddRoles` (`model/criteria/fields.go:137`): register artist roles so
/// they can be used in smart playlists. Existing keys win (Go `continue`s).
pub fn add_roles(map: &mut FieldMap, roles: &[String]) {
    for role in roles {
        let name: String = role.to_lowercase();
        if map.contains_key(&name) {
            continue;
        }
        map.insert(
            name,
            FieldInfo {
                is_role: true,
                ..FieldInfo::default()
            },
        );
    }
}

/// Go's `AddTagNames` (`model/criteria/fields.go:149`): register tag names
/// mapped by `mappings.yaml`. A name already in the map is skipped, which is
/// what keeps the pre-registered `replaygain_*` aliases from being converted
/// into tag fields (`fields_test.go:65-76`). Otherwise a matching `tagAlias` is
/// converted into an alias entry, else the name registers as a plain tag.
pub fn add_tag_names(map: &mut FieldMap, tag_names: &[String]) {
    for tag_name in tag_names {
        let name: String = tag_name.to_lowercase();
        if map.contains_key(&name) {
            continue;
        }

        // Convert a matching `tagAlias` into an alias entry (`fields.go:155-161`).
        // Go mutates the *iterated value copy* (`fm.Alias = key`), so the entry
        // that carries the `tagAlias` keeps its own flags untouched; the port
        // clones it the same way.
        match scan_tag_alias(map, name.to_string()) {
            Some(key) => {
                let mut entry: FieldInfo = match map.get(&key) {
                    Some(info) => info.clone(),
                    None => FieldInfo::default(),
                };
                entry.alias = key;
                entry.tag_alias = String::new();
                map.insert(name, entry);
            }
            None => {
                map.insert(
                    name,
                    FieldInfo {
                        is_tag: true,
                        ..FieldInfo::default()
                    },
                );
            }
        }
    }
}

/// The field-map key whose `tag_alias` equals `name`, or `None` — the inner
/// loop of Go's `AddTagNames` (`fields.go:155-162`).
#[must_use]
pub fn scan_tag_alias(map: &FieldMap, name: String) -> Option<String> {
    for (key, info) in map.iter() {
        if info.tag_alias == name {
            return Some(key.to_string());
        }
    }
    None
}

/// Go's `AddNumericTags` (`model/criteria/fields.go:171`): flip `Numeric` on an
/// existing entry, or register a new numeric tag.
pub fn add_numeric_tags(map: &mut FieldMap, tag_names: &[String]) {
    for tag_name in tag_names {
        let name: String = tag_name.to_lowercase();
        let existing: Option<FieldInfo> = map.get(&name).cloned();
        match existing {
            Some(info) => {
                let mut entry = info;
                entry.numeric = true;
                map.insert(name, entry);
            }
            None => {
                map.insert(
                    name,
                    FieldInfo {
                        is_tag: true,
                        numeric: true,
                        ..FieldInfo::default()
                    },
                );
            }
        }
    }
}

/// Go's `AllFieldNames` (`model/criteria/fields.go:113`): every registered key.
#[must_use]
pub fn all_field_names(map: &FieldMap) -> Vec<String> {
    map.keys().cloned().collect()
}

/// Go's `LookupField` (`model/criteria/fields.go:122`): case-insensitive lookup
/// returning the entry plus its canonical name, mirroring Go's
/// `(FieldInfo, bool)` return — Go's `FieldInfo.name` (`fields.go:17`) becomes
/// the second element here.
pub fn lookup_field(map: &FieldMap, name: String) -> (FieldInfo, String, bool) {
    let key: String = name.to_lowercase();
    match map.get(&key) {
        Some(info) => {
            let canonical: String = canonical_name(info.clone(), key);
            (info.clone(), canonical, true)
        }
        None => (FieldInfo::default(), String::new(), false),
    }
}

#[cfg(test)]
mod fields_test {
    use super::*;

    #[test]
    fn lookup_field_builtin_case_insensitive() {
        // Arrange: port of `model/criteria/fields_test.go:10-15`.
        let map = default_field_map();
        // Act:
        let (info, name, ok) = lookup_field(&map, "Title".to_string());
        // Assert
        assert!(
            ok,
            "fields_test.go:13 — built-ins are found case-insensitively"
        );
        assert_eq!(name, "title", "fields_test.go:14");
        assert_eq!(
            info,
            FieldInfo::default(),
            "fields_test.go:14 — a plain field carries no flags",
        );
    }

    #[test]
    fn lookup_field_alias_releasetype() {
        // Arrange: port of `model/criteria/fields_test.go:17-23`.
        let map = default_field_map();
        // Act:
        let (info, name, ok) = lookup_field(&map, "albumtype".to_string());
        // Assert
        assert!(ok, "fields_test.go:20");
        assert_eq!(name, "releasetype", "fields_test.go:21");
        assert!(info.is_tag, "fields_test.go:22");
    }

    #[test]
    fn add_tag_names_registers_tag() {
        // Arrange: port of `model/criteria/fields_test.go:25-33`.
        let mut map = default_field_map();
        // Act:
        add_tag_names(&mut map, &["task3_mood".to_string()]);
        let (info, name, ok) = lookup_field(&map, "task3_mood".to_string());
        // Assert
        assert!(ok, "fields_test.go:30");
        assert_eq!(name, "task3_mood", "fields_test.go:31");
        assert!(info.is_tag, "fields_test.go:32");
    }

    #[test]
    fn add_numeric_tags_marks_numeric() {
        // Arrange: port of `model/criteria/fields_test.go:35-44`.
        let mut map = default_field_map();
        // Act:
        add_tag_names(&mut map, &["task3_score".to_string()]);
        add_numeric_tags(&mut map, &["task3_score".to_string()]);
        let (info, name, ok) = lookup_field(&map, "task3_score".to_string());
        // Assert
        assert!(ok, "fields_test.go:41");
        assert_eq!(name, "task3_score", "fields_test.go:42");
        assert!(info.is_tag, "fields_test.go:43");
        assert!(info.numeric, "fields_test.go:44");
    }

    #[test]
    fn add_roles_registers_role() {
        // Arrange: port of `model/criteria/fields_test.go:46-54`.
        let mut map = default_field_map();
        // Act:
        add_roles(&mut map, &["task3_producer".to_string()]);
        let (info, name, ok) = lookup_field(&map, "task3_producer".to_string());
        // Assert
        assert!(ok, "fields_test.go:50");
        assert_eq!(name, "task3_producer", "fields_test.go:52");
        assert!(info.is_role, "fields_test.go:53");
    }

    #[test]
    fn lookup_field_replaygain_column_field() {
        // Arrange: port of `model/criteria/fields_test.go:56-63`.
        let map = default_field_map();
        // Act:
        let (info, name, ok) = lookup_field(&map, "rgAlbumGain".to_string());
        // Assert
        assert!(ok, "fields_test.go:59");
        assert_eq!(name, "rgalbumgain", "fields_test.go:60");
        assert!(info.nullable, "fields_test.go:61");
        assert!(!info.is_tag, "fields_test.go:62");
    }

    #[test]
    fn add_tag_names_keeps_replaygain_alias() {
        // Arrange: port of `model/criteria/fields_test.go:65-76`: an existing
        // key is skipped, so the pre-registered alias survives as an alias.
        let mut map = default_field_map();
        // Act:
        add_tag_names(&mut map, &["replaygain_album_gain".to_string()]);
        let (info, name, ok) = lookup_field(&map, "replaygain_album_gain".to_string());
        // Assert
        assert!(ok, "fields_test.go:72");
        assert_eq!(name, "rgalbumgain", "fields_test.go:73");
        assert!(info.nullable, "fields_test.go:74");
        assert!(!info.is_tag, "fields_test.go:75");
    }

    #[test]
    fn lookup_field_nullable_string_fields() {
        // Arrange: port of `model/criteria/fields_test.go:78-88`.
        let map = default_field_map();
        // Act:
        for name in [
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
            let (info, _, ok) = lookup_field(&map, name.to_string());
            // Assert
            assert!(ok, "{name} is registered");
            assert!(info.nullable, "{name} is nullable");
            assert!(!info.numeric, "{name} is not numeric");
        }
    }
}
