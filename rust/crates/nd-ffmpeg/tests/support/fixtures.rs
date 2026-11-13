use std::path::{Path, PathBuf};

pub fn fixture_path(name: &str) -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .ancestors()
        .find(|p| p.join("go.mod").is_file())
        .expect("repository root containing go.mod");
    root.join("tests/fixtures").join(name)
}

pub fn fixture(name: &str) -> String {
    let path = fixture_path(name);
    assert!(
        path.exists(),
        "required fixture missing: {}",
        path.display()
    );
    path.to_string_lossy().into_owned()
}
