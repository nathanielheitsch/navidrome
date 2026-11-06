//! Port of `consts/consts.go` — the typed defaults table consumed by
//! `setViperDefaults` (conf/configuration.go:974-1155) and the config loader.
//!
//! Only the constants the fresh-service config key set needs are carried:
//! defaults referenced by config keys, PID rules, and the entity/URL-name
//! enums referenced from config-derived logic.

use std::time::Duration;

/// Application name (`consts.AppName`).
pub const APP_NAME: &str = "navidrome";

/// Default SQLite connection string appended to `DataFolder` when `DbPath` is
/// empty (`consts.DefaultDbPath`).
pub const DEFAULT_DB_PATH: &str =
    "navidrome.db?cache=shared&_busy_timeout=15000&_journal_mode=WAL&_foreign_keys=on&synchronous=normal";

/// Default `SessionTimeout` (`consts.DefaultSessionTimeout`, 48h).
pub const DEFAULT_SESSION_TIMEOUT: Duration = Duration::from_secs(48 * 3600);
/// Default `SmartPlaylistRefreshDelay` (`consts.DefaultSmartRefresh`, 5s).
pub const DEFAULT_SMART_REFRESH: Duration = Duration::from_secs(5);
/// Default `DefaultShareExpiration` (`consts.DefaultShareExpiration`, 8760h = 1 year).
pub const DEFAULT_SHARE_EXPIRATION: Duration = Duration::from_secs(8760 * 3600);
/// Default `AuthWindowLength` (`consts.DefaultAuthWindowLength`, 20s).
pub const DEFAULT_AUTH_WINDOW_LENGTH: Duration = Duration::from_secs(20);
/// Default `Scanner.WatcherWait` (`consts.DefaultWatcherWait`, 5s).
pub const DEFAULT_WATCHER_WAIT: Duration = Duration::from_secs(5);
/// Default `UIPlaybackReportInterval` (`consts.DefaultUIPlaybackReportInterval`, 1m).
pub const DEFAULT_UI_PLAYBACK_REPORT_INTERVAL: Duration = Duration::from_secs(60);
/// Default `DevActivityPanelUpdateRate` (`consts.DefaultActivityPanelUpdateRate`, 300ms).
pub const DEFAULT_ACTIVITY_PANEL_UPDATE_RATE: Duration = Duration::from_millis(300);
/// Request-throttle backlog limit (`consts.RequestThrottleBacklogLimit`).
pub const REQUEST_THROTTLE_BACKLOG_LIMIT: i64 = 100;
/// Request-throttle backlog timeout (`consts.RequestThrottleBacklogTimeout`, 1m).
pub const REQUEST_THROTTLE_BACKLOG_TIMEOUT: Duration = Duration::from_secs(60);
/// Artist info TTL (`consts.ArtistInfoTimeToLive`, 24h).
pub const ARTIST_INFO_TIME_TO_LIVE: Duration = Duration::from_secs(24 * 3600);
/// Album info TTL (`consts.AlbumInfoTimeToLive`, 7d).
pub const ALBUM_INFO_TIME_TO_LIVE: Duration = Duration::from_secs(7 * 24 * 3600);
/// Insights initial delay (`consts.InsightsInitialDelay`, 30m).
pub const INSIGHTS_INITIAL_DELAY: Duration = Duration::from_secs(30 * 60);
/// Default info language (`consts.DefaultInfoLanguage`).
pub const DEFAULT_INFO_LANGUAGE: &str = "en";

/// Default `Scanner.Extractor` (`consts.DefaultScannerExtractor`).
pub const DEFAULT_SCANNER_EXTRACTOR: &str = "taglib";
/// Zero-width space used to join hashed id args (`consts.Zwsp`).
pub const ZWSP: &str = "\u{200b}";
/// Artist name joiner (`consts.ArtistJoiner`).
pub const ARTIST_JOINER: &str = " • ";

/// Default `PID.Track` (`consts.DefaultTrackPID`).
pub const DEFAULT_TRACK_PID: &str = "musicbrainz_trackid|albumid,discnumber,tracknumber,title";
/// Default `PID.Album` (`consts.DefaultAlbumPID`).
pub const DEFAULT_ALBUM_PID: &str =
    "musicbrainz_albumid|albumartistid,album,albumversion,releasedate";

