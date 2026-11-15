//! Time helpers — port of `utils/time.go`.

use std::time::Duration;

/// Returns the newest of `times`, ignoring zero (`None`) values
/// (`utils.TimeNewest`). All-`None` input yields `None` (Go returns the zero
/// time).
#[must_use]
pub fn time_newest(times: &[Option<std::time::SystemTime>]) -> Option<std::time::SystemTime> {
    let mut newest: Option<std::time::SystemTime> = None;
    for t in times.iter().flatten() {
        if newest.is_none_or(|n| t > &n) {
            newest = Some(*t);
        }
    }
    newest
}

/// Errors from [`parse_duration`].
#[derive(Debug)]
pub enum DurationError {
    /// The input was not a valid duration.
    Invalid(String),
    /// The duration was negative.
    Negative(String),
}

impl std::fmt::Display for DurationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(s) => write!(f, "invalid duration: {s:?}"),
            Self::Negative(s) => write!(f, "negative duration not allowed: {s:?}"),
        }
    }
}

/// A parsed duration: weeks/days plus a sub-day remainder.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParsedDuration {
    /// Whole weeks.
    pub weeks: u64,
    /// Whole days after the weeks.
    pub days: u64,
    /// Sub-day remainder (hours and below), in Go `time.Duration.String`
    /// form where relevant.
    pub rest: Duration,
}

/// Parses a Go duration string, extended with `d` (24h) and `w` (168h) units
/// (`utils.ParseDuration`). Negative durations are rejected.
///
/// # Errors
/// [`DurationError::Invalid`] for malformed input, [`DurationError::Negative`]
/// for negative values.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    // utils.ParseDuration expands d/w first, then delegates to Go's signed
    // time.ParseDuration and rejects a negative resulting Duration.
    let expanded = expand_day_week(s);
    let (negative, mut rest) = match expanded.as_bytes().first() {
        Some(b'-') => (true, &expanded[1..]),
        Some(b'+') => (false, &expanded[1..]),
        _ => (false, expanded.as_str()),
    };
    if rest == "0" {
        return Ok(Duration::ZERO);
    }
    if rest.is_empty() {
        return Err(format!("invalid duration {s:?}: empty"));
    }

    const MAX_ABS_NANOS: u64 = 1_u64 << 63;
    let mut total_nanos = 0_u64;
    while !rest.is_empty() {
        let bytes = rest.as_bytes();
        let mut i = 0;
        let mut whole = 0_u64;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            whole = whole
                .checked_mul(10)
                .and_then(|value| value.checked_add(u64::from(bytes[i] - b'0')))
                .filter(|value| *value <= MAX_ABS_NANOS)
                .ok_or_else(|| format!("invalid duration {s:?}: overflow"))?;
            i += 1;
        }
        let has_whole = i > 0;
        let mut fraction = 0_u64;
        let mut scale = 1.0_f64;
        let mut has_fraction = false;
        let mut fraction_overflow = false;
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                has_fraction = true;
                if !fraction_overflow {
                    if fraction > (MAX_ABS_NANOS - 1) / 10 {
                        fraction_overflow = true;
                    } else {
                        let next = fraction * 10 + u64::from(bytes[i] - b'0');
                        if next > MAX_ABS_NANOS {
                            fraction_overflow = true;
                        } else {
                            fraction = next;
                            scale *= 10.0;
                        }
                    }
                }
                i += 1;
            }
        }
        if !has_whole && !has_fraction {
            return Err(format!("invalid duration {s:?}: expected number"));
        }

        let unit_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_digit() && bytes[i] != b'.' {
            i += 1;
        }
        if unit_start == i {
            return Err(format!("invalid duration {s:?}: missing unit"));
        }
        let unit = &rest[unit_start..i];
        let unit_nanos: u64 = match unit {
            "ns" => 1,
            "us" | "µs" | "μs" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60_000_000_000,
            "h" => 3_600_000_000_000,
            _ => return Err(format!("invalid duration {s:?}: unknown unit {unit:?}")),
        };
        let mut component = whole
            .checked_mul(unit_nanos)
            .filter(|value| *value <= MAX_ABS_NANOS)
            .ok_or_else(|| format!("invalid duration {s:?}: overflow"))?;
        if fraction > 0 && scale.is_finite() {
            let fractional_nanos = (fraction as f64 * (unit_nanos as f64 / scale)) as u64;
            component = component
                .checked_add(fractional_nanos)
                .filter(|value| *value <= MAX_ABS_NANOS)
                .ok_or_else(|| format!("invalid duration {s:?}: overflow"))?;
        }
        total_nanos = total_nanos
            .checked_add(component)
            .filter(|value| *value <= MAX_ABS_NANOS)
            .ok_or_else(|| format!("invalid duration {s:?}: overflow"))?;
        rest = &rest[i..];
    }

    if negative && total_nanos != 0 {
        return Err(format!("negative duration not allowed: {s:?}"));
    }
    if !negative && total_nanos > i64::MAX as u64 {
        return Err(format!("invalid duration {s:?}: overflow"));
    }
    Ok(Duration::new(
        total_nanos / 1_000_000_000,
        (total_nanos % 1_000_000_000) as u32,
    ))
}

