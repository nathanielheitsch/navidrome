//! API-00 native-corpus integrity gate (dispatch 0 of
//! `docs/rust-port/subplans/post-api-00-queue.md`).
//!
//! This test consumes the tracked corpus produced by the **in-process Go native
//! router capture** (`tests/testdata/go_native/`, `tests/data/native/`) and
//! proves the corpus is internally and externally consistent *before* any
//! handler is written against it. It reads only the tracked fixture bytes; it
//! never launches Go, never opens a listener, never touches the network, and
//! never mutates the tracked corpus. Negative controls run on in-memory
//! mutations or on a copy inside an owned temporary root.
//!
//! What is asserted:
//!
//! * exact four-case inventory — manifest list versus files on disk, both
//!   directions, so a missing or extra fixture fails rather than skips;
//! * the pinned Go source provenance: `source_ref`, its tree and the build-input
//!   digest must equal the frozen constants here, and are well-formed.
//!   Live verification that the actual Go build inputs match the pinned ref is
//!   `capture.sh`'s responsibility (README:141-160, exit 5); this Rust test reads
//!   no Git and no Go tree, so it runs anywhere the tracked corpus is present.
//! * every captured response's raw body bytes, length, SHA256 and kind are
//!   self-consistent with the recorded metadata;
//! * the frozen corpus digests (case-bytes digest and full-artifact digest) use
//!   the capture's exact hashing order;
//! * no authenticated-success claim anywhere in the corpus: this slice is
//!   unauthenticated only.
//! * the schema itself is strict (`deny_unknown_fields`), so a provenance field
//!   that reintroduces a moving-worktree value fails here.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Frozen first-slice expectations. Any change to these literals means the
/// corpus was re-captured or hand-edited, which requires a new capture plus
/// parent approval — not an update of this constant in passing.
const EXPECTED_CASES: [&str; 4] = [
    "invalid_auth_token",
    "song_get_all_unauthenticated",
    "song_get_one_unauthenticated",
    "unknown_route_unauthenticated",
];
/// Pinned ORIGINAL Go ref: the last commit touching the captured Go build
/// inputs. Deliberately not the capture's or the branch's commit, so Rust- and
/// docs-only commits cannot move it. Here it is asserted only as a constant the
/// manifest must agree with; `capture.sh` independently resolves and verifies it.
const EXPECTED_SOURCE_REF: &str = "b95e12ed5ff4ea8661edb1752c6e8507cc8fbbb2";
/// Digest over `path + mode + git blob hash` of the compared Go build inputs at
/// the pinned ref. `capture.sh` computes and verifies it against the real ref and
/// on-disk bytes; the Rust test only requires the manifest to agree with it.
const EXPECTED_BUILD_INPUT_DIGEST: &str =
    "2f392437582502dfd6f2bdcf8b9acf72c5eefd36c4cc1fc6683dda40050905ab";
/// SHA256 over `name \0 bytes \0` of every file in `cases/` (sorted names).
const EXPECTED_CASES_SHA256: &str =
    "84e8d8d9ad2f11ccef7e289910ec54e58149d688631ed10873c0eef6cbd253e2";
/// SHA256 over the FULL artifact (manifest + all cases) in the capture's walk
/// order, with paths relative to the corpus root.
const EXPECTED_FULL_SHA256: &str =
    "2e95056e96366c7bbd825f2d95a3961827e0f81f4fc343374e2375b4d79a1713";
const EXPECTED_CORPUS_SCOPE: &str = "first-slice";
const EXPECTED_KIND: &str = "api00-native-corpus-first-slice";
const ORIGIN_PREFIX: &str = "in-process Go native router capture";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    kind: String,
    origin: String,
    behavior: String,
    production_binary_run: bool,
    listener_opened: bool,
    authenticated: bool,
    network_used: bool,
    corpus_scope: String,
    provenance: Provenance,
    cases: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Provenance {
    source_ref: String,
    source_ref_tree: String,
    go_source_sha256: String,
    go_version: String,
    build_tags: String,
    sqlite_runtime: String,
    os: String,
    arch: String,
    toolchain: String,
    module_pins: Vec<String>,
    go_mod_sha256: String,
    go_sum_sha256: String,
    synthetic_config_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    case_id: String,
    origin: String,
    behavior_class: String,
    request_method: String,
    request_path: String,
    auth_mode: String,
    status: u16,
    response_headers: std::collections::BTreeMap<String, String>,
    body_kind: String,
    body_length: usize,
    body_sha256: String,
    body: String,
    provenance: serde_json::Map<String, serde_json::Value>,
    verdict: String,
}

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/native")
}

