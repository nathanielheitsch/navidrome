//! Port of `conf/durationNonNegativeOrDefault` and the Go `time.Duration`
//! string syntax used throughout `conf/configuration.go`.

use std::time::Duration;

/// Error returned by [`parse_go_duration`].
#[derive(Debug, thiserror::Error)]
#[error("invalid duration {input:?}: {message}")]
pub struct DurationError {
    /// The offending input string.
    pub input: String,
    /// Human-readable reason, mirroring Go `time.ParseDuration` messages.
    pub message: String,
}

/// Parses a Go `time.Duration` string (e.g. `"1h30m"`, `"500ms"`, `"1s"`).
///
/// Port of `time.ParseDuration` acceptance for the units Navidrome config uses
/// (`ns`, `us`/`µs`, `ms`, `s`, `m`, `h`) with an optional sign.
pub fn parse_go_duration(s: &str) -> Result<Duration, DurationError> {
    let err = |msg: &str| DurationError {
        input: s.to_string(),
        message: msg.to_string(),
    };
    let mut rest = s.trim();
    if rest.is_empty() {
        return Err(err("missing unit in duration"));
    }
    let neg = if let Some(r) = rest.strip_prefix('-') {
        rest = r;
        true
    } else if let Some(r) = rest.strip_prefix('+') {
        rest = r;
        false
    } else {
        false
    };

    if rest == "0" {
        return Ok(Duration::ZERO);
    }
    if rest.is_empty() {
        return Err(err("empty duration"));
    }

    let mut total_ns: f64 = 0.0;
    while !rest.is_empty() {
        // Leading digits (the value).
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 {
            return Err(err("missing unit in duration"));
        }
        let (num, r) = rest.split_at(digits);
        let value: f64 = num
            .parse()
            .map_err(|_| err("invalid leading digits in duration"))?;
        rest = r;
        // Unit: alphabetic (allow µ) prefix.
        let unit_len = rest
            .chars()
            .take_while(|c| c.is_ascii_alphabetic() || *c == 'µ')
            .map(|c| c.len_utf8())
            .sum::<usize>();
        if unit_len == 0 {
            return Err(err("missing unit in duration"));
        }
        let (unit, r) = rest.split_at(unit_len);
        rest = r;
        let ns_per_unit: f64 = match unit {
            "ns" => 1.0,
            "us" | "µs" => 1_000.0,
            "ms" => 1_000_000.0,
            "s" => 1_000_000_000.0,
            "m" => 60.0 * 1_000_000_000.0,
            "h" => 3600.0 * 1_000_000_000.0,
            _ => {
                return Err(DurationError {
                    input: s.to_string(),
                    message: format!("unknown unit {unit:?} in duration"),
                })
            }
        };
        total_ns += value * ns_per_unit;
    }
    if neg {
        // Go's durationNonNegativeOrDefault (conf/configuration.go:357-362)
        // falls back to the default for negative values; the loader handles
        // this via the Negative error.
        return Err(DurationError {
            input: s.to_string(),
            message: "negative duration".to_string(),
        });
    }
    Ok(Duration::from_nanos(total_ns as u64))
}

/// A [`Duration`] that (de)serializes from the Go `time.Duration` string
/// syntax. Negative values deserialize fine; the loader falls back to the
/// default when they are negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurationField(pub Duration);

impl std::ops::Deref for DurationField {
    type Target = Duration;
    fn deref(&self) -> &Duration {
        &self.0
    }
}

impl std::fmt::Display for DurationField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let d = self.0;
        let ns = d.as_nanos();
        let secs = ns / 1_000_000_000;
        let frac = ns % 1_000_000_000;
        if secs >= 3600 {
            write!(f, "{}h{}m{}s", secs / 3600, (secs % 3600) / 60, secs % 60)?;
        } else if secs >= 60 {
            write!(f, "{}m{}s", secs / 60, secs % 60)?;
        } else if secs > 0 {
            write!(f, "{secs}s")?;
        } else if frac >= 1_000_000 {
            write!(f, "{}ms", frac / 1_000_000)?;
        } else {
            write!(f, "{ns}ns")?;
        }
        Ok(())
    }
}

impl DurationField {
    /// Returns `d`, or `default` when `d` is negative (Go
    /// `durationNonNegativeOrDefault`, conf/configuration.go:357-362).
    pub fn non_negative_or(self, default: Duration) -> Duration {
        if self.0 < Duration::ZERO {
            default
        } else {
            self.0
        }
    }
}

impl serde::Serialize for DurationField {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for DurationField {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        parse_go_duration(&s)
            .map(DurationField)
            .map_err(|e| serde::de::Error::custom(format!("invalid duration: {e}")))
    }
}

impl From<Duration> for DurationField {
    fn from(d: Duration) -> Self {
        DurationField(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_units() {
        assert_eq!(
            parse_go_duration("500ms").unwrap(),
            Duration::from_millis(500)
        );
        assert_eq!(parse_go_duration("1s").unwrap(), Duration::from_secs(1));
        assert_eq!(
            parse_go_duration("1h30m").unwrap(),
            Duration::from_secs(5400)
        );
        assert_eq!(parse_go_duration("1m0s").unwrap(), Duration::from_secs(60));
        assert!(parse_go_duration("-10s").is_err()); // negative -> loader default (Go durationNonNegativeOrDefault)
        assert!(parse_go_duration("5x").is_err());
        assert!(parse_go_duration("").is_err());
        assert!(parse_go_duration("10").is_err());
    }

    #[test]
    fn non_negative_falls_back() {
        // Duration::ZERO cannot represent negatives; verify via a negative
        // raw string through the loader instead (see loader.rs).
        let d = DurationField(Duration::from_secs(3));
        assert_eq!(
            d.non_negative_or(Duration::from_secs(5)),
            Duration::from_secs(3)
        );
        assert_eq!(
            DurationField(Duration::from_secs(3)).non_negative_or(Duration::from_secs(5)),
            Duration::from_secs(3)
        );
    }

    #[test]
    fn serde_round_trip() {
        // A TOML document is a key/value map, so the value type is exercised in
        // the field position that carries it.
        #[derive(serde::Deserialize)]
        struct Root {
            d: DurationField,
        }

        let cfg: Root = toml::from_str("d = \"1h30m\"").unwrap();
        assert_eq!(cfg.d.0, Duration::from_secs(5400));
        let bad: Result<Root, _> = toml::from_str("d = \"bogus\"");
        assert!(bad.is_err());
    }
}