/// `Prometheus.MetricsPath` default (`consts.PrometheusDefaultPath`).
pub const PROMETHEUS_DEFAULT_PATH: &str = "/metrics";
/// Prometheus basic-auth user (`consts.PrometheusAuthUser`).
pub const PROMETHEUS_AUTH_USER: &str = "navidrome";

/// `Scanner.PurgeMissing` enum values (`consts.PurgeMissing*`).
pub const PURGE_MISSING_NEVER: &str = "never";
/// `Scanner.PurgeMissing = "always"`.
pub const PURGE_MISSING_ALWAYS: &str = "always";
/// `Scanner.PurgeMissing = "full"`.
pub const PURGE_MISSING_FULL: &str = "full";

/// Album play count modes (`consts.AlbumPlayCountMode*`).
pub const ALBUM_PLAY_COUNT_MODE_ABSOLUTE: &str = "absolute";
/// `AlbumPlayCountMode = "normalized"`.
pub const ALBUM_PLAY_COUNT_MODE_NORMALIZED: &str = "normalized";

/// `LastFM.BaseURL` default (`consts.DefaultListenBrainzBaseURL` peer).
pub const DEFAULT_LISTENBRAINZ_BASE_URL: &str = "https://api.listenbrainz.org/1/";
/// `ListenBrainz.ArtistAlgorithm` default.
pub const DEFAULT_LISTENBRAINZ_ARTIST_ALGORITHM: &str =
    "session_based_days_9000_session_300_contribution_5_threshold_15_limit_50_skip_30";
/// `ListenBrainz.TrackAlgorithm` default.
pub const DEFAULT_LISTENBRAINZ_TRACK_ALGORITHM: &str =
    "session_based_days_9000_session_300_contribution_5_threshold_15_limit_50_skip_30";

/// `Agents` default.
pub const DEFAULT_AGENTS: &str = "deezer,lastfm,listenbrainz";
/// `DefaultDownsamplingFormat` (`consts.DefaultDownsamplingFormat`).
pub const DEFAULT_DOWNSAMPLING_FORMAT: &str = "opus";

/// Default UI values (`consts.DefaultUI*`).
pub const DEFAULT_UI_VOLUME: i64 = 100;
/// Default `UISearchDebounceMs`.
pub const DEFAULT_UI_SEARCH_DEBOUNCE_MS: i64 = 200;
/// Default `UICoverArtSize`.
pub const DEFAULT_UI_COVER_ART_SIZE: i64 = 300;
/// Default `MaxSidebarPlaylists`.
pub const DEFAULT_MAX_SIDEBAR_PLAYLISTS: i64 = 100;
/// Default `MaxImageUploadSize` string (`consts.DefaultMaxImageUploadSize`).
pub const DEFAULT_MAX_IMAGE_UPLOAD_SIZE: &str = "10MB";
/// Default `MaxImageSize` string (`consts.DefaultMaxImageSize`).
pub const DEFAULT_MAX_IMAGE_SIZE: &str = "20MB";
/// Default `UILoginBackgroundURL` (`consts.DefaultUILoginBackgroundURL`).
pub const DEFAULT_UI_LOGIN_BACKGROUND_URL: &str = "/backgrounds";

/// Default cache size string used by `TranscodingCacheSize`/`ImageCacheSize`.
pub const DEFAULT_CACHE_SIZE: &str = "100MB";

/// URL path prefixes (`consts.URLPath*`).
pub const URL_PATH_UI: &str = "/app";
/// Native API path prefix.
pub const URL_PATH_NATIVE_API: &str = "/api";
/// Subsonic API path prefix (kept for path routing tables; the Subsonic API
/// itself is out of scope for the fresh service).
pub const URL_PATH_SUBSONIC_API: &str = "/rest";
/// Public share path prefix.
pub const URL_PATH_PUBLIC: &str = "/share";
/// Public share images path prefix.
pub const URL_PATH_PUBLIC_IMAGES: &str = "/share/img";
/// Jellyfin API path prefix.
pub const URL_PATH_JELLYFIN_API: &str = "/jellyfin";
/// API v1 path prefix.
pub const URL_PATH_APIV1: &str = "/api/v1";