fn load_manifest(root: &Path) -> Result<Manifest, String> {
    let path = root.join("manifest.json");
    let text = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn load_case(root: &Path, id: &str) -> Result<Case, String> {
    let path = root.join("cases").join(format!("{id}.json"));
    let bytes = fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let case: Case = serde_json::from_slice(&bytes)
        .map_err(|e| format!("parse {} ({id}): {e}", path.display()))?;
    if case.case_id != id {
        return Err(format!(
            "{} declares case_id {:?}, which differs from its file name {:?}",
            path.display(),
            case.case_id,
            id
        ));
    }
    Ok(case)
}

/// Exact inventory: the manifest's list, the files on disk, and the expected
/// frozen set must agree, with no duplicates, no missing and no extra fixture.
fn check_inventory(root: &Path) -> Result<Vec<Case>, String> {
    let manifest = load_manifest(root)?;
    let listed: BTreeSet<String> = manifest.cases.iter().cloned().collect();
    if listed.len() != manifest.cases.len() {
        return Err(format!(
            "manifest cases list contains duplicates ({} entries, {} distinct)",
            manifest.cases.len(),
            listed.len()
        ));
    }
    let expected: BTreeSet<String> = EXPECTED_CASES.iter().map(|s| s.to_string()).collect();
    if listed != expected {
        return Err(format!(
            "manifest case inventory {:?} is not the frozen first-slice set {:?}",
            manifest.cases, EXPECTED_CASES
        ));
    }

    let cases_dir = root.join("cases");
    let entries =
        fs::read_dir(&cases_dir).map_err(|e| format!("read {}: {e}", cases_dir.display()))?;
    let mut on_disk: BTreeSet<String> = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("read {}: {e}", cases_dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = name.strip_suffix(".json") else {
            return Err(format!(
                "unexpected file in {}: {name}",
                cases_dir.display()
            ));
        };
        if !on_disk.insert(id.to_string()) {
            return Err(format!(
                "duplicate case file {name} in {}",
                cases_dir.display()
            ));
        }
    }
    if on_disk != listed {
        let missing: Vec<&String> = listed.difference(&on_disk).collect();
        let extra: Vec<&String> = on_disk.difference(&listed).collect();
        return Err(format!(
            "case inventory drift: manifest lists {:?} but disk holds {:?} (missing {:?}, extra {:?})",
            manifest.cases,
            on_disk.iter().collect::<Vec<_>>(),
            missing,
            extra
        ));
    }

    let mut cases = Vec::with_capacity(listed.len());
    for id in &listed {
        cases.push(load_case(root, id)?);
    }
    Ok(cases)
}

/// Body self-consistency: the recorded hash, length and kind must follow from
/// the raw bytes actually stored in the fixture.
fn check_body_integrity(case: &Case) -> Result<(), String> {
    let raw = case.body.as_bytes();
    let want = hex_of(&sha256(raw));
    if case.body_sha256 != want {
        return Err(format!(
            "{}: body_sha256 {} does not match sha256 of the stored {} body bytes ({want})",
            case.case_id,
            case.body_sha256,
            raw.len()
        ));
    }
    if case.body_length != raw.len() {
        return Err(format!(
            "{}: body_length {} does not match the stored body ({} bytes)",
            case.case_id,
            case.body_length,
            raw.len()
        ));
    }
    let classified = classify_body(raw);
    if case.body_kind != classified {
        return Err(format!(
            "{}: body_kind {:?} does not match the classification of the stored bytes ({classified:?})",
            case.case_id, case.body_kind
        ));
    }
    // Content type and body kind must not contradict each other.
    let content_type = case
        .response_headers
        .get("content-type")
        .ok_or_else(|| format!("{}: no content-type header recorded", case.case_id))?;
    let ct = content_type.split(';').next().unwrap_or("").trim();
    match case.body_kind.as_str() {
        "json-object" | "json-array" | "json-like" => {
            if ct != "application/json" {
                return Err(format!(
                    "{}: body_kind {:?} with content-type {content_type:?}",
                    case.case_id, case.body_kind
                ));
            }
        }
        "text" => {
            if !ct.starts_with("text/") {
                return Err(format!(
                    "{}: body_kind \"text\" with content-type {content_type:?}",
                    case.case_id
                ));
            }
        }
        "empty" => {
            if !raw.is_empty() {
                return Err(format!(
                    "{}: body_kind \"empty\" but {} body bytes are stored",
                    case.case_id,
                    raw.len()
                ));
            }
        }
        other => {
            return Err(format!("{}: unknown body_kind {other:?}", case.case_id));
        }
    }
    Ok(())
}

/// Reimplements the capture's `classifyBody` (`main.go:756-774`) so the recorded
/// kind is verified rather than assumed.
fn classify_body(raw: &[u8]) -> &'static str {
    let text = String::from_utf8_lossy(raw);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "empty";
    }
    let first = trimmed.chars().next().unwrap_or('\0');
    if first == '{' || first == '[' {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            match value {
                serde_json::Value::Object(_) => return "json-object",
                serde_json::Value::Array(_) => return "json-array",
                _ => {}
            }
        }
        return "json-like";
    }
    "text"
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Lowercase hex without adding a `hex` dependency (already locked via nd-utils,
/// but a test file does not need a dependency edge for six characters of logic).
fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Case-bytes digest in the capture's exact order: sorted names, then
/// `name \0 bytes \0`.
fn cases_digest(cases_dir: &Path) -> Result<String, String> {
    let mut names: Vec<String> = fs::read_dir(cases_dir)
        .map_err(|e| format!("read {}: {e}", cases_dir.display()))?
        .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("read {}: {e}", cases_dir.display()))?;
    names.sort();
    let mut hasher = Sha256::new();
    for name in &names {
        let bytes = fs::read(cases_dir.join(name))
            .map_err(|e| format!("read {}: {e}", cases_dir.join(name).display()))?;
        hasher.update(name.as_bytes());
        hasher.update([0u8]);
        hasher.update(&bytes);
        hasher.update([0u8]);
    }
    Ok(hex_of(&hasher.finalize()))
}

