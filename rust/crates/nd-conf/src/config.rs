//! Port of `conf/configuration.go`: the typed `Config` struct, the TOML
//! loader, and the `ND_`-env precedence ladder for the fresh Rust service
//! (docs/rust-port/config-contract.md).
//!
//! Deliberate fresh-service changes vs Go (issues.md #03):
//! * no `Subsonic.*`, `Plugins.*`, `Jukebox.*`, `MPVPath`/`MPVCmdTemplate`,
//!   `PasswordEncryptionKey`, plugin `Dev*` keys, deprecated
//!   `Scanner.GenreSeparators`/`GroupAlbumReleases`;
//! * no legacy-alias/deprecation table — only canonical spellings;
//! * unknown config keys are a load **error**, not a warning;
//! * the Go `ND_`-in-file remap stays: an `ND_*` key in the TOML remaps to its
//!   canonical key and is a fatal error when both spellings are present.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use crate::bytes::{parse_bytes, ByteSize, ByteSizeError, ByteSizeField};
pub use crate::consts as defaults;
pub use crate::crontab::{validate_crontab, CrontabError};
pub use crate::dir::Dir;
pub use crate::duration::{parse_go_duration, DurationError, DurationField};
pub use crate::keys::{
    all_accepted_lower_keys, canonical_display_name, env_var_name, is_string_map_subkey,
    nd_key_to_canonical, KeyDef, KeyType, SectionDef, SECTIONS,
};

/// Error type for config loading.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The TOML file failed to parse.
    #[error("error parsing config file: {0}")]
    Parse(#[from] toml::de::Error),
    /// The config file contains a key outside the fresh key set.
    #[error("option '{0}' is not recognized and will not be accepted: unknown config key (fresh key set, see docs/rust-port/config-contract.md)")]
    UnknownKey(String),
    /// The config file uses an `ND_`-prefixed key alongside its canonical spelling.
    #[error("config file contains both '{nd}' and '{canonical}': remove the ND_-prefixed version (the ND_ prefix is only needed for environment variables, not config file keys)")]
    DuplicateNdKey {
        /// The offending `ND_*` spelling.
        nd: String,
        /// The canonical spelling it collides with.
        canonical: String,
    },
    /// A value failed its semantic validation (byte size, crontab, URL, …).
    #[error("invalid config value: {0}")]
    Invalid(String),
}

/// Per-tag configuration (`TagConf`, conf/configuration.go:198-205).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TagConf {
    /// Ignore this tag entirely.
    pub ignore: bool,
    /// Aliases this tag is also known under.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Target type for the tag value.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub r#type: String,
    /// Max length for the tag value.
    pub max_length: i64,
    /// Separator(s) to split multi-value tags on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub split: Vec<String>,
    /// Album-level tag.
    pub album: bool,
}

/// `Search` section (`searchOptions`, conf/configuration.go:298-301).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    /// Search backend: `fts` or `legacy` (anything else falls back to `fts`).
    pub backend: String,
    /// Treat the search box as a full string.
    pub full_string: bool,
}

impl Default for SearchConfig {
    fn default() -> Self {
        SearchConfig {
            backend: "fts".into(),
            full_string: false,
        }
    }
}

/// `Matcher` section (:303-306).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MatcherConfig {
    /// Prefer starred results.
    pub prefer_starred: bool,
    /// Fuzzy-match threshold.
    pub fuzzy_threshold: i64,
}

impl Default for MatcherConfig {
    fn default() -> Self {
        MatcherConfig {
            prefer_starred: true,
            fuzzy_threshold: 85,
        }
    }
}

/// `ExtAuth` section (`extAuthOptions` :292-296).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtAuthConfig {
    /// Trusted reverse-proxy sources (CIDR / unix `@` semantics live in the
    /// auth crate; frozen by #S4).
    pub trusted_sources: String,
    /// Header carrying the authenticated user.
    pub user_header: String,
    /// External logout URL (http(s), validated).
    pub logout_url: String,
}

impl Default for ExtAuthConfig {
    fn default() -> Self {
        ExtAuthConfig {
            trusted_sources: String::new(),
            user_header: "Remote-User".into(),
            logout_url: String::new(),
        }
    }
}

/// `HTTPHeaders` section (:247-249).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpHeadersConfig {
    /// `X-Frame-Options` value.
    pub frame_options: String,
}

impl Default for HttpHeadersConfig {
    fn default() -> Self {
        HttpHeadersConfig {
            frame_options: "DENY".into(),
        }
    }
}

/// `Prometheus` section (:251-255).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrometheusConfig {
    /// Enable the metrics endpoint.
    pub enabled: bool,
    /// Endpoint path.
    pub metrics_path: String,
    /// Basic-auth password.
    pub password: String,
}

impl Default for PrometheusConfig {
    fn default() -> Self {
        PrometheusConfig {
            enabled: false,
            metrics_path: crate::consts::PROMETHEUS_DEFAULT_PATH.into(),
            password: String::new(),
        }
    }
}

/// `Scanner` section (`scannerOptions` :167-180), minus dropped keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScannerConfig {
    /// Enable the scanner.
    pub enabled: bool,
    /// Schedule (`"0"`/`""` disables; crontab syntax).
    pub schedule: String,
    /// Wait before reacting to filesystem events.
    pub watcher_wait: DurationField,
    /// Scan at startup.
    pub scan_on_startup: bool,
    /// Metadata extractor (taglib).
    pub extractor: String,
    /// Joiner for multi-artist names.
    pub artist_joiner: String,
    /// Artist names that must not be split.
    pub artist_split_exceptions: Vec<String>,
    /// Follow symlinks while walking.
    pub follow_symlinks: bool,
    /// Ignore dot-prefixed folders.
    pub ignore_dot_folders: bool,
    /// Missing-file purge policy: `never` | `always` | `full`.
    pub purge_missing: String,
}