/// HTTP user agent (`consts.HTTPUserAgent` pattern).
pub const HTTP_USER_AGENT: &str = concat!(
    "Navidrome/",
    env!("CARGO_PKG_VERSION"),
    " - https://github.com/navidrome"
);

/// Default HTTP client timeout (`consts.DefaultHttpClientTimeOut`, 10s).
pub const DEFAULT_HTTP_CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

/// `ServerReadHeaderTimeout` (`consts.ServerReadHeaderTimeout`, 3s).
pub const SERVER_READ_HEADER_TIMEOUT: Duration = Duration::from_secs(3);

/// Database analyze schedule (`consts.DBAnalyzeCheckSchedule`).
pub const DB_ANALYZE_CHECK_SCHEDULE: &str = "@every 30m";
/// Max age before a DB ANALYZE run is considered stale (`consts.DBAnalyzeMaxAge`).
pub const DB_ANALYZE_MAX_AGE: Duration = Duration::from_secs(24 * 3600);

/// Artwork enqueue-missing schedule (`consts.ArtworkEnqueueMissingSchedule`).
pub const ARTWORK_ENQUEUE_MISSING_SCHEDULE: &str = "@every 1h";
/// Artwork prune schedule (`consts.ArtworkPruneSchedule`).
pub const ARTWORK_PRUNE_SCHEDULE: &str = "@daily";

/// Model `property` keys (`consts.*Key`).
pub const INITIAL_SETUP_FLAG_KEY: &str = "InitialSetup";
/// Property key set after a full scan is required post-migration.
pub const FULL_SCAN_AFTER_MIGRATION_FLAG_KEY: &str = "FullScanAfterMigration";
/// Property key marking deferred playlist import.
pub const PLAYLISTS_IMPORT_PENDING_FLAG_KEY: &str = "PlaylistsImportPending";
/// Property key for the last scan error.
pub const LAST_SCAN_ERROR_KEY: &str = "LastScanError";
/// Property key for the last scan type.
pub const LAST_SCAN_TYPE_KEY: &str = "LastScanType";
/// Property key for the last scan start time.
pub const LAST_SCAN_START_TIME_KEY: &str = "LastScanStartTime";
/// Property key for the last DB ANALYZE time.
pub const LAST_DB_ANALYZE_AT_KEY: &str = "LastDBAnalyzeAt";
/// Property key for the last DB ANALYZE attempt time.
pub const LAST_DB_ANALYZE_ATTEMPT_AT_KEY: &str = "LastDBAnalyzeAttemptAt";
/// Property key marking a pending DB ANALYZE.
pub const DB_ANALYZE_PENDING_KEY: &str = "DBAnalyzePending";
/// Property key counting DB ANALYZE failures.
pub const DB_ANALYZE_FAILURE_COUNT_KEY: &str = "DBAnalyzeFailureCount";
/// Property key the artwork config fingerprint check compares against.
pub const ART_CONF_FINGERPRINT_PROPERTY_KEY: &str = "ArtConfFingerprint";

/// HTTP header names (`consts.*Header`).
pub const UI_AUTHORIZATION_HEADER: &str = "X-ND-Authorization";
/// Client unique-id header name.
pub const UI_CLIENT_UNIQUE_ID_HEADER: &str = "X-ND-Client-Unique-Id";

/// JWT property keys (`consts.JWT*`).
pub const JWT_SECRET_KEY: &str = "JWTSecret";
/// Public secret property key.
pub const JWT_PUBLIC_SECRET_KEY: &str = "JWTPublicSecret";
/// JWT issuer.
pub const JWT_ISSUER: &str = "ND";

/// API key prefix (`consts.APIKeyPrefix`).
pub const API_KEY_PREFIX: &str = "nds_";

/// Dev-mode initial admin user (`consts.DevInitialUserName`).
pub const DEV_INITIAL_USER_NAME: &str = "admin";
/// Dev-mode initial admin display name.
pub const DEV_INITIAL_NAME: &str = "Dev Admin";

/// Entity type names (`consts.Entity*`).
pub const ENTITY_ARTIST: &str = "artist";
/// Entity type name for playlists.
pub const ENTITY_PLAYLIST: &str = "playlist";
/// Entity type name for radios.
pub const ENTITY_RADIO: &str = "radio";

