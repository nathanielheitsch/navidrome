//! crate errors — `core/ffmpeg/ffmpeg.go` `ProbeError` plus the bare
//! `errors.New`/`fmt.Errorf` sites. `ProbeError` keeps Go's two-headed
//! reporting: the path-free `reason` is client-safe (`SafeReason()`),
//! `detail()` (composed from ffprobe stderr) may contain paths.

/// The `ProbeError` struct (`ffmpeg.go:177-229`): an ffprobe failure.
#[derive(Debug, Clone)]
pub struct ProbeError {
    /// Media path the probe ran against (`Path`).
    pub path: String,
    /// Path-free reason safe to expose to clients (`SafeReason()`).
    pub reason: String,
    /// Marks the MEDIA FILE itself as missing (`NotFound`). A ffprobe launch
    /// failure also wraps fs.ErrNotExist in Go, so callers must not infer
    /// this from the error chain — the flag is set only from the file's stat.
    pub not_found: bool,
}

/// Crate error enum: the bare `errors.New`/`fmt.Errorf` sites in ffmpeg.go.
#[derive(Debug, thiserror::Error)]
pub enum FFmpegError {
    /// `ffmpeg.go:66` — `ErrAnimatedWebPUnsupported`.
    #[error("ffmpeg lacks libwebp_anim encoder — install an ffmpeg build with libwebp")]
    ErrAnimatedWebPUnsupported,

    /// `ffmpeg.go:139-147` — `fileExists`: "'{path}' is a directory".
    #[error("'{path}' is a directory")]
    IsDirectory {
        /// The path that statted as a directory (`ffmpeg.go:143`).
        path: String,
    },

    /// `ffmpeg.go:296` — parseProbeOutput miss.
    #[error("no audio stream found in ffprobe output")]
    NoAudioStream,

    /// `ffmpeg.go:167` — "parsing ffprobe output: {err}".
    #[error("parsing ffprobe output: {0}")]
    ProbeParse(String),

    /// `ffmpeg.go:604-622` — `ffmpegCmd()` returned the LookPath failure: no
    /// resolved binary. Carries the unresolved path (Go returns `ffmpegPath`
    /// alongside the error, and `ProbeAudioStream`/`Transcode`/`ExtractImage`/
    /// `ConvertAnimatedImage` propagate it verbatim at `ffmpeg.go:76-78`,
    /// `:129-131`, `:150-153`).
    #[error("ffmpeg not available: {0}")]
    NotAvailable(String),
}

/// `fileAccessReason` (`ffmpeg.go:200-209`): map a stat failure to a clear,
/// path-free reason so a moved/unreadable file reads as "file not found"
/// rather than a raw ffprobe message. Go switches on `errors.Is(err,
/// fs.ErrNotExist)`/`fs.ErrPermission`; the port classifies at the call
/// site and passes this token.
pub fn file_access_reason(stat_kind: String) -> String {
    match stat_kind.as_str() {
        "not_found" => "file not found".to_string(),
        "permission_denied" => "permission denied".to_string(),
        _ => "file not accessible".to_string(),
    }
}