/// Full-artifact digest, reproducing the capture's `digestOfTree` walk:
/// `filepath.WalkDir` visits each directory's entries in **lexical** order,
/// recursing into a subdirectory at that entry's position, and hashes only
/// files as `rel \0 bytes \0`.
fn full_digest(root: &Path) -> Result<String, String> {
    let mut hasher = Sha256::new();
    walk_lex(root, root, &mut hasher)?;
    Ok(hex_of(&hasher.finalize()))
}

fn walk_lex(root: &Path, dir: &Path, hasher: &mut Sha256) -> Result<(), String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map_err(|e| format!("read {}: {e}", dir.display()))?
        .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("read {}: {e}", dir.display()))?;
    names.sort();
    for name in names {
        let path = dir.join(&name);
        let meta = fs::metadata(&path).map_err(|e| format!("stat {}: {e}", path.display()))?;
        if meta.is_dir() {
            walk_lex(root, &path, hasher)?;
            continue;
        }
        if fs::symlink_metadata(&path)
            .map_err(|e| format!("stat {}: {e}", path.display()))?
            .is_symlink()
        {
            return Err(format!("symlink inside corpus: {}", path.display()));
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|_| format!("{} not under {}", path.display(), root.display()))?;
        let bytes = fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update([0u8]);
        hasher.update(&bytes);
        hasher.update([0u8]);
    }
    Ok(())
}

