//! The fresh-service TOML key set (docs/rust-port/config-contract.md §1–4):
//! every accepted root and section key, its type, and its default.
//!
//! This is the deliberate fresh-service change vs Go: no `Subsonic.*`,
//! `Plugins.*`, `Jukebox.*`, `MPVPath`/`MPVCmdTemplate`,
//! `PasswordEncryptionKey`, dropped `Dev*` plugin keys, dropped deprecated
//! `Scanner.*` keys, no legacy aliases, and **unknown keys are fatal**.
//!
//! Keys are matched case-insensitively (viper lowercases on load;
//! `canonicalOptionName` conf/configuration.go:704-715).

use std::time::Duration;

use crate::consts;

/// The value type a key carries, driving TOML decoding and validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyType {
    /// A plain string.
    Str,
    /// An integer.
    Int,
    /// A boolean.
    Bool,
    /// A Go-duration string (`"5m"`).
    Duration,
    /// A byte-size string (`"100MB"`).
    ByteSize,
    /// A free-form string map (`Tags.*`, `DevLogLevels.*`).
    StringMap,
    /// A string list (comma-separated or TOML array).
    StringList,
    /// A nested section: listed separately in [`SECTIONS`].
    Section,
}

/// A single config key: canonical spelling, type, default.
#[derive(Debug, Clone)]
pub struct KeyDef {
    /// Canonical (PascalCase / dotted) key spelling.
    pub name: &'static str,
    /// Value type.
    pub ty: KeyType,
    /// Default value, as the string/int/bool the loader applies.
    pub default: DefaultValue,
}

