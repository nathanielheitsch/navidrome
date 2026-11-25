//! BOOT-01 increment A: startup preflight (steps 1–2) acceptance tests.
//!
//! Everything runs against synthetic temp roots with an injected environment
//! and an injected startup cwd, so no test touches the real process cwd, the
//! real environment, or a production data directory. No test calls
//! `std::env::set_current_dir`, so the suite stays parallel-safe.
//!
//! Coverage against `boot-api.md:202` (the music-root half of the matrix) and
//! `boot01-scout-memo.md` §2:
//! * `ND_CONFIGFILE` resolution: explicit flag > env > loader defaults; an
//!   env-named **existing** file's keys apply; an env-named **missing** file
//!   is skipped exactly like Go's `os.Stat` guard (`conf/configuration.go:1201`).
//! * the single cwd anchor: relative flag/env config paths, the music root and
//!   the data folder all resolve against the injected startup cwd, which
//!   differs from the real process cwd; a missing flag target is fatal.
//! * invalid config is fatal before any filesystem creation, and reported
//!   structurally: an allowlisted class plus a numeric location, never raw
//!   config text (`Display`, `Debug`, and the source chain are all asserted).
//! * music root: missing / regular-file / unreadable / symlink / Unicode /
//!   relative / trailing-slash; a missing root stays absent afterwards.
//! * secrets never appear in error messages or `Debug`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nd_cli::preflight::{
    preflight, resolve_config_file, ConfigErrorClass, ConfigSource, PreflightError,
    PreflightOptions,
};

/// An isolated, per-test temp tree. `tempfile::TempDir` creates the directory
/// exclusively (pid + random suffix) and removes it on drop, so tests never
/// collide and never touch a production path.
struct Sandbox {
    inner: tempfile::TempDir,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let inner = tempfile::Builder::new()
            .prefix(&format!("nd-cli-preflight-{tag}-"))
            .tempdir()
            .expect("create isolated temp root");
        Sandbox { inner }
    }

    fn root(&self) -> &Path {
        self.inner.path()
    }

    /// `<root>/<name>`, created as a directory.
    fn dir(&self, name: &str) -> PathBuf {
        let p = self.root().join(name);
        std::fs::create_dir_all(&p).expect("create sandbox dir");
        p
    }

    /// `<root>/<name>` containing `contents`.
    fn file(&self, name: &str, contents: &str) -> PathBuf {
        let p = self.root().join(name);
        std::fs::write(&p, contents).expect("write sandbox file");
        p
    }

    fn join(&self, name: &str) -> PathBuf {
        self.root().join(name)
    }
}

/// A minimal valid config with the given music folder and port.
fn toml_with(music: &str, port: i64) -> String {
    format!("MusicFolder = \"{music}\"\nPort = {port}\n")
}

/// Builds a lookup closure over an injected map (no process env involved).
fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn lookup(map: HashMap<String, String>) -> impl Fn(&str) -> Option<String> {
    move |k| map.get(k).cloned()
}

/// Canonicalized form of a path, for comparisons against the outcome.
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|e| panic!("canonicalize {p:?}: {e}"))
}

/// Runs a preflight with the single cwd anchor injected, so the real process
/// cwd is never consulted.
fn run(
    config_file: Option<&str>,
    startup_cwd: Option<&Path>,
    pairs: &[(&str, &str)],
) -> Result<nd_cli::preflight::PreflightOutcome, PreflightError> {
    let options = PreflightOptions {
        config_file,
        env: Some(Box::new(lookup(env_of(pairs)))),
        startup_cwd,
        effective_uid: Some(1000),
        allow_zero_port: false,
    };
    preflight(options)
}

/// Renders an error and its whole source chain, for disclosure assertions.
fn full_text(err: &PreflightError) -> String {
    let mut text = err.to_string();
    let mut src = std::error::Error::source(err);
    while let Some(e) = src {
        text.push_str(" | ");
        text.push_str(&e.to_string());
        src = e.source();
    }
    text
}

// ------------------------------------------------------ ND_CONFIGFILE rule

/// A configured file's keys must actually apply: today `Config::load(None)`
/// never consults `ND_CONFIGFILE`, so `MusicFolder`/`Port` were invisible.
#[test]
fn configfile_env_names_existing_file_and_its_keys_apply() {
    let sb = Sandbox::new("envfile");
    let music = sb.dir("music");
    let cfg = sb.file("conf.toml", &toml_with(music.to_str().unwrap(), 4599));

    let out = run(
        None,
        Some(sb.root()),
        &[("ND_CONFIGFILE", cfg.to_str().unwrap())],
    )
    .expect("env-named existing config file must load");

    assert_eq!(out.config_source, ConfigSource::Env);
    assert_eq!(out.config.port, 4599, "configured Port must apply");
    assert_eq!(out.config.music_folder, music.to_str().unwrap());
    assert_eq!(
        out.config.config_file,
        cfg.to_str().unwrap(),
        "config_file must record the file actually used"
    );
    assert_eq!(out.music_root, canonical(&music));
}

/// The explicit argument beats `ND_CONFIGFILE` (Go: `cfgFile != ""` wins).
#[test]
fn explicit_config_path_beats_configfile_env() {
    let sb = Sandbox::new("precedence");
    let music = sb.dir("music");
    let flag_file = sb.file("flag.toml", &toml_with(music.to_str().unwrap(), 1234));
    let env_file = sb.file(
        "env.toml",
        &toml_with(sb.dir("other").to_str().unwrap(), 5678),
    );

    let out = run(
        Some(flag_file.to_str().unwrap()),
        Some(sb.root()),
        &[("ND_CONFIGFILE", env_file.to_str().unwrap())],
    )
    .expect("flag path must load");

    assert_eq!(out.config_source, ConfigSource::Flag);
    assert_eq!(out.config.port, 1234, "flag file wins over ND_CONFIGFILE");
    assert_eq!(out.config.music_folder, music.to_str().unwrap());
    assert_eq!(out.config.config_file, flag_file.to_str().unwrap());
}

/// An env-named path that does not exist is skipped, not fatal (Go's
/// `os.Stat` guard), and the normal loader defaults take over. Crucially the
/// missing file must not be created by resolution.
#[test]
fn configfile_env_missing_path_is_skipped_and_stays_absent() {
    let sb = Sandbox::new("envmissing");
    let music = sb.dir("music");
    let ghost = sb.join("nope.toml");
    assert!(!ghost.exists());

    let out = run(
        None,
        Some(sb.root()),
        &[("ND_CONFIGFILE", ghost.to_str().unwrap())],
    )
    .expect("a missing ND_CONFIGFILE target must be skipped like Go's os.Stat guard");

    assert_eq!(out.config_source, ConfigSource::Defaults);
    assert_eq!(out.config.port, 4533, "loader default port");
    assert_eq!(
        out.config.music_folder, "./music",
        "loader default music folder"
    );
    assert_eq!(out.config.config_file, "", "no file was used");
    assert_eq!(out.music_root, canonical(&music), "default ./music vs cwd");
    assert!(
        !ghost.exists(),
        "config-path resolution must not create the missing file"
    );
}