/// Expand exactly the `-?\\d+(?:\\.\\d+)?[dw]` matches in utils/time.go.
fn expand_day_week(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let mut number_start = i;
        if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            number_start += 1;
        }
        if bytes[number_start].is_ascii_digit() {
            let mut end = number_start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end < bytes.len()
                && bytes[end] == b'.'
                && end + 1 < bytes.len()
                && bytes[end + 1].is_ascii_digit()
            {
                end += 1;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
            }
            if end < bytes.len() && matches!(bytes[end], b'd' | b'w') {
                let value: f64 = s[start..end].parse().unwrap_or(0.0);
                let hours = if bytes[end] == b'w' {
                    value * 24.0 * 7.0
                } else {
                    value * 24.0
                };
                out.push_str(&format!("{hours}h"));
                i = end + 1;
                continue;
            }
        }
        let ch = s[i..].chars().next().expect("valid UTF-8 boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Formats a whole-week/day duration using those units, falling back to
/// hour/minute/second rendering for the sub-day remainder
/// (`utils.FormatDuration`), so [`parse_duration`] round-trips.
#[must_use]
pub fn format_duration(d: Duration) -> String {
    if d < Duration::from_secs(3600 * 24) {
        return format_sub_day(d);
    }
    let mut b = String::new();
    let week = 7 * 24 * 3600;
    let mut secs = d.as_secs();
    let weeks = secs / week;
    if weeks > 0 {
        b.push_str(&format!("{weeks}w"));
        secs %= week;
    }
    let day = 24 * 3600;
    let days = secs / day;
    if days > 0 {
        b.push_str(&format!("{days}d"));
        secs %= day;
    }
    if secs > 0 || d.subsec_nanos() > 0 {
        b.push_str(&format_sub_day(Duration::new(secs, d.subsec_nanos())));
    }
    b
}

fn format_sub_day(d: Duration) -> String {
    let secs = d.as_secs();
    let nanos = d.subsec_nanos();
    if secs == 0 && nanos == 0 {
        return "0s".to_string();
    }
    if secs >= 3600 && secs.is_multiple_of(3600) && nanos == 0 {
        return format!("{}h", secs / 3600);
    }
    // Go's time.Duration.String selects ns, µs, or ms for values below one
    // second, using up to 0/3/6 fractional digits respectively.
    if secs == 0 && nanos < 1_000_000_000 {
        let (divisor, suffix, precision) = if nanos < 1_000 {
            (1, "ns", 0)
        } else if nanos < 1_000_000 {
            (1_000, "µs", 3)
        } else {
            (1_000_000, "ms", 6)
        };
        let whole = nanos / divisor;
        let remainder = nanos % divisor;
        if remainder == 0 {
            return format!("{whole}{suffix}");
        }
        let fraction = format!("{remainder:0precision$}");
        return format!("{whole}.{}{suffix}", fraction.trim_end_matches('0'));
    }
    // time.Duration.String for the sub-day remainder
    let mut out = String::new();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        out.push_str(&format!("{h}h"));
    }
    if m > 0 || (h > 0 && (s > 0 || nanos > 0)) {
        out.push_str(&format!("{m}m"));
    }
    {
        if nanos == 0 {
            out.push_str(&format!("{s}s"));
        } else {
            let frac = format!("{:09}", nanos);
            let frac = frac.trim_end_matches('0');
            out.push_str(&format!("{s}.{frac}s"));
        }
    }
    if out.is_empty() {
        return "0s".to_string();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_newest_returns_the_newest() {
        assert_eq!(time_newest(&[]), None);
        let t1 = std::time::SystemTime::UNIX_EPOCH;
        let t2 = t1 + Duration::from_secs(31_556_952);
        let t3 = t1 + Duration::from_secs(1000);
        assert_eq!(time_newest(&[Some(t1)]), Some(t1));
        assert_eq!(time_newest(&[Some(t1), Some(t2), Some(t3)]), Some(t2));
        assert_eq!(time_newest(&[None, Some(t2), None]), Some(t2));
    }

    #[test]
    fn parse_duration_parses_valid_durations() {
        let cases = [
            ("90m", Duration::from_secs(90 * 60)),
            ("12h", Duration::from_secs(12 * 3600)),
            ("1d", Duration::from_secs(24 * 3600)),
            ("1w", Duration::from_secs(7 * 24 * 3600)),
            ("3d", Duration::from_secs(72 * 3600)),
            ("1d12h", Duration::from_secs(36 * 3600)),
            ("1w2d3h", Duration::from_secs((7 * 24 + 2 * 24 + 3) * 3600)),
            ("0.5d", Duration::from_secs(12 * 3600)),
            (".5d", Duration::from_secs(7 * 60 + 12)),
            (
                "1.5.5d",
                Duration::from_secs(4_075) + Duration::from_millis(200),
            ),
            ("0", Duration::ZERO),
            ("+0", Duration::ZERO),
            ("-0", Duration::ZERO),
            ("+1h", Duration::from_secs(3600)),
            ("+1d", Duration::from_secs(24 * 3600)),
            ("500ms", Duration::from_millis(500)),
            ("100ns", Duration::from_nanos(100)),
            ("1.5us", Duration::from_nanos(1_500)),
            ("1.5µs", Duration::from_nanos(1_500)),
            ("1.5μs", Duration::from_nanos(1_500)),
            ("1ms2s", Duration::from_millis(2001)),
            ("1.111s", Duration::from_millis(1111)),
            ("-0.5ns", Duration::ZERO),
        ];
        for (input, expected) in cases {
            let d = parse_duration(input).unwrap_or_else(|e| panic!("{input}: {e}"));
            assert_eq!(d, expected, "input {input}");
        }
    }

    #[test]
    fn parse_duration_rejects_invalid_durations() {
        for input in [
            "",
            "tomorrow",
            "42",
            "d",
            "5y",
            "3.d",
            "1h-30m",
            "100000000d",
            "9999999999999999999999h",
        ] {
            assert!(parse_duration(input).is_err(), "input {input:?}");
        }
    }

    #[test]
    fn parse_duration_rejects_negative_durations() {
        for input in ["-1d", "-0.5w", "-30m"] {
            let err = parse_duration(input).expect_err("must reject");
            assert!(
                err.contains("negative duration"),
                "input {input:?} err {err}"
            );
        }
    }

    #[test]
    fn format_duration_uses_the_largest_whole_units() {
        let cases = [
            (Duration::from_secs(7 * 24 * 3600), "1w"),
            (Duration::from_secs(24 * 3600), "1d"),
            (Duration::from_secs(72 * 3600), "3d"),
            (Duration::from_secs(36 * 3600), "1d12h"),
            (Duration::from_secs((7 * 24 + 2 * 24 + 3) * 3600), "1w2d3h"),
            (Duration::from_secs(12 * 3600), "12h"),
            (Duration::from_secs(90 * 60), "1h30m0s"),
            (Duration::ZERO, "0s"),
            (Duration::from_micros(1_500), "1.5ms"),
            (
                Duration::from_micros(1_500) + Duration::from_nanos(25),
                "1.500025ms",
            ),
            (Duration::from_nanos(100), "100ns"),
            (Duration::from_secs(3_601), "1h0m1s"),
        ];
        for (input, expected) in cases {
            assert_eq!(format_duration(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn format_duration_round_trips_through_parse_duration() {
        for input in ["1d", "1w", "1d12h", "90m", "500ms", "100ns", "1.5µs", "0"] {
            let d = parse_duration(input).unwrap_or_else(|e| panic!("{input}: {e}"));
            let formatted = format_duration(d);
            let d2 = parse_duration(&formatted).unwrap_or_else(|e| panic!("{formatted}: {e}"));
            assert_eq!(d2, d, "input {input}");
        }
    }
}