impl Default for ScannerConfig {
    fn default() -> Self {
        ScannerConfig {
            enabled: true,
            schedule: "0".into(),
            watcher_wait: DurationField(crate::consts::DEFAULT_WATCHER_WAIT),
            scan_on_startup: true,
            extractor: crate::consts::DEFAULT_SCANNER_EXTRACTOR.into(),
            artist_joiner: crate::consts::ARTIST_JOINER.into(),
            artist_split_exceptions: Vec::new(),
            follow_symlinks: true,
            ignore_dot_folders: true,
            purge_missing: crate::consts::PURGE_MISSING_NEVER.into(),
        }
    }
}

/// `Backup` section (:266-270).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackupConfig {
    /// Number of backups to keep.
    pub count: i64,
    /// Backup directory.
    pub path: Dir,
    /// Backup schedule (crontab).
    pub schedule: String,
}

/// `PID` section (:272-275).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PidConfig {
    /// Track persistent-id rule.
    pub track: String,
    /// Album persistent-id rule.
    pub album: String,
}

impl Default for PidConfig {
    fn default() -> Self {
        PidConfig {
            track: crate::consts::DEFAULT_TRACK_PID.into(),
            album: crate::consts::DEFAULT_ALBUM_PID.into(),
        }
    }
}

/// `Inspect` section (:277-282).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InspectConfig {
    /// Enable the inspect endpoint.
    pub enabled: bool,
    /// Max concurrent inspect requests.
    pub max_requests: i64,
    /// Throttle backlog limit.
    pub backlog_limit: i64,
    /// Throttle backlog timeout.
    pub backlog_timeout: DurationField,
}

impl Default for InspectConfig {
    fn default() -> Self {
        InspectConfig {
            enabled: true,
            max_requests: 1,
            backlog_limit: crate::consts::REQUEST_THROTTLE_BACKLOG_LIMIT,
            backlog_timeout: DurationField(crate::consts::REQUEST_THROTTLE_BACKLOG_TIMEOUT),
        }
    }
}

/// `Transcoding` section (:182-186).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TranscodingConfig {
    /// Max concurrent transcodes (0 = unlimited).
    pub max_concurrent: i64,
    /// Max concurrent transcodes per user.
    pub max_concurrent_per_user: i64,
    /// Allow clients to cancel transcodes.
    pub enable_cancellation: bool,
}

/// `LastFM` section (:207-216).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LastFmConfig {
    /// Enable Last.fm integration.
    pub enabled: bool,
    /// API key.
    pub api_key: String,
    /// Shared secret.
    pub secret: String,
    /// Info language.
    pub language: String,
    /// Scrobble only the first artist.
    pub scrobble_first_artist_only: bool,
}

impl Default for LastFmConfig {
    fn default() -> Self {
        LastFmConfig {
            enabled: true,
            api_key: String::new(),
            secret: String::new(),
            language: crate::consts::DEFAULT_INFO_LANGUAGE.into(),
            scrobble_first_artist_only: false,
        }
    }
}

/// `Deezer` section (:218-224).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeezerConfig {
    /// Enable Deezer integration.
    pub enabled: bool,
    /// Info language.
    pub language: String,
}

impl Default for DeezerConfig {
    fn default() -> Self {
        DeezerConfig {
            enabled: true,
            language: crate::consts::DEFAULT_INFO_LANGUAGE.into(),
        }
    }
}

/// `ListenBrainz` section (:226-231).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ListenBrainzConfig {
    /// Enable ListenBrainz integration.
    pub enabled: bool,
    /// API base URL.
    pub base_url: String,
    /// Recommended-recs artist algorithm.
    pub artist_algorithm: String,
    /// Recommended-recs track algorithm.
    pub track_algorithm: String,
}

impl Default for ListenBrainzConfig {
    fn default() -> Self {
        ListenBrainzConfig {
            enabled: true,
            base_url: crate::consts::DEFAULT_LISTENBRAINZ_BASE_URL.into(),
            artist_algorithm: crate::consts::DEFAULT_LISTENBRAINZ_ARTIST_ALGORITHM.into(),
            track_algorithm: crate::consts::DEFAULT_LISTENBRAINZ_TRACK_ALGORITHM.into(),
        }
    }
}

/// `Jellyfin` section (:233-245).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct JellyfinConfig {
    /// Enable the Jellyfin-compatible API.
    pub enabled: bool,
    /// Server name reported to clients.
    pub server_name: String,
    /// Comma-separated public users.
    pub exposed_public_users: String,
    /// Respond to auto-discovery broadcasts.
    pub auto_discovery: bool,
    /// Enable QuickConnect.
    pub quick_connect: bool,
    /// Max concurrent streams.
    pub max_concurrent_streams: i64,
}

impl Default for JellyfinConfig {
    fn default() -> Self {
        JellyfinConfig {
            enabled: false,
            server_name: String::new(),
            exposed_public_users: String::new(),
            auto_discovery: false,
            quick_connect: true,
            // Go default: max(2, MaxOpenConns()/2); MaxOpenConns is bounded by
            // SQLite, so 2 is the conservative fresh default.
            max_concurrent_streams: 2,
        }
    }
}