/// `ND_CONFIGFILE` pointing at a directory is fatal (Go fatals on the read).
#[test]
fn configfile_env_must_be_a_regular_file() {
    let sb = Sandbox::new("envdir");
    let dir = sb.dir("not-a-file");

    // ResolvedConfig deliberately has no Debug (it holds a Config with secret
    // fields), so the negative result is asserted by matching, not expect_err.
    let result = resolve_config_file(
        None,
        Some(&lookup(env_of(&[("ND_CONFIGFILE", dir.to_str().unwrap())]))),
        sb.root(),
    );
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("a directory cannot be used as a config file"),
    };

    assert!(matches!(err, PreflightError::ConfigFileNotAFile { .. }));
    assert!(err.to_string().contains("not a regular file"));
}

/// An empty flag value counts as "not supplied" (Go `cfgFile != ""`).
#[test]
fn empty_flag_value_falls_through_to_env_then_defaults() {
    let sb = Sandbox::new("emptyflag");
    let music = sb.dir("music");
    let cfg = sb.file("conf.toml", &toml_with(music.to_str().unwrap(), 4611));

    let out = run(
        Some(""),
        Some(sb.root()),
        &[("ND_CONFIGFILE", cfg.to_str().unwrap())],
    )
    .expect("empty flag must fall through to ND_CONFIGFILE");
    assert_eq!(out.config_source, ConfigSource::Env);
    assert_eq!(out.config.port, 4611);

    let out = run(Some(""), Some(sb.root()), &[]).expect("no config at all uses defaults");
    assert_eq!(out.config_source, ConfigSource::Defaults);
    assert_eq!(out.config.port, 4533);
}

/// A flag-named path that does not exist is fatal (Go fatals at
/// `viper.ReadInConfig` when `cfgFile` is set), and the target must stay
/// absent — a fatal startup must not leave a config file behind.
#[test]
fn missing_flag_config_path_is_fatal_and_stays_absent() {
    let sb = Sandbox::new("flagmissing");
    sb.dir("music");
    let ghost = sb.join("nope.toml");
    assert!(!ghost.exists());

    let err = run(Some(ghost.to_str().unwrap()), Some(sb.root()), &[])
        .expect_err("an explicitly requested config file that cannot be read must be fatal");
    assert!(
        matches!(err, PreflightError::ConfigFileUnreadable { .. }),
        "{err:?}"
    );
    assert!(err.to_string().contains("cannot be read"));
    assert!(
        !ghost.exists(),
        "resolution must not create the missing config file"
    );
}

/// A flag-named directory is fatal with the same class, and no `ND_CONFIGFILE`
/// fallback is attempted.
#[test]
fn flag_config_path_must_be_a_regular_file() {
    let sb = Sandbox::new("flagdir");
    let dir = sb.dir("not-a-file");
    let music = sb.dir("music");
    let real = sb.file("real.toml", &toml_with(music.to_str().unwrap(), 4700));

    let err = run(
        Some(dir.to_str().unwrap()),
        Some(sb.root()),
        &[("ND_CONFIGFILE", real.to_str().unwrap())],
    )
    .expect_err("a directory cannot be an explicitly requested config file");
    assert!(
        matches!(err, PreflightError::ConfigFileUnreadable { .. }),
        "{err:?}"
    );
    assert!(err.to_string().contains("not a regular file"));
}

// ----------------------------------------- one cwd anchor for every path

/// The injected startup cwd differs from the real process cwd, and the config
/// file inside it has a name that exists nowhere else: a **relative flag**
/// value must select that file and its non-default setting must apply.
#[test]
fn relative_flag_path_is_selected_from_the_injected_startup_cwd() {
    let sb = Sandbox::new("anchorflag");
    let startup = sb.dir("startup");
    // Uniquely named so a fall-through to the real process cwd cannot succeed.
    let name = "nd-cli-anchor-flag-only.toml";
    sb.file(
        &format!("startup/{name}"),
        &toml_with(sb.dir("startup/music").to_str().unwrap(), 4711),
    );
    assert!(!Path::new(name).exists(), "test premise: unique name");

    let out = run(Some(name), Some(&startup), &[])
        .expect("relative flag path must resolve against the injected startup cwd");

    assert_eq!(out.config_source, ConfigSource::Flag);
    assert_eq!(
        out.config.port, 4711,
        "the anchored file's setting must apply"
    );
    assert_eq!(
        PathBuf::from(&out.config.config_file),
        canonical(&startup.join(name)),
        "the flag file must be the one under the startup cwd"
    );
    assert_eq!(out.startup_cwd, canonical(&startup));
}

/// Same anchor for a **relative `ND_CONFIGFILE`** value: it selects the
/// uniquely named file under the injected startup cwd, its setting applies, and
/// the relative music root inside it resolves against that same anchor.
#[test]
fn relative_env_path_is_selected_from_the_injected_startup_cwd() {
    let sb = Sandbox::new("anchorenv");
    let startup = sb.dir("startup");
    let name = "nd-cli-anchor-env-only.toml";
    sb.file(
        &format!("startup/{name}"),
        "MusicFolder = \"music\"\nPort = 4722\n",
    );
    sb.dir("startup/music");
    assert!(!Path::new(name).exists(), "test premise: unique name");

    let out = run(None, Some(&startup), &[("ND_CONFIGFILE", name)])
        .expect("relative ND_CONFIGFILE must resolve against the injected startup cwd");

    assert_eq!(out.config_source, ConfigSource::Env);
    assert_eq!(
        out.config.port, 4722,
        "the anchored file's setting must apply"
    );
    assert_eq!(
        PathBuf::from(&out.config.config_file),
        canonical(&startup.join(name)),
        "the env file must be the one under the startup cwd"
    );
    assert_eq!(out.music_root, canonical(&startup.join("music")));
}

/// The anchor is shared with the music root: a relative root resolves under the
/// startup cwd, and a root that exists nowhere under it is reported missing and
/// never created.
#[test]
fn relative_music_root_resolves_from_the_same_anchor() {
    let sb = Sandbox::new("anchorroot");
    let startup = sb.dir("startup");
    let cfg = sb.file("startup/conf.toml", "MusicFolder = \"music\"\n");
    let music = sb.dir("startup/music");

    let out = run(Some(cfg.to_str().unwrap()), Some(&startup), &[])
        .expect("the root must resolve from the startup cwd");
    assert_eq!(out.music_root, canonical(&music));

    let empty = sb.dir("startup-empty");
    let cfg2 = sb.file("startup-empty/conf.toml", "MusicFolder = \"music\"\n");
    let err = run(Some(cfg2.to_str().unwrap()), Some(&empty), &[])
        .expect_err("no music root under the startup cwd must be fatal");
    assert!(
        matches!(err, PreflightError::MusicRootMissing { .. }),
        "{err:?}"
    );
    assert!(!empty.join("music").exists(), "the root is never created");
}

