//! Authenticated Go-native song corpus integrity and non-vacuity gate.
//!
//! The Go harness is the capture oracle. This test independently checks the tracked artifact,
//! hashes the build inputs it records (including embedded migrations), and has an in-memory
//! broken-capture control. It never launches Go, accesses a listener, or mutates fixtures.
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

const CASES: [&str; 8] = [
    "regular_user_granted_success",
    "same_record_visible_authorized",
    "song_detail_authorized",
    "song_detail_authorized_control",
    "song_detail_denied_library",
    "song_detail_missing",
    "song_detail_no_session",
    "song_list_authorized",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    origin: String,
    authenticated: bool,
    production_binary_run: bool,
    listener_opened: bool,
    network_used: bool,
    scanner_produced_rows: usize,
    synthetic_identity: String,
    deterministic_transformations: Vec<String>,
    provenance: std::collections::BTreeMap<String, String>,
    cases: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Case {
    case_id: String,
    request_method: String,
    request_path: String,
    auth_mode: String,
    status: u16,
    response_headers: std::collections::BTreeMap<String, String>,
    body: String,
    body_length: usize,
    body_sha256: String,
    behavior: String,
}

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/native_song")
}

fn parse_manifest(dir: &Path) -> Result<Manifest, String> {
    let raw = fs::read(dir.join("manifest.json")).map_err(|e| e.to_string())?;
    serde_json::from_slice(&raw).map_err(|e| e.to_string())
}

fn load_case(dir: &Path, name: &str) -> Result<Case, String> {
    let raw =
        fs::read(dir.join("cases").join(format!("{name}.json"))).map_err(|e| e.to_string())?;
    let case: Case = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    if case.case_id != name {
        return Err(format!(
            "case filename {name:?} disagrees with embedded id {:?}",
            case.case_id
        ));
    }
    Ok(case)
}