/// A default value for a [`KeyDef`].
#[derive(Debug, Clone)]
pub enum DefaultValue {
    /// String default.
    Str(&'static str),
    /// Integer default.
    Int(i64),
    /// Boolean default.
    Bool(bool),
    /// Duration default.
    Duration(Duration),
    /// Computed at load time (e.g. `MaxImageSize` floor, CPU-derived keys).
    Computed(&'static str),
}

impl DefaultValue {
    /// Renders the default as the string a TOML writer would use.
    pub fn as_display(&self) -> String {
        match self {
            DefaultValue::Str(s) => (*s).to_string(),
            DefaultValue::Int(i) => i.to_string(),
            DefaultValue::Bool(b) => b.to_string(),
            DefaultValue::Duration(_) => "<duration>".to_string(),
            DefaultValue::Computed(c) => (*c).to_string(),
        }
    }
}

macro_rules! str_key {
    ($name:literal) => {
        KeyDef {
            name: $name,
            ty: KeyType::Str,
            default: DefaultValue::Str(""),
        }
    };
}

/// Root-level keys, in the order they appear in `configOptions`
/// (conf/configuration.go:33-165). Dropped keys are absent by design.
pub const ROOT_KEYS: &[KeyDef] = &[
    KeyDef {
        name: "Address",
        ty: KeyType::Str,
        default: DefaultValue::Str("0.0.0.0"),
    },
    KeyDef {
        name: "Port",
        ty: KeyType::Int,
        default: DefaultValue::Int(4533),
    },
    KeyDef {
        name: "UnixSocketPerm",
        ty: KeyType::Str,
        default: DefaultValue::Str("0660"),
    },
    KeyDef {
        name: "EnforceNonRootUser",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
    KeyDef {
        name: "MusicFolder",
        ty: KeyType::Str,
        default: DefaultValue::Str("./music"),
    },
    KeyDef {
        name: "DataFolder",
        ty: KeyType::Str,
        default: DefaultValue::Str("."),
    },
    KeyDef {
        name: "CacheFolder",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "DbPath",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "LogLevel",
        ty: KeyType::Str,
        default: DefaultValue::Str("info"),
    },
    KeyDef {
        name: "LogFile",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "SessionTimeout",
        ty: KeyType::Duration,
        default: DefaultValue::Duration(consts::DEFAULT_SESSION_TIMEOUT),
    },
    KeyDef {
        name: "BaseURL",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "BasePath",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "BaseHost",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "BaseScheme",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "TLSCert",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "TLSKey",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "UILoginBackgroundURL",
        ty: KeyType::Str,
        default: DefaultValue::Str(consts::DEFAULT_UI_LOGIN_BACKGROUND_URL),
    },
    KeyDef {
        name: "UIWelcomeMessage",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "MaxSidebarPlaylists",
        ty: KeyType::Int,
        default: DefaultValue::Int(consts::DEFAULT_MAX_SIDEBAR_PLAYLISTS),
    },
    KeyDef {
        name: "EnableTranscodingConfig",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
    KeyDef {
        name: "EnableDownloads",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "EnableExternalServices",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "EnableM3UExternalAlbumArt",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
    KeyDef {
        name: "EnableInsightsCollector",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "EnableScheduledDBAnalyze",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "EnableMediaFileCoverArt",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "TranscodingCacheSize",
        ty: KeyType::ByteSize,
        default: DefaultValue::Str(consts::DEFAULT_CACHE_SIZE),
    },
    KeyDef {
        name: "ImageCacheSize",
        ty: KeyType::ByteSize,
        default: DefaultValue::Str(consts::DEFAULT_CACHE_SIZE),
    },
    KeyDef {
        name: "AlbumPlayCountMode",
        ty: KeyType::Str,
        default: DefaultValue::Str(consts::ALBUM_PLAY_COUNT_MODE_ABSOLUTE),
    },
    KeyDef {
        name: "EnableArtworkPrecache",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "AutoImportPlaylists",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "DefaultPlaylistPublicVisibility",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
    KeyDef {
        name: "PlaylistsPath",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "SmartPlaylistRefreshDelay",
        ty: KeyType::Duration,
        default: DefaultValue::Duration(consts::DEFAULT_SMART_REFRESH),
    },
    KeyDef {
        name: "AutoTranscodeDownload",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
    KeyDef {
        name: "DefaultDownsamplingFormat",
        ty: KeyType::Str,
        default: DefaultValue::Str(consts::DEFAULT_DOWNSAMPLING_FORMAT),
    },
    KeyDef {
        name: "RecentlyAddedByModTime",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
    KeyDef {
        name: "PreferSortTags",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
    KeyDef {
        name: "EnableNaturalSorting",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
    KeyDef {
        name: "IgnoredArticles",
        ty: KeyType::Str,
        default: DefaultValue::Str("The El La Los Las Le Les Os As O A"),
    },
    KeyDef {
        name: "IndexGroups",
        ty: KeyType::Str,
        default: DefaultValue::Str(
            "A B C D E F G H I J K L M N O P Q R S T U V W X-Z(XYZ) [Unknown]([)",
        ),
    },
    KeyDef {
        name: "FFmpegPath",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "CoverArtPriority",
        ty: KeyType::Str,
        default: DefaultValue::Str("cover.*, folder.*, front.*, embedded, external"),
    },
    KeyDef {
        name: "CoverArtQuality",
        ty: KeyType::Int,
        default: DefaultValue::Int(75),
    },
    KeyDef {
        name: "EnableWebPEncoding",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
    KeyDef {
        name: "ArtistArtPriority",
        ty: KeyType::Str,
        default: DefaultValue::Str("artist.*, album/artist.*, external"),
    },
    KeyDef {
        name: "ArtistImageFolder",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "DiscArtPriority",
        ty: KeyType::Str,
        default: DefaultValue::Str(
            "disc*.*, cd*.*, cover.*, folder.*, front.*, discsubtitle, embedded",
        ),
    },
    KeyDef {
        name: "LyricsPriority",
        ty: KeyType::Str,
        default: DefaultValue::Str(".ttml,.yaml,.yml,.elrc,.lrc,.srt,.txt,embedded"),
    },
    KeyDef {
        name: "EnableGravatar",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
    KeyDef {
        name: "EnableFavourites",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "EnableStarRating",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "EnableUserEditing",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "EnableArtworkUpload",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "MaxImageUploadSize",
        ty: KeyType::ByteSize,
        default: DefaultValue::Str(consts::DEFAULT_MAX_IMAGE_UPLOAD_SIZE),
    },
    KeyDef {
        name: "MaxImageSize",
        ty: KeyType::ByteSize,
        default: DefaultValue::Str(consts::DEFAULT_MAX_IMAGE_SIZE),
    },
    KeyDef {
        name: "EnableSharing",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "ShareURL",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "DefaultShareExpiration",
        ty: KeyType::Duration,
        default: DefaultValue::Duration(consts::DEFAULT_SHARE_EXPIRATION),
    },
    KeyDef {
        name: "DefaultDownloadableShare",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
    KeyDef {
        name: "DefaultTheme",
        ty: KeyType::Str,
        default: DefaultValue::Str("Dark"),
    },
    KeyDef {
        name: "DefaultLanguage",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "DefaultUIVolume",
        ty: KeyType::Int,
        default: DefaultValue::Int(consts::DEFAULT_UI_VOLUME),
    },
    KeyDef {
        name: "UISearchDebounceMs",
        ty: KeyType::Int,
        default: DefaultValue::Int(consts::DEFAULT_UI_SEARCH_DEBOUNCE_MS),
    },
    KeyDef {
        name: "UICoverArtSize",
        ty: KeyType::Int,
        default: DefaultValue::Int(consts::DEFAULT_UI_COVER_ART_SIZE),
    },
    KeyDef {
        name: "EnableReplayGain",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "EnableCoverAnimation",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "EnableNowPlaying",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "UIPlaybackReportInterval",
        ty: KeyType::Duration,
        default: DefaultValue::Duration(consts::DEFAULT_UI_PLAYBACK_REPORT_INTERVAL),
    },
    KeyDef {
        name: "GATrackingID",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "EnableLogRedacting",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "AuthRequestLimit",
        ty: KeyType::Int,
        default: DefaultValue::Int(5),
    },
    KeyDef {
        name: "AuthWindowLength",
        ty: KeyType::Duration,
        default: DefaultValue::Duration(consts::DEFAULT_AUTH_WINDOW_LENGTH),
    },
    KeyDef {
        name: "EnableScrobbleHistory",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "Tags",
        ty: KeyType::StringMap,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "Agents",
        ty: KeyType::Str,
        default: DefaultValue::Str(consts::DEFAULT_AGENTS),
    },
    // Dev keys (conf/configuration.go:134-164), minus the dropped plugin pair.
    KeyDef {
        name: "DevLogLevels",
        ty: KeyType::StringMap,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "DevLogSourceLine",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
    KeyDef {
        name: "DevEnableProfiler",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
    KeyDef {
        name: "DevAutoCreateAdminPassword",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "DevAutoLoginUsername",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "DevActivityPanel",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "DevActivityPanelUpdateRate",
        ty: KeyType::Duration,
        default: DefaultValue::Duration(consts::DEFAULT_ACTIVITY_PANEL_UPDATE_RATE),
    },
    KeyDef {
        name: "DevSidebarPlaylists",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "DevShowArtistPage",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "DevUIShowConfig",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "DevNewEventStream",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "DevOffsetOptimize",
        ty: KeyType::Int,
        default: DefaultValue::Int(50000),
    },
    KeyDef {
        name: "DevAPIv1",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
    KeyDef {
        name: "DevArtworkMaxRequests",
        ty: KeyType::Int,
        default: DefaultValue::Computed("max(2, num_cpu/2)"),
    },
    KeyDef {
        name: "DevArtworkThrottleBacklogLimit",
        ty: KeyType::Int,
        default: DefaultValue::Int(consts::REQUEST_THROTTLE_BACKLOG_LIMIT),
    },
    KeyDef {
        name: "DevArtworkThrottleBacklogTimeout",
        ty: KeyType::Duration,
        default: DefaultValue::Duration(consts::REQUEST_THROTTLE_BACKLOG_TIMEOUT),
    },
    KeyDef {
        name: "DevArtworkThrottleBuffered",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "DevArtworkWorkerConcurrency",
        ty: KeyType::Int,
        default: DefaultValue::Computed("max(2, num_cpu/2)"),
    },
    KeyDef {
        name: "DevArtworkExternalMaxRPS",
        ty: KeyType::Int,
        default: DefaultValue::Int(2),
    },
    KeyDef {
        name: "DevArtistInfoTimeToLive",
        ty: KeyType::Duration,
        default: DefaultValue::Duration(consts::ARTIST_INFO_TIME_TO_LIVE),
    },
    KeyDef {
        name: "DevAlbumInfoTimeToLive",
        ty: KeyType::Duration,
        default: DefaultValue::Duration(consts::ALBUM_INFO_TIME_TO_LIVE),
    },
    KeyDef {
        name: "DevExternalScanner",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "DevScannerThreads",
        ty: KeyType::Int,
        default: DefaultValue::Int(5),
    },
    KeyDef {
        name: "DevSelectiveWatcher",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "DevInsightsInitialDelay",
        ty: KeyType::Duration,
        default: DefaultValue::Duration(consts::INSIGHTS_INITIAL_DELAY),
    },
    KeyDef {
        name: "DevEnablePlayerInsights",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "DevExternalArtistFetchMultiplier",
        ty: KeyType::Int,
        default: DefaultValue::Computed("1.5"),
    },
    KeyDef {
        name: "DevPreserveUnicodeInExternalCalls",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
    KeyDef {
        name: "DevEnableMediaFileProbe",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
];

/// A nested section of the fresh TOML (`docs/rust-port/config-contract.md §2`).
#[derive(Debug, Clone)]
pub struct SectionDef {
    /// Section name (PascalCase, matching the Go struct field).
    pub name: &'static str,
    /// Keys allowed inside the section.
    pub keys: &'static [KeyDef],
}

/// `Search` section keys (`searchOptions` :298-301).
pub const SEARCH_KEYS: &[KeyDef] = &[
    KeyDef {
        name: "Search.Backend",
        ty: KeyType::Str,
        default: DefaultValue::Str("fts"),
    },
    KeyDef {
        name: "Search.FullString",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
];

/// `Matcher` section keys (:303-306).
pub const MATCHER_KEYS: &[KeyDef] = &[
    KeyDef {
        name: "Matcher.PreferStarred",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "Matcher.FuzzyThreshold",
        ty: KeyType::Int,
        default: DefaultValue::Int(85),
    },
];

/// `ExtAuth` section keys (`extAuthOptions` :292-296).
pub const EXTAUTH_KEYS: &[KeyDef] = &[
    KeyDef {
        name: "ExtAuth.TrustedSources",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "ExtAuth.UserHeader",
        ty: KeyType::Str,
        default: DefaultValue::Str("Remote-User"),
    },
    KeyDef {
        name: "ExtAuth.LogoutURL",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
];

/// `HTTPHeaders` section keys (:247-249).
pub const HTTPHEADERS_KEYS: &[KeyDef] = &[KeyDef {
    name: "HTTPHeaders.FrameOptions",
    ty: KeyType::Str,
    default: DefaultValue::Str("DENY"),
}];

/// `Prometheus` section keys (:251-255).
pub const PROMETHEUS_KEYS: &[KeyDef] = &[
    KeyDef {
        name: "Prometheus.Enabled",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
    KeyDef {
        name: "Prometheus.MetricsPath",
        ty: KeyType::Str,
        default: DefaultValue::Str(consts::PROMETHEUS_DEFAULT_PATH),
    },
    KeyDef {
        name: "Prometheus.Password",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
];

/// `Scanner` section keys (`scannerOptions` :167-180), minus the dropped
/// deprecated `GenreSeparators`/`GroupAlbumReleases`.
pub const SCANNER_KEYS: &[KeyDef] = &[
    KeyDef {
        name: "Scanner.Enabled",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "Scanner.Schedule",
        ty: KeyType::Str,
        default: DefaultValue::Str("0"),
    },
    KeyDef {
        name: "Scanner.WatcherWait",
        ty: KeyType::Duration,
        default: DefaultValue::Duration(consts::DEFAULT_WATCHER_WAIT),
    },
    KeyDef {
        name: "Scanner.ScanOnStartup",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "Scanner.Extractor",
        ty: KeyType::Str,
        default: DefaultValue::Str(consts::DEFAULT_SCANNER_EXTRACTOR),
    },
    KeyDef {
        name: "Scanner.ArtistJoiner",
        ty: KeyType::Str,
        default: DefaultValue::Str(consts::ARTIST_JOINER),
    },
    KeyDef {
        name: "Scanner.ArtistSplitExceptions",
        ty: KeyType::StringList,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "Scanner.FollowSymlinks",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "Scanner.IgnoreDotFolders",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "Scanner.PurgeMissing",
        ty: KeyType::Str,
        default: DefaultValue::Str(consts::PURGE_MISSING_NEVER),
    },
];

/// `Backup` section keys (:266-270).
pub const BACKUP_KEYS: &[KeyDef] = &[
    KeyDef {
        name: "Backup.Count",
        ty: KeyType::Int,
        default: DefaultValue::Int(0),
    },
    KeyDef {
        name: "Backup.Path",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "Backup.Schedule",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
];

/// `PID` section keys (:272-275).
pub const PID_KEYS: &[KeyDef] = &[
    KeyDef {
        name: "PID.Track",
        ty: KeyType::Str,
        default: DefaultValue::Str(consts::DEFAULT_TRACK_PID),
    },
    KeyDef {
        name: "PID.Album",
        ty: KeyType::Str,
        default: DefaultValue::Str(consts::DEFAULT_ALBUM_PID),
    },
];

/// `Inspect` section keys (:277-282).
pub const INSPECT_KEYS: &[KeyDef] = &[
    KeyDef {
        name: "Inspect.Enabled",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "Inspect.MaxRequests",
        ty: KeyType::Int,
        default: DefaultValue::Int(1),
    },
    KeyDef {
        name: "Inspect.BacklogLimit",
        ty: KeyType::Int,
        default: DefaultValue::Int(consts::REQUEST_THROTTLE_BACKLOG_LIMIT),
    },
    KeyDef {
        name: "Inspect.BacklogTimeout",
        ty: KeyType::Int,
        default: DefaultValue::Computed("request-throttle backlog timeout (ms)"),
    },
];

/// `Transcoding` section keys (:182-186).
pub const TRANSCODING_KEYS: &[KeyDef] = &[
    KeyDef {
        name: "Transcoding.MaxConcurrent",
        ty: KeyType::Int,
        default: DefaultValue::Int(0),
    },
    KeyDef {
        name: "Transcoding.MaxConcurrentPerUser",
        ty: KeyType::Int,
        default: DefaultValue::Int(0),
    },
    KeyDef {
        name: "Transcoding.EnableCancellation",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
];

/// `LastFM` section keys (:207-216).
pub const LASTFM_KEYS: &[KeyDef] = &[
    KeyDef {
        name: "LastFM.Enabled",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "LastFM.ApiKey",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "LastFM.Secret",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "LastFM.Language",
        ty: KeyType::Str,
        default: DefaultValue::Str(consts::DEFAULT_INFO_LANGUAGE),
    },
    KeyDef {
        name: "LastFM.ScrobbleFirstArtistOnly",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
];

/// `Deezer` section keys (:218-224).
pub const DEEZER_KEYS: &[KeyDef] = &[
    KeyDef {
        name: "Deezer.Enabled",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "Deezer.Language",
        ty: KeyType::Str,
        default: DefaultValue::Str(consts::DEFAULT_INFO_LANGUAGE),
    },
];

/// `ListenBrainz` section keys (:226-231).
pub const LISTENBRAINZ_KEYS: &[KeyDef] = &[
    KeyDef {
        name: "ListenBrainz.Enabled",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "ListenBrainz.BaseURL",
        ty: KeyType::Str,
        default: DefaultValue::Str(consts::DEFAULT_LISTENBRAINZ_BASE_URL),
    },
    KeyDef {
        name: "ListenBrainz.ArtistAlgorithm",
        ty: KeyType::Str,
        default: DefaultValue::Str(consts::DEFAULT_LISTENBRAINZ_ARTIST_ALGORITHM),
    },
    KeyDef {
        name: "ListenBrainz.TrackAlgorithm",
        ty: KeyType::Str,
        default: DefaultValue::Str(consts::DEFAULT_LISTENBRAINZ_TRACK_ALGORITHM),
    },
];

/// `Jellyfin` section keys (:233-245).
pub const JELLYFIN_KEYS: &[KeyDef] = &[
    KeyDef {
        name: "Jellyfin.Enabled",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
    KeyDef {
        name: "Jellyfin.ServerName",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "Jellyfin.ExposedPublicUsers",
        ty: KeyType::Str,
        default: DefaultValue::Str(""),
    },
    KeyDef {
        name: "Jellyfin.AutoDiscovery",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(false),
    },
    KeyDef {
        name: "Jellyfin.QuickConnect",
        ty: KeyType::Bool,
        default: DefaultValue::Bool(true),
    },
    KeyDef {
        name: "Jellyfin.MaxConcurrentStreams",
        ty: KeyType::Int,
        default: DefaultValue::Computed("max(2, max_open_conns/2)"),
    },
];

/// All nested sections of the fresh TOML key set.
pub const SECTIONS: &[SectionDef] = &[
    SectionDef {
        name: "Search",
        keys: SEARCH_KEYS,
    },
    SectionDef {
        name: "Matcher",
        keys: MATCHER_KEYS,
    },
    SectionDef {
        name: "ExtAuth",
        keys: EXTAUTH_KEYS,
    },
    SectionDef {
        name: "HTTPHeaders",
        keys: HTTPHEADERS_KEYS,
    },
    SectionDef {
        name: "Prometheus",
        keys: PROMETHEUS_KEYS,
    },
    SectionDef {
        name: "Scanner",
        keys: SCANNER_KEYS,
    },
    SectionDef {
        name: "Backup",
        keys: BACKUP_KEYS,
    },
    SectionDef {
        name: "PID",
        keys: PID_KEYS,
    },
    SectionDef {
        name: "Inspect",
        keys: INSPECT_KEYS,
    },
    SectionDef {
        name: "Transcoding",
        keys: TRANSCODING_KEYS,
    },
    SectionDef {
        name: "LastFM",
        keys: LASTFM_KEYS,
    },
    SectionDef {
        name: "Deezer",
        keys: DEEZER_KEYS,
    },
    SectionDef {
        name: "ListenBrainz",
        keys: LISTENBRAINZ_KEYS,
    },
    SectionDef {
        name: "Jellyfin",
        keys: JELLYFIN_KEYS,
    },
];

/// Keys that may appear anywhere as free-form map subkeys
/// (`canonicalOptionName` prefix rule, conf/configuration.go:766-768).
pub const STRING_MAP_PREFIXES: &[&str] = &["tags.", "devloglevels."];

/// Resolves a lowercased dotted key against the fresh key set. Returns the
/// canonical [`KeyDef`] when known, `None` for unknown keys.
pub fn lookup(lower_key: &str) -> Option<&'static KeyDef> {
    ROOT_KEYS
        .iter()
        .find(|k| k.name.to_ascii_lowercase() == lower_key)
        .or_else(|| {
            SECTIONS
                .iter()
                .flat_map(|s| s.keys.iter())
                .find(|k| k.name.to_ascii_lowercase() == lower_key)
        })
}

/// True when the lowercased dotted key is a subkey of a free-form map option
/// (`Tags.custom.Aliases`, `DevLogLevels.app=debug`).
pub fn is_string_map_subkey(lower_key: &str) -> bool {
    STRING_MAP_PREFIXES.iter().any(|p| lower_key.starts_with(p))
        && lower_key.matches('.').count() > 1
}

/**
 * Every accepted key, canonical spelling lowercased — the complete
 * acceptance set used by the unknown-key check and the env-var walk
 * (viper lowercases keys on load; `canonicalOptionName`
 * conf/configuration.go:704-715).
 */
pub fn all_accepted_lower_keys() -> Vec<String> {
    let mut keys: Vec<String> = ROOT_KEYS
        .iter()
        .filter(|k| k.ty != KeyType::StringMap)
        .map(|k| k.name.to_ascii_lowercase())
        .chain(
            SECTIONS
                .iter()
                .flat_map(|s| s.keys.iter().map(|k| k.name.to_ascii_lowercase())),
        )
        .collect();
    // The map roots themselves are accepted as section heads.
    keys.push("tags".to_string());
    keys.push("devloglevels".to_string());
    keys.sort_unstable();
    keys
}

/// Maps an `ND_`-prefixed config-file key to its canonical dotted key
/// (`ndKeyToCanonical`, conf/configuration.go:700-702): `nd_scanner_schedule`
/// → `scanner.schedule`.
pub fn nd_key_to_canonical(key: &str) -> String {
    key.strip_prefix("nd_").unwrap_or(key).replace('_', ".")
}

/// Computes the environment-variable name for a dotted canonical key
/// (`envVarName`, conf/configuration.go:629-634): `Scanner.Schedule` →
/// `ND_SCANNER_SCHEDULE`.
pub fn env_var_name(option: &str) -> String {
    if option.is_empty() {
        return String::new();
    }
    format!("ND_{}", option.to_ascii_uppercase().replace('.', "_"))
}

/// Renders a lowercase dotted key in canonical PascalCase for messages
/// (`toPascalCase`-alike display): `scanner.schedule` → `Scanner.Schedule`.
pub fn canonical_display_name(lower_key: &str) -> String {
    lower_key
        .split('.')
        .map(|seg| {
            let mut c = seg.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

// The macro is used only above; keep it referenced for rustdoc.
#[allow(unused)]
fn _macro_keeper() {
    let _ = str_key!("x");
}