/// An unresolvable startup cwd is fatal and names that cwd.
#[test]
fn unresolved_startup_cwd_is_fatal() {
    let sb = Sandbox::new("cwdunresolved");
    let missing = sb.join("no-such-dir");

    let err = run(Some("conf.toml"), Some(&missing), &[])
        .expect_err("an unresolvable startup cwd must be fatal");
    match err {
        PreflightError::StartupCwdUnresolved { path, .. } => {
            assert_eq!(path, missing.to_string_lossy());
        }
        other => panic!("expected StartupCwdUnresolved, got {other:?}"),
    }
}

/// A path whose name carries invalid UTF-8 bytes must never reach the loader
/// through a lossy conversion: `to_string_lossy` rewrites the bad byte to
/// U+FFFD, so a sibling directory *literally named* with that character would
/// be loaded instead of failing. Both tiers must refuse, and neither config may
/// be read.
///
/// Some filesystems (macOS/APFS) reject non-UTF8 names outright with
/// `EILLEGAL_BYTE_SEQUENCE`, so creation is the precondition; when the platform
/// refuses to create the directory the case degrades to an explicit, loud skip
/// rather than a silent pass.
#[cfg(unix)]
#[test]
fn non_utf8_config_path_is_refused_instead_of_loading_the_sibling() {
    use std::os::unix::ffi::OsStrExt;

    let sb = Sandbox::new("nonutf8");
    let bad = sb.root().join(std::ffi::OsStr::from_bytes(b"cfg-\xff-dir"));
    if let Err(e) = std::fs::create_dir(&bad) {
        if e.raw_os_error() == Some(41 /* EILLEGAL_BYTE_SEQUENCE */) || e.raw_os_error() == Some(92)
        {
            eprintln!(
                "SKIP non_utf8_config_path_is_refused: this filesystem refuses to \
                 create non-UTF8 names ({}); the checked conversion stays uncovered here",
                e
            );
            return;
        }
        panic!("create invalid-byte directory: {e}");
    }

    // The config inside the invalid-byte directory, and the sibling a lossy
    // conversion would silently name — each with a distinct non-default port so
    // loading either one is observable.
    std::fs::write(
        bad.join("conf.toml"),
        "MusicFolder = \"music\"\nPort = 4788\n",
    )
    .unwrap();
    std::fs::create_dir(bad.join("music")).unwrap();

    let lossy = sb.root().join("cfg-\u{fffd}-dir");
    assert!(
        lossy != bad,
        "test premise: the lossy name is a different path"
    );
    std::fs::create_dir(&lossy).expect("create replacement-character sibling");
    std::fs::write(
        lossy.join("conf.toml"),
        "MusicFolder = \"music\"\nPort = 4799\n",
    )
    .unwrap();
    std::fs::create_dir(sb.root().join("cfg-\u{fffd}-dir/music")).unwrap();

    // The invalid-byte directory is itself a usable startup cwd: it exists and
    // canonicalizes, so the refusal must come from the path conversion.
    let err = run(Some("conf.toml"), Some(&bad), &[])
        .expect_err("a flag path with invalid bytes must be refused");
    assert!(
        matches!(
            err,
            PreflightError::ConfigPathNotUtf8 {
                tier: "--config flag"
            }
        ),
        "{err:?}"
    );
    let text = format!("{} | {:?}", full_text(&err), err);
    assert!(
        !text.contains("4788"),
        "the refused config must not be read: {text}"
    );
    assert!(
        !text.contains("4799"),
        "the lossy sibling must not be loaded: {text}"
    );
    assert!(
        !text.contains('\u{fffd}'),
        "lossy path text must not be reported: {text}"
    );

    let err = run(None, Some(&bad), &[("ND_CONFIGFILE", "conf.toml")])
        .expect_err("an env path with invalid bytes must be refused");
    assert!(
        matches!(
            err,
            PreflightError::ConfigPathNotUtf8 {
                tier: "ND_CONFIGFILE"
            }
        ),
        "{err:?}"
    );
    let text = format!("{} | {:?}", full_text(&err), err);
    assert!(
        !text.contains("4788"),
        "the refused config must not be read: {text}"
    );
    assert!(
        !text.contains("4799"),
        "the lossy sibling must not be loaded: {text}"
    );

    // Refusing must not read, create, or remove anything.
    assert!(bad.join("conf.toml").exists() && lossy.join("conf.toml").exists());
}

// ------------------------------------------------------ invalid config first

/// Unknown keys are fatal, and the failure precedes any filesystem creation.
#[test]
fn invalid_config_is_fatal_before_any_creation() {
    let sb = Sandbox::new("badconfig");
    let data = sb.join("data");
    let cfg = sb.file("conf.toml", "[Whatever]\nFoo = \"bar\"\n");

    let out = preflight(PreflightOptions {
        config_file: Some(cfg.to_str().unwrap()),
        env: Some(Box::new(lookup(env_of(&[])))),
        startup_cwd: Some(sb.root()),
        effective_uid: Some(1000),
        allow_zero_port: false,
    })
    .expect_err("unknown key must be fatal");

    match &out {
        PreflightError::ConfigLoad { class, .. } => {
            assert_eq!(*class, ConfigErrorClass::UnknownKey, "got {out:?}");
        }
        other => panic!("expected ConfigLoad, got {other:?}"),
    }
    assert!(
        !data.exists(),
        "no data directory may be created when config validation fails"
    );
}

/// A malformed line carrying a credential must not echo the credential, but
/// must still name the class and a numeric byte offset from the parse span.
#[test]
fn malformed_config_error_reports_class_and_location_not_the_line() {
    let sb = Sandbox::new("secretline");
    // Unquoted value on a password line: toml renders that line in its error.
    let cfg = sb.file(
        "conf.toml",
        "DevAutoCreateAdminPassword = s3cret-not-a-literal\n",
    );

    let err = preflight(PreflightOptions {
        config_file: Some(cfg.to_str().unwrap()),
        env: Some(Box::new(lookup(env_of(&[])))),
        startup_cwd: Some(sb.root()),
        effective_uid: Some(1000),
        allow_zero_port: false,
    })
    .expect_err("unquoted TOML value must be fatal");

    assert!(matches!(err, PreflightError::ConfigLoad { .. }));
    let text = full_text(&err);
    assert!(
        !text.contains("s3cret-not-a-literal"),
        "config error must not quote the source line: {text}"
    );
    assert!(
        !text.contains("DevAutoCreateAdminPassword"),
        "the offending key name must not be echoed either: {text}"
    );
    assert!(
        text.contains("not valid TOML"),
        "class still reported: {text}"
    );
    assert!(
        text.contains("at byte offset 29"),
        "offset from the parse span still reported: {text}"
    );
    assert!(
        !text.contains(" at line") && !text.contains("column"),
        "no line/column is derived, since that would require re-reading: {text}"
    );
    assert!(
        text.contains("conf.toml"),
        "which file failed is still reported: {text}"
    );
    assert!(
        std::error::Error::source(&err).is_none(),
        "no source: the upstream toml error must not be reachable"
    );
}

