//! Issue #03 test suite: key-set snapshot, unknown-key fatality,
//! env-override precedence, defaults spot-checks, release-TOML load.
//! Mirrors `conf/configtest/` + `conf/testdata/` fixtures.

use std::collections::HashMap;
use std::time::Duration;

use nd_conf::{Config, ConfigError};

fn write_tmp(name: &str, contents: &str) -> String {
    let dir = std::env::temp_dir().join(format!("nd-conf-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, contents).unwrap();
    p.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------- key set

/// Snapshot of the fresh key set (docs/rust-port/config-contract.md §1–§4).
/// Any diff here is a deliberate key-set change that must be reviewed.
#[test]
fn key_set_snapshot() {
    let mut keys: Vec<String> = nd_conf::keys::all_accepted_lower_keys();
    keys.sort_unstable();
    let snapshot = keys.join(",");
    let expected = "address,agents,albumplaycountmode,artistartpriority,artistimagefolder,authrequestlimit,authwindowlength,autoimportplaylists,autotranscodedownload,backup.count,backup.path,backup.schedule,basehost,basepath,basescheme,baseurl,cachefolder,coverartpriority,coverartquality,datafolder,dbpath,deezer.enabled,deezer.language,defaultdownloadableshare,defaultdownsamplingformat,defaultlanguage,defaultplaylistpublicvisibility,defaultshareexpiration,defaulttheme,defaultuivolume,devactivitypanel,devactivitypanelupdaterate,devalbuminfotimetolive,devapiv1,devartistinfotimetolive,devartworkexternalmaxrps,devartworkmaxrequests,devartworkthrottlebackloglimit,devartworkthrottlebacklogtimeout,devartworkthrottlebuffered,devartworkworkerconcurrency,devautocreateadminpassword,devautologinusername,devenablemediafileprobe,devenableplayerinsights,devenableprofiler,devexternalartistfetchmultiplier,devexternalscanner,devinsightsinitialdelay,devloglevels,devlogsourceline,devneweventstream,devoffsetoptimize,devpreserveunicodeinexternalcalls,devscannerthreads,devselectivewatcher,devshowartistpage,devsidebarplaylists,devuishowconfig,discartpriority,enableartworkprecache,enableartworkupload,enablecoveranimation,enabledownloads,enableexternalservices,enablefavourites,enablegravatar,enableinsightscollector,enablelogredacting,enablem3uexternalalbumart,enablemediafilecoverart,enablenaturalsorting,enablenowplaying,enablereplaygain,enablescheduleddbanalyze,enablescrobblehistory,enablesharing,enablestarrating,enabletranscodingconfig,enableuserediting,enablewebpencoding,enforcenonrootuser,extauth.logouturl,extauth.trustedsources,extauth.userheader,ffmpegpath,gatrackingid,httpheaders.frameoptions,ignoredarticles,imagecachesize,indexgroups,inspect.backloglimit,inspect.backlogtimeout,inspect.enabled,inspect.maxrequests,jellyfin.autodiscovery,jellyfin.enabled,jellyfin.exposedpublicusers,jellyfin.maxconcurrentstreams,jellyfin.quickconnect,jellyfin.servername,lastfm.apikey,lastfm.enabled,lastfm.language,lastfm.scrobblefirstartistonly,lastfm.secret,listenbrainz.artistalgorithm,listenbrainz.baseurl,listenbrainz.enabled,listenbrainz.trackalgorithm,logfile,loglevel,lyricspriority,matcher.fuzzythreshold,matcher.preferstarred,maximagesize,maximageuploadsize,maxsidebarplaylists,musicfolder,pid.album,pid.track,playlistspath,port,prefersorttags,prometheus.enabled,prometheus.metricspath,prometheus.password,recentlyaddedbymodtime,scanner.artistjoiner,scanner.artistsplitexceptions,scanner.enabled,scanner.extractor,scanner.followsymlinks,scanner.ignoredotfolders,scanner.purgemissing,scanner.scanonstartup,scanner.schedule,scanner.watcherwait,search.backend,search.fullstring,sessiontimeout,shareurl,smartplaylistrefreshdelay,tags,tlscert,tlskey,transcoding.enablecancellation,transcoding.maxconcurrent,transcoding.maxconcurrentperuser,transcodingcachesize,uicoverartsize,uiloginbackgroundurl,uiplaybackreportinterval,uisearchdebouncems,uiwelcomemessage,unixsocketperm";
    // The expected snapshot above is intentionally loose: whitespace is not
    // part of key names; normalize both sides before comparing.
    let norm = |s: &str| {
        s.split(',')
            .map(|k| k.replace(' ', ""))
            .collect::<Vec<_>>()
            .join(",")
    };
    assert_eq!(
        snapshot,
        norm(expected).replace(' ', ""),
        "fresh key set changed"
    );
}

/// Dropped keys must NOT resolve (fresh key set, contract §4).
#[test]
fn dropped_keys_are_absent() {
    for dropped in [
        "subsonic.legacyclients",
        "plugins.enabled",
        "jukebox.enabled",
        "mpvpath",
        "mpvcmdtemplate",
        "passwordencryptionkey",
        "devenablepluginsinsights",
        "devplugincompilationtimeout",
        "scanner.genreseparators",
        "scanner.groupalbumreleases",
        "searchfullstring",
        "coverjpegquality",
        "spotify.id",
    ] {
        assert!(
            nd_conf::keys::lookup(dropped).is_none(),
            "dropped key must not resolve: {dropped}"
        );
    }
}

// ------------------------------------------------------- unknown-key fatal

/// Go only warned (`logUnknownOptions`); the fresh loader refuses to start.
#[test]
fn unknown_keys_are_fatal() {
    let p = write_tmp(
        "unknown.toml",
        "[Whatever]\nFoo = \"bar\"\n\n[Scanner]\nArtistJoiner = \" x \"\n",
    );
    let err = Config::load(Some(&p)).unwrap_err();
    match &err {
        ConfigError::UnknownKey(k) => {
            assert!(k.starts_with("whatever."), "got {k}");
        }
        other => panic!("expected UnknownKey, got {other:?}"),
    }
}

#[test]
fn case_insensitive_keys_are_accepted() {
    let p = write_tmp(
        "casing.toml",
        "musicfolder = \"/x\"\nScanNeR.Schedule = \"@daily\"\n",
    );
    let c = Config::load(Some(&p)).unwrap();
    assert_eq!(c.music_folder, "/x");
    assert_eq!(c.scanner.schedule, "@daily");
}

// ------------------------------------------------------------- ND_ remap

#[test]
fn nd_prefixed_file_keys_remap_to_canonical() {
    // Fixture mirrors conf/testdata/cfg_nd_keys.toml.
    let p = write_tmp(
        "nd_keys.toml",
        "ND_ADDRESS = \"127.0.0.1\"\nND_PORT = 4531\nND_SCANNER_SCHEDULE = \"@every 1h\"\n",
    );
    let c = Config::load(Some(&p)).unwrap();
    assert_eq!(c.address, "127.0.0.1");
    assert_eq!(c.port, 4531);
    assert_eq!(c.scanner.schedule, "@every 1h");
}

#[test]
fn nd_and_canonical_both_present_is_fatal() {
    // Fixture mirrors conf/testdata/cfg_nd_conflict.toml.
    let p = write_tmp(
        "nd_conflict.toml",
        "ND_ADDRESS = \"127.0.0.1\"\nAddress = \"0.0.0.0\"\n",
    );
    let err = Config::load(Some(&p)).unwrap_err();
    assert!(
        matches!(err, ConfigError::DuplicateNdKey { .. }),
        "expected DuplicateNdKey, got {err:?}"
    );
}

// --------------------------------------------------------- env precedence

#[test]
fn env_overrides_file() {
    let p = write_tmp(
        "base.toml",
        "Address = \"127.0.0.1\"\nMusicFolder = \"/file\"\n",
    );
    let mut env: HashMap<String, String> = HashMap::new();
    env.insert("ND_ADDRESS".into(), "10.0.0.1".into());
    let c = Config::load_with_env(Some(&p), |k| env.get(k).cloned()).unwrap();
    assert_eq!(c.address, "10.0.0.1"); // env wins
    assert_eq!(c.music_folder, "/file"); // file wins over default
}

#[test]
fn env_typed_values_parse() {
    let mut env: HashMap<String, String> = HashMap::new();
    env.insert("ND_PORT".into(), "9999".into());
    env.insert("ND_ENABLEDOWNLOADS".into(), "false".into());
    env.insert("ND_SESSIONTIMEOUT".into(), "90m".into());
    let c = Config::load_with_env(None, |k| env.get(k).cloned()).unwrap();
    assert_eq!(c.port, 9999);
    assert!(!c.enable_downloads);
    assert_eq!(c.session_timeout.0, Duration::from_secs(90 * 60));
}

// ------------------------------------------------------------- defaults

#[test]
fn defaults_spot_checks() {
    let c = Config::default();
    assert_eq!(c.address, "0.0.0.0");
    assert_eq!(c.port, 4533);
    assert_eq!(c.music_folder, "./music");
    assert_eq!(c.data_folder.as_str(), ".");
    assert_eq!(c.log_level, "info");
    assert_eq!(c.session_timeout.0, Duration::from_secs(48 * 3600));
    assert_eq!(
        c.default_share_expiration.0,
        Duration::from_secs(8760 * 3600)
    );
    assert_eq!(c.smart_playlist_refresh_delay.0, Duration::from_secs(5));
    assert_eq!(c.auth_window_length.0, Duration::from_secs(20));
    assert_eq!(c.scanner.watcher_wait.0, Duration::from_secs(5));
    assert_eq!(c.transcoding_cache_size.raw(), "100MB");
    assert_eq!(c.image_cache_size.raw(), "100MB");
    assert_eq!(c.max_image_upload_size.raw(), "10MB");
    assert_eq!(c.max_image_size.raw(), "20MB");
    assert_eq!(c.cover_art_quality, 75);
    assert_eq!(c.ui_cover_art_size, 300);
    assert_eq!(c.default_ui_volume, 100);
    assert_eq!(c.ignored_articles, "The El La Los Las Le Les Os As O A");
    assert!(c.enable_downloads);
    assert!(!c.enable_transcoding_config);
    assert_eq!(c.search.backend, "fts");
    assert!(!c.search.full_string);
    assert!(c.matcher.prefer_starred);
    assert_eq!(c.matcher.fuzzy_threshold, 85);
    assert_eq!(c.ext_auth.user_header, "Remote-User");
    assert_eq!(c.http_headers.frame_options, "DENY");
    assert!(!c.prometheus.enabled);
    assert_eq!(c.prometheus.metrics_path, "/metrics");
    assert_eq!(c.scanner.extractor, "taglib");
    assert_eq!(c.scanner.purge_missing, "never");
    assert!(c.scanner.enabled);
    assert_eq!(
        c.pid.track,
        "musicbrainz_trackid|albumid,discnumber,tracknumber,title"
    );
    assert_eq!(
        c.pid.album,
        "musicbrainz_albumid|albumartistid,album,albumversion,releasedate"
    );
    assert_eq!(c.agents, "deezer,lastfm,listenbrainz");
    assert!(!c.jellyfin.enabled);
    assert!(c.listen_brainz.enabled);
    assert_eq!(c.listen_brainz.base_url, "https://api.listenbrainz.org/1/");
    assert_eq!(c.dev_external_artist_fetch_multiplier, 1.5);
}

// ----------------------------------------------- computed paths & clamps

#[test]
fn cache_folder_and_db_path_are_derived() {
    let mut c = Config::default();
    c.normalize_and_validate().unwrap();
    let df = c.data_folder.as_str();
    let expected_cache = std::path::Path::new(df).join("cache");
    assert_eq!(c.cache_folder.as_str(), expected_cache.to_str().unwrap());
    assert!(c.db_path.starts_with(df));
    assert!(c.db_path.contains("navidrome.db"));
}

#[test]
fn ui_cover_art_size_clamps() {
    let p = write_tmp("clamp.toml", "UICoverArtSize = 50\n");
    let c = Config::load(Some(&p)).unwrap();
    assert_eq!(c.ui_cover_art_size, 200);
}

#[test]
fn max_image_size_floored_at_upload_size() {
    let p = write_tmp(
        "floor.toml",
        "MaxImageUploadSize = \"30MB\"\nMaxImageSize = \"10MB\"\n",
    );
    let c = Config::load(Some(&p)).unwrap();
    assert_eq!(c.max_image_size.as_bytes(), 30_000_000);
}

#[test]
fn search_backend_falls_back_to_fts() {
    let p = write_tmp("backend.toml", "[Search]\nBackend = \"bogus\"\n");
    let c = Config::load(Some(&p)).unwrap();
    assert_eq!(c.search.backend, "fts");
    let p = write_tmp("backend_legacy.toml", "[Search]\nBackend = \"LEGACY\"\n");
    let c = Config::load(Some(&p)).unwrap();
    assert_eq!(c.search.backend, "legacy");
}

#[test]
fn invalid_schedule_is_fatal() {
    let p = write_tmp("bad_sched.toml", "[Scanner]\nSchedule = \"junk\"\n");
    let err = Config::load(Some(&p)).unwrap_err();
    assert!(matches!(err, ConfigError::Invalid(_)));
}

#[test]
fn invalid_byte_size_is_fatal() {
    let p = write_tmp("bad_size.toml", "MaxImageSize = \"bananas\"\n");
    assert!(Config::load(Some(&p)).is_err());
}

#[test]
fn invalid_logout_url_is_fatal() {
    let p = write_tmp("bad_url.toml", "[ExtAuth]\nLogoutURL = \"ftp://x\"\n");
    let err = Config::load(Some(&p)).unwrap_err();
    assert!(matches!(err, ConfigError::Invalid(_)));
}

#[test]
fn negative_duration_falls_back_to_default() {
    let p = write_tmp("neg_dur.toml", "SessionTimeout = \"-10s\"\n");
    let c = Config::load(Some(&p)).unwrap();
    assert_eq!(c.session_timeout.0, Duration::from_secs(48 * 3600));
}

#[test]
fn valid_durations_load() {
    // Fixture mirrors conf/testdata/valid_duration.toml.
    let p = write_tmp(
        "valid_dur.toml",
        "SessionTimeout = \"1s\"\nSmartPlaylistRefreshDelay = \"1s\"\nDefaultShareExpiration = \"1s\"\n\
         AuthWindowLength = \"1s\"\nUIPlaybackReportInterval = \"1s\"\n\
         [Scanner]\nWatcherWait = \"1s\"\n",
    );
    let c = Config::load(Some(&p)).unwrap();
    assert_eq!(c.session_timeout.0, Duration::from_secs(1));
    assert_eq!(c.scanner.watcher_wait.0, Duration::from_secs(1));
}

#[test]
fn purge_missing_enum_is_validated() {
    let p = write_tmp("purge.toml", "[Scanner]\nPurgeMissing = \"always\"\n");
    let c = Config::load(Some(&p)).unwrap();
    assert_eq!(c.scanner.purge_missing, "always");
    let p = write_tmp(
        "purge_bad.toml",
        "[Scanner]\nPurgeMissing = \"sometimes\"\n",
    );
    // Go resets to "never" and returns an error; the fresh loader fatalizes it.
    assert!(Config::load(Some(&p)).is_err());
}

// --------------------------------------------------------- release toml

/// The shipped `release/linux/navidrome.toml` must load unchanged.
#[test]
fn release_linux_toml_loads() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../release/linux/navidrome.toml"
    );
    let c = Config::load(Some(path)).unwrap();
    assert_eq!(c.data_folder.as_str(), "/var/lib/navidrome");
    assert_eq!(c.music_folder, "/opt/navidrome/music");
}

/// The Go test fixture conf/testdata/cfg.toml (minus dropped keys:
/// `ReverseProxyUserHeader` is a dropped legacy alias) loads fine.
#[test]
fn go_cfg_toml_fixture_loads_after_trim() {
    let trimmed = "\
musicFolder = \"/toml/music\"\n\
uiWelcomeMessage = \"Welcome toml\"\n\
\n\
[Tags.artist]\n\
Split = [';']\n\
\n\
[Tags.custom]\n\
aliases = [\"toml\", \"test\"]\n\
";
    let p = write_tmp("go_cfg_trimmed.toml", trimmed);
    let c = Config::load(Some(&p)).unwrap();
    assert_eq!(c.music_folder, "/toml/music");
    assert_eq!(c.ui_welcome_message, "Welcome toml");
    assert_eq!(c.tags["artist"].split, [";"]);
    assert_eq!(c.tags["custom"].aliases, ["toml", "test"]);
}

/// An INI `[default]` file is NOT accepted: the fresh service promises TOML
/// plus the ND_ env tier only (contract §6 non-goal).
#[test]
fn ini_is_not_supported() {
    let p = write_tmp("cfg.ini", "[default]\nMusicFolder = /ini/music\n");
    assert!(Config::load(Some(&p)).is_err());
}

/// Full-file exercise: every section of the fresh key set, all at once.
#[test]
fn full_fresh_toml_loads() {
    let toml = r#"
Address = "127.0.0.1"
Port = 9999
SessionTimeout = "2h"
MaxImageSize = "40MB"
Tags.dev.Aliases = ["x"]

[Search]
Backend = "legacy"
FullString = true

[Matcher]
PreferStarred = false
FuzzyThreshold = 90

[ExtAuth]
UserHeader = "X-User"
LogoutURL = "https://sso.example/logout"

[Prometheus]
Enabled = true

[Scanner]
Schedule = "@every 2h"
PurgeMissing = "full"
ArtistSplitExceptions = ["AC/DC", "Tyler, the creator"]

[Backup]
Path = "/backups"
Schedule = "@daily"
Count = 3

[PID]
Track = "custom"

[Inspect]
Enabled = false

[Transcoding]
MaxConcurrent = 4

[LastFM]
ApiKey = "k"

[Deezer]
Language = "fr"

[ListenBrainz]
BaseURL = "https://lb/"

[Jellyfin]
Enabled = true
"#;
    let p = write_tmp("full.toml", toml);
    let c = Config::load(Some(&p)).unwrap();
    assert_eq!(c.address, "127.0.0.1");
    assert_eq!(c.port, 9999);
    assert_eq!(c.session_timeout.0, Duration::from_secs(2 * 3600));
    assert_eq!(c.max_image_size.raw(), "40MB");
    assert_eq!(c.tags["dev"].aliases, ["x"]);
    assert_eq!(c.search.backend, "legacy");
    assert!(c.search.full_string);
    assert!(!c.matcher.prefer_starred);
    assert_eq!(c.ext_auth.user_header, "X-User");
    assert!(c.prometheus.enabled);
    assert_eq!(c.scanner.schedule, "@every 2h");
    assert_eq!(c.scanner.purge_missing, "full");
    assert_eq!(
        c.scanner.artist_split_exceptions,
        ["AC/DC", "Tyler, the creator"]
    );
    assert_eq!(c.backup.schedule, "@daily");
    assert_eq!(c.backup.count, 3);
    assert_eq!(c.pid.track, "custom");
    assert!(!c.inspect.enabled);
    assert_eq!(c.transcoding.max_concurrent, 4);
    assert_eq!(c.last_fm.api_key, "k");
    assert_eq!(c.deezer.language, "fr");
    assert_eq!(c.listen_brainz.base_url, "https://lb/");
    assert!(c.jellyfin.enabled);
    // Path(): lazy MkdirAll contract.
    let tmp = std::env::temp_dir().join(format!("nd-conf-path-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let dir = nd_conf::Dir::new(tmp.join("nested").to_str().unwrap());
    assert!(dir.path().is_ok());
    assert!(dir.path().is_ok());
    let _ = std::fs::remove_dir_all(&tmp);
}