/// The typed configuration (port of `configOptions`, conf/configuration.go:33-165).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Path of the config file actually used (non-settable; `ND_CONFIGFILE`
    /// or explicit argument selects it).
    #[serde(skip)]
    pub config_file: String,
    /// Bind address.
    pub address: String,
    /// Bind port.
    pub port: i64,
    /// Unix socket permissions.
    pub unix_socket_perm: String,
    /// Refuse to run as root (posix).
    pub enforce_non_root_user: bool,
    /// Music library root.
    pub music_folder: String,
    /// Data directory (state, cache parent).
    pub data_folder: Dir,
    /// Cache directory (default `<DataFolder>/cache`).
    pub cache_folder: Dir,
    /// SQLite path (default `<DataFolder>/<DefaultDbPath>`).
    pub db_path: String,
    /// Log level.
    pub log_level: String,
    /// Log file.
    pub log_file: String,
    /// Web session timeout.
    pub session_timeout: DurationField,
    /// Public base URL, decomposed into base_path/host/scheme.
    pub base_url: String,
    /// Path component of `base_url`.
    pub base_path: String,
    /// Host component of `base_url`.
    pub base_host: String,
    /// Scheme component of `base_url`.
    pub base_scheme: String,
    /// TLS certificate path.
    pub tls_cert: String,
    /// TLS key path.
    pub tls_key: String,
    /// Login background URL.
    pub ui_login_background_url: String,
    /// Login welcome message.
    pub ui_welcome_message: String,
    /// Max playlists in the sidebar.
    pub max_sidebar_playlists: i64,
    /// Expose transcoding config UI.
    pub enable_transcoding_config: bool,
    /// Allow downloads.
    pub enable_downloads: bool,
    /// Allow outbound calls to third-party services.
    pub enable_external_services: bool,
    /// Allow external album art in M3U playlists.
    pub enable_m3u_external_album_art: bool,
    /// Send anonymous insights.
    pub enable_insights_collector: bool,
    /// Schedule SQLite ANALYZE.
    pub enable_scheduled_db_analyze: bool,
    /// Read cover art from media file tags.
    pub enable_media_file_cover_art: bool,
    /// Transcoding cache size.
    pub transcoding_cache_size: ByteSizeField,
    /// Image cache size.
    pub image_cache_size: ByteSizeField,
    /// Album play count mode: `absolute` | `normalized`.
    pub album_play_count_mode: String,
    /// Precache artwork.
    pub enable_artwork_precache: bool,
    /// Import playlists from disk.
    pub auto_import_playlists: bool,
    /// Default visibility of imported playlists.
    pub default_playlist_public_visibility: bool,
    /// doublestar pattern for playlist import.
    pub playlists_path: String,
    /// Delay before smart playlists refresh.
    pub smart_playlist_refresh_delay: DurationField,
    /// Transcode downloads.
    pub auto_transcode_download: bool,
    /// Default downsampling format.
    pub default_downsampling_format: String,
    /// Search section.
    pub search: SearchConfig,
    /// Matcher section.
    pub matcher: MatcherConfig,
    /// Recently-added sort uses mtime.
    pub recently_added_by_mod_time: bool,
    /// Prefer sort tags for sorting.
    pub prefer_sort_tags: bool,
    /// Enable natural (case-folded) sorting.
    pub enable_natural_sorting: bool,
    /// Ignored articles for sorting.
    pub ignored_articles: String,
    /// Index group definitions.
    pub index_groups: String,
    /// ffmpeg binary path.
    pub ffmpeg_path: String,
    /// Cover art priority list.
    pub cover_art_priority: String,
    /// Cover art quality.
    pub cover_art_quality: i64,
    /// Encode webp artwork.
    pub enable_webp_encoding: bool,
    /// Artist art priority list.
    pub artist_art_priority: String,
    /// Folder of artist images.
    pub artist_image_folder: String,
    /// Disc art priority list.
    pub disc_art_priority: String,
    /// Lyrics priority list.
    pub lyrics_priority: String,
    /// Use gravatar avatars.
    pub enable_gravatar: bool,
    /// Enable favourites.
    pub enable_favourites: bool,
    /// Enable star rating.
    pub enable_star_rating: bool,
    /// Enable user editing.
    pub enable_user_editing: bool,
    /// Enable artwork upload.
    pub enable_artwork_upload: bool,
    /// Max upload size for artwork.
    pub max_image_upload_size: ByteSizeField,
    /// Max image cache entry size.
    pub max_image_size: ByteSizeField,
    /// Enable sharing.
    pub enable_sharing: bool,
    /// Custom share URL prefix.
    pub share_url: String,
    /// Default share expiration.
    pub default_share_expiration: DurationField,
    /// Default share downloadability.
    pub default_downloadable_share: bool,
    /// Default theme.
    pub default_theme: String,
    /// Default UI language.
    pub default_language: String,
    /// Default volume.
    pub default_ui_volume: i64,
    /// Search debounce (ms).
    pub ui_search_debounce_ms: i64,
    /// Artwork size for UI requests (clamped 200–1200).
    pub ui_cover_art_size: i64,
    /// Enable ReplayGain tags.
    pub enable_replay_gain: bool,
    /// Enable cover art animation.
    pub enable_cover_animation: bool,
    /// Enable now-playing.
    pub enable_now_playing: bool,
    /// Now-playing report interval.
    pub ui_playback_report_interval: DurationField,
    /// Google Analytics tracking id.
    pub ga_tracking_id: String,
    /// Redact secrets in logs.
    pub enable_log_redacting: bool,
    /// Auth attempts before lockout.
    pub auth_request_limit: i64,
    /// Auth lockout window.
    pub auth_window_length: DurationField,
    /// External auth section.
    pub ext_auth: ExtAuthConfig,
    /// Security headers section.
    pub http_headers: HttpHeadersConfig,
    /// Metrics section.
    pub prometheus: PrometheusConfig,
    /// Scanner section.
    pub scanner: ScannerConfig,
    /// Backup section.
    pub backup: BackupConfig,
    /// Persistent-id rules section.
    pub pid: PidConfig,
    /// Inspect section.
    pub inspect: InspectConfig,
    /// Transcoding section.
    pub transcoding: TranscodingConfig,
    /// Last.fm section.
    pub last_fm: LastFmConfig,
    /// Deezer section.
    pub deezer: DeezerConfig,
    /// ListenBrainz section.
    pub listen_brainz: ListenBrainzConfig,
    /// Jellyfin section.
    pub jellyfin: JellyfinConfig,
    /// Persist scrobbles.
    pub enable_scrobble_history: bool,
    /// Free-form per-tag configuration.
    pub tags: BTreeMap<String, TagConf>,
    /// Enabled agent integrations (comma-separated).
    pub agents: String,
    /// Per-logger levels (`DevLogLevels.*`).
    pub dev_log_levels: BTreeMap<String, String>,
    /// Include source lines in logs.
    pub dev_log_source_line: bool,
    /// Enable the profiler.
    pub dev_enable_profiler: bool,
    /// Auto-create the admin password (dev).
    pub dev_auto_create_admin_password: String,
    /// Auto-login as this user (dev).
    pub dev_auto_login_username: String,
    /// Show the activity panel.
    pub dev_activity_panel: bool,
    /// Activity panel update rate.
    pub dev_activity_panel_update_rate: DurationField,
    /// Show dev playlists in sidebar.
    pub dev_sidebar_playlists: bool,
    /// Show the artist page.
    pub dev_show_artist_page: bool,
    /// Show config in dev UI.
    pub dev_ui_show_config: bool,
    /// New event stream endpoint.
    pub dev_new_event_stream: bool,
    /// OFFSET optimization threshold.
    pub dev_offset_optimize: i64,
    /// Artwork max concurrent requests.
    pub dev_artwork_max_requests: i64,
    /// Artwork throttle backlog limit.
    pub dev_artwork_throttle_backlog_limit: i64,
    /// Artwork throttle backlog timeout.
    pub dev_artwork_throttle_backlog_timeout: DurationField,
    /// Buffer artwork throttle responses.
    pub dev_artwork_throttle_buffered: bool,
    /// Artwork worker concurrency.
    pub dev_artwork_worker_concurrency: i64,
    /// Max external artwork requests per second.
    pub dev_artwork_external_max_rps: i64,
    /// Artist info cache TTL.
    pub dev_artist_info_time_to_live: DurationField,
    /// Album info cache TTL.
    pub dev_album_info_time_to_live: DurationField,
    /// Use the external scanner.
    pub dev_external_scanner: bool,
    /// Scanner thread count.
    pub dev_scanner_threads: u64,
    /// Use the selective watcher.
    pub dev_selective_watcher: bool,
    /// Insights first-run delay.
    pub dev_insights_initial_delay: DurationField,
    /// Enable player insights.
    pub dev_enable_player_insights: bool,
    /// External artist fetch multiplier (Go float64; carried as string
    /// percent-free float, kept in a dedicated field for clarity).
    pub dev_external_artist_fetch_multiplier: f64,
    /// Preserve unicode in external calls.
    pub dev_preserve_unicode_in_external_calls: bool,
    /// Probe media files for artwork.
    pub dev_enable_media_file_probe: bool,
    /// Enable API v1.
    pub dev_apiv1: bool,
}