/// A **wrong-type numeric password** is the canonical leak canary: `nd-conf`
/// reports it as `Invalid` with the raw value embedded, so a forwarding
/// sanitizer would print the credential.
#[test]
fn wrong_type_numeric_password_is_not_echoed() {
    let sb = Sandbox::new("numsecret");
    // A TOML integer: the loader reaches its serde type check and embeds the
    // raw value in `ConfigError::Invalid`, which is the leak being prevented.
    let canary = "4242424242";
    let cfg = sb.file(
        "conf.toml",
        &format!("MusicFolder = \"/tmp\"\nDevAutoCreateAdminPassword = {canary}\n"),
    );

    let err = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[])
        .expect_err("a numeric password is a type error and must be fatal");
    let class = match &err {
        PreflightError::ConfigLoad { class, .. } => *class,
        other => panic!("expected ConfigLoad, got {other:?}"),
    };
    assert_eq!(class, ConfigErrorClass::InvalidValue);

    // Display, Debug and the source chain are all checked: a nested source
    // would re-expose the loader's raw text.
    for rendered in [full_text(&err), format!("{err:?}")] {
        assert!(!rendered.contains(canary), "canary leaked in {rendered}");
        assert!(
            !rendered.contains("dev_auto_create_admin_password"),
            "field name leaked: {rendered}"
        );
    }
    assert!(err.to_string().contains("failed validation"));
    assert!(std::error::Error::source(&err).is_none());
}

/// The same canary on the secret-bearing `LastFM` fields, whose values `Config`
/// also derives `Debug` over.
#[test]
fn wrong_type_secret_fields_are_not_echoed() {
    let sb = Sandbox::new("numlastfm");
    let cfg = sb.file(
        "conf.toml",
        concat!(
            "MusicFolder = \"/tmp\"\n",
            "[LastFm]\n",
            "ApiKey = 99887766\n",
            "Secret = 55443322\n",
        ),
    );

    let err = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[])
        .expect_err("numeric API key/secret must be fatal");
    let text = format!("{} | {:?}", full_text(&err), err);
    for value in ["99887766", "55443322", "last_fm.api_key", "last_fm.secret"] {
        assert!(!text.contains(value), "leaked {value} in {text}");
    }
}

/// An **invalid env value** is the other leak path: `ND_*` values flow into the
/// same `Invalid` message, so an injected credential-bearing env value must
/// never surface.
#[test]
fn invalid_env_value_is_not_echoed() {
    let sb = Sandbox::new("badenv");
    let music = sb.dir("music");
    let cfg = sb.file("conf.toml", &toml_with(music.to_str().unwrap(), 4650));
    let canary = "canary-purge-value-not-printed";

    let err = run(
        Some(cfg.to_str().unwrap()),
        Some(sb.root()),
        &[("ND_SCANNER_PURGEMISSING", canary)],
    )
    .expect_err("an invalid Scanner.PurgeMissing must be fatal");
    let class = match &err {
        PreflightError::ConfigLoad { class, .. } => *class,
        other => panic!("expected ConfigLoad, got {other:?}"),
    };
    assert_eq!(class, ConfigErrorClass::InvalidValue);

    let text = format!("{} | {:?}", full_text(&err), err);
    assert!(
        !text.contains(canary),
        "env value leaked into the error: {text}"
    );
    assert!(std::error::Error::source(&err).is_none());
}

/// A validation error whose upstream text embeds a full URL with credentials
/// must report only the class — the previous userinfo-scrubbing sanitizer is
/// replaced by withholding the value entirely.
#[test]
fn url_with_userinfo_is_not_echoed_at_all() {
    let sb = Sandbox::new("userinfo");
    let cfg = sb.file(
        "conf.toml",
        "[ExtAuth]\nLogoutURL = \"ftp://user:pw@example.com\"\n",
    );

    let err = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[])
        .expect_err("a non-http LogoutURL must be fatal");
    assert!(matches!(err, PreflightError::ConfigLoad { .. }));
    let text = format!("{} | {:?}", full_text(&err), err);
    for secret in ["pw@", "user:", "ftp://", "example.com"] {
        assert!(
            !text.contains(secret),
            "URL material leaked {secret}: {text}"
        );
    }
    assert!(
        text.contains("failed validation"),
        "class still useful: {text}"
    );
}

/// The same for `BaseURL`, whose rejection text embeds the whole value —
/// including userinfo when the scheme is missing.
#[test]
fn base_url_with_userinfo_is_not_echoed() {
    let sb = Sandbox::new("baseurl");
    let cfg = sb.file(
        "conf.toml",
        "MusicFolder = \"/tmp\"\nBaseURL = \"canaryuser:canarypw@canary.example\"\n",
    );

    let err = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[])
        .expect_err("an invalid BaseURL must be fatal");
    let text = format!("{} | {:?}", full_text(&err), err);
    assert!(
        !text.contains("canarypw") && !text.contains("canaryuser"),
        "userinfo leaked: {text}"
    );
}

/// An invalid crontab schedule and an invalid byte size both quote their value
/// upstream; neither may surface.
#[test]
fn schedule_and_byte_size_values_are_not_echoed() {
    let sb = Sandbox::new("schedbytes");
    let cases: [(&str, &str); 2] = [
        (
            "sched.toml",
            "MusicFolder = \"/tmp\"\n[Scanner]\nSchedule = \"canary-not-cron\"\n",
        ),
        (
            "bytes.toml",
            "MusicFolder = \"/tmp\"\nMaxImageSize = \"canary-bogus\"\n",
        ),
    ];
    for (name, contents) in cases {
        let cfg = sb.file(name, contents);
        let err = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[]).unwrap_err();
        let class = match &err {
            PreflightError::ConfigLoad { class, .. } => *class,
            other => panic!("{name}: expected ConfigLoad, got {other:?}"),
        };
        assert_eq!(class, ConfigErrorClass::InvalidValue, "{name}");
        let text = format!("{} | {:?}", full_text(&err), err);
        assert!(
            !text.contains("canary-not-cron") && !text.contains("canary-bogus"),
            "{name} leaked its value: {text}"
        );
    }
}

/// The config file path is still reported when the default tier ran, and the
/// A root-level `ND_` duplicate is reported as its own class, and neither key
/// name is echoed.
#[test]
fn duplicate_nd_key_class_is_reported_without_key_text() {
    let sb = Sandbox::new("dupnd");
    let cfg = sb.file("conf.toml", "Port = 1\nND_Port = 2\n");

    let err = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[])
        .expect_err("a canonical key alongside its ND_ spelling must be fatal");
    match &err {
        PreflightError::ConfigLoad { class, path, .. } => {
            assert_eq!(*class, ConfigErrorClass::DuplicateNdKey);
            assert_eq!(path, cfg.to_str().unwrap(), "file still named");
        }
        other => panic!("expected ConfigLoad, got {other:?}"),
    }
    assert!(err.to_string().contains("ND_-prefixed"));
    // The loader's upstream text names both keys (`'ND_PORT'` and `'Port'`);
    // neither may survive here.
    let text = format!("{} | {:?}", full_text(&err), err);
    assert!(
        !text.contains("ND_PORT") && !text.contains("'Port'"),
        "leaked: {text}"
    );
}

