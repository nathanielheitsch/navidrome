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
    let negative = s.trim_start().starts_with('-');
    let expanded = expand_day_week(s);
    let mut total_secs: f64 = 0.0;
    let mut any = false;
    let mut rest = expanded.as_str();
    while !rest.is_empty() {
        let sign_len = rest.chars().take_while(|c| *c == '-').count();
        let num_len = sign_len
            + rest[sign_len..]
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .count();
        if num_len == 0 {
            return Err(format!("invalid duration {s:?}: unexpected unit"));
        }
        let (num, unit_rest) = rest.split_at(num_len);
        let value: f64 = num.parse().map_err(|_| format!("invalid duration {s:?}"))?;
        let unit = unit_rest
            .chars()
            .next()
            .ok_or_else(|| format!("invalid duration {s:?}: missing unit"))?;
        let unit_str: String = unit_rest
            .chars()
            .skip(1)
            .take_while(|c| matches!(c, 's' | 'm' | 'h' | 'n' | 'u' | 'µ'))
            .collect();
        let mult = match (unit, unit_str.as_str()) {
            ('h', "") => 3600.0,
            ('m', "") => 60.0,
            ('s', "") => 1.0,
            ('n', "s") | ('u', "s") | ('µ', "s") => {
                return Err(format!("invalid duration {s:?}: unsupported unit"))
            }
            _ => return Err(format!("invalid duration {s:?}: unknown unit {unit:?}")),
        };
        total_secs += value * mult;
        rest = &unit_rest[1 + unit_str.len()..];
        any = true;
    }
    if !any {
        return Err(format!("invalid duration {s:?}: empty"));
    }
    let negative = negative || total_secs < 0.0;
    if negative {
        return Err(format!("negative duration not allowed: {s:?}"));
    }
    let nanos = total_secs.fract() * 1e9;
    Ok(Duration::new(total_secs as u64, nanos as u32))
}

/// Expands `<num>d` and `<num>w` (with optional fraction and sign) into hours.
fn expand_day_week(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'-' || c.is_ascii_digit() || c == b'.' {
            // Consume a number.
            let start = i;
            let mut j = i;
            if bytes[j] == b'-' {
                j += 1;
            }
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b'.') {
                j += 1;
            }
            // Check for d/w unit.
            if j < bytes.len() && (bytes[j] == b'd' || bytes[j] == b'w') {
                let num = std::str::from_utf8(&bytes[start..j]).unwrap_or("");
                let value: f64 = num.parse().unwrap_or(0.0);
                let mut hours = value * 24.0;
                if bytes[j] == b'w' {
                    hours = value * 24.0 * 7.0;
                }
                out.push_str(&format!("{hours}h"));
                i = j + 1;
                continue;
            }
            // Not a day/week: copy through.
            out.push_str(std::str::from_utf8(&bytes[start..j]).unwrap_or(""));
            i = j;
            continue;
        }
        out.push(char::from(c));
        i += 1;
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
    if secs >= 3600 && secs.is_multiple_of(3600) && nanos == 0 {
        return format!("{}h", secs / 3600);
    }
    // time.Duration.String for the sub-day remainder
    let mut out = String::new();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        out.push_str(&format!("{h}h"));
    }
    if m > 0 {
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
        ];
        for (input, expected) in cases {
            let d = parse_duration(input).unwrap_or_else(|e| panic!("{input}: {e}"));
            assert_eq!(d, expected, "input {input}");
        }
    }

    #[test]
    fn parse_duration_rejects_invalid_durations() {
        for input in ["", "tomorrow", "42", "d", "5y"] {
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
        ];
        for (input, expected) in cases {
            assert_eq!(format_duration(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn format_duration_round_trips_through_parse_duration() {
        for input in ["1d", "1w", "1d12h", "90m"] {
            let d = parse_duration(input).unwrap_or_else(|e| panic!("{input}: {e}"));
            let formatted = format_duration(d);
            let d2 = parse_duration(&formatted).unwrap_or_else(|e| panic!("{formatted}: {e}"));
            assert_eq!(d2, d, "input {input}");
        }
    }
}