impl Default for Config {
    fn default() -> Self {
        use crate::consts as c;
        Config {
            config_file: String::new(),
            address: "0.0.0.0".into(),
            port: 4533,
            unix_socket_perm: "0660".into(),
            enforce_non_root_user: false,
            music_folder: "./music".into(),
            data_folder: Dir::new("."),
            cache_folder: Dir::new(""),
            db_path: String::new(),
            log_level: "info".into(),
            log_file: String::new(),
            session_timeout: DurationField(c::DEFAULT_SESSION_TIMEOUT),
            base_url: String::new(),
            base_path: String::new(),
            base_host: String::new(),
            base_scheme: String::new(),
            tls_cert: String::new(),
            tls_key: String::new(),
            ui_login_background_url: c::DEFAULT_UI_LOGIN_BACKGROUND_URL.into(),
            ui_welcome_message: String::new(),
            max_sidebar_playlists: c::DEFAULT_MAX_SIDEBAR_PLAYLISTS,
            enable_transcoding_config: false,
            enable_downloads: true,
            enable_external_services: true,
            enable_m3u_external_album_art: false,
            enable_insights_collector: true,
            enable_scheduled_db_analyze: true,
            enable_media_file_cover_art: true,
            transcoding_cache_size: ByteSizeField::from_raw(c::DEFAULT_CACHE_SIZE),
            image_cache_size: ByteSizeField::from_raw(c::DEFAULT_CACHE_SIZE),
            album_play_count_mode: c::ALBUM_PLAY_COUNT_MODE_ABSOLUTE.into(),
            enable_artwork_precache: true,
            auto_import_playlists: true,
            default_playlist_public_visibility: false,
            playlists_path: String::new(),
            smart_playlist_refresh_delay: DurationField(c::DEFAULT_SMART_REFRESH),
            auto_transcode_download: false,
            default_downsampling_format: c::DEFAULT_DOWNSAMPLING_FORMAT.into(),
            search: SearchConfig::default(),
            matcher: MatcherConfig::default(),
            recently_added_by_mod_time: false,
            prefer_sort_tags: false,
            enable_natural_sorting: false,
            ignored_articles: "The El La Los Las Le Les Os As O A".into(),
            index_groups: "A B C D E F G H I J K L M N O P Q R S T U V W X-Z(XYZ) [Unknown]([)"
                .into(),
            ffmpeg_path: String::new(),
            cover_art_priority: "cover.*, folder.*, front.*, embedded, external".into(),
            cover_art_quality: 75,
            enable_webp_encoding: false,
            artist_art_priority: "artist.*, album/artist.*, external".into(),
            artist_image_folder: String::new(),
            disc_art_priority: "disc*.*, cd*.*, cover.*, folder.*, front.*, discsubtitle, embedded"
                .into(),
            lyrics_priority: ".ttml,.yaml,.yml,.elrc,.lrc,.srt,.txt,embedded".into(),
            enable_gravatar: false,
            enable_favourites: true,
            enable_star_rating: true,
            enable_user_editing: true,
            enable_artwork_upload: true,
            max_image_upload_size: ByteSizeField::from_raw(c::DEFAULT_MAX_IMAGE_UPLOAD_SIZE),
            max_image_size: ByteSizeField::from_raw(c::DEFAULT_MAX_IMAGE_SIZE),
            enable_sharing: true,
            share_url: String::new(),
            default_share_expiration: DurationField(c::DEFAULT_SHARE_EXPIRATION),
            default_downloadable_share: false,
            default_theme: "Dark".into(),
            default_language: String::new(),
            default_ui_volume: c::DEFAULT_UI_VOLUME,
            ui_search_debounce_ms: c::DEFAULT_UI_SEARCH_DEBOUNCE_MS,
            ui_cover_art_size: c::DEFAULT_UI_COVER_ART_SIZE,
            enable_replay_gain: true,
            enable_cover_animation: true,
            enable_now_playing: true,
            ui_playback_report_interval: DurationField(c::DEFAULT_UI_PLAYBACK_REPORT_INTERVAL),
            ga_tracking_id: String::new(),
            enable_log_redacting: true,
            auth_request_limit: 5,
            auth_window_length: DurationField(c::DEFAULT_AUTH_WINDOW_LENGTH),
            ext_auth: ExtAuthConfig::default(),
            http_headers: HttpHeadersConfig::default(),
            prometheus: PrometheusConfig::default(),
            scanner: ScannerConfig::default(),
            backup: BackupConfig::default(),
            pid: PidConfig::default(),
            inspect: InspectConfig::default(),
            transcoding: TranscodingConfig::default(),
            last_fm: LastFmConfig::default(),
            deezer: DeezerConfig::default(),
            listen_brainz: ListenBrainzConfig::default(),
            jellyfin: JellyfinConfig::default(),
            enable_scrobble_history: true,
            tags: BTreeMap::new(),
            agents: c::DEFAULT_AGENTS.into(),
            dev_log_levels: BTreeMap::new(),
            dev_log_source_line: false,
            dev_enable_profiler: false,
            dev_auto_create_admin_password: String::new(),
            dev_auto_login_username: String::new(),
            dev_activity_panel: true,
            dev_activity_panel_update_rate: DurationField(c::DEFAULT_ACTIVITY_PANEL_UPDATE_RATE),
            dev_sidebar_playlists: true,
            dev_show_artist_page: true,
            dev_ui_show_config: true,
            dev_new_event_stream: true,
            dev_offset_optimize: 50000,
            dev_artwork_max_requests: 2,
            dev_artwork_throttle_backlog_limit: c::REQUEST_THROTTLE_BACKLOG_LIMIT,
            dev_artwork_throttle_backlog_timeout: DurationField(
                c::REQUEST_THROTTLE_BACKLOG_TIMEOUT,
            ),
            dev_artwork_throttle_buffered: true,
            dev_artwork_worker_concurrency: 2,
            dev_artwork_external_max_rps: 2,
            dev_artist_info_time_to_live: DurationField(c::ARTIST_INFO_TIME_TO_LIVE),
            dev_album_info_time_to_live: DurationField(c::ALBUM_INFO_TIME_TO_LIVE),
            dev_external_scanner: true,
            dev_scanner_threads: 5,
            dev_selective_watcher: true,
            dev_insights_initial_delay: DurationField(c::INSIGHTS_INITIAL_DELAY),
            dev_enable_player_insights: true,
            dev_external_artist_fetch_multiplier: 1.5,
            dev_preserve_unicode_in_external_calls: false,
            dev_enable_media_file_probe: true,
            dev_apiv1: false,
        }
    }
}