/// Special artist names (`consts.VariousArtists` / `Unknown*`).
pub const VARIOUS_ARTISTS: &str = "Various Artists";
/// Sentinel Various Artists id — a constant, not a runtime derivation
/// (`consts.VariousArtistsID`); legal only while `NewHash` stays byte-exact
/// (owned by `nd-id`, issue #02).
pub const VARIOUS_ARTISTS_ID: &str = "63sqASlAfjbGMuLP4JhnZU";
/// Display name for unknown albums.
pub const UNKNOWN_ALBUM: &str = "[Unknown Album]";
/// Display name for unknown artists.
pub const UNKNOWN_ARTIST: &str = "[Unknown Artist]";
/// Sentinel unknown-artist id lives in `nd-id` (issue #02): it is
/// `id.NewHash(strings.ToLower(UnknownArtist))`, a hash derivation whose
/// byte-exact value is owned by that crate, not duplicated here.
/// MusicBrainz id for Various Artists (`consts.VariousArtistsMbzId`).
pub const VARIOUS_ARTISTS_MBZ_ID: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

/// Legacy reversible-password store keys (`consts.DefaultEncryptionKey`,
/// `consts.PasswordsEncryptedKey`) — dead config per
/// `docs/rust-port/credentials.md`: the AES-GCM store is model A and never
/// ported, so the encryption key constant is carried only as a documented
/// tombstone, never consumed.
pub const DEFAULT_ENCRYPTION_KEY: &str = "just for obfuscation";

/// Scan ignore file name (`consts.ScanIgnoreFile`).
pub const SCAN_IGNORE_FILE: &str = ".ndignore";
/// Artwork folder name (`consts.ArtworkFolder`).
pub const ARTWORK_FOLDER: &str = "artwork";
/// Hashed artwork subfolder (`consts.HashedArtworkFolder`).
pub const HASHED_ARTWORK_FOLDER: &str = "hashed";
/// i18n resources folder (`consts.I18nFolder`).
pub const I18N_FOLDER: &str = "i18n";

/// Default transcodings table (`consts.DefaultTranscodings`), carried as
/// typed rows for the transcoding defaults UI (`EnableTranscodingConfig`).
pub struct DefaultTranscoding {
    /// Human name of the transcoding.
    pub name: &'static str,
    /// Target format identifier.
    pub target_format: &'static str,
    /// Default bitrate (kbps, 0 = lossless).
    pub default_bit_rate: i64,
    /// ffmpeg command template.
    pub command: &'static str,
}

/// The default transcoding definitions (`consts.DefaultTranscodings`).
pub const DEFAULT_TRANSCODINGS: &[DefaultTranscoding] = &[
    DefaultTranscoding {
        name: "mp3 audio",
        target_format: "mp3",
        default_bit_rate: 192,
        command: "ffmpeg -ss %t -i %s -map 0:a:0 -map_metadata 0 -map_metadata 0:s:a:0 -b:a %bk -v 0 -f mp3 -",
    },
    DefaultTranscoding {
        name: "opus audio",
        target_format: "opus",
        default_bit_rate: 128,
        command: "ffmpeg -ss %t -i %s -map 0:a:0 -map_metadata 0 -map_metadata 0:s:a:0 -b:a %bk -v 0 -c:a libopus -f opus -",
    },
    DefaultTranscoding {
        name: "aac audio",
        target_format: "aac",
        default_bit_rate: 256,
        command: "ffmpeg -ss %t -i %s -map 0:a:0 -map_metadata 0 -map_metadata 0:s:a:0 -b:a %bk -v 0 -c:a aac -f adts -",
    },
    DefaultTranscoding {
        name: "flac audio",
        target_format: "flac",
        default_bit_rate: 0,
        command: "ffmpeg -ss %t -i %s -map 0:a:0 -map_metadata 0 -map_metadata 0:s:a:0 -v 0 -c:a flac -f flac -",
    },
];