/// No authenticated-success claim: the manifest declares the slice
/// unauthenticated, and every case must actually match that claim.
fn check_unauthenticated_only(manifest: &Manifest, cases: &[Case]) -> Result<(), String> {
    if manifest.kind != EXPECTED_KIND {
        return Err(format!(
            "manifest kind {:?} is not {EXPECTED_KIND:?}",
            manifest.kind
        ));
    }
    if manifest.corpus_scope != EXPECTED_CORPUS_SCOPE {
        return Err(format!(
            "corpus_scope {:?} is not {EXPECTED_CORPUS_SCOPE:?}; this gate only owns the first slice",
            manifest.corpus_scope
        ));
    }
    for flag in [
        ("authenticated", manifest.authenticated),
        ("production_binary_run", manifest.production_binary_run),
        ("listener_opened", manifest.listener_opened),
        ("network_used", manifest.network_used),
    ] {
        if flag.1 {
            return Err(format!("manifest claims {} = true", flag.0));
        }
    }
    if !manifest.origin.starts_with(ORIGIN_PREFIX) {
        return Err(format!(
            "manifest origin {:?} does not state the in-process capture origin",
            manifest.origin
        ));
    }
    if !manifest
        .behavior
        .contains("NOT authenticated song behavior")
    {
        return Err(format!(
            "manifest behavior {:?} does not state that authenticated song behavior is out of scope",
            manifest.behavior
        ));
    }
    for case in cases {
        if !case.origin.starts_with(ORIGIN_PREFIX) {
            return Err(format!(
                "{}: origin {:?} is not the in-process capture",
                case.case_id, case.origin
            ));
        }
        if case.behavior_class.is_empty() {
            return Err(format!("{}: no behavior_class recorded", case.case_id));
        }
        if case.request_method != "GET" {
            return Err(format!(
                "{}: request_method {:?} is outside this slice (only GET)",
                case.case_id, case.request_method
            ));
        }
        if !case.request_path.starts_with("/api/") || case.request_path.contains("..") {
            return Err(format!(
                "{}: request_path {:?} is not a native API path under the /api prefix",
                case.case_id, case.request_path
            ));
        }
        if !(case.auth_mode == "none" || case.auth_mode == "invalid_token") {
            return Err(format!(
                "{}: auth_mode {:?} is outside this slice (only \"none\" or \"invalid_token\"; login and valid tokens are not captured here)",
                case.case_id, case.auth_mode
            ));
        }
        if case.status < 400 {
            return Err(format!(
                "{}: status {} is a success response, which this unauthenticated slice cannot claim",
                case.case_id, case.status
            ));
        }
        if case.status != 401 && case.status != 404 {
            return Err(format!(
                "{}: status {} is outside the captured 401/404 classes",
                case.case_id, case.status
            ));
        }
        if case.verdict != "observed-matches-source-derived-spec" {
            return Err(format!("{}: verdict {:?}", case.case_id, case.verdict));
        }
        if case.provenance.is_empty() {
            return Err(format!("{}: no provenance recorded", case.case_id));
        }
    }
    Ok(())
}

/// Credential hygiene: no real credential transport header and no
/// credential-shaped value survives in the fixtures.
fn check_no_credentials(cases: &[Case]) -> Result<(), String> {
    for case in cases {
        for header in [
            "authorization",
            "cookie",
            "set-cookie",
            "x-auth-token",
            "x-nd-authorization",
        ] {
            if case.response_headers.contains_key(header) {
                return Err(format!(
                    "{}: credential header {header:?} present",
                    case.case_id
                ));
            }
        }
        for needle in [
            "jwt_secret",
            "subsonicSalt",
            "subsonicToken",
            "refresh_token",
            "api_key",
        ] {
            if case.body.contains(needle) && !case.body.contains("<redacted>") {
                return Err(format!(
                    "{}: body mentions {needle} unredacted",
                    case.case_id
                ));
            }
        }
    }
    Ok(())
}

fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c.to_ascii_lowercase()))
        && value == value.to_ascii_lowercase()
}