/// Raw key/value pairs from a config file, keys lowercased — the input to the
/// remap + unknown-key checks.
type RawPairs = Vec<(String, toml::Value)>;

/// Flattens a parsed TOML table into dotted (lowercased) key/value pairs,
/// including nested tables. Inline tables become their dotted subkeys too.
fn flatten_table(table: &toml::Value, prefix: &str, out: &mut RawPairs) {
    match table {
        toml::Value::Table(t) => {
            for (k, v) in t {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                match v {
                    toml::Value::Table(_) => flatten_table(v, &key.to_ascii_lowercase(), out),
                    other => out.push((key.to_ascii_lowercase(), other.clone())),
                }
            }
        }
        other => out.push((prefix.to_ascii_lowercase(), other.clone())),
    }
}

/// Collects the set of unknown keys (as written, lowercased) after the
/// `ND_` remap, honoring the free-form map prefixes.
fn unknown_keys(pairs: &RawPairs) -> Vec<String> {
    let mut unknown = Vec::new();
    for (key, _) in pairs {
        if crate::keys::lookup(key).is_some() {
            continue;
        }
        if crate::keys::is_string_map_subkey(key) {
            continue;
        }
        // ND_-prefixed keys that remap to a known key were already handled.
        if key.starts_with("nd_") {
            let canonical = crate::keys::nd_key_to_canonical(key);
            if crate::keys::lookup(&canonical).is_some() {
                continue;
            }
        }
        unknown.push(key.clone());
    }
    unknown
}