// -------------------------------------------------------- boot-value checks

#[test]
fn port_must_be_in_production_range() {
    let sb = Sandbox::new("port");
    let music = sb.dir("music");

    let cases: Vec<(i64, PreflightError)> = vec![
        (65536, PreflightError::InvalidPort { value: 65536 }),
        (-1, PreflightError::InvalidPort { value: -1 }),
        (0, PreflightError::ZeroPort),
    ];
    for (port, expected) in cases {
        let cfg = sb.file(
            &format!("conf-{port}.toml"),
            &toml_with(music.to_str().unwrap(), port),
        );
        let err = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[])
            .expect_err("port must be rejected");
        assert_eq!(err, expected, "port {port}");
    }
    assert_eq!(music.read_dir().unwrap().count(), 0, "root stayed empty");

    // Boundary values accepted.
    for port in [1_i64, 65535] {
        let cfg = sb.file(
            &format!("conf-ok-{port}.toml"),
            &toml_with(music.to_str().unwrap(), port),
        );
        let out = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[])
            .unwrap_or_else(|e| panic!("port {port} must be accepted: {e}"));
        assert_eq!(out.config.port, port);
    }
}

/// `0` is only meaningful as an explicit test-injection port.
#[test]
fn zero_port_requires_explicit_test_injection() {
    let sb = Sandbox::new("zeroport");
    let music = sb.dir("music");
    let cfg = sb.file("conf.toml", &toml_with(music.to_str().unwrap(), 0));
    let map = env_of(&[]);

    let out = preflight(PreflightOptions {
        config_file: Some(cfg.to_str().unwrap()),
        env: Some(Box::new(lookup(map))),
        startup_cwd: Some(sb.root()),
        effective_uid: Some(1000),
        allow_zero_port: true,
    })
    .expect("allow_zero_port must accept port 0");
    assert_eq!(out.config.port, 0);
}

#[test]
fn auth_limit_and_window_must_be_positive() {
    let sb = Sandbox::new("authlimit");
    let music = sb.dir("music");

    let cfg = sb.file(
        "limit.toml",
        &format!(
            "MusicFolder = \"{}\"\nAuthRequestLimit = 0\n",
            music.to_str().unwrap()
        ),
    );
    let err = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[])
        .expect_err("AuthRequestLimit 0 must be rejected");
    assert_eq!(err, PreflightError::InvalidAuthRequestLimit { value: 0 });

    // AuthWindowLength is normalized to the default when negative, so a
    // literal zero is the reachable invalid case.
    let cfg = sb.file(
        "window.toml",
        &format!(
            "MusicFolder = \"{}\"\nAuthWindowLength = \"0s\"\n",
            music.to_str().unwrap()
        ),
    );
    let err = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[])
        .expect_err("AuthWindowLength 0 must be rejected");
    assert_eq!(err, PreflightError::InvalidAuthWindowLength);
}

/// Durations convert to whole seconds with checked units: a sub-second
/// configured window must not silently become 0.
#[test]
fn duration_conversion_is_checked_not_truncated() {
    let sb = Sandbox::new("duration");
    let music = sb.dir("music");

    let cfg = sb.file(
        "subsec.toml",
        &format!(
            "MusicFolder = \"{}\"\nSessionTimeout = \"1500ms\"\n",
            music.to_str().unwrap()
        ),
    );
    let err = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[])
        .expect_err("truncating 1.5s to 1s would change the policy value");
    assert!(
        matches!(err, PreflightError::DurationConversion { .. }),
        "{err:?}"
    );

    // Exact whole-second values convert, and the outcome carries them.
    let cfg = sb.file(
        "ok.toml",
        &format!(
            "MusicFolder = \"{}\"\nSessionTimeout = \"90m\"\nAuthWindowLength = \"45s\"\n",
            music.to_str().unwrap()
        ),
    );
    let out = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[]).unwrap();
    assert_eq!(out.session_timeout_secs, 5400);
    assert_eq!(out.auth_window_secs, 45);

    // Defaults carry through unchanged (48h / 20s).
    let out = run(None, Some(sb.root()), &[]).expect("defaults must preflight cleanly");
    assert_eq!(
        out.session_timeout_secs,
        Duration::from_secs(48 * 3600).as_secs() as i64
    );
    assert_eq!(out.auth_window_secs, 20);
}

/// Unsupported transports must refuse rather than be silently ignored.
#[test]
fn tls_and_unix_socket_are_refused_before_listening() {
    let sb = Sandbox::new("transport");
    let music = sb.dir("music");

    let cfg = sb.file(
        "tls.toml",
        &format!(
            "MusicFolder = \"{}\"\nTLSCert = \"/tmp/cert.pem\"\nTLSKey = \"/tmp/key.pem\"\n",
            music.to_str().unwrap()
        ),
    );
    let err = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[])
        .expect_err("TLS cannot be silently downgraded to HTTP");
    assert!(matches!(err, PreflightError::UnsupportedTls), "{err:?}");

    let cfg = sb.file(
        "unix.toml",
        &format!(
            "MusicFolder = \"{}\"\nAddress = \"unix:/tmp/nd.sock\"\n",
            music.to_str().unwrap()
        ),
    );
    let err = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[])
        .expect_err("a unix socket address cannot be silently served over TCP");
    assert!(
        matches!(err, PreflightError::UnsupportedUnixSocket),
        "{err:?}"
    );
}

/// The unsupported-socket error carries no address at all, so neither a
/// URL-shaped address nor its userinfo can reach `Display` or `Debug`.
#[test]
fn unix_socket_error_carries_no_address() {
    let sb = Sandbox::new("addrsecret");
    let music = sb.dir("music");
    let cfg = sb.file(
        "unix.toml",
        &format!(
            "MusicFolder = \"{}\"\nAddress = \"unix://addruser:addrpw@host/x?token=socket-canary\"\n",
            music.to_str().unwrap()
        ),
    );

    let err = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[])
        .expect_err("a unix address must be refused");
    assert!(
        matches!(err, PreflightError::UnsupportedUnixSocket),
        "{err:?}"
    );
    let text = format!("{} | {:?}", full_text(&err), err);
    assert!(
        !text.contains("addrpw") && !text.contains("addruser"),
        "address userinfo leaked: {text}"
    );
    assert!(
        !text.contains("socket-canary"),
        "address query value leaked: {text}"
    );
    assert!(
        !text.contains("host/x") && !text.contains("unix://"),
        "the raw address must not be carried at all: {text}"
    );
    assert!(text.contains("unix socket"), "class still reported: {text}");
}

// ----------------------------------------------------- root-user restriction