/// Offline login background data URL (`consts.DefaultUILoginBackgroundURLOffline`).
pub const DEFAULT_UI_LOGIN_BACKGROUND_OFFLINE: &str = concat!(
    "data:image/png;base64,",
    "iVBORw0KGgoAAAANSUhEUgAAAMgAAADICAIAAAAiOjnJAAAABGdBTUEAALGPC/xhBQAAAiJJREFUeF7t0IEAAAAAw6D5Ux/khVBhwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDBgwIABAwYMGDDVlwABBWcSrQAAAABJRU5ErkJggg=="
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_spot_checks() {
        assert_eq!(DEFAULT_SESSION_TIMEOUT, Duration::from_secs(48 * 3600));
        assert_eq!(DEFAULT_SHARE_EXPIRATION, Duration::from_secs(8760 * 3600));
        assert_eq!(DEFAULT_SMART_REFRESH, Duration::from_secs(5));
        assert_eq!(DEFAULT_AUTH_WINDOW_LENGTH, Duration::from_secs(20));
        assert_eq!(DEFAULT_WATCHER_WAIT, Duration::from_secs(5));
        assert_eq!(ARTIST_INFO_TIME_TO_LIVE, Duration::from_secs(24 * 3600));
        assert_eq!(ALBUM_INFO_TIME_TO_LIVE, Duration::from_secs(7 * 24 * 3600));
        assert_eq!(INSIGHTS_INITIAL_DELAY, Duration::from_secs(30 * 60));
        assert_eq!(
            DEFAULT_ACTIVITY_PANEL_UPDATE_RATE,
            Duration::from_millis(300)
        );
        assert_eq!(DEFAULT_UI_PLAYBACK_REPORT_INTERVAL, Duration::from_secs(60));
        assert_eq!(REQUEST_THROTTLE_BACKLOG_TIMEOUT, Duration::from_secs(60));
        assert_eq!(DEFAULT_UI_VOLUME, 100);
        assert_eq!(DEFAULT_UI_SEARCH_DEBOUNCE_MS, 200);
        assert_eq!(DEFAULT_UI_COVER_ART_SIZE, 300);
        assert_eq!(DEFAULT_MAX_SIDEBAR_PLAYLISTS, 100);
        assert_eq!(DEFAULT_MAX_IMAGE_UPLOAD_SIZE, "10MB");
        assert_eq!(DEFAULT_MAX_IMAGE_SIZE, "20MB");
        assert_eq!(DEFAULT_CACHE_SIZE, "100MB");
        assert_eq!(DEFAULT_SCANNER_EXTRACTOR, "taglib");
        assert_eq!(DEFAULT_INFO_LANGUAGE, "en");
        assert_eq!(DEFAULT_AGENTS, "deezer,lastfm,listenbrainz");
        assert_eq!(DEFAULT_DOWNSAMPLING_FORMAT, "opus");
        assert_eq!(ARTIST_JOINER, " • ");
        assert_eq!(ZWSP, "\u{200b}");
        assert_eq!(PROMETHEUS_DEFAULT_PATH, "/metrics");
        assert_eq!(URL_PATH_PUBLIC_IMAGES, "/share/img");
        assert_eq!(VARIOUS_ARTISTS_ID, "63sqASlAfjbGMuLP4JhnZU");
        assert!(VARIOUS_ARTISTS_ID
            .chars()
            .all(|c| c.is_ascii_alphanumeric()));
        assert_eq!(
            DEFAULT_TRACK_PID,
            "musicbrainz_trackid|albumid,discnumber,tracknumber,title"
        );
        assert_eq!(
            DEFAULT_ALBUM_PID,
            "musicbrainz_albumid|albumartistid,album,albumversion,releasedate"
        );
        assert_eq!(
            DEFAULT_LISTENBRAINZ_ARTIST_ALGORITHM.len(),
            DEFAULT_LISTENBRAINZ_TRACK_ALGORITHM.len()
        );
        assert_eq!(DEFAULT_TRANSCODINGS.len(), 4);
        assert_eq!(DEFAULT_TRANSCODINGS[0].name, "mp3 audio");
        assert_eq!(DEFAULT_TRANSCODINGS[3].target_format, "flac");
    }

    #[test]
    fn pid_constants_are_the_go_values() {
        // consts/consts.go:157-159
        assert_eq!(
            DEFAULT_ALBUM_PID,
            "musicbrainz_albumid|albumartistid,album,albumversion,releasedate"
        );
        assert_eq!(
            DEFAULT_TRACK_PID,
            "musicbrainz_trackid|albumid,discnumber,tracknumber,title"
        );
    }
}