/// Remaps `ND_`-prefixed keys found in the file to their canonical dotted
/// keys. Both spellings present is a fatal duplicate error
/// (`remapEnvVarKeysFromConfig`, conf/configuration.go:589-617).
fn remap_nd_keys(pairs: &mut RawPairs) -> Result<(), ConfigError> {
    let nd_indices: Vec<usize> = pairs
        .iter()
        .enumerate()
        .filter(|(_, (k, _))| k.starts_with("nd_") && k != "nd_")
        .map(|(i, _)| i)
        .collect();
    for i in nd_indices {
        let (key, value) = pairs[i].clone();
        let canonical = crate::keys::nd_key_to_canonical(&key);
        let display_nd = format!(
            "ND_{}",
            key.strip_prefix("nd_").unwrap().to_ascii_uppercase()
        );
        if pairs.iter().any(|(k, _)| *k == canonical) {
            return Err(ConfigError::DuplicateNdKey {
                nd: display_nd,
                canonical: crate::keys::canonical_display_name(&canonical),
            });
        }
        // Unknown canonical keys surface in the unknown-key check.
        pairs.push((canonical, value));
        pairs[i].0 = format!("\u{0}consumed:{key}");
    }
    pairs.retain(|(k, _)| !k.starts_with('\u{0}'));
    Ok(())
}

/**
 * Applies one config pair onto a TOML table of the serialized `Config`,
 * resolving each dotted segment case- and underscore-insensitively against
 * the typed field names (viper lowercases keys on load; the Rust struct
 * keeps snake_case fields, so `MusicFolder` → `music_folder`,
 * `LastFM.ApiKey` → `last_fm.api_key`). Unresolved segments are free-form
 * map subkeys (`Tags.*`, `DevLogLevels.*`) and are inserted verbatim.
 */
fn insert_pair(
    table: &mut toml::map::Map<String, toml::Value>,
    segments: &[&str],
    value: toml::Value,
) {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| *c != '_')
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let seg = segments[0];
    let existing = table.keys().find(|k| norm(k) == norm(seg)).cloned();
    if segments.len() == 1 {
        let key = existing.unwrap_or_else(|| seg.to_string());
        table.insert(key, value);
        return;
    }
    match existing {
        Some(k) => {
            let entry = table.get_mut(&k).unwrap();
            if !entry.is_table() {
                *entry = toml::Value::Table(toml::map::Map::new());
            }
            let sub = entry.as_table_mut().unwrap();
            insert_pair(sub, &segments[1..], value);
        }
        None => {
            let mut sub = toml::map::Map::new();
            insert_pair(&mut sub, &segments[1..], value);
            table.insert(seg.to_string(), toml::Value::Table(sub));
        }
    }
}

/**
 * A file/env key was provided; apply it onto the typed [`Config`] field by
 * field, mirroring viper's key/value merge. Uses serde as the merge mechanism.
 */
fn apply_pairs(config: &mut Config, pairs: &RawPairs) -> Result<(), ConfigError> {
    // Serialize current config to a value table, overwrite with file pairs,
    // deserialize back. Config keys are case-insensitive; they are resolved
    // against the snake_case serde field names in `insert_pair`.
    let mut table: toml::map::Map<String, toml::Value> = toml::Value::try_from(&*config)
        .map_err(|e| ConfigError::Invalid(format!("serializing defaults: {e}")))?
        .as_table()
        .cloned()
        .unwrap_or_default();
    for (key, value) in pairs {
        let segments: Vec<&str> = key.split('.').collect();
        insert_pair(&mut table, &segments, value.clone());
    }
    // Go durationNonNegativeOrDefault: a negative duration logs a warning and
    // keeps the default rather than failing the whole load.
    let applied = Config::deserialize(toml::Value::Table(table.clone()));
    match applied {
        Ok(c) => *config = c,
        Err(e) if e.to_string().contains("negative duration") => {}
        Err(e) => return Err(ConfigError::Invalid(format!("applying config values: {e}"))),
    }
    Ok(())
}

/// Deserializes a config-file scalar (string/int/bool/float) from an env var.
fn env_value(field_ty: &str, raw: &str) -> Result<toml::Value, ConfigError> {
    let v = match field_ty {
        "bool" => toml::Value::Boolean(
            raw.parse::<bool>()
                .map_err(|_| ConfigError::Invalid(format!("invalid bool {raw:?}")))?,
        ),
        "integer" => toml::Value::Integer(
            raw.parse::<i64>()
                .map_err(|_| ConfigError::Invalid(format!("invalid integer {raw:?}")))?,
        ),
        "float" => toml::Value::Float(
            raw.parse::<f64>()
                .map_err(|_| ConfigError::Invalid(format!("invalid float {raw:?}")))?,
        ),
        // Strings, durations ("10s"), byte sizes ("10MB") stay strings.
        _ => toml::Value::String(raw.to_string()),
    };
    Ok(v)
}