#[cfg(unix)]
#[test]
fn enforce_non_root_user_rejects_uid_zero() {
    let sb = Sandbox::new("root");
    let music = sb.dir("music");
    let cfg = sb.file(
        "conf.toml",
        &format!(
            "MusicFolder = \"{}\"\nEnforceNonRootUser = true\n",
            music.to_str().unwrap()
        ),
    );
    let map = env_of(&[]);

    let err = preflight(PreflightOptions {
        config_file: Some(cfg.to_str().unwrap()),
        env: Some(Box::new(lookup(map.clone()))),
        startup_cwd: Some(sb.root()),
        effective_uid: Some(0),
        allow_zero_port: false,
    })
    .expect_err("uid 0 must be refused when EnforceNonRootUser is set");
    assert_eq!(err, PreflightError::RunningAsRoot);

    let out = preflight(PreflightOptions {
        config_file: Some(cfg.to_str().unwrap()),
        env: Some(Box::new(lookup(map))),
        startup_cwd: Some(sb.root()),
        effective_uid: Some(1000),
        allow_zero_port: false,
    })
    .expect("a non-root uid must pass");
    assert_eq!(out.music_root, canonical(&music));
}

/// The root check runs before the music-root/filesystem stage, matching Go's
/// ordering (`conf/configuration.go:386`).
#[cfg(unix)]
#[test]
fn root_check_precedes_music_root_resolution() {
    let sb = Sandbox::new("rootorder");
    // No music directory at all: if the root check ran after resolution we
    // would see MusicRootMissing instead of RunningAsRoot.
    let cfg = sb.file(
        "conf.toml",
        "MusicFolder = \"music\"\nEnforceNonRootUser = true\n",
    );

    let err = preflight(PreflightOptions {
        config_file: Some(cfg.to_str().unwrap()),
        env: Some(Box::new(lookup(env_of(&[])))),
        startup_cwd: Some(sb.root()),
        effective_uid: Some(0),
        allow_zero_port: false,
    })
    .expect_err("uid 0 must be refused");
    assert_eq!(err, PreflightError::RunningAsRoot);
    assert!(
        !sb.join("music").exists(),
        "the refused startup must not have created the music root"
    );
}

/// Without the flag, uid 0 is allowed (Go default `EnforceNonRootUser=false`).
#[test]
fn non_root_enforcement_is_off_by_default() {
    let sb = Sandbox::new("rootoff");
    let music = sb.dir("music");

    let out = preflight(PreflightOptions {
        config_file: None,
        env: Some(Box::new(lookup(env_of(&[])))),
        startup_cwd: Some(sb.root()),
        effective_uid: Some(0),
        allow_zero_port: false,
    })
    .expect("uid 0 is allowed when EnforceNonRootUser is false");
    assert_eq!(out.music_root, canonical(&music));
}

// ------------------------------------------------------- music-root handling

/// A relative root resolves against the supplied startup cwd, not the
/// process cwd, and the returned path is canonical.
#[test]
fn relative_root_resolves_from_supplied_cwd() {
    let sb = Sandbox::new("relative");
    let music = sb.dir("music");

    let cfg_file = sb.file("conf.toml", "MusicFolder = \"music\"\n");
    let out = run(Some(cfg_file.to_str().unwrap()), Some(sb.root()), &[]).unwrap();
    assert_eq!(out.music_root, canonical(&music));
    assert!(out.music_root.is_absolute());
    assert_eq!(out.startup_cwd, canonical(sb.root()));
}

/// The real process cwd is never consulted for the music root: the relative
/// root resolves under the injected anchor even though the real cwd has no such
/// directory.
#[test]
fn relative_root_resolves_from_anchor_not_process_cwd() {
    let sb = Sandbox::new("proccwd");
    let music = sb.dir("deep/music");
    let cfg = sb.file("conf.toml", "MusicFolder = \"deep/music\"\n");

    let out = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[]).unwrap();
    assert_eq!(out.music_root, canonical(&music));
    assert!(
        !Path::new("deep/music").exists(),
        "test premise: the real cwd has no deep/music"
    );
}

/// A trailing slash on the configured root is normalized away.
#[test]
fn trailing_slash_root_is_normalized() {
    let sb = Sandbox::new("slash");
    let music = sb.dir("music");
    let configured = format!("{}/", music.to_str().unwrap());
    let cfg = sb.file("conf.toml", &format!("MusicFolder = \"{configured}\"\n"));

    let out = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[]).unwrap();
    assert_eq!(out.music_root, canonical(&music));
    assert!(
        !out.music_root.to_string_lossy().ends_with('/'),
        "canonical root must have no trailing separator"
    );
}

/// `..` components are collapsed by canonicalization.
#[test]
fn dot_dot_components_are_collapsed() {
    let sb = Sandbox::new("dotdot");
    let music = sb.dir("music");
    std::fs::create_dir_all(sb.join("music/nested")).unwrap();
    let configured = format!("{}/nested/..", music.to_str().unwrap());
    let cfg = sb.file("conf.toml", &format!("MusicFolder = \"{configured}\"\n"));

    let out = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[]).unwrap();
    assert_eq!(out.music_root, canonical(&music));
}

/// A missing root fails nonzero-ish and — the point of the `Dir::path` hazard
/// — the directory must still be absent afterwards.
#[test]
fn missing_root_fails_and_stays_absent() {
    let sb = Sandbox::new("missing");
    let ghost = sb.join("music");
    assert!(!ghost.exists());

    let err = run(None, Some(sb.root()), &[]).expect_err("a missing music root must fail");
    assert!(
        matches!(err, PreflightError::MusicRootMissing { .. }),
        "{err:?}"
    );
    assert!(err.to_string().contains("never creates"));
    assert!(!ghost.exists(), "the music root must never be auto-created");
    assert!(
        !sb.join("data").exists() && !sb.join("cache").exists(),
        "a failed preflight must not create data or cache directories either"
    );
}

/// A regular file at the configured root is reported as not-a-directory and
/// is left untouched.
#[test]
fn regular_file_root_is_not_a_directory() {
    let sb = Sandbox::new("file");
    let file = sb.file("music", "not a directory");

    let err = run(None, Some(sb.root()), &[]).expect_err("a file cannot be the music root");
    assert!(
        matches!(err, PreflightError::MusicRootNotDirectory { .. }),
        "{err:?}"
    );
    assert_eq!(
        std::fs::metadata(&file).unwrap().len(),
        "not a directory".len() as u64,
        "the file must be untouched"
    );
    assert!(file.is_file(), "the file must still be a regular file");
}

/// A configured root with no read permission is refused.
///
/// Limitation, stated rather than hidden: this asserts on a non-root euid
/// only. Running the suite as uid 0 (or under a sandbox that grants access
/// regardless of mode bits) makes `read_dir` succeed even at mode 000, so the
/// case degrades to an explicit skip; the skip is loud, not silent.
#[cfg(unix)]
#[test]
fn unreadable_root_is_refused_when_mode_bits_are_enforced() {
    use std::os::unix::fs::PermissionsExt;

    let sb = Sandbox::new("unreadable");
    let music = sb.dir("music");
    std::fs::set_permissions(&music, std::fs::Permissions::from_mode(0o000)).unwrap();

    let readable = std::fs::read_dir(&music).is_ok();
    if readable {
        // euid 0 / permissive sandbox: the mode bits are not enforced, so the
        // permission path cannot be exercised here. Restore and report.
        let _ = std::fs::set_permissions(&music, std::fs::Permissions::from_mode(0o755));
        eprintln!(
            "SKIP unreadable_root_is_refused: mode 000 is still readable here \
             (privileged euid or permissive sandbox), so the permission path \
             cannot be exercised in this environment"
        );
        return;
    }

    let err = run(None, Some(sb.root()), &[]).expect_err("an unreadable root must be refused");
    let _ = std::fs::set_permissions(&music, std::fs::Permissions::from_mode(0o755));
    assert!(
        matches!(err, PreflightError::MusicRootNotReadable { .. }),
        "{err:?}"
    );
}

