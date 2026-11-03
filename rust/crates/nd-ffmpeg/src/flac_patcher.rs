//! `core/ffmpeg/flac_streaminfo.go` — repair the STREAMINFO total_samples
//! ffmpeg leaves at 0 when writing FLAC to a pipe: a decoder cannot seek a
//! cached FLAC without it. Port: `patchFLACDuration`/`setFLACTotalSamples`.

/// `flacPrefixLen` (`flac_streaminfo.go:12`) — through the last total_samples
/// byte.
pub const FLAC_PREFIX_LEN: usize = 26;

/// `flacMaxTotalSamples` (`flac_streaminfo.go:13`) — the 36-bit field.
pub const FLAC_MAX_TOTAL_SAMPLES: u64 = (1u64 << 36) - 1u64;

/// `setFLACTotalSamples` (`flac_streaminfo.go:51-66`) over a full 26-byte
/// prefix (the Go caller only invokes it after a full `io.ReadFull`), returning
/// the possibly-patched bytes. Byte-exact gates: "fLaC" magic AND the first
/// metadata block must be STREAMINFO (`prefix[4] & 0x7F == 0`), header rate
/// non-zero, total_samples field currently zero (an already-populated header
/// is left alone), and the computed total must fit the 36-bit field.
pub fn set_flac_total_samples(prefix: Vec<u8>, duration: f32) -> Vec<u8> {
    if prefix.len() < FLAC_PREFIX_LEN {
        return prefix;
    }

    if prefix[..4] != *"fLaC".as_bytes() || prefix[4] & 0x7F != 0 {
        return prefix;
    }

    let info = be_u64_at(&prefix, 18);
    let rate = info >> 44;
    if rate == 0 || info & FLAC_MAX_TOTAL_SAMPLES != 0 {
        return prefix;
    }

    // Go: math.Round(float64(duration) * float64(rate)) — duration is a float32
    // field, so 0.7f32 (0.69999998...) * 44100 rounds to 30870, not truncation's
    // 30869.
    let total = round_half_away((duration as f64) * (rate as f64));
    if total > FLAC_MAX_TOTAL_SAMPLES {
        return prefix;
    }

    let mut out = prefix;
    let combined = info | total;
    for i in 0..8 {
        out[18 + i] = (combined >> ((7usize - i) * 8)) as u8;
    }
    out
}

/// `patchFLACDuration` (`flac_streaminfo.go:18-47`) applied to a whole stream
/// buffer: duration <= 0 passes through untouched, as does a stream shorter
/// than the fields we patch. The Go patcher splices the patched 26-byte
/// prefix back in front of the un-read remainder; over a fully-read stream
/// that splice is this identical byte-for-byte operation (the golden fixtures
/// in tests/fixtures record Go's full-stream output).
pub fn patch_flac_duration(stream: Vec<u8>, duration: f32) -> Vec<u8> {
    if duration <= 0.0 || stream.len() < FLAC_PREFIX_LEN {
        return stream;
    }

    let head = set_flac_total_samples(stream[..FLAC_PREFIX_LEN].to_vec(), duration);
    let mut out = head;
    out.extend(stream[FLAC_PREFIX_LEN..].to_vec());
    out
}

/// Go's `math.Round`: ties away from zero (inputs here are positive).
fn round_half_away(x: f64) -> u64 {
    if x <= 0.0 {
        return 0;
    }

    (x + 0.5).floor() as u64
}

/// Big-endian u64 read at `off` (Go `binary.BigEndian.Uint64(prefix[18:])`).
fn be_u64_at(buf: &[u8], off: usize) -> u64 {
    let mut acc: u64 = 0;
    for i in 0..8 {
        acc = (acc << 8) | (buf[off + i] as u64);
    }
    acc
}