/// Provenance must agree with the frozen Go source pin and be well-formed.
/// `capture.sh` is what verifies the ref and the real build inputs; here the
/// check is that the corpus declares exactly the pinned ref and digest, so a
/// hand-edited or re-pointed manifest fails without touching Git or Go.
fn check_provenance(p: &Provenance) -> Result<(), String> {
    if p.source_ref != EXPECTED_SOURCE_REF {
        return Err(format!(
            "provenance source_ref {:?} is not the frozen pinned Go ref {EXPECTED_SOURCE_REF}",
            p.source_ref
        ));
    }
    if p.go_source_sha256 != EXPECTED_BUILD_INPUT_DIGEST {
        return Err(format!(
            "provenance build-input digest {:?} is not the frozen {EXPECTED_BUILD_INPUT_DIGEST}",
            p.go_source_sha256
        ));
    }
    if !is_hex(&p.source_ref, 40) {
        return Err(format!(
            "source_ref {:?} is not a lowercase commit hash",
            p.source_ref
        ));
    }
    if !is_hex(&p.source_ref_tree, 40) {
        return Err(format!(
            "source_ref_tree {:?} is not a lowercase tree hash",
            p.source_ref_tree
        ));
    }
    if !is_hex(&p.go_source_sha256, 64) {
        return Err(format!(
            "go_source_sha256 {:?} is not a lowercase SHA256",
            p.go_source_sha256
        ));
    }
    if p.build_tags != "netgo sqlite_fts5" {
        return Err(format!("unexpected build_tags {:?}", p.build_tags));
    }
    if p.go_version.is_empty() || !p.go_version.starts_with("go") {
        return Err(format!("go_version {:?} is not recorded", p.go_version));
    }
    if p.sqlite_runtime.is_empty() {
        return Err("sqlite_runtime must be recorded".to_string());
    }
    if p.toolchain != format!("{}/{} {}", p.os, p.arch, p.go_version) {
        return Err(format!(
            "toolchain {:?} does not match os/arch/go_version",
            p.toolchain
        ));
    }
    if p.module_pins.len() != 12 {
        return Err(format!(
            "expected six modules pinned in both go.mod and go.sum (12 entries), got {}",
            p.module_pins.len()
        ));
    }
    if !p
        .module_pins
        .iter()
        .all(|pin| pin.contains("h1:") || pin.starts_with("go.mod:"))
    {
        return Err("module_pins must pair go.mod entries with go.sum h1: hashes".to_string());
    }
    if !(is_hex(&p.go_mod_sha256, 64) && is_hex(&p.go_sum_sha256, 64)) {
        return Err("go_mod_sha256/go_sum_sha256 must be lowercase SHA256".to_string());
    }
    if !is_hex(&p.synthetic_config_sha256, 64) {
        return Err("synthetic_config_sha256 must be a lowercase SHA256".to_string());
    }
    Ok(())
}

/// Copy the tracked corpus into an owned temporary root so a negative control
/// can mutate a fixture without touching the tracked one.
fn copy_corpus(dest: &Path) -> Result<(), String> {
    let src = corpus_root();
    fs::create_dir_all(dest.join("cases"))
        .map_err(|e| format!("create {}: {e}", dest.display()))?;
    fs::copy(src.join("manifest.json"), dest.join("manifest.json"))
        .map_err(|e| format!("copy manifest: {e}"))?;
    let cases = src.join("cases");
    for entry in fs::read_dir(&cases).map_err(|e| format!("read {}: {e}", cases.display()))? {
        let entry = entry.map_err(|e| format!("read {}: {e}", cases.display()))?;
        let name = entry.file_name();
        fs::copy(cases.join(&name), dest.join("cases").join(&name))
            .map_err(|e| format!("copy {}: {e}", name.to_string_lossy()))?;
    }
    Ok(())
}