fn validate_case(case: &Case) -> Result<(), String> {
    let expected = match case.case_id.as_str() {
        "song_list_authorized" => (200, "application/json"),
        "song_detail_authorized"
        | "song_detail_authorized_control"
        | "same_record_visible_authorized"
        | "regular_user_granted_success" => (200, "application/json"),
        "song_detail_missing" | "song_detail_denied_library" => (404, "application/json"),
        "song_detail_no_session" => (401, "application/json"),
        _ => return Err(format!("unknown case id {}", case.case_id)),
    };
    if case.status != expected.0 {
        return Err(format!("{}: unexpected status", case.case_id));
    }
    if case
        .response_headers
        .get("content-type")
        .map(String::as_str)
        != Some(expected.1)
    {
        return Err(format!("{}: unexpected content type", case.case_id));
    }
    if case.body_length != case.body.len() {
        return Err(format!("{}: body length mismatch", case.case_id));
    }
    if case.body_sha256 != hex(&Sha256::digest(case.body.as_bytes())) {
        return Err(format!("{}: body digest mismatch", case.case_id));
    }
    match case.case_id.as_str() {
        "song_detail_no_session" => {
            let body: serde_json::Value = serde_json::from_str(&case.body)
                .map_err(|_| "no-session body is not JSON".to_string())?;
            if body != serde_json::json!({"error": "Not authenticated"}) {
                return Err("no-session body is not the observed authentication error".into());
            }
        }
        "song_detail_missing" => {
            if !case.body.contains("no-such-song-api00") {
                return Err("missing-song response lacks requested ID".into());
            }
        }
        "song_detail_denied_library" => {
            if !case.body.contains("fixture-song-library-two") {
                return Err("denial does not identify the restricted row".into());
            }
        }
        "song_detail_authorized" | "song_detail_authorized_control" => {
            let value: serde_json::Value = serde_json::from_str(&case.body)
                .map_err(|_| "authorized detail is not JSON".to_string())?;
            if value["id"] != "fixture-song-library-one" || value["title"] != "Visible Fixture Song"
            {
                return Err(
                    "authorized detail does not contain expected visible scanner row".into(),
                );
            }
        }
        "same_record_visible_authorized" => {
            let value: serde_json::Value = serde_json::from_str(&case.body)
                .map_err(|_| "authorized scope control is not JSON".to_string())?;
            if value["id"] != "fixture-song-library-two" {
                return Err("authorized scope control lacks restricted row".into());
            }
        }
        "regular_user_granted_success" => {
            if !case.body.contains("fixture-song-library-one") {
                return Err("granted read lacks visible scanner row".into());
            }
        }
        "song_list_authorized" => {
            let rows: serde_json::Value = serde_json::from_str(&case.body)
                .map_err(|_| "song list body is not JSON".to_string())?;
            let mut ids: Vec<_> = rows
                .as_array()
                .ok_or("song list is not an array")?
                .iter()
                .map(|row| row["id"].as_str().unwrap_or(""))
                .collect();
            ids.sort_unstable();
            if ids != ["fixture-song-library-one", "fixture-song-library-two"]
                || case
                    .response_headers
                    .get("x-total-count")
                    .map(String::as_str)
                    != Some("2")
            {
                return Err("song list does not contain exactly the two scanned rows".into());
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn inventory(dir: &Path, manifest: &Manifest) -> Result<Vec<Case>, String> {
    let listed: BTreeSet<_> = manifest.cases.iter().cloned().collect();
    let expected: BTreeSet<_> = CASES.iter().map(|s| s.to_string()).collect();
    if listed != expected || listed.len() != manifest.cases.len() {
        return Err(format!(
            "unexpected manifest case inventory: {:?}",
            manifest.cases
        ));
    }
    let mut disk = BTreeSet::new();
    for entry in fs::read_dir(dir.join("cases")).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = name.strip_suffix(".json") else {
            return Err(format!("unexpected case artifact {name:?}"));
        };
        disk.insert(id.to_string());
    }
    if disk != expected {
        return Err(format!("on-disk case inventory differs: {disk:?}"));
    }
    CASES.iter().map(|id| load_case(dir, id)).collect()
}

fn check_recorded_inventory(manifest: &Manifest) -> Result<(), String> {
    let mut compiled = std::collections::BTreeMap::new();
    for (key, value) in &manifest.provenance {
        if let Some(path) = key.strip_prefix("compiled:") {
            compiled.insert(path, value);
        }
    }
    for required in [
        "scanner/controller.go",
        "scanner/folder_entry.go",
        "scanner/phase_1_folders.go",
        "scanner/phase_2_missing_tracks.go",
        "scanner/phase_3_refresh_albums.go",
        "persistence/sql_base_repository.go",
        "resources/mappings.yaml",
        "resources/mime_types.yaml",
    ] {
        if !compiled.contains_key(required) {
            return Err(format!("pinned source inventory omits {required}"));
        }
    }
    if !compiled
        .keys()
        .any(|path| path.starts_with("db/migrations/"))
    {
        return Err("pinned inventory omits embedded migrations".into());
    }
    let mut digest = Sha256::new();
    for (path, value) in &compiled {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("invalid source digest for {path}"));
        }
        digest.update(path.as_bytes());
        digest.update([0]);
        digest.update(value.as_bytes());
        digest.update([0]);
    }
    let actual = hex(&digest.finalize());
    if manifest.provenance.get("compiled_local_inventory_sha256") != Some(&actual) {
        return Err("recorded compiled source inventory digest mismatch".into());
    }
    if actual != "bdec10f9c3958f852b0e39465157c3a4ef9d940d8cf47571c931beb91e26c5fb" {
        return Err("compiled source inventory differs from pinned capture".into());
    }
    for key in ["go_version", "build_tags", "sqlite_runtime", "os", "arch"] {
        if manifest.provenance.get(key).is_none_or(String::is_empty) {
            return Err(format!("missing toolchain provenance {key}"));
        }
    }
    Ok(())
}

#[test]
fn authenticated_song_corpus_integrity_and_scope_controls() {
    let root = corpus();
    let manifest = parse_manifest(&root).expect("song corpus manifest parses");
    assert!(manifest.authenticated);
    assert!(!manifest.production_binary_run);
    assert!(!manifest.listener_opened);
    assert!(!manifest.network_used);
    assert!(manifest.origin.contains("Go native router") && manifest.origin.contains("httptest"));
    assert!(manifest.scanner_produced_rows >= 2);
    assert!(manifest
        .synthetic_identity
        .contains("credentials and bearer tokens not recorded"));
    assert!(manifest.deterministic_transformations.len() >= 3);
    check_recorded_inventory(&manifest)
        .expect("pinned compiled source inventory is internally consistent");

    let cases = inventory(&root, &manifest).expect("exact fixture inventory");
    let by_id: std::collections::BTreeMap<_, _> = cases
        .iter()
        .map(|case| (case.case_id.as_str(), case))
        .collect();
    for case in &cases {
        validate_case(case).expect("case passes integrity and semantic validation");
        assert_eq!(case.request_method, "GET");
        assert!(case.behavior.len() > 5);
        if case.case_id == "song_detail_no_session" {
            assert_eq!(case.auth_mode, "no-session");
        } else {
            assert_eq!(case.auth_mode, "signed-session");
        }
        for forbidden in [
            "authorization",
            "cookie",
            "set-cookie",
            "x-nd-authorization",
            "x-auth-token",
        ] {
            assert!(
                !case.response_headers.contains_key(forbidden),
                "credential header {forbidden}"
            );
        }
        for forbidden in ["Bearer ", "disposable-only", "password", "jwt_secret"] {
            assert!(
                !case.body.contains(forbidden),
                "credential-shaped body material {forbidden}"
            );
        }
    }

    let control = by_id["same_record_visible_authorized"];
    let denied = by_id["song_detail_denied_library"];
    assert_eq!(
        control.request_path, denied.request_path,
        "scope control must query the same row"
    );
    assert_eq!(control.status, 200);
    assert!(control.body.contains("fixture-song-library-two"));
    assert_eq!(denied.status, 404);
    assert!(denied
        .body
        .contains("MediaFile(id:fixture-song-library-two) not found"));
    let granted = by_id["regular_user_granted_success"];
    assert_eq!(granted.status, 200);
    assert!(granted.body.contains("fixture-song-library-one"));
}

#[test]
fn case_validator_rejects_semantic_corruption_after_integrity_is_updated() {
    let dir = corpus();
    let original = load_case(&dir, "song_detail_no_session").expect("no-session fixture");
    validate_case(&original).expect("captured no-session response matches observed contract");

    let mut wrong_status = original.clone();
    wrong_status.status = 500;
    assert!(
        validate_case(&wrong_status).is_err(),
        "500 no-session response must fail semantic validation"
    );

    let mut wrong_type = original.clone();
    wrong_type
        .response_headers
        .insert("content-type".into(), "text/plain".into());
    assert!(
        validate_case(&wrong_type).is_err(),
        "text/plain no-session response must fail semantic validation"
    );

    let mut wrong_body = original;
    wrong_body.body = r#"{"error":"different failure"}"#.into();
    wrong_body.body_length = wrong_body.body.len();
    wrong_body.body_sha256 = hex(&Sha256::digest(wrong_body.body.as_bytes()));
    assert!(
        validate_case(&wrong_body).is_err(),
        "body with valid updated integrity metadata must fail semantic validation"
    );

    let detail = load_case(&dir, "song_detail_authorized_control").expect("authorized detail");
    let mut same_length = detail.clone();
    same_length.body.replace_range(0..1, "[");
    same_length.body_sha256 = hex(&Sha256::digest(same_length.body.as_bytes()));
    assert!(
        validate_case(&same_length).is_err(),
        "same-length changed body with updated hash must fail semantic validation"
    );
}