/// A directory readable but whose *entries* are unreadable still preflight
/// at bootstrap: step 2 requires a readable root, and per-entry errors belong
/// to the scanner (step 5+), not startup.
#[cfg(unix)]
#[test]
fn root_with_unreadable_child_directory_is_accepted() {
    use std::os::unix::fs::PermissionsExt;

    let sb = Sandbox::new("childperm");
    let music = sb.dir("music");
    let locked = sb.dir("music/locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

    let out = run(None, Some(sb.root()), &[]);
    let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));
    match out {
        Ok(o) => assert_eq!(o.music_root, canonical(&music)),
        Err(e) => panic!("root itself is readable, so startup must not fail: {e:?}"),
    }
}

/// Symlink policy at bootstrap is an open launch decision (`boot-api.md:200`,
/// scout-memo BLOCKER C). Increment A pins the canonicalization half: a
/// symlinked root is reported as its canonical target, which is what step 5's
/// stored-root comparison and the SC stream boundary consume. Rejecting
/// external symlink targets is the stream-side rule, not a startup one.
#[test]
fn symlinked_root_is_canonicalized_to_its_target() {
    let sb = Sandbox::new("symlink");
    let real = sb.dir("real");
    let link = sb.join("link");
    std::os::unix::fs::symlink(&real, &link).expect("create symlink root");

    let cfg = sb.file(
        "conf.toml",
        &format!("MusicFolder = \"{}\"\n", link.to_str().unwrap()),
    );
    let out = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[]).unwrap();

    assert_eq!(
        out.music_root,
        canonical(&real),
        "the canonical target, not the link path, is what later stored-root comparison needs"
    );
    assert_ne!(
        out.music_root, link,
        "the returned root must not be the unresolved symlink"
    );
    assert!(!std::fs::metadata(&link).unwrap().is_file());
    assert!(canonical(&real).is_dir());
}

/// A broken symlink reports the missing target and creates nothing.
#[test]
fn broken_symlink_root_is_missing() {
    let sb = Sandbox::new("brokenlink");
    let link = sb.join("link");
    std::os::unix::fs::symlink(sb.join("nowhere"), &link).expect("create broken symlink");

    let cfg = sb.file(
        "conf.toml",
        &format!("MusicFolder = \"{}\"\n", link.to_str().unwrap()),
    );
    let err = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[])
        .expect_err("a broken symlink is not a usable root");
    assert!(
        matches!(err, PreflightError::MusicRootMissing { .. }),
        "{err:?}"
    );
    assert!(
        !sb.join("nowhere").exists(),
        "the broken target must stay absent"
    );
}

/// Unicode roots round-trip; canonicalization preserves them exactly.
#[test]
fn unicode_root_roundtrips() {
    let sb = Sandbox::new("unicode");
    // Accented, CJK, emoji, and a combining-mark sequence.
    let music = sb.dir("música/音乐/🎵/naïve");
    let cfg = sb.file(
        "conf.toml",
        &format!("MusicFolder = \"{}\"\n", music.to_str().unwrap()),
    );

    let out = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[]).unwrap();
    assert_eq!(out.music_root, canonical(&music));
    assert!(
        out.music_root.to_string_lossy().contains("🎵"),
        "Unicode components survive canonicalization"
    );

    // Relative Unicode root.
    let rel = sb.dir("café");
    let cfg = sb.file("rel.toml", "MusicFolder = \"café\"\n");
    let out = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[]).unwrap();
    assert_eq!(out.music_root, canonical(&rel));
}

/// A Unicode path component survives the anchor, so relative config paths and
/// relative roots behave identically under non-ASCII names.
#[test]
fn unicode_anchor_roundtrips() {
    let sb = Sandbox::new("unicodecwd");
    let startup = sb.dir("startup-é");
    std::fs::write(
        startup.join("conf.toml"),
        "MusicFolder = \"music\"\nPort = 4733\n",
    )
    .unwrap();
    let music = sb.dir("startup-é/music");

    let out = run(None, Some(&startup), &[("ND_CONFIGFILE", "conf.toml")])
        .expect("Unicode relative config path must resolve from the Unicode startup cwd");
    assert_eq!(out.config.port, 4733);
    assert_eq!(out.music_root, canonical(&music));
    assert!(out.config.config_file.contains("startup-é"));
}

/// An empty configured root is refused before any path work.
#[test]
fn empty_music_folder_is_refused() {
    let sb = Sandbox::new("emptyfolder");
    let cfg = sb.file("conf.toml", "MusicFolder = \"\"\n");

    let err = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[])
        .expect_err("empty music folder must be refused");
    assert_eq!(err, PreflightError::EmptyMusicFolder);
}

// ------------------------------------------------------------ outcome shape

/// The outcome carries the committed config plus the resolved paths and
/// checked seconds that later bootstrap steps consume.
#[test]
fn outcome_exposes_paths_and_checked_seconds() {
    let sb = Sandbox::new("shape");
    let music = sb.dir("music");
    let data = sb.dir("data");
    let cfg = sb.file(
        "conf.toml",
        &format!(
            "MusicFolder = \"{}\"\nDataFolder = \"{}\"\nPort = 4600\n",
            music.to_str().unwrap(),
            data.to_str().unwrap()
        ),
    );

    let out = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[]).unwrap();
    assert_eq!(out.music_root, canonical(&music));
    assert_eq!(out.data_folder, data);
    assert_eq!(out.config.port, 4600);
    assert_eq!(out.config_source, ConfigSource::Flag);
    assert_eq!(out.session_timeout_secs, 172800);
    assert_eq!(out.auth_window_secs, 20);
    // The DB string is handed on verbatim: its URI query is not path
    // characters, and D-OPEN owns the resolution.
    assert!(
        out.db_path.contains("?") && out.db_path.contains("journal_mode=WAL"),
        "db_path keeps its default URI parameters: {}",
        out.db_path
    );
    assert!(out.db_path.starts_with(data.to_str().unwrap()));
    assert!(
        !out.data_folder.join("cache").exists(),
        "preflight must not create the cache directory"
    );
}