#[test]
fn corpus_case_inventory_matches_manifest_and_disk() {
    let cases =
        check_inventory(&corpus_root()).expect("first-slice corpus inventory must be intact");
    assert_eq!(cases.len(), EXPECTED_CASES.len());
    let ids: Vec<&str> = cases.iter().map(|c| c.case_id.as_str()).collect();
    assert_eq!(ids, EXPECTED_CASES.to_vec());
}

#[test]
fn corpus_provenance_pins_stable_go_ref_and_input_digest() {
    // Provenance is asserted against the frozen constants only. It is NOT
    // recomputed here: proving the real Go build inputs match the pinned ref
    // needs Git and the Go tree, and belongs to `capture.sh` (README:141-160,
    // exit 5). Asserting the manifest agrees with the frozen pin is what a
    // fixture-only Rust test can do, and it catches any hand-edit of the corpus.
    let manifest =
        load_manifest(&corpus_root()).expect("manifest must parse against the frozen schema");
    check_provenance(&manifest.provenance)
        .expect("tracked provenance must agree with the frozen Go source pin");
}

#[test]
fn corpus_response_bytes_length_and_hash_are_self_consistent() {
    let cases = check_inventory(&corpus_root()).expect("inventory");
    for case in &cases {
        check_body_integrity(case).unwrap_or_else(|e| panic!("body integrity: {e}"));
    }
}

#[test]
fn corpus_hashes_match_frozen_digests() {
    let root = corpus_root();
    assert_eq!(
        cases_digest(&root.join("cases")).expect("cases digest"),
        EXPECTED_CASES_SHA256,
        "case-bytes digest drifted: re-capture and re-freeze deliberately, never hand-edit"
    );
    assert_eq!(
        full_digest(&root).expect("full artifact digest"),
        EXPECTED_FULL_SHA256,
        "full artifact digest drifted: manifest and/or case bytes changed"
    );
}

#[test]
fn corpus_makes_no_authenticated_success_claim() {
    let manifest = load_manifest(&corpus_root()).expect("manifest");
    let cases = check_inventory(&corpus_root()).expect("inventory");
    check_unauthenticated_only(&manifest, &cases).expect("unauthenticated-only slice invariant");
    check_no_credentials(&cases).expect("no credential material in fixtures");
}

#[test]
fn native_corpus_has_source_and_toolchain_provenance() {
    // boot-api.md:183 acceptance name, scoped to the first slice.
    let manifest = load_manifest(&corpus_root()).expect("manifest");
    assert!(!manifest.provenance.source_ref.is_empty());
    assert!(!manifest.provenance.go_source_sha256.is_empty());
    assert!(!manifest.provenance.build_tags.is_empty());
    assert!(!manifest.provenance.go_version.is_empty());
    assert!(!manifest.provenance.sqlite_runtime.is_empty());
    assert!(!manifest.origin.is_empty());
}

#[test]
fn missing_fixture_fails_not_skips() {
    let scratch = tempfile::tempdir().expect("owned temp root");
    copy_corpus(scratch.path()).expect("copy corpus into owned temp root");
    let removed = scratch
        .path()
        .join("cases/song_get_all_unauthenticated.json");
    fs::remove_file(&removed).expect("remove the copy, never a tracked fixture");

    let err = check_inventory(scratch.path()).expect_err("a missing fixture must fail, not skip");
    assert!(
        err.contains("inventory drift") && err.contains("missing"),
        "{err}"
    );
    assert!(
        err.contains("song_get_all_unauthenticated"),
        "the missing case must be named: {err}"
    );

    // The tracked corpus is untouched by this control.
    check_inventory(&corpus_root()).expect("tracked corpus still intact");
}

#[test]
fn extra_fixture_fails_not_skips() {
    let scratch = tempfile::tempdir().expect("owned temp root");
    copy_corpus(scratch.path()).expect("copy corpus into owned temp root");
    fs::write(
        scratch.path().join("cases/song_get_all_authenticated.json"),
        b"{\"case_id\":\"song_get_all_authenticated\"}",
    )
    .expect("write an extra file into the copy");

    let err = check_inventory(scratch.path()).expect_err("an extra fixture must fail, not skip");
    assert!(err.contains("extra"), "{err}");
    assert!(err.contains("song_get_all_authenticated"), "{err}");
    check_inventory(&corpus_root()).expect("tracked corpus still intact");
}

