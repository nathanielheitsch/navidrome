//! `nd-ffmpeg` — Rust port of `core/ffmpeg/**` (issue #07, docs/rust-port/issues.md
//! Part B): the ffmpeg/ffprobe process adapter plus the FLAC STREAMINFO patcher.
//!
//! Design divergence from Go (frozen here): Go's `FFmpeg` interface threads an
//! ambient `context.Context` through every call and cancels the child when that
//! context is cancelled. Rust has no ambient context — the returned stream
//! ([`FFCmd`], after Go's `ffCmd` struct) owns the tokio child handle, and an
//! explicit `close()`/cancellation SIGKILLS the whole process group
//! (`process_group(1)` on unix) and reaps the child: no zombies, no orphaned
//! stdout — the hard requirement of #07.

pub mod args;
pub mod encoder_probe;
pub mod errors;
pub mod ffmpeg_cmd;
pub mod flac_patcher;
pub mod probe;

pub use args::{
    bit_depth_to_sample_fmt, build_dynamic_args, build_template_args, create_ffmpeg_command,
    ffprobe_path, fix_cmd, inject_before_output, inject_dynamic_audio_flags, is_default_command,
    is_lossless_output_format, TranscodeOptions,
};
pub use flac_patcher::{
    patch_flac_duration, set_flac_total_samples, FLAC_MAX_TOTAL_SAMPLES, FLAC_PREFIX_LEN,
};
pub use probe::{
    parse_probe_output, probe_client_reason, probe_detail, AudioProbeResult, ProbeFormat,
    ProbeOutput, ProbeStream,
};
