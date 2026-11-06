# Config-Format Contract (#S3) — Fresh TOML key set

Status: **FROZEN** for the Rust port. This is the contract consumed by `nd-conf`
(Part B #03). It is a *fresh-service* contract per Part C item 12 (issues.md:502):
there are no existing user TOML files to stay compatible with, so this document
enumerates the Go `configOptions` key set and then **applies the settled
decisions** rather than recording a compatibility story.

- Go source of truth: `conf/configuration.go` (`configOptions` struct at :33-165,
  nested sections :167-306, viper defaults `setViperDefaults` :974-1155).
- Settled decisions (Part C item 12, issues.md:502):
  - **No `Subsonic.*` keys** (`subsonicOptions` conf/configuration.go:188-196;
    the whole Subsonic API is out of scope for the fresh service).
  - **No `Plugins.*` keys** (`pluginsOptions` conf/configuration.go:284-290;
    plugin host cancelled per Part C item 14, issues.md:504).
  - **No `Jukebox.*` keys** (`jukeboxOptions` conf/configuration.go:259-264;
    the MPV jukebox is not ported).
  - **No `PasswordEncryptionKey`** (conf/configuration.go:113, default at :1051;
    dead config per Part C item 12 — the AES-GCM reversible store it keyed is
    model A and never ported; see docs/rust-port/credentials.md).
  - **No legacy-alias / deprecation table.** The 10 renames in
    `deprecatedOptions` (conf/configuration.go:539-550) and the removals
    `Spotify.ID`/`Spotify.Secret` (:552) are **dropped, not recorded**: the
    fresh TOML only ever accepts the new spellings. There is no
    `mapDeprecatedOption` (:623-628) and no `explicitlySet` override
    (:630-635) in the Rust loader.
  - **Unknown keys in a config file are a startup ERROR**, not a warning. The
    Go behavior (`logUnknownOptions` :645-653, `unknownConfigKeys` :676-698)
    only warned; the Rust loader must refuse to start.

## 1. TOML root keys (carried)

All names are the Go field names (PascalCase), all cited from
`conf/configuration.go:33-165` unless noted. Every key below is **carried** into
the fresh TOML with its Go semantics and Go default
(`setViperDefaults`, conf/configuration.go:974-1155).

| Key | Type | Go default | Cite (struct / default) |
|---|---|---|---|
| `Address` | string | `"0.0.0.0"` | :35 / :980 |
| `Port` | int | `4533` | :36 / :981 |
| `UnixSocketPerm` | string | `"0660"` | :37 / :982 |
| `EnforceNonRootUser` | bool | `false` | :38 / :983 (validation :872-882) |
| `MusicFolder` | string | `./music` | :39 / :975 |
| `DataFolder` | Dir | `"."` | :40 / :977 |
| `CacheFolder` | Dir | `<DataFolder>/cache` when empty | :41 / :976, computed :391-393 |
| `DbPath` | string | `<DataFolder>/<DefaultDbPath>` when empty | :42 / :404-406 |
| `LogLevel` | string | `"info"` | :43 / :978 |
| `LogFile` | string | `""` | :44 / :979 |
| `SessionTimeout` | duration | `DefaultSessionTimeout` | :45 / :984, non-neg :431 |
| `BaseURL` | string | `""` (decomposed into BasePath/Host/Scheme, :467-477) | :46 / :985 |
| `BasePath` | string | `""` | :47 / — |
| `BaseHost` | string | `""` | :48 / — |
| `BaseScheme` | string | `""` | :49 / — |
| `TLSCert` | string | `""` | :50 / :986 |
| `TLSKey` | string | `""` | :51 / :987 (both required to enable TLS, :323-325) |
| `UILoginBackgroundURL` | string | `DefaultUILoginBackgroundURL` | :52 / :988 |
| `UIWelcomeMessage` | string | `""` | :53 / :989 |
| `MaxSidebarPlaylists` | int | `DefaultMaxSidebarPlaylists` | :54 / :990 |
| `EnableTranscodingConfig` | bool | `false` | :55 / :991 |
| `EnableDownloads` | bool | `true` | :56 / :1000 |
| `EnableExternalServices` | bool | `true` (disables integrations when false, :806-817) | :57 / :1001 |
| `EnableM3UExternalAlbumArt` | bool | `false` | :58 / :1002 |
| `EnableInsightsCollector` | bool | `true` | :59 / :1046 |
| `EnableScheduledDBAnalyze` | bool | `true` | :60 / :1047 |
| `EnableMediaFileCoverArt` | bool | `true` | :61 / :1003 |
| `TranscodingCacheSize` | byte-size string | `"100MB"` | :62 / :992 |
| `ImageCacheSize` | byte-size string | `"100MB"` | :63 / :993 |
| `AlbumPlayCountMode` | string | `absolute` | :64 / :994 |
| `EnableArtworkPrecache` | bool | `true` | :65 / :995 |
| `AutoImportPlaylists` | bool | `true` | :66 / :996 |
| `DefaultPlaylistPublicVisibility` | bool | `false` | :67 / :997 |
| `PlaylistsPath` | string | `""` (doublestar-pattern validated, :819-827) | :68 / :998 |
| `SmartPlaylistRefreshDelay` | duration | `DefaultSmartRefresh` | :69 / :999, non-neg :432 |
| `AutoTranscodeDownload` | bool | `false` | :70 / :1004 |
| `DefaultDownsamplingFormat` | string | `DefaultDownsamplingFormat` | :71 / :1005 |
| `RecentlyAddedByModTime` | bool | `false` | :74 / :1010 |
| `PreferSortTags` | bool | `false` | :75 / :1011 |
| `EnableNaturalSorting` | bool | `false` | :76 / :1012 |
| `IgnoredArticles` | string | `"The El La Los Las Le Les Os As O A"` | :77 / :1013 |
| `IndexGroups` | string | `"A B C … [Unknown]([)"` (see :1014) | :78 / :1014 |
| `FFmpegPath` | string | `""` | :79 / :1015 |
| `CoverArtPriority` | string | `"cover.*, folder.*, front.*, embedded, external"` | :82 / :1018 |
| `CoverArtQuality` | int | `75` | :83 / :1019 |
| `EnableWebPEncoding` | bool | `false` | :84 / :1020 |
| `ArtistArtPriority` | string | `"artist.*, album/artist.*, external"` | :85 / :1021 |
| `ArtistImageFolder` | string | `""` | :86 / :1022 |
| `DiscArtPriority` | string | `"disc*.*, cd*.*, …"` (see :1023) | :87 / :1023 |
| `LyricsPriority` | string | `".ttml,.yaml,…" (see :1024)` | :88 / :1024 |
| `EnableGravatar` | bool | `false` | :89 / :1025 |
| `EnableFavourites` | bool | `true` | :90 / :1026 |
| `EnableStarRating` | bool | `true` | :91 / :1027 |
| `EnableUserEditing` | bool | `true` | :92 / :1028 |
| `EnableArtworkUpload` | bool | `true` | :93 / :1038 |
| `MaxImageUploadSize` | byte-size string | `DefaultMaxImageUploadSize` (validated :457, :856-869) | :94 / :1039 |
| `MaxImageSize` | byte-size string | `DefaultMaxImageSize` (floored at MaxImageUploadSize, :523-529) | :95 / :1040 |
| `EnableSharing` | bool | `true` | :96 / :1041 |
| `ShareURL` | string | `""` | :97 / :1042 |
| `DefaultShareExpiration` | duration | `DefaultShareExpiration` | :98 / :1043, non-neg :433 |
| `DefaultDownloadableShare` | bool | `false` | :99 / :1044 |
| `DefaultTheme` | string | `"Dark"` | :100 / :1029 |
| `DefaultLanguage` | string | `""` | :101 / :1030 |
| `DefaultUIVolume` | int | `DefaultUIVolume` | :102 / :1031 |
| `UISearchDebounceMs` | int | `DefaultUISearchDebounceMs` | :103 / :1032 |
| `UICoverArtSize` | int | `DefaultUICoverArtSize` (clamped 200–1200, :512-516) | :104 / :1033 |
| `EnableReplayGain` | bool | `true` | :105 / :1034 |
| `EnableCoverAnimation` | bool | `true` | :106 / :1035 |
| `EnableNowPlaying` | bool | `true` | :107 / :1036 |
| `UIPlaybackReportInterval` | duration | `DefaultUIPlaybackReportInterval` | :108 / :1037, non-neg :434 |
| `GATrackingID` | string | `""` | :109 / :1045 |
| `EnableLogRedacting` | bool | `true` | :110 / :1048 |
| `AuthRequestLimit` | int | `5` | :111 / :1049 |
| `AuthWindowLength` | duration | `DefaultAuthWindowLength` | :112 / :1050, non-neg :435 |
| `ExtAuth` | section | see §2 | :114 |
| `HTTPHeaders` | section | see §2 | :116 |
| `Prometheus` | section | see §2 | :117 |
| `Scanner` | section | see §2 | :118 |
| `Backup` | section | see §2 | :120 |
| `PID` | section | see §2 | :121 |
| `Inspect` | section | see §2 | :122 |
| `Transcoding` | section | see §2 | :124 |
| `LastFM` | section | see §2 | :125 |
| `Deezer` | section | see §2 | :126 |
| `ListenBrainz` | section | see §2 | :127 |
| `Jellyfin` | section | see §2 | :128 |
| `EnableScrobbleHistory` | bool | `true` | :129 / :1100 |
| `Tags` | free-form map (prefix `Tags.*`) | — | :130 / prefix rule :766-768 |
| `Agents` | string | `"deezer,lastfm,listenbrainz"` | :131 / :1084 |
| `Search` | section | see §2 | :72 |
| `Matcher` | section | see §2 | :73 |

`ConfigFile` (conf/configuration.go:34, `conf:"-"`) is **non-settable** — it is
populated from the file actually used (:403) and must not be a TOML key; the
file is chosen via `--configfile` or `ND_CONFIGFILE` (:1197-1208).

## 2. Nested sections (carried)

Cites give struct and viper-default line ranges in `conf/configuration.go`.

- **`Search`** (`searchOptions` :298-301 / defaults :1006-1007):
  `Backend` (string, `"fts"`, normalized ∈ {`fts`,`legacy`} with fallback to
  `fts`, :933-942), `FullString` (bool, `false`).
- **`Matcher`** (:303-306 / :1008-1009): `PreferStarred` (bool, `true`),
  `FuzzyThreshold` (int, `85`).
- **`ExtAuth`** (`extAuthOptions` :292-296 / :1052-1054): `TrustedSources`
  (string, `""` — CIDR + unix-`@` semantics live in `server/auth.go:212,418,427`,
  frozen by #S4), `UserHeader` (string, `"Remote-User"`), `LogoutURL` (string,
  `""`, http(s) URL-validated, :459/:914-931).
- **`HTTPHeaders`** (:247-249 / :1101): `FrameOptions` (string, `"DENY"`).
- **`Prometheus`** (:251-255 / :1055-1057): `Enabled` (bool, `false`),
  `MetricsPath` (string, `PrometheusDefaultPath`), `Password` (string, `""`).
- **`Scanner`** (`scannerOptions` :167-180 / :1062-1073): `Enabled` (bool,
  `true`), `Schedule` (string, `"0"`; empty/`"0"` disables, crontab-validated
  :884-892/:904-910), `WatcherWait` (duration, `DefaultWatcherWait`, non-neg
  :436), `ScanOnStartup` (bool, `true`), `Extractor` (string,
  `DefaultScannerExtractor`, invalid falls back :518-521), `ArtistJoiner`
  (string, `ArtistJoiner`), `ArtistSplitExceptions` (string list, `[]`),
  `FollowSymlinks` (bool, `true`), `IgnoreDotFolders` (bool, `true`),
  `PurgeMissing` (string, `"never"` ∈ {never, always, full}, :845-854).
  **Dropped:** `GenreSeparators` (:175, deprecated in Go — use `Tags.genre.Split`)
  and `GroupAlbumReleases` (:176, deprecated — use `PID.Album`). Both are
  *already* deprecated upstream with in-config replacements; the fresh TOML
  carries only the replacements (`Tags.*`, `PID.*`).
- **`Backup`** (:266-270 / :1102-1104): `Count` (int, `0`), `Path` (Dir, `""`),
  `Schedule` (string, `""`, crontab-validated :894-902).
- **`PID`** (:272-275 / :1105-1106): `Track` (string, `DefaultTrackPID`),
  `Album` (string, `DefaultAlbumPID`); empty values get defaults (:501-503).
- **`Inspect`** (:277-282 / :1107-1110): `Enabled` (bool, `true`),
  `MaxRequests` (int, `1`), `BacklogLimit` (int), `BacklogTimeout` (int).
- **`Transcoding`** (:182-186 / :1081-1083): `MaxConcurrent` (int, `0`),
  `MaxConcurrentPerUser` (int, `0`), `EnableCancellation` (bool, `false`).
- **`LastFM`** (:207-216 / :1085-1089): `Enabled` (bool, `true`), `ApiKey`
  (string), `Secret` (string), `Language` (string, `DefaultInfoLanguage`),
  `ScrobbleFirstArtistOnly` (bool, `false`). Computed `Languages` (:215
  `conf:"-"`, :506, :829-843) is non-settable.
- **`Deezer`** (:218-224 / :1090-1091): `Enabled` (bool, `true`), `Language`
  (string). Computed `Languages` (:223 `conf:"-"`, :509) is non-settable.
- **`ListenBrainz`** (:226-231 / :1092-1095): `Enabled` (bool, `true`),
  `BaseURL` (string), `ArtistAlgorithm` (string), `TrackAlgorithm` (string).
- **`Jellyfin`** (:233-245 / :1096-1099, :1132): `Enabled` (bool, `false`),
  `ServerName` (string), `ExposedPublicUsers` (string), `AutoDiscovery`
  (bool, `false`), `QuickConnect` (bool, `true`), `MaxConcurrentStreams` (int,
  `max(2, MaxOpenConns()/2)`).
- **`Tags.*`** (free-form map of `TagConf` :198-205, prefix rule :766-768):
  per-tag keys `Ignore`, `Aliases`, `Type`, `MaxLength`, `Split`, `Album`.

## 3. Dev keys (carried, `Dev*`)

`DevLogLevels` (free-form map, prefix `DevLogLevels.*`, :134, :766-768),
`DevLogSourceLine` (:135, default `false` :1118), `DevEnableProfiler` (:136,
:1119), `DevAutoCreateAdminPassword` (:137, :1120), `DevAutoLoginUsername`
(:138, :1121), `DevActivityPanel` (:139, `true` :1122),
`DevActivityPanelUpdateRate` (:140, 300ms :1123, non-neg :438),
`DevSidebarPlaylists` (:141, `true` :1124), `DevShowArtistPage` (:142, `true`
:1125), `DevUIShowConfig` (:143, `true` :1126), `DevNewEventStream` (:144,
`true` :1127), `DevOffsetOptimize` (:145, 50000 :1129), `DevArtworkMaxRequests`
(:146, :1133), `DevArtworkThrottleBacklogLimit` (:147, :1134),
`DevArtworkThrottleBacklogTimeout` (:148, :1135, non-neg :439),
`DevArtworkThrottleBuffered` (:149, `true` :1136),
`DevArtworkWorkerConcurrency` (:150, :1139), `DevArtworkExternalMaxRPS` (:151,
2 :1142), `DevArtistInfoTimeToLive` (:152, :1143, non-neg :440),
`DevAlbumInfoTimeToLive` (:153, :1144, non-neg :441), `DevExternalScanner`
(:154, `true` :1145), `DevScannerThreads` (:155, 5 :1146),
`DevSelectiveWatcher` (:156, `true` :1147), `DevInsightsInitialDelay` (:157,
:1148, non-neg :442), `DevEnablePlayerInsights` (:158, `true` :1149),
`DevExternalArtistFetchMultiplier` (:161, 1.5 :1152),
`DevPreserveUnicodeInExternalCalls` (:162, `false` :1153),
`DevEnableMediaFileProbe` (:163, `true` :1154), `DevAPIv1` (:164, `false`
:1128).

**Dropped Dev keys** (sole consumers are removed subsystems):
`DevEnablePluginsInsights` (:159, :1150) and `DevPluginCompilationTimeout`
(:160, :1151) — plugins host cancelled (issues.md:504); dropped together with
`Plugins.*`.

## 4. Dropped keys — complete list with citations

| Dropped key | Go cite | Reason (settled decision) |
|---|---|---|
| `Subsonic.AppendSubtitle`, `Subsonic.AppendAlbumVersion`, `Subsonic.ArtistParticipations`, `Subsonic.DefaultReportRealPath`, `Subsonic.EnableAverageRating`, `Subsonic.LegacyClients`, `Subsonic.MinimalClients` | conf/configuration.go:188-196, defaults :1074-1080 | Subsonic API absent from the fresh service (issues.md:502) |
| `Plugins.Enabled`, `Plugins.Folder`, `Plugins.CacheSize`, `Plugins.AutoReload`, `Plugins.LogLevel` | conf/configuration.go:284-290, defaults :1111-1115; folder defaults :395-401 | Plugin host cancelled (issues.md:504) |
| `Jukebox.Enabled`, `Jukebox.Devices`, `Jukebox.Default`, `Jukebox.AdminOnly` | conf/configuration.go:259-264, defaults :1058-1061 | Jukebox not ported (issues.md:502) |
| `MPVPath`, `MPVCmdTemplate` | conf/configuration.go:80-81, defaults :1016-1017 | Only consumer is the dropped jukebox; dropped with it (consequence of issues.md:502 — flagged for the verifier as an inference) |
| `PasswordEncryptionKey` | conf/configuration.go:113, default :1051 | Dead config; keys the never-ported AES-GCM store (issues.md:502; credentials.md) |
| `DevEnablePluginsInsights`, `DevPluginCompilationTimeout` | conf/configuration.go:159-160, defaults :1150-1151 | Plugins cancelled (issues.md:504) |
| `Scanner.GenreSeparators`, `Scanner.GroupAlbumReleases` | conf/configuration.go:175-176, defaults :1069-1070; deprecated entries :540-541 | Already deprecated upstream; only the replacements (`Tags.genre.Split`, `PID.*`) are carried |
| All 10 legacy aliases (`SearchFullString`, `ReverseProxyWhitelist`, `ReverseProxyUserHeader`, `HTTPSecurityHeaders.CustomFrameOptionsValue`, `CoverJpegQuality`, `SimilarSongsMatchThreshold`, `EnableTranscodingCancellation`, and the three ignored ones above) | conf/configuration.go:539-550 | No deprecation table in the fresh service (issues.md:502); only canonical spellings exist |
| `Spotify.ID`, `Spotify.Secret` | conf/configuration.go:552 | Already removed upstream; not resurrected |

The `ConfigFile` field (conf/configuration.go:34, `conf:"-"`) and the computed
fields `LastFM.Languages` (:215), `Deezer.Languages` (:223) are non-settable
and are **not** TOML keys.

## 5. Retained semantics layer

1. **Case-insensitive keys.** Viper lowercases every key on load
   (conf/configuration.go:717-744); canonical spelling is PascalCase per
   `configKeys` (:748-780) and `canonicalOptionName` (:704-715). The Rust
   loader matches keys case-insensitively and preserves this acceptance set.
2. **`ND_`-env remap with duplicate hard error.** `ND_`-prefixed keys appearing
   *in the config file* are remapped to canonical dotted keys
   (`remapEnvVarKeysFromConfig` :589-619, `ndKeyToCanonical` :700-702); if both
   spellings are present it is a **fatal** error (:601-607). Environment
   variables bind with prefix `ND_` and `.`→`_` replacement
   (`InitConfig` :1181-1187); `ND_CONFIGFILE` selects the file (:1197-1208).
3. **Precedence ladder** (highest wins): explicit `viper.Set` (none in the
   fresh loader — the deprecated-alias tier is gone) → environment variable →
   config file key → built-in default (`setViperDefaults` :974-1155). INI
   `[default]` section merges into the root map
   (`parseIniFileConfiguration` :784-804; fixture `conf/testdata/cfg.ini`).
4. **Unknown keys are a startup error.** `unknownConfigKeys` (:676-698)
   computed the unknown set (minus INI `default.*` passthrough and resolvable
   `nd_*` keys); the Go loader only warned (:645-653). **The Rust loader must
   `logFatal`/exit on any unknown key** (issues.md:502). Fixtures for the
   error behavior: `conf/testdata/cfg_unknown_keys.toml`,
   `cfg_unknown_casing.{toml,ini,json,yaml}`, `cfg_nd_bogus.toml`,
   `cfg_nd_conflict.toml`.
5. **Value types.**
   - `Dir` (conf/dir.go:9-77): plain path string; `os.MkdirAll` on every
     `Path()` call (idempotent), optional perm (default `os.ModePerm`),
     empty path returned as-is. Applies to `DataFolder`, `CacheFolder`,
     `Backup.Path`.
   - Byte-size strings (`humanize.ParseBytes`): `TranscodingCacheSize`,
     `ImageCacheSize`, `MaxImageUploadSize`, `MaxImageSize` — validated by
     `validateByteSize` (:856-869); `MaxImageSize` is floored at
     `MaxImageUploadSize` (:523-529).
   - Durations (Go `time.Duration` syntax; negative values fall back to the
     default with a warning, `durationNonNegativeOrDefault` :357-362, calls
     :431-443): `SessionTimeout`, `SmartPlaylistRefreshDelay`,
     `DefaultShareExpiration`, `UIPlaybackReportInterval`, `AuthWindowLength`,
     `Scanner.WatcherWait`, and the `Dev*TimeToLive`/throttle durations.
     Fixture: `conf/testdata/valid_duration.toml` / `invalid_duration.toml`.
   - Crontab validation (`scheduler.ParseCrontab`, robfig 5-field format):
     `Scanner.Schedule` (:884-892) and `Backup.Schedule` (:894-902), both via
     `validateSchedule` (:904-910).
   - String lists: comma-separated env/env-file values accepted
     (`Scanner.ArtistSplitExceptions`).
   - Free-form maps: `Tags.*` and `DevLogLevels.*` accept arbitrary subkeys
     (prefix rule :766-768).
6. **`Search.Backend` fallback.** Normalized case-insensitively; only `fts`
   and `legacy` are valid; anything else logs an error and falls back to
   `fts` (`normalizeSearchBackend` :933-942).
7. **Startup validation carried unchanged** (all in `Load`, :452-463 and
   inline): `EnforceNonRootUser` (:872-882), `PlaylistsPath` doublestar
   patterns (:819-827), `Scanner.PurgeMissing` enum (:845-854),
   byte-sizes, `ExtAuth.LogoutURL` http(s) URL (:914-931), `BaseURL` parse and
   decomposition (:467-477), `UICoverArtSize` clamp (:512-516),
   `Scanner.Extractor` fallback (:518-521), empty-`PID.*` defaults
   (:501-503), language-list computation (:829-843).

## 6. Non-goals / consumers

- No INI/JSON/YAML config support is required beyond what #03 chooses; this
  contract fixes the **key set and semantics**, not the parser. The Go loader
  accepted all four (fixtures in `conf/testdata/`); the fresh service only
  promises TOML plus the `ND_`-env tier.
- Consumers gated on this contract: #03 (loader), #14/#17/#18/#24 (feature
  keys), #32 (`EnableInsightsCollector`), #05/#12 (`Agents`,
  `Scanner.*`, `Matcher.*`).