#[test]
fn provenance_pin_negative_control_fails_in_scratch() {
    // Non-vacuity for the provenance gate now that it no longer reads Git: a
    // manifest whose pin was repointed to some other ref or digest must fail,
    // while the tracked corpus is the positive control.
    let scratch = tempfile::tempdir().expect("owned temp root");
    copy_corpus(scratch.path()).expect("copy corpus into owned temp root");
    let manifest_path = scratch.path().join("manifest.json");

    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("read")).expect("parse");
    value["provenance"]["source_ref"] =
        serde_json::Value::String("ad3438318e8f56b624b33471828d61617c36c9f2".into());
    fs::write(&manifest_path, value.to_string()).expect("write re-pointed source_ref");
    let manifest = load_manifest(scratch.path()).expect("manifest parses");
    let err = check_provenance(&manifest.provenance)
        .expect_err("a repointed source_ref must fail the frozen pin");
    assert!(err.contains("source_ref"), "{err}");

    // Restoring source_ref but drifting the build-input digest must fail too,
    // so the digest is genuinely pinned and not merely shape-checked.
    value["provenance"]["source_ref"] = serde_json::Value::String(EXPECTED_SOURCE_REF.into());
    value["provenance"]["go_source_sha256"] = serde_json::Value::String("0".repeat(64));
    fs::write(&manifest_path, value.to_string()).expect("write drifted build-input digest");
    let manifest = load_manifest(scratch.path()).expect("manifest parses");
    let err = check_provenance(&manifest.provenance)
        .expect_err("a drifted build-input digest must fail the frozen pin");
    assert!(err.contains("build-input digest"), "{err}");

    // Positive control: the tracked manifest passes the same checks.
    let tracked = load_manifest(&corpus_root()).expect("tracked manifest");
    check_provenance(&tracked.provenance).expect("tracked provenance is the positive control");
}