/// A relative `DataFolder` resolves against the same anchor, and the loader's
/// relative `db_path` string is handed on verbatim.
#[test]
fn relative_data_folder_resolves_from_the_anchor() {
    let sb = Sandbox::new("reldata");
    let startup = sb.dir("startup");
    let cfg = sb.file(
        "startup/conf.toml",
        "MusicFolder = \"music\"\nDataFolder = \"data\"\n",
    );
    sb.dir("startup/music");

    let out = run(Some(cfg.to_str().unwrap()), Some(&startup), &[])
        .expect("relative DataFolder must resolve from the anchor");
    assert_eq!(out.data_folder, canonical(&startup).join("data"));
    // `db_path` is the loader's verbatim string: a relative DataFolder yields a
    // relative SQLite string, and D-OPEN owns that resolution.
    assert!(
        out.db_path.starts_with("data"),
        "db_path is the loader's verbatim string: {}",
        out.db_path
    );
    assert!(
        !out.data_folder.exists(),
        "the data folder is reported, never created"
    );
}

/// `Debug` on the outcome must not expose secret-bearing config fields.
#[test]
fn debug_never_exposes_secret_config_fields() {
    let sb = Sandbox::new("debug");
    let music = sb.dir("music");
    let cfg = sb.file(
        "conf.toml",
        &format!(
            concat!(
                "MusicFolder = \"{}\"\n",
                "DevAutoCreateAdminPassword = \"dev-pass-do-not-print\"\n",
                "[LastFm]\nApiKey = \"lastfm-key-do-not-print\"\n",
                "Secret = \"lastfm-secret-do-not-print\"\n",
            ),
            music.to_str().unwrap()
        ),
    );

    let out = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[]).unwrap();
    let text = format!("{out:?}");
    for secret in [
        "dev-pass-do-not-print",
        "lastfm-key-do-not-print",
        "lastfm-secret-do-not-print",
    ] {
        assert!(
            !text.contains(secret),
            "Debug leaked a secret value: {secret}"
        );
    }
    assert!(text.contains("music_root"), "non-secret facts still shown");
    assert!(
        !text.contains("address"),
        "Debug must omit the caller-supplied address entirely: {text}"
    );
    assert!(
        text.contains("config_source"),
        "resolution facts still shown"
    );

    // PreflightOptions hides the injected environment too.
    let opts = PreflightOptions {
        config_file: Some("x.toml"),
        env: Some(Box::new(|_| Some("ND_SESSIONPASSWORD=hunter".to_string()))),
        startup_cwd: None,
        effective_uid: Some(1000),
        allow_zero_port: false,
    };
    let text = format!("{opts:?}");
    assert!(
        !text.contains("hunter"),
        "options Debug must not print env values"
    );
    assert!(text.contains("<injected lookup>"));
}

/// `DbPath` is a caller-supplied SQLite URI whose query can carry credentials
/// (`DbPath = "file:x.db?password=…"`). `Debug` omits `db_path` entirely
/// rather than printing part of it, while the field itself stays verbatim for
/// D-OPEN.
#[test]
fn outcome_debug_omits_db_path_entirely() {
    let sb = Sandbox::new("dburi");
    let music = sb.dir("music");
    let data = sb.dir("data");
    let cfg = sb.file(
        "conf.toml",
        &format!(
            "MusicFolder = \"{}\"\nDataFolder = \"{}\"\n",
            music.to_str().unwrap(),
            data.to_str().unwrap()
        ),
    );

    let out = run(
        Some(cfg.to_str().unwrap()),
        Some(sb.root()),
        &[("ND_DBPATH", "file:/tmp/nd.db?password=db-canary-pw")],
    )
    .unwrap_or_else(|e| panic!("a configured DbPath must preflight: {e:?}"));

    // The verbatim URI is still handed to later bootstrap (D-OPEN contract).
    assert!(
        out.db_path.contains("password=db-canary-pw"),
        "db_path must stay verbatim: {}",
        out.db_path
    );

    let text = format!("{out:?}");
    assert!(
        !text.contains("db-canary-pw"),
        "Debug leaked the DB URI query: {text}"
    );
    assert!(
        !text.contains("db_path"),
        "Debug must not print the db_path field at all: {text}"
    );
    assert!(
        !text.contains("/tmp/nd.db"),
        "Debug must not print even the path half of the URI: {text}"
    );
}

/// `Config` itself derives `Debug` over secrets and over `DbPath`, so this
/// documents the boundary the preflight must respect: the outcome's `Debug` is
/// the only redacted view, and callers must not print `config` directly.
#[test]
fn config_debug_is_the_unredacted_boundary_documented_by_the_module() {
    let sb = Sandbox::new("configdebug");
    let music = sb.dir("music");
    let cfg = sb.file(
        "conf.toml",
        &format!(
            "MusicFolder = \"{}\"\nDbPath = \"file:/tmp/nd.db?password=db-canary-pw\"\n",
            music.to_str().unwrap()
        ),
    );

    let out = run(Some(cfg.to_str().unwrap()), Some(sb.root()), &[]).unwrap();
    // The outcome is safe…
    assert!(
        !format!("{out:?}").contains("db-canary-pw"),
        "outcome Debug must withhold the query"
    );
    // …while `Config` is not, which is why the outcome never delegates to it.
    assert!(
        format!("{:?}", out.config).contains("db-canary-pw"),
        "test premise: Config's derived Debug is unredacted"
    );
}

/// A successful preflight leaves the filesystem exactly as it found it:
/// nothing created, nothing removed.
#[test]
fn successful_preflight_creates_nothing() {
    let sb = Sandbox::new("readonly");
    let music = sb.dir("music");
    std::fs::write(music.join("a.flac"), b"fake").unwrap();

    let before = count_entries(sb.root());
    let out = run(None, Some(sb.root()), &[("ND_PORT", "4611")]).unwrap();
    let after = count_entries(sb.root());

    assert_eq!(
        before, after,
        "preflight is read-only: no directory or file created"
    );
    assert_eq!(out.config.port, 4611, "env tier still applies to the port");
    assert!(
        out.data_folder.exists(),
        "default DataFolder is the cwd, which exists"
    );
}

/// A failed config load also creates nothing, and reports the default tier as
/// `(defaults)` rather than an empty path, so the log line stays actionable.
#[test]
fn failed_default_tier_load_names_the_tier_and_creates_nothing() {
    let sb = Sandbox::new("defaultsfail");
    let before = count_entries(sb.root());

    let err = run(
        None,
        Some(sb.root()),
        &[("ND_SCANNER_PURGEMISSING", "canary-value")],
    )
    .expect_err("an invalid env value must be fatal");
    match &err {
        PreflightError::ConfigLoad { path, class, .. } => {
            assert_eq!(path, "(defaults)", "tier named rather than blank: {err:?}");
            assert_eq!(*class, ConfigErrorClass::InvalidValue);
        }
        other => panic!("expected ConfigLoad, got {other:?}"),
    }
    assert!(!full_text(&err).contains("canary-value"));
    assert_eq!(count_entries(sb.root()), before, "read-only on failure too");
}

fn count_entries(root: &Path) -> usize {
    let mut n = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                n += 1;
                if e.path().is_dir() {
                    stack.push(e.path());
                }
            }
        }
    }
    n
}