impl Config {
    /// Loads configuration from the TOML file `path` (if any) plus
    /// `ND_*` environment variables, with the precedence ladder
    /// env > file > default (`InitConfig`/`Load`, conf/configuration.go:403-463,
    /// :1181-1208). Unknown file keys are fatal.
    pub fn load(path: Option<&str>) -> Result<Self, ConfigError> {
        Self::load_with_env(path, |k| std::env::var(k).ok())
    }

    /// [`Config::load`] with an injected environment lookup (test seam).
    pub fn load_with_env(
        path: Option<&str>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let mut config = Config {
            config_file: path.unwrap_or("").to_string(),
            ..Config::default()
        };

        let mut pairs: RawPairs = Vec::new();
        if let Some(p) = path.filter(|p| !p.is_empty()) {
            let data = std::fs::read_to_string(p)
                .map_err(|e| ConfigError::Invalid(format!("reading config file {p:?}: {e}")))?;
            let parsed: toml::Value = toml::from_str(&data)?;
            flatten_table(&parsed, "", &mut pairs);
            remap_nd_keys(&mut pairs)?;
            let unknown = unknown_keys(&pairs);
            if let Some(k) = unknown.first() {
                return Err(ConfigError::UnknownKey(k.clone()));
            }
            apply_pairs(&mut config, &pairs)?;
        }

        // Environment tier: ND_<SECTION>_<KEY> with '.'→'_'.
        // Precedence: env > file (already applied) > defaults.
        let env_pairs = collect_env_pairs(&config, &env);
        if !env_pairs.is_empty() {
            apply_pairs(&mut config, &env_pairs)?;
        }

        config.normalize_and_validate()?;
        Ok(config)
    }

    /// Applies the post-load normalization and validation
    /// (conf/configuration.go:426-530): computed paths, duration fallbacks,
    /// schedule validation, byte sizes, URL checks, clamps, PID defaults.
    pub fn normalize_and_validate(&mut self) -> Result<(), ConfigError> {
        use crate::consts as c;
        // Computed paths.
        if self.cache_folder.is_empty() {
            self.cache_folder = Dir::new(
                std::path::Path::new(self.data_folder.as_str())
                    .join("cache")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        if self.db_path.is_empty() {
            self.db_path = std::path::Path::new(self.data_folder.as_str())
                .join(
                    c::DEFAULT_DB_PATH
                        .trim_start_matches('/')
                        .split('?')
                        .next()
                        .unwrap_or("navidrome.db"),
                )
                .to_string_lossy()
                .into_owned()
                + &self.db_path_params();
        }
        // Duration non-negative fallbacks.
        self.session_timeout = DurationField(
            self.session_timeout
                .non_negative_or(c::DEFAULT_SESSION_TIMEOUT),
        );
        self.smart_playlist_refresh_delay = DurationField(
            self.smart_playlist_refresh_delay
                .non_negative_or(c::DEFAULT_SMART_REFRESH),
        );
        self.default_share_expiration = DurationField(
            self.default_share_expiration
                .non_negative_or(c::DEFAULT_SHARE_EXPIRATION),
        );
        self.ui_playback_report_interval = DurationField(
            self.ui_playback_report_interval
                .non_negative_or(c::DEFAULT_UI_PLAYBACK_REPORT_INTERVAL),
        );
        self.auth_window_length = DurationField(
            self.auth_window_length
                .non_negative_or(c::DEFAULT_AUTH_WINDOW_LENGTH),
        );
        self.scanner.watcher_wait = DurationField(
            self.scanner
                .watcher_wait
                .non_negative_or(c::DEFAULT_WATCHER_WAIT),
        );
        self.dev_activity_panel_update_rate = DurationField(
            self.dev_activity_panel_update_rate
                .non_negative_or(c::DEFAULT_ACTIVITY_PANEL_UPDATE_RATE),
        );
        self.dev_artwork_throttle_backlog_timeout = DurationField(
            self.dev_artwork_throttle_backlog_timeout
                .non_negative_or(c::REQUEST_THROTTLE_BACKLOG_TIMEOUT),
        );
        self.dev_artist_info_time_to_live = DurationField(
            self.dev_artist_info_time_to_live
                .non_negative_or(c::ARTIST_INFO_TIME_TO_LIVE),
        );
        self.dev_album_info_time_to_live = DurationField(
            self.dev_album_info_time_to_live
                .non_negative_or(c::ALBUM_INFO_TIME_TO_LIVE),
        );
        self.dev_insights_initial_delay = DurationField(
            self.dev_insights_initial_delay
                .non_negative_or(c::INSIGHTS_INITIAL_DELAY),
        );

        // Schedules.
        match self.scanner.schedule.trim() {
            "0" | "" => self.scanner.schedule = String::new(),
            s => {
                crate::crontab::validate_crontab(s).map_err(|e| {
                    ConfigError::Invalid(format!("invalid Scanner.Schedule {s:?}: {}", e.message))
                })?;
            }
        }
        if !self.backup.path.is_empty() && !self.backup.schedule.is_empty() && self.backup.count > 0
        {
            crate::crontab::validate_crontab(&self.backup.schedule).map_err(|e| {
                ConfigError::Invalid(format!(
                    "invalid Backup.Schedule {:?}: {}",
                    self.backup.schedule, e.message
                ))
            })?;
        } else {
            self.backup.schedule = String::new();
        }

        // Scanner.PurgeMissing enum (validatePurgeMissingOption,
        // conf/configuration.go:845-851): Go resets and returns an error;
        // the fresh loader makes it fatal (contract: unknown/invalid values
        // refuse to start).
        match self.scanner.purge_missing.trim() {
            c::PURGE_MISSING_NEVER | c::PURGE_MISSING_ALWAYS | c::PURGE_MISSING_FULL => {}
            v => {
                return Err(ConfigError::Invalid(format!(
                    "invalid Scanner.PurgeMissing value: {v:?}. Must be one of: never, always, full"
                )));
            }
        }
        // Byte sizes already validated at deserialize; floor MaxImageSize.
        if self.max_image_size.as_bytes() < self.max_image_upload_size.as_bytes() {
            self.max_image_size = self.max_image_upload_size.clone();
        }

        // ExtAuth.LogoutURL: http(s) with non-empty host.
        let url = self.ext_auth.logout_url.trim();
        if !url.is_empty() {
            let scheme_end = url.find("://");
            let valid = match scheme_end {
                Some(i) => {
                    let scheme = &url[..i];
                    (scheme == "http" || scheme == "https")
                        && url[i + 3..]
                            .split('/')
                            .next()
                            .is_some_and(|h| !h.is_empty())
                }
                None => false,
            };
            if !valid {
                return Err(ConfigError::Invalid(format!(
                    "invalid ExtAuth.LogoutURL {url:?}: a full http(s) URL with a non-empty host is required"
                )));
            }
        }

        // BaseURL decomposition (Go url.Parse + path/host/scheme split,
        // conf/configuration.go:467-477).
        if !self.base_url.is_empty() {
            let (scheme, rest) = self.base_url.split_once("://").ok_or_else(|| {
                ConfigError::Invalid(format!(
                    "invalid BaseURL {:?}: missing scheme",
                    self.base_url
                ))
            })?;
            let (host, path) = match rest.find('/') {
                Some(i) => (&rest[..i], &rest[i..]),
                None => (rest, ""),
            };
            if host.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "invalid BaseURL {:?}: empty host",
                    self.base_url
                )));
            }
            self.base_scheme = scheme.to_string();
            self.base_host = host.to_string();
            self.base_path = path.to_string();
        }

        // Search backend normalization: only fts/legacy, else fallback to fts
        // (normalizeSearchBackend, conf/configuration.go:933-942).
        let b = self.search.backend.trim().to_ascii_lowercase();
        self.search.backend = match b.as_str() {
            "fts" | "legacy" => b,
            _ => "fts".to_string(),
        };

        // UICoverArtSize clamp.
        if !(200..=1200).contains(&self.ui_cover_art_size) {
            self.ui_cover_art_size = self.ui_cover_art_size.clamp(200, 1200);
        }

        // PID defaults for empty rules.
        if self.pid.album.is_empty() {
            self.pid.album = c::DEFAULT_ALBUM_PID.into();
        }
        if self.pid.track.is_empty() {
            self.pid.track = c::DEFAULT_TRACK_PID.into();
        }

        // Scanner.Extractor fallback (only taglib is ported).
        if self.scanner.extractor != c::DEFAULT_SCANNER_EXTRACTOR {
            self.scanner.extractor = c::DEFAULT_SCANNER_EXTRACTOR.into();
        }

        Ok(())
    }

    fn db_path_params(&self) -> String {
        let base = crate::consts::DEFAULT_DB_PATH
            .split('?')
            .nth(1)
            .unwrap_or("");
        format!("?{base}")
    }
}