#[test]
fn changed_response_body_fails_hash_and_digest_checks() {
    let scratch = tempfile::tempdir().expect("owned temp root");
    copy_corpus(scratch.path()).expect("copy corpus into owned temp root");

    // Mutate the copy's body bytes while keeping the recorded hash, exactly the
    // tampering this gate must catch.
    let path = scratch.path().join("cases/invalid_auth_token.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("read copy")).expect("parse copy");
    value["body"] = json_body("{\"error\":\"Not authenticated!\"}");
    fs::write(&path, value.to_string()).expect("write mutated copy");

    let cases = check_inventory(scratch.path()).expect("inventory unaffected");
    let case = cases
        .iter()
        .find(|c| c.case_id == "invalid_auth_token")
        .expect("case present");
    let err = check_body_integrity(case).expect_err("a changed body must fail the recorded hash");
    assert!(err.contains("body_sha256"), "{err}");
    // Non-vacuity: the identical check passes on the tracked bytes, so the
    // failure above is caused by the mutation, not by a broken check.
    let tracked = load_case(&corpus_root(), "invalid_auth_token").expect("tracked case");
    check_body_integrity(&tracked).expect("tracked body integrity is the positive control");

    let digest = full_digest(scratch.path()).expect("digest of mutated copy");
    assert_ne!(
        digest, EXPECTED_FULL_SHA256,
        "a mutated body must change the corpus digest"
    );
    assert_ne!(
        cases_digest(&scratch.path().join("cases")).expect("cases digest"),
        EXPECTED_CASES_SHA256,
        "a mutated body must change the case-bytes digest"
    );

    // Consistency, in the other direction: the recorded kind contradicts the
    // bytes, while hash and length are updated so the kind check is what fires.
    let mut wrong_kind: serde_json::Value = serde_json::from_slice(
        &fs::read(corpus_root().join("cases/invalid_auth_token.json")).expect("read tracked"),
    )
    .expect("parse");
    let text = "plain text, not json";
    wrong_kind["body"] = json_body(text);
    wrong_kind["body_sha256"] = serde_json::Value::String(hex_of(&sha256(text.as_bytes())));
    wrong_kind["body_length"] = serde_json::Value::from(text.len());
    wrong_kind["body_kind"] = serde_json::Value::String("json-object".into());
    fs::write(&path, wrong_kind.to_string()).expect("write kind-mismatched copy");
    let cases = check_inventory(scratch.path()).expect("inventory");
    let case = cases
        .iter()
        .find(|c| c.case_id == "invalid_auth_token")
        .expect("case present");
    let err = check_body_integrity(case).expect_err("recorded kind must follow from the bytes");
    assert!(err.contains("body_kind"), "{err}");
    check_inventory(&corpus_root()).expect("tracked corpus still intact");
}

#[test]
fn wrong_scope_or_false_success_claim_fails() {
    let scratch = tempfile::tempdir().expect("owned temp root");
    copy_corpus(scratch.path()).expect("copy corpus into owned temp root");
    let manifest_path = scratch.path().join("manifest.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("read")).expect("parse");

    // Wrong scope.
    value["corpus_scope"] = serde_json::Value::String("full-corpus".into());
    fs::write(&manifest_path, value.to_string()).expect("write");
    let manifest = load_manifest(scratch.path()).expect("manifest parses");
    let cases = check_inventory(scratch.path()).expect("inventory");
    let err = check_unauthenticated_only(&manifest, &cases).expect_err("wrong scope must fail");
    assert!(err.contains("corpus_scope"), "{err}");
    let tracked_manifest = load_manifest(&corpus_root()).expect("tracked manifest");
    let tracked_cases = check_inventory(&corpus_root()).expect("tracked inventory");
    check_unauthenticated_only(&tracked_manifest, &tracked_cases)
        .expect("tracked manifest is the positive control for scope");

    // Claiming the slice was authenticated.
    value["corpus_scope"] = serde_json::Value::String(EXPECTED_CORPUS_SCOPE.into());
    value["authenticated"] = serde_json::Value::Bool(true);
    fs::write(&manifest_path, value.to_string()).expect("write");
    let manifest = load_manifest(scratch.path()).expect("manifest parses");
    let err =
        check_unauthenticated_only(&manifest, &cases).expect_err("authenticated claim must fail");
    assert!(err.contains("authenticated"), "{err}");

    // A case that pretends to be an authenticated success.
    fs::write(&manifest_path, value.to_string()).expect("write manifest");
    let case_path = scratch
        .path()
        .join("cases/song_get_all_unauthenticated.json");
    let mut case_value: serde_json::Value =
        serde_json::from_slice(&fs::read(&case_path).expect("read")).expect("parse");
    case_value["status"] = serde_json::Value::from(200u16);
    case_value["auth_mode"] = serde_json::Value::String("valid_token".into());
    fs::write(&case_path, case_value.to_string()).expect("write mutated case");
    let manifest = load_manifest(&corpus_root()).expect("tracked manifest");
    let cases = check_inventory(scratch.path()).expect("inventory of copy");
    let err = check_unauthenticated_only(&manifest, &cases).expect_err("success fixture must fail");
    assert!(err.contains("auth_mode") || err.contains("status"), "{err}");

    // A provenance field that reintroduces a moving-worktree value must be
    // rejected by the strict schema, not silently accepted.
    let mut strict: serde_json::Value =
        serde_json::from_slice(&fs::read(corpus_root().join("manifest.json")).expect("read"))
            .expect("parse");
    strict["provenance"]["worktree_head"] = serde_json::Value::String("ad343831".into());
    let err = serde_json::from_value::<Manifest>(strict)
        .expect_err("a moving-worktree provenance field must not be accepted");
    assert!(err.to_string().contains("worktree_head"), "{err}");

    check_inventory(&corpus_root()).expect("tracked corpus still intact");
}

/// Build a JSON string value carrying `text`, so mutations read clearly.
fn json_body(text: &str) -> serde_json::Value {
    serde_json::Value::String(text.to_string())
}
