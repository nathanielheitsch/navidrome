//! Port of `core/storage/storage_test.go` + `core/storage/local/extractors`
//! dispatch contract. Registry is a process-global static (Go tests reset the
//! map; here each test uses its own schema name so registrations don't clash).

use nd_tagreader::extractors::{get_extractor, register_extractor, Extractor};
use nd_tagreader::storage::{local_path_to_url, register, storage_for, Storage, LOCAL_SCHEMA_ID};

/// Identity constructor — Go registers `func(url) Storage { return &fake… }`.
fn make_storage(scheme: String, path: String) -> Storage {
    Storage { scheme, path }
}

fn get_ctor(fs_root: String, base_dir: String) -> Extractor {
    Extractor { fs_root, base_dir }
}

#[test]
fn unregistered_schema_errors() {
    let miss = storage_for("zz-missing://tmp");
    match miss {
        Err(e) => assert_eq!(e.to_string(), "schema 'zz-missing' not registered"),
        Ok(_) => panic!("unregistered schema must error"),
    }
}

#[test]
fn registered_schema_dispatches_to_constructor() {
    register("zz-unit-file", make_storage);
    let s = storage_for("zz-unit-file:///tmp").unwrap();
    assert_eq!(s.scheme, "zz-unit-file");
    assert_eq!(s.path, "/tmp");
}

#[test]
fn bare_path_matches_local_path_to_url() {
    register(LOCAL_SCHEMA_ID, make_storage);
    let via_for = storage_for("/tmp/music").unwrap();
    let direct = local_path_to_url("/tmp/music");
    assert_eq!(via_for.scheme, direct.scheme);
    assert_eq!(via_for.path, direct.path);
}

#[test]
fn escape_roundtrip_golden_entries() {
    // storage_test.go table: hash, spaces, question marks, ampersands, mixed.
    let inputs: &[&str] = &[
        "/tmp/test#folder/file.mp3",
        "/tmp/test folder/file with spaces.mp3",
        "/tmp/test?query/file.mp3",
        "/tmp/test&amp/file.mp3",
        "/tmp/Song #1 & More?.mp3",
    ];
    for input in inputs.iter() {
        let u = local_path_to_url(input);
        assert_eq!(
            u.path,
            input.to_string(),
            "Path == input (Go url.Parse decodes back)"
        );
    }
}

#[test]
fn extractor_registry_dispatch() {
    register_extractor("taglib", get_ctor);
    assert!(get_extractor("taglib").is_ok(), "registered id found");
    assert!(get_extractor("zz-nope").is_err(), "missing id errors");
}