/// Walks `ND_*` env vars against the typed config and builds the pairs to
/// apply, using the key set as the acceptance filter (unknown env keys are
/// ignored, matching viper's behavior of only binding known keys).
fn collect_env_pairs(config: &Config, env: &impl Fn(&str) -> Option<String>) -> RawPairs {
    let template =
        toml::Value::try_from(config).unwrap_or(toml::Value::Table(toml::map::Map::new()));
    let mut pairs = RawPairs::new();
    for key in crate::keys::all_accepted_lower_keys() {
        let var = crate::keys::env_var_name(&key);
        let Some(raw) = env(&var) else { continue };
        if raw.is_empty() {
            continue;
        }
        // Find the type from the template (defaults table) or the key set.
        let ty = field_type(&template, &key)
            .or_else(|| crate::keys::lookup(&key).map(|k| key_type_name(k.ty)));
        let ty = ty.unwrap_or_else(|| {
            // Free-form maps carry a value string each.
            "string".to_string()
        });
        if let Ok(value) = env_value(&ty, &raw) {
            pairs.push((key.to_string(), value));
        }
    }
    pairs
}

/**
 * Case- and underscore-insensitive table lookup (see [`insert_pair`]).
 */
fn table_get<'a>(table: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| *c != '_')
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let table = table.as_table()?;
    table
        .keys()
        .find(|k| norm(k) == norm(key))
        .and_then(|k| table.get(k))
}

/**
 * The TOML type name of a config key, read off the serialized default config.
 */
fn field_type(template: &toml::Value, key: &str) -> Option<String> {
    let mut segments = key.split('.');
    let head = segments.next()?;
    let rest: Vec<&str> = segments.collect();
    let mut cur = table_get(template, head)?;
    let leaf = match rest.split_last() {
        Some((leaf, parents)) => {
            for seg in parents {
                cur = table_get(cur, seg)?;
            }
            leaf
        }
        None => head,
    };
    let v = table_get(cur, leaf)?;
    Some(
        match v {
            toml::Value::String(_) => "string",
            toml::Value::Integer(_) => "integer",
            toml::Value::Boolean(_) => "bool",
            toml::Value::Float(_) => "float",
            _ => "string",
        }
        .to_string(),
    )
}

/// Maps a [`KeyType`] to a TOML type name for env parsing.
fn key_type_name(ty: KeyType) -> String {
    match ty {
        KeyType::Int => "integer",
        KeyType::Bool => "bool",
        _ => "string",
    }
    .to_string()
}
